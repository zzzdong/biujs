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
  │  帧槽分配（内存驻留）                  src/compiler/frame_slots.rs
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
- `src/vm/native.rs`（82 行）：**内建实现载体** `NativeEntry`（函数对象自带实现）与唯一调用入口 `VM::invoke_native`。
- `src/vm/builtins_dispatch.rs`（3963 行）：仍在按名字字符串分派的那部分内建（arity 表 / 前缀约定 / 旁路名单），**正在被 `native.rs` 逐族取代**。

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

- **对象：`Object` 是具体枚举**（19 个变体，对应原 `JSObject` 的实现体；
  `dyn` 已消失、`Sized`）。`ObjectKind` 保留供 `kind()` 使用。
  多数具体对象内部包一个 `OrdinaryObject` 做属性存储。
  内建原型对象（`Object.prototype` / `Array.prototype` / …）不再有独立变体：
  它与 `Ordinary` 的差别只有 `class_name`，而默认值就是 `"Object"`。
- **对象层只做 own 属性描述符**：`JSObject` 的 15 个方法（`property_get` /
  `define_property` / `own_keys` / `get_prototype` / …）都只看"这一个对象"，
  不执行用户代码。ES 的抽象操作（`[[Get]]` / `[[Set]]` / `[[HasProperty]]` /
  `[[Delete]]`）在 VM 侧，因为 accessor 与 Proxy trap 需要重入调用。
  `[[Get]]` / `[[Set]]` / `[[HasProperty]]` 的链式实现是
  `VM::object_get_chain` / `set_member` / `object_has_property`，**每一跳都先检查
  Proxy**（链上任意位置是 Proxy，它的 trap 就接管整个操作）。`prototype.rs` 里的
  `internal_get` / `find_descriptor` / `internal_has_property` 是纯描述符 walk，
  只给"不需要执行用户代码"的内部读取用 —— 拿它做 JS 可见的 `[[Get]]` 会漏掉链上
  的 trap。`JSObject::has_own_property` 名字里带 `own` 也是同一件事：它只查自有
  属性，ES 的 `[[HasProperty]]`（含继承）是 `VM::object_has_property`。
- **`[[SetPrototypeOf]]` 只有一份判定**：`ObjectHeader::set_prototype` 返回 bool
  （相同 ⇒ true；不可扩展 / 冻结 ⇒ false），`Object.setPrototypeOf` 把 false 变成
  TypeError、`Reflect.setPrototypeOf` 原样返回；环检测在
  `prototype::prototype_chain_contains`。
- **属性操作的错误是类型化的**：`property_set` / `define_property` 返回
  `Result<bool, PropertyError>`（`PropertyErrorKind::{Type, Range}`），不是
  `String`。绝大多数违规是 TypeError，唯一的 RangeError 来自 `ArraySetLength`
  （`arr.length = -1`、`Object.defineProperty([], "length", {value: -1})`）。
  以前这条分类靠 `"RangeError: "` 前缀穿过 20 个 `impl JSObject`，任何一次
  `format!` 拼接都会静默丢掉它；现在 `From<PropertyError> for RuntimeError`
  是唯一还原点。
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

**分派正在从「按名字字符串」迁到「函数对象自带实现」**（`src/vm/native.rs`）：

```rust
// 注册时把实现放进函数对象；调用点只做一次间接调用
NativeFunctionObject::new("Math.max").with_entry(NativeEntry::Heap(impl_fn))
// exec.rs / CallMethod 的唯一入口
VM::invoke_native(callee, this, args, module)
```

这么改的理由：分派的键必须是**函数对象身份**，不能是接收者的类型 ——
`Array.prototype.slice` 可以被取出挂到任意对象上调用，规范里它与
`String.prototype.slice` 是两个不同的函数对象。按名字分派把这个身份压成了字符串，
于是归属（`__map_method__` / `Owner.prototype.x`）、捕获状态（`__promise_resolve__<id>`）、
arity、`name` 显示串各自需要一个解码器，四张表 + 三次前缀 strip 都是同一个根因。

已迁移：`Math`、`console`（全部方法）、`Date`（原型方法与 `Date.now/parse/UTC`）、
`Number.prototype` 与 `Boolean.prototype`（含 `Number.prototype.toString.length === 1`
的显式 arity）、`Promise` 六个静态方法与 `Promise.prototype.then/catch/finally`（首个
`NativeEntry::Vm`：`builtins/` 只登记 `VmSlot` 符号，`vm/native.rs` 在注册期
解析成函数指针）、`Object.assign`、`Array.isArray/from/of`、
`String.fromCharCode/fromCodePoint`、`Object.defineProperty/defineProperties/create/
getOwnPropertyDescriptor/hasOwn` 与 `Reflect.has/deleteProperty/
getOwnPropertyDescriptor/defineProperty`（九个名字共用一个
`VM::object_static_vm`：注册期给定 ToPropertyKey / Proxy trap / 描述符策略）、
`String.raw`、`JSON.stringify/parse`、`TypedArray.from/of`、`Map.groupBy`、
`$262.detachArrayBuffer`、`Object.keys/getOwnPropertyNames/getOwnPropertySymbols/
getPrototypeOf/setPrototypeOf/isExtensible/preventExtensions`（后七个与前面
九个共用 `VM::object_static_vm`）、`Proxy.revocable`、全部 `Reflect.*` 静态方法，以及剩余纯静态方法
（`Object.values/entries/fromEntries/is/freeze/seal/isFrozen/isSealed`、`Number.*`、
`Symbol.for/keyFor`、`Error.isError`、`ArrayBuffer.isView`）。
`call_native_by_name` 的 ToPropertyKey 名单已清空，`Object.prototype.toLocaleString`
那条从未被注册名命中的死分支也已删除。`CallMethod` 的早段静态探测（连同
`defer_to_native_dispatch` 名单与每次调用的 `format!("Owner.method")`）、
`call_native_by_name` 的整个 `name.contains('.')` 分支、死代码
`call_builtin_method` 全部删除：静态方法现在只有「属性解析 → `invoke_native`
→ entry」一条路。清理时发现 `Object.getOwnPropertyDescriptors` 以前只靠早段
探测的按名兜底才能调用（原型上压根没注册），补上 Heap entry 后反而多过 4 条。`catch` 对非 promise 接收者的处理也因此
简化：用户自己的 `catch` 方法由属性解析天然优先，不再需要在分派口特判。
原型方法族开始迁移：`Array.prototype.entries/keys/values` 三者各带一个
`VmSlot`，同名方法在 Map/Set/TypedArray 上是**不同函数对象**，靠函数身份
区分，不再由分派口按接收者形状猜。12 个迭代器工厂（Array/Map/Set/TypedArray
各 3 个）已完成。迁移中发现 `VM::make_iterator_with` 原来靠
`native_function_name == "__proto_method__values"` 判断「是不是通用的 array-like
快路径」——迁移后四个家族名字撞车，Set 的展开与 `union` 会错误地走 array-like
得到空数组；改成与 `Array.prototype.values` 的**函数对象身份**比较后修复；随之删掉
`call_native_by_name` 里三处「按 `keys/values/entries` 名字 + 接收者形状猜归属」
的死分支（约 26 行）。Array 回调族的第一步已完成：`try_array_callback_method` 的 `method: &str`
换成穷尽的 `ArrayOp`，`CALLBACK_METHODS` / `SPECIES_METHODS` 两张名单与
`match method` 的字符串分支全部消失（名字只在错误信息里由 `as_str()` 取）。
`ArrayOp` 已接到 `NativeEntry::VmArray`：`Array.prototype` 全部 35 个方法
（27 个算法 + 8 个读族/字符串族特殊方法）注册时带 `VmSlot::ArrayProto(op)`，
`VM::array_proto_host` 取代 `call_native_by_name` 的 `"Array.prototype."` 分支
（`ToObject` → `toString`/`concat` 特例 → 参数强转 → species/回调/读族 → builtin 兜底）。
`CallMethod` 的三处早段拦截已删除。第一次直接删时 Array 掉 3 条，定位到原因是
host 在回调/读族之前先做了参数强转，导致 `copyWithin` 的 `valueOf` 多跑一次、
`slice` 走了 species 而非读族；把 host 顺序改成「回调 → 读族 → 强转 → species
→ builtin」后 Array 变成 2579/49（基线 2577/51，+2/-2）。全量 17710/833
（上次 17699/844）。护栏 memory 由 2 变 3：多的一条是
`TypedArray/copyWithin/coerced-values-end-detached-prototype.js`，与本族改动
无关但仍需核对。

`String.prototype` 全部 32 个方法也已迁入 `NativeEntry::VmString(StringOp)`：
`VM::string_proto_host` 统一做协议分派（`@@match`/`@@replace`/…）、`ToString(this)`、
参数 `ToPrimitive` 与内建兜底；`call_native_by_name` 的 `"String.prototype."`
分支和 `CallMethod` 的 `try_string_callback_method` / `try_match_all_method`
早段拦截已删除，原语字符串接收者改走同一个 host。String 套件 991/64（基线
987/68，+4/-4）；全量仍是 17710/833，逐套件相对基线**全部为改善、无回退**。
迁移中的状态是
`NativeFunctionObject.entry == None`：
这类内建仍走 `call_native_by_name` / `call_native` / `call_static_method` /
`call_prototype_method` 四条旧路径，每迁一族就少一批，最后一条回退与四张表一起删。

## 5. 已知的结构性弱点（下一阶段的输入）

按"改了能拿到什么"排序：

| # | 弱点 | 后果 |
|---|---|---|
| 1 | **P1 已有堆与句柄，但还没有回收器**：`Heap` 只在 VM 析构时整片释放，单次 `run` 内不回收 | 临时垃圾一直留到 run 结束 ⇒ 单用例内存峰值上升（实测 `TEST262_MEMORY_MB` 曾须 256→384（P2 mark-sweep + 宿主侧入根落地后已**降回 256** 且回到原头条））。正解是 P2 的 mark-sweep，见 `docs/GC-DESIGN.md` |
| 2 | 闭包是 `HashMap<String, Value>` 的**创建时快照** | "闭包共享可变单元"不成立（`counter()` 仍返回 `0,0,0`）—— 这是**语义 bug**，不是性能问题 |
| 3 | 内建分派按名字字符串（**迁移中**，已迁 `Math` / `console` / `Date` / `Number.prototype` / `Boolean.prototype` / `Promise`（六静态方法 + `then/catch/finally`） / `Object.assign` / `Array.isArray,from,of` / `String.fromCharCode,fromCodePoint` / 五个 `Object.*` 与四个 `Reflect.*` 静态方法 / `String.raw` / `JSON.*` / `TypedArray.from,of` / `Map.groupBy` / `$262.detachArrayBuffer` / 七个剩余 `Object.*` 静态方法 / `Proxy.revocable` / 全部 `Reflect.*` / 剩余纯静态方法（`Object.values,entries,fromEntries,is,freeze,seal,isFrozen,isSealed`、`Number.*`、`Symbol.for,keyFor`、`Error.isError`、`ArrayBuffer.isView`） | arity 表 / 前缀约定 / 旁路名单；加一个原型方法要动多处。目标形状见 §4 的 `NativeEntry` |
| 4 | `Value::Function(u32)` 双表示 | `strict_eq` 特判、`materialize_function`、`callable_func_id` |
| 5 | `src/vm/mod.rs` 曾 11269 行（现已拆到 6288 行 + `exec.rs` 3404 行） | 单文件过大，改动定位与 review 都困难 |
| 6 | 属性存储是 `HashMap<String, Value>` | 属性访问是哈希查找；没有 shape / 内联缓存 |

> 说明：`Value` 是否要做 NaN-boxing（8 B 定长）曾经被列为里程碑。**现已判定为可选**：
> 它是纯性能项，不是正确性所需，且依赖"先有堆与句柄"。详见 `docs/PLAN.md`。
