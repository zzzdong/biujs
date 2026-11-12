# biujs 全量审查与重构计划（2026-10-24）

> 审查对象：工作区当前状态（`develop` @ `8865938` + 18 个未提交文件，+690/−187）。
> 方法：全量 test262、未入册套件定向实测、源码逐层审查、与 ChakraCore 对照。
> 结论分级：**S0** = 可证的正确性/内存安全问题；**S1** = 结构性错配，会持续产生 bug/成本；
> **S2** = 工程与可观测性问题。
>
> **进展（2026-10-24/25）**：0.2 `invoke_boundaries` 作用域化 + 2 条回归测试、
> 0.3 `pending_captured` 入根 + 单测、0.4 `rbp <= rsp` debug 断言、
> 0.1 护栏计数写入基线、0.5 常量/立即数的运行时检查、
> 0.6 字节码 dumper + 16 组 golden（`tests/golden/bytecode/`）并删除
> `Module::debug_instructions` 的 IR 克隆、0.7 scope 表收口（`noStrict`/`raw` 跳过、
> 删掉过宽的源码子串门控、所有用例统一前置 `"use strict"`）、
> 0.8 strict 运行时收口（`with` 编译期拒绝、隐式全局赋 `ReferenceError`、
> 顶层 `this = globalThis`、全局对象接 `Object.prototype`）、
> 3A 删 `BIUJS_REG_VARS` 与变量寄存器分配（`regalloc.rs` 1063 行 → `frame_slots.rs`
> 87 行）已落地。
> 默认内存预算调回 384MB；0.8 之后全量为
> **17,271 / 20,504（84.23%）**，相对 0.7 基线 +65 条，无逐套件回退。

---

## 0. 结论摘要

1. **当前通过率 84.23%**（17,271 / 20,504，跳过 8,577，失败 3,233；0.8 之后
   2026-10-25 基线）。语言层 `language/*` 与内建层 `built-ins/*` 的差距仍是主要矛盾
   —— **差距在内建，不在语法**。
2. 384MB 下与 2026-10-18 基线（17,363 / 3,314）逐套件一致。此前 256MB 预算下少 5 条，
   是 `built-ins/Array` 的**内存护栏误伤**：`TEST262_MEMORY_MB=0` 时 Array 回到 2311
   通过。**工作区没有语义回退**；2026-10-24 已把默认预算调回 384MB。
3. **未入册的四个 ES6 套件实测只有 45.31%**（735 / 1,622）：
   Date 45.5%、ArrayBuffer 23.5%、DataView 44.1%、TypedArrayConstructors 58.4%。
   全部入册后总盘约为 **18,093 / 22,299 = 81.14%**，还剩 **4,206 条失败**。
   这还没有算 modules 与 RegExp —— 它们目前被整体排除在分母外。
4. **目标口径（已部分收口）**：`noStrict` / `raw` 用例现在跳过（strict-only 目标），
   `Date` / `RegExp` / `label:` / `print(` / `$ERROR` / `=>` 等过宽的**源码子串门控**已删除
   （Object +166、String +49、Array +28）。仍未收口的是：
   `class-fields-public`（2,054 条）、`object-rest`、`resizable-arraybuffer` 等后 ES6 特性
   **没有**被 gate，直接进入分母；`built-ins/Date` 等 ES6 套件仍未入册。
   **83.92% 仍是“当前支持面”的通过率，不是 ES6 一致率。**
5. **strict 运行时语义（0.8 已修）**：此前 `"use strict"` 只有 oxc 的早期错误
   （重复参数、`with`、八进制、`eval`/`arguments` 赋值），而
   `"use strict"; x = 1` 仍会创建全局、sloppy 的 `with ({x:2}) { … }` 能编译且行为错误、
   脚本顶层 `this` 是 `undefined`、`globalThis` 的原型是 `null`。
   现在：`with` 一律编译期拒绝、严格模式下写不可解析的名字抛 `ReferenceError`、
   顶层 `this === globalThis`、全局对象接上 `Object.prototype`。
   **仍未建模**：逐函数级严格性（`function f() { "use strict"; }`），
   以及"块级 `let` 的绑定应住在声明式记录里"——它现在还是全局属性（Phase 1B 的事）。
6. **架构判断**：VM/GC/字节码的方向是对的（真 SSA + 块参数、EH 侧表、精确根、
   `Gc` 瘦句柄），但有三处结构性错配：
   - ~~**无 JIT 却保留完整 IR/SSA/regalloc**~~（约 10.5k 行，**已削掉 ~1k 行**：
     `BIUJS_REG_VARS` 与整套寄存器分配于 2026-10-25 删除，`regalloc.rs` 收缩为
     `frame_slots.rs` 的槽位分配）。剩余问题不变：`codegen`/`ssa`/`cfg`
     几乎没有测试，加一条 IR 指令仍要改 4–5 处 —— 这是“复杂度没有消费者”的主体。
   - **对象模型缺 atom/shape**：每次属性访问都 `Rc::new(String)`（`property.rs:11`），
     每次 get 克隆 56B 的 `PropertyDescriptor`；数组元素散在 `elements`/`properties`/
     `holes` 三处；20 个 `Object` 变体各抄一份 header。
   - **内建分派按名字字符串 + 前缀 + arity 表 + 旁路名单**：新增一个原型方法要改 5–9 处；
     `vm` 与 `builtins` 互相依赖（190 / 159 处），无法独立测试或替换。
   另外还有一组 **S0**：`invoke_boundaries` 提前返回泄漏、`pending_captured` 未入根、
   `rsp` 三写者、GC 悬垂句柄（已加 `Free` 态检查；句柄代次要等 `Value` 缩下来）。

**建议顺序**：先修 S0 + 测量口径（Phase 0），再做“对象/环境/分派”重构（Phase 1），
然后“帧/GC/字节码”（Phase 2），最后“编译器简化 + 语义补齐”（Phase 3/4）。
不要先做 NaN-boxing、并行 test262、可移动/分代 GC。

---

## 1. 测量基线与可信度

### 1.1 三级验证（2026-10-24 实测）

| 项 | 结果 |
|---|---|
| `cargo test --release --lib` | 242 passed |
| `cargo test --release --test features` | 629 passed |
| `cargo test --release --test guards` | 7 passed |
| 构建警告 | 77（`cargo build --release`） |
| `cargo clippy --release --lib` | **442 warnings**（主要是 `collapsible_if` 88、冗余字段名 79、`Value` 上多余的 `clone` 61、冗余闭包 36、未用 import 30+） |
| 源码规模 | 45,322 行：compiler 10,969 / vm 21,170 / builtins 9,885（删寄存器分配后 -691） |
| 全量 test262（384MB，scope 收口 + strict 收口后） | **20,504 执行 = 17,271 通过 + 3,233 失败；8,577 跳过；84.23%** |
| 护栏 | timeout 0 / step-limit 0 / memory 14 |
| 全量墙钟 | 约 7–9 分钟（4 片串行；不含首次全量构建） |

复算命令：

```sh
./scripts/phase2-status.sh
awk -F'\t' '!/^#/{p+=$2;s+=$3;f+=$4} END{printf "passed %d skipped %d failed %d exec %d rate %.2f%%\n",p,s,f,p+f,100*p/(p+f)}' docs/phase2-status.tsv
```

### 1.2 基线漂移与“假回退”

- `docs/phase2-status.tsv`（2026-10-18）曾是唯一自洽快照：17,363 / 3,314 / 8,404。
- 256MB 下实测 17,358 / 3,319，唯一逐套件差异是 `built-ins/Array` 2311 → 2306；
  关掉内存护栏后 Array 回到 **2311** 通过，5 条失败均为 `memory budget exceeded`。
- 也就是说：**5 条不是语义回退，是 256MB 预算的边界用例**。2026-10-24 默认预算已
  调回 **384MB**：全量回到 **17,363 / 3,314 / 8,404**，无逐套件回退。
- 2026-10-25 做完 0.7 scope 收口后刷新基线：**17,206 / 3,298 / 8,577（83.92%）**。
  逐套件“回退”全部来自 `noStrict` 不再计入分母（失败数同时下降），净收益是
  Object +166、String +49、Array +28；唯一失败数上升的 `expressions/addition`
  （4 → 5）来自此前被 `"Date"` / `print(` 门控挡住的用例（Date 加法、`f + 1`、
  Symbol → Number 抛错、求值顺序），是**暴露出来的真缺口**。
- 基线快照必须把 `TEST262_TIMEOUT_MS / TEST262_STEP_LIMIT / TEST262_MEMORY_MB`
  一起记录，否则每次比较都会把护栏漂移当成代码回退。脚本已支持写入与比较，
  新快照头部形如
  `# guards: timeout 0 step-limit 0 memory 14 (TEST262_TIMEOUT_MS=15000 …)`。
- 文档里的数字已经互相矛盾：README 83.42%（10-17）、PROJECT 标 10-11 实为 10-17、
  PLAN 84.06%（更早）。本次审查以 tsv + 实测为准；0.10 会把这些数字改成脚本生成。

### 1.3 未入册套件实测（临时 runner，已删除）

| 套件 | 通过 / 执行 | 通过率 | 失败 | 跳过 |
|---|---|---|---|---|
| `built-ins/Date` | 265 / 583 | 45.5% | 318 | 11 |
| `built-ins/ArrayBuffer` | 48 / 204 | 23.5% | 156 | 17 |
| `built-ins/DataView` | 203 / 460 | 44.1% | 257 | 101 |
| `built-ins/TypedArrayConstructors` | 219 / 375 | 58.4% | 156 | 363 |
| **合计** | **735 / 1,622** | **45.31%** | **887** | **492** |

> 测量时去掉了 runner 里过宽的源码子串门控 `"Date"` / `"RegExp"`，否则 Date 整族会被
> 跳过。临时 runner 已删除，工作区未留下文件。

**入册影响**：executed 20,677 → 22,299，passed 17,358 → 18,093，总通过率
**83.95% → 81.14%**。入册本身不提高指标，它只是把“看不见的债”变成“看得见的债”。

---

## 2. 差距清单

### 2.1 失败集中在哪

按大类：

| 大类 | 套件数 | 通过 / 执行 | 失败 | 通过率 |
|---|---|---|---|---|
| `language/*` | 86 | 8,756 / 9,601 | 845 | 91.20% |
| `built-ins/*` | 21 | 8,607 / 11,076 | 2,469 | 77.71% |

失败绝对值 Top 15（tsv 基线；当前 Array 因护栏 +5）：

| # | 套件 | 失败 | 占 3,319 |
|---|---|---|---|
| 1 | `built-ins/TypedArray` | 634 | 19.1% |
| 2 | `built-ins/Array` | 574 | 17.3% |
| 3 | `built-ins/Object` | 340 | 10.3% |
| 4 | `built-ins/String` | 303 | 9.1% |
| 5 | `language/statements/class` | 144 | 4.3% |
| 6 | `built-ins/Promise` | 141 | 4.3% |
| 7 | `built-ins/Function` | 137 | 4.1% |
| 8 | `built-ins/Number` | 104 | 3.1% |
| 9 | `language/statements/for-of` | 101 | 3.0% |
| 10 | `language/expressions/class` | 94 | 2.8% |
| 11 | `language/expressions/object` | 61 | 1.8% |
| 12 | `language/expressions/assignment` | 60 | 1.8% |
| 13 | `built-ins/Error` | 45 | 1.4% |
| 14 | `built-ins/Proxy` | 45 | 1.4% |
| 15 | `built-ins/Set` | 32 | 1.0% |

前 5 个套件占 60.2%，前 10 个占 77.6%。**先把 TypedArray / Array / Object / String /
class / Promise / Function / Number 这 8 个桶打掉，就能覆盖约 80% 的失败。**

### 2.2 按根因归类（实测与估算分开）

| 根因 | 主要套件 | 规模 | 依据 |
|---|---|---|---|
| TypedArray 长尾：detached/resizable buffer、索引 exotic、descriptor 形状、species、from/of、回调族 | TypedArray | 634 | tsv；`src/builtins/typedarray.rs`；runner 无 `$262` |
| Array 长尾：holes、species、`Array.from` 通用迭代器、`isArray`/Proxy、巨大 index | Array | 574 | tsv + 失败明细 |
| Object：全局对象缺失、`defineProperty` 描述符、Annex B、`ToPropertyKey` | Object | 340 | tsv；`globalThis` 实测 |
| String：RegExp 表达力、`Symbol.match/replace/search/split`、UTF-16 存储 | String | 303 | tsv；`PLAN.md` P3.7 残留 |
| class：求值顺序、own property、`super` | class + expressions/class | 238 | tsv；B8 未完成 |
| Promise：`await` 就地排空、与已排队 job 不交错 | Promise | 141 | tsv；B35 残留 |
| Function：`toString` 源码形状、`length`/`name`、bind/call/apply | Function | 137 | tsv；`function.rs` 占位 |
| Number：`toFixed`/`toExponential`/`toPrecision`、radix | Number | 104 | tsv；无文档记录，需逐条归类 |
| for-of / generators / yield：`IteratorClose`、next-result 类型、finally return | 三个套件 | 101 + 44 + 20 | tsv；P6/P3 |
| Error：缺 `ErrorObject`/`[[ErrorData]]`/`stack` 访问器 | Error + NativeErrors | 45 + 14 | B41 撤回 |
| Proxy/Reflect：原型链陷阱、描述符不变量、receiver | Proxy/Reflect | 45 + 1 | B45 残留 |
| 小套件系统性 bug：`delete`、`instanceof`、`unsigned-right-shift`、`return`、`this` | 多个 | ≈100 | tsv，文档无批次记录 |
| **未入册四套件** | Date/AB/DV/TAConstructors | **887** | §1.3 实测 |

> 说明：除“未入册四套件”和少量引用文档的条目外，细项多为估算；tsv 只有计数，没有
> 逐条原因。要把估算变成实测，跑一次
> `TEST262_FAILURES=99999` 并用 `scripts/kpi-noise.py` 归类。

### 2.3 目标口径的问题（0.7 已处理 c 与 d，a/b 仍开放）

**(a) 跳过的 8,577 条不是同一类东西。** 实测跳过原因 Top（0.7 之前的口径）：

| 原因 | 条数 | 性质 |
|---|---|---|
| `class-methods-private` / `class-static-methods-private` / `class-fields-private` / `class-static-fields-private` | 3,732 | ES2022，范围外 |
| `async-iteration` | 1,820 | ES2018，范围外 |
| `BigInt` | 658 | ES2020，范围外 |
| `eval(` | 347 | 明确不做 |
| `RegExp` 源码模式 | 223 | 过宽（见下） |
| `Date` 源码模式 | 160 | **过宽：Date 已有实现（B31）** |
| `cross-realm` | 113 | 测试设施缺失 |
| Promise ES2018+ | 220 | 已实现但登记为范围外 |
| `numeric-separator` | 97 | ES2021 |
| `with (` | 96 | 明确不做 |
| `Array.fromAsync` | 95 | ES2024 |
| `new Function(` / `Function("` | 146 | 明确不做 |
| `WeakMap` / `Map` | 116 | 已入册，门控陈旧 |
| `class-static-block` | 60 | ES2022 |
| `String.prototype.replaceAll` | 40 | ES2021 |
| `await-dictionary` / 尾调用 | 70 | 范围外 |

**(b) 有些后 ES6 特性没有 gate，直接进分母。** 已入册套件里
`class-fields-public` 有 2,054 条、`class-static-fields-public` 213 条、
`object-rest` 278 条、`resizable-arraybuffer` 303 条、`Symbol.asyncIterator` 398 条。
其中 class fields / object rest 实际已实现，跑它们不是错；但它们不属于 ES6，
**混进同一个通过率就无法回答“ES6 还差多少”**。

**(c) 源码子串门控在隐藏已实现的功能 —— 已修。**
`UNSUPPORTED_PATTERNS` 里原有 `"Date"`、`"RegExp"`、`"label:"`、`"print("`、`"$ERROR"`、
`"=>` 等。**Date 已有实现**（B31，实测 45.5%），但任何源码含 `"Date"` 的用例都被跳过；
`label:` 跳过所有带标签语句的用例，而标签是 ES3 语法且引擎已支持（实测 `outer: for`
正常）。0.7 把它们全部删掉，只保留"语法上明确不做"的四类（`eval` / `Function` 构造器 /
`with` / modules）。收益：Object +166、String +49、Array +28。

**(d) strict-only 目标与 `noStrict` 混跑 —— 已修（测量侧）。** 0.7 起
`noStrict` / `raw` 用例跳过，其余所有用例统一前置 `"use strict"`（此前只有
`onlyStrict` 会前置，普通用例按 sloppy 跑）。跳过 +173、执行 20,677 → 20,504。
注意这只解决了**测量**：引擎的 strictness 仍然是“指令驱动”（见 §2.4）——
运行时严格语义与全局对象仍是缺口，那是 0.8 的事。

### 2.4 语言层的实际缺口（不只是内建）

以下均为实测：

```js
"use strict"; x = 1;          // 没报 ReferenceError，创建了全局
with ({x: 2}) { x }           // sloppy 下能编译；读到 undefined（应为 2）
this                          // 顶层 this = undefined（应为 globalThis）
Object.getPrototypeOf(globalThis) // null（应为 Object.prototype）
typeof globalThis.hasOwnProperty   // "undefined"（应继承自 Object.prototype）
```

**0.8 之后的状态**：

```js
with ({x: 2}) { x }                  // 编译期拒绝（之前能编译，读到 undefined）
"use strict"; x = 1                  // ReferenceError（之前创建全局）
this === globalThis                  // true（之前 undefined）
Object.getPrototypeOf(globalThis) === Object.prototype   // true（之前 null）
typeof globalThis.hasOwnProperty     // "function"（之前 "undefined"）
({}) instanceof globalThis           // TypeError（顺带修掉一个真 bug，见下）
```

`this` 从 `undefined` 变成全局对象，立刻暴露了 `instanceof` 的一个存量 bug：
ES 12.10.4 第 4 步规定 RHS 是**不可调用对象时抛 TypeError**，而实现直接走了原型链
遍历（`src/vm/exec.rs` 的 `Instr::InstanceOf`）。之前 `this` 是 `undefined`，
"不是对象"这一条先抛了 TypeError，把 bug 遮住了。已修，`instanceof` 套件回到基线。

仍存在的缺口：
- **逐函数级严格性**没有建模 —— 现在只有脚本首部的指令会被识别，
  `Module::strict` 是全模块一个布尔；
- **块级 `let`/`const` 的绑定**仍然落在全局属性上（`store_env` → globals），
  而不是声明式记录；Phase 1B 的 `EnvRecord` 会一次性解决；
- 全局对象仍是 `State::globals: HashMap` 的视图（`src/vm/object.rs` 的
  `GlobalObject`），只是现在有了 `[[Prototype]]`；真 `GlobalObject`（属性描述符、
  `var` 的非可配置语义）也属于 1B。

### 2.5 “离目标还有多远”的三种口径

| 口径 | 当前 | 说明 |
|---|---|---|
| **A. 当前支持面**（已入册 107 套件，scope 收口后） | **84.23%**（17,271 / 20,504） | 现有数字；混入后 ES6 特性 |
| **B. ES6 核心 + 已实现扩展**（再加入 Date/AB/DV/TAConstructors） | ≈ **81.4%**（18,006 / 22,126） | 建议采用；明确列出排除项 |
| **C. ES6 全家桶**（再加 modules / RegExp） | 未知，预计 < 70% | modules 完全没做；RegExp 无完整引擎 |

建议采用 **B**，并把它写成机器可读的 scope 表：
- 在范围：language（strict）、Array、Object、Function、String、Number、Boolean、Symbol、
  Error/NativeErrors、JSON、Math、Date、RegExp（若决定做）、Promise、Map/Set/Weak*、
  Proxy/Reflect、ArrayBuffer/DataView/TypedArray*、generators、iterators。
- 明确排除：`eval` / `with` / `Function` 构造器、modules（或单独立项）、BigInt、
  async-iteration、Temporal、Intl、SharedArrayBuffer/Atomics、WeakRef、
  尾调用优化、class 私有字段/static block、ES2021+ 的 optional chaining /
  logical assignment / numeric separator / replaceAll 等。
- 指标：**双轨** —— `es6-core` 与 `extended`；两者都报 passed/executed/failed/skipped/
  guards/wall-clock。低于 50% 的套件不入册（现有纪律），但一旦入册就必须在
  `IN_SCOPE_PENDING` / `OUT_OF_SCOPE_FEATURES` 里二选一。

---

## 3. 架构审查

### 3.1 做得好的地方（不要在重构里丢掉）

- **字节码是运行期契约**：`Module` + `CodeBlock` + `Instr`，VM 只认字节码；
- **真 SSA + 块参数**：phi 用 `Jump{dst,args}` / `BrIf{true_args,false_args}` 表达，
  异常边显式进 CFG（`ssabuilder.rs`）；
- **EH 侧表**：`EhRegion { start, exits, catch, finally }` 不靠 pc 区间猜嵌套；
- **GC 是精确根**：`Trace` 协议 + `Heap`/`Gc<T>` + ephemeron（`mark_ephemerons`），
  比 ChakraCore 的 conservative stack scan 更适合 Rust；
- **护栏**：timeout / step / memory 三档，且把 guard 失败变成普通失败，避免整片丢结果；
- **测试纪律**：逐套件回退检测、快照、入册三步同批。

### 3.2 S1：编译器——没有消费者的 IR/SSA/regalloc

| 证据 | 说明 |
|---|---|
| `src/compiler/codegen.rs:67` | ~~`memory_resident_vars = env::var("BIUJS_REG_VARS").is_err()`~~ **已删除（2026-10-25）**：固定内存驻留，`BIUJS_REG_VARS` 与整套寄存器分配（约 900 行）一并移除 |
| `codegen.rs:1044-1061` | ~~`gen_operand` 对 `Value::Variable` 直接 `ensure_stack_slot`，只有开了 `BIUJS_REG_VARS` 才 `alloc()`~~ **已删除**：`Value::Variable` 一律走 `FrameSlots::ensure_slot` |
| `codegen.rs:96-98` | 任何含 `try` 的函数强制内存驻留 |
| `regalloc.rs`（1,060 行） | 区间分组着色；但默认路径只用来给临时值分配寄存器 |
| `compiler/mod.rs:35-204` | 一个函数干完 parse/semantic/lower/SSA/codegen/link；`DelayedJump`/`ResumeExc` 手工加模块偏移 |
| `ir/instruction.rs` | 加一条指令要同步 `defined_and_used`、`Display`、codegen match、`bytecode.rs` 宏 —— 4–5 处 |
| `semantic.rs:92-95` | 行号恒为 1，column 是字节偏移；IR 指令无 span |
| `lowering` | `CompileError` 出现 **0 次**；不支持语法走 `_ =>` 静默降级；`LowerError`/`NotImplemented` 是死变体 |
| `compiler/` 测试 | 33 个 `#[test]`，codegen/ssa/cfg/semantic/symbol = 0 |

结论：这套 IR/SSA 的**算法骨架没问题**，但它的复杂度没有换来东西：
没有 JIT，没有优化 pass，变量默认在内存，寄存器路径不可测。ChakraCore 的边界
（AST → bytecode，JIT 才用 IR）反证了这一点。

**建议（分两步，不要大爆炸）**：
1. **先删双路径**：删除 `BIUJS_REG_VARS` 与变量寄存器分配；保留一个简单的
   栈槽分配器。`regalloc` 只负责临时值的寄存器/栈选择，行为固定、可测。
2. **再评估直出**：目标形态是 `AST → ScopeTree/脱糖 → BytecodeEmitter → FunctionCode`，
   没有 IR/SSA/phi。先做一个 **2 周 spike**（只覆盖表达式 + 语句 + 函数调用），
   对比 LOC、编译时间、test262 通过数。若 spike 能在不改 VM 的前提下达到同等通过数，
   再分片迁移；否则保留 IR，只做 1。

**不要**在没有 spike 的情况下直接重写 5k 行 lowering —— 它承载了 class、解构、
生成器等大量语义，失败成本远高于收益。

### 3.3 S1：对象模型——缺 atom/shape，且存储分散

| 证据 | 说明 |
|---|---|
| `vm/property.rs:11-13` | `PropertyKey::from_str` = `Rc::new(s.to_string())`，**每次属性访问一次分配** |
| `vm/object.rs:155-159` | `PropertyTable` = `BTreeMap<PropertyKey, PropertyDescriptor>` + `creation_order: Vec`；O(log n) + 字符串比较 |
| `vm/object.rs:460/1896/3376` | 每次 get 克隆整个 `PropertyDescriptor`（约 56B，含 3 个 Value/访问器） |
| `vm/object.rs:576-592` | 数组索引值可能在 `elements`、`properties`、`holes` **三处**；一次读判 3 次 |
| `vm/object.rs:19` / `965-969` | `MAX_DENSE_ELEMENTS = 1<<20`；超过直接 `TypeError`，没有真稀疏模式 |
| `vm/gc.rs:703-724` | `Object` 是 20 变体具体枚举；`Map/Set/WeakMap` 各自重复 5 个 header 字段 |
| `vm/gc.rs:728+` | `forward!` 对 14 个方法 × 20 变体展开 match；加一个 exotic 对象要写全套 |
| `vm/value.rs:45-56` | `Value::Function(u32)` 是非 JS 可见的裸函数 id；`is_callable()` 不认它，代码里到处 `|| matches!(v, Function(_))` |
| `builtins/mod.rs:73-104` | 原型用 `thread_local!` 注册表；`object.rs:603` 的核心对象构造反向依赖它 |

**目标形态**：
- `Atom(u32)` 驻留表；`PropertyKey::{Index(u32), Atom(Atom), Symbol(u32)}`；
- `ObjectHeader { shape: ShapeId, slots: Box<[Value]>, proto, flags }`，`Shape` 用
  transition cache 共享；`Dictionary` 作为慢路径 fallback；
- 内部 get 返回 `&Value` / 轻量 `PropSlot`，`PropertyDescriptor` 只在 API 边界物化；
- 数组 `Vec<Element>`（`Hole | Value | Accessor`）+ 稀疏 map fallback；
- 删 `Value::Function(u32)`，所有函数都是 `Gc<Object>`；
- 5 个真正的 exotic（Array 索引、TypedArray 索引、String 包装索引、Proxy、Global）
  覆盖 header，其余走默认实现。

### 3.4 S1：内建分派——名字字符串驱动，改一处错两处

| 证据 | 说明 |
|---|---|
| `vm/builtins_dispatch.rs:28-505` | `call_native_by_name`：30+ 个 `if name == …` / `strip_prefix` |
| `builtins/mod.rs:1167/1225/1346` | 三张名字 match（构造器/静态/原型） |
| `builtins/mod.rs:1998-2098` | 100 行 arity 表，先 strip 5 个前缀 |
| `vm/object.rs:1655-1699` | `install_metadata` 反向解析 6 个前缀 + 4 组特例 |
| `vm/exec.rs:2839-2856` | `vm_handled_static` 旁路名单，与 dispatch 的 `if name` 重复 |
| `builtins/mod.rs:1925-1945` | `__proto_method__`、`__promise_finally__<id>_f` 等把状态编码进名字 |
| `builtins/array.rs:315-344` vs `vm/mod.rs:754-873` | 同一件“按索引读”有两套语义：一套绕过 accessor/原型链，一套走真 `[[Get]]` |
| `array.rs:285` / `builtins/mod.rs:105` | `ToObject` 两份；`LengthOfArrayLike` 三份；species 四处 |

证据：`findLast` 需要改 **9 处 / 4 个文件**；`toSorted` 需要 6 处。没有编译器护栏，
漏改 arity 只是静默 `length === 0`，漏改名单只是静默走错分支。

**目标形态**：
- `enum NativeId { ArrayPush, MapGet, TypedArrayMap, … }`；`NativeFunctionObject`
  存 `id`（不是 `name: String`）；`static BUILTIN_TABLE: &[BuiltinDef]` 带
  `{ name, atom, arity, flags, func }`；
- 有状态的 native 用 `enum NativePayload` 字段（PromiseFinally{id,tag}、Bound{…}、
  IteratorNext{kind}），不再编码进名字；
- 统一签名：`fn(&mut Vm, this: Value, args: &[Value]) -> Result<Value, Completion>`；
- `ToObject` / `ToPropertyKey` / `LengthOfArrayLike` / species / 迭代协议只保留一份，
  放在 `runtime/ops.rs`；
- `HostOps` 边界（或一个 `VmOps` 门面）切断 `vm ⇄ builtins` 的循环依赖。

### 3.5 S1：全局环境与 Realm

当前是四套东西拼起来：
- `State::globals: Rc<RefCell<HashMap<String, Value>>>`（全局变量）
- `State::script_env: HashMap<String, Option<Value>>`（脚本 let/const TDZ）
- `closure_var_stack: Vec<HashMap<String, EnvCell>>`（函数捕获）
- `Object::Global` 的 `store` 是 `globals` 的视图（`object.rs:2437`）

后果：
- 顶层 `this` = `undefined`（实测）；`globalThis` 原型 = `null`；
- 全局对象没有 `Object.prototype` 的方法，`getOwnPropertyDescriptor(globalThis,"undefined")`
  取不到；
- 环境查找是 O(帧深度 × HashMap) + 字符串；没有编译期 `(depth, index)` 布局；
- 原型表用 `thread_local!`，多 VM/Realm 隔离靠析构时清理，`builtins::wrapper_prototype`
  被核心对象构造反向调用。

**目标形态**：`Realm { global: Gc<Object>, intrinsics: Intrinsics, … }` 归 `VM` 所有；
`GlobalObject` 有真正的 `ObjectHeader`，`proto = Object.prototype`；`var`/函数声明
成为全局对象属性，`let/const/class` 进 script declarative record；环境是
`EnvRecord` 链（Declarative / Object / Function / Global），编译期给出槽位；
删除 `thread_local` 原型表。

### 3.6 S0：VM/帧/异常/GC 的正确性风险

| # | 问题 | 证据 | 后果 |
|---|---|---|---|
| 1 | `invoke_boundaries` 泄漏 | `exec.rs:887` push；`895/939` 的 `?` 提前返回；`965` 才 pop | 一次 RangeError 后本 run **不再回收**；`handle_throw` 的边界判定也会用过期值 |
| 2 | `pending_captured` 未入根 | `mod.rs:5578` 字段；`trace.rs:77-98` 未 trace | 指令边界就是安全点；任何拉长 `ClosureVar`→`Make*FuncObj` 窗口的改动都会 UAF |
| 3 | `rsp` 三写者 | `unwind_frames_to`（`mod.rs:3069` 派生）、`handle_throw`（`3108` 快照）、`restore_execution_state`（`4626` 快照）、`drive_bytecode_frame`（`exec.rs:983` 再压） | 跨挂起边界的几何没有单一所有者；生成器路径已有 8/5 处特例 |
| 4 | ~~GC 槽无 generation~~ **已部分修** | `gc.rs`：`Gc` 只有 `NonNull<Slot>`；`sweep` 放回 free list | 漏根后槽被复用 ⇒ 读到另一个类型合法的对象，静默类型混淆。**已加** `borrow` 族的 `Free` 态检查（对已释放、未复用槽的访问当场 abort）；**未加**句柄代次 —— 那会让 `Gc` 8B→12–16B、`Value` 16B→24B（+50% 且可能碰翻 5 条 Array 内存护栏用例）。前置条件：1A 把 `Value::String` 换成 `Atom(u32)` 后 `Value` 有空间，届时补上 |
| 5 | 热路径 panic | `bytecode.rs:1276` `as_immd`、`exec.rs` 的 `constants[immd as usize]`、`-1` 哨兵 | release `panic=abort`，一条坏用例带走整片 |
| 6 | 帧不变量无运行时守卫 | `mod.rs:5841-5849` 的互校只剩注释；`step` 不再调用 | `rbp == frame.base` 现在只靠约定 |
| 7 | 宿主侧 rooting 与闸门 | `Scope/Root/Roots` 已接约 20 处，但 `invoke_boundaries` 非空时整段跳过收集 | 正确但脆弱；应改为 `RootScope` RAII + 完整入根后再撤闸门 |

### 3.7 S2：字节码/指令集与工程

- 92 条指令、3 层 match（`step` → 17 组表 → 组内）；`run_instruction` 未命中时
  静默 `jump_offset(1)`（`exec.rs:1599-1602`），语义错误会被“空操作”吞掉。
- `IndexGet|PropGet` 同一个 arm、`IndexSet|PropSet`、`IndexDelete|PropDelete`：
  6 个 opcode → 3 种行为；调用族 8 个；环境族 7 个。
- `Try` 的操作数在 release 下不参与控制流（EH 表权威），只是死编码；`Await` 不挂起，
  却被归到 `Suspend`。
- 全局扁平 `Module.instructions` + 绝对 pc，导致函数不能独立编译/缓存/重定位
  （`compiler/mod.rs:175-192` 手工平移）。
- `host/mod.rs` 与 `module/mod.rs` 是 TODO 空壳；README 说“embeddable”，但没有
  注册原生函数、宿主对象、模块加载、错误栈的 API。
- 文档与基线漂移；没有 CI；`cargo clippy` 442 warnings；没有字节码 dumper/golden 测试；
  `Module` 把整份 IR 克隆进 `debug_instructions`（`codegen.rs:137`）只为 Display。

### 3.8 ChakraCore 对照（`~/code/open_source/ChakraCore`）

值得照抄的：

| ChakraCore 做法 | 证据 | 对 biujs 的映射 |
|---|---|---|
| 字节码一张宏驱动主表，操作数布局与语义分离 | `lib/Runtime/ByteCode/OpCodes.h`、`OpLayouts.h` | `Opcode`/`Desc` 已接近；补一张 `OpInfo`（name/shape/effects），供 dumper/verifier/未来 IC 共用 |
| 单点 `switch`，IP 在局部变量 | `lib/Runtime/Language/InterpreterLoop.inl:160-190` | 去掉 `step → 17 组 → 组内` 三层 match，改一层穷尽 match |
| 运行期单元是 per-function `FunctionBody` | `lib/Runtime/Base/FunctionBody.h` | 用 `FunctionCode { code, constants, eh, spans }` 取代全局扁平 `Module.instructions` |
| `DynamicObject` + 形状/属性槽 | `lib/Runtime/Types/DynamicObject.h:58-170` | `ObjectHeader { shape, slots, proto, flags }` |
| `PropertyId` 驻留 + inline/aux 双槽 | `lib/Runtime/Base/PropertyRecord.h` | `Atom(u32)` + `PropertyKey::Index` |
| 内建表 + 整数枚举分派 | `lib/Runtime/Library/JavascriptBuiltInFunctionList.h`、`JavascriptLibrary.cpp:3688+` | `NativeId` + `BUILTIN_TABLE`，不再按名字字符串 |
| 统一入口签名，`this` 与参数分开 | `JavascriptFunction.h:136-142` | `fn(&mut Vm, this: Value, args: &[Value])` |
| 独立 bytecode dumper + dump 基线 | `lib/Runtime/ByteCode/ByteCodeDumper.cpp`、`test/*/rlexe.xml` | `BIUJS_DUMP` 升级为稳定 dumper + golden 测试 |

明确不抄（对无 JIT 的 Rust 解释器是过度设计）：

- 三档操作数编码 / 6 个 prefix opcode（`OpLayoutsCommon.h`）；
- `Profiled*` 双份 opcode、IC 预热、`InlineCache` 失效链（`OpCodes.h:80-118`、`InlineCache.h`）；
- 10 个 `TypeHandler` 子类与 lock+copy 演进（`lib/Runtime/Types/*`）——biujs 只需
  `Shape` 快路径 + `Dictionary` 慢路径两个分支；
- conservative stack scan + card table（`Recycler.cpp:1652-1779`）——Rust 做不到，
  且现有 `Gc<T>`+`Trace`+`Root` 更正确；
- JsBuiltIn（用 JS 写内建再编译内嵌）、bailout、JIT/IRBuilder；
- `SparseArraySegment` 链式分段（先用 `Vec<Element>` + 稀疏 map 即可）。

**最重要的对照结论**：ChakraCore 的 AST 只服务 `ByteCodeEmitter`，IR/SSA 属于可整体
删掉的 Backend。biujs 没有 JIT，却保留了 Backend 形态的 IR/SSA/regalloc —— 这是
§3.2 建议“先删双路径、再 spike 直出”的直接依据。

---

## 4. 目标架构

```
JS 源码
  │ oxc parser
  ▼
AST ── ScopeTree / 脱糖（语义、解构、class、生成器状态）── BytecodeEmitter
  │                                                         │
  │                                                         ▼
  │                                            FunctionCode { code, constants, eh, spans }
  │                                                         │
  ▼                                                         ▼
Engine/Context ── Realm { global, intrinsics } ── VM { Heap, Frame, EnvRecord }
                                                   │
                                    ┌──────────────┼──────────────┐
                                    ▼              ▼              ▼
                                  Frame          Heap          Builtins
                              (regs, pc, env)  (Gc, Trace)   (NativeId 表)
```

原则：
1. **字节码是唯一运行期契约**；AST/IR 不驻留，函数可独立编译与缓存。
2. **单一表示**：一个值一个存储位置；一个属性一个键表示；一个方法一个 ID。
3. **对象 = header + 少量 exotic**；shape 快路径 + dictionary 慢路径，不做 20 套 header。
4. **严格模式是引擎模式**，不是指令：所有源码按 strict 解析/执行；`with`/隐式全局
   永远报错；`noStrict` 用例跳过。
5. **没有 panic 的运行时**：所有操作数/常量访问是 `Option`/`Result`；`RefCell` 冲突
   转成 `InternalError`；`panic=abort` 只用于真正的 bug。
6. **GC 根是显式的**：`RootScope` RAII、`Trace` derive、槽 generation；收集只在
   安全点，原生回调期间要么完整入根、要么明确跳过。
7. **边界清楚**：`vm` 不依赖具体 builtin 类型；`builtins` 只依赖 `VmOps`/`Realm`；
   `host` 是唯一的嵌入接口。

---

## 5. 计划

> 估算是“单人 + AI 辅助”的粗估，单位人日；实际以每批验收为准。
> **每批的硬门槛**：`./scripts/phase2-status.sh` 无逐套件回退（guard 配置一致）、
> 护栏计数不增、墙钟 ±5%、新增测试覆盖、文档/快照同批更新。

### Phase 0 — 止血 + 测量可信化（5–10 人日，先做）

| # | 任务 | 验收 |
|---|---|---|
| 0.1 | 基线记录 guard 配置；`phase2-status.sh` 输出并比较 timeout/step/memory | 5 条 Array “回退”被解释为预算差异，或预算调回后归零 |
| 0.2 | `BoundaryGuard`（Drop 里 pop）；`drive_bytecode_frame`/`run_generator_frame`/`deliver_into_frame` 全改用它 | 构造“native 回调里栈溢出→被 JS 捕获→继续跑”，断言边界为空、堆继续回收 |
| 0.3 | `State::trace` 补 `pending_captured`；加 GC stress 单测 | `BIUJS_GC_STRESS=1` 下 feature 全绿 |
| 0.4 | `step` 里加 debug 断言：`rbp == frames.last().base`、`rsp` 边界、frame 深度 | 全量 debug 构建零命中 |
| 0.5 | `Operand::as_immediate() -> Option`；环境/调用指令的 `-1` 哨兵改 `Option`；`constants.get()` 替代索引 | `src/vm` 无 `as_immd` panic 路径；坏模块返回 `InternalError` |
| 0.6 | 字节码 dumper + 20–30 个脚本的 golden 测试；`Module` 去掉 `debug_instructions` 克隆（或改为可选 feature） | 同一源码字节码逐字节稳定；Module 内存下降 |
| ~~0.7~~ **已完成** | scope 表收口：删 `"Date"`/`"RegExp"`/`"label:"`/`print(`/`$ERROR` 等过宽 pattern；`noStrict`/`raw` 跳过；所有用例统一前置 `"use strict"`；`is_unsupported` 加了陷阱用例 | 全量 17,206 / 20,504（83.92%）；Object +166、String +49、Array +28；基线已刷新并写入 guard 行 |
| ~~0.8~~ **已完成** | strict 运行时收口：`with` 永远编译报错；严格模式隐式全局赋 `ReferenceError`（用"脚本声明过名字"集合区分）；顶层 `this = globalThis`；全局对象接 `Object.prototype` | 四条探针通过；`instanceof` 套件回到基线（顺带修掉一个真 bug：ES 12.10.4 第 4 步"RHS 不可调用应抛 TypeError"） |
| ~~0.9~~ **已完成** | CI：push/PR 跑 build + unit + features + guards + bytecode golden + 编译警告基线；周度 scheduled 跑全量 `phase2-status.sh` | PR 不再把回归留到人工发现 |
| ~~0.10~~ **已完成** | 文档去重：`scripts/doc-numbers.sh` 从基线快照生成 README / PROJECT 的通过率（含护栏预算），`--update` 自动调用；PLAN 的历史验收数字不属于管辖范围 | 一处数据源 |

### Phase 1 — 对象 / 环境 / 分派（30–50 人日，分 4 批）

**1A Atom + PropertyKey（5–8 人日）**

**两步都已落地（2026-10-25）。**

**第二步**：`PropertyKey::Str(Rc<String>)` → `Str(Atom)`，驻留表挂在 `Heap` 上
（`src/vm/atom.rs`，`WELL_KNOWN` 预驻留 + 动态表）。要点：

- 属性**存储**改成按 `PropertyKey` 建键（`globals`、`GlobalObject`、`OrdinaryObject`
  的字典都是），于是哈希/比较变成整数运算；
- 常用名比较走 `PropertyKey::is("length")`（静态表查一次 + 整数相等），不再解析回字符串；
- 只有真要字符串的地方（`Object.keys`、JSON、错误消息、Proxy trap 参数）才
  `key.resolve(&heap)` / `key.display(&heap)`；
- 内建函数各自按需加 `&mut Heap`（分派表本来就有 heap）；注册用的闭包从不被调用，
  一律换成占位实现 —— 顺手拆掉了"闭包捕获 `&mut heap`"那批假绑定。

**第一步（同日，先前）**：`PropertyKey::Index(u32)` + 唯一的规范索引判定
（`canonical_index` / `PropertyKey::array_index`）。索引键不再构造 `Rc<String>`，
也不再在每次访问时重新解析字符串；顺带修掉一个静默 bug —— 判定原先散在两处且
不一致（`keys()` 用规范判定，数组读写直接 `parse`），于是 `a["01"]` 读到了
`a[1]`。全量 +2 条（Object +1、Array +1），无回退。
验收（两步合并）：属性读不再有每次 `Rc::new(String)`；`Value` 仍 16 B；
常用名比较是整数比较；全量 17,273 / 20,504 = 84.24%，无逐套件回退。

坑（值得记住）：
- `PropertyKey::intern` 必须和 `from_str` 一样把规范索引折成 `Index`，否则
  `{0:'a'}` 存 `Index(0)`、查用 `Str("0")` 对不上；
- `State` 不持有堆，所以"名字 → 键"的驻留只能在 `VM` 侧做；`resolve_env_name`
  不再回退查 globals（改由 `LoadEnv`/`TypeOfEnv` 带堆回退）；
- 读路径用 `Heap::lookup`（不插入），避免未知变量名把驻留表撑大。

**1B ObjectHeader + Shape + GlobalObject（10–15 人日）**

**第一片已落地（2026-10-26）：全局对象的属性带上真描述符。**

`GlobalObject` 以前只是 `HashMap<PropertyKey, Value>` —— 一张裸表，于是每个内建
全局都是 `w/e/c = true`：`Object.keys(globalThis)` 会列出 50 个内建，`for-in` 也能
看到 `Math`。现在 `globals` / `GlobalObject` 存 `PropertyDescriptor`，并且：

- 内建注册用 `{writable: true, enumerable: false, configurable: true}`（ES 19.2）；
- `var` / 顶层函数声明按 `CreateGlobalVarBinding` 是 **non-configurable**：
  `delete globalThis.x` 现在是无操作（判据是名字在本程序里声明过；sloppy 给未声明
  名赋值仍然 configurable）；
- 写全局走描述符：只读的全局属性被观察到（strict 抛 TypeError，sloppy 静默失败）；
- `[[DefineOwnProperty]]` 走 `validate_property_redefinition`。

**验收：全量 17,273 → 17,304（84.24% → 84.39%，+31 条），无逐套件回退。** 提升全部
来自"内建全局非可枚举"这一条：每个内建各 +1，`Object` +9、`NativeErrors` +6。

**第二片已落地（2026-10-26）：`ObjectHeader`。**

`prototype` / `extensible` / `frozen` / `sealed` 以前在 7 个变体里各存一份，另有 7 个
变体内嵌 `OrdinaryObject`（`base`）间接持有 —— "取原型"有两条路，冻结/密封要同步三个
布尔。现在它们只有一处（`src/vm/object.rs` 的 `ObjectHeader`），20 个变体统一嵌入
（含 `iterator.rs` 的 `NativeIteratorObject` 与 `builtins` 的 `ProtoObject`）。

顺带把三处不一致收成 ES 的样子（这也是 +1 的来源）：
- `set_prototype` 一律拒绝冻结对象（以前只有部分变体检查）；
- `freeze` 一律置"已冻结"（以前 Function/NativeFunction/PrimitiveWrapper 只关了可扩展）；
- 全局对象的 `[[Prototype]]` 也是 GC 根（其它变体早就 trace，漏了它）。

**验收：全量 17,304 → 17,305（84.39% → 84.40%，+1），无逐套件回退。**

**第三片已落地（2026-10-26）：`undefined` / `NaN` / `Infinity` 成为全局对象的属性。**

ES 19.1.1–19.1.3：三者是全局对象的属性，`{writable: false, enumerable: false,
configurable: false}`。之前它们只是编译器里的字面量，于是
`Object.getOwnPropertyDescriptor(globalThis, 'undefined')` 是 `undefined`，而
`undefined = 5` 会造出一个可写的同名全局。现在：描述符可读、`getOwnPropertyNames`
列出它们、`Object.keys` 不列、`NaN = 5` 是空操作（严格模式 TypeError）。

顺带修掉一处：`var NaN;` 在严格模式不能抛 —— ES `CreateGlobalVarBinding` 第 4 步
规定"名字已存在时声明不写"（ES5 里 `NaN` 不是关键字，`S8.5_A4_T1` 就测这条）。当前
IR 把提升写拼成了普通 `StoreEnv undefined`，所以在 `StoreEnv` 的全局分支里按"值是
undefined 且名字是本程序声明的 var"判成声明。**待办**：IR 里加一条
`DeclareGlobalVar` 把"声明"与"赋值"真正分开，这段启发式才能去掉。

**验收：全量 17,305 → 17,311（84.40% → 84.43%，+6），无逐套件回退。**

**第四片已落地（2026-10-26）：把堆传到对象模型的可变路径（1D 入口签名的头一件）。**

`JSObject::property_set` / `define_property` / `property_delete` 现在带
`&mut Heap`（约 146 个调用点，含 `Object` 的转发、`prototype.rs` 的 `internal_set` /
`internal_delete`、以及一批原本拿不到堆的注册辅助函数）。**语义零变化**：全量
17,311 / 20,504（84.43%）与改前逐位相同，护栏计数不变。

它解掉的是一个具体的阻塞：形状（Shape）必须**按 VM 共享**（`Atom` 是 per-heap 的，
跨 VM 复用形状会把 `length` 和 `x` 串味），所以形状表得挂在堆上；而形状迁移发生在
"加属性"时 —— 也就是这三个方法里。此前它们只能 `&mut self`，Shape 无处落地。

**第五片已落地（2026-10-26）：`Shape` 结构与迁移表（`src/vm/shape.rs`）。**

纯新增，还没有接到对象上：`Shape { parent: Weak, table(全量键→槽位), keys(创建序),
added, slot_count, transitions }`，空形状挂在 `Heap` 上（`Heap::root_shape`）。

三个决定：
- **`table` 装全量键**而不是只装新增的那一个：读就是一次哈希，不必沿链往上走。
  代价是迁移要复制表 —— 我们的对象普遍只有几十个属性，形状数量有限，可接受
  （ChakraCore 的类型也是这么做的）。
- **父边用 `Weak`**：父→子强（迁移表要留住子形状才能复用），子→父弱，否则父子
  互相强引用就是个环，形状永远回收不掉。
- **形状按堆共享**：root 在 `Heap` 上；atom 是 per-heap 的，跨 VM 复用会把不同的
  字符串串成同一个键。

**验收：全量 17,311 / 20,504（84.43%）逐位不变；单测 255 → 259**（新增 4 条：
槽位按创建序、迁移共享、父边是 Weak、`added` 记录迁移键）。

**第六片已落地（2026-10-26）：`OrdinaryObject` 接入形状。**

`properties: PropertyTable` → `shape + slots + attrs + dict`：

- 默认数据属性（w/e/c 全真）只占一个**槽位**，值以裸 `Value` 存在 `slots` 里；
- 非默认属性（accessor / 只读 / 不可枚举 / 不可配置）额外存一份描述符到 `attrs`；
- **首次 `delete` 转字典模式**（`to_dict`），之后所有属性回到一张 `PropertyTable`
  —— 删除会打乱布局，硬留快速模式只会让形状越分越碎（V8 同样处理）；
- 形状的 root 在堆上，所以对象**创建时不需要形状**：`shape: None` 就是"还没有属性"，
  第一次加属性时才从 `heap.root_shape()` 起链 —— 这正是上一批把堆传进来的用处；
- `own_keys` 走 `Shape::keys`（创建序）再按 `OrdinaryOwnPropertyKeys` 重排（索引键
  升序在前）。排序抽成了 `es_own_key_order`，属性表与形状共用一遍。

过程中修掉的两处（都是"两条存储路径"的典型坑）：
- `own_keys` 忘了重排 ⇒ `{0:'p',1:'q',length:2}` 的键序变成 `length,0,1`；
- `property_set` 只写槽位没写 `attrs` ⇒ 非默认数据属性的赋值看起来没生效
  （`e.message = undefined` 不生效，`Error#toString` 就错了）。

**验收：全量 17,311 / 20,504（84.43%）逐位不变；单测 259 → 264**（新增 5 条：同布局
共享形状、非默认属性读写、删除转字典模式、索引键排序、冻结标属性）。

**第七片已落地（2026-11-10）：`FunctionObject` 接入形状槽位。**

函数对象此前自带 `ObjectHeader` + `PropertyTable`，并抄了一份属性实现：
`property_set` 无条件把属性覆盖成默认数据描述符（`f.length = 9` 会冲掉只读位）、
`property_delete` 不看冻结/密封、`seal` 只调 `prevent_extensions`。现在内嵌
`OrdinaryObject`（`base`），14 个 `JSObject` 方法全部委托，函数本体只留
`func_id` / `name` / 捕获环境；`Trace` 一并交给 `base`（槽位 / `attrs` / 字典三路）。
顺带收成 ES 的样子：`length` / `name` 的非默认描述符不再被赋值冲掉、类静态
getter 的 accessor 由同一套 `attrs` 保管、冻结/密封正确标注属性位、删除降级字典。

**验收**：三级 267 / 638 / 7 + 字节码 golden 全绿（新增 3 条单元 + 3 条 feature
断言）；`Object` 尺寸断言仍过。全量 **17,311 → 17,316 / 20,504
（84.43% → 84.45%，+5）**，无逐套件回退，护栏 0/0/14 与跳过 8577 均不变。
提升：`built-ins/Object` +2、`language/statements/function` +1、
`language/statements/generators` +1、`language/expressions/generators` +1。

**第八片已落地（2026-11-10）：`PrimitiveWrapperObject` 接入槽位 + 0.5 收尾。**

两件事同批，都是"两条属性存储路径 / 运行期隐式崩溃"这条线：

1. `PrimitiveWrapperObject` 的 `header + properties` 换成 `base: OrdinaryObject`，
   自有属性走 shape / `attrs` / 字典模式；String 包装的**虚拟**属性（`length` 与规范
   索引）抽成 `virtual_property()`，仍由被包装的字符串决定、不占槽位。顺带收掉三处
   旧偏差：普通赋值冲掉非默认描述符、删除不看 `configurable`/冻结、`seal` 只调
   `prevent_extensions`；`define_property` 对虚拟键校验通过后不再把它写进属性表
   （那会让 `own_keys` 出现重复索引键）。`Trace` 由 `base` 覆盖原型/槽位/attrs/字典。
2. **0.5 收尾**：`try_immd` / `immd` / `constant_at` 早已落地，但跳转类操作数
   `RelPc` / `AbsPc` 上 6 处运行期 `as_immd()` 仍会 `panic`（release 是
   `panic = "abort"`，一条坏字节码会带走整个分片）。现在两类都有 `try_immd()`，
   VM 侧新增 `pc_immd()` 统一转成 `RuntimeError::InternalError`，并补一条
   "畸形模块返回错误而非 abort"的单测。`src/vm` 运行期 `as_immd()` 归零
   （只剩编译期 codegen 与字节码单测在用）。

**验收**：三级 270 / 639 / 7 + 字节码 golden 全绿（新增 1 条 VM 单测、2 条 shape
单测、1 条 feature 断言）；`Object` 尺寸断言仍过；debug 构建（`cfg(debug_assertions)`
的 `Try` 一致性断言）编译通过。全量 **17,316 → 17,329 / 20,504
（84.45% → 84.52%，+13）**，无逐套件回退，护栏 0/0/14 与跳过 8577 均不变，
墙钟 108.3s。提升：`built-ins/Object` +12、`built-ins/Proxy` +1。

**第九片已落地（2026-11-11）：对象层的三条边界收口。**

三件事都属于"trait 的签名或职责说不清，于是规则被迫散落在调用方"：

1. **链上 Proxy**：`[[Get]]` / `[[Set]]` / `[[HasProperty]]` 的链式遍历原来用
   `prototype::find_descriptor` / `internal_has_property`（纯描述符 walk），只检查
   **起点**是不是 Proxy。于是链上任意一跳是 Proxy 时 trap 根本不跑：
   `Object.create(new Proxy({foo:1},{get(){return 42}})).foo` 得到 `1`，
   `('zzz' in Object.create(new Proxy({},{has(){return true}})))` 得到 `false`。
   现在三条操作各有 VM 侧链式实现（`object_get_chain` / `set_member` /
   `object_has_property`），**每一跳都先问 Proxy**；`Reflect.has` 也改走 VM
   （builtins 层没有 VM，原来直接落到纯 walk）。`proxy_has` 的无 trap 分支同样
   回到链式实现，嵌套 Proxy（target 也是 Proxy）因此正确。
2. **String 包装的索引键按码元**：`own_keys` 用 `chars().count()`（码点），而
   `[[GetOwnProperty]]` / `has_own_property` 用 UTF-16 码元 ——
   `new String("😀")` 的 `Reflect.ownKeys` 只报 `0`，但 `hasOwnProperty("1")` 是 true。
3. **`[[SetPrototypeOf]]` 返回 bool**：`JSObject::set_prototype` 从 `()` 改回
   `bool`，判定收进 `ObjectHeader`（相同 ⇒ true；不可扩展 ⇒ false），
   `Object.setPrototypeOf` 只负责把 false 变成 TypeError、
   `Reflect.setPrototypeOf` 原样返回，环检测抽成
   `prototype::prototype_chain_contains`（原来两处各写一份 while 循环）。
   顺带删掉 `JSObject: Any`（`as_any` 早已不存在）。

同批还合并了一个冗余变体：`Object::Proto` 与 `Object::Ordinary` 的差别只有
`class_name`，而 `OrdinaryObject` 的默认值就是 `"Object"` —— 内建原型对象现在
直接是 `Object::Ordinary`，20 个变体收敛到 19 个，少了一整套手写的
`impl JSObject` + `impl Trace`。

**验收**：三级 272 / 651 / 7 全绿；全量 **17,481 → 17,488 / 20,504
（85.25% → 85.29%）**，无逐套件回退（唯一变化 `built-ins/Proxy`
**219 → 226，失败 43 → 36**），护栏计数不变。新增回归：`proxy.rs`
`traps_fire_on_the_prototype_chain`、`string_utf16.rs`
`string_wrapper_own_keys_are_code_units`、`reflect.rs` 的不可扩展同原型断言。

**第十片已落地（2026-11-11）：属性错误通道类型化。**

`JSObject::property_set` / `define_property` 的错误从 `String` 换成
`PropertyError { kind: Type | Range, message }`（`vm/property.rs`）：

- 唯一的 RangeError 来源是 `ArraySetLength`（ES 10.4.2.4）。以前它靠
  `PROPERTY_ERROR_RANGE_PREFIX = "RangeError: "` 穿过对象层，再由
  `RuntimeError::from_property_error` 剥前缀 —— 任何一次 `format!` 拼接都会让
  `Object.defineProperty([], "length", {value: -1})` 静默变成 TypeError。
- 现在两个方向都是 `From`：`From<PropertyError> for RuntimeError` 是唯一还原点，
  `From<RuntimeError> for PropertyError` 供 `validate_array_length` 这类"校验用
  RuntimeError"的辅助函数折回属性通道。
- 37 处 `Err("…".to_string())` 与 29 处签名机械迁移；`PropertyError` 的
  `kind()` / `message()` 取代了"看字符串前缀"的推断。

**验收：三级 272 / 651 / 7 全绿；全量 17,488 / 20,504（85.29%）与改前**
**逐位相同**（纯类型重构，语义零变化），无逐套件回退，护栏计数不变。

**第十一片已落地（2026-11-11）：`has_property` → `has_own_property`。**

ES 的 `[[HasProperty]]` 沿原型链，而对象层的这个方法只查自有 —— 同名不同义。
上一片修的"链上 Proxy 不触发 trap"有一半根源就是这条边界没写进名字
（`internal_has_property` 与 `has_property` 看起来都是 "has property"）。
改名后调用点自解释；链式查询只保留 `VM::object_has_property` 与
`prototype::internal_has_property`。**全量逐位不变。**

**第十二片已落地（2026-11-11）：判别点收敛 + `[[Delete]]` 语义。**

- `ObjectKind` 与 `Object` 变体对齐：删 `Error` / `Arguments`（没有任何对象
  返回它们），`Buffer` → `ArrayBuffer`（前者是 Node 语义）。`kind()` 被当类型
  判别用了约 50 处，永不返回的值等于永不匹配的分支。
- `kind() == ObjectKind::Proxy` 的 8 处散写收敛为 `Object::is_proxy()`。
- `[[Delete]]`（ES 10.1.9.2）：`OrdinaryObject` / `NativeFunctionObject` 在属性
  不存在时返回 false，且先无条件 `to_dict()` —— `delete o.missing` 会把对象永久
  打成字典模式；`NativeFunctionObject` 还完全没检查冻结/密封。这些以前被
  `internal_delete` 入口的提前判断掩盖。同时把 `is_frozen_desc_set` /
  `is_sealed_desc_set` 从 `&dyn JSObject` 改成泛型。

**验收：全量 17,488 / 20,504（85.29%）逐位不变，无逐套件回退。**
**踩坑**：给 Array 的 hole 情形加 `return true` 早退时忘了删 `properties` 里的
条目 —— accessor 索引同时是 hole 与 properties 条目，`delete arr[0]` 留下删不掉
的 getter，全量 -45（Array -10、Object -35）。

**第十三片已落地（2026-11-12）：`NativeFunctionObject` 接入 `OrdinaryObject`。**

它是最后一个自带 `ObjectHeader` + `PropertyTable` 并手写 15 个 `JSObject` 方法的
变体。迁到 `base: OrdinaryObject` 之后：

- 手写实现缩成一行 `delegate_js_object!`；
- `length` / `name` 由新增的 `OrdinaryObject::install_descriptors` 安装 ——
  构造期拿不到 `&mut Heap`（形状 root 在堆上），所以直接进**字典模式**。内建函数
  对象本来就只有个位数属性，且它们在这之前就常年是 `PropertyTable`，没有退化；
- 顺手修掉手写 `property_set` 覆盖只读位的偏差（`Array.isArray.length = 9`）；
- 删掉失去调用者的 `freeze_property_table` / `seal_property_table`。

**第十四片已落地（2026-11-12）：严格模式的 `delete`。**

ES 12.5.4.1：`[[Delete]]` 答 false 时严格模式抛 TypeError，sloppy 只求值为 false。
`PropDelete` / `IndexDelete` 以前两种模式都静默写回 false，
`"use strict"; delete Object.freeze({x:1}).x` 因此不抛（`Reflect.deleteProperty`
按规范返回布尔，走的是另一条路，不受影响）。

**验收：全量 17,488 → 17,499 / 20,504（85.29% → 85.34%）**，无逐套件回退：
`language/expressions/delete` +6、`built-ins/Proxy` +2（`deleteProperty` trap 的
不变式用例）、`built-ins/Object` +2、`built-ins/Boolean` +1；lib 272 /
features 654 / guards 7 全绿，护栏 0/0/14 不变。

**第十五片已落地（2026-11-12）：运算符的 `ToNumeric` 与 `Date` 的 `default` hint。**

三处都在"运算符直接读 `Value::to_number()`"上：

- `>>>` 的结果被 `as i32` 折回有符号：`-1 >>> 0` 得到 -1、`2147483648 >>> 0`
  得到负值 —— 整条无符号右移语义都是错的。现在按 u32 → f64。
- 位运算（`& | ^ << >> >>> ~`）与一元 `-` 直接调 `Value::to_number()`：对象操作数
  跳过 `ToPrimitive`（`{valueOf(){return -4}} >>> 1` 得到 0），Symbol 折成 NaN 而
  不抛。新增 `VM::to_numeric`（`ToPrimitive(hint number)`，Symbol ⇒ TypeError），
  `+ - * / % **`、位运算、一元 `-` / `~` 全部改走它。
- `Date` 的 `ToPrimitive` 在 `"default"` hint 下按 `"string"` 走（ES 20.3.4.2）：
  `date + date` 是字符串拼接而不是数值相加。`Date.prototype[@@toPrimitive]` 本体
  要实现 OrdinaryToPrimitive（调 valueOf/toString），内建层装不了，所以在
  `VM::to_primitive` 里折算 hint，可观察结果与规范一致。

**验收：全量 17,499 → 17,539 / 20,504（85.34% → 85.54%），无逐套件回退。**
提升：`unsigned-right-shift` 24→37、`bitwise-and/or/xor` 19→22、
`bitwise-not` 11→13、`unary-minus` 10→11、`left-shift` 34→37、
`right-shift` 26→29、`addition` 35→39、`multiplication` 32→33、`division` 35→36、
`modulus` 31→32、`subtraction` 30→31、`exponentiation` 34→35。

**第十六片已落地（2026-11-12）：函数体之外的 `return` 是 Early Error。**

ES 12.9：`ReturnStatement` 不在 FunctionBody / 箭头函数体 / 方法体里就是
SyntaxError。oxc 的 parser 与 semantic builder 都不报这一条，于是
`return 1;` 在脚本顶层能跑通并返回 1（`language/statements/return` 整组失败）。

`semantic.rs` 新增 `function_depth`：函数声明 / 函数表达式 / 箭头函数体的遍历
`+1`，`ReturnStatement` 在深度 0 时记 CompileError；顺带补上
`DoWhileStatement` 的遍历（`do { return; } while (0)` 以前整条被 visitor 跳过）。
已知边界：类方法体、`switch` 的 case 体仍未遍历 —— 这是**漏报**（不误报），
按"先不把合法代码判错"的顺序留着。

**验收：全量 17,539 → 17,549 / 20,504（85.54% → 85.59%），无逐套件回退**
（`language/statements/return` 5 → 15）。

**第十七片已落地（2026-11-12）：`instanceof` 与 `Function.prototype`。**

`instanceof` 按 ES 12.9.4 / 7.3.19 归位：

- 两侧都物化。以前只物化 RHS，而函数声明产生的 LHS 是裸 `Value::Function(id)`，
  链遍历起点直接是 `None` —— `function f(){}; f instanceof Function` 得到 false。
- `prototype` 走 **Get**（accessor 会执行），且结果必须是对象，否则 TypeError。
  以前用 own-only 的 `property_get`（getter 不跑）并把"取不到"当 false。
- 左侧不是对象 ⇒ false，且发生在 `Get(C, "prototype")` **之前**
  （`0 instanceof Function.prototype` 既不跑 getter 也不抛）。

`Function.prototype` 改成真正的**函数对象**（ES 19.2.3）：`typeof` 是 "function"、
调用返回 undefined、`length` 0、`name` ""、`[[Prototype]]` 是 `Object.prototype`。
以前它是普通对象，`[] instanceof Function.prototype` 一律 "not callable"。

**验收：全量 17,549 → 17,563 / 20,504（85.59% → 85.66%）**，无逐套件回退：
`language/expressions/instanceof` 22→27、`built-ins/Function` 170→178、
`language/expressions/property-accessors` 15→16。

**第十八片已落地（2026-11-12）：`IsConstructor` 与构造器注册。**

`NativeFunctionObject` 新增 `constructible` 标志（默认 false），由构造器注册点
显式标 true：`link_constructor_prototype`（20 处，覆盖 Object/Array/Function/
Error 家族/Map/Set/Weak*/Promise/Date/RegExp/ArrayBuffer/DataView/TypedArray）
与 `Proxy`（它没有 `prototype` 属性，不走那个函数），`Symbol` 在注册后显式取消
（有 `Symbol.prototype` 却不是构造器）。

两处消费它：

- `native_construct` 入口：非构造器的内建函数 `new` 时抛 TypeError ——
  `new Math.max()`、`new (function(){}).call()`、`new Symbol()`；
- `is_constructor`（`Reflect.construct` 的 newTarget 检查）：原来靠"名字里有没有
  `.`"猜，于是 `Function.prototype.apply` 这种不带点的内建方法被当成构造器；
  箭头函数也改成按 `captured_this` 判定。

顺带把 `instanceof` 抽成 `VM::instanceof_operator`，以支持规范要求的递归：C 是
bound function 时对它的 target 重新做一次（7.3.19 step 2）—— bound function
自己没有 `prototype`，`new (F.bind(null))() instanceof F.bind(null)` 以前在
step 4 拿到 undefined 就抛了。

**验收：全量 17,569 → 17,731 / 20,504（85.69% → 86.48%），无逐套件回退。**
提升：`Array` +41、`String` +32、`TypedArray` +30、`Set` +15、`Map` +12、
`Function` +8、`WeakMap` +6、`Number` +6、`Object` +5、`WeakSet` +3、
`Boolean` +2、`Symbol` +2、`Promise` +2、`Proxy` +1、`Error` +1，两个
`Symbol.iterator` 子套件各 +1。

剩余（按序）：
- descriptor 惰性物化：`Map` / `Set` 也换到槽位（`NativeFunction` 已完成），
  把 `PropertyTable` 收成"只有字典模式用"。`Map` / `Set` 牵动 `WeakMap` /
  `WeakSet` 的 ephemeron trace；`Array` 与 1C 耦合，留到那时。
- `Realm` / `EnvRecord`；对象尺寸断言仍过。

**1C 数组 + 函数（8–12 人日）**
- `Vec<Element>` + `BitSet` holes + 稀疏 fallback；删 `MAX_DENSE_ELEMENTS` 硬报错。
- 统一索引 get/set/has，消除 `array.rs:315-344` 与 `vm/mod.rs:754-873` 的双语义。
- 删 `Value::Function(u32)`；函数一律 `Gc<Object>`。
- 验收：Array hole/accessor/species 用例；`new.target`/`instanceof` 同一性；无回退。

**1D NativeId + HostOps（8–12 人日）**
- `NativeId` + `BUILTIN_TABLE`；prefix/payload 字段化；删 `vm_handled_static`。
- 统一 `ToObject`/`ToPropertyKey`/`LengthOfArrayLike`/species/迭代协议；
  `VmOps` 门面切断循环依赖。
- 验收：新增一个原型方法 ≤ 2 处；`grep strip_prefix` 在分派路径归零；
  `builtins/` 不出现 `crate::vm::object::` 具体类型。

### Phase 2 — 帧 / GC / 字节码（20–30 人日）

- **2A 几何单一所有者**：`Geometry { rsp, rbp, closure, seh, frames, delegate }`；
  `unwind_to(Geometry)` 唯一写者；挂起/恢复显式 `Suspended`；删 `saved_rsp/rbp` 特例。
- **2B 每帧寄存器窗口**：`Frame { regs: Box<[Value]>, code: Rc<FunctionCode>, pc, env }`；
  删 codegen 的 call-save Push/Pop 与 `SavedExecutionState.registers` 快照。
- **2C 字节码 v2**：per-function `FunctionCode`（局部 pc）、`OpInfo` 表、
  `Option<Operand>`、单层穷尽 match、无 `handled: bool`/静默 fallback；
  合并 Index/Prop；`Await` 归位；`Try` 死操作数删除。
- **2D GC**：`Slot.generation` + `Gc{index,generation}`（先 debug 校验、release 可报错）；
  `Trace` derive/macro；`RootScope` RAII；原生回调完整入根后再撤 `invoke_boundaries` 闸门。
- 验收：生成器/yield/EH 全绿；GC stress 全量零 abort；每步断言零命中；
  字节码 trace 与旧版逐条等价（除预期合并）。

### Phase 3 — 编译器（30–50 人日，先 spike）

- **~~3A~~ 已完成**：删 `BIUJS_REG_VARS` 与变量寄存器分配；固定内存驻留语义。
  `regalloc.rs`（1063 行）→ `frame_slots.rs`（约 100 行，含 2 个单测）；
  字节码 golden 中 2/16 个文件变化，逐条核对为**纯槽位重编号**（指令序列与帧大小不变）。
- **3B** `ScopeTree` 统一 semantic/symbol/自由变量走查；捕获环境变 `(depth,index)`。
- **3C** 直出 spike：表达式 + 语句 + 调用 + 简单函数，产出 `FunctionCode`；
  对比 LOC / 编译时间 / test262 通过数。**只有在同等通过数下才继续。**
- **3D** 分片迁移 lowering（表达式 → 语句 → 解构/默认参数 → class → 生成器）；
  每片跑全量；删 IR/SSA/regalloc/codegen 的旧路径。
- **3E** span/`CompileError` 贯穿：不支持语法显式报错；错误带行列；运行期错误带栈。
- **3F** `SourceType` 参数化；modules 要么实现，要么显式 `NotImplemented`。

### Phase 4 — 语义补齐（60–100 人日，可与 1–3 并行）

按“解锁条数 / 依赖”排序：

| 优先级 | 事项 | 预期 | 依赖 |
|---|---|---|---|
| P4-1 | Date 去门控 + 入册 + 修 318 条 | +265 通过，但总率下降（入册） | 0.7 |
| P4-2 | `$262.detachArrayBuffer` + detached/resizable buffer；AB/DV/TAConstructors 入册 | +735 通过；先补 887 失败 | 1C、2A |
| P4-3 | Array：holes、species、`Array.from` 通用迭代器、`isArray`/Proxy | +300…+400 | 1C |
| P4-4 | Object：描述符、Annex B、`ToPropertyKey`、global | +150…+250 | 1B |
| P4-5 | class 求值顺序 / own property / `super` | +200 | 1B |
| P4-6 | generators/for-of：`IteratorClose`、next-result、finally return | +120…+160 | 2A |
| P4-7 | Promise：真挂起 `await`、与微任务队列交错 | +80…+140 | 2B |
| P4-8 | Error：`ErrorObject` + `stack` 访问器 | +45…+60 | 1B |
| P4-9 | Function：`toString`、`length`/`name`、bind/call/apply | +80…+120 | 1D |
| P4-10 | Number：toFixed/toPrecision/radix | +60…+100 | 无 |
| P4-11 | String：UTF-16 存储、`Symbol.match/replace/search/split` | +50…+90 | 1A |
| P4-12 | TypedArray 长尾：索引 exotic、descriptor、species、from/of、回调 | +400…+500 | 1C/1D |
| P4-13 | RegExp 引擎（lookaround/backref）| +40…+80（String）+ 可选 RegExp 套件 | 决策 |
| P4-14 | modules | 未知（755 文件） | 决策 |

### Phase 4 首批（2026-11-10）：内建原型的内部槽

按「先量后改」：定向跑 `built-ins/Number,built-ins/Boolean` 的失败明细，
35 条 `Number.prototype.toString(N)` 与 Boolean 的 `toString`/`valueOf` 一族都
报 "[object Object]" —— 根因是 `Boolean.prototype` / `Number.prototype` 被建成
普通 `ProtoObject`，没有 ES 19.3.3 / 20.1.3 规定的
[[BooleanData]] = false / [[NumberData]] = +0。现在两者在 `Builtins::new` 里建成
`PrimitiveWrapperObject`（原型为 `Object.prototype`），于是 `toString` /
`valueOf` / `== false` 都走共享的包装对象路径；`Object.prototype.toString` 的
通用分派没有动（同名方法不能按 `this` 猜归属，那是 1D 的债）。
`String.prototype` 有虚拟索引与 `length` 的额外语义，留到下一片。
顺带修 `Number.isSafeInteger` 对非 Number 参数误做 `ToNumber`（"1" 被判成安全整数）。

**验收**：三级 270 / 640 / 7 + 字节码 golden；全量 **17,329 → 17,376 / 20,504
（84.52% → 84.74%，+47）**，无逐套件回退，护栏 0/0/14 与跳过 8577 不变，
墙钟 107.3s。`built-ins/Number` 197 → 240（失败 105 → 62）、
`built-ins/Boolean` 29 → 33。

`Number` 剩余 62 条里可直接继续的：数值格式化方法对非法参数抛出的错误类型
（`e instanceof TypeError` 10 条 / RangeError 2 条）、
`Number.parseFloat === parseFloat` 的函数同一性（2 条）、
`Object.prototype.toString.call(Number.prototype)` 的 `[[Class]]`（2 条）。

### Phase 4 第二批（2026-11-10）：严格模式的 `%ThrowTypeError%`

定向明细里 `built-ins/Function` 的 28 条 `15.3.5-*gs.js` 都只断言「读
`fn.caller` 应抛 TypeError」，根因是 `Function.prototype` 缺 ES 9.2.7 的
`caller` / `arguments` 访问器。现在两者注册为 `%ThrowTypeError%`（getter/setter
同一个内建，非枚举、不可配置），按名字分派加一条 TypeError 返回；feature
断言同时钉住描述符形状。

**验收**：三级 270 / 641 / 7 + 字节码 golden；`built-ins/Function`
141 → 170（失败 128 → 99）；全量 **17,376 → 17,413 / 20,504
（84.74% → 84.92%，+37）**，无逐套件回退，护栏 0/0/14 与跳过 8577 不变，
墙钟 107.2s。除 Function 外还带动 `language/statements/function` +4、
两处 class +3、generators +1 —— 同一族 strict-mode 探针散落在多个套件里。

### Phase 4 第三批（2026-11-10）：数组 length 描述符的 ToNumber

`built-ins/Object/defineProperty` / `defineProperties` 的 14 条
`15.2.3.6-4-1xx` / `15.2.3.7-6-a-1xx` 把 `length` 的 [[Value]] 设成带
`toString` 的对象，期望 `arr.length` 变成对应数字；实际抛
`RangeError: Invalid array length`。根因是 `ArrayObject::define_property`
用 Rust 层的 `Value::to_number()`，不会调用用户 `valueOf` / `toString`，得到 NaN。

修法不改 `ArrayObject` 的接口：在 `Object.defineProperty` /
`defineProperties` 的 VM 分支（`vm/builtins_dispatch.rs`）里，对 Array +
`length` 的描述符先用 `VM::to_primitive(..., "number")` + `to_number_throwing`
把 `value` 字段换成 Number，再交给 builtins 层。`ToPropertyDescriptor` 本来就在
VM 层读过字段（含用户 getter），这里只补 ES 10.4.2.4 的 ToNumber 前置步骤，
其余对象/键的路径一字未动。

**验收**：三级 270 / 642 / 7 + 字节码 golden；`built-ins/Object`
3095 → 3107（失败 284 → 272）；全量 **17,413 → 17,425 / 20,504
（84.92% → 84.98%，+12）**，无逐套件回退，护栏 0/0/14 与跳过 8577 不变，
墙钟 104.4s。

### Phase 4 第四批（2026-11-10）：`ToPropertyKey` 走 VM 层 + `Object.is` 缺省参数

Object 剩余 272 条里最大的一族是 22 条 `Cannot read properties of undefined
(reading 'value')`：`Object.getOwnPropertyDescriptor(obj, {toString(){…}})`
的 key 在 builtins 层用 Rust 层 `to_js_string()` 转换，不跑用户 `toString`，
于是查不到属性、返回 undefined。同类还有 `Object.hasOwn` /
`Object.prototype.hasOwnProperty` 用 `Symbol.toPrimitive` 返回 Symbol 键的 6 条。

现在三者都在 VM 层过 `to_property_key_value`（复用 `reflect_key_args` 的既有
helper）：静态方法在 `call_native_by_name` 转换第二个参数，`hasOwnProperty`
在 `CallMethod` 分派前转换第一个参数；builtins 层只接收 String / Symbol。
另外 `Object.is()` / `Object.is(x)` 的缺失参数按 undefined 参与 SameValue
（原来直接抛 TypeError，4 条）。

**验收**：三级 270 / 643 / 7 + 字节码 golden；`built-ins/Object`
3107 → 3121（失败 272 → 258）；全量 **17,425 → 17,439 / 20,504
（84.98% → 85.05%，+14）**，无逐套件回退，护栏 0/0/14 与跳过 8577 不变，
墙钟 106.7s。

### Phase 4 第五批（2026-11-11）：补齐 `Date.prototype` 缺失的 get/set 一族

`built-ins/Object/getOwnPropertyDescriptor/15.2.3.3-4-135` 与 `141..151`
共 12 条只检查一件事：`Object.getOwnPropertyDescriptor(Date.prototype, name)`
要返回 data 描述符，且 `desc.value === Date.prototype[name]`。实测这些名字
（`getUTCMilliseconds`、`setHours` / `setMinutes` / `setSeconds` /
`setMilliseconds` 与七个 `setUTC*`）在原型上**根本没有属性** —— 既没注册，
也不在按名字分派表里。这是 1D 名字分派债的一个具体缺口。

现在在 `register_date_prototype` 里注册这 12 个方法，并在
`call_prototype_method` 接入分派。实现复用 `parts` / `time_from_components`，
新增 `date_set_parts(this, args, start, count, utc)`：从 `start` 分量起用最多
`count` 个实参覆盖，其余保留，UTC 变体按 UTC 拆分与重建（越界值沿用既有
进位规则）。`getUTCMilliseconds` 直接走 `date_component(obj, 7, true)`。
feature 断言同时钉住描述符与 `setUTCHours(1,2,3,4)` 的实际结果。

**验收**：三级 270 / 644 / 7 + 字节码 golden；`built-ins/Object`
3121 → 3133（失败 258 → 246）；全量 **17,439 → 17,452 / 20,504
（85.05% → 85.12%，+13）**，无逐套件回退
（`language/expressions/property-accessors` 顺带 +1），护栏 0/0/14 与
跳过 8577 不变，墙钟 106.9s。

### Phase 4 第六批（2026-11-11）：内建原型属性收口 + 0.2 / 1B 收尾（批量）

按「批量修改、统一验证」一次落地三组：

1. **`RegExp.prototype` 的只读 accessor**（ES 22.2.5）：`source` / `flags` /
   `global` / `ignoreCase` / `multiline` / `sticky` / `unicode` / `dotAll`
   以前一个都没注册，`RegExp.prototype.source` 是 undefined，反射 API 也查不到
   （`15.2.3.3-4-212..215`）。现在注册为 accessor：getter 是带
   `__regexp_get__` 前缀的内建，按名字分派到 `regexp_prototype_getter`，从
   `RegExpObject` 读 source/flags；`RegExp.prototype` 自身按空正则返回
   `"(?:)"` / 空 flags。顺带把 `Array/Number.prototype.toLocaleString` 注册成
   自有属性，并让 Array 的 `toLocaleString` 走 join 而不是 `[object Array]`。
2. **0.2 收尾**：`with_invoke_boundary` 内定义 Drop 型 `BoundaryGuard`（弹出 +
   嵌套深度互校）；`drive_bytecode_frame` 拆成 RAII wrapper + `inner`，三处手动
   push/pop 全部移除。补一条嵌套守卫单测（原有两条泄漏回归继续覆盖 setup 失败路径）。
3. **1B 第九片**：内建原型 `ProtoObject` 的 `header + PropertyTable` 换成
   `OrdinaryObject` 委托（14 个 `JSObject` 方法 + `Trace`），与函数 / 包装对象
   共用 shape 槽位 / `attrs` / 字典降级；顺带收掉原型对象 `property_set`
   无条件覆盖描述符、`delete` 不看 `configurable`、`freeze/seal` 只动头部的偏差。

**验收（一次统一验证）**：三级 271 / 646 / 7 + 字节码 golden 全绿；全量
**17,452 → 17,462 / 20,504（85.12% → 85.16%，+10）**，无逐套件回退
（`built-ins/Object` +6、`built-ins/Number` +2、`built-ins/Array` +1、
`language/expressions/assignment` +1），护栏 0/0/14 与跳过 8577 不变，
墙钟 116.7s。

**仍未做（有硬前置，不塞进本批）**：0.4 完整几何断言（生成器挂起路径的帧模型
属 Phase 2）、S0#4 的 `Gc` 句柄代次（`Value == 16 B`）、`invoke_boundaries`
收集闸门（原生回调完整入根之前不能撤）、`rsp` 单写者（2A 前置）、1B 剩余
`NativeFunction`（`new` 无堆参数，需改约 50 处调用点）/ `Map` / `Set`
（牵动 `WeakMap` / `WeakSet` 的 ephemeron trace）、1C / 1D / Phase 2 / 3 / 4 / 5。

### Phase 4 第七批（2026-11-11）：1B Map/Set 委托 + `defineProperty` 的 `ToPropertyKey`

1. **1B 第十 / 十一片**：`MapObject` / `SetObject` 的 `header + PropertyTable`
   换成 `OrdinaryObject` 委托（14 个 `JSObject` 方法 + `Trace` + ephemeron 的
   `trace_shallow`），集合 `entries` 完全不动，因此 `WeakMap` / `WeakSet` 的
   ephemeron 语义不受影响。顺带收掉赋值冲掉描述符、`delete` 不看
   `configurable`、`freeze/seal` 只动头部的偏差。
2. **`Object.defineProperty` 的属性键**：与 `getOwnPropertyDescriptor` /
   `hasOwn` 同族 —— `Object.defineProperty(obj, {toString(){…}}, desc)` 以前
   用 Rust 层 `to_js_string()`，不跑用户转换（`15.2.3.6-2-43..48`）。现在把
   它加进 VM 层 `reflect_key_args` 名单；`defineProperties` 的第二参数是
   props 对象，刻意**不**加。

**验收（统一验证）**：三级 272 / 648 / 7 + 字节码 golden 全绿；全量
**17,462 → 17,475 / 20,504（85.16% → 85.23%，+13）**，无逐套件回退
（`built-ins/Object` +6、两处 `class` 各 +2、`computed-property-names` +2、
`language/expressions/object` +1），护栏 0/0/14 与跳过 8577 不变。

仍未做（硬前置，见 §6.1）：`NativeFunctionObject`（56 处构造调用点，
`new` 无堆参数）、0.4 完整几何断言（Phase 2 帧模型）、S0#4 `Gc` 代次
（`Value == 16 B`）、`invoke_boundaries` 闸门、`rsp` 单写者、1C / 1D 及
Phase 2 / 3 / 4 / 5。

### 对象模型重构片 1（2026-11-11）：统一 [[Get]]/[[Set]] 的 accessor 语义

按上一轮对 `gc.rs:762-784` 枚举 + `JSObject` 胖接口的设计分析，第一片先修
「两条路径语义相反」这个会掩盖后续重构回归的前置问题：

1. **删除死路径 `prototype.rs::internal_set`**：它遇到 accessor 是
   `TODO: call setter`，实际用 `property_set` 把 accessor 覆盖成数据属性，
   与 `VM::set_member` 真正 `invoke(setter)` 相反；全仓无调用点。文件里写明
   契约：[[Get]]/[[Set]] 的唯一活入口是 `VM::get_member` / `VM::set_member`，
   本文件只提供不执行用户代码的描述符 walk（`internal_get` / `find_descriptor`）。
2. **`lookup_property_on_object` 补 getter 调用**：以前用 `internal_get` 跳过
   accessor（`vm/mod.rs:845` 曾为同类 bug 打过补丁），现在与 `get_member`
   一致 —— `find_descriptor` + `invoke(getter)`，且不重复触发 Proxy trap。
3. **`Object.prototype.__proto__` accessor**（ES Annex B.2.2.1）：以前完全没注册，
   `Object.getOwnPropertyDescriptor(Object.prototype, '__proto__')` 是 undefined。
   现在注册 `{get, set, enumerable:false, configurable:true}`；getter 复用
   `Object.getPrototypeOf`，setter 复用 `Object.setPrototypeOf` 的循环检测但按
   Annex B 失败静默。`JSON.parse` 创建的同名自有数据属性不受影响。

**验收（统一验证）**：三级 272 / 649 / 7 + 字节码 golden；全量
**17,475 → 17,481 / 20,504（85.23% → 85.26%，+6）**，无逐套件回退
（`built-ins/Object` +5、`language/expressions/call` +1），护栏 0/0/14 与
跳过 8577 不变。

**片 1 未做（留到片 2/3/4）**：`internal_get` 另 3 处调用点仍是 data-only
（需与 trait 收敛一起改成 VM 读取）；抽 `ObjectLayout`、`JSObject` 瘦化、
`forward!` 收敛；Array / Global / NativeFunction 存储收编；原型表从
`thread_local` 改挂 Realm；对象字面量 `__proto__: p` 仍创建自有属性而非设置原型。

### 对象模型重构片 2（2026-11-11）：分派表与委托样板单一化

片 1 统一了 accessor 语义；片 2 收敛「新增一个变体要同步多张表」的问题，
**零运行时语义变化**：

1. `gc.rs` 新增 `for_each_variant!`：20 个 arm 的变体清单成为唯一来源，
   `forward!`（18 个 `JSObject` 方法）与 `impl Trace for Object` 都从它展开。
   以前这两份 20-arm 表是分开手写的，新增变体要各改一次。
2. `object.rs` 新增 `delegate_js_object!`（放在 trait 定义之后、所有变体之前）：
   为内嵌 `OrdinaryObject` 的变体生成整套 15 个存储方法 +
   `kind`/`type_of`/`class_name`；四参数简写把 `typeof` 默认成 `"object"`，
   五参数供函数对象传 `"function"`。`FunctionObject` 的 83 行手写委托、
   旧的 `delegate_collection_object!`（91 行，WeakMap / WeakSet 使用）全部
   换成一行宏调用。

结果：新增一个普通变体只需「`for_each_variant!` 加一行 arm + 一行
`delegate_js_object!` + `ObjectKind` / 原型注册」；`forward!` 与 GC 分派不再
需要同步。两文件净删除 106 行重复代码（118 插入 / 224 删除）。

**验收（统一验证）**：三级 272 / 649 / 7 + 字节码 golden；全量
**17,481 / 20,504（85.26%）与基线逐位一致**，无逐套件回退，护栏 0/0/14、
跳过 8577 不变，墙钟 115.8s —— 正是纯重构应有的结果。头条未变，基线快照
无需刷新。

**片 2 未做**：其余纯委托变体（Date / RegExp / Promise / ArrayBuffer /
TypedArray / DataView / Proxy / Map / Set）仍是手写委托，可机械换成宏；
`ObjectKind` 降级、exotic 变体存储收编（Array / Global / NativeFunction）、
原型表 Realm 化留到片 3 / 4。

### 对象模型重构片 3 前半（2026-11-11）：6 个纯委托变体宏化

片 2 的 `delegate_js_object!` 现在覆盖剩余 6 个纯委托变体：
`DateObject` / `PromiseObject` / `ArrayBufferObject` / `DataViewObject` /
`MapObject` / `SetObject` —— 它们只有内部槽不同，`JSObject` 的 15 个方法
全是 `self.base.xxx()`。逐个 impl（59–82 行）换成一行宏调用，
`src/vm/object.rs` 净删 402 行（6 插入 / 408 删除），零运行时语义变化。

仍保留手写 impl 的 3 个 exotic 变体（宏覆盖不了）：`RegExpObject`
（`lastIndex` / `source` / `flags` 虚拟属性）、`TypedArrayObject`（整数索引
元素）、`ProxyObject`（动态 `type_of` + target 转发）。加上
`PrimitiveWrapper` 的 String 虚拟索引与 Array / Global / NativeFunction 的
自定义存储，「共享实现负责普通属性、exotic 只覆写必要钩子」的边界至此清楚。

**验收（统一验证）**：三级 272 / 649 / 7 + 字节码 golden；全量
**17,481 / 20,504（85.26%）与基线逐位一致**，无逐套件回退，护栏 0/0/14、
跳过 8577 不变，墙钟 115.4s。头条未变，基线快照无需刷新。

**下一步**：`ObjectKind` 降级（消除 `kind()` 与 `as_*` 的双重判定、约 117 处
`kind` 分支）；exotic 存储收编（Array / Global / NativeFunction）；原型表从
`thread_local` 改挂 Realm。

### Phase 5 — 宿主/模块（可选，15–25 人日）

- `Engine` / `Context` / `JsValue` 公共 API；原生函数注册；宿主对象；错误栈；
  模块加载器 trait；`module/mod.rs` 不再是 TODO。
- 若不做，README 删掉“embeddable”或明确“仅 CLI/库内嵌”。

### 里程碑与验收

| 里程碑 | 目标 |
|---|---|
| M0（Phase 0 完成） | S0 全清；基线可复现；strict 语义/全局对象探针通过；Date 可测；CI 绿 |
| M1（Phase 1 完成） | 对象/环境/分派重构落地；新增方法 ≤2 处；globalThis 真对象；无逐套件回退 |
| M2（Phase 2 完成） | 几何单写者；GC generation；字节码 v2；GC stress 全绿 |
| M3（Phase 3 完成） | 编译器 LOC 下降 ≥40%；span/CompileError 全覆盖；通过数不低于 M2 |
| M4（Phase 4 完成） | 口径 B ≥95%；Date/AB/DV/TAConstructors 入册且 ≥80% |

---

## 6. 接下来 10 个具体动作（按顺序）

已完成：1（boundary Drop 守卫，2026-11-11 收尾）、2（pending_captured 入根）、
3（guard 配置+预算 384MB）、4（`rbp <= rsp` 几何断言）、
5（运行时立即数/常量检查，2026-11-10 收尾）、7（scope 表收口 + 基线刷新）、
8（strict 运行时收口）、9（字节码 dumper + 16 组 golden，删除 IR 克隆）、
6（删 `BIUJS_REG_VARS` 与变量寄存器分配，固定内存驻留语义）、
0.10（文档数字由 `doc-numbers.sh` 从快照生成）、0.9（CI：PR 门禁 + 周度全量）、
S0#4 第一档（`borrow` 族对已释放槽的访问 abort）、
**1A 两步**（`Index(u32)` + 单一索引判定；`Str(Atom)` 驻留 + 存储按 `PropertyKey` 建键）、
**1B 十一片**（…；`Shape` 结构与迁移表；`OrdinaryObject` 接入形状 + 字典模式降级；
第七片 `FunctionObject` 委托、第八片 `PrimitiveWrapperObject` 委托、
第九片内建原型 `ProtoObject` 委托、第十/十一片 `MapObject` / `SetObject` 委托）。

### 6.1 对账（2026-11-10，按代码逐项核对）

文档此前把 0.2 / 0.4 / 0.5 记为「已完成」，实际只完成了一部分：

| 项 | 真实状态 | 证据 |
|---|---|---|
| 0.2 | **2026-11-11 已完成**：`with_invoke_boundary` 内定义 Drop 型 `BoundaryGuard`（弹出 + 嵌套深度互校），`drive_bytecode_frame` 拆成 RAII wrapper + inner，手动 push/pop 全部移除；3 条泄漏回归 | `vm/mod.rs`、`vm/exec.rs` |
| 0.4 | 部分：`step` 里只有 `debug_assert!(rbp <= rsp)`；`rbp == frames.last().base` 在生成器挂起路径尚不成立，frame 深度断言未加 | `vm/exec.rs` `step` |
| 0.5 | **本轮收尾**：`try_immd` / `immd` / `constant_at` 早已有，但 `RelPc`/`AbsPc` 上 6 处运行期 `as_immd()` 仍会 abort；现补 `try_immd` + `pc_immd` + 坏模块回归 | `bytecode.rs`、`vm/mod.rs`、`vm/exec.rs` |
| 0.1 / 0.3 / 0.6–0.10 | 已完成 | 见 §5 表格 |

S0 仍挂着三项：`Gc` 句柄代次（被 `Value == 16 B` 硬阻塞；文档原写的前置
「1A 把 `Value::String` 换成 `Atom`」**并未发生**，`value.rs` 仍是 `Rc<String>`）、
`invoke_boundaries` 收集闸门（原生回调完整入根前不能撤）、`rsp` 的 7 个写者
（2A 几何单一所有者的前置）。**M0 因此不能算全清**，M1 还差 1B 剩余 + 1C/1D；
M2/M3/M4 未开始。Phase 3 只有 3A（删 `BIUJS_REG_VARS`）落地，IR/SSA 仍在；
1C（`MAX_DENSE_ELEMENTS`、三处数组存储、`Value::Function`）、1D（名字字符串分派）、
Phase 2（`FunctionCode` / `OpInfo` / 单层 match）都还是原样。

剩余（按顺序）—— Phase 0 主体已收口（0.2 已收尾），1A 也收口，下面是：
    0.4 收尾（`rbp == frames.last().base` 的完整断言，前置是 Phase 2 修生成器挂起几何）→
    S0#4 第二档（GC 句柄代次 —— 先解决 `Value::String` 的表示才有空间）→
    1B 剩余（NativeFunction 换到槽位 —— `new` 无堆参数，约 56 处调用点；
            Array 与 1C 耦合；之后 Realm / EnvRecord）→
    1C（数组存储 + 删 `Value::Function`）→ 1D（NativeId + HostOps）→
    Phase 2（帧几何/GC/字节码 v2）→ Phase 3（编译器简化）。

---

## 7. 明确不做 / 待决策

- **不做 NaN-boxing**：纯性能项，且依赖对象模型稳定；`Value` 已 16B。
- **不做可移动/分代 GC**：先做 generation + 完整 rooting；写屏障在当前的直接字段写下
  无处可挂。移动收集器只有在内存护栏仍不达标时才评估。
- **不并行跑 test262**：按套件位置切片不均，实测更慢且护栏分类漂移；要加速先按用例数切。
- **不把 `vm/mod.rs` 继续拆文件当目标**：拆文件是 cosmetic；先拆职责（frame/geometry/
  dispatch/ops）。
- **不追 out-of-scope 特性的 100%**：BigInt、async-iteration、class 私有字段、
  Temporal、Intl、SharedArrayBuffer 等明确排除；不要再把已实现但范围外的特性
  悄悄放进分母。
- **待决策**：modules 是否属于目标；RegExp 是否换 `regress`/自研引擎；
  `class-fields-public`/`object-rest` 等已实现扩展是否保留在 extended 指标里。

---

## 附录 A：复现命令

```sh
# 三级
cargo test --release --lib
cargo test --release --test features
cargo test --release --test guards

# 全量（脚本会分 4 片、加 ulimit）
./scripts/phase2-status.sh

# 关掉内存护栏，验证那 5 条 Array 不是语义回退
TEST262_SUITES=built-ins/Array TEST262_MEMORY_MB=0 \
  cargo test --release --test test262_runner -- --nocapture

# 失败明细（归类用）
TEST262_FAILURES=99999 cargo test --release --test test262_runner -- --nocapture > /tmp/full.txt
python3 scripts/kpi-noise.py /tmp/full.txt
```

## 附录 B：关键证据索引

| 主题 | 文件:行 |
|---|---|
| 默认内存驻留 | `src/compiler/codegen.rs:67`、`:1044-1061` |
| 编译主流程/重定位 | `src/compiler/mod.rs:35-204` |
| 无 span / 无 CompileError | `src/compiler/semantic.rs:92-95`；`src/compiler/lowering/mod.rs`（0 处） |
| 属性键分配 | `src/vm/property.rs:11-13` |
| 属性表/描述符克隆 | `src/vm/object.rs:155-159`、`:460`、`:1896`、`:3376` |
| 数组三处存储 | `src/vm/object.rs:576-592`、`:675-681`、`:965-969` |
| Object 枚举/forward | `src/vm/gc.rs:703-724`、`:728+` |
| `Value::Function` | `src/vm/value.rs:45-56`、`:153-171`、`:362-368` |
| 名字分派/arity/前缀 | `src/vm/builtins_dispatch.rs:28-505`；`src/builtins/mod.rs:1998-2098`；`src/vm/object.rs:1655-1699` |
| 全局对象 | `src/vm/object.rs:2437`；`src/vm/mod.rs:5536`、`:5850-5895` |
| 边界泄漏 | `src/vm/exec.rs:887`、`:895`、`:939`、`:965` |
| Trace 漏项 | `src/vm/trace.rs:77-98`；`src/vm/mod.rs:5578` |
| `rsp` 三写者 | `src/vm/mod.rs:3069`、`:3108`、`:4626`；`src/vm/exec.rs:983` |
| GC 槽/ABA | `src/vm/gc.rs:59-72`、`:328-351` |
| 指令分派 | `src/vm/exec.rs:1563-1602`；`src/bytecode.rs:601+` |
| 测试门控 | `tests/test262_runner.rs:376-431`、`:440-455`、`:473-500`、`:790-924` |
| 宿主/模块空壳 | `src/host/mod.rs`、`src/module/mod.rs` |
