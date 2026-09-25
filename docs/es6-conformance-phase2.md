# ES6 一致性第二阶段计划书（M5 → M8）

> **起算点**：2026-09-25。**前一份**`docs/es6-conformance-plan.md` 覆盖 M0 → M4，其 M2' / M3 / M4 的验收条件均已达标（核对见该文档 §1.3.1），自此作为**工作日志与历史基线**保留。
> **本文件是第二阶段的任务书**，自包含：从这里出发不需要读旧文档就能排期与施工。旧文档只在需要追溯"某个结论是怎么来的"时查阅。
> **本阶段的起点不是空档**：当前已执行的 17477 条里仍有 3622 条 ES6 范围内的失败（§1.2），那是 M2/M3/M4 的残余，构成 M7。

---

## 1. 起点基线（2026-09-25 实测）

| 指标 | 数值 | 说明 |
|------|------|------|
| test262 执行 | 17477 | runner 实际跑的数 |
| 通过 | **13232** | 主指标（B1 后） |
| 失败 | 4245 | 其中约 3600 条由 ES6 范围内特性驱动，约 640 条涉及范围外特性 |
| 跳过 | 8109 | 全部为范围外或 G3 排除项 |
| 通过率 | 75.71% | 参考值 |
| 单元测试 | 190 全绿 | |
| feature 集成测试 | 23 个文件 / 420 用例全绿 | |
| 全量耗时 | 2m48s | B1 的协议化 spread 带来的上升，见 §6.1 |
| runner 覆盖面 | 25586 / 53568 个测试文件（**47%**） | 见 §2.3 |

**关键事实（本轮实测）**：把 M5/M6 的 11 个套件加进 runner 后，**通过数一条不变（13211）**，执行数只从 17477 涨到 19029，多出来的 1552 条**全是失败**，通过率掉到 69.43%。原因是那些套件里绝大多数测试仍被 `UNSUPPORTED_FEATURES` 挡着被跳过 —— 光"入册"不解决问题，必须**入册与解除门控联动**（§2.2）。

---

## 2. 度量协议（本阶段首先固定，否则进度不可见）

### 2.1 三个并列数字

每次全量回归报告必须同时给出：

| 数字 | 含义 | 用途 |
|------|------|------|
| **计划内通过数 / 计划内执行数** | 主指标 | §6.1 的"不低于上一提交"用绝对数 |
| 目标套件通过率 | 质量指标 | 判断"这批做完没有" |
| 范围外排除数 | 必须逐条可解释 | §6.2 的治理要求 |

**"计划内测试面"的定义**：`runner::SUITES` 列出的套件 ∪ §3 判定为"在范围"的全部 built-ins 目录。任何**在范围但未被 runner 枚举**的测试，都要登记在 §2.3 的承诺面清单里 —— 不进 CI 数字可以，但**不允许不知情**。

### 2.2 入册与解锁联动规则（本阶段纪律）

一个在范围但未实现的特性的完整交付 = **三件事同批完成**：

1. 实现该特性；
2. 从 `tests/test262_runner.rs` 的 `UNSUPPORTED_FEATURES` 移除其特性名 → 相关测试**从跳过转为执行**；
3. 若其套件不在 `SUITES` 里，同时加入 → 该套件**进入分母**。

只做第 1 步而不做 2/3，等于**白干**（进度不可见）；只做 2/3 而不做 1，等于往池里倒垃圾（§1 已实测过：+1552 条纯失败、通过数零增长）。

### 2.3 承诺面清单（在范围、当前不在 CI 数字内）

| 套件 | 文件数 | 其中被特性门控跳过 | 入册后直接执行 |
|------|--------|--------------------|----------------|
| `built-ins/TypedArray` | 1446 | 1339 | 107 |
| `built-ins/TypedArrayConstructors` | 738 | 736 | 2 |
| `built-ins/Promise` | 677 | 494 | 183 |
| `built-ins/DataView` | 561 | 127 | 434 |
| `built-ins/Set` | 383 | 26 | 357 |
| `built-ins/Proxy` | 311 | 309 | 2 |
| `built-ins/ArrayBuffer` | 221 | 36 | 185 |
| `built-ins/Map` | 204 | 56 | 148 |
| `built-ins/Reflect` | 153 | 153 | 0 |
| `built-ins/WeakMap` | 141 | 69 | 72 |
| `built-ins/WeakSet` | 85 | 23 | 62 |
| **合计** | **4920** | 3370 | 1550 |
| `built-ins/Date` | 594 | —（不在 `SUITES`） | — |

runner 未枚举的其余部分（`language/expressions` 11095、`language/statements` 9337 等）绝大多数是**已在 SUITES 中按子目录覆盖**的父目录，还有 `built-ins/Temporal` 4603、`built-ins/RegExp` 1879、`built-ins/Iterator` 514、`language/module-code` 755 属范围外。**M8-V1 负责把这份清单补全并写明每一条的理由。**

---

## 3. 范围判定（本阶段）

沿用第一阶段的 §1.1 三条硬约束（strict-only / ES6 特性集 / 无 eval + with），本阶段新增两条判定：

| 项 | 判定 | 理由 |
|----|------|------|
| `Date`（594） | **待决** → M8-V2 | 第一阶段 §1.2 写"在（ES5 但属必备）"，实际 0 开工也 0 度量。本阶段**必须**给出"实现"或"降级"的结论，不允许继续悬空 |
| RegExp 驱动的失败（`String/prototype` 的 `replace`/`match`/`search`/`split` 合计 145，`Array` 侧若干） | **不计入 M7 的 KPI** | 引擎无正则实现（第一阶段已判范围外）。但这批测试**仍在执行并计入失败**，需要 M8-V3 在报告里单独扣除或补进跳过表 —— 否则 M7 的"清零"目标永远达不到 |
| `Promise` | 在范围 | 需要先定微任务调度点（§5 M5-C3） |
| `Proxy` / `Reflect` | 在范围 | 依赖的 `[[Get]]`/`[[Set]]`/`[[DefineOwnProperty]]` 已成型，可以开工 |

---

## 4. 阶段目标与总体验收

**总目标**：把 §2.3 承诺面清单里的 ES6 特性补齐，并清掉 §1 的 3622 条范围内失败。

| # | 验收项 | 当前 | 目标 |
|---|--------|------|------|
| A1 | M7 范围内失败 | 3622 | ≤ 800（RegExp 驱动部分按 §3 扣除后计入） |
| A2 | M7 的六个任务包（§5） | 0/6 | 6/6 有交付记录且各自验收达标 |
| A3 | `Map` / `Set` / `WeakMap` / `WeakSet` | 0% | 各自套件通过率 ≥ 80%，且已入册解锁 |
| A4 | `Proxy` / `Reflect` | 0% | 各自套件通过率 ≥ 50%（依赖内部方法，允许更低） |
| A5 | `ArrayBuffer` / `DataView` / `TypedArray` | 0% | 各自套件通过率 ≥ 50%；detach/resizable 允许登记偏差 |
| A6 | `Promise` | 0% | 套件通过率 ≥ 50%（含微任务调度点落地） |
| A7 | `Date` | 悬空 | 结论落地（实现则 ≥ 50%，降级则写进范围外栏） |
| A8 | 计划内通过数 | 13232 | ≥ 20000（本阶段结束时） |
| A9 | 单元 / feature 测试 | 190 / 417 | 全绿；每个任务包新增 ≥ 5 条断言的 feature 用例 |
| A10 | 文档一致性 | §1.2 现状栏仍有过时项 | 与 `es6-feature-support.md`、README 三者逐项对齐 |

---

## 5. 任务分解

### M5 集合与反射（ES6 承诺内，当前 0%）

| 任务 | 内容 | 规模 | 依赖 | 验收 |
|------|------|------|------|------|
| **C1 `Map` / `Set`** | 插入序、`size`、`get`/`set`/`has`/`delete`/`clear`、`forEach`、`keys`/`values`/`entries`、`Symbol.iterator`、`-0` 归一化、`SameValueZero` | 204 + 383 | 迭代器基建已就绪 | 两套件入册解锁后通过率 ≥ 80%；`built-ins/Set/prototype/forEach` 与 `Map/prototype/forEach` 的插入序用例全过 |
| **C2 `WeakMap` / `WeakSet`** | 只需 `get`/`set`/`has`/`delete`；`Rc` 下弱引用退化为强引用，偏差记入 §8 | 141 + 85 | 无 | 通过率 ≥ 80%；偏差写入文档 |
| **C4 `Proxy` / `Reflect`** | 陷阱分发（`get`/`set`/`has`/`deleteProperty`/`ownKeys`/`getOwnPropertyDescriptor`/`defineProperty`/`apply`/`construct`/`getPrototypeOf`/`isExtensible`…）+ `Reflect` 13 个静态方法 | 311 + 153 | `[[Get]]`/`[[Set]]`/`[[DefineOwnProperty]]`（M3 已交付） | 通过率 ≥ 50%；`Revocable`、`Proxy` 与内置方法交互的用例优先 |
| **C3 `Promise`** | 状态机、`then` 链、`resolve`/`reject`/`all`/`race`、微任务队列（**需要 VM 主循环后明确的 draining 点**） | 677 | 调度点需新设计 | 通过率 ≥ 50%；`then` 回调的时序用例（`Promise/…/then-callback-async`）必须过 |

### M6 TypedArray / ArrayBuffer / DataView（ES6 承诺内，当前 0%）

| 任务 | 内容 | 规模 | 依赖 | 验收 |
|------|------|------|------|------|
| **T1 `ArrayBuffer` + `DataView`** | 底层字节缓冲（`Vec<u8>`）、`DataView` 的 8 种读写 × 端序 | 221 + 561 | 无 | 通过率 ≥ 50% |
| **T2 TypedArray** | 11 种视图类型、`buffer`/`byteOffset`/`byteLength`/`length`、`set`/`subarray`/`slice`、构造器重载（`from`/`of`/iterable/arraylike/buffer+offset） | 1446 + 738 | T1 | 通过率 ≥ 50% |
| **T3 detach / resizable** | `resizable-arraybuffer` 与 detach 语义；当前以"缺 `Uint8Array`"形式在失败池里出现 88 条 | — | T2 | 允许登记偏差，但不得让池里出现"未实现"型失败 |

### M7 协议与长尾一致性（当前范围内失败 3622 条的主干）

| 任务 | 内容 | 规模 | 来源 |
|------|------|------|------|
| **P1 Array 回调方法的泛型/访问器路径** | `reduceRight` 79、`lastIndexOf` 60、`reduce` 54、`indexOf` 50、`map`/`some`/`every`/`filter`/`forEach` 各 ≈40、`sort` 33。根因：builtin 层直接读密集存储，读不到原型链上的访问器、也绕过 `[[Set]]` | ≈ 450 | 旧文档 §2.3 |
| **P2 class 求值顺序与 own-property 语义** | `Expected SameValue` 345、`X should be an own property` 79、`Cannot read undefined` 66（求值顺序、`super` 相关、静态块前的初始化） | 705 | §2.1ac 新暴露 |
| **P3 迭代协议剩余** | `for-of` 321：`IteratorClose` 计数、`iterator-next-result-type`、**spread 改走 `GetIterator`**（当前 `ArrayPushSpread` 用 `array_like_elements` 快照，完全不过协议） | 321 | 旧文档 §2.3 |
| **P4 Object 描述符长尾** | `defineProperty` 131 + `defineProperties` 86 | 217 | — |
| **P5 数据模型三债** | ① 脚本完成值不可靠（`vm.run` 返回中间表达式）；② 闭包捕获对象时快照点错误（`box.i += 1` 报 `Cannot create property 'i' on number`）；③ `ArrayPushSpread` 与 ① 同源 | — | 旧文档 §2.3（本轮新登记） |
| **P6 生成器剩余** | `statements/generators` 87 + `expressions/generators` 96 + `expressions/yield` 20；含 `yield*` 无 `throw` 时的 `IteratorClose`、`finally { return x }` 的取值、生成器作构造器 | 203 | 旧文档 §2.3 |

### M8 度量与文档收口

| 任务 | 内容 | 验收 |
|------|------|------|
| **V1 覆盖率口径** | 补全 §2.3 承诺面清单，逐条写明"在范围但未入 CI"或"范围外"的理由 | 清单与 `SUITES` 一致，无未解释项 |
| **V2 `Date` 决策** | 实现（594）或降级 | 结论写进 §3，不允许悬空 |
| **V3 RegExp 驱动失败归类** | 145 + 若干条：在报告里单列或补进跳过表 | M7 的 A1 目标可被真实测量 |
| **V4 三方文档对齐** | §1.2 现状栏 / `es6-feature-support.md` / README | 逐项一致 |

---

## 6. 施工顺序（批次表）

**排序原则**：先做**独立且便宜**的（无跨层改动、单点收益确定），再做**贵且相互牵扯**的；`Promise` 因需要新机制放最后。

| 批次 | 内容 | 规模 | 说明 |
|------|------|------|------|
| **B0** ✅ | 度量铺底：§2.3 清单入文档；把"入册+解锁联动"写进 runner 的注释与 §6.2 | — | **已完成**，见 §6.1 |
| **B1** ✅ | M7-P3 的 spread 走 `GetIterator`（含 `ArrayPushSpread` 重写） | +21 | **已完成**，见 §6.1；顺带修好 `@@iterator === values` 与 `values/keys/entries` 的接收者校验 |
| **B2** | M5-C1 `Map`/`Set`（含入册解锁） | 587 | ES6 承诺内最早能整体交付、不依赖新机制 |
| **B3** | M5-C2 `WeakMap`/`WeakSet` | 226 | 与 B2 同构，可顺带 |
| **B4** | M8-V3 RegExp 失败归类 + M8-V2 `Date` 决策 | — | 把 KPI 的杂音清掉，再动大件 |
| **B5** | M7-P4 Object 描述符长尾 | 217 | 单点确定 |
| **B6** | M7-P6 生成器剩余 + M7-P5 数据模型三债 | 203 + — | 债不还，后面的用例会持续被它误导 |
| **B7** | M7-P1 Array 回调泛型/访问器上移 VM | ≈450 | **最贵的一批**，跨 builtin/VM 两层，建议再拆 2–3 个小批 |
| **B8** | M7-P2 class 求值顺序与 own-property | 705 | 单批第二贵 |
| **B9** | M5-C4 `Proxy`/`Reflect`（含入册解锁） | 464 | 依赖已就绪 |
| **B10** | M6-T1 `ArrayBuffer` + `DataView` | 782 | |
| **B11** | M6-T2 `TypedArray` + `TypedArrayConstructors` | 2184 | 同质、可批量 |
| **B12** | M5-C3 `Promise`（先定微任务调度点） | 677 | 需新机制，放最后 |
| **B13** | M8-V1/V4 收口 | — | 覆盖率清单 + 三方文档对齐 |

**每批的固定动作**：三级验证（`tests/features/` 新断言 → 目标套件定向跑 → 带内存上限的全量回归）→ 逐套件核对**零回退** → 更新本文档的批次状态 → 提交。

### 6.1 批次记录

#### B0 度量铺底 —— 已完成（2026-09-25）

`tests/test262_runner.rs` 里原来的 `UNSUPPORTED_FEATURES` 把两种含义不同的跳过混在一起。已拆成两条：

| 常量 | 含义 | 处理 |
|------|------|------|
| `OUT_OF_SCOPE_FEATURES`（29 条） | 范围外或 G3 永久排除 | 预期长期跳过 |
| `IN_SCOPE_PENDING`（8 条：`Map` / `Promise` / `Proxy` / `Reflect` / `Set` / `TypedArray` / `WeakMap` / `WeakSet`） | **在范围但未实现** | §2.2 的纪律写在常量注释里：实现该特性时**必须同批**从这里删掉，必要时同时把套件加进 `SUITES` |

`should_skip` 改调 `is_unsupported()`，两个表都查。**行为中性已验证**：拆分后跳过数仍是 8109，通过数不变。

#### B1 spread 走 `GetIterator` —— 已完成（2026-09-25）

**起点**：通过 13211 / 17477（75.59%）。

`Opcode::ArrayPushSpread`（`[...src]` 与 `f(...src)` 共用）原来用 `array_like_elements` —— 那是 `length` + 整数键的**快照**，既不走迭代协议也不执行访问器。改成 `make_iterator` + 循环 `iterator_next` 到结束（ES 13.2.5.5 / 13.3.8.1）。不需要 `IteratorClose`：源被抽干，`next()` 抛出时按规范直接传播。

**这一批的实际范围比计划大**：把协议接上之后，三处被快照掩盖的缺陷立刻暴露出来，都在同一批修掉：

1. **内置工厂对普通对象无限递归**。`o[Symbol.iterator] = Array.prototype[Symbol.iterator]` 会让内置工厂调回 `make_iterator` 再调回工厂 —— Rust 栈溢出、进程 SIGABRT（正是 §7.1 记过的那类崩溃）。根因是内置工厂被写成"回到 `make_iterator(this)`"，而普通对象既不匹配数组分支也不匹配字符串分支。改为按 `ToLength(Get(O,"length"))` + 索引的**泛型类数组**语义 —— 这本来就是 `Array.prototype.values` 的定义（ES 23.1.3.30）。
2. **`Array.prototype[Symbol.iterator]` 不是 `Array.prototype.values`**。规范要求两者是**同一个函数对象**；原来是给数组和字符串各注册一个共享的 `__iterator_factory__`。结果是 `@@iterator === values` 为假，且 `Array.prototype[Symbol.iterator].call(o)` 不可用（工厂假定接收者已带该方法）。改为数组的 `@@iterator` 直接指向已注册的 `values`；字符串保留自己的函数（它迭代码点，不是索引）。`make_iterator` 相应地把 `__proto_method__values` 也认作内置工厂。
3. **`values` / `keys` / `entries` 对非类数组接收者会抛错**。它们现在先做 `require_object_coercible(this)`（`values.call(null)` 是 TypeError），再按 `ToLength(Get(O,"length"))` 取值，没有 `length` 的接收者迭代为空（`values.call({})` 是空迭代器，不是错误）。

**效果**：

| 指标 | 起点 | 本批后 |
|------|------|--------|
| 全量通过 | 13211（75.59%） | **13232**（75.71%） |
| 失败 | 4266 | **4245** |
| `language/expressions/call` | 44 | **54** |
| `language/expressions/new` | 25 | **35** |
| `built-ins/Array` | 1946 | **1947** |

**+21，逐套件零回退**（含一次用 `git stash` 做的 Array 套件前后对比：三条新失败来自 `values/keys/entries` 的接收者校验被我改宽，补上 `RequireObjectCoercible` 后消失；同时修好 `Array/prototype/Symbol.iterator.js`）。单元 190 全绿；features 417 → 420（新增三组：spread 走协议、替换工厂被复用、内置工厂是泛型的）。

**耗时**：全量 2m05s → **2m48s**。协议化 spread 每次多一次 `@@iterator` 查询与逐元素 `next`（原生迭代器路径无 `invoke`，但不为零）。仍在 §7 的 2× 护栏内，但**下一批要盯着这个数**：若继续上升，需要为 `[...真数组]` 加一条"解析到内置工厂时直接取快照"的快速路径（须保证访问器语义与协议一致，即与 M7-P1 一起做）。

**残留**（转入 M7-P1，不在本批范围）：

- `[...a]` 中数组元素的**访问器不执行**（`array_like_elements` 对真数组读密集存储）。实测 `var a=[1,2]; Object.defineProperty(a,0,{get:()=>99}); [...a]` 得到 `[,2]`，应为 `[99,2]`。与 `built-ins/Array/prototype/includes/values-are-not-cached.js`、`.../values/iteration-mutable.js` 同源，都是"builtin 层读不到访问器/不按 `[[Get]]` 取值"。

---

## 7. 验证与回归策略（本阶段增补）

沿用第一阶段的 §6（三级验证、跳过表治理、失败原因聚合、性能护栏、单元测试、内存护栏），本阶段增补三条：

1. **零回退带逐套件核对**。全量通过数不低于上一提交是必要不充分条件 —— 新语义可能同时修好 A 与弄坏 B。每次提交前用脚本对比逐套件通过数，出现下降必须解释或回退。
2. **范围外杂音单列**。报告里 RegExp / ES2022 公开字段等**不可能通过**的失败要单独成列，否则 M7 的"清零"目标不可测（M8-V3）。
3. **崩溃优先于失败**。引擎里的无界递归/栈溢出会以 SIGABRT 打断整轮回归且不留线索（已发生过一次）。`BIUJS_TEST262_TRACE=1` 逐条打印测试路径是标准排查手段；任何新批次的第一次全量回归都要留意进程是否走完全程。

---

## 8. 风险登记

| 风险 | 等级 | 应对 |
|------|------|------|
| **P1（Array 泛型/访问器）是跨层改动**，builtin 层看不到原型链与 `[[Set]]`，必须上移 VM | 高 | 拆小批；每小批只动一个方法族；先补 feature 用例锁住现有行为 |
| **`Promise` 需要新的调度点**，是 VM 主循环之外的机制 | 高 | 放在最后；先写一个最小的微任务队列 + `then` 时序用例作为探针 |
| **`Proxy` 会放大既有语义缺口**（任何内部方法不完整都会在陷阱交互里暴露） | 中 | 先做 `Reflect`（它是"直接暴露内部方法"，失败即定位），再做 `Proxy` |
| **`TypedArray` 同质但量大（2184）** | 中 | 用生成式 feature 用例覆盖 11 个视图 × 操作矩阵 |
| **度量口径再次漂移** | 中 | §2.1 的三个数字并列写进报告；范围判定变更必须同时改 §3 与 runner |
| **旧债被误当新问题重复排查** | 中 | §5 各任务的"来源"列直接指向旧文档条目；本阶段所有新结论只写进本文件 |
| `Rc` 下 `WeakMap`/`WeakSet` 不是真弱引用 | 低 | 明确登记偏差，不假装支持 |

---

## 9. 文档维护

- **本文件是第二阶段的唯一计划**：批次状态、验收数字、范围判定变更都写在这里；不再往第一阶段文档追加计划性内容（那份只作为工作日志冻结）。
- 每完成一批更新 §6 表格的状态列与 §1 的基线数字。
- 一个任务包交付时，同时更新 `docs/es6-feature-support.md` 对应条目。
