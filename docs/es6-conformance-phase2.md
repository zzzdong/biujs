# ES6 一致性第二阶段计划书（M5 → M8）

> **起算点**：2026-09-25。**前一份**`docs/es6-conformance-plan.md` 覆盖 M0 → M4，其 M2' / M3 / M4 的验收条件均已达标（核对见该文档 §1.3.1），自此作为**工作日志与历史基线**保留。
> **本文件是第二阶段的任务书**，自包含：从这里出发不需要读旧文档就能排期与施工。旧文档只在需要追溯"某个结论是怎么来的"时查阅。
> **本阶段的起点不是空档**：当前已执行的 17477 条里仍有 3622 条 ES6 范围内的失败（§1.2），那是 M2/M3/M4 的残余，构成 M7。

---

## 1. 起点基线（2026-09-25 实测）

| 指标 | 数值 | 说明 |
|------|------|------|
| test262 执行 | 18024 | runner 实际跑的数（B2a 起含 `built-ins/Map`，B2b 起含 `built-ins/Set`） |
| 通过 | **13943** | 主指标（B2b 后） |
| 失败 | 4081 | 其中约 3400 条由 ES6 范围内特性驱动，约 640 条涉及范围外特性 |
| 跳过 | 8149 | 范围外 / G3 排除项 + 已入册套件自己的 feature 门控（`Map` 30 条 + `Set` 26 条） |
| 通过率 | 77.36% | 参考值 |
| 单元测试 | 190 全绿 | |
| feature 集成测试 | 32 个文件 / 442 用例全绿 | |
| 全量耗时 | 2m50s | B1 的协议化 spread 带来的上升，见 §6.1 |
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
| A8 | 计划内通过数 | 13264 | ≥ 20000（本阶段结束时） |
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
| **B2a** ✅ | M5-C1 前半：`Map`（含入册解锁） | 204（入册后执行 162） | **已完成**，见 §6.1；套件通过率 95.68%，A3 的 ≥80% 达标 |
| **B2b** ✅ | M5-C1 后半：`Set`（含 `set-methods` 七个算子） | 383 | **已完成**，见 §6.1；套件通过率 94.40%，A3 的 ≥80% 达标 |
| **B3** | M5-C2 `WeakMap`/`WeakSet` | 226 | 与 B2 同构，可顺带 |
| **B4** | M8-V3 RegExp 失败归类 + M8-V2 `Date` 决策 | — | 把 KPI 的杂音清掉，再动大件 |
| **B5a** ✅ | M7-P4 前半：数组 `length` 的错误种类（应为 RangeError） | +32 | **已完成**，见 §6.1 |
| **B5b** ✅ | M7-P4 后半：`defineProperty` 的 TypeError 缺口、六个完整性方法、数组 `length` 的下取整 | **+118**（净） | **已完成**，见 §6.1；含一条**已解释的** `built-ins/Array` −11（strict-only 所致，见 §6.1 末） |
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

#### B5a 数组 `length` 的错误种类 —— 已完成（2026-09-25）

**起点**：通过 13232 / 17477（75.71%）。

**根因**：`[[DefineOwnProperty]]` / `[[Set]]` 的失败只能通过一个 `String` 通道上报，调用点一律 `.map_err(RuntimeError::TypeError)` —— **错误种类在通道里被抹平**。而 `ArraySetLength`（ES 10.4.2.4）对非法的 `length` 要求 **RangeError**，`ArrayObject` 的两个分支（`define_property` 与 `property_set`）里 `validate_array_length(n)` 本来返回的正是 `RuntimeError::RangeError`，却被 `.map_err(|_| "Invalid array length".to_string())` 丢掉了。

**改动**：给 `String` 通道加一个只承载这一种差异的前缀。

1. `RuntimeError::into_property_error()`：把 `RangeError` 渲染成 `"RangeError: <msg>"`，其余照旧（`PROPERTY_ERROR_RANGE_PREFIX`）。
2. `RuntimeError::from_property_error()`：把带前缀的消息还原成 `RangeError`，其余仍是 `TypeError`。
3. `ArrayObject` 的 `define_property` / `property_set` 两处对 `length` 改用 `into_property_error`（不再丢类型）。
4. 所有**写入路径**的 `String → RuntimeError` 转换点改用 `from_property_error`：`Object.defineProperty` / `defineProperties` 的两处、`set_member` 的两处 `property_set`、以及 `define_property` 的 `length` 用法。

只改写入路径，读路径（`find_descriptor` / `internal_get` / `internal_has_property`）保持原样 —— 它们不可能产生带前缀的消息。

**效果**：**+32**（13232 → 13264），通过率 75.71% → 75.89%，失败 4245 → 4213。`built-ins/Object` 2611 → **2639**（`defineProperties/15.2.3.7-6-a-*` 一族的 28 条），`built-ins/Array` 1947 → **1951**（`length/15.4.5.1-3.d-*`、`define-own-prop-length-error`、`splice/create-non-array-invalid-len`）。

**逐套件零回退**；单元 190 全绿；features 420 → 421（新增 `array_length_errors_are_range_errors`，覆盖 8 种非法长度写入的 RangeError 与 3 种仍应为 TypeError 的写入，加上长度合法增减）。

**残留**（M7-P4 的后半）：

- `*.length = X` 之外，**非可写数组索引的重定义没有抛错**：实测 `Object.defineProperty(a, 0, {value:1,writable:false}); Object.defineProperty(a, 0, {value:2})` 静默通过，应为 TypeError。属 `validate_property_redefinition` 在数组索引分支的缺口。
- P4 里剩下的 `Expected a TypeError to be thrown`（约 42 条）与 `TypeError: undefined is not a function`（26 条）尚未归族。

#### B5b Object 描述符长尾 —— 已完成（2026-09-25）

**起点**：通过 13264 / 17477（75.89%）；`built-ins/Object` 2639 / 3112；`built-ins/Array` 1951。

**先订正一处错误的"已定位根因"**。B5a 的残留里写着"`Object.defineProperty(a, 0, {value:1, writable:false})` 之后再 `{value:2}` 静默通过，应为 TypeError"。这条**是错的**：拿 node 对照，那两句本来就该静默通过 —— 描述符没提 `configurable` 时它继承当前值（数组元素默认 `true`），所以第二步合法。本批没有动它（动就会引入回退）。真正的根因是在把 `built-ins/Object` 的 473 条失败按消息聚合之后才浮出来的，共 8 条，都在下面。

**根因**

| # | 根因 | 证据（失败规模） |
|---|------|------------------|
| R1 | 访问器槽里装着 `undefined` 却被当成可调用：`{get: undefined}` 装的 getter 是 `Some(Undefined)`，读属性时 `invoke` 一个 `undefined` | 26 条 `TypeError: undefined is not a function`（`defineProperty/15.2.3.6-4-4xx` 一族） |
| R2 | 只改 `enumerable`/`configurable` 的部分描述符把已有访问器**重写成数据属性**；`{}` 也没有按"字段全缺 → 直接 true"早退 | 20 条 `Expected obj[x] to equal data, actually undefined` / `to be writable, but was not` |
| R3 | `ToPropertyDescriptor` 没拒绝"访问器字段 + `value`/`writable`"的混合描述符 | 12 条 `Expected a TypeError to be thrown`（`15.2.3.6-3-*`、`15.2.3.7-5-b-*`） |
| R4 | `ArraySetLength` 缺"删到非可配置索引就停下"的规则（ES 10.4.2.4 step 12.c：先把 `length` 夹到该索引之上，再报失败） | `defineProperty/15.2.3.6-4-11x/16x/17x` 与 `defineProperties/15.2.3.7-6-a-16x` 约 20 条 |
| R5 | `length` 只读时仍能往 `length` 之上新增索引（ES 10.4.2.1 step 4.b 走 `[[DefineOwnProperty]]`） | `15.2.3.6-4-188/189` 等 4 条 |
| R6 | `Object.preventExtensions` / `seal` / `freeze` / `isExtensible` / `isSealed` / `isFrozen` 六个动词在 `register_object_statics` 里还是桩，且 `FunctionObject` / `PrimitiveWrapperObject` / `NativeFunctionObject` 的 `[[Freeze]]`/`[[Seal]]` 是空实现，`is_frozen`/`is_sealed` 返回的是一个布尔字段而不是"现算" | `seal` 34 / `freeze` 9 / `preventExtensions` 11 / `isExtensible` 8 / `isFrozen` 3 一族 |
| R7 | `Object.getPrototypeOf` 不做 `ToObject`（`null` 应当 TypeError，原始值应当回答其包装对象的原型） | `getPrototypeOf/*` 13 条中的 8 条 |
| R8 | `Object.setPrototypeOf` 既不检查目标是否可扩展，也不断环（`Object.setPrototypeOf(o, o)` 会留下循环原型链） | 3 条（`set-failure-non-extensible`、`prototype/__proto__/set-cycle`/`set-immutable`） |

**改动**

1. `vm/property.rs`：新增 `invoked_getter()` / `invoked_setter()`（把 `Some(Undefined)` 折成 `None`），所有发起调用的点改走它 —— `get_member`、`get_from_prototype`、`set_member`、`prototype::internal_set` 两处。R1。
2. `builtins/object.rs::apply_property_descriptor`：① 描述符六个字段全缺且属性已存在 → 直接 `Ok(true)`；② 已有访问器且描述符没提 `value`/`writable` → 走访问器分支（保留现有 get/set）；③ 补 `get`/`set` 必须"可调用或 undefined"的校验。R2、R3（后半）。
3. `vm/mod.rs::to_property_descriptor`：先扫一遍四个字段的存在性，混合即 TypeError。R3（前半）。
4. `vm/object.rs`：抽出 `ArrayObject::resize_length(new_len) -> bool`（顶层元素逐个删，遇到非可配置索引就把 length 夹到它上面并报失败），`define_property` 与 `property_set` 的 `length` 分支共用；两处索引分支补"`length` 只读时不得新增元素"。R4、R5。
5. `builtins/object.rs`：六个完整性动词有了真身（原始值参数不报错 —— `isExtensible` 答 `false`、`isFrozen`/`isSealed` 答 `true`、三个动词原样返回），`register_object_statics` 的桩改成转发同一份实现，两条派发路径不会再漂；`setPrototypeOf` 补"同值短路 / 非可扩展 / 环检测"；`getPrototypeOf` 改走 `ToObject`。R6、R7、R8。
6. `vm/object.rs`：新增 `freeze_property_table` / `seal_property_table` / `is_frozen_desc_set` / `is_sealed_desc_set` 四个共享实现 —— `[[Freeze]]`/`[[Seal]]` 作用于属性表，`is_frozen`/`is_sealed` 改为**按 `TestIntegrityLevel`（ES 6.1.7.4）现算**（不是布尔字段），因此对没有自有属性的对象会正确地继承"是否可扩展"的答案；`FunctionObject` / `NativeFunctionObject` 各加 `extensible` 字段，`PrimitiveWrapperObject` 的 `define_property` 补可扩展性与重定义校验，`ArrayObject::seal` 补密集元素（只去 `configurable`，保留可写）。R6。

**效果**

| 指标 | 起点 | R1+R2 后 | +R3…R5 后 | +R6…R8 后 |
|------|------|----------|-----------|-----------|
| 全量通过 | 13264 | 未测（套件定向跑） | 13354 | **13382** |
| `built-ins/Object` | 2639 | 2691 | 2738 | **2757** |
| `built-ins/Array` | 1951 | — | 1940 | **1940** |

净 **+118**（13264 → 13382，75.89% → 76.57%，失败 4213 → 4095）。逐套件口径：

```
提升  language/statements/class                        1106 -> 1110
提升  language/computed-property-names                 36 -> 40
提升  built-ins/Symbol                                 37 -> 39
提升  built-ins/Object                                 2639 -> 2757
提升  built-ins/Function                               190 -> 191
回退  built-ins/Array                                  1951 -> 1940
```

单元 190 全绿；features 421 → **429**（新增 8 组：`object_builtins.rs` 的 undefined 访问器槽、部分描述符保留访问器、混合描述符、`length` 下取整、只读 `length` 下的新增索引、完整性等级覆盖包装对象与函数、`setPrototypeOf` 拒绝冻结目标与环、`getPrototypeOf` 需要对象）。跳过数仍是 8109。

**关于 `built-ins/Array` 的 −11（已解释，选择不回退）**：这 11 条全是 `every`/`filter`/`forEach`/`map`/`some`/`reduce`/`reduceRight`/`indexOf`/`lastIndexOf` 的 `*-7-b-16` / `*-9-a-19` 变体，**flags 是 `noStrict`**，要点恰恰是"`arr.length = 2` 失败时**静默忽略**、被长度截掉的索引不该被删"。本引擎 strict-only（§3 硬约束 1），赋值失败只能抛 TypeError。同一段输入在 node 的 strict 模式下同样抛 TypeError（`len` 保持 3），所以新行为是对的，是这批测试的 sloppy 语义不可达。取舍与第一阶段 §2.1n（`set_member` 改为抛错）一致。**若将来要拿回它们，只能引入"assignment failure is silent"的 sloppy 通道，那是一个独立的、比 B5b 大得多的批次。**

另有 2 条 `built-ins/Object` 的 `isFrozen/15.2.3.12-3-1`、`isSealed/15.2.3.11-4-1` 由**假通过**转为真失败：它们写的是 `Object.isFrozen(this)`，`this` 是 `undefined`（缺全局对象），而 `Object.isFrozen(undefined)` 按规范是 `true`。修好全局对象会同时修好这两条。

**残留**（下一批的输入）

- **缺全局对象**：脚本顶层 `this` 是 `undefined`，全局变量活在 `State::globals` 这个 `HashMap` 里、不挂在对象上。`built-ins/Object` 现在有 46 条 `Cannot convert undefined or null to object` 直接或间接来自它，跨套件的量更大。它不是一个 5 行改动：要让 `this` 真的拥有属性，全局读写就得改走对象 + 原型链。**建议单独成批**。
- **数组字面量的省略元素不出 hole**：`[0,,2].hasOwnProperty("1")` 现在给 `true`（`lower_array` 的 `Elision` 分支退化成 push `undefined`）。要修得给 `Elision` 一条能在运行时标记 hole 的指令（五层改动），并把 `hasOwnProperty` / `in` / `Array.prototype` 各方法的口径一起对齐。
- **`ToPropertyKey` 走不通用户代码**：`Object.defineProperty(obj, {toString: function () { return 'abc'; }}, {})` 应当用 `"abc"` 作键；builtin 层做不了 `ToPrimitive`（要跑用户函数），得上移 VM。约 10 条（`defineProperty/15.2.3.6-2-4x`、`getOwnPropertyDescriptor/15.2.3.3-2-4x`）。
- **`ToNumber` 走不通用户代码**：`Object.defineProperty(a, 'length', {value: {toString: ...}})` 现在只会得到 `RangeError: Invalid array length`（因为对象是 NaN）。同一条病根的约 14 条。
- **严格模式下删除不可配置属性不抛 TypeError**：`obj[sym] = ...` 之后再 `delete obj[sym]` 应抛（`symbol-data-property-default-strict`）。
- **Annex B 遗留访问器**：`prototype/__proto__`（13）、`__defineGetter__`/`__defineSetter__`（各 10）、`__lookupGetter__`/`__lookupSetter__`（各 7）合计约 47 条，都要按 `[[Get]]`/`[[Set]]` 走接收者，必须在 VM 侧实现。
- **范围外杂音**（M8-V3 处理，本批不动）：`Object.fromEntries` 24（ES2019）、`Object.groupBy` 14（ES2024）、`Object.getOwnPropertyDescriptors` 13（ES2017）—— 它们已在执行、计入失败，但不在 ES6 目标集里。

#### B2a `Map` —— 已完成（2026-09-26）

**起点**：通过 13382 / 17477（76.57%）。

**交付**：`Map` 从 `IN_SCOPE_PENDING` 摘掉，`built-ins/Map` 加进 `SUITES`（§2.2 的"实现 + 解锁 + 入册"三件事同批）。入册后 `built-ins/Map` **155 / 162 = 95.68%**，A3 的"≥ 80%"达标。

**改动**（按 `architecture.md` §3.1 的清单，实际落点）

| 层 | 内容 |
|----|------|
| `vm/object.rs` | `MapObject`：`Vec<Option<(K,V)>>` 保插入序、`SameValueZero` 查键（`set` 时把 `-0` 归一成 `+0`）、`size`/`get`/`set`/`has`/`delete`/`clear`/`entry_from`；`impl JSObject`（含 `[[Freeze]]`/`[[Seal]]`/`[[DefineOwnProperty]]`，`class_name` = `"Map"`）。**删除用墓碑（`None`）而不是 `Vec::remove`** —— 迭代器是"活"的，按下标前进，一旦前面的条目被删掉、后面的元素左移，下标就会跳读 |
| `vm/iterator.rs` | `NativeIteratorState::Map { map, kind, idx }` + `MapIterKind{Key,Value,Entry}`；`native_next` 每一步重新借一次 map（回调可能在两步之间改它） |
| `builtins/map.rs`（新） | `get`/`set`/`has`/`delete`/`clear`、`size` 访问器、`Symbol.toStringTag`、`Symbol.species` 访问器；`as_map()` 提供"接收者必须是 `[[MapData]]`"的检查 |
| `builtins/mod.rs` | `map_prototype` 字段 + `register()` 里装配构造器/原型/`@@iterator === entries`；新增 `MAP_METHOD_PREFIX` 与 `set_map_method`（见下）；arity 表加 `Map`/`get`/`has`/`delete`/`set`/`getOrInsert*`/`Map.groupBy` |
| `vm/mod.rs` | 构造器 `map_construct`（`GetIterator` + 逐条 `Get(item,"0")`/`Get(item,"1")` + `Get(map,"set")` 后 `Call`，任一步骤 abrupt 都先 `IteratorClose`）、`map_for_each`、`map_iterator`、`Map.groupBy`、`size`/`species` 访问器派发 |
| `tests/` | `tests/features/map.rs`（7 组断言）+ runner 的解锁/入册 |

**三个必须记下来的坑**

1. **`keys`/`values`/`entries`/`forEach` 不能和 `Array.prototype` 同一套前缀**。它们名字一样，但接收者规则相反：`Array.prototype.keys.call(1)` 规范要求返回空迭代器，`Map.prototype.keys.call(1)` 必须是 TypeError。按名字派发区分不开，于是给 Map 的四个方法单独一个 `__map_method__` 前缀，并在 `NativeFunctionObject::install_metadata` / arity 表里同步剥掉。
2. **`get`/`set`/`has`/`delete`/`clear` 的派发必须带"接收者真是 Map"的守卫**。把它们直接加进 `call_prototype_method` 的名字表会劫持**任何**对象上的同名数据属性 —— 实测 `var obj = { get: function () { return this.value; } }; obj.get()` 立刻变成 `Map.prototype.get called on an incompatible receiver`（新的 feature 用例 `this_survives_nested_call` 抓到的）。
3. **`CallMethod` 的数组回调拦截跑在原生派发之前**。`m.forEach(cb)` 会先被 `try_array_callback_method` 当成"没有 `length` 的类数组"吞掉（回调一次都不执行）。修法是让 Map 接收者在那里提前 `return Ok(None)`；注意**只能对 Map 生效** —— 我第一版写成"不是类数组就退出"，把 `Array.prototype.forEach.call(true, cb)`（应静默迭代 0 次）等 20 条打挂了。

**顺带修掉的两条既有问题**

- `iterator_close` 新增"外层完成是 throw"的分支（ES 7.4.6 step 7）：此前无论外层是不是抛出，`return()` 返回非对象都会变成 TypeError，把真正的 `Test262Error` 盖掉。
- `NativeFunctionObject::define_property` / `property_delete` 补上可扩展性与重定义校验（`C.prototype` 是非可配置的，重新定义应当抛）。

**一次"解锁即倒垃圾"的事故（已修，必须记住）**

`is_unsupported` 的匹配是 `starts_with || contains`。`"Array.prototype.flatMap".contains("Map")` 为真 —— 也就是说 `Map` 一直"顺带"把 `Array.prototype.flatMap` 的 20 条用例锁着。把 `Map` 从 `IN_SCOPE_PENDING` 摘掉的瞬间，这 20 条（ES2019，未实现）涌进执行池，`built-ins/Array` 掉了 20 条通过。修法：在 `OUT_OF_SCOPE_FEATURES` 里**显式**登记 `Array.prototype.flatMap`（范围外 + 未实现，与"跳过表只剩范围外/G3"的定义一致），并在 `is_unsupported` 上写明这个匹配的锋利边缘。修完 `built-ins/Array` 回到 1941（比起点 1940 多 1），跳过数从 8109 变成 **8143**（Map 套件自带的 42 条门控用例进表，另有 8 条曾被 `Map` 顺带跳过、现在真正执行）。

**效果**

| 指标 | 起点 | 本批后 |
|------|------|--------|
| 全量通过 | 13382（76.57%） | **13584**（76.98%） |
| 失败 | 4095 | **4063** |
| 执行 / 跳过 | 17477 / 8109 | 17647 / 8143 |
| `built-ins/Map` | 未入册 | **155 / 162 = 95.68%** |
| `built-ins/Symbol` | 39 | **54** |
| `built-ins/Number` | 180 | **189** |
| `language/statements/for-of` | 376 | **381** |
| `language/statements/class` | 1110 | **1113** |
| `built-ins/NativeErrors` / `Error` | 62 / 36 | **68 / 38** |
| 其余 | — | `String`+1、`Object`+2、`Function`+1、`Array`+1、`types`+1、`expressions/class`+1 |

净 **+202**，**逐套件零回退**（`scripts/phase2-status.sh` 判定"无逐套件回退"）。单元 190 全绿；features 429 → **436**（新增 `tests/features/map.rs`，7 组）。

**残留**

- `built-ins/Map` 剩 7 条：4 条是 `Map.groupBy` 之后用 `Array.from(map.keys())` 取结果 —— **`Array.from` 根本不走迭代协议**（`builtins/array.rs::array_from` 只按 `length` + 索引取快照），这是 ES6 范围内的独立缺口，`built-ins/Array/from` 里还有 30 条同源失败；1 条 `map.js` 等全局对象（顶层 `this`）；2 条需要"闭包内可写外部变量"（M7-P5 数据模型债）。
- **`Set` / `WeakMap` / `WeakSet` 仍未开工**（B2b），`Map` 的迭代器状态与接收者检查可以直接复用。
- `Map.groupBy`（ES2024）与 `getOrInsert`/`getOrInsertComputed`（ES2026 提案）**不在 ES6 承诺面内**，但已在钉住的 test262 里、且和 `Map` 同一套迭代/回调机制，所以随本批实现了（38 条）。M8-V3 若要按"范围外"把它们从 KPI 里摘掉，可以再讨论 —— 它们不是 M7 的承诺。

#### B2b `Set` —— 已完成（2026-09-26）

**起点**：通过 13584 / 17647（76.98%）。

**交付**：`Set` 从 `IN_SCOPE_PENDING` 摘掉，`built-ins/Set` 加进 `SUITES`。入册后 `built-ins/Set` **337 / 357 = 94.40%**，A3 的 ≥80% 达标。

**改动**

| 层 | 内容 |
|----|------|
| `vm/object.rs` | `SetObject`（`Vec<Option<Value>>` 保插入序、`SameValueZero`、`-0` 归一、墓碑删除）+ `impl JSObject`，与 `MapObject` 同构 |
| `vm/iterator.rs` | `NativeIteratorState::Set { set, kind, idx, done }` + `SetIterKind{Value, Entry}`（`entries` 产出 `[v, v]`） |
| `builtins/set.rs`（新） | `add`/`has`/`delete`/`clear`、`size` 访问器、`Symbol.toStringTag`、`Set.prototype.keys === values`（同一个函数对象） |
| `builtins/mod.rs` | `set_prototype` 字段与装配、`Set[Symbol.species]` 访问器、arity（`Set` 0、`add`/`has`/`delete` 1、`set-methods` 各 1） |
| `vm/mod.rs` | `set_construct`（`GetIterator` + `Get(set,"add")` 后 `Call`，任一 abrupt 先 `IteratorClose`）、`set_for_each`、`set_iterator`、`map_method`/`set_method` 拆分、`SetRecord` + 七个集合算子 |
| `tests/` | `tests/features/set.rs`（6 组断言）+ runner 的解锁/入册 |

**两个必须记住的坑**

1. **Map 与 Set 的原型方法必须各有各的派发前缀**。B2a 时我用的是单一 `__collection_method__` + "按接收者类型分派"，结果 `Set.prototype.has.call(new Map())` 会被分派到 `Map.prototype.has` 而**不抛错** —— 规范要求它是 TypeError。同名方法（`has`/`delete`/`clear`）到底属于哪个原型，只能靠**派发名**区分：现在 `MAP_METHOD_PREFIX` 与 `SET_METHOD_PREFIX` 各自成对，`VM::map_method` / `VM::set_method` 各自校验接收者。`call_prototype_method` 里那批带接收者守卫的同名分支随之删掉。
2. **集合迭代器"走完一轮"是永久的**。`values-iteration-mutable.js` 明确要求：迭代器耗尽之后再 `add` 新元素，后续 `next()` 仍然是 `done`。规范的做法是把内部"迭代对象"置空（ES 23.2.5.2.1 step 9），实现里就是状态上一个 `done` 标志。Map 侧同源，一并加上。

**顺带把 `set-methods`（ES2024）实现掉了**：`union` / `intersection` / `difference` / `symmetricDifference` / `isSubsetOf` / `isSupersetOf` / `isDisjointFrom` 共 179 条，是本套件最大的失败族。要点：

- `GetSetRecord` 的 `ToNumber(size)` **会跑用户代码**（`size` 可以是带 `valueOf` 的对象，测试会数这个调用），所以不能停留在 `Value::to_number()`，要走 VM 的 `to_primitive`；
- `intersection` / `difference` 按**较小的一侧**决定遍历谁，这直接决定结果顺序（`[...new Set([3,2,1,0]).intersection(new Set([1,3,5]))]` 是 `[1,3]` 而不是 `[3,1]`）；`difference` 在 `this.size ≤ arg.size` 时**根本不创建** `keys()` 迭代器（`allows-set-like-*.js` 就查这个）；
- 提前得出答案时（`isSupersetOf`/`isDisjointFrom` 返回 `false`）必须 `IteratorClose`，测试数 `return()` 调用次数；
- 结果永远是**普通 `Set`**（`%Set.prototype%`），不看子类也不看 `@@species`。

**效果**

| 指标 | 起点 | 本批后 |
|------|------|--------|
| 全量通过 | 13584（76.98%） | **13943**（77.36%） |
| 失败 | 4063 | **4081**（新增执行的用例里含失败） |
| 执行 / 跳过 | 17647 / 8143 | 18024 / 8149 |
| `built-ins/Set` | 未入册 | **337 / 357 = 94.40%** |
| `built-ins/Map` | 155（+ 新解锁 12 条） | **167 / 174 = 95.98%** |
| `language/statements/for-of` | 381 | **386** |
| `language/statements/class` | 1113 | **1116** |
| 其余 | — | `Object`+1、`expressions/class`+1 |

净 **+359**，**逐套件零回退**。单元 190 全绿；features 436 → **442**（新增 `tests/features/set.rs`，6 组）。

**残留**

- `built-ins/Set` 剩 20 条，全部是**结构性债**而不是 Set 本身的问题：
  - 7 条 `*/size-is-a-number.js` 要求 `size` 是 BigInt 时抛 TypeError —— **引擎没有 BigInt**（§3 范围外），这 7 条要等 BigInt 决策；
  - 7 条 `*/set-like-class-order.js`、`*/set-like-class-mutation.js` 依赖"闭包内自增外部标量"（`index++`、`nextCalls++`），即 M7-P5 的闭包捕获债 —— 其中 `symmetricDifference` 那条会退化成死循环（撞步数上限）；
  - 1 条 `set.js` 等全局对象（顶层 `this`）。
- `WeakMap` / `WeakSet` 仍未开工（B3）。它们可以完全复用本批的 `MAP_METHOD_PREFIX`/`SET_METHOD_PREFIX` 套路，但**只有 `get`/`set`/`has`/`delete` 四个方法**（无 `size`、无迭代器），`Rc` 下"弱"引用退化为强引用这一点要按 §8 登记偏差。

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

## 9. 文档维护与接手

**四份文档的分工**（新接手者按此顺序读）：

| 文档 | 角色 | 更新时机 |
|------|------|----------|
| `handover.md` | **接手入口**：怎么跑、当前状态、三条纪律、下一步 | 每批完成后更新 §4 的表格与"下一步" |
| **本文件** | **当前计划**：目标、批次表、批次记录、度量口径 | 每批完成后更新 §1 基线、§6 表格状态列、§6.1 追加批次记录 |
| `architecture.md` | **实现地图**：管线、运行时模型、任务→改哪里、地雷 | 架构层面有变动时（新指令层、新对象类型、新增一条纪律） |
| `es6-feature-support.md` | 逐特性支持矩阵（对使用者的口径） | 一个任务包交付时 |

**基线快照与回退检测**：`scripts/phase2-status.sh` 跑齐三级验证并与 `docs/phase2-status.tsv` 逐套件对比。
默认只检查（不改文件）；`--update` 才刷新快照 —— 完成一批后运行它，并把快照随改动一起提交。
脚本里的 `EXPECTED_SKIPPED` 是"跳过表只剩范围外/G3 排除项"的不变量，它变了会主动告警。

**不再做的事**：不往 `es6-conformance-plan.md`（M0–M4 工作日志）追加计划性内容 —— 它已冻结为历史基线；
它的 §1.3 记录了本次冻结时的订正清单。
