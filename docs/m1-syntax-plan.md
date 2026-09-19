# M1 设计与任务书：语言语法补齐（迭代器 / for-of / 解构 / 展开 / 模板字符串 / 默认参数）

> **状态**：规划中
> **前置**：跨块活跃性分析修复（`92897b3`）已完成，寄存器分组分配在正确 liveness 下工作
> **基线**：test262 3608/8485（42.52%），另有 15287 个测试因特性缺失被跳过
> **目标**：补齐 ES6 严格模式的核心语法层，解锁约 4500+ 个被跳过的测试，
> 预期全量通过率提升至 **50%+**

---

## 1. 范围与目标

M1 只做 **语法层 + 迭代协议**，不做独立内置对象（Map/Set/Promise/RegExp 归 M4）。

| 特性 | 依赖 | 解锁 test262 测试（约） |
|------|------|------------------------|
| T1 迭代器协议基座 | — | Array/String/arguments 可迭代 |
| T2 `for-of` | T1 | 751 |
| T3 `for-in`（复用 T1 机制） | T1 | 115 |
| T4 模板字符串插值（含 `ToString` 语义） | — | 76 |
| T5 默认参数 | — | 3（直接）+ 解构前置 |
| T6 剩余参数 `...rest` | — | 11（直接）+ 展开共享基建 |
| T7 展开 `...`（调用 / 数组字面量 / `new`） | T1 | 331 |
| T8 数组解构（声明 / 赋值 / 参数 / catch） | T1,T5 | 122 + 散布在其他套件 |
| T9 对象解构 + rest 属性 | T8 | 与 T8 合并计 |

**完成标准**：
1. `UNSAFE_FEATURES` 跳过表移除 `for-of`、`destructuring-binding`、`spread-syntax`、
   `rest-parameters` 后，全量回归不回退现有 3608 个通过；
2. 对应套件通过率 ≥ 60%（for-of ≥ 50%，模板/默认参数/剩余 ≥ 80%）；
3. `tests/features/` 每个任务有至少 5 条集成断言；
4. 188 个单元测试持续通过。

**不在 M1 范围**：tagged template 完整语义（`raw` 冻结数组、`call-site` 缓存）、
Symbol.iterator 的完整协议细节（`return()`/`throw()` 只做 close 触发）、
生成器（M4）、对象解构的计算键（可后补）。

---

## 2. 现状盘点（代码调研结论）

### 已有、可直接复用的基础设施

| 组件 | 位置 | 状态 |
|------|------|------|
| `LoopContext`（break/continue 目标栈） | `lowering/mod.rs:33` | ✅ 可直接给 for-of 用 |
| `lower_for` 的 CFG 降级模板 | `lowering/mod.rs:526` | ✅ 结构可复制 |
| `MakeIterator` / `IterateNext` IR 指令 | `ir/instruction.rs` | ⚠️ 已定义、lowering 未发射 |
| `MakeIter` / `IterNext` 操作码 | `vm/mod.rs:1093` | ❌ TODO 桩（恒返回 done=false） |
| `MakeArray` / `ArrayPush` | VM | ✅ |
| `frame_argc`（每帧实参计数） | `vm/mod.rs` | ✅ 剩余参数的关键依赖 |
| 规范错误可被 JS 捕获（SEH 桥接） | `vm/mod.rs` step() | ✅ `assert.throws` 可用 |
| `Symbol` 构造器 / `Symbol.for` | `builtins/symbol.rs` | ⚠️ 需补 `Symbol.iterator` 常量注册 |
| 模板字符串 lowering | `lowering/mod.rs:1443` | ❌ 只拼接静态 quasis，**丢弃表达式** |

### 缺失（需要从零实现）

- `ForOfStatement` / `ForInStatement` 走到 `lower_statement` 的
  `unimplemented statement` 兜底分支（`lowering/mod.rs:212`）
- `binding_pattern_name`（`lowering/mod.rs:1665`）遇到解构模式只打警告并绑定
  `"<destructured>"` —— 参数解构、catch 解构全部失效
- `Argument::SpreadElement` / `ArrayExpressionElement::SpreadElement` 被降级为
  `null` 占位（`lowering/mod.rs:1215/1317`）
- `TemplateLiteral` 的 `expressions` 字段被完全忽略
- 函数参数没有默认值处理（`FuncParam` 只有名字）

---

## 3. 核心架构设计

### 3.1 迭代器协议（一切的地基）

**双路径设计**：为常见内置类型提供零分配快路径，用户对象走完整协议慢路径。

```
                  ┌─────────────────────────────┐
   src 值 ───────▶│ GetIterator (VM 内部)        │
                  └──────┬──────────────────────┘
                         │ 分派
        ┌────────────────┼──────────────────────┐
        ▼                ▼                      ▼
  ArrayObject      String / arguments      其它对象（慢路径）
  原生迭代器状态    原生迭代器状态           调用 src[Symbol.iterator]()
  {vec, idx}       {chars, idx}            得到 iterator 对象
        └────────────────┴──────────────────────┘
                         ▼
        IterateNext {iter} → (value, done)
        快路径：直接从内部状态产出，不分配 {value,done} 对象
        慢路径：invoke(it.next) → 读 result.value / result.done
```

**VM 侧新增**（`vm/iterator.rs`，新文件）：

```rust
pub enum NativeIterator {
    Array { items: Vec<Value>, idx: usize },
    String { chars: Vec<Value>, idx: usize },   // 预拆 UTF-16 码元
    JsIterator { iterator: Value, done_key: PropertyKey, ... },
}
```

- `MakeIterator` 语义完善：数组/字符串 → 原生状态；其它对象 →
  `find_descriptor(src, Symbol.iterator)`（复用跨块修复引入的 `find_descriptor`）
  并 `invoke`，失败按规范抛 `TypeError`（自动可被 `try/catch` 捕获）。
- `IterateNext` 语义完善：快路径直接产出；慢路径 `invoke(next, iterator, [])`
  后读 `result.value` / `ToBoolean(result.done)`。
- `Symbol.iterator` 原生方法注册到 `Array.prototype` / `String.prototype`，
  返回一个包装原生迭代器的对象（`NativeIteratorObject`，供用户代码手动迭代）。

**关键决策**：`IterateNext` 保留现有 IR 签名 `{iter, item, has_next}` 不变，
避免牵动 SSA/codegen —— 快路径不分配结果对象是纯 VM 内部优化。

### 3.2 `for-of` 降级（CFG 模式）

完全复用 `lower_for` / `LoopContext` 的结构，新增 `lower_for_of`：

```
        iter_blk:  it = MakeIterator(rhs)          ← rhs 在循环外只求值一次
        │
        ▼
 ┌─▶ cond_blk:  (item, has_next) = IterateNext(it)
 │       │ has_next?
 │       ├─ false ──────────────▶ after_blk
 │       ▼
 │   body_blk:  绑定循环变量 = item        ← let: 每轮写入同一栈槽
 │              (解构模式在此展开，见 3.3)     const: 同样写入（新绑定语义）
 │       │
 │       ▼
 │   step_blk:  jump cond_blk             ← continue 目标
 │
 └─ break ──▶ close_blk: IteratorClose(it) ──▶ after_blk
```

- `continue` → `step_blk`（与 break 不同，**不** close 迭代器，规范语义）。
- `IteratorClose` 仅对慢路径迭代器生效（调 `iterator.return()` 并吞掉异常）；
  原生快路径为 no-op。M1 先实现 break/正常结束两条路径的 close，
  异常传播路径的 close（`try` 包裹 body 抛出）列入 T2 验收的可选项。
- `for-in`：`lower_for_in` 先求 `Object.keys(obj)`（复用已有静态方法）得到
  字符串数组，再走与 for-of 完全相同的 CFG —— 零新增机制。
- 循环变量的 `let` 语义：每轮 `assign` 写入同一 IR 变量即可满足绝大多数
  test262 用例；**闭包按轮次捕获**（`for (let i of …) { fns.push(()=>i) }`
  每轮新绑定）依赖 `ClosureVar` 值快照机制，M1 标注为已知简化，
  相关 test262 用例允许失败（约 20 个）。

### 3.3 解构 —— 纯 lowering 降糖，零新增指令

`ArrayPattern` / `ObjectPattern` 在 lowering 层展开为基本读写序列：

```
[a, , b = 1, ...rest] = src
  ↓
it = MakeIterator(src)
(a, ok) = IterateNext(it)        # ok=false → a = undefined
(_, ok) = IterateNext(it)        # elision：丢弃
(b, ok) = IterateNext(it)
b = (ok && b !== undefined) ? b : 1        # 默认值：done 或 undefined 触发
(rest, ok) = IterateNext(it)     # rest：循环收集剩余到 MakeArray+ArrayPush
while ok: rest.push(item); (item, ok) = IterateNext(it)
```

- 对象模式：`prop_get` + `undefined` 检查 + 默认值；rest 属性需要
  "自有可枚举键 - 已取键"（复用 `Object.keys` 静态方法 + `in` 判断）。
- 嵌套模式：递归展开，被解构的中间值本身来自上一级绑定。
- **参数解构**：`lower_function_inner` 序言里把解构模式展开为
  `load_arg(i)` 起始的绑定序列；模式中带默认值时先做 undefined 检查。
- **声明 vs 赋值目标**：声明走 `symbols.insert`；赋值目标（`[a,b] = arr`）
  对每个叶子目标复用 `lower_assignment` 的目标机制（含成员目标）。
- 需要新增的辅助：`lower_binding_pattern(pattern, value) -> ()`
  （值已求出）与 `lower_binding_pattern_with_source(source_expr) -> ()`
  （值由模式自身驱动迭代）。

**与 `global_names` 的交互**：叶子目标是脚本级全局时，复用 T-全局修复的
`StoreEnv` 路径（见 `lower_assignment` 的 global 分支）。

### 3.4 展开 / 剩余

**核心难点：当前调用约定是静态 argc**（`Call func, argc` / `CallEx callee, argc`）。
展开使参数数量运行期才知道。

**设计：新增 `CallExS`（spread call）家族**：

| 新指令 | 语义 |
|--------|------|
| `CallExS { callee, args: ArrayValue, result }` | 调用，参数取自数组值（数组即参数列表） |
| `NewS { dst, constructor, args: ArrayValue }` | 构造调用，同上 |

- lowering：先求值所有参数到中间值，展开元素通过迭代写入一个临时
  `MakeArray` 数组；非展开参数照常 `ArrayPush`。最终该数组就是参数列表。
- VM：`CallExS` 复用 `invoke(callee, this, args_vec, module)` ——
  `invoke` 已接受 `&[Value]`，把 `ArrayObject` 展开成 `Vec<Value>` 即可；
  `frame_argc` 记录动态个数，`arguments` 对象自动正确。
- **剩余参数**：不需要新指令。`lower_function_inner` 序言中，
  rest 参数绑定 `MakeRest(i)`——新增轻量指令：把 `[rbp-argc .. rbp-i-1]`
  的实参收进新数组（`frame_argc` 已知动态个数）。
- 数组字面量展开 `[a, ...b, c]`：逐元素 `ArrayPush`，展开元素走迭代循环
  push（T1 机制）。
- `CallMethod` 的方法调用展开（`obj.m(...args)`）走 `CallMethodS`
  或降级为 `tmp = obj.m; CallExS`（推荐后者，少一条指令）。

### 3.5 模板字符串

现有 `lower_template_literal` 丢弃 `expressions`，重写为：

```
`a${x}b${y}`  →  result = "a"
                 result = ToStringConcat(result, ToString(x))
                 result = ToStringConcat(result, "b") ...
```

**关键语义点**：不能用 `Addx` —— `(1).toString()` 语境下数字相加是算术。
新增 `Opcode::ToString { dst, src }`（ES ToString：对象走 ToPrimitive("string")
→ 已有的 `to_primitive(hint="string")`）。拼接用 `Addx`（此时至少一侧是字符串）。

- quasis 与 expressions 严格交错（现有实现只取 quasis 是错的）。
- Tagged template（``tag`x${y}` ``）：M1 降级为
  `CallExS(tag, [stringsObj, y, ...])`，`stringsObj` 为普通数组 +
  `raw` 数组属性（不做冻结与 call-site 缓存，27 个测试中先解锁基础部分）。

### 3.6 默认参数

在 `lower_function_inner` 的参数绑定段展开：

```
function f(a, b = a * 2, c = b + 1)
  ↓ (序言，位于入口块)
v_a = load_arg 0
v_b = (v_a 相关默认表达式可引用之前的参数)
if load_arg 1 is undefined: v_b = a * 2  else: v_b = load_arg 1
...
```

- CFG 展开：每个带默认值的参数产生一个 `if undefined` 分支（入口块 →
  default_blk / bind_blk → merge_blk），或者用 `br_if` 线性排列 ——
  **必须保持 CFG 线性**，避免入口块分叉影响 SSA 序言约定（`load_arg`
  必须仍在入口块）。
- 默认表达式作用域：只能引用**之前**的参数与外层捕获（TDZ 之后参数
  视为未绑定 —— M1 以 symbols 顺序自然实现，不额外建模 TDZ 错误）。
- 与参数解构组合：`lower_binding_pattern` 的默认值机制与 3.3 相同，
  入口是 `load_arg(i)` 而非迭代。

---

## 4. IR / 字节码变更清单

| 变更 | 类型 | 说明 |
|------|------|------|
| `MakeIterator { dst, src }` | 语义完善 | VM 桩 → 双路径实现 |
| `IterateNext { iter, item, has_next }` | 语义完善 | VM 桩 → 双路径实现 |
| `IteratorClose { iter }` | 新增 | 慢路径调 `return()` 并吞异常；快路径 no-op |
| `ToString { dst, src }` | 新增 | ES ToString（模板字符串 / 其余场景复用） |
| `MakeRest { dst, from_index }` | 新增 | 剩余参数：收集 `[rbp-argc, rbp-from_index)` 实参 |
| `CallExS { callee, args, result }` | 新增 | 动态参数调用 |
| `NewS { dst, ctor, args }` | 新增 | 动态参数构造 |

每条新指令需同步修改：`defined_and_used` / `Display` / `ssabuilder` 重命名分支 /
`codegen` 发射 / `bytecode.rs` Opcode + Display。**此清单是 codegen 侧的完整
改动面**；`regalloc` 无需改动（活跃性修复后组分配对新指令透明）。

---

## 5. 任务分解（依赖序）

| # | 任务 | 依赖 | 主要改动 | 验收 |
|---|------|------|----------|------|
| T1 | 迭代器协议基座 | — | `vm/iterator.rs` 新建；MakeIterator/IterateNext 完善；Symbol.iterator 注册 | 手动迭代数组/字符串/用户对象；`assert.throws(TypeError)` 覆盖不可迭代对象 |
| T2 | `for-of` + `for-in` | T1 | `lower_for_of` / `lower_for_in`；`IteratorClose` | for-of 套件 ≥ 50%（751 测试）；for-in ≥ 60%（115） |
| T3 | 模板字符串插值 | — | 重写 `lower_template_literal`；`ToString` 指令 | template-literal 套件 ≥ 80%（76 测试） |
| T4 | 默认参数 | — | `lower_function_inner` 序言 CFG 展开 | 全部 default-parameters 相关用例 + 抽样回归 |
| T5 | 剩余参数 | T1 | `MakeRest`；`FuncParam` 携带 rest 标记 | rest-parameters 用例；`arguments` 与 rest 共存 |
| T6 | 展开（调用/数组/new） | T1,T5 | `CallExS`/`NewS`；lowering 参数收集 | spread 相关 331 测试 ≥ 50% |
| T7 | 数组解构 | T1,T4 | `lower_binding_pattern` 族函数；声明/赋值/参数/catch 四入口 | destructuring 122 测试 ≥ 60% |
| T8 | 对象解构 + rest 属性 | T7 | 对象模式展开；自有键枚举辅助 | 与 T7 合并验收 |

**提交节奏**：每个 T 独立提交（保持 `92897b3` 之后的可回退性），
提交信息注明解锁的测试规模。

---

## 6. 风险与对策

| 风险 | 影响 | 对策 |
|------|------|------|
| 解构展开产生大量 IR，寄存器压力上升 | 分配失败/性能下降 | 活跃性已修复，理论上正确；回归时对比 `BIUJS_FLUSH=1`；必要时恢复 `spill_all` 路径 |
| `CallExS` 动态参数与 `collect_call_args`/`arguments`/`frame_argc` 交互 | 参数错位 | 复用 `invoke` 单一入口（它已统一处理 `frame_argc`）；冒烟用例覆盖 `arguments.length` |
| 慢路径迭代重入用户代码（`Symbol.iterator`/`next` 内再触发迭代） | 重入借用冲突 | `find_descriptor`/`invoke` 已是可重入模式；快路径不受影响 |
| `for-of` 迭代器 close 的异常路径 | try 包裹抛出时泄漏 close 调用 | 先保证 break/正常路径（test262 大多数用例），异常路径单独子任务 |
| `let` 循环变量按轮捕获 | 少数闭包用例失败 | 已知简化，在 runner 备注并允许失败；M4 前不阻塞 |
| `for-in` 键顺序 | 字符串键按插入序即可满足多数用例 | 复用 `Object.keys` 现有实现，不引入整数键排序规范细节 |

---

## 7. 验证与回归策略

1. **每任务三级验证**：
   - 单元/feature 集成测试（`tests/features/`，断言语义而非实现）；
   - 对应 test262 套件定向跑（`TEST262_SUITES=...`）；
   - 提交前全量回归，通过数不得低于上一提交。
2. **runner 配套改动**（随最后一个 T 提交）：
   `UNSAFE_FEATURES` 移除 `for-of`/`destructuring-binding`/`spread-syntax`/
   `rest-parameters`；新增 `UNSKIP` 机制按套件粒度启用。
3. **性能护栏**：T6 完成后跑一次全量计时（当前 ~40s），劣化 > 2× 需要定位
   （怀疑点：慢路径迭代每步的 `invoke` 开销、解构展开的 IR 膨胀）。

---

## 8. 对既有文档的修订承诺

M1 完成时同步更新：
- `docs/es6-feature-support.md`：`for...of`/`for...in`/解构/展开/剩余/默认参数/
  模板插值 从 "Out of scope" 移入正文表格（✅/⚠️），并修正
  "No `arguments` object" 一节（`arguments` 已实现）；
- `docs/biu-js-engine-architecture.md`：架构图 Built-ins 迭代器条目、
  IR 指令清单补充；
- 新增 `docs/iterators.md`（T1 交付时）：双路径迭代器设计、
  `NativeIterator` 状态机、与 SEH 的交互。
