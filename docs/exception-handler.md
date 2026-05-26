# 异常处理架构设计

## 概述

biujs 实现了 JavaScript 的 SEH（Structured Exception Handling，结构化异常处理）模型，支持 `try`/`catch`/`finally` 以及 `throw` 语句。设计采用**多层抽象**：AST → IR → Bytecode → VM，每层职责清晰。

---

## 1. 架构层次

```
┌─────────────────────────────────────────────────────┐
│  AST (oxc)                                          │
│  TryStatement { block, handler?, finalizer? }       │
│  ThrowStatement { argument }                         │
└────────────────────┬────────────────────────────────┘
                     │ lowering
┌────────────────────▼────────────────────────────────┐
│  IR (Control Flow Graph + SSA)                      │
│  PushSeh / PopSeh / Throw / LoadException /         │
│  ResumeException                                     │
└────────────────────┬────────────────────────────────┘
                     │ codegen
┌────────────────────▼────────────────────────────────┐
│  Bytecode                                           │
│  Try / EndTry / ThrowExc / LoadException /          │
│  ResumeExc                                          │
└────────────────────┬────────────────────────────────┘
                     │ VM execution
┌────────────────────▼────────────────────────────────┐
│  VM                                                 │
│  SehRecord 栈，handle_throw(), jump()                │
└─────────────────────────────────────────────────────┘
```

---

## 2. Bytecode 指令

| 指令 | 操作数 | 语义 |
|------|--------|------|
| `Try` | `catch_offset, finally_offset, _` | 向 SEH 栈压入一个 handler 记录。`catch_offset` 和 `finally_offset` 是相对于当前 PC 的跳转偏移。 |
| `EndTry` | `_, _, _` | 弹出栈顶 SEH 记录（正常退出 try 块时调用）。 |
| `ThrowExc` | `src, _, _` | 抛出异常（支持 `Immd` 常量池索引或寄存器/栈值）。 |
| `LoadException` | `dst, _, _` | 将 `Rv` 寄存器中的异常值加载到 `dst`。 |
| `ResumeExc` | `_, _, _` | finally 块结束时调用。检查是否有待处理的异常，若有则决定跳转到 catch 或向外传播。 |

---

## 3. IR 指令

### PushSeh
```rust
PushSeh {
    handler: BlockId,       // catch 块（无 catch 时指向 finally）
    finally: Option<BlockId>, // finally 块
}
```

### PopSeh
```rust
PopSeh
```
在 IR 层面，`PopSeh` 出现在两个位置：
- try 体正常结束时（在跳转到 finally 之前）
- catch 体正常结束时（在跳转到 finally 或 after_finally 之前）

### Throw
```rust
Throw {
    value: Value,    // 异常值（支持 Variable / Constant / Primitive）
    args: Vec<Value>, // phi 参数（传递给 catch 块的参数值）
}
```
`Throw` 是终结指令，其后代码为死代码。`args` 用于 SSA phi 节点：catch 块的参数需要从 throw 块传递过来。

### LoadException
```rust
LoadException { dst: Value }
```
从 VM 的 `Rv` 寄存器读取异常值并存入 IR 变量。只在 catch 块内部使用。

### ResumeException
```rust
ResumeException { args: Vec<Value> }
```
finally 块结束时的"终结"指令。检查 `SehRecord.pending_exception`：
- **有异常且有未被执行的 catch** → 跳转到 catch（传递 args）
- **有异常但没有 catch** → 弹出 SEH，向外传播异常
- **无异常** → 继续执行（不弹出 SEH）

---

## 4. VM 执行

### SehRecord 结构
```rust
struct SehRecord {
    handler_pc: usize,              // catch 处理程序 PC（0 表示无 catch）
    finally_pc: usize,              // finally 处理程序 PC（0 表示无 finally）
    saved_rsp: usize,               // try 入口时的栈指针
    saved_rbp: usize,               // try 入口时的帧指针
    in_finally: bool,               // 是否正在执行 finally
    pending_exception: Option<Value>, // 进入 finally 时暂存的异常
    catch_executed: bool,           // catch 是否已执行过（防止重复捕获）
}
```

### handle_throw 逻辑

```
handle_throw(exc_val):
  1. 弹出栈顶 SehRecord
  2. 恢复 rsp, rbp 到 try 入口状态
  3. 如果有 finally 且未在执行 finally 中：
     → 标记 in_finally=true，暂存 pending_exception
     → 将 record 重新压栈
     → jump 到 finally_pc
  4. 否则如果有 catch：
     → 将 exc_val 写入 Rv 寄存器
     → jump 到 handler_pc
  5. 否则（无 catch，无 finally）：
     → 递归 handle_throw（向外层传播）
  6. 如果 SEH 栈为空：
     → 返回 RuntimeError::Thrown(exc_val)
```

### ResumeExc 逻辑

```
ResumeExc:
  1. 如果栈顶 record 处于 in_finally 状态且有 pending_exception：
     a. 如果有真正的 catch（handler_pc ≠ 0 且 handler_pc ≠ finally_pc）
        且 catch 尚未执行过（!catch_executed）：
        → 标记 catch_executed=true，清除 in_finally/pending_exception
        → 将异常写入 Rv，jump 到 handler_pc
     b. 否则：
        → 弹出 SEH record，递归 handle_throw（向外传播）
  2. 否则如果 in_finally 但无 pending_exception：
     → 清除 in_finally 标志（正常结束，不弹出 SEH）
```

### 设计要点

1. **handler_pc == finally_pc 的情况**：当 `try-finally`（无 catch）时，lowering 会将 `handler` 设为 `finally`，此时 handler_pc == finally_pc。`ResumeExc` 通过 `has_real_catch = handler_pc != 0 && handler_pc != finally_pc` 区分这种情况，避免异常被"捕获"到 finally 自身。

2. **catch_executed 标志**：防止 `catch` 块中抛出新异常后又回到同一 catch 的循环。这是嵌套 `try/catch/finally` 正确性的关键。

3. **栈帧恢复**：进入异常处理时，rsp/rbp 恢复到 try 入口点，这意味着 finally 和 catch 块在 try 的"栈帧上下文"中执行。

---

## 5. Control Flow 设计

### 正常流（无异常）

```
    PushSeh
    ┌─────────────────────┐
    │  try body           │
    └─────────┬───────────┘
              │ (正常结束)
              ▼
         PopSeh
              │
    ┌─────────▼───────────┐
    │  finally (如果存在)  │──ResumeExc（无待处理异常）──▶ after_finally
    └─────────────────────┘
```

### 异常流（try 体抛异常，有 catch + finally）

```
    throw "error"
         │
    ┌────▼─────────────────────────────┐
    │ handle_throw:                    │
    │   1. 发现 finally，暂存异常       │
    │   2. jump → finally              │
    └────┬─────────────────────────────┘
         ▼
    ┌─────────────────────┐
    │  finally body       │
    └─────────┬───────────┘
              │ ResumeExc（有 pending exception，catch 未执行）
              ▼
    ┌─────────────────────┐
    │  catch body         │──LoadException 读取 Rv
    └─────────┬───────────┘
              │ (正常结束)
              ▼
         PopSeh
              │
    ┌─────────▼───────────┐
    │  finally (再次执行)  │──ResumeExc（无待处理异常）──▶ after_finally
    └─────────────────────┘
```

### 异常流（catch 体抛异常，嵌套场景）

```
    inner throw "inner"
         │
    ┌────▼─────────────────────────────────────────┐
    │ handle_throw → finally → ResumeExc            │
    │ (有 catch 且未执行) → jump to inner catch     │
    └────┬──────────────────────────────────────────┘
         ▼
    inner catch: result = 1; throw "outer"
         │
    ┌────▼──────────────────────────────────────────┐
    │ handle_throw:                                 │
    │   内层 SehRecord.catch_executed 被 ResumeExc   │
    │   设为 true（第1次），但当前是新的异常           │
    │   发现 finally → jump to finally              │
    └────┬──────────────────────────────────────────┘
         ▼
    inner finally (再次执行，result += 10)
         │
    ┌────▼──────────────────────────────────────────┐
    │ ResumeExc:                                    │
    │   pending="outer", catch_executed=true        │
    │   → 弹出内层 SEH，handle_throw("outer")       │
    └────┬──────────────────────────────────────────┘
         ▼
    outer catch: result += 100
         │
    result = 111 ✓
```

---

## 6. Lowering 策略

[`lower_try`](src/compiler/lowering/mod.rs#L411) 生成以下控制流结构：

```rust
fn lower_try(try_stmt):
    // 创建基本块
    try_body    = create_block("try_body")
    catch_blk   = create_block("catch")
    finally_blk = create_block("finally")
    after_finally = create_block("after_finally")

    // SEH 处理器：catch 优先，否则 finally
    seh_handler = has_catch ? catch_blk : finally_blk
    seh_finally = has_finally ? Some(finally_blk) : None

    // 1. 注册 SEH handler + 添加异常边
    push_seh(seh_handler, seh_finally)
    add_exception_edge(try_body, seh_handler)
    if has_finally: add_exception_edge(try_body, finally_blk)
    jump(try_body)

    // 2. Try 体
    switch_to_block(try_body)
    lower_statements(try_stmt.block.body)
    if not terminated:
        pop_seh()                    // 正常退出，注销 SEH
        if has_finally: jump(finally_blk)
        else: jump(after_finally)

    // 3. Catch 块（如果有）
    if has_catch:
        switch_to_block(catch_blk)
        exc_val = load_exception()   // 从 Rv 读取异常值
        lower_statements(catch_clause.body)
        if not terminated:
            pop_seh()                // 正常退出 catch
            if has_finally: jump(finally_blk)
            else: jump(after_finally)

    // 4. Finally 块（如果有）
    if has_finally:
        switch_to_block(finally_blk)
        lower_statements(finalizer.body)
        if not terminated:
            resume_exception()       // 检查待处理异常
            jump(after_finally)

    switch_to_block(after_finally)
```

**关键设计决策**：
- `PopSeh` 不在 catch 入口执行，而是由 **ResumeExc** 在 finally 结束后处理 SEH 弹出
- `add_exception_edge` 为 SSA builder 提供 CFG 边，确保异常路径上的变量可见性

---

## 7. SSA 异常边处理

[`SSABuilder::add_exception_edges_for_throws`](src/compiler/ir/ssabuilder.rs#L74) 负责为所有异常路径建立 CFG 边：

1. **Throw 指令**：扫描当前 SEH 作用域栈，为每个 `(catch_handler, finally_handler)` 添加异常边
2. **ResumeException 指令**：使用预记录的外层 SEH 作用域（`finally_outer_scope`）为 finally 块的 ResumeException 添加向外层 catch 的边

```
finally_outer_scope[finally_blk] = seh_scope_before_push
  ↑ 在 PushSeh 时记录

ResumeException 出现时：
  for (catch_handler, _) in finally_outer_scope[block]:
      add_edge(block, catch_handler)
```

这确保了 finally 中写入的变量通过 SSA phi 节点正确传递到外层 catch。

---

## 8. 测试覆盖

### 单元测试（13个核心场景）

**基本异常处理（8个）**

| 测试 | 场景 | 预期 |
|------|------|------|
| `try_catch` | 基本 throw → catch | result = 1 |
| `try_catch_finally` | throw → catch → finally | result = 11 |
| `try_finally_no_throw` | 正常执行 + finally | result = 11 |
| `try_catch_finally_no_throw` | 正常 try（无 throw）+ finally | result = 11 |
| `try_finally_with_throw` | 内层 try-finally + 外层 catch | result = 11 |
| `nested_try_catch_finally` | 嵌套 try/catch/finally，catch 中再次 throw | result = 111 |
| `try_finally_propagate` | finally 后异常传播到外层 catch | result = 11 |
| `try_finally_propagate_no_catch_body` | finally 后传播，外层 catch 无操作 | result = 1 |

**break/continue + finally（2个）**

| 测试 | 场景 | 预期 |
|------|------|------|
| `break_in_try_finally` | break 在 try-finally 中执行 | result = 20 |
| `continue_in_try_finally` | continue 在 try-finally 中执行 | result = 3 |

**return + finally（5个）**

| 测试 | 场景 | 预期 |
|------|------|------|
| `return_in_try_finally` | return 在 try-finally 中执行 | 返回值 = 1 |
| `return_in_try_finally_with_result_modification` | finally 修改变量不影响返回值 | 返回值 = 5 |
| `return_in_try_finally_nested` | 嵌套 try-finally 中的 return | 返回值 = 1 |
| `return_in_catch_finally` | return 在 catch-finally 中执行 | 返回值 = 2 |
| `return_in_try_catch_finally_no_throw` | 无异常时的 return | 返回值 = 3 |

### test262 测试套件

| 测试套件 | 数量 | 说明 |
|----------|------|------|
| `language/statements/try` | 54 | try-catch-finally 语句测试 |
| `built-ins/Error` | 93 | Error 构造器和原型测试 |
| `built-ins/NativeErrors` | 94 | TypeError, ReferenceError, RangeError 等测试 |

**测试状态**：
- 大部分 test262 测试被跳过（使用 `var`、`eval`、`arguments` 等不支持特性）
- 核心异常处理逻辑测试通过
- Error 对象原型链测试通过（支持 `instanceof`）

---

## 9. 实现状态更新

### ✅ 已实现的特性

| 特性 | 状态 | 说明 |
|------|------|------|
| Error 类型对象 | ✅ | 所有标准 Error 类型已实现：Error, TypeError, ReferenceError, RangeError, URIError, EvalError |
| 异常对象原型链 | ✅ | Error.prototype 及子类型原型链已建立，支持 `instanceof` 检测 |
| break/continue 与 finally | ✅ | 通过 DelayedJump 机制实现，break/continue 会先执行所有 pending 的 finally 块 |
| return 与 finally | ✅ | 通过 `delayed_return` 标志实现，return 会先执行所有 pending 的 finally 块 |

### 🚧 待实现的特性

无。异常处理核心功能已全部实现。

### 实现细节：DelayedJump 机制（break/continue）

`break`/`continue` 在 try-finally 中的处理流程：

```
break/continue 目标
       │
       ▼
┌─────────────────────┐
│ DelayedJump         │  检查当前 SEH 栈中是否有未执行的 finally
│   - target: 偏移量   │  
│   - seh_depth: 深度  │
└─────────┬───────────┘
          │
    ┌─────┴─────┐
    ▼           ▼
有 finally    无 finally
    │           │
    ▼           ▼
执行 finally   直接跳转
    │
    ▼
ResumeExc 检查 delayed_jump_target
    │
    ▼
跳转到原始目标
```

### 实现细节：DelayedReturn 机制（return）

`return` 在 try-finally 中的处理流程：

```
return 语句
       │
       ▼
┌─────────────────────┐
│ Ret 指令            │  检查当前 SEH 栈中是否有未执行的 finally
│   - 返回值在 Rv 寄存器 │
└─────────┬───────────┘
          │
    ┌─────┴─────┐
    ▼           ▼
有 finally    无 finally
    │           │
    ▼           ▼
执行 finally   正常返回
    │
    ▼
ResumeExc 检查 delayed_return
    │
    ▼
弹出 SEH，继续返回（Rv 值保持不变）
```

**关键代码**（`src/vm/mod.rs:56-85`）：

```rust
Opcode::Ret => {
    // Check if we need to execute any finally blocks before returning
    let mut finally_to_execute = None;
    for (idx, record) in self.state.seh_stack.iter().enumerate() {
        if record.finally_pc != 0 && !record.in_finally {
            finally_to_execute = Some(idx);
            break;
        }
    }

    if let Some(idx) = finally_to_execute {
        // Mark the record as in_finally and set delayed return flag
        if let Some(record) = self.state.seh_stack.get_mut(idx) {
            record.in_finally = true;
            record.delayed_return = true;
        }
        let finally_pc = self.state.seh_stack[idx].finally_pc;
        self.state.jump(finally_pc);
    } else {
        // No pending finally blocks, proceed with normal return
        // ... normal return logic
    }
}
```

**ResumeExc 处理 delayed_return**（`src/vm/mod.rs:1010-1016`）：

```rust
} else if delayed_return {
    // Delayed return after finally execution
    self.state.seh_stack.pop();
    // Continue to normal return flow - re-execute Ret opcode
    // The return value is already in Rv
    return Ok(());
}
```

```rust
Opcode::DelayedJump => {
    let offset = operands[0].as_immd();
    let seh_depth = operands[1].as_immd() as usize;

    // 检查是否需要执行 finally 块
    let mut finally_to_execute = None;
    let stack_len = self.state.seh_stack.len();
    let start_idx = stack_len.saturating_sub(seh_depth);
    
    for idx in start_idx..stack_len {
        if let Some(record) = self.state.seh_stack.get(idx) {
            if record.finally_pc != 0 && !record.in_finally {
                finally_to_execute = Some((idx, record.finally_pc));
                break;
            }
        }
    }

    if let Some((idx, finally_pc)) = finally_to_execute {
        // 标记 in_finally 并设置延迟跳转目标
        if let Some(record) = self.state.seh_stack.get_mut(idx) {
            record.in_finally = true;
            record.delayed_jump_target = Some(offset);
        }
        self.state.jump(finally_pc);
    } else {
        // 所有 finally 执行完毕，直接跳转
        self.state.jump_offset(offset);
    }
}
```

### return + finally 的实现计划

需要修改 `Opcode::Ret` 的处理逻辑：

```rust
Opcode::Ret => {
    // 1. 检查是否有 pending 的 finally 块
    // 2. 如果有，保存返回值到特殊寄存器
    // 3. 执行 finally 链
    // 4. finally 结束后恢复返回值并真正返回
}
```

这需要：
- 新增 `ReturnValue` 寄存器或栈槽保存延迟返回值
- 修改 `ResumeExc` 处理 return 场景（无异常但有延迟返回值）
- Lowering 阶段生成不同的 return 路径（在 SEH 作用域内 vs 外）
