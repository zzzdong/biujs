# biujs 架构

照**当前实现**写（2026-10-11）。想看"当初为什么这么设计"翻 `docs/archive/`。
本文只描述代码现在长什么样，以及哪些地方是已知的结构性弱点。

## 1. 整体流水线

```
JS 源码
  │  oxc_parser / oxc_semantic          src/compiler/parser.rs, semantic.rs
  ▼
AST ── 符号表 ──────────────────────────  src/compiler/symbol.rs
  │  lowering（JSASTLower）               src/compiler/lowering/
  ▼
IR（SSA 形态 + CFG）───────────────────── src/compiler/ir/{builder,ssabuilder,cfg,instruction}.rs
  │  寄存器分配                            src/compiler/regalloc.rs
  │  指令选择 / 布局 / patch 回填           src/compiler/codegen.rs
  ▼
字节码：Module { Vec<CodeBlock>, … }      src/bytecode.rs
  │
  ▼
虚拟机（寄存器式，单步循环）               src/vm/
```

- `Module` 里每个函数一个 `CodeBlock`；运行期一律读**函数自己的**属性（M1 的成果）。
- 指令表 90 条，`Kind` 分类：`Call` / `Jump` / `Return` / `Throw` / `Suspend` /
  `Bookkeeping`（现只剩 `AddC`/`SubC`）/ 其余 `Normal`。表本身是"漏登记即编译错误"
  的（`Kind::kind()` 没有兜底分支）。

## 2. 虚拟机

`src/vm/mod.rs`（**6288 行**，已从 11269 行拆出 —— 见下方"拆分现状"），外加同目录的子模块：

- `src/vm/exec.rs`（**3404 行**）：`step`（`Halt` / `Ret` 之外全部指令用它分派）、`to_primitive`、`enter_call` / `invoke*` / `open_frame` / `drive_bytecode_frame`、`json_*`、`run_instruction` 与 17 个 `run_<group>`。
- `src/vm/proxy.rs`（748 行）：`Proxy` 对象与 `proxy_static_trap` 等 9 个静态 trap 钩子。
- `src/vm/builtins_dispatch.rs`（1083 行）：按名字字符串的内建分派（arity 表 / 前缀约定 / 旁路名单）。

> 拆分现状（2026-10-11）：P0-4 已拆完三步 —— `run_instruction` 的 1740 行巨型 match 先按语义切成 17 个 `run_<group>`，再把 Proxy（`proxy_static_trap` 等）与内建分派搬出，最后把整个执行循环（`step` + `run_*`）搬进 `exec.rs` 子模块。mod.rs 由 11269 行降到 6288 行。逻辑未改，全量零回退（16567 / 7843 / 3141，护栏 7/0/22）。

### 2.1 状态

```rust
struct State {
    data_stack: Vec<Value>,        // 值栈，rsp 索引
    registers: [Value; 19],        // 寄存器堆 + 单独的 Rv（返回值）
    frames: Vec<Frame>,
    seh_stack: Vec<SehRecord>,
    closure_var_stack: Vec<HashMap<String, Value>>,
    globals / global_object / script_env / promise_jobs / …
    rsp: usize,
    rbp: usize,
}
```

### 2.2 帧

`Frame` 持有：`pc`、`base`、`rsp`、`this`、`this_state`、`func`、`argc`、
`construct`、`new_target`、`closure_depth`、`seh_depth`。

原先散在七条平行栈（`this_stack` / `function_stack` / `frame_argc` / …）里的东西
已经收进帧；`closure_var_stack` 与 `seh_stack` 仍是两条独立的栈，由帧里的两个
水位字段记录回退位置。

**两条派生不变量**（这是近几批重构的核心成果）：

- `rbp` = `frames.last().base` —— 只在**开帧**时写一次；
- 退栈由 `unwind_frames_to(frame_depth, closure_depth, seh_depth)` 一处完成，
  它截断三条栈并把 `rbp` 定到**留下来的那一帧**的 `base`、把 `rsp` 定到
  **被弹出的那一帧**的出生 `rsp`（这个区别很关键：取错了就静默错）。

三个地方**不能**用派生值、必须用快照（跨生成器挂起/恢复边界，几何会挪）：
`handle_throw` 的 `SehRecord.saved_rbp/rsp`、`restore_execution_state`、
`drive_bytecode_frame`。它们的赋值都放在 `unwind_frames_to` **之后**（派生是默认，
快照是显式覆盖）。

### 2.3 异常

- 编译期给出 `EhRegion { start, exits: Vec<usize>, catch, finally }` —— 注意是
  **出口集合**而不是 `[start, end)` 区间，因为 RPO 布局下嵌套 try 的 pc 顺序不表达
  嵌套关系。
- 运行期靠 `seh_stack` + `SehRecord`（含 `saved_rsp` / `saved_rbp` /
  `saved_closure_depth` / `saved_frame_depth` / `finally_pc` …）。

### 2.4 生成器 / async

帧可以被挂起（`save_execution_state` → `SavedExecutionState`）再恢复；
`invoke_boundaries` 用来判定"异常算帧内还是帧外"。**深度必须在装帧之前取**——
这条教训在历史上出现过四次。

## 3. 值与对象

```rust
pub enum Value {
    Undefined, Null, Bool(bool), Number(f64),
    String(Rc<String>), Symbol(Rc<SymbolData>),
    Object(Gc<Object>),            // P1：8 B 瘦句柄，见 §3.1
    Function(u32),                 // 内部：按 id 引用字节码函数，不是 JS 可见类型
}
```

- **对象：`Object` 是具体枚举**（20 个变体，对应原 `JSObject` 的 20 个实现体；
  `dyn` 已消失、`Sized`）。`ObjectKind` 保留供 `kind()` 使用。
  多数具体对象内部包一个 `OrdinaryObject` 做属性存储。
- `Value` = **16 B**（原 24 B），`Gc<Object>` = 8 B。

### 3.1 堆与句柄（`src/vm/gc.rs`，P1）

```rust
pub struct Heap { chunks: Vec<Vec<RefCell<Object>>>, bytes: Cell<usize> }
pub struct Gc<T> { ptr: NonNull<RefCell<T>> }   // Copy，无 Drop、无引用计数
```

- **堆归 `VM` 持有**（本引擎里 VM 即 runtime），VM 析构时**整片释放**。
  不引入 `thread_local` 全局堆、不设 fallback 堆：只有**分配**需要接触堆，
  一律显式传 `&mut Heap`（现有 117 处形参）；`Gc::borrow()` 直接解引用、不碰堆。
- `RefCell` 从各对象类型内部**上移到堆槽**，所以 235 处 `borrow()` 调用文本不变。
- **`Object` 尺寸 = 最大变体**，故大变体必须装箱（实测：`GeneratorObject` 曾内联
  `SuspendedFrame` 撑到 624 B，装箱后 ≤256 B）。`gc.rs` 有断言守着这个上限。
- **P1 不回收**。环（`prototype` ⇄ `constructor` 等）不再需要手工断——它们都在同一个
  堆里，随堆整片释放。`Builtins::teardown` 那条**写死的 23 项断环清单已删除**
  （漏过一次的实际后果：DataView 没进清单 ⇒ 每条用例漏一整个 realm ⇒ 撞穿 4 GB
  `ulimit`、进程 SIGABRT）。
- **契约**：`Gc` 无引用计数，**任何 `Gc`（及其所在的 `Value`）不得活过它所属的
  `Heap`/`VM`**。`src/lib.rs::eval` 是已知触发点，用 `mem::forget(vm)` 兜住。
- 设计（含 P2 的 mark-sweep、根集、安全点、不变量清单）见 `docs/GC-DESIGN.md`。

## 4. 内建对象

`src/builtins/` 20 个文件。`mod.rs` 负责注册全局与原型。

**分派是按名字字符串做的**：`call_native_by_name` / `call_prototype_method` 里
`match name`。由此派生出 arity 表、`__proto__xxx` 前缀约定、`vm_handled_static`
旁路名单等一整套约定 —— 这是一处已知的设计债（对应"调用分派应该走 entry point"）。

## 5. 已知的结构性弱点（下一阶段的输入）

按"改了能拿到什么"排序：

| # | 弱点 | 后果 |
|---|---|---|
| 1 | **P1 已有堆与句柄，但还没有回收器**：`Heap` 只在 VM 析构时整片释放，单次 `run` 内不回收 | 临时垃圾一直留到 run 结束 ⇒ 单用例内存峰值上升（实测 `TEST262_MEMORY_MB` 须 256→384 才维持原头条）。正解是 P2 的 mark-sweep，见 `docs/GC-DESIGN.md` |
| 2 | 闭包是 `HashMap<String, Value>` 的**创建时快照** | "闭包共享可变单元"不成立（`counter()` 仍返回 `0,0,0`）—— 这是**语义 bug**，不是性能问题 |
| 3 | 内建分派按名字字符串 | arity 表 / 前缀约定 / 旁路名单；加一个原型方法要动多处 |
| 4 | `Value::Function(u32)` 双表示 | `strict_eq` 特判、`materialize_function`、`callable_func_id` |
| 5 | `src/vm/mod.rs` 曾 11269 行（现已拆到 6288 行 + `exec.rs` 3404 行） | 单文件过大，改动定位与 review 都困难 |
| 6 | 属性存储是 `HashMap<String, Value>` | 属性访问是哈希查找；没有 shape / 内联缓存 |

> 说明：`Value` 是否要做 NaN-boxing（8 B 定长）曾经被列为里程碑。**现已判定为可选**：
> 它是纯性能项，不是正确性所需，且依赖"先有堆与句柄"。详见 `docs/PLAN.md`。
