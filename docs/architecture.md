# BiuJS 实现地图（当前状态）

> **这份文档解决两个问题**：接到一个任务后「该改哪里」、以及「哪里会咬人」。
> **不解决**：为什么这样设计。设计蓝图见 `biu-js-engine-architecture.md`（2026-09-21 的早期设计文档，
> 其中"排除 `var`/`arguments`"一类表述**已过时**，以本文件为准）。
>
> 初次接手请从 `handover.md` 开始。计划与批次见 `es6-conformance-phase2.md`。
>
> **行号会漂**：它们是 2026-09-25 的快照。定位函数请用 `grep -n "fn 名字" 文件`，不要记行号。

---

## 1. 端到端管线

```
源码 ──▶ parse_js ──▶ Program ──▶ SemanticAnalyzer ──▶ JSASTLower ──▶ IrUnit(CFG)
                                                                        │
                                                    SSABuilder(SSA + throw→handler)
                                                                        │
                                                        Codegen + RegAlloc ──▶ Module(Bytecode)
                                                                        │
                                                                   VM::run
```

| 阶段 | 入口 | 产物 | 要点 |
|------|------|------|------|
| 驱动 | `Compiler::compile`（`compiler/mod.rs:35`） | `Module` | 解析 → 语义 → 降级 → 逐函数 SSA → 代码生成 |
| 解析 | `parse_js`（`compiler/parser.rs:153`） | oxc `Program` | `Parser::new(..., SourceType::cjs())`；随后跑 **oxc 语义构建器**（`:170`，`check_syntax_error(true)`）补齐 strict 早期错误 —— 那批检查**不在解析器里** |
| 语义 | `SemanticAnalyzer::analyze`（`compiler/semantic.rs:58`） | `Vec<CompileError>` | 目前只查 `const`/`let` 重赋值。**注意它是编译期静态检查，覆盖不到解构这类间接写入** |
| 降级 | `JSASTLower`（`compiler/lowering/mod.rs:69`） | IR | 3687 行，最重的编译器文件。语句 `lower_statement:207`、表达式 `lower_expression:1251`；try/SEH `lower_try:1126`；class `lower_class:251`；解构 `bind_pattern:2599`；`yield` `:1286` |
| IR | `ir/instruction.rs:183` | `Instruction` 枚举 | 配套 `builder.rs`（`InstBuilder` 发射助手）、`cfg.rs`（块/支配/布局）、`ssabuilder.rs`（SSA + `into_throw_to_handlers:878`，把 `throw` 连到 handler 块） |
| 代码生成 | `Codegen::generate_code`（`compiler/codegen.rs:47`） | `&[Bytecode]` | 与 `regalloc.rs`（线性扫描 + 溢出）配合 |
| 字节码 | `bytecode.rs` | `Module:10`、`Opcode:144`（**86 个**） | `Module` 还带 `func_info`（名字/arity）、`generators`、`derived_ctors`、**`exit_pc`**（每个函数末尾 `Ret` 的 pc，生成器恢复用） |
| 运行 | `VM::run`（`vm/mod.rs:190`） | 值 | `step:253` 只管 `Halt`/`Ret`（含 finally 分发与帧弹出），其余进 `run_instruction:1501` |

### 1.1 嵌套执行与哨兵 pc（理解 VM 的关键）

`step` 在 `pc` 越界时返回 `Ok(false)` 结束循环（`:254`）。因此"跑到帧结束"的实现方式是**把 pc 跳到 `module.instructions.len()`**（`resume_sentinel:4731`）：`Ret` 跳到 `return_pc`、`Yield` 跳到哨兵，嵌套循环随之退出。`invoke` 也用同一个哨兵当 `return_pc`。**任何"恢复一个帧"的代码都必须自己把这条还原清楚**。

---

## 2. 运行时模型

### 2.1 帧与栈

`State`（`vm/mod.rs:5424`）里的每一条栈及其用途：

| 字段 | 用途 |
|------|------|
| `data_stack` | 操作数栈 + 局部槽；`rbp`/`rsp` 是它的下标。参数在 `[rbp-(i+1)]`，缺参读 `Undefined`（`get_value_from_stack:5575`） |
| `ctrl_stack` | 每条 3 个 usize：`return_pc`、`saved_seh_depth`、`saved_closure_depth`。**这是"帧"的真正边界** |
| `registers: [Value; 19]` | 全局寄存器文件，**不随帧保存** —— 所以挂起/恢复必须自己快照（见 `SuspendedFrame.registers`） |
| `seh_stack: Vec<SehRecord>` | 异常处理器链（见 2.3） |
| `function_stack` / `function_val` | 当前被调函数（`current_function_id()` 用它查 `exit_pc`） |
| `this_stack` / `this_val` / `this_state` | `this` 及其"派生构造器是否已 `super()`"状态 |
| `construct_stack` / `new_target_stack` | `[[Construct]]` 与 `new.target` 透传 |
| `closure_var_stack` | 闭包捕获变量表；`saved_closure_depth` 决定恢复时截断到哪 |
| `frame_argc` | 每帧实参个数（`arguments` 物化用） |
| `invoke_boundaries: Vec<usize>`（VM 字段 `:119`） | Rust 驱动帧的 `ctrl_stack` 深度。`handle_throw:3804` **拒绝跨过它派发** —— 否则嵌套循环的栈记账会崩 |

`enter_frame(argc)`（`:5629`）做深度检查（`MAX_CALL_DEPTH = 512`）并压入 `this`/`function`/`argc`。

### 2.2 调用入口

`invoke` / `invoke_with_new_target`（`vm/mod.rs:577`/`:593`）是"从 Rust 里调 JS"的唯一入口：保存 pc/rsp/rbp/闭包深度/SEH 深度/this/构造状态（`:664`），**压入 `invoke_boundaries`**（`:677`），装帧，跑嵌套循环（`:726`），全部还原（`:736`）。

**硬性一致性要求**：`invoke` 必须镜像 `Call` opcode 的全部特例。已踩过的坑 —— `Call` 会检查 `module.generators` 并构造生成器对象，而 `invoke` 早期不检查，于是经 `invoke` 调用生成器函数会**立刻执行函数体**（并因此无界递归）。新增任何"调用前后有特殊处理"的情形（生成器、类构造器、bound 函数…）时，两处都要改。

### 2.3 异常（SEH）

- `SehRecord`（`:5673`）字段：`handler_pc`、`finally_pc`、`saved_rsp/rbp`、`in_finally`、`pending_exception`、`catch_executed`、`delayed_jump_target`、`delayed_return`、以及 `saved_{closure,ctrl,this,construct,new_target}_depth`。
- 压入/弹出：`Opcode::Try`（`:2680`）/ `Opcode::EndTry`（`:2711`）。
- `handle_throw`（`:3804`）：找最近的记录，`unwind_frames_to(...)` 截断各类栈，跳到 handler；`finally` 用 `in_finally` + `pending_exception` + `ResumeExc` 实现"跑完 finally 再继续抛/返回"。
- `ResumeExc`（`:3043`）：`delayed_return` 分支**必须显式跳回 `Module::exit_pc`**，不能顺着往下落 —— 它后面的指令是"正常完成"的续接，落下去会执行本应被 `return` 跳过的语句、并被出口块的 `Mov rv, undefined` 冲掉返回值。
- `finally` 的选取顺序：`Opcode::Ret` 扫描 `seh_stack` 时必须**最内层优先**（`.rev()`），因为 epilogue 只能收尾"栈顶那条"。

### 2.4 生成器（挂起 / 恢复）

| 方法 | 作用 |
|------|------|
| `create_generator:4201` | 只建对象，不跑函数体（`Call` 与 `invoke` 都走它） |
| `generator_next:4230` | `restore_generator_frame` → 把 `next(v)` 的值写进 `Yield` 的目标 → `jump(pc+1)` |
| `generator_abrupt:4360` | `return()`/`throw()`：**恢复函数体**再交付完成 —— `return` 跳到 `exit_pc` 让 finally 跑，`throw` 交给 SEH |
| `run_generator_frame:4547` | 跑嵌套循环并收尾；**推 `invoke_boundaries`** |
| `extract/restore_generator_frame` | 快照/还原挂起帧 |
| `SuspendedFrame`（`object.rs:1856`） | `data`、`argc`、`bp_offset`、`pc`、`this`、`function_val`、`closure_maps`、`registers[19]`、**`delegates`**、**`seh`** |

生成器相关的每一处改动都要问三个问题：**寄存器快照了吗**（全局寄存器文件）、**SEH 带上了吗**（`try` 里的 `yield`）、**`invoke_boundaries` 的深度是装帧前取的吗**。

### 2.5 迭代器

- `make_iterator:4856`：**先无条件做 `GetMethod(obj, @@iterator)`**（可观察：getter 抛错、方法被删除/替换），只有解析到**内置工厂**时才走数组/字符串的快路径。
- `iterator_next:4973`：JS 迭代器走 `invoke(it.next)`（每步一次调用，贵）；原生迭代器走 `native_next`（廉价）。
- `iterator_close:5049`：ES 7.4.6 三步 —— `return` 不可调用抛 TypeError、调用自身抛错吞掉、**正常返回但非对象抛 TypeError**。
- `NativeIteratorState`（`vm/iterator.rs:67`）：`Array{items,idx}`、`String{chars,idx}`、`Js{iterator}`。

### 2.6 对象与属性

- `JSObject` trait（`object.rs:28`）的 20 个方法：`kind`/`as_any(_mut)`/`property_get`/`property_set`/`define_property`/`property_delete`/`has_property`/`own_keys`/`get/set_prototype`/`is_extensible`/`prevent_extensions`/`is_frozen`/`freeze`/`is_sealed`/`seal`/`type_of`/`class_name`/`to_primitive`。
- 实现者与"各自拥有什么"：

| 类型 | 拥有 |
|------|------|
| `OrdinaryObject`（`:406`） | `properties: PropertyTable`、`prototype`、`extensible/frozen/sealed`、`class_name` |
| `ArrayObject`（`:780`） | **`elements: Vec<Value>`（密集存储）**、`properties`、`holes: BTreeSet<usize>`、`length_writable` |
| `FunctionObject`（`:1271`） | `func_id`、`name`、`captured_this` / `captured_new_target` / `captured_vars` |
| `NativeFunctionObject`（`:1476`） | `name`（**派发就靠它**）、`properties`（`length`/`name` 元数据，`install_metadata:1510`） |
| `GeneratorObject`（`:1890`） | `state`、`args`、`this`、`suspended: Option<SuspendedFrame>` |
| `NativeIteratorObject`（`iterator.rs:100`） | 迭代器状态 |

- `PropertyKey`（`property.rs:5`）= `Str(Rc<String>) | Symbol(u64)`；`PropertyDescriptor:45`；`ObjectKind:99`（20 个变体，`Map`/`Set` 等**目前只是占位**，无实现）。
- `prototype.rs`：`find_descriptor:83` / `internal_get:36` / `internal_set:110` / `internal_has_property:183` / `internal_delete:212`。**错误类型是 `String`**（见地雷 5）。

### 2.7 内置对象层

- `Builtins`（`builtins/mod.rs:306`）：各原型 `Rc<RefCell<dyn JSObject>>`；`register(globals):385` 装配全局 + `link_constructor_prototype:372`。
- 安装助手：`set_prototype_method:1344`、`set_prototype_method_arity:1358`、`mark_prototype_method:1377`（**只登记名字，由 VM 实现**）、`set_static_method:1232`。
- 运行时派发：`call_native_by_name`（`vm/mod.rs:1275`）**按 `NativeFunctionObject.name` 匹配**，处理 `call/apply/bind`、生成器/迭代器前缀、`PROTO_METHOD_PREFIX`；`call_prototype_method:818`、`call_native:669`。
- 元数据：arity 表 `builtin_arity`（`builtins/mod.rs:1276`）；显示名 `native_function_name:1392`；前缀常量 `PROTO_METHOD_PREFIX="__proto_method__"`（`:1262`）、`CLASS_CTOR_FLAG`（`:1260`）。

---

## 3. 任务 → 改动位置（拿到任务先查这张表）

| 任务类型 | 主路径 | 必须同步的地方 |
|----------|--------|----------------|
| 新增/修正语法 | `compiler/parser.rs`（语法由 oxc 负责，多半不用改）+ `compiler/lowering/mod.rs` | 需要新指令时见下一行 |
| **新增一条 VM 指令** | `bytecode.rs`（Opcode + Display）→ `compiler/ir/instruction.rs`（变体 + **操作数读写集** + Display）→ `compiler/ir/builder.rs` → `compiler/codegen.rs` → `src/vm/mod.rs`（handler） | `compiler/ir/ssabuilder.rs` 的 rename 分支；以及所有对 `Instruction`/`Opcode` 的穷尽 match（编译器会逐个报出来）。参考模板：`IteratorClose` / `RequireObjectCoercible`，五层不到 40 行 |
| 新增内置方法（**不需要**跑用户代码） | `builtins/<obj>.rs` 注册 + `builtins/mod.rs` 的 arity 表 | —— |
| 新增内置语义（**需要** `[[Get]]`/`[[Set]]`/`Construct`/回调） | `src/vm/mod.rs` 的派发层（`call_native_by_name` 之上的分支） | builtins 层做不到，别在那里硬写 |
| 属性 / 描述符语义 | `vm/property.rs` + `vm/prototype.rs` + 各对象的 `JSObject` impl | `builtins/object.rs` 的静态方法；**错误种类**受 String 通道影响（地雷 5） |
| 迭代协议 | `vm/iterator.rs` + `vm/mod.rs` 的 `make_iterator`/`iterator_next`/`iterator_close` | `compiler/lowering` 里 `for-of`/spread/解构的降级 |
| 生成器 | `vm/mod.rs` 的 `generator_*` + `vm/object.rs` 的 `SuspendedFrame` | `compiler/lowering` 的 `Yield` 降级 + `Module::exit_pc` 的记录 |
| **新增对象类型**（Map/Set/TypedArray…） | `vm/object.rs` 新增 struct + `impl JSObject`；`property.rs` 的 `ObjectKind` 加变体；`builtins/<x>.rs` + `Builtins` 字段 + `register` 装配 | 可迭代则加 `NativeIteratorState` 变体；`call_native_by_name` 的派发分支 |
| 严格模式早期错误 | 已由 oxc 语义构建器承担（`parse_js`） | runner 侧的 `onlyStrict` 前置 |
| 测试与度量 | `tests/test262_runner.rs`（`SUITES:500`、两套特性表 `:171`/`:206`、`should_skip:256`、`build_source:117`） | 计划书 §2.2 的"入册 + 解锁"联动纪律 |

### 3.1 示例：新增一个集合类型（Map / Set / TypedArray）的改动清单

`Map` / `Set` 目前**零脚手架** —— `ObjectKind` 里那两个变体只是占位，没有任何实现。按顺序动这些地方：

1. `vm/object.rs`：新增 `MapObject { entries: Vec<(Value, Value)>, prototype, properties, ... }`，`impl JSObject`。
   - 插入序用 `Vec` 保序（`Map` 规范要求插入序），查找走 `SameValueZero`。**别用 `HashMap` 直接当存储**：`SameValueZero` 与 `Hash` 语义不同（`-0`/`+0`、`NaN`），且要保序。
   - `get_prototype`/`set_prototype`/`property_get`/`property_set` 转发到内部的 `properties` + `prototype`（照抄 `OrdinaryObject` 的那几段）。
   - `kind()` 返回 `ObjectKind::Map`（那就得把 `property.rs` 里现有的占位变体用起来）。
2. `vm/iterator.rs`：`NativeIteratorState` 加变体（如 `MapEntries { items: Vec<Value>, idx }`），并在 `native_next` 里实现推进。若迭代器要"活"（规范要求 Map/Set 迭代器观察迭代期间的增删），就不能用快照，需要在状态里持有对象引用 + 索引 —— **这是本任务里最容易做错的一点**。
3. `builtins/map.rs`：`register_map_prototype(&proto)`（用 `set_prototype_method` / `set_prototype_method_arity`）+ `size` 的 getter（用 `define_property` 装访问器）。
4. `builtins/mod.rs`：`Builtins` 加 `map_prototype` 字段；`register()` 里建构造器、`link_constructor_prototype`、插入全局 `globals.insert("Map", ...)`；arity 表加各方法。
5. `vm/mod.rs` 的 `call_native_by_name`：加 `Map` 构造器与需要用户回调/内部方法的成员（`new Map(iterable)` 要走迭代协议 → **必须在 VM 侧**，用 `make_iterator` + `iterator_next`）。
6. `tests/test262_runner.rs`：从 `IN_SCOPE_PENDING` 删掉 `"Map"`，并把 `built-ins/Map` 加进 `SUITES`（§2.2 的联动纪律）。
7. `tests/features/`：至少覆盖 插入序 / `SameValueZero`（`-0`、`NaN`）/ `size` / 迭代 / 迭代期间的增删 / `new Map(iterable)`。

---

## 4. 地雷与不变量（每一条都踩过）

**错误种类会被 String 通道抹平**。`JSObject::define_property` / `property_set` / `prototype::*` 的失败是 `Result<_, String>`，调用点一律包成 `TypeError`。而 `ArraySetLength` 要求 `RangeError`。现在的做法：`RuntimeError::into_property_error()` 把 RangeError 渲染成 `"RangeError: <msg>"`，`from_property_error()` 还原。**新增任何走 String 通道的错误都要问：种类是否会丢。**

**builtin 层看不到原型链与访问器**。`builtins/*` 直接读 `ArrayObject::elements` 密集存储、走快照（`array_like_elements`），因此**绕过 `[[Get]]`/`[[Set]]` 与访问器**。凡语义要求"按 `[[Get]]` 取值 / 按 `[[Set]]` 写入 / 可能执行用户代码"的，必须上移 VM。已知欠账：Array 回调方法的泛型/访问器路径、`[...a]` 中数组元素的访问器、索引读写不经原型链。

**帧深度必须在装帧之前取**。生成器恢复、`invoke`、迭代器体都要把"调用方的 `ctrl_stack` 深度"记为边界（`invoke_boundaries`）。用装帧**之后**的深度会让本帧自己的 handler 被判成"帧外"，表现为异常穿透、控制栈下溢。这条教训出现过四次。

**`invoke` 与 `Call` opcode 必须行为一致**（见 2.2）。

**新增指令是五层改动**，别指望一处搞定（见 §3 第二行）。同时注意 `dyn-capture`：闭包捕获是**创建时值快照**，所以"在闭包里累加计数"这类写法在本引擎里不成立（写测试时尤其要注意；见 `dyn-capture.md`）。

**寄存器是全局的**。任何挂起/恢复都必须显式快照 `registers`（`SuspendedFrame` 已带）。

**脚本完成值不可靠**。`VM::run` 返回的不一定是"最后一个产生值的语句"的值（实测会返回中间表达式）。写 feature 测试**不要**靠末尾表达式取值，改为在 JS 里 `throw` 断言，用 `eval_js(..).unwrap()`。

**无界递归会以 SIGABRT 结束进程**。测试线程的 Rust 栈小于 CLI 主线程，引擎自己的深度护栏来不及报 `RangeError`。排查手段：`BIUJS_TEST262_TRACE=1` 逐条打印正在跑的测试路径，崩溃时最后一行就是元凶。

---

## 5. 命令行与验证入口

| 命令 | 用途 |
|------|------|
| `cargo test --release --lib` | 190 个单元测试 |
| `cargo test --release --test features` | 421 条语义断言（`tests/features/*.rs`，helper 在 `tests/helpers.rs`；**用 `eval_js`/`eval_number`/… 取值，注意上面的完成值地雷**） |
| `./scripts/phase2-status.sh` | **推荐**：一键跑三级验证并与基线快照对比，自动列出逐套件回退 |
| `ulimit -v 6000000; TEST262_FAILURES=0 cargo test --release --test test262_runner -- --nocapture` | 全量回归（**必须带内存上限**，见计划 §6.5） |
| `TEST262_SUITES=built-ins/Array cargo test ... --test test262_runner` | 定向跑（按 `SUITES:500` 里的目录名做子串匹配） |
| `TEST262_FAILURES=900 ...` | 打印每个套件的前 N 条失败，用于聚合根因 |
| `BIUJS_DUMP=1 ./target/release/biujs file.js` | 打印字节码（看帧布局、`Ret`/`ResumeExc` 排布时必用） |

环境变量总览：`BIUJS_DUMP`（`main.rs:35`）、`BIUJS_TRACE_ITER`（`vm/mod.rs:4858`）、`BIUJS_REG_VARS`（`codegen.rs:43`）、`BIUJS_TEST262_TRACE` / `TEST262_SUITES` / `TEST262_FAILURES`。

---

## 6. 代码规模（2026-09-25）

| 文件 | 行数 | 归属 |
|------|------|------|
| `vm/mod.rs` | 6327 | VM/State、调用、SEH、生成器、派发 |
| `compiler/lowering/mod.rs` | 3687 | AST → IR |
| `vm/object.rs` | 2029 | `JSObject` trait + 各对象类型 |
| `builtins/mod.rs` | 1410 | 内置装配与派发 |
| `compiler/codegen.rs` | 1099 | IR → 字节码 |
| `compiler/regalloc.rs` | 1063 | 活跃性 + 线性扫描 |
| `builtins/array.rs` | 1052 | `Array.prototype` |
| `compiler/ir/instruction.rs` | 1019 | IR 定义 |
| `bytecode.rs` | 962 | `Module` / `Opcode`(86) |
| `vm/value.rs` | 961 | `Value` 与转换 |
| `compiler/ir/ssabuilder.rs` | 881 | SSA + throw→handler |
| `builtins/object.rs` | 780 | `Object` 静态/原型 |
| `builtins/string.rs` | 575 | `String.prototype` |
| `compiler/ir/builder.rs` | 570 | 发射助手 |
| `builtins/json.rs` | 481 | JSON |
| `compiler/ir/cfg.rs` | 432 | CFG / 支配 / 布局 |
| `builtins/symbol.rs` | 347 | Symbol |
| `compiler/semantic.rs` | 301 | const 重赋值检查 |
| `vm/prototype.rs` | 234 | `[[Get]]`/`[[Set]]` 沿链遍历 |
| `vm/iterator.rs` | 208 | 迭代器协议 |
| `vm/property.rs` | 125 | `PropertyKey`/`Descriptor`/`ObjectKind` |
| `compiler/parser.rs` | 253 | oxc 调用 + 早期错误 |
| `compiler/symbol.rs` | 77 | `SymbolTable` |

合计约 26.8k 行。
