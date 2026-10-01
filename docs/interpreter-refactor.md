# 解释器与字节码重构方案（P0–P3）

> **用途**：字节码编码 + 解释器执行模型的结构性重构执行文档，供新分支 / 新会话直接施工。
> **自包含**：从这里出发不需要读其它文档就能排期与施工；需要背景时按 §6 的索引去查。
> **锚点说明**：文中的 `文件:行号` 以 2026-10-01 的 `develop` 状态为准，改动后会漂移，
> **以符号名（`fn` / `struct` / 枚举变体名）为准，行号只作初次导航**。
> **度量基线**：通过 16561 / 执行 19708 / 失败 3147 / 跳过 7843；单元 190、feature 523、护栏 7。

---

## 0. 一页速览

| 阶段 | 内容 | 工作量 | 风险 | 直接消掉什么 |
|------|------|--------|------|--------------|
| **P0a** | 指令**容器**改 enum（命名字段） | 1 周 | 低 | `operands[0..2]` 位置约定、注释当元数据 |
| **P0b** | 字段**类型**收紧 + 穷尽 `desc()` + 与 IR 读写集互校 | 1–1.5 周 | 低 | "跳转偏移当寄存器读"整类 bug；三份知识互不校验 |
| **P1** | 调用归一：一个 `enter_call(Call\|Construct, …)` | 1–1.5 周 | 中 | `invoke` 必须镜像 `Call` 的特例（已踩四次）；每加一种可调用类型补 4–5 处 |
| **P2** | `CodeBlock`/`FunctionBody` per function | 1.5–2 周 | 中 | `Module` 全局可变状态；`generators`/`asyncs`/`derived_ctors` 三个 HashSet |
| **P3a** | 帧结构归一（**保留**嵌套循环与哨兵 pc） | 1.5–2 周 | 中 | 10 条平行栈（约 171 个引用点）；"帧深度必须在装帧之前取"；`frame_argc.truncate` 疑点 |
| **P3b** | 堆上调用栈 + 单层主循环 | 2–3 周 | 高 | 4 层嵌套 `while step()`、哨兵 pc、`PushC/PopC/MovC` 三条指令、256MB 测试线程 |
| **并行语义线** | TypedArray/DataView/ArrayBuffer 入册解锁；Proxy 残留 | 见 §7 | 低 | 纯赚 test262 分数（本重构本身**不动**任何数字） |

**三条决策前提**

1. **本重构的第一目标是可维护性，不是 test262 分数。** 唯一允许的变化是"零回退"。
   若需要数字增长，用 §7 的语义线并行推进，不要指望 P0–P3 带来分数。
2. **与 ChakraCore 的边界**：搬**思想**（表驱动元数据、寄存器在帧内、单一调用入口），
   不搬**代码**，也不搬它在 C++ 里为了 JIT/GC 才存在的那部分（见 §6）。
3. **一次只动一件事。** 尤其：不要在 P0 的同时把跳转**相对化**（理由见 §5.6）。

**两个停止点（可以在任意一处收手，回归必须全绿）**
- 停止点 A：做完 P0a+P0b+P1 —— 日常"加功能要打补丁"的多数坑消失。
- 停止点 B：做完 P3a —— 拿走 P3 约 60% 的收益、30% 的成本，控制流一行不动。
- **P3b 之前有 Go/No-Go 判据**（§3.6）。

### 0.1 第一次开工的顺序（P0a）

> **已执行，见 §3.1 的施工结果与 §9 的进度表。** 下面这段保留为**最初的计划**，
> 其中两处事后被证伪，施工时不要再照抄：
> 1. 第 2 步的 `Opcode::Add` **不存在**——算术指令是 `Addx`（对象加法）/ `AddC`（栈调整）；
> 2. 第 6 步"删掉 `debug_instructions`"是**错的**，理由见 §1.1 的订正。
>
> 实际路径是"**用脚本按指令表一次性铺开**"（93 个变体、111 处 codegen 发射点、
> 22 处测试发射点），而不是先做 5 个变体试水——因为枚举的穷尽性能让编译器把遗漏点全列出来，
> 分批的收益小于它带来的"半迁移状态"成本。真正需要分批的是 **P0a-2**（见 §3.1）。

```bash
git checkout -b refactor/interpreter-p0a
# 1) 先确认起点干净：单元/feature/护栏全绿 + 逐套件零回退
MEM_LIMIT_KB=4000000 ./scripts/phase2-status.sh
```

2. `src/bytecode.rs`：新增 `Instr`，**先只做 5 个变体**
   （`LoadConst` / `Add` / `Jump` / `Ret` / `Halt`）+ `opcode()` + `operands()`，
   与现有 `Bytecode` 并存。
3. `codegen.rs`：只把这 5 个变体的 5 处发射点改成结构体字面量；
   `run_instruction`：只改对应的 5 条 arm。
4. 跑一次全量（口径同第 1 步）——**必须一个数都不动**。这一步验证"闭环通了、
   且 enum 化不影响语义"，是后续铺开的信心来源。
5. 把 `Module.instructions` 的类型切到 `Vec<Instr>`，让编译器列出**全部**构造点；
   按文件逐个铺开（`codegen` 109 → `vm` 124 → `bytecode` 104 → `lowering` 56）。
6. 删掉 `Bytecode` 与 `Module.debug_instructions`；`Display` 改走 `operands()`。
7. 全量回归 + `--update` + 更新 §9 进度表 + 提交（`refactor(P0a): …`）。

**不要在第 5 步之前动 `Module.instructions` 的类型**：编译器一次性报出几百个错误时，
你分不清哪些是自己改错的。

---

## 1. 现状诊断（实测数据）

### 1.1 字节码层（`src/bytecode.rs`，999 行）

- `Opcode`：**93 个变体**，全部是无数据的 unit 变体。
  （P0a 施工时订正：本文档早先写的"113"是把 `Register` 的 19 个变体一起数进去了。）
- `Operand`：**5 个变体** —— `Primitive(Primitive)` / `Register(Register)` / `Stack(isize)` /
  `Immd(isize)` / `Symbol(u32)`，尺寸 16 B。
- `Bytecode { opcode: Opcode, operands: [Operand; 3] }`，**尺寸 56 B**。
  - **固定 3 槽**：这条指令实际用几个、哪个是目的、是否跳转/调用/返回，只写在变体上方的一行注释里
    （如 `/// load_const dst, const_id`）。
  - 3 槽对齐到 8 字节后浪费 5 B；enum 版反而更小（**实测 48 B**）。
  - 实测的"没用的第三槽"：`Try`（两个偏移 + 一个恒为 0 的填充）、`Halt` / `Ret` / `EndTry` /
    `ResumeExc` / `PrologueEnd`（0 个操作数却是 3 槽）、`DelegateClose`（1 个操作数）。
- `Module` 一把梭：`constants` / `symtab` / `func_info` / `generators` / `asyncs` / `derived_ctors` /
  `exit_pc` / `instructions` / `debug_instructions`。
  （P0a 施工时订正：`debug_instructions` **不要删**。它带的是这一条指令对应的 **IR** 行
  （SSA 变量名、块名），而 `Instr` 只带字节码层的操作数——删了会丢掉 dump 里唯一能看懂
  数据流的那半信息。本文档早先"enum 自带全部信息"的判断是错的。）

### 1.2 编译管线（**这一层不要动**）

```
oxc parser ─► AST ─► lowering(AST→IR, 4609 行) ─► ssabuilder(SSA+throw→handler, 916)
           ─► cfg(432) ─► regalloc(活跃性+线性扫描, 1063) ─► codegen(IR→字节码, 1141) ─► Module
```

- `ir/instruction.rs:559 defined_and_used_vars() -> (Vec<Variable>, Vec<Variable>)` 是
  **liveness/regalloc 的唯一读写集来源**，它 match 的是 **IR 变体**，与字节码的 `[Operand; 3]` **无关**。
- `regalloc` 的产物是 `Variable → Register | Stack` 的映射；`codegen` 把它物化成 `Operand`。
  即：**`Operand` 的 5 个变体是寄存器分配的输出格式**，不是"字节码设计"。
- ChakraCore 的 `ByteCodeGenerator.cpp`（5411 行）是**单遍无 SSA**。
  **照抄它是降级**——不要说"移植前端"。

### 1.3 解释器层（`src/vm/mod.rs`，10838 行）

- `State` 持有**全局** `data_stack: Vec<Value>` + `registers: [Value; 19]` + `rsp` / `rbp` / `pc`。
- **10 条平行栈**（每帧都要同步压/弹）：

  | 栈 | `vm/mod.rs` 内引用点 |
  |---|---|
  | `seh_stack` | 37 |
  | `closure_var_stack` | 33 |
  | `construct_stack` | 19 |
  | `new_target_stack` | 17 |
  | `ctrl_stack` | 15 |
  | `this_stack` | 12 |
  | `this_state` | 12 |
  | `frame_argc` | 11 |
  | `invoke_boundaries` | 10 |
  | `function_stack` | 5 |
  | **合计** | **≈171** |

- **Rust 调用栈就是 JS 调用栈**：4 层嵌套 `while self.step(module)? {}`
  （`run:376`、`invoke_with_new_target:1103`、生成器恢复 `:8677`、`invoke_construct:9355`）。
- **哨兵 pc**：`let return_pc = module.instructions.len();`（`:1065`、`:9316`）用越界 pc 当返回地址。
- **`Opcode::PushC` / `PopC` / `MovC`**（各 7 处引用）与 `AddC` / `SubC`：
  存在的唯一理由是保存/恢复 **Rust 驱动帧**。
- `invoke_boundaries`：保证 Rust 驱动帧内抛出的异常不被它外侧的 handler 抓走。
- 后果：test262 runner 必须给套件线程 **256 MB 栈**（`tests/test262_runner.rs:936`）。

### 1.4 结构性坑源（本方案要消灭的根因）

同一件事有**三份互相不校验的知识**：

| # | 位置 | 形态 |
|---|------|------|
| ① | `ir/instruction.rs:559` | IR 变体 match（喂 liveness/regalloc） |
| ② | `compiler/codegen.rs` | 位置约定（谁在 `operands[0]`）+ **回填表** |
| ③ | `vm/mod.rs` 的 113 条 arm | `operands[i]` 位置读取 |

新增一条指令时三处必须同时改对，**漏一处编译不报、测试也不报**。
现有地雷记录（`architecture.md` §4）中的"新增一条 VM 指令是五层改动"、
"`invoke` 必须镜像 `Call` opcode 的特例"、"寄存器是全局的"、
"帧深度必须在装帧之前取"都是这一根因的不同表现。

---

## 2. 目标与非目标

### 2.1 目标

1. 指令的**元信息单一来源**，且"漏登记"是**编译错误**。
2. 操作数的**种类**由类型表达（`Reg` / `ConstIndex` / `JumpOffset` / `SymbolId`），
   不再靠位置与注释。
3. **一个**调用入口，`[[Call]]` 与 `[[Construct]]` 只是它的一个参数。
4. 函数的字节码/常量/元信息**按函数持有**，VM 不再持有模块级可变状态。
5. 帧是**显式对象**（一条 `Vec<Frame>`），而不是散落在 10 条平行栈里的隐式约定。

### 2.2 明确不做（附理由）

| 不做 | 理由 |
|------|------|
| 整体重写 / 移植 ChakraCore | 现状 19708 执行 / 16561 通过；重写到同等水位 12–24 人月。ChakraCore 的 `lib/` 是 **810k 行**（Backend 238k / Common 105k / Library 138k） |
| GC（`Gc<T>` 替换 `Rc<RefCell<dyn JSObject>>`） | 单项 2–4 人月，且要重写所有对象类型。没有它，NaN-boxing 也没有意义 |
| NaN-boxing / 值表示优化 | 依赖 GC；且当前瓶颈不在值尺寸 |
| 字节码序列化 / 预编译缓存 | `Value` 里是 `Rc<RefCell<dyn JSObject>>`，先要定义对象图序列化。**YAGNI** |
| crate 拆分（parser/bytecode/runtime/interpreter/embed） | `js-runtime` 与 `js-interpreter` 在当前代码里是同一坨（`vm/mod.rs` 10838 行），拆开本身就是数周且风险最高。**放到最后或不做** |
| 换掉 SSA/regalloc 改单遍发射 | 是降级（§1.2） |
| 换 `almond` 解析器 | 现状用 `oxc_parser`（完整 ES）。almond 只到 ES5+部分 ES2015 |
| `RegLayout` 小/大寄存器布局 | Rust enum 下零收益：寄存器数编译期已知。布局压缩只在要序列化时才有意义 |
| 跳转**相对化** | 现在 `PushC`/`exit_pc`/`PopC` 的返回地址语义依赖**绝对 pc**；相对化会连带改生成器挂起恢复与 SEH 重入，是 P3b 的量级。**不要与 P0 混做** |

---

## 3. 阶段方案

> 每个阶段都遵循同一条验收线：定向套件通过率不降 + `./scripts/phase2-status.sh` **逐套件零回退** +
> feature 全绿 + 新增 ≥5 条断言（§4）。

### 3.1 P0a：指令容器改 enum（**P0a-1 已完成**，见 §9）

> **施工结果（2026-10-01）**：P0a 拆成了两半，因为"容器替换"和"逐条 arm 改成命名解构"
> 的风险与收益都不在同一量级：
>
> | 半 | 内容 | 状态 | 代价 |
> |----|------|------|------|
> | **P0a-1** | 指令表（`define_instrs!`）+ `Instr` + `opcode()` / `arity()` / `arity_of()` / `slots()` / `from_parts()` / `unary()` / `binary()`；codegen 111 处发射点改成命名字段；`Module.instructions: Vec<Instr>`；7 处回填改成 `if let Instr::X { .. }` | **已完成** | 单片回归 +10.7%（`slots()` 的 64 字节拷贝 + 多一次 `opcode()` 匹配） |
> | **P0a-2** | `run_instruction` 的 85 条 arm 从"`match opcode` + `slots()[i]`"改成"`match *inst` + 命名字段解构"（含 6 条分组 arm、3 处 `operands.get/first()`）；`step()` 顺手去掉每步的克隆与 `opcode()` 匹配 | **已完成** | 单片回归 114.6s → **107.1s**（基线 103.5s，≈+3.5%）；`slots()` **保留**，理由见 §9.1 第 6 条 |
>
> 表按 **arity 分成四组**（`;` 分隔）：这样"这条指令有几个操作数"在定义处就看得见。
> 两个运行期构造器（`unary` / `binary`）是为 `Instruction::UnaryOp` / `BinaryOp` 携带的
> **运行时 opcode** 准备的——旧编码下这两处完全不做 arity 检查，现在 `arity_of` 兜住了。

**目标**：`[Operand; 3]` → 命名字段的 enum。**语义零变化**，只换容器。

**实测收益**：`Bytecode` 56 B → `Instr` **48 B**（enum 由最大变体决定，判别位吃进 padding）。

```rust
// src/bytecode.rs
pub enum Instr {
    LoadConst { dst: Operand, index: Operand },
    Add       { dst: Operand, lhs: Operand, rhs: Operand },
    Jump      { offset: Operand },
    Call      { dst: Operand, callee: Operand, argc: Operand },
    Ret       { value: Operand },
    Halt,
    // …113 个变体
}

impl Instr {
    pub fn opcode(&self) -> Opcode { /* 穷尽 match，用于 Display / 诊断 */ }
    pub fn operands(&self) -> &[Operand];          // 泛型消费者（回填、dump、校验器）
    pub fn operands_mut(&mut self) -> &mut [Operand];
}
```

**触及文件与规模（实测）**

| 文件 | `Opcode::` 引用 | 说明 |
|------|----------------|------|
| `compiler/codegen.rs` | 109 | 发射点 + **7 处回填**（见 §5.4） |
| `vm/mod.rs` | 124 | `run_instruction` 的 113 条 arm |
| `src/bytecode.rs` | 104 | 定义 + `Display for Module` |
| `compiler/lowering/mod.rs` | 56 | 多为构造 IR，涉及面较小 |
| 其它 | 2 | `compiler/mod.rs`、`vm/object.rs` |

`operands[i]` 直接下标共 **21 处**（`codegen.rs` / `vm/mod.rs` / `bytecode.rs`）。

**步骤**

1. 在 `bytecode.rs` 加 `Instr`（先与 `Opcode`/`Bytecode` 并存），实现 `opcode()` / `operands()`。
2. `Module.instructions: Vec<Bytecode>` → `Vec<Instr>`；`codegen` 的 109 处构造点改成结构体字面量。
3. `run_instruction` 从 `match opcode` 改为 `match instr`，arm 内**解构命名**字段
   （`let Instr::Add { dst, lhs, rhs } = instr`）——这一步是"位置下标消失"的关键。
4. 删掉 `Bytecode` 与 `Module.debug_instructions`（enum 自带信息）。
5. **保留** `Opcode`（`Instr::opcode()` 返回它），供 `is_jump` 一类判定与诊断使用；
   若要彻底删，放到 P0b 之后。

**陷阱**：`Display for Module` 现在泛型打印 `operands` 数组，改成走 `operands()` 即可，
**不要**在这里逐个变体手写——那等于又造一份会漂移的知识。

**验收**：19708 条执行 / 16561 通过 / 7843 跳过**一个数都不许动**。

---

### 3.2 P0b：字段类型收紧 + 穷尽 `desc()` + 互校测试

**目标**：把"操作数的种类"从注释变成类型；把三份知识变成"一份 + 两份派生"。

```rust
// 新类型：让非法组合写不出来
pub struct Reg(u16);          // 物理寄存器 / 虚拟寄存器槽
pub struct ConstIndex(u32);
pub struct JumpOffset(i32);   // ❌ 原设计：单一"绝对 pc"。**已被 P0b-1a 证伪** ——
                              //    Jump/BrIf 是相对、DelayedJump/ResumeExc 是绝对，
                              //    要拆成 RelPc / AbsPc，见 §3.2.1 第 1 条
pub struct SymbolId(u32);

pub enum Instr {
    LoadConst { dst: Reg, index: ConstIndex },
    Add       { dst: Reg, lhs: Reg, rhs: Reg },
    Jump      { offset: JumpOffset },
    GetProp   { dst: Reg, obj: Reg, name: SymbolId },
    // …
}

/// 字节码层的操作数元信息：**唯一的出口**。
/// 注意：`desc()` 只服务**工具与测试**（回填校验、dump、互校），**不在热路径上**——
/// 主循环仍然直接解构命名字段。所以它返回 `Vec` 也完全可以，不必为此加依赖。
pub struct Desc { pub writes: Option<Reg>, pub reads: Vec<Reg>, pub kind: Kind }
pub enum Kind { Normal, Jump, Call, Return, Throw }

impl Instr { pub fn desc(&self) -> Desc { match self { /* 穷尽 */ } } }
```

**为什么这比 ChakraCore 的宏表强**：C++ 的 `OpCodeList.h` 也是单一来源，但没有穷尽性检查；
Rust 的 `match` 穷尽性让"新增变体忘了登记 desc"变成**编译错误**。

**但在本仓库里"穷尽 match 只有两个"更好**：`codegen` 一侧在**构造**（`Value::Variable → Reg`
由 regalloc 决定），`vm` 一侧在**解构**。两边都由 enum 的穷尽性保护。

**互校测试（把三份知识锁在一起）**

新增 `tests/bytecode_consistency.rs`：

1. 对每个 `Instr` 变体：断言 `desc()` 声明的操作数**个数** == `operands().len()`。
2. 对每个有 IR 对应物的变体：断言 `desc().reads/writes` 与
   `ir::Instruction::defined_and_used_vars()` 的**变量个数与方向**一致。
3. 对每个变体：断言 `codegen` 发射的实例满足 `desc()`（可用一个小样例程序跑一遍全流程后遍历 `Module`）。

第 2 条是重点：**用已有的 SSA 读写集当裁判**，于是 `desc()` 不是"第三份人肉维护的知识"，
而是被 IR 层校验过的派生。

**陷阱**：`Operand::Stack` / `Operand::Primitive` 等**不要**收掉——它们是 regalloc 的决定
（溢出槽、立即数），字节码不能替它决定。类型收紧只针对"这个字段是什么角色"。

---

### 3.2.1 P0b-1a 实际做法（`Kind`）与它挖出来的三处发现

**做法**：表里每行可以带一个**可选**的 kind 标记（`Jump { offset } @Jump`），没标 = `Normal`。
宏据此生成 `kind()` / `kind_of()`，**没有兜底分支** —— 往表里加指令而没想它属于哪一类，
编译期就报错。另生成 `Instr::ALL`（全部 opcode，顺序即表顺序），让测试能"逐条过一遍"。

**元测试（`kind_matches_the_vm_implementation`）**：`kind` 声称的是"这条指令执行时会发生什么"，
唯一能证明它没说谎的是 `run_instruction` 的实现本身 —— 于是测试直接 `include_str!("vm/mod.rs")`，
切出每条 arm，按 kind 检查它的正文里有没有相应证据：

| kind | 正文里必须能找到 |
|------|------------------|
| `Call` | `self.invoke(` / `self.construct(` / `self.invoke_with_new_target(` / `builtins::call_native(` |
| `Jump` | `self.state.jump(` / `self.state.jump_offset(` |
| `Suspend` | `generator_yielded` / `await_value` |
| `Throw` | `self.handle_throw(` |
| `Bookkeeping` | `rsp` |
| `Return` | `unreachable!(`（`Ret`/`Halt` 由 `step()` 处理） |

这些断言是**单向**的：标了 `Call` 就必须真的在调用；反过来不成立，见下面第 3 条。

**它挖出来的三处"只存在于实现里"的知识**（都写进了注释，不再靠口口相传）：

1. **`Jump` / `BrIf` 是相对偏移**（`state.jump_offset`），**`DelayedJump` / `ResumeExc` /
   `PrologueEnd` 是绝对 pc**（`state.jump`）。本文档 §3.2 原来写"`JumpOffset` 仍是绝对 pc"是
   **错的** —— 所以类型收紧时一个 `JumpOffset(i32)` 不够，要分成 `RelPc` / `AbsPc`
   （`jump_offsets_are_relative_or_absolute` 测试钉住了这条差异）。
2. **`Try` 有 pc 字段但执行期不跳**（登记 SEH 记录后照样往下走），`IterNext` 写布尔寄存器也不是
   跳转（分支由随后的 `BrIf` 完成）。所以 `Kind` 只能定义成"**执行期的控制流效应**"，
   而不是"有没有 pc 字段" —— "跳转偏移当寄存器读"那类 bug 发生在**字段角色**层面，
   是 `desc()` 的职责，不是 `Kind` 的。
3. **`InstanceOf` / `PropGet` / `ToString` 能跑用户代码**（`Symbol.hasInstance` / getter / `valueOf`），
   但它们的**形状**不是一次调用。所以 `Kind::Call` 收紧为"指令本身就是一次调用"
   （callee + 栈上 `argc`），否则 `Call` 会膨胀到把半个指令集吞进去。

**对 P0b 剩余部分的输入**：

- `Desc.writes` **不能**是 `Option<Reg>`（§3.2 原设计）：`New.callee` 与 `CallMethod.callee`
  是 **in/out** 操作数（先 `get_value` 再回写），所以写集也得是 `Vec`。
- `ThrowExc` 的抛点是 `self.handle_throw(`，`ResumeExc` 也会 `handle_throw`；做角色标注时
  别按"函数名像不像抛异常"来猜。

---

### 3.2.2 P0b-1b 实际做法（操作数角色 + `desc()`）

**表里每个字段都带角色**（`Mov { dst: w, src: r }`），字母表：

| 标记 | `Role` | 含义 |
|------|--------|------|
| `r` | `Read` | 值流入口 |
| `w` | `Write` | 值流出口 |
| `rw` | `ReadWrite` | 先读后写（in/out） |
| `jr` | `RelPc` | **相对**偏移：`pc = pc + off`（`Jump` / `BrIf` / `Try`） |
| `ja` | `AbsPc` | **绝对** pc：`pc = off`（`DelayedJump` / `ResumeExc`） |
| `n` | `Meta` | 常量下标 / `argc` / SEH 深度 / 名字符号 / 编译期栈指针 |

角色是**必填**的（宏的匹配器要求 `字段 : 角色`），所以"漏标"不是疏忽，是编译错误。

**角色是怎么定下来的（两步，都不是"人肉维护"）**

1. **从实现反推**：脚本扫 `run_instruction` 的每条 arm，看每个字段有没有被
   `get_value` / `set_value` / `resolve_property_key` 碰过 → 得到第一稿；
   跳转、元数据、in/out 这些"访问器看不出来"的由人工补（第一稿只留 1 个字段没定，
   `DelegateClose.iter` —— 它在 VM 里被 `..` 整个忽略，所以是 `n`）。
2. **回实现取证**：`roles_match_the_vm_implementation` 直接读 `vm/mod.rs`，对每个字段断言
   "标的角色"与"实际有没有 `get_value` / `set_value` 这个字段"一致（`Meta`/`RelPc`/`AbsPc`
   反过来断言**不**出现在值流里）。这样 `desc()` 的读写集就是**被实现校验过的派生**。

**测试本身也被证伪过**：故意把 `Mov { dst: w }` 改成 `dst: r`，测试立刻红，并打出
"标成 Read，但实现与之不符（读过=false、写过=true）"；改回来即绿。守卫不是摆设。

**形状上的两个修正**（原设计 §3.2 的 `Desc { writes: Option<Reg> }` 表达不了）：

- 写集必须是 `Vec`：`New.callee` / `CallMethod.callee` 是 **in/out** —— 先读出来当 `this`、
  再把解析结果写回同一个操作数，于是新角色 `rw`。
- 除了读写，还要把 pc 目标单列（`RelPc` / `AbsPc`）：`Jump` 与 `Try` **都是相对**偏移，
  但 `DelayedJump` / `ResumeExc` 是绝对 pc —— 这正是 P0b-2 类型收紧要分开的两个类型。

**两处豁免**：`Yield.value` / `Await.src` 走 `-1`（"无操作数"）约定，VM 先 `match` 字段再取值，
于是证据长成 `match value { Operand::Immd(-1) => …, src => self.get_value(src)? }` ——
名字对不上，测试里显式列了这两个 arm 并写明理由。**P0b 把 `-1` 换成 `Option<Operand>` 后，
这条豁免就该删掉**（否则它就成了新的历史包袱）。

**`desc()` 的实现顺带证明了 `slots()` 值得留**：它就是 `slots()` 与 `roles_of(opcode())`
的 zip，不需要为 93 个变体再写一遍"字段 × 角色"的 match。

---

### 3.3 P1：调用路径归一

**现状**：5 个调用 opcode（`Call` / `CallEx` / `CallNative` / `CallMethod` / `New` + `CallSpread`）
× 5 个 Rust 入口（`invoke:867` / `invoke_with_new_target:899` / `invoke_construct:9268` /
`construct` / `construct_with_new_target` / `native_construct:9500`）。

**证据（本仓库最近的两次）**：
- B45：为了让 Proxy 可调用/可构造，必须**分别**给 `CallEx`、`Opcode::New`、`invoke`、`construct` 打补丁。
- B46：`Reflect.construct` 的 `new.target` 又要单独接线（`construct_with_new_target`）。

**目标**：

```rust
enum CallKind { Call, Construct { new_target: Value } }

fn enter_call(&mut self, kind: CallKind, callee: &Value, this: Value,
              args: &[Value], dst: Option<Reg>, module: &Module)
    -> Result<Control, RuntimeError>;
```

所有入口都调它：opcode、`invoke`、原生回调、`Reflect.apply`、`Reflect.construct`、`super()`、
`array.map(fn)`。可调用体的种类判定（字节码函数 / 原生 / 生成器 / async / 绑定 / 类 /
代理 / Reflect 的 apply 陷阱）集中在**这一处**的 match 里。

**顺序要求**：在 P0a 之后做（否则归一后的入口仍是 3 槽下标）。

**验收**：`Proxy` 与 `Reflect` 两套件的通过数不减；新增一条 feature 断言
"新增一种可调用体只改一处"（可以是一个把代理当构造器用的用例）。

---

### 3.4 P2：`CodeBlock` per function

**现状**：`vm/mod.rs` 里 **44 处 `module.` 访问**（`symtab` / `constants` / `generators` /
`asyncs` / `derived_ctors` / `exit_pc` / `func_info` / `instructions`）。
`module` 作为参数被穿透到几乎每个函数。

**目标**：

```rust
pub struct CodeBlock {
    pub instructions: Vec<Instr>,
    pub constants: Vec<Constant>,
    pub func_info: (String, usize),
    pub flags: FnFlags,          // Generator | Async | DerivedCtor | …
    pub exit_pc: Option<usize>,
}
```

`CodeBlock` 由函数对象（`FunctionObject` / `NativeFunctionObject` 旁的元数据）持有；
VM 从**当前帧**取它，而不是从参数取。

**步骤**

1. `Module` 保留"装配期"的角色（给 `Compiler` 用），但 `VM::run` 之后所有运行期访问走 `CodeBlock`。
2. 帧里加 `code: Rc<CodeBlock>`（P3a 之后自然落在 `Frame` 上）。
3. 删掉 `generators` / `asyncs` / `derived_ctors` 三个 `HashSet<u32>` 与 `exit_pc: HashMap`——
   它们都是"按函数 id 查属性"，改用 `CodeBlock.flags` 后，每个函数**自包含**。
4. `generator_registry` / `iterator_registry` / `func_objs` 这些**跨函数**的注册表保留在 VM 上。

**风险**：44 处是扇出改动，容易漏。建议先在 `Frame`（或 `State`）里放一个
`current_code: Rc<CodeBlock>`，把 44 处逐个替换为 `self.code()`，**再**去掉 `module` 参数。

**验收**：`language/statements/generators`、`language/expressions/generators`、
`language/statements/class` 三个套件不减。

---

### 3.5 P3a：帧结构归一（**保留**现有执行模型）

**目标**：把 10 条平行栈（≈171 个引用点）合成一个显式 `Frame`，**不动**控制流
（嵌套循环与哨兵 pc 原样保留）。这是"拿走 P3 六成收益、三成成本"的那一半。

```rust
pub struct Frame {
    pub code: Rc<CodeBlock>,      // P2 之后
    pub rsp: usize,               // 指向全局 data_stack 的窗口
    pub rbp: usize,
    pub pc: usize,
    pub this_val: Value,
    pub this_state: u8,           // THIS_NONE / 已绑定
    pub function_val: Value,
    pub new_target: Value,
    pub is_construct: bool,
    pub argc: usize,
    pub closure-depth: usize,     // 指向 closure_var_stack 的水位
    pub seh-depth: usize,
    pub ctrl-depth: usize,
}

pub struct Frames { stack: Vec<Frame> }   // State 上一个字段
```

**逐项替换映射**

| 现在 | 之后 |
|------|------|
| `this_stack` + `this_state` + `function_stack` + `construct_stack` + `new_target_stack` + `frame_argc` | `Frame` 的字段（`frames.last_mut()`） |
| `closure_var_stack` 深度 / `seh_stack` 深度 / `ctrl_stack` 深度 | `Frame` 里记**水位**，回退时 `truncate(depth)` |
| `invoke_boundaries` | `let boundary = self.frames.len();` |
| "帧深度必须在装帧之前取" | 结构性消失（深度 = `frames.len()`） |

**为什么先做这一半**：控制流一行不动 ⇒ 回归噪声可归因；而收益里的
"帧深度顺序 / `frame_argc.truncate(saved_this_depth)` 疑点 / 平行栈同步"占了大头。

**验收**：全套件零回退；`guards:` 行的三个计数不增。

---

### 3.6 P3b：堆上调用栈 + 单层主循环（**Go/No-Go 之后**）

**目标**：Rust 调用栈不再等于 JS 调用栈。

- **Go/No-Go 判据**（P3a 之后回头看）：
  1. `Ret` 与 SEH 各自只有**一处**实现，且都只读 `frames`；
  2. 生成器不再依赖全局 `data_stack` 的隐式假设；
  3. `frames` 的字段已收拢（没有第 11 条平行栈冒出来）。
  三条都满足 ⇒ 风险从"高"降到"中"，可以做。否则停。

**内容**

1. 4 层嵌套 `while self.step()`（`run:376` / `invoke:1103` / 生成器:8677 / `invoke_construct:9355`）
   合并成**一层**主循环：`loop { self.step()? }`；调用 = 压帧并继续同一循环；返回 = 弹帧并写入调用方 `dst`。
2. `invoke` 变成"跑到 `frames.len() == boundary` 为止"的可重入驱动（**边界概念不会消失**，见 §5.6）。
3. **删掉 `Opcode::PushC` / `PopC` / `MovC`**（各 7 处引用，另 `AddC`/`SubC`）——
   它们只为保存/恢复 Rust 驱动帧而存在。
4. 哨兵 pc（`:1065` / `:9316` / `:8883`）删除；`exit_pc` 映射（生成器 resume 到 `Ret`）
   一并重新设计。
5. SEH 改成"弹帧直到找到 handler"，`finally` 每层都要跑（`ResumeExc` 语义不变）。
6. `tests/test262_runner.rs:936` 的 256 MB 栈可以还给 default。

**必须同时改的东西（清单，漏一项就会出诡异 bug）**

- `Ret` 的构造返回语义（`construct_stack` 的替代）、派生类 `this_state`（`super()` 未绑定时返回）
- `super()` 与 `Reflect.construct` 的 `new.target` 传递（B46 刚接的线）
- 生成器挂起/恢复（`SuspendedFrame`、`generator_yield_pc`、`PrologueEnd` 泊车）
- `async`/`await`（`await` 就地排空微任务队列 + `drive_bytecode_frame:1028` 的状态存/取）
- `IteratorClose`（`delegate_stack`）
- 原生回调的异常边界（`invoke_boundaries` → 帧深度）
- 步数/墙钟/堆三层护栏的会计（一个循环 vs 嵌套循环）
- `VM::run` 的状态重置清单（B37 修的 OOM：重置"一次运行触及的全部状态"）

**性能陷阱（最容易踩）**：`Frame` 里**不要**放 `regs: Vec<Value>`（每次调用一次 malloc）。
正确形态是"一整块寄存器 arena + 帧存 (base, len)"：

```rust
pub struct State {
    regs: Vec<Value>,          // 一整块；帧只持有窗口 (base, len)
    frames: Vec<Frame>,
    // …
}
// 访问一律索引式：self.regs[frame.base + r]，不要 let f = &mut self.frames[i]
```

**borrowck 会逼出写法**：`regs` 与 `frames` 两个字段不能同时可变借用，
所以所有 arm 都要写成 `self.frames[i]` / `self.regs[base + r]` 形式。
这是 P3b 里最耗时的机械劳动，**先做一小块（比如只改 `LoadConst`/`Add`/`Jump` 与一个调用）
验证写法，再铺开**。

---

## 4. 每批的固定动作（沿用项目纪律）

1. **先读**本文件对应阶段的小节 + `architecture.md` §4 的地雷清单。
2. 补 feature 断言（**每批 ≥5 条**，写进 `tests/features/` 对应文件；本重构建议新增
   `tests/features/instr_encoding.rs` 或复用现有文件）。
3. 定向跑该阶段涉及的套件（`TEST262_SUITES=… cargo test --release --test test262_runner test262_report`）。
4. **跑 `./scripts/phase2-status.sh`**：三级验证 + 逐套件对比。有回退就解释或回退，**不要带着回退提交**。
5. 更新本文件的"进度"列 + `docs/handover.md` 的 §4 状态表。
6. 若改动了 `SUITES` / `IN_SCOPE_PENDING`，同步 `scripts/phase2-status.sh` 的 `EXPECTED_SKIPPED`。
7. `--update` 刷新快照，和本批改动一起提交。

**本重构的额外纪律**

- 每个阶段**独立提交**，提交信息注明阶段号（`refactor(P0a): …`）。
- **不允许**在同一提交里同时出现"结构变化"与"语义变化"。
- 全量回归必须确认进程走完全程、且 `guards:` 行没有异常计数（`architecture.md` §7.3）。

---

## 5. 关键实现要点（跨阶段）

### 5.1 `desc()` 的形状

`Desc` 只需要三样：`writes: Option<Reg>`、`reads: Vec<Reg>`、`kind: Kind`。
`desc()` **不在热路径上**（主循环直接解构命名字段），所以这里用 `Vec` 即可，
不要为它引入新依赖；若确实想避免分配，用 `[Reg; 3] + len: u8`。
`kind` 用于：跳转回填校验、`Ret`/`Throw` 的特殊处理、以及"这条指令能不能成为跳转目标"。
**不要**把 `Reg` 换成 `Operand`：`desc()` 描述的是**寄存器语义**，
而 `Operand::Primitive`/`Stack` 是 regalloc 决定的物化形式。

### 5.2 回填表（`codegen.rs` 的 7 处）

现在是 `let mut patchs: Vec<PatchFn>`（`codegen.rs:75`），事后按下标改写：

```
codegen.rs:81   → operands[2] 写栈大小
codegen.rs:571  → operands[0] 跳转目标
codegen.rs:604  → operands[1]
codegen.rs:607  → operands[2]
codegen.rs:634  → operands[0]
codegen.rs:639  → operands[1]
codegen.rs:716  → operands[0] 延迟跳转的绝对地址
```

P0a 之后写成类型安全的形态：

```rust
patchs.push(Box::new(move |this: &mut Self| {
    if let Instr::Jump { offset } = &mut this.codes[pos] {
        *offset = Operand::new_immd(absolute);
    } else {
        unreachable!("patch site is not a Jump");   // 或 debug_assert!
    }
}));
```

P0b 之后 `offset` 是 `JumpOffset`，连 `unreachable!` 都可以换成编译期保证
（回填表按 target 类型分组）。

### 5.3 三份知识的收敛顺序

不要试图一次收敛。顺序是：
**① `desc()` 写出来 → ② 互校测试接上 IR 读写集 → ③ 让 codegen 的发射点也走 `desc()` 校验**。
第 ③ 步做完，"漏登记"才是彻底的编译/测试期错误。

### 5.4 生成器：P3 **不**消除寄存器拷贝

只要挂起期间寄存器窗口不能被复用，就有两种选择：把切片 `Vec::from` 出来（≈现状，但更干净），
或给每个生成器一块独立 arena。P3 的真实改进是把这个拷贝从"全局 `data_stack` 的隐式属性"
变成"生成器自己的局部问题"。**不要**在方案里承诺"生成器不再拷贝"。

### 5.5 "边界"概念不会消失

原生回调（`Array.prototype.map(fn)`）仍然需要"跑到帧深度回到 N 为止"，
所以 `invoke` 仍然是一个 Rust 层的可重入驱动。P3b 只是把边界从
"`ctrl_stack` 深度"换成 "`frames.len()`"。

### 5.6 为什么跳转暂时不能相对化

`PushC` / `PopC` / `exit_pc` 的返回地址语义依赖**绝对 pc**
（`state.pushc(self.state.pc + 1)`）。相对化会连带改：
生成器挂起时的 pc 记录、`Ret` 到哨兵、SEH 重入。
那是 P3b 的量级 —— **不要与 P0 混做**，否则回归噪声无法归因。

### 5.7 度量口径

沿用 `es6-conformance-phase2.md` §2.1：报告里同时给
**计划内通过数 / 目标套件通过率 / 范围外排除数**三个数，
并附 `./scripts/phase2-status.sh` 的逐套件 diff。

---

## 6. 与 ChakraCore 的对照（核查自 `~/open_source/ChakraCore`）

| ChakraCore | Rust 移植方案 | 判断 |
|---|---|---|
| `lib/Runtime/ByteCode/OpCodes.h`（905 行）：`MACRO(opcode, layout, attr, dbgAttr)`，406 条表项 | `Instr` enum + 穷尽 `desc()` + 互校测试 | **采纳思想**，且用穷尽性检查做得更强 |
| `LayoutTypes.h`（111 行）：`Reg1/Reg2/Br/BrReg1…` + `_Small/_Medium/_Large` + `Profiled*` | 不需要布局表；操作数类型由字段类型表达 | **不采纳**（`_Small/_Large` 是为序列化与 JIT；`Profiled*` 是为内联缓存） |
| `OpCodeUtil.h`：`OpCodeLayouts[]` / `GetOpCodeLayout` / `EncodedSize` | `desc()` | 采纳思想 |
| `ByteCodeReader`（115+310 行）：`ReadOp` / `PeekOp` / `GetLayout<T>` | P0a 之后不必要（enum 直接携带） | **不采纳** |
| `InterpreterStackFrame.h:197` `Var m_localSlots[0]`：**寄存器在帧内** | 寄存器 arena + `Frame{base,len}` | **采纳思想**（但不要每帧一个 `Vec`） |
| `InterpreterStackFrame.cpp` 9663 行 / 406 opcode | —— | 提醒：表驱动解决的是**元数据来源**，不是"代码少" |
| `ByteCodeGenerator.cpp` 5411 行：单遍无 SSA | —— | **不采纳**（本仓库已有 SSA+regalloc） |
| `OpCode::PushC/PopC/MovC` 一类"解释器簿记指令" | P3b 删除 | 采纳 |
| `hasBailedOut` / `entered_catch` | `try_stack` 进 `Frame`，语义简化 | 采纳（大幅简化） |
| `Gc<>` / `Var` 标记指针 / `Recycler` | —— | **不采纳**（见 §2.2） |
| `JavascriptProxy` 在对象层转发陷阱 | **本仓库必须在 VM 层截获**（8 个截获点） | **有意偏离**：对象层调不动 JS；`Rc<RefCell<..>>` 下重入会 panic |

参考规模（衡量"移植"的代价）：`lib/` 合计 **810,519 行** ——
Backend(JIT) 238,764 / Library 138,685 / Common 105,688 / Language 80,455 /
Parser 54,214 / ByteCode 40,559 / Types 26,164。

---

## 7. 度量基线与本期间的预期

| 指标 | 基线（2026-10-01） |
|------|-------------------|
| test262 执行 / 通过 / 失败 / 跳过 | 19708 / **16561** / 3147 / 7843 |
| 通过率 | 84.03% |
| 单元 / feature / 护栏 | 190 / 523 / 7 |
| 全量耗时 | 约 3m（4 分片，单片内存上限 4 GB；默认 2.5 GB 会被 `built-ins/Array` 的 ~384MB 合法分配打断） |

**本重构期间唯一允许的变化是"零回退"。** 若要数字增长，用下面这条**语义线**并行推进
（互不冲突，可交替做）：

- `built-ins/TypedArray` / `built-ins/TypedArrayConstructors` / `built-ins/DataView` / `built-ins/ArrayBuffer`
  的**入册解锁**（B43 实现了对象但没入册；`TypedArray` 还在 `IN_SCOPE_PENDING`）。
  **按 B46 的实测经验，这类"入册解锁"是纯赚分**：单是摘掉 `Reflect` 一个特性名就让
  跳过数 −323、通过数 +296。做法：先加进 `SUITES` + 从 `IN_SCOPE_PENDING` 摘掉，
  定向跑一次看通过率，达标（A5 的 ≥50%）就留下并同批更新快照与 `EXPECTED_SKIPPED`。
- `Proxy` 的两条残留：原型链上的代理不触发陷阱（约 10 条，需把原型链查询上移 VM）；
  描述符的深度不变量（`IsCompatiblePropertyDescriptor`，约 11 条）。

---

## 8. 风险登记

| 风险 | 等级 | 应对 |
|------|------|------|
| P0a 是扇出改动（≈395 处 `Opcode::`、21 处 `operands[i]`），漏改不会编译报错的部分少但存在 | 中 | 靠"改完 `Module.instructions` 的类型"让编译器扫出全部构造点；`Display` 走 `operands()` 不要手写 |
| P0b 的互校测试可能发现**既有**的不一致（IR 读写集与实际使用不符） | 中 | 是好事：先把不一致登记为已知偏差，不要为了测试变绿而改语义 |
| P1 归一后调用语义回流（`super()` / 构造返回 / 代理陷阱 / Reflect） | 中 | 定向跑 `class` / `new.target` / `Proxy` / `Reflect` 四套件；新增 feature 断言 |
| P2 的 44 处 `module.` 扇出 | 中 | 先引入 `self.code()`，再逐个替换，**不要**一次删掉 `module` 参数 |
| P3a 改动面 ≈171 个引用点，但控制流不动 | 中 | 按"先只读、后改写"两轮推进；每轮一次全量回归 |
| P3b 是唯一"高风险"项，失败模式微妙（SEH 跨帧、生成器 resume、`super()`） | 高 | 严格遵守 §3.6 的 Go/No-Go；先在 3–5 条指令上验证"单层循环 + 索引式访问"的写法 |
| 重构期间数字停住，与 A8 目标冲突 | 中 | 用 §7 的语义线并行推进；每个阶段结束都更新 handover 的"下一步" |
| 中途放弃导致"半新半旧" | 中 | 两个停止点（§0）保证任一处收手都是自洽状态；每个阶段独立提交 |

---

## 9. 进度（施工时逐行更新）

| 阶段 | 状态 | 提交 | 备注 |
|------|------|------|------|
| **P0a-1** 容器 enum + 指令表（codegen / Module / 回填） | **已完成**（2026-10-01） | 见 `git log --oneline refactor/interpreter-p0a` | 通过 16561 / 执行 19708 / 跳过 7843 **一个数未动**、零逐套件回退；单元 190→**194**；单片回归 +10.7%（待 P0a-2 收回） |
| **P0a-2** `run_instruction` 逐 arm 命名解构（85 arm，含 6 条分组） | **已完成**（2026-10-01） | 见 `git log --oneline refactor/interpreter-p0a` | 单片 114.6s → 107.1s（基线 103.5s）；通过 16561 / 执行 19708 / 跳过 7843 **仍然一个数未动** |
| **P0b-1a** `Kind` 分类（表里 `@Kind` 标记 + 穷尽 `kind()`）+ VM 源码元测试 | **已完成**（2026-10-01） | 见 `git log --oneline refactor/interpreter-p0a` | 单元 194→**198**；4 条测试，其中 1 条直接读 `vm/mod.rs` 取证 |
| **P0b-1b** 操作数角色（表里 `dst: w` / `index: r` / `target: jr`）+ `desc()` + 同款元测试 | **已完成**（2026-10-01） | 同上 | 单元 →**200**；角色用"从实现反推 + 回实现取证"两步定下来 |
| P1 调用归一 | 未开始 | | |
| P2 CodeBlock per function | 未开始 | | `vm/mod.rs` 里 `module.` 访问点 44 处 |
| P3a 帧结构归一 | 未开始 | | 平行栈 ≈171 个引用点 |
| P3b 堆帧 + 单层循环 | 未开始 | | Go/No-Go：见 §3.6 |
| P0b-2 类型收紧（`RelPc`/`AbsPc`、`Reg`/`ConstIndex`/`SymbolId`） | 未开始 | | 证据已在 §3.2.1：偏移必须分相对/绝对；`New.callee` 是 in/out 不能收成 `Option` |
| 语义线：TypedArray 族入册解锁 | 未开始 | | 可随时插入；纯赚 test262 分数 |

### 9.1 P0a 的实际改动（P0a-1 + P0a-2，至此收口）

| 文件 | 改了什么 | 规模 |
|------|----------|------|
| `src/bytecode.rs` | 新增 `define_instrs!` 指令表（93 行、按 arity 分四组）+ `Instr` + `opcode()` / `arity()` / `arity_of()` / `slots()` / `from_parts()` / `unary()` / `binary()` / `Display`；删除 `Bytecode` 及其构造器与 `Display`；`Opcode` 加 `PartialEq, Eq`；自带单测改为断言新契约（`arity` / `slots` 填充 / `Display` 不再打印填充 / `unary`·`binary` 的 arity 校验） | +374 / −… |
| `src/compiler/codegen.rs` | 111 处 `Bytecode::single/double/triple/empty(Opcode::X, …)` → `Instr::X { … }`（`Try` 的第三槽被丢弃）；7 处回填改成 `if let Instr::X { .. }` / `match &this.codes[pos]`（**类型安全**：往非跳转指令塞偏移现在是编译错/`unreachable!`）；2 处运行期 opcode → `Instr::unary` / `Instr::binary` | 499 行变动 |
| `src/vm/mod.rs` | `inst: &Instr` + `let operands = inst.slots(); let opcode = inst.opcode();`（**93 条 arm 一行未改**，这是过渡桥接）；generator 恢复处那一处 `inst.operands[0]` 顺手改成 `if let Instr::Yield { dst, .. }`（P0a-2 的先例） | 129 行变动 |
| `src/compiler/mod.rs` | `Vec<Bytecode>` → `Vec<Instr>`，`code.opcode` → `code.opcode()` | 4 行 |

**P0a-2 的改动**（`src/vm/mod.rs` 588 行、`src/bytecode.rs` 注释）

| 位置 | 改了什么 |
|------|----------|
| `run_instruction` | 85 条 arm 改成 `match *inst` + 命名字段解构；182 处 `operands[N]` 变成字段名；3 处 `operands.get/first()` 改成字段；2 条带嵌套 dispatch 的 arm 补了局部 `let opcode = inst.opcode();` |
| `step()` | 指令改为**按引用**取出（去掉每步 48 字节克隆），并直接 `match *inst { Instr::Halt {} … }`（去掉每步的 `opcode()` 匹配） |
| `Instr::slots()` | 保留，但用途改为工具侧（`Display` + 单测），文档里写明**不在热路径上** |

**P0a-2 实际怎么做的（六条经验，都踩过）**

1. **匹配 `*inst` 而不是 `inst`**：`Instr` 是 `Copy`，按值匹配后每个绑定都是 `Operand`
   而不是 `&Operand` —— 于是 arm 体里原有表达式**一字不用改类型**（`get_value(dst)`、
   `set_value(dst, v)`、`resolve_property_key(a2, …)` 都直接吃 `Operand`）。
   若改成引用绑定，182 处都要补 `*`。
2. **按基线缩进切 arm**，否则会把 `Instr::BitAnd | BitOr | BitXor` **体内那层**
   `match opcode { Opcode::BitAnd => … }` 也当成同级 arm 切出来（第一版脚本就是这么错的：
   85 条 arm 被切成了 89 段，且 `}` 配对全乱）。
3. 那 2 条带嵌套 dispatch 的 arm 补了一行 `let opcode = inst.opcode();` —— 它们用 opcode
   变量做二次分派，而外层已经不提供 `opcode` 了。（想彻底去掉的话，应把那层的两个分支
   抽成一个带 `Opcode` 参数的辅助函数，属于 P1 的范畴。）
4. **单变体 arm 绑定真实字段名**（`Instr::LoadConst { dst, index }` → `index.as_immd()`）；
   **分组 arm 用位置别名** `a0/a1/a2`，因为 or-模式要求各分支绑定同名，而
   `IndexGet { index }` 与 `PropGet { property }` 的字段名不同。
5. `Yield` / `Await` / `Call` 的 3 处 `operands.get(N)` / `operands.first()` 改成命名字段。
   其中两处是 `-1` 的"无操作数"标记：现在写成 `match value { Operand::Immd(-1) => … , src => … }`，
   原来的 `None` 分支（永远不可达）随之消失。**这仍是 P0b 该换成 `Option<Operand>` 的地方。**
6. **`slots()` 保留下来，没有按计划删掉**：`Display for Instr` 需要"泛型地看待一条指令"，
   否则 93 个变体各写一遍格式串 —— 那正是这张表要消灭的重复知识。它已经不在热路径上
   （`run_instruction` 改成命名字段解构了），所以留作**工具侧接口**。

顺带修掉的一处：`step()` 原来每条指令 `module.instructions[pc].clone()`（48 字节拷贝）
再 `inst.opcode()`（93 分支的匹配）。现在指令**按引用**取出，并直接 `match *inst { Instr::Halt {} … }`。

**性能实测（单片，同一命令、同一环境，各测一次）**：基线 103.5s → P0a-1 114.6s →
P0a-2 107.1s。剩余的 ≈+3.5% 大概率来自 `match *inst` 那次 48 字节拷贝（旧代码是按引用解构的），
也可能是单次测量噪声 —— 没有做多次取样的方差分析，不声称更精确的数字。
