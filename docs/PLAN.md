# 下一阶段任务计划

起点是**当前实现**（`docs/ARCHITECTURE.md`），不是旧方案。旧方案在
`docs/archive/architecture-redesign.md`，只用来溯源，不再照着施工。

## 0. 现状一句话

test262 **84.06%**（16567 / 执行 19708）。剩下的 16% 绝大多数是**缺语义**
（BigInt、可增长 ArrayBuffer、Realm、生成器细节、`Symbol.toStringTag`…），
不是吞吐，也不是帧模型的问题。

## 1. 判据（决定顺序）

1. **先修语义 bug，再做性能优化**。84% 的天花板是"特性没实现"，不是"跑得慢"。
2. **先消结构性隐患，再加功能**。手工断环（`Builtins::teardown` 的 23 项清单）
   已经造成过一次全量崩溃，越晚改越贵。
3. **能独立验证的先做**。每一步都要能单独跑 `phase2-status.sh` 确认零回退。
4. **改动核心类型不要分批**（编译器会列全位点，半迁移成本更高）。

## 2. 任务清单

### P0 — 收尾与减负（小、独立、立刻能做）

| # | 任务 | 验收 |
|---|---|---|
| P0-1 | 删 `Instr::Call` 死 arm（唯一生产者 `IrBuilder::call_function` 已无调用方） | 编译通过、全量零回退 |
| P0-2 | `AddC`/`SubC` 退役：让帧真正拥有一段栈窗口（开帧时按 `stack_size` 划好，收帧自动回收） | 全量零回退；`Bookkeeping` 归零 |
| P0-3 | 删 `invoke_boundaries`；`tests/test262_runner.rs` 的 256 MB 栈恢复默认 | 全量零回退、护栏计数不增 |
| P0-4 | **拆分 `src/vm/mod.rs`** —— 已从 11269 行拆到 `mod.rs` 6288 + `exec.rs` 3404 + `proxy.rs` 748 + `builtins_dispatch.rs` 1083，逻辑未改、全量零回退（§3.4 收尾可选） | 见 §3 |
| P0-5 | 生成器两处自装帧与 `open_frame` 合并 | 全量零回退 |

### P1 — 消环：堆 + 句柄（M3）— **已完成（2026-10-12）**

**为什么先做它**：手工断环清单是事故源，且每加一个构造函数都要记着补一项。

实际形态与最初设想有几处出入，**以 `docs/GC-DESIGN.md` 为准**：

1. ✅ `Heap`（分块、不移动、只分配不回收）+ `Gc<T>`（`Copy` 句柄，无引用计数）。
   堆归 `VM` 持有，VM 析构时整片释放；分配一律显式传 `&mut Heap`
   （现有 117 处形参），**不用 `thread_local`、不设 fallback 堆**。
2. ✅ `Value::Object(Rc<RefCell<dyn JSObject>>)` → `Value::Object(Gc<Object>)`；
   `Object` 是**具体枚举**（20 变体），`dyn` 消失。`Value` 由 24 B 降到 **16 B**。
3. ✅ `RefCell` 从各对象类型内部**上移到堆槽**（原计划"386 处 `borrow()` 全部消失"
   改为"收拢到堆槽"）—— 于是 235 处 `borrow()` 调用**文本不变**，churn 小一个量级。
4. ✅ `Builtins::teardown` 的 **23 项断环清单整条删除**，连同 `VM::run` 与
   `Drop for VM` 里两处同类补丁。
5. ✅ `Object` 尺寸守门：`GeneratorObject::suspended` 装箱（624 B → ≤256 B）。

**没有做** `HandleScope` + `Rooted`：推迟到 P2 之前，见 `GC-DESIGN.md` §6.2
与 `REFACTOR-HEAP-GC.md` 阶段 B3。

**验收结果**：三级全绿（211 / 538 / 7）；全量 **16567 / 7843 / 3141（84.06%）**，
**无逐套件回退**。唯一漂移是内存护栏：堆在单次 `run` 内不回收，单用例峰值上升，
故 `TEST262_MEMORY_MB` 默认曾由 256 提到 384（成因已量化并定位：那 5 条是**预算**问题，
不是语义回退）。**P2 收集器 + 阶段 E 宿主侧入根完成后已降回 256** 且回到 16567
（护栏 memory 22 → 12；代价是同一批 ~29 条问题用例从 memory 守卫挪到 timeout 守卫，
timeout 7 → 17，无新增坏测试）。见 `docs/REFACTOR-HEAP-GC.md` §5 R3 / 阶段 E。
`built-ins/Array` +3、`language/statements/class` +2、`built-ins/Object` +1。

后续步骤见 `docs/REFACTOR-HEAP-GC.md`（阶段 B 铺路 / 阶段 C 收集器）。

### P2 — 收集器（M4）

1. `sweep` + free list + 阈值触发。
2. **GC stress 模式**（每次分配都收集 + 释放时填 `0xDD`）跑 feature 全套，
   把所有 rooting bug 炸出来。
3. **弱引用 → `WeakMap`/`WeakSet` 从"条目永不回收"改成真回收** —— **已完成**（2026-10-12，
   ephemeron marking）：`Trace for WeakMapObject`/`WeakSetObject` 改为只 trace 自身属性与
   原型（条目不再保活键），收集器在 mark 之后、sweep 之前补一遍 `Heap::mark_ephemerons`：
   键已标记 ⇒ 标记值（迭代到不动点，因为标记一个值可能让*另一个* WeakMap 的键变活）；
   键未标记 ⇒ 删掉条目。单测覆盖了"键死 ⇒ 键值皆回收、条目清空"与"键活 ⇒ 值被保住"两个方向。
4. ~~护栏的堆预算改读 `Heap::bytes_allocated`~~ —— **已撤销**：该口径只算
   `size_of::<Object>()`，**不含**对象内 `Vec` / `HashMap` 持有的堆外内存
   （数组元素、属性表才是大头），比现有的全局分配器代理更差。见 `GC-DESIGN.md` §11。

**验收**：`BIUJS_GC_STRESS=1` 下 feature 全绿（这是真正的验收）；
循环引用（`obj.self = obj`、长原型链、闭包环）在显式收集后被回收。
⇒ 到此 `Builtins::teardown` 的断环清单可以整条删掉。

### P3 — 语义线（并行，随时可插）

按"解锁多少用例"排序：

| 任务 | 现状 | 备注 |
|---|---|---|
| `DataView` 收尾 | 186 / 101 / 274 = **40.43%**，未入册 | 剩：不带 `new` 调用要 `TypeError`；`length`/`name`/`Symbol.toStringTag` 描述符；约 79 条靠 `$262.detach`（harness 未定义）、30 条要可增长 ArrayBuffer。跨过 50% 需再 +44 条 |
| `%TypedArray%.prototype` 方法族 | 几乎全缺（`map/filter/reduce/every/indexOf/join/sort/copyWithin`） | 这是 `built-ins/TypedArray` 874 条失败的主因 |
| 闭包共享可变单元 | **语义 bug**：`counter()` 返回 `0,0,0` | 见 §4 |

### P4 — 对象形状（M6）与调用分派

- 属性存储从 `HashMap<String, Value>` 改成 shape + 内联槽（**第一次给出性能数字**）。
- 内建分派从"按名字字符串 `match`"改成 entry point（消掉 arity 表 /
  `__proto__xxx` 前缀约定 / 旁路名单）。

## 3. 拆分 `src/vm/mod.rs`（P0-4）

**实测**（2026-10-11，拆分前）：11269 行，**229 个函数**。最大的几个：

| 行数 | 函数 | 说明 |
|---|---|---|
| **1740** | `run_instruction` | **指令分派 —— 一个函数顶全库 4.3%，是真正的抓手** |
| 436 | `call_native_by_name` | 内建函数按名字分派 |
| 376 | `try_array_callback_method` | `Array.prototype.{map,filter,…}` 的快速路径 |
| 343 | `proxy_static_trap` | Proxy 静态陷阱 |
| 224 | `get_global` / 210 `generator_abrupt` / 194 `set_like_method` | |
| 192 | `step` | 单步循环外壳（本身不大） |

**所以拆分的重点不是"按模块切文件"，而是先把 `run_instruction` 这一个 1740 行的
巨型 `match` 拆开。** 建议顺序（先拆函数，再挪文件）：

### 3.1 第一步：拆 `run_instruction`（1740 行）

按 `Kind` / 语义分组，把每个分支体抽成 `fn run_<group>(&mut self, inst: &Instr, …)`
放在同一文件里，让 `run_instruction` 只剩一个薄分派表。候选分组：

`Call` / `Jump` / `Return` / `Throw` / `Suspend` / `Bookkeeping` / 属性访问 /
算术 / 比较 / 对象字面量与数组 / 迭代器与 `for-of` / 生成器与 async。

**纪律**：一次只抽一组，抽完立刻跑全量；**纯移动不改逻辑**。

### 3.2 第二步：抽大函数

`call_native_by_name`（436）、`try_array_callback_method`（376）、
`proxy_static_trap`（343）各自成文件/模块。

> **已完成**（2026-10-11）：`proxy_static_trap` 等 9 个 trap 钩子与 `Proxy` 对象
> 整体搬到 `src/vm/proxy.rs`；按名字字符串的内建分派（arity 表 / 前缀约定 /
> 旁路名单）搬到 `src/vm/builtins_dispatch.rs`。逻辑未改，全量零回退。
> 下一步 `vm/call.rs` 等更细的职责切分**暂未做**（见 §3.4）。

### 3.3 第三步：按职责切文件

| 建议模块 | 内容 |
|---|---|
| `vm/frame.rs` | `Frame` / `FrameBirth` / `open_frame` / `note_frame` / `unwind_frames_to` / `frame_base` |
| `vm/exec.rs` | `step` / `run_instruction` 与各 `run_<group>` |
| `vm/call.rs` | `enter_call` / `invoke_*` / `Control` / `CallArgs` / 构造器路径 |
| `vm/exception.rs` | `SehRecord` / `handle_throw` / `throw` 路径 |
| `vm/generator.rs` | `SavedExecutionState` / 挂起恢复 / `discard_generator_frame` |
| `vm/state.rs` | `State` 与 `data_stack` / 寄存器 / `push` / `pop` / `ensure` |
| `vm/builtins_dispatch.rs` | `call_native_by_name` / `call_prototype_method` 与相关旁路名单 |

先在**同一个 crate 内**分 `mod` 文件，不要动 `pub` 边界。
**每移动一个模块跑一次全量确认零回退。**

### 3.4 拆分现状与收尾

**已完成**（2026-10-11，纯移动、逻辑未改、全量零回退）：

1. `run_instruction` 的 1740 行巨型 match 按 `Kind` / 语义切成 **17 个 `run_<group>`**
   （实际分组：`suspend` / `call` / `jump` / `stack` / `load` / `arith` / `compare` /
   `type` / `iteration` / `call_spread` / `convert` / `array` / `object` / `exception` /
   `construct` / `function` / `halt`），`run_instruction` 只剩一张"指令 → 组"的分派表。
2. Proxy（`proxy_static_trap` 等）与内建分派搬出 → `proxy.rs` / `builtins_dispatch.rs`。
3. 整个执行循环（`step` + `run_instruction` + 17 个组）搬出 → `exec.rs`（3404 行）。

**结果**：`src/vm/mod.rs` 由 **11269 行降到 6288 行**（−44%）。

**收尾可选**（不再阻塞 P0-4 验收）：§3.3 表里更细的 `frame.rs` / `call.rs` /
`exception.rs` / `generator.rs` / `state.rs` 切分 —— 它们只是把剩余的方法按职责归并，
纯移动、每步跑全量即可。鉴于 mod.rs 已减半、且更细的切分边际收益递减，可留待顺手做。

> 验证提醒：搬文件会改变方法可见性（私有方法对父模块不可见），所以搬出去的方法要加
> `pub(crate)`、出现在它们签名里的枚举（`FrameMode` / `Control` / `CallArgs` /
> `CalleeKind`）也要提为 `pub(crate)`。bytecode.rs 的元测试直接读 VM 源码取证，范围
> 从单文件变成 `vm/dispatch_sources()` 列表，漏掉一个文件覆盖断言立刻报出来。

### P3.6 — Array 读族改走 `[[Get]]`（2026-10-17 已完成，M7-P1 第一批）

`join` / `slice` / `includes` / `at` / `concat` / `reverse` 原来在 `ArrayObject`
的密集存储上工作，三件事同时错：访问器元素读成空位（`Object.defineProperty(a,0,{get})`
之后 `a.join()` 得到 `",2"`）、类数组接收者走不通、原型链上的 `length` 访问器看不到。

交付（`vm/builtins_dispatch.rs::try_array_read_method`）：
1. 元素清单统一走 `array_like_entries`（`[[Get]]` + `HasProperty`），与
   `map`/`filter`/`indexOf` 同一条路；
2. **稠密快速路径**（`ArrayObject::dense_elements`）：无洞、无访问器元素时直接
   用密集槽位，否则 20k 元素循环实测慢 2×；
3. 接收者允许清单（Array / Ordinary / Arguments / Function / Proxy），把
   `String.prototype.{slice,includes,at,concat}`、TypedArray、`ArrayBuffer.prototype.slice`
   的同名方法让出去（这三处各自都是一次真正的回退，已写进注释）；
4. `CreateDataPropertyOrThrow`：species 目标拒绝写入时抛 TypeError；
5. `ToIntegerOrInfinity` 走 `to_number_throwing`（`[1,2].at(Symbol())` 是 TypeError）；
6. `[...a]` / `for-of` 的数组快路径同样改为 `[[Get]]`（此前读密集存储，
   访问器不执行）。

验收：`built-ins/Array` 2155 → **2186**（`concat` 32→16、`includes` 21→13、
`slice` 16→12、`join` 10→8），`String` +1、`TypedArray` +3、`for-of` +1；
全量 17195（+36），无逐套件回退；微基准（20k 元素 × 200 轮）1560ms → 1333ms。

### P3.5 — 带标签模板（2026-10-17 已完成）

`` tag`a${x}b` `` 此前**整段缺失**：`lower_expression` 没有
`TaggedTemplateExpression` 分支，于是掉进兜底 arm 编译成 `null`（release 下连日志
都没有）。`String.raw` 作为内建存在，但 `` String.raw`a\nb` `` 返回 `null`。

交付：
1. `lower_tagged_template`：按规范顺序求值 tag 引用 → 模板对象 → 替换表达式 →
   调用；成员 tag 保留接收者（`obj.m\`\`` 的 `this` 是 `obj`）；
2. **站点缓存**（`GetTemplateObject`）：新指令 `MakeTemplate { dst, site }` +
   `Module::templates` 静态表 + VM 上按 `site` 的缓存；同一站点跨求值复用同一个
   **冻结**对象（元素不可写/不可配置、`raw` 三个属性都不可），缓存已入 GC 根集；
3. 顺带修掉一个更深的语义缺口：`ToString`/`ToNumber` 指令把 `ToPrimitive` 的
   abrupt completion **吞掉**了（回退成 "[object Object]" / `NaN`）——
   现在用户 `toString`/`valueOf` 抛出会传播（ES 12.2.8.5 step 4）。

入册：`language/expressions/tagged-template`（18 通过 / 8 跳过 / 1 失败 —— 失败是
`noStrict` 用例，严格模式固有的偏差）与 `language/expressions/template-literal`
（54 / 3 / 0）。验收：三级 233 / 581 / 7 全绿；全量 17159（+82），无逐套件回退，
另外 7 个套件因 ToPrimitive 修复各 +1（`unary-plus`、四种 `++`/`--`、`new.target`、
`String` +4）。

## 4. 两条待定的语义 bug（属于"修 bug"，不是重构）

1. ✅ **闭包共享可变单元**（2026-10-16 已修）：被嵌套函数捕获的绑定现在住在
   共享单元（`EnvCell`）里 —— 序言 `MakeCell`，读写走 `LoadEnv`/`StoreEnv`，
   闭包捕获的是单元而不是创建时的快照。连带的三处语义一并落定：
   ① 块级绑定（`for (let …)` 头）**保留**按值捕获，那是"每次迭代一个值"；
   ② 被捕获的 `let` 的死区是**动态**的（单元有"未初始化"态，声明写入走
   `InitLexical`）；③ 参数默认值里的闭包看不到函数体的 `var`
   （ES 9.2.10 step 27，参数作用域与 var 环境分开）。
   验收：`tests/features/closures.rs`（18 条）+ 全量 +144、零逐套件回退。
2. **`handle_throw` 在 catch 路径多弹一次 `seh_stack`**（历史 known bug #4），
   由帧上的 try 栈按构造消除 —— 验收标准是"去掉那条 `#[ignore]` 后变绿"。

## 5. 明确不做 / 后置

- **NaN-boxing（8 B 值表示）**：判定为**可选**。它是纯性能项，不是正确性所需
  （"寄存器是帧内定长窗口"与 `Value` 是 8 B 还是 24 B 无关），且依赖"先有堆与
  句柄"。如果哪天要做，顺序是 **P1 之后、P4（对象形状）之前** —— 否则对象形状
  的字段类型要改两遍。
- **并行跑测试**：实测过，`TEST262_JOBS=4`（4 核）比串行**更慢**（512s vs 452s），
  且护栏分类会漂（timeout 7→11、memory 22→18）。原因：分片按"套件位置"切，而
  套件大小极不均衡；且内存护栏是**进程级全局分配器**，多线程会共用同一个预算。
  想真加速要先按**用例数**切匀。`TEST262_JOBS` 已留作开关，默认 1。

## 6. 每一步的验证协议

```
cargo test --release --lib            # 单元
cargo test --release --test features  # feature 断言
cargo test --release --test guards    # 护栏
./scripts/phase2-status.sh            # 全量 + 逐套件对比基线
```

**必须同时看**：头条数字（passed/skipped/failed）、护栏三计数、墙钟。
只看头条会漏掉"通过数不变但护栏漂了"这类回归（历史上真实发生过）。
