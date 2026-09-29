# ES6 一致性第二阶段计划书（M5 → M8）

> **起算点**：2026-09-25。**前一份**`docs/es6-conformance-plan.md` 覆盖 M0 → M4，其 M2' / M3 / M4 的验收条件均已达标（核对见该文档 §1.3.1），自此作为**工作日志与历史基线**保留。
> **本文件是第二阶段的任务书**，自包含：从这里出发不需要读旧文档就能排期与施工。旧文档只在需要追溯"某个结论是怎么来的"时查阅。
> **本阶段的起点不是空档**：当前已执行的 17477 条里仍有 3622 条 ES6 范围内的失败（§1.2），那是 M2/M3/M4 的残余，构成 M7。

---

## 1. 起点基线（2026-09-25 实测）

| 指标 | 数值 | 说明 |
|------|------|------|
| test262 执行 | 18266 | runner 实际跑的数（B2/B3 起含 `Map`/`Set`/`WeakMap`/`WeakSet`） |
| 通过 | **14183** | 主指标（B3 后） |
| 失败 | 4083 | 其中约 3400 条由 ES6 范围内特性驱动，约 640 条涉及范围外特性 |
| 跳过 | 8133 | 范围外 / G3 排除项 + 已入册套件自己的 feature 门控 |
| 通过率 | 77.65% | 参考值 |
| 单元测试 | 190 全绿 | |
| feature 集成测试 | 33 个文件 / 448 用例全绿 | |
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
| `built-ins/Date` | 594 | —（B14 入册；§3.1 已判"实现"） | — |

runner 未枚举的其余部分（`language/expressions` 11095、`language/statements` 9337 等）绝大多数是**已在 SUITES 中按子目录覆盖**的父目录，还有 `built-ins/Temporal` 4603、`built-ins/RegExp` 1879、`built-ins/Iterator` 514、`language/module-code` 755 属范围外。**M8-V1 负责把这份清单补全并写明每一条的理由。**

---

## 3. 范围判定（本阶段）

沿用第一阶段的 §1.1 三条硬约束（strict-only / ES6 特性集 / 无 eval + with），本阶段新增两条判定：

| 项 | 判定 | 理由 |
|----|------|------|
| `Date`（594） | **在范围，实现**（批次 **B14**） | 第一阶段 §1.2 已判"ES5 但属必备"，M8-V2 的结论见下 |
| RegExp 驱动的失败 | **报告单列，不动跳过表** | 引擎无正则实现（第一阶段已判范围外），但实测只有 100 条纯粹因它失败，且它们同时依赖范围内特性（§3.1） |
| `Promise` | 在范围 | 需要先定微任务调度点（§5 M5-C3） |
| `Proxy` / `Reflect` | 在范围 | 依赖的 `[[Get]]`/`[[Set]]`/`[[DefineOwnProperty]]` 已成型，可以开工 |

### 3.1 KPI 归类（M8-V3 结论，2026-09-26 实测）

A1（"范围内失败 ≤ 800"）只有在把范围外失败摘出来之后才可测。归类脚本
`scripts/kpi-noise.py` 对全量输出做一次分桶，可随时复跑：

```sh
ulimit -v 6000000
TEST262_FAILURES=99999 cargo test --release --test test262_runner -- --nocapture > /tmp/full.txt
python3 scripts/kpi-noise.py /tmp/full.txt
```

2026-09-26（4083 条失败）的结果：

| 桶 | 条数 | 占比 | 判据（脚本里的规则） |
|----|------|------|----------------------|
| **范围内真实缺口** | **3807** | 93.2% | 其余全部 |
| RegExp 驱动 | 100 | 2.4% | 测试源码里出现正则字面量或 `RegExp`/`.exec(`/`.test(`/`Symbol.match` 等 |
| 后 ES6 的 API | 176 | 4.3% | 测试直接调用引擎**完全没有**的 API：`Object.fromEntries`(24)、`Object.groupBy`(14)、`Object.getOwnPropertyDescriptors`(12)、`Array.prototype.toSpliced/toSorted/toReversed/findLast/flat`(55)、`Math.sumPrecise`(8)、`BigInt(`(50) 等 |
| 读不到源码 | 0 | — | 路径解析失败（脚本自身的 bug 信号） |

两条**必须写在报告里**的口径说明：

1. **`class-fields-public` / `class-static-fields-public`（489 条）留在"范围内"桶里**，不单列扣除。
   它们的失败与 `destructuring-binding`、`computed-property-names`、`generators` 等范围内特性共存
   （§3 的纪律：跳掉会埋掉真实覆盖）。脚本单独打印这个数量，供报告加脚注。
2. **不动跳过表**。RegExp 与后 ES6 的 API 都是"这个测试永远不可能通过"的**充分**条件，却不是**必要**条件：
   同样的失败里有相当一部分（例如 `set-methods` 的 19 条）真实原因是范围内的债（闭包捕获、全局对象）。
   把它们整目录跳掉，数字会好看，A1 反而不可测。因此 §3 的这条判定是"报告单列"，不是"补进跳过表"。

**M8-V2 `Date` 的结论：实现（不降级）**，作为独立批次 B14 排入计划：

- 594 个测试里 336 个是纯算术（`prototype/get*` 144 + `prototype/set*` 192），148 个是 `Date.UTC`，
  即**不需要外部数据**就能正确实现（`MakeDay`/`MakeTime`/`MakeDate` 都是闭式算法）；
- 需要外部数据的两块按"登记偏差"处理（§8）：**无时区数据库** → 本地时间等于 UTC、
  `getTimezoneOffset()` 返回 0；**无 locale 数据** → `toLocale*` 退回默认格式
  （与 `Number`/`String` 的 `toLocale*` 现状一致）；
- `Date.parse` 只承诺 ISO-8601（`toISOString` 的输出一定能解析回来），其余格式返回 `NaN`。
- 验收按 A7：套件入册后通过率 ≥ 50%。

---

## 4. 阶段目标与总体验收

**总目标**：把 §2.3 承诺面清单里的 ES6 特性补齐，并清掉 §1 的 3622 条范围内失败。

| # | 验收项 | 当前 | 目标 |
|---|--------|------|------|
| A1 | M7 范围内失败 | 3807（§3.1 归类后） | ≤ 800（RegExp 驱动与后 ES6 API 按 §3.1 扣除后计入） |
| A2 | M7 的六个任务包（§5） | 0/6 | 6/6 有交付记录且各自验收达标 |
| A3 | `Map` / `Set` / `WeakMap` / `WeakSet` | ✅ 96.3% / 94.5% / 99.2% / 98.8%（B2a–B3 已入册解锁） | 各自套件通过率 ≥ 80%，且已入册解锁 |
| A4 | `Proxy` / `Reflect` | 0% | 各自套件通过率 ≥ 50%（依赖内部方法，允许更低） |
| A5 | `ArrayBuffer` / `DataView` / `TypedArray` | 0% | 各自套件通过率 ≥ 50%；detach/resizable 允许登记偏差 |
| A6 | `Promise` | 0% | 套件通过率 ≥ 50%（含微任务调度点落地） |
| A7 | `Date` | 结论已落地（§3.1：实现），0 开工 | 批次 B14 交付后套件通过率 ≥ 50% |
| A8 | 计划内通过数 | 15978 | ≥ 20000（本阶段结束时） |
| A9 | 单元 / feature / 护栏测试 | 190 / 503 / 7 全绿 | 全绿；每个任务包新增 ≥ 5 条断言的 feature 用例 |
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
| **B3** ✅ | M5-C2 `WeakMap`/`WeakSet` | 226 | **已完成**，见 §6.1；两套件 99.24% / 98.75%，A3 的 ≥80% 达标 |
| **B4** ✅ | M8-V3 RegExp 失败归类 + M8-V2 `Date` 决策 | — | **已完成**，见 §6.1 与 §3.1：归类脚本 `scripts/kpi-noise.py`（3807 / 100 / 176），`Date` 判"实现" |
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
| **B14** | M5-C5 `Date`（含入册解锁） | 594 | B4 判定的落点；336 条纯算术 + 148 条 `Date.UTC` 不依赖外部数据，偏差见 §3.1 |
| **B15** | M7-P7 解构赋值族（B4 侦察出的五族，按 1→3→2 拆批） | 974 | **B15a 已完成**（头部赋值模式，+177）、**B15b 已完成**（顺带挖出的"标签语句被整段丢弃"，+24），见 §6.1；族 3/2 与生成器参数债见 B15b 的残留 |
| **B16** | M7-P8 `rest` 参数（全形态）+ `SUITES` 空条目造成的覆盖空洞 | 11 + | **已完成**，见 §6.1 |
| **B17** | M7-P9 生成器参数的**调用时绑定**（`PrologueEnd` 屏障 + 帧泊车） | 39 + | **已完成**，见 §6.1；+88，零回退 |
| **B18** | M7-P10 `var` 的**函数作用域与提升** + 块作用域遮蔽 | 60 + | **已完成**，见 §6.1；+62，2 处回退已逐条解释 |
| **B19** | M7-P11 `try` 内 abrupt 完成（`break`/`continue` 跳出 `try`） | 6 + | **已完成**，见 §6.1；+6，零回退，并修回 B18 被它遮挡的 2 条用例 |
| **B20** | M7-P12 TDZ（`let`/`const`/类名绑定的"已绑定但未初始化"窗口） | 20 + | **已完成**，见 §6.1；+9，零回退，修回 B18 遗留的 class 用例 |
| **B21** | M7-P13 脚本**声明记录**与全局对象分离（数据模型债第一件） | 3 + | **已完成**，见 §6.1：+3，零回退；B20 残留 1 落地，闭包从此看到脚本级 `let` 的**活**绑定 |
| **B22** | M7-P14 NamedEvaluation（匿名函数/类取绑定名） | 1056 | **已完成**，见 §6.1：**+271，零回退**，15 个套件提升 |

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

#### B3 `WeakMap` / `WeakSet` —— 已完成（2026-09-26）

**起点**：通过 13943 / 18024（77.36%）。

**交付**：两个特性名从 `IN_SCOPE_PENDING` 摘掉，`built-ins/WeakMap` 与 `built-ins/WeakSet` 加进 `SUITES`。`WeakMap` **131 / 132 = 99.24%**、`WeakSet` **79 / 80 = 98.75%**（各只剩 1 条 `weakmap.js` / `weakset.js`，卡在全局对象）。

**改动**（比 B2a/B2b 小得多，因为完全复用了它们的存储与派发套路）

| 层 | 内容 |
|----|------|
| `vm/object.rs` | `WeakMapObject` / `WeakSetObject`：内部各自包一个 `MapObject` / `SetObject`，`kind()` 与 `class_name()` 覆盖成 `WeakMap`/`WeakSet`，其余 20 个 `JSObject` 方法由一个 `delegate_collection_object!` 宏转发 |
| `builtins/weak.rs`（新） | 四个 / 三个方法 + `CanBeHeldWeakly` 键规则；两个原型各注册 `Symbol.toStringTag`（顺手抽了 `define_string_tag` 助手） |
| `builtins/mod.rs` | `WEAKMAP_METHOD_PREFIX` / `WEAKSET_METHOD_PREFIX`、两个原型字段与装配、arity |
| `vm/mod.rs` | `weakmap_method` / `weakset_method`（前缀派发 + 精确接收者检查）、`weakmap_construct` / `weakset_construct`（`GetIterator` + `Get(map,"set")`/`Get(set,"add")` 后 `Call`，任一 abrupt 先 `IteratorClose`）、`WeakMap.prototype.getOrInsert` / `getOrInsertComputed`（`upsert` 提案） |
| `vm/value.rs` | `SymbolData` 加 `registered` 标志（`Symbol.for` 造出来的符号带 `true`） |
| `tests/` | `tests/features/weak_collections.rs`（6 组断言）+ runner 的解锁/入册 |

**两个规范细节（都踩到了）**

1. **`CanBeHeldWeakly` 接受对象与"非注册 Symbol"**（ES 24.2.1）。最初只认对象，结果 9 条 Symbol 键用例直接失败（`WeakMap` 的 `*symbol-key*` 一族、`WeakSet` 的 `adds-symbol-element`）。同时又不能放宽到底：`Symbol.for('x')` 是**注册**符号，永久可达，规范禁止把它当弱键 —— 这要求 `SymbolData` 记住自己是不是注册出来的（原来没有这个信息）。
2. **`set`/`add` 抛错，但 `get`/`has`/`delete` 对非法键必须安静**（分别给 `undefined`/`false`/`false`）。这一条把“键规则”拆成了两个方向，不能一把梭。

**效果**

| 指标 | 起点 | 本批后 |
|------|------|--------|
| 全量通过 | 13943（77.36%） | **14183**（77.65%） |
| 失败 | 4081 | **4083** |
| 执行 / 跳过 | 18024 / 8149 | 18266 / 8133 |
| `built-ins/WeakMap` | 未入册 | **131 / 132 = 99.24%** |
| `built-ins/WeakSet` | 未入册 | **79 / 80 = 98.75%** |
| `built-ins/Map` | 167（+ 新解锁） | **180 / 187 = 96.26%** |
| `built-ins/Set` | 337 / 357 | **344 / 364 = 94.51%** |
| `language/statements/class` | 1116 | **1122** |
| 其余 | — | `Object`+2、`expressions/class`+2 |

净 **+240**，**逐套件零回退**。单元 190 全绿；features 442 → **448**（新增 `tests/features/weak_collections.rs`，6 组）。

**偏差登记（§8 已更新）**：条目存储在 `Rc` 里，**键不可达时不会被回收** —— "弱"这一点在本引擎不可观察。其余可观察面（键规则、无 `size`/无 `@@iterator`/无 `clear`、`get`/`has`/`delete` 的安静回答、构造器走迭代协议）都按规范实现。

**残留**：两个套件各剩 1 条 `weakmap.js` / `weakset.js`，都只是 `verifyProperty(this, 'WeakMap', …)` —— 卡在**全局对象**（顶层 `this` 是 `undefined`）。

#### B4 KPI 归类（M8-V3）与 `Date` 决策（M8-V2）—— 已完成（2026-09-26）

**起点**：通过 14183 / 18266，失败 4083。

**这一批不改引擎**，只回答两个悬空的问题，让 A1/A7 从"无法测量"变成"可测量、已判定"。

**1. 分类（M8-V3）**：新增 `scripts/kpi-noise.py`，对全量输出做分桶，可复跑：

```
4083 条失败 = 3807 范围内真实缺口（93.2%）+ 100 RegExp 驱动（2.4%）+ 176 后 ES6 API（4.3%）
```

- 脚本读**测试源码**而不是靠猜：正则字面量 / `RegExp` / `.exec(` / `.test(` / `Symbol.match` 等出现即判 RegExp 驱动；
  直接调用引擎完全没有的 API（`Object.fromEntries` 24、`Object.groupBy` 14、`Object.getOwnPropertyDescriptors` 12、
  `toSpliced`/`toSorted`/`toReversed`/`findLast`/`flat` 55、`Math.sumPrecise` 8、`BigInt(` 50 …）判后 ES6。
  规则与局限写在脚本头部（例如"这个测试能不能在没有正则引擎的情况下通过"，而不是"唯一根因是什么"）。
- 结论是**报告单列，不动跳过表**：这两类都只是"永远不可能通过"的充分条件，不是必要条件 ——
  同一批失败里有相当一部分（`set-methods` 的 19 条就是）真实原因是范围内的债（闭包捕获、全局对象），
  整目录跳掉会让数字变好而 A1 不可测。这与 §3 里 `class-fields-public` 的纪律一致。
- `class-fields-public` / `class-static-fields-public`（489 条）**留在范围内桶里**，脚本单独打印计数供报告加脚注。
- 附带产出：失败用例的 feature 标签分布（前几名：`destructuring-binding` 974、`generators` 611、
  `class-fields-public` 368、`default-parameters` 317、`class` 294、`computed-property-names` 193、`Symbol.iterator` 182）。
  这张表直接指出下一批该往哪打：**解构 + 生成器 + class 元素**三族占了范围内失败的一半以上。

**2. `Date`（M8-V2）**：判定**实现，不降级**，落点排成新批次 **B14**（§6 批次表）。

- 依据：594 个测试里 336 个是纯算术（`prototype/get*` 144 + `prototype/set*` 192），148 个是 `Date.UTC` ——
  `MakeDay`/`MakeTime`/`MakeDate` 都是闭式算法，不需要时区库；真正依赖外部数据的只有格式化与解析两小块。
- 登记偏差（§8）：无时区数据库 → 本地时间 == UTC、`getTimezoneOffset()` 恒为 0；无 locale 数据 →
  `toLocale*` 退回默认格式；`Date.parse` 只承诺 ISO-8601，其余格式 `NaN`。
- 验收按 A7：入册后套件通过率 ≥ 50%。

**效果**：文档状态变更（§3/§3.1/A1/A3/A7/A8/A9/§6 批次表），**引擎与数字不变**（14183 / 18266，跳过 8133，逐套件无回退）。

**残留**

- B14（`Date`）与 B6（生成器 + 数据模型三债）都在排队；§3.1 的标签分布说明**解构（974）/ 生成器（611）/ class 元素（489）**
  是范围内失败最大的三块，B6 之后建议优先按这个顺序拆批，而不是按计划书原来的 B7/B8 顺序。
- 归类脚本的分桶是启发式：`BigInt(` 50 条里可能混着"只是提到 BigInt"的用例；`regexp` 桶也包含"正则 + 真实缺口"的混合用例。
  §3.1 已写明这个局限；如果 A1 要作为完成判据，先用它做**减法**、再由人工抽查 20 条确认。

**下一批的侦察（2026-09-26，同批完成）**：把"解构 974"当成一个根因是错的，实测至少五族，得拆开打：

1. **`for`/`for-of` 头部的"赋值模式"完全不赋值**（声明形式正常）—— 单条最小复现：
   ```js
   var v; for ([v] of [[2]]) {}   // v 仍是 undefined（node: 2）
   var w; for (var [w] of [[2]]) { }   // 正常
   ```
   落点在降级层 `bind_for_of_left`（`ForStatementLeft` 的赋值目标分支），是这五族里最便宜的一族。
2. **默认值位置上的匿名函数没有推断 `name`**：`dflt-*-elem-id-init-fn-name-{arrow,cls,cover,fn,gen}` 全族
   （预期 `"arrow"`/`"cls"`/`"cover"`/`"fn"`/`"gen"`，实际 `""`/`"<anonymous>"`/`"<class>"`）。规范要求
   `[x = function () {}] = []` 里的函数名取自绑定名（NamedEvaluation）。
3. **解构 `null`/`undefined` 不抛 `TypeError`**：`dflt-ary-ptrn-elem-ary-val-null` 一族。
4. **迭代器/rest 语义**：`array-elem-iter-*`(16)、`ary-ptrn-rest-*`(23)。
5. **对象模式**：`obj-ptrn-*`(62+76) + `obj-prop-elem`(24) + `obj-id-init`(21)。

`dstr` 之外，同一个 for-of 目录里还有 41 条"应抛 TypeError 却没抛"、35 条"应抛 Test262Error 却没抛"
（多为测试自己 `throw` 的断言没被执行到，即上面的第 1 族把断言整段跳过了）。**建议按 1 → 3 → 2 的顺序拆批**：
先修最窄、最容易验证的头部赋值模式，再看非对象解构的 abrupt 检查，最后处理 NamedEvaluation。

---

#### B15a `for-of`/`for-in` 头部的赋值模式 —— 已完成（2026-09-26）

**起点**：通过 14183 / 18266（77.65%），失败 4083，跳过 8133。

**根因**：`bind_for_of_left` 只处理了 `VariableDeclaration`、`AssignmentTargetIdentifier` 与两个成员表达式；
`ArrayAssignmentTarget` / `ObjectAssignmentTarget`（即"头部是模式"的 `AssignmentPattern`）落进 `_ =>` 分支，
只打一条 `log::warn!`。于是 `for ([a, b] of …)` 的循环体**带着全部仍是 `undefined` 的目标**跑：
`language/statements/for-of/dstr` 那 138 条 "Expected SameValue(«undefined», «2»)" 与 35 条
"Expected a Test262Error to be thrown"（测试自己的断言根本没被喂到值）都是它。

最小复现（修复前）：

```js
var v; for ([v] of [[2]]) {}   // v 仍是 undefined（node: 2）
var w; for (var [w] of [[2]]) { w = w + 1; }   // 声明形式一直是好的
```

**改动**：两行 —— 两个分支改调已经在用的 `bind_array_assignment_target` / `bind_object_assignment_target`
（与 `[a, b] = arr`、`({x} = obj)` 完全同一条路径，因此默认值、省略元素、rest、嵌套、成员目标、
以及"模式提前结束要 `IteratorClose`"的义务都是现成的，没有第二套语义）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14183 | **14360** | **+177** |
| 失败 | 4083 | 3906 | −177 |
| 通过率 | 77.65% | **78.62%** | +0.97pp |
| 单元 / feature | 190 / 448 | 190 / **454** | +6 条断言（新文件 `tests/features/destructuring.rs`） |

逐套件核对（`./scripts/phase2-status.sh`）：只有 `language/statements/for-of` 变化（386 → 563），
**其余全部持平，无一条新失败**（对 177 条修好的用例做了路径级 diff，`comm -13` 为空）。

**同批附带的工具改动**：三层执行护栏（§7.1）。第一批全量回归里那条幽灵用例
（`Set/prototype/symmetricDifference/set-like-class-mutation.js`）单条烧 60s，最坏的一次把 3 分钟的全量
拖成 15 分钟；现在墙钟（15s）/ 步数 / 堆增量（256MiB）三层都变成**普通失败**，`tests/guards.rs` 7 条
断言覆盖护栏自身。定稿后逐套件核对仍为零回退（TOTAL 14360 与关掉护栏时一致）。

**残留**（下一批的输入）

- `language/statements/for-of` 还剩 **144** 条，最大的族：`ary-ptrn-*` 21、`obj-ptrn-*` 15、`elem-trlg-*` 10、
  `rest-non-*` 7、`close-via-*` 7、`label-from-*` 6、`rest-iter-*` 5、`prop-elem-*` 5、`id-init-*` 5、`elem-iter-*` 5。
  其中 `rest-non-*`（rest 目标不可迭代/为 null 应抛 TypeError）与 `close-via-*`（break/return/label 引发的 `IteratorClose`）
  是成体系的语义，建议下一片先看这两族。
- 同一批还验证过一个**不是**缺口的现象：`for (x of it) { break; }` 的 `IteratorClose` 是好的
  （`return()` 确实被调用），别被探针骗了。
- **探针方法学**：本批的自测脚本被闭包捕获债干扰 —— `function counter(){ var i = 0; return function () { return i++; }; }`
  三次调用返回 `1,1,1`（应为 `0,1,2`），凡是"闭包内可变状态"驱动的探针都会给出假结论
  （本项目已有多次：iterator 计数器、`return()` 计数器）。**结论一律以 test262 的路径级 diff 为准**。
- §6.1 的侦察里余下四族仍在队列：3（解构 `null`/`undefined` 不抛 TypeError）、2（默认值匿名函数的 NamedEvaluation）、
  4（迭代器/rest）、5（对象模式）。

---

#### B15b 标签语句被整段丢弃 —— 已完成（2026-09-26）

**起点**：通过 14360 / 18266（78.62%），失败 3906，跳过 8133。

**怎么发现的**：B15a 之后 for-of 还剩 144 条，其中一族叫 `break-label` / `continue-label` /
`*-label-from-try|catch|finally`，报文是 `Expected SameValue(«0», «1»)`。最小复现直接把触发条件缩到最窄：

```js
lblD: while (d < 2) { d = d + 1; }   // d 仍是 0
lblG: { g = 5; }                     // g 仍是 0
```

**根因**：`lower_statement` 里**没有 `Statement::LabeledStatement` 分支**，标签语句落进
`_ => log::warn!("unimplemented statement")` —— 不只是 `break label` 不工作，而是**带标签的语句体
被整段删掉**。任何形如 `lbl: while (…)`、`lbl: { … }` 的代码都静默地什么都不做（只留一行 warning）。

**改动**（`src/compiler/lowering/mod.rs`）：

- 新增 `LabelContext`（break 目标、可空的 continue 目标、SEH 深度、所属循环深度）与
  `pending_labels`：标签在循环之前降级，所以先挂起，由 `enter_loop_context` 认领 —— 这就是
  `a: b: for (…)` 链式标签共享同一对目标的方式；
- `leave_loop_context` 回收该循环认领的标签（按 `loop_depth` 判定，非循环标签由 `lower_labeled` 自己弹栈）；
- 非迭代标签（`lbl: { … }`、`lbl: switch (…)`）自己开一个 `label_after` 块，`break lbl` 跳到块尾；
- `lower_break`/`lower_continue` 接受可选标签，按名字从内向外解析；
- **`lower_loop_exit` 增加 `ctx_seh_depth` 参数**：跨多层循环的标签跳转要按*标签所属*循环的
  `finally` 层数展开，而不是最内层的（否则 `lbl: while (true) { try { …; break lbl; } finally { … } }` 会漏跑或重跑 `finally`）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14360 | **14384** | **+24** |
| 失败 | 3906 | 3882 | −24 |
| 通过率 | 78.62% | **78.75%** | +0.13pp |
| 单元 / feature / 护栏 | 190 / 454 / 7 | 190 / **459** / 7 | +5 条断言（`tests/features/control_flow.rs`） |

逐套件：`break` 11 → **18**（7 条全清）、`for-of` 563 → 571、`for` 314 → 318、`continue` 15 → 18、
`while` 17 → 18、`do-while` 18 → 19；**其余零变化**（`./scripts/phase2-status.sh` 结论：无逐套件回退）。

**残留**（下一批的输入）

1. **早错误缺失（与上面同族，但需要新机制）**：未定义标签、`continue` 指向非迭代标签在规范里都是
   *SyntaxError*，现在只 `log::warn!`。lowerer 目前没有错误通道，要修得先给它一个（或加一趟后置检查）。
2. **生成器参数绑定时机（B15b 顺带挖出，独立根因）**：
   ```js
   function* f([[x]]) {}   f([null]);   // node: 调用时抛 TypeError；本站：不抛
   var it = f([null]); it.next();        // 本站把错误推迟到这里才报
   ```
   引擎把生成器的参数绑定推迟到第一次 `next()`（`generator_next` 的 `SuspendedStart` 分支才建帧），
   规范要求在**调用时**（FunctionDeclarationInstantiation）完成。受影响的是
   `language/{statements,expressions}/generators/dstr/*` 里所有"应在调用时抛"的用例（实测三个套件里
   `Expected a TypeError` 共 40 条，多数属这一族）。修法需要给生成器标出"参数区结束"的 pc，
   在 `create_generator` 里跑完参数区再挂起 —— 不要图省事把整个函数体跑起来（`var it = g(); calls++` 这类
   用例会立刻挂）。
3. **生成器 rest 参数完全未绑定**：`function* g7(...rest) { yield rest.length; }` → `ReferenceError: undefined variable: rest`
   （普通函数的 rest 参数正常，只有生成器这条路径漏了）。这条独立、量小，可以和第 2 条一批做。
4. B4 侦察出的五族里**族 3（解构 null/undefined 的 abrupt 检查）在简单形状上已是对的**（实测 12 个形状与 node 一致），
   剩下的失败全部落在上面的生成器参数族里；族 2（默认值匿名函数的 NamedEvaluation）仍在队列。

---

#### B16 `rest` 参数（全形态）与覆盖空洞 —— 已完成（2026-09-26）

**起点**：通过 14384 / 18266（78.75%），失败 3882，跳过 8133。

**怎么发现的**：B15b 的残留 3 写的是"生成器 rest 参数完全未绑定"。动手前先确认根因，结果比描述严重得多：

```js
function f(...a) { return a.length; }   // ReferenceError: undefined variable: a
```

**rest 参数在所有函数形态下都没实现**（普通函数/函数表达式/箭头/方法/构造器/生成器），不只是生成器。

**根因**：`lower_function_inner` 拿的是 `&func.params.items` —— 而 rest 元素在 AST 里是
`FormalParameters::rest`，**不在 `items` 里**。降级遍历 `items` 时它从未被绑定，`...rest` 的名字
自然不在符号表里，任何引用都直接 `ReferenceError`。

**顺带挖出一个覆盖空洞**：`SUITES` 里写的是 `language/functions/rest-parameters`，
而测试实际在 `language/rest-parameters`（`language/functions/` 目录根本不存在）。`run_suite`
对不存在的目录返回 0，于是这个套件在每张表里都是 `0 0 0` —— **静默的覆盖空洞**。已改名，
`phase2-status.sh` 的对比也把这次改名如实报成"移除一个套件 / 新增一个套件"。

**改动**：

- `lower_function_inner` 改为接收整个 `FormalParameters`，在参数循环之后绑定 rest：
  `make_rest(params.len())`（`MakeRest` 指令早就存在，类的默认构造器一直在用它）+ `bind_pattern`，
  因此 `...[]`/`...{}` 这类 *模式* rest 也一并可用；
- `SUITES`：`language/functions/rest-parameters` → `language/rest-parameters`。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14384 | **14399** | +15 |
| 执行 | 18266 | **18277** | +11（新入册的套件） |
| 失败 | 3882 | 3878 | −4 |
| 跳过 | 8133 | 8133 | 0（跳过表未动） |
| 通过率 | 78.75% | **78.78%** | |
| 单元 / feature / 护栏 | 190 / 459 / 7 | 190 / **464** / 7 | +5 条断言（`tests/features/functions.rs`） |

逐套件核对：`language/rest-parameters` 新增（**11/11，100%**）、`language/expressions/arrow-function`
174 → 177、`built-ins/Map` 180 → 181，**无任何回退**。

**残留**

1. **生成器参数的"调用时绑定"**（B15b 残留 2，实测两个生成器套件里 `Expected a TypeError to be thrown`
   共 39 条属这一族）：参数绑定被推迟到第一次 `next()`，而规范要求在调用时完成。修法已论证：
   在参数区末尾发一条屏障指令，`create_generator` 里建帧并跑到屏障后挂起（复用
   `restore_generator_frame`/`store_suspended_generator` 的帧快照），**不要**把函数体也跑起来。
   这块是生成器帧机制（本项目的地雷区），单独一批做并单独回归。
2. rest 参数本身的语义边界（`no-alias-arguments` 等 11 条已全绿）没有已知缺口。
3. B15b 残留 1（标签早错误）与 B4 侦察的族 2（默认值匿名函数的 NamedEvaluation）仍在队列。

---

#### B17 生成器参数的"调用时绑定" —— 已完成（2026-09-26）

**起点**：通过 14399 / 18277（78.78%），失败 3878，跳过 8133。

**根因**：生成器的参数绑定被推迟到第一次 `next()`（`generator_next` 的 `SuspendedStart` 分支才建帧），
而 ES 9.2.12 `FunctionDeclarationInstantiation` 在**调用时**就完成。最小复现：

```js
function* f([[x]]) {}
f([null]);          // node: 调用时抛 TypeError；本站：不抛（错误被推迟到 next()）
```

**改动**（这一批动的是生成器帧，所以先读透了挂起/恢复机制）：

- **IR/字节码加一条屏障指令** `PrologueEnd`（无操作数无定义；`ssabuilder` 的两个 match 都有兜底分支，
  只有 `rename_instruction` 需要补一个空 arm）；
- **降级层**：`lower_function_inner` 在参数（含 rest 与 `arguments`）之后、函数体之前，为生成器发这条屏障；
- **`create_generator` 改为 `Result`**：建完生成器对象后立即调 `run_generator_prologue` ——
  建帧（新提取的 `push_generator_frame`，与首 `next()` 共用同一套帧布局）→ 打上
  `running_generator_prologue` 标记 → 复用 `run_generator_frame` 跑到屏障 →
  得到帧快照后以 **`SuspendedStart` + 帧** 泊车（`store_suspended_generator` 增加了状态参数）；
- **屏障处理**：`Opcode::PrologueEnd` 只在 `running_generator_prologue` 为真时挂起，
  手法与 `Yield` 完全一致（`generator_yielded` + `jump(instructions.len())`），**但不报告值**
  ——生成器仍处于"尚未开始"，首 `next()` 从屏障之后继续；
- **`generator_next`**：`SuspendedStart` 分支先看有没有泊好的帧，有就直接恢复并 `pc + 1` 继续，
  没有才走原来的建帧路径。

**关键设计点**（都写进了代码注释）：跑**仅参数区**，绝不跑函数体 —— 体里第一条指令可能就是调用方
不该看见的 `yield`，而"体里迭代 ⇒ 重新进入本函数"正是原来那条"会无限递归"注释描述的事故。
屏障让两者都不发生。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14399 | **14487** | **+88** |
| 失败 | 3878 | 3790 | −88 |
| 通过率 | 78.78% | **79.26%** | +0.48pp |
| 单元 / feature / 护栏 | 190 / 464 / 7 | 190 / **468** / 7 | +4 条断言（`tests/features/generators.rs`） |

逐套件核对：`language/statements/generators` 165 → **209**、`language/expressions/generators`
176 → **220**，**其余全部持平（零回退）** —— 生成器帧的既有语义（`yield` / `return()` / `throw()` /
`yield*` / `for-of` 关闭）都用一组与 node 逐项对比的探针复核过：

```
nested=TypeError | dflt=TypeError | noniter=TypeError | throwing-dflt=boom
y1=1 y2=2 | done=true value=end | ret={"value":9,"done":true} | throw-in=thrown-in
before=0 mid=1 after=2        ← 调用时只跑参数区，函数体仍未开始
```

**残留**

1. **生成器的闭包捕获本来就是坏的**（与本批无关，复核时发现）：
   ```js
   function outer() { var secret = 41; function* g() { yield secret; yield secret + 1; } return g; }
   var it = outer()();   // ReferenceError: undefined variable: secret
   ```
   这是"闭包是创建时值快照"那笔债的生成器版本（`SuspendedFrame::closure_maps` 的切片口径可疑：
   `extract_generator_frame` 用的是推入帧内映射**之后**的深度）。要与数据模型债一起修。
2. **解构非对象的报错文案难看**：现在是 `Cannot read properties of null (reading 'Symbol(…)')`，
   类型对（`TypeError`，test262 只查类型）但文案应该来自 `RequireObjectCoercible`/`GetIterator`。
3. 之后回到 **B4 侦察的族 2**（默认值匿名函数的 NamedEvaluation）与 **B15b 残留 1**（标签早错误）。

---

#### B18 `var` 的函数作用域与提升 —— 已完成（2026-09-26）

**起点**：通过 14487 / 18277（79.26%），失败 3790，跳过 8133。

**怎么发现的**：B17 之后按残留继续查"生成器闭包捕获"，先用探针描形状，结果第一行就撞上另一个根因：

```js
{ var a = 1; }   a;      // node: 1；修前：ReferenceError: undefined variable: a
```

**根因（三个独立缺陷）**

1. **`var` 被当成块作用域**：`lower_variable_declaration` 走的是 `SymbolTable::insert`（当前块），
   块一结束名字就没了。`var` 属于**函数**（脚本则是全局），必须落到函数作用域。
2. **没有前置提升**：`var` 声明语句执行前名字应当已存在（值是 `undefined`），
   `typeof later; var later = 1` 才是 `undefined` 而不是 `ReferenceError`。
3. **块内遮蔽脚本级名字失效**：`lower_identifier` 只要名字在 `global_names` 里就走 `LoadEnv`，
   于是 `let x = 1; { let x = 2; }` 里块内读到的仍然是外层那个 1。

另外顺带发现：循环头的 `let`/`const` 会泄漏到循环之外（`for (let i …) {} typeof i` → `number`）。

**改动**（`symbol.rs` + `lowering/mod.rs`）

- `SymbolTable`: 新增 `insert_at(scope_index, …)`、`lookup_at`、`lookup_depth`、`scope_count`；
- 降级器新增 `var_scope`（该函数的作用域层）与 `var_binding`（当前是否在绑定一个 `var` 模式）、
  `publishes_as_global()`（脚本级 `var` 即使在块里也必须发布进全局环境 —— 少发布一次而读取仍走
  `LoadEnv`，就是 `for (var [c = 23] = [undefined]; …)` 读出 `undefined` 的原因）；
- `lower_variable_declaration` 按 `decl.kind` 分流：`var` 进 `var_scope`，`let`/`const` 留在当前块；
- 新增 `hoist_var_bindings` + 语句级收集器 `collect_var_names`/`collect_binding_names`
  （递归进块/if/循环/try/switch/标签，**不**进嵌套函数），在 `lower_program` 与
  `lower_function_inner` 各调一次；
- **已提升的 `var` 声明必须复用同一个槽位**（不能为同一个名字再 `alloc` 一个值）：两个独立的 SSA 值
  会让块后的读取落到一个分支从未喂过的 phi 上 —— `{ var a = 1; } a` 会变成 `undefined`；
- 循环头加作用域（`for`/`for-of`/`for-in`），`bind_for_of_left` 的 `var` 分支改走同一套 `var` 路径；
- `lower_identifier` 的全局环境快路径加条件：只有当**解析到的绑定就是脚本作用域那一层**时才走，
  否则读寄存器（修遮蔽）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14487 | **14549** | **+62** |
| 失败 | 3790 | 3728 | −62 |
| 通过率 | 79.26% | **79.60%** | +0.34pp |
| 单元 / feature / 护栏 | 190 / 468 / 7 | 190 / **473** / 7 | +5 条断言（`tests/features/variables.rs`） |

提升的套件（18 个）：`while` 18→25、`for` 318→326、`do-while` 19→26、`variable` 123→131、
`try` 135→140、`String` 609→617、`for-in` 75→77、`let` 111→113、`const` 108→110、`assignment` 388→390、
`function` 317→319、`types` 91→92、`block` 11→12、`continue` 18→19、`Array` 1941→1943、
`Object` 2762→2764、`Map` 181→182、`Boolean` 26→27。

**逐套件核对：2 处回退，逐条解释（§7.1 要求）**

1. `language/statements/for-of` 571 → 570：新增 2 条失败（`break-from-try`、`continue-from-try`）、
   新修好 1 条（`head-let-destructuring`）。**底层是一个既有缺陷** —— "从 `try` 内 `break` 跳出"在
   *基线*上就是坏的，与本批无关：

   ```js
   var log = [];
   for (var x of [1]) { try { log.push("a"); break; } catch (e) {} log.push("b"); }
   log.push("c");
   throw log.join(",");      // node: "a,c"；基线：返回 1（跳到脚本外面，后面的语句根本没跑）
   ```

   用 stashed 基线二进制跑同一段也是错的（已确认）。`var` 提升改变了槽位/寄存器分配之后，
   这两条 test262 用例才踩上它。**这不是"接受的回退"，而是下一批 B19 的目标。**
2. `language/statements/class` 1122 → 1121：`name-binding/in-extends-expression-assigned`
   （`var x = (class x extends x {});` 期望 `ReferenceError`，现在给 `TypeError`）。
   原因是 `var` 提升让外层 `x` 变成"已声明但未初始化"，而规范在这里靠**类名绑定的 TDZ** 抛
   `ReferenceError` —— 引擎还没有 TDZ。修 TDZ 会一并修好 `let`/`const` 的 TDZ 族
   （`for-of/dstr/*-put-let` 那 19 条"Expected a ReferenceError"同源）。

**残留**（下一批的输入）

1. **B19：`try` 内 abrupt 完成**（上面第 1 条）。控制流/SEH 的正确性问题，优先级最高：
   最小复现在基线上就失败，且它正遮挡 B18 的 2 条用例。
2. **TDZ**（`let`/`const`/类名绑定）：`for-of` 套件里 19 条"Expected a ReferenceError"、
   `*-put-let`/`*-put-const` 一族、以及上面的 class 用例都指向它。
3. **per-iteration 绑定**（ES6 `CreatePerIterationEnvironment`）：循环头 `let` 的闭包应当每轮
   一个新绑定（`closure-per-iteration=2,2`，应为 `0,1`）。B18 只做到"不泄漏"，没做"每轮新绑定"。
4. **闭包共享可变单元**（B18 起点想查的那件事）：`var` 捕获仍是创建时快照，
   `function m(){ var i = 0; return function () { i = i + 1; return i; }; }` 的 `m()()` 仍是 `1,1`
   （应为 `1,2`）；生成器的 `SuspendedFrame::closure_maps` 切片口径也在这条线索上。

---

#### B19 `try` 内的 abrupt 完成 —— 已完成（2026-09-26）

**起点**：通过 14549 / 18277（79.60%），失败 3728，跳过 8133。

**根因（两处，缺一不可）**

1. **跳板块被当成死代码裁掉**。`break`/`continue` 在 `try` 里会发一条 `DelayedJump` 指向一个
   trampoline 块，但 CFG 的连边只在 `try` **有 `finally`** 时才加（`pending_exits` 从 finally
   连到跳板）。没有 finally 时跳板没有任何前驱 → codegen 整块裁掉 → 补丁后的目标地址指向指令流之外。
2. **绝对地址被当成相对跳转**。`DelayedJump` 的操作数经 codegen 打补丁成**绝对 PC**（`ResumeExc`
   的 finally 分支正是按地址消费它），但"没有 finally 要跑"的分支用的是 `jump_offset`（相对）。
   于是控制流跳到了指令流末尾之外，程序直接"掉出末尾"—— 后面的语句一条都不执行，这也是
   B18 的 `for-of/break-from-try`、`continue-from-try` 两条用例被它遮挡的方式。
   同一分支还没有弹出被跳过的 SEH 记录（finally 分支由 `ResumeExc` 逐层弹，这里没人弹）。

最小复现（**基线上同样失败**，不是 B18 引入的）：

```js
var log = [];
for (var x of [1]) { try { log.push("a"); break; } catch (e) {} log.push("b"); }
log.push("c");
throw log.join(",");      // node: "a,c"；修前：返回 1（后面的语句根本没跑）
```

**改动**（`lowering/mod.rs` + `vm/mod.rs`）

- `SehFrameInfo` 增加 `exit_edges: Vec<(发出块, 跳板)>`：`lower_loop_exit` 在登记 `pending_exits`
  时一并记录发出块；`lower_try` 按 `finally_blk.unwrap_or(发出块) → 跳板` 连边 —— 有 finally 时仍
  从 finally 连（数据流本来就经过它），没有时从发出块连。
- `Opcode::DelayedJump` 的无 finally 分支改用绝对跳转 `state.jump(offset)`，并弹出
  `seh_depth` 条 SEH 记录。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14549 | **14555** | **+6** |
| 失败 | 3728 | 3722 | −6 |
| 通过率 | 79.60% | **79.64%** | |
| 单元 / feature / 护栏 | 190 / 473 / 7 | 190 / **478** / 7 | +5 条断言（`tests/features/control_flow.rs`） |

逐套件：`language/statements/try` 140 → **144**、`language/statements/for-of` 570 → **572**
（后者的 +2 正是 B18 记录里标为"既有缺陷所致"的那两条），**无逐套件回退** ——
B18 遗留的 `class` −1（TDZ）之外再无回退。

与 node 逐项对比过的形态（9 个，全部一致）：`break`/`continue` 出只有 catch 的 try、
带 finally 的 break/continue（finally 都跑了）、标签 break 出 try、嵌套 try、`return` 出 try、
try 内 throw 被 catch（循环继续），以及 while / do-while 里的 break。

**残留**

1. **TDZ**（`let`/`const`/类名绑定）：`for-of/dstr/*-put-let` 那 19 条"Expected a ReferenceError"
   与 B18 遗留的 `class/name-binding/in-extends-expression-assigned` 同属一族，量最大、收益最直接。
2. **per-iteration 绑定**（ES6 `CreatePerIterationEnvironment`）：循环头 `let` 的闭包应每轮一个新绑定
   （`closure-per-iteration=2,2`，应为 `0,1`）。
3. **闭包共享可变单元**：`var` 捕获仍是创建时快照；生成器 `SuspendedFrame::closure_maps` 的切片口径
   也在这条线索上。
4. `try` 里 `yield*` 与 delegate 的 abrupt 交互（B18 的 `*-from-try` 里 `yield-from-try` 已通过，
   但更深的组合还没专门验证过）。

---

#### B20 TDZ：`let`/`const`/类名的"已绑定但未初始化"窗口 —— 已完成（2026-09-26）

**起点**：通过 14555 / 18277（79.64%），失败 3722，跳过 8133。

**根因**：`let`/`const` 只有在其**声明语句被降级时**才进入符号表，于是声明之前的读取解析到外层
（或全局环境）拿到 `undefined` —— 既不抛 `ReferenceError`，也不遮蔽外层同名绑定。类名同理：
`class x extends x {}` 的 heritage 读到的是外层那个 `var x`（B18 提升后是 `undefined`），于是报
`TypeError` 而不是规范要求的 `ReferenceError`。

规范里这段窗口叫 TDZ：绑定**存在**（所以它遮蔽外层同名），只是还没有值，读写都要抛 `ReferenceError`。

**改动**（`symbol.rs` + `lowering/mod.rs`）

- `Variable` 从 `Variable(slot)` 变成 `{ slot, initialized }`，`Variable::uninitialized` 表示在死区；
  `SymbolTable<Variable>::mark_initialized` 结束这个窗口；
- 每个作用域入口**预声明**本层的 `let`/`const`（脚本、函数、块、循环头）：`hoist_lexical_bindings`
  + 收集器 `collect_lexical_names`（不进嵌套块 —— 每块自己预声明）；
- 读取（`lower_identifier`）、写入（`store_into_identifier`）、普通/复合赋值、`++`/`--`
  四处检查 `initialized`，否则发 `throw ReferenceError`；
- 类名走**独立作用域**：`lower_class` 在求值 heritage 之前把类名绑为未初始化，类构好之后标记并弹栈 ——
  这样它既遮蔽外层同名（关键：`var q = class q extends q {}`），窗口又覆盖 heritage 与类体。

两个必须记的细节（都是踩过的）：预声明与"复用已声明槽位"的查名**只能查当前层**。用整条作用域链查会把
块级 `let` 当成"外层已有同名"而跳过预声明，接着声明又去复用外层的槽位 —— 结果 B18 刚修好的
`let x = 1; { let x = 2; }` 又变回 1（`shadow=1`）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14555 | **14564** | **+9** |
| 失败 | 3722 | 3713 | −9 |
| 通过率 | 79.64% | **79.68%** | |
| 单元 / feature / 护栏 | 190 / 478 / 7 | 190 / **482** / 7 | +4 条断言（`tests/features/variables.rs`） |

逐套件：`for-of` 572 → **576**、`assignment` 390 → **394**、`class` 1121 → **1122**
（最后这 +1 正是 B18 记录里标为"需要 TDZ"的 `name-binding/in-extends-expression-assigned`），
**无逐套件回退**。

与 node 逐项对比过的形状（8 个，全部一致）：块内 `typeof` / 读取 / 赋值 / `+=` / `++` 命中死区、
遮蔽时读内层未初始化的名字、`{ x } = …` 赋给后面才声明的 `let`（test262 用的形状）、声明之后正常、
`class q extends q {}`、普通类与 `extends` 正常。

**残留**

1. **跨函数的脚本级 `let` TDZ**：`let y` 在脚本层，某个函数在其声明前读 `y` —— 本站给 `undefined`，
   规范要求 `ReferenceError`。原因是脚本级 `let` 被发布成了全局环境里的属性，闭包经 `LoadEnv`
   读到的是那个属性，没有死区信息。真正的修法是把"脚本声明记录"与"全局对象"分开 —— 属于数据模型债，
   量不小，建议与闭包共享单元一起做。
2. **per-iteration 绑定**（ES6 `CreatePerIterationEnvironment`）：循环头 `let` 的闭包应每轮一个新绑定
   （`closure-per-iteration=2,2`，应为 `0,1`）。
3. **闭包共享可变单元**：`var` 捕获仍是创建时快照；生成器 `SuspendedFrame::closure_maps` 切片口径
   也在这条线索上。第 1 条做完之后这两条会一起顺。

#### B21 脚本声明记录与全局对象分离 —— 已完成（2026-09-27）

**起点**：通过 14564 / 18277（79.64%），失败 3713，跳过 8133。

**根因（B20 残留 1，也是数据模型债的第一件）**：脚本层的 `let`/`const`/`class` 一直是
**用 `define_global` 发布进全局环境**的 —— 与 `var` 同一个 `HashMap`。这违反规范的
GlobalEnvironmentRecord 结构（它有 **ObjectRecord + DeclarativeRecord 两个分量**），
后果有三条：闭包经 `LoadEnv` 读到的是"全局属性"，**没有死区信息**（B20 残留 1）；
`globalThis.x` 本应是 `undefined` 却能看到 `let x`；闭包里的读取还会走
`closure_var_stack` 的**创建时快照**，脚本级 `let` 的跨函数写入看不见。

**设计**（照规范的两个分量拆开，三条新机制）：

| 机制 | 作用 |
|------|------|
| `DeclareLexical` 指令 | 脚本入口登记脚本级词法名 → 声明记录里一条**未初始化**项（死区） |
| `InitLexical` 指令 | 声明自身的初始化 —— 死区唯一允许的写（`InitializeBinding`） |
| `State::resolve_env_name` | `LoadEnv` / `StoreEnv` / `TypeOfEnv` **统一**的解析链：声明记录 → 闭包快照 → 全局对象 |

规范依据：`InitializeBinding` 与 `SetMutableBinding` 是两回事（前者结束死区，后者在死区里抛
`ReferenceError`）；`typeof` 必须在死区抛错、只在**未解析**时才答 `"undefined"` —— 两条链必须同源，
否则 `typeof C` 在方法里得到 `"undefined"`（实测就是这样）。

降级侧四处改动：`collect_lexical_names` 开始收集 `class` 名字（类名同样不是全局属性）；
`hoist_lexical_bindings(…, declare_in_script_record)` 只在脚本层发 `DeclareLexical`；
`define_global` 分流（脚本词法名 → `InitLexical`，其余 → `StoreEnv`）；嵌套函数的符号表
**剔除脚本词法名**（剔除 `global_names` 的同一处理）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14564 | **14567** | **+3** |
| 失败 | 3713 | 3710 | −3 |
| 通过率 | 79.64% | **79.70%** | +0.06pp |
| 单元 / feature / 护栏 | 190 / 482 / 7 | 190 / **483** / 7 | +1 用例（6 条断言） |

逐套件（`./scripts/phase2-status.sh`）：`language/statements/let` 113 → 114、
`built-ins/Object` 2764 → 2766，**其余持平、无回退**。另对
`language/statements/{let,const,class}` 做了路径级 diff：修好 1（
`let/global-closure-set-before-initialization.js` —— 正是 B20 残留 1），零新失败。

**四条踩出来的规矩**（都写进代码注释）

1. **声明自身的初始化不能走 `StoreEnv`**：会被自己刚立的死区规则拒绝（`let x = 1` 自抛）。
   规范早就把这两件事分开，编译器侧也要分成两条指令。
2. **`init_lexical` 要传"刚赋好值的槽位"，不能传另一个块里的 SSA 值** —— 后者会落到一个
   没人喂过的 phi 上（与 B18 提升 `var` 槽位时的坑同源）。
3. **SSA 重命名不能偷懒**：`InitLexical` 的 `value` 是*使用*，漏了重命名让 codegen 写进
   未写过的寄存器 —— 表现为 `class Ok {}` 之后 `typeof Ok === "undefined"`。
4. **嵌套函数必须把脚本词法名从符号表剔除**：脚本层的预绑定是"死区标记"，泄漏进方法体后
   **每个 class 方法读自己的类名都抛 `ReferenceError`**（实测让 class/let/const 三套件一次掉 81 条）。

**残留**（下一批的输入）

1. **函数级的闭包共享可变单元**（B19 残留 + 本条之后的最后一块）：脚本记录已经能跨函数共享，
   但**函数内**的 `var`/`let` 捕获仍是创建时快照 —— 探针 `counter()` 三次调用仍是 `1,1,1`。
   它与 per-iteration 绑定（`closure-per-iteration=2,2` 应为 `0,1`）是同一处机制，
   生成器 `SuspendedFrame::closure_maps` 的切片口径也在这条线上。
2. **NamedEvaluation**（B15b 侦察的第 2 族）：`*fn-name-*` 测试文件 **1056 个**，
   `dflt-*-elem-id-init-fn-name-{arrow,cls,cover,fn,gen}` 五族各 11 条 —— 规范要求
   `[x = function () {}] = []` 里的函数名取自绑定名。这是范围内失败里**最集中的一族**，排为 B22。

   同批做的侦察（探针形状与 node 逐项对比）：**整族缺失**，连最普通的
   `var f = function () {}` 都是 `<anonymous>`。七个位置全错、无一例外：

   | 形状 | 现状 | node |
   |------|------|------|
   | `var f = function () {}` | `<anonymous>` | `f` |
   | `[a = function () {}] = []` | `<anonymous>` | `a` |
   | `({ o = function () {} } = {})` | `<anonymous>` | `o` |
   | `function p(x = function () {}) {}` | `<anonymous>` | `x` |
   | `[c = class {}] = []` | `<class>` | `c` |
   | `[g = function* () {}] = []` | `<anonymous>` | `g` |
   | `[arrow = () => {}] = []` | `""` | `arrow` |

   落点：`bind_pattern` 的 `AssignmentPattern` 分支把默认值交给 `eval_default_on`
   （`self.eval_default_on(&[is_undef], &ap.right, result.clone())`），**不带绑定名**；
   `lower_variable_declaration` / 赋值表达式 / 形参绑定同样没有名字提示。
   `lower_function_inner(Some(name), …)` 只设**自身**声明名，缺的是"给一个匿名表达式补名"
   （`SetFunctionName`）这条原语 —— 一个 helper + 各位置传入名字提示即可。
3. `let`/`const` 套件剩余的 `dstr/*` 失败几乎全是上面这条；此外 `engine panic: SuspendedYield
   without a frame`（`ary-ptrn-*-step-err`，20 条）是生成器债，与 B6 一起看。

---

#### B22 NamedEvaluation —— 已完成（2026-09-27）

**起点**：通过 14567 / 18277（79.70%），失败 3710，跳过 8133。

**根因**：**整族缺失**。ES `NamedEvaluation` 要求匿名的函数/类在"落进一个有名字的地方"时取那个名字
（`var f = function () {}` 的函数名是 `f`），而引擎从不这样做：`lower_function_inner(Some(name), …)`
只设*自身*声明名，没有任何"给匿名表达式补名"的原语。七个位置全错：变量初始化、解构默认值
（`[a = function () {}] = []`）、对象模式默认值、形参默认值、赋值表达式、对象字面量属性值、类表达式。
另外匿名函数/类的 `name` 被填成占位名 `<anonymous>` / `<class>`，而规范要求**空串**。

**设计**

| 机制 | 作用 |
|------|------|
| `SetFunctionName` 指令 | (re)define 函数/类对象的 `name` 自有属性（`configurable: true` 正是规范允许这么做的原因） |
| `lower_expression_named(expr, hint)` | 只有**匿名形态**吃这个名字提示（具名函数表达式保留自己的名字），`hint` 为 `None` 时不做事 |
| `name_hint(Option<String>)` | 提示以**值**（`load_constant`）传递，因为计算键的名字要到运行时才知道 |

接线共 10 处：变量声明、`bind_pattern` 的两处 `AssignmentPattern`、赋值模式的三处
`AssignmentTargetWithDefault`、形参默认值、赋值表达式的三处标识符目标、对象字面量的 `Init` 属性。
名字提示只在"目标能借出名字"时给出（标识符、静态键）；成员目标、嵌套模式、复合赋值都给 `None`。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14567 | **14838** | **+271** |
| 失败 | 3710 | 3439 | −271 |
| 通过率 | 79.70% | **81.18%** | +1.48pp |
| 单元 / feature / 护栏 | 190 / 483 / 7 | 190 / **484** / 7 | +1 用例（12 条断言） |

15 个套件提升、**零回退**：`class` +49、`expressions/class` +49、`for-of` +27、`expressions/object` +26、
`for` +18、`generators`（表达式侧）+15、`expressions/function` +13、`assignment` +13、
`function` +12、`generators` +12、`variable` +9、`let` +9、`const` +9、`try` +6、`arrow-function` +4。
`guards` 计数 3 + 8 全部落在基线里本已失败的用例上（任何套件都没有下降，所以它们不可能是原先通过的）。

**四条踩出来的规矩**

1. **`SetFunctionName` 的 `name` 操作数是*值*，不是立即数**。名字提示用 `make_constant` 造出来的是
   `Value::Constant`，codegen 当立即数发出去，运行时立刻报
   `cannot load value from immediate operand` —— 必须 `load_constant` 落到寄存器。
2. **计算键不能再次 `ToString`**。`lower_member_key` 已经做过 `ToPropertyKey`；提示里再来一次会让键的
   `toString` 跑两遍（feature 测试当场抓住 `k1,k1,k2,k2:123`，应为 `k1,k2:123`）。
3. **匿名函数/类的 `name` 是空串**，不是 `<anonymous>` / `<class>` 占位名。占位名来自
   `func_info` 用 `Name::to_string()`（Display 的兜底）取值；现在语义值走 `unwrap_or_default()`，
   调试输出仍保留占位名（两者本来就该分开）。
4. **隐式默认构造器也要分流**：`class {}` 的 `constructor() {}` 是引擎自己造的，它同样得按匿名/具名
   决定是否叫 `<class>`（漏了这一处，`(class {}).name` 依旧是 `<class>`）。

**残留**（下一批的输入）

1. **实例类字段整族缺失**（本批顺带确认）：`class K { f = 1; }` 之后 `new K().f` 是 `undefined`，
   `f = function () {}` / 箭头同理。静态字段是好的（`K.s` 能读到）。这正是 §3 里登记的
   `class-fields-public` 债（489 条），而它同时挡着 `fn-name` 族里"字段值取字段名"的一批。
2. **accessor 与 symbol 键的名字**：`get m` / `set m` 的 `name` 应分别带前缀，`{ [sym]: fn }` 应得
   `"[desc]"`；现在这两类都没有提示（`SetFunctionName` 只认字符串）。
3. 形参、解构默认值、对象字面量都已覆盖；`for`/`for-of` 头部与 `class` 方法体里剩下的失败
   回到 B15b 侦察的其余族（迭代器/rest 语义、对象模式长尾）。

---

#### B23 实例类字段 —— 已完成（2026-09-27）

**起点**：通过 14838 / 18277（81.18%），失败 3439，跳过 8133。

**根因**：字段初始化代码**只发给"有显式构造器"的类**。`lower_class` 给显式构造器传了
`instance_fields`，但**没有显式构造器时**它自己手搓的那个默认构造器（`constructor(...args) { super(...args); }`）
压根没拿到字段列表 —— `class K { f = 1 }` 因此一行字段代码都不执行，`new K().f` 是 `undefined`。
另外三处：

1. **派生类的字段被放在 `super()` 之前**：`this` 还没初始化，直接抛
   `ReferenceError: Must call super constructor in derived class before accessing 'this'`。
   规范里 `InitializeInstanceElements` 在 `super()` **返回后**才跑。
2. **默认构造器的符号表没做"外部绑定改走环境"**：`JSASTLower::new(&mut func_builder, symbols)`
   直接克隆外层表，于是字段里的脚本级 `var`（`class K { [key] = 1 }` 的 `key`）读到的是
   *外层帧的寄存器* —— 键成了某个无关对象，属性落成 `"[object Object]"`。
3. **计算键落在源码文本上**：`[key]` 之前走 `property_key_to_string`（AST 文本），
   于是 `k.kk` 是 `undefined` 而 `k.key` 是 1。

**设计**

| 改动 | 作用 |
|------|------|
| `emit_instance_field_inits(fields)` | 字段安装抽成方法：`lower_member_key` 求键（静态/计算统一）+ NamedEvaluation 提示 + `set_member` |
| `statement_calls_super(stmt)` | 派生类的发射点：语句里出现（直接包含）`super(...)` 就在其后安装字段 |
| 默认构造器分流 | 基类：体前安装；派生类：隐式 `super(...args)` 之后安装 |
| `nested_function_symbols(captured)` | 把 `lower_function_inner` 里那段"剔除环境绑定"抽出来，默认构造器**同源复用** |

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 14838 | **15082** | **+244** |
| 失败 | 3439 | 3195 | −244 |
| 通过率 | 81.18% | **82.52%** | +1.34pp |
| 单元 / feature / 护栏 | 190 / 484 / 7 | 190 / **485** / 7 | +1 用例（10 条断言） |

逐套件：`language/statements/class` 1171 → 1297、`language/expressions/class` 1048 → 1166，
**其余持平、无回退**。

与 node 逐项对比 12 个形状，**11 个一致**：字段顺序与 `this` 互引、无初始值字段（`undefined` 且
可枚举）、自有属性 vs 原型、描述符 `writable/enumerable/configurable` 全 true、派生类（显式
`super` 与隐式默认构造器）、箭头字段捕获实例 `this`、函数字段取字段名、静态字段不受影响、
`for` 里 `Object.keys` 顺序。唯一的差异是**计算键被按实例重复求值**（`key-evals=2`，应为 1）。

**四条踩出来的规矩**

1. **"有显式构造器"和"没有"是两条产生路径**，任何挂在构造器上的语义（字段、`arguments`、
   参数绑定）都要在两条路径上都接一遍 —— 默认构造器是手搓的，最容易漏。
2. **派生类的 `this` 在 `super()` 之前不可碰**：字段安装点必须跟着 `super()` 走，
   否则不是"字段没装"而是直接抛错（更响、也更难看出是同一件事）。
3. **手搓的嵌套函数也要用同一套作用域规则**：默认构造器克隆了裸符号表，字段里的脚本级 `var`
   读到外层寄存器，表现为"键变成 `[object Object]`"——症状离根因很远。现在统一走
   `nested_function_symbols`。
4. **计算键是运行时值**：退回源码文本能"看起来装上"，但属性名是错的。

**残留**（下一批的输入）

1. **计算键应在类定义时求值一次**（`CreateClassFieldDefinitions`）：现在按实例重复求值。
   要修得把键值存到运行时（隐藏数组或闭包捕获）—— 与"字段初始化器看不见构造器形参"是同一类
   "字段有自己的作用域与时刻"的问题。
2. **私有字段**（`#x`，`class-fields-private`）：完全未做，是 `class` 两套件里剩下的主要一块。
3. `super()` 埋在更大表达式里时，字段的发射点仍是旧行为（会跟着第一个含 `super` 的语句走）。

---

#### B24 生成器的两处生命周期漏洞 —— 已完成（2026-09-27）

**起点**：通过 15082 / 18277（82.52%），失败 3195，跳过 8133。

**根因一：方法调用路径漏了"生成器"判定。** `Call` 指令、`invoke`、`New` 都检查
`module.generators`，但 `CallMethod` 的尾部自己搭帧（`enter_frame` + `jump`），**没有**这个检查 ——
于是 `class C { *m() {} }` 的 `new C().m()` 会**立即执行生成器体**：它的 `Ret` 返回进了*调用者的*帧，
整个模块因此提前结束（脚本的返回值成了那个生成器对象），`it.next()` 是 `undefined`。
这是 `class` 两套件里最大的一块：32 条 `engine panic` + 56 条 `Cannot read properties of undefined
(reading 'value')`。

**根因二：抛出后的生成器没有收尾。** 生成器体让异常逃出时，几处 `run_generator_frame(...)?` 直接
`?` 返回，`finish_generator` 没跑 —— 生成器停在 `Executing` 状态。之后任何 `next()` 都会落到
"resume at a yield" 分支，而那里没有挂起的帧，`expect("SuspendedYield without a frame")`
**直接 abort 进程**（CLI 里就是整轮结束；runner 里被 `catch_unwind` 兜成一条 `engine panic:` 失败 ——
B25 的审计确认当前全量里已无 `engine panic`）。触发它的是 test262 里极常见的一行：
`iter.next()` 抛错之后测试再调一次 `iter.next()`。

**改动（两处，约 20 行）**

| 改动 | 作用 |
|------|------|
| `CallMethod` 尾部先查 `callable_func_id(...)` + `module.generators` | 生成器方法只建对象，和 `Call`/`invoke` 一致 |
| `run_generator_frame_or_complete(...)` | 帧里逃出异常时把生成器标记为 Completed（参数前导那条路径单独处理） |
| 兜底 | `expect("SuspendedYield without a frame")` → `TypeError`，异常不再升级成进程 abort |

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15082 | **15204** | **+122** |
| 失败 | 3195 | 3073 | −122 |
| 通过率 | 82.52% | **83.19%** | +0.67pp |
| 单元 / feature / 护栏 | 190 / 485 / 7 | 190 / **487** / 7 | +2 用例（8 条断言） |

14 个套件提升、**零回退**：`statements/class` +37、`expressions/class` +32、`expressions/object` +13、
`for-of` +6、`for` +6、`function` +4、`generators` +4、`variable` +2、`try` +2、`let` +2、`const` +2、
`expressions/function` +4、`expressions/generators` +4、`arrow-function` +4。

**两条踩出来的规矩**

1. **"哪个调用路径"比"什么函数"更容易漏**：同一件事（生成器只建对象）在 `Call`/`invoke`/`New` 里
   都做了，唯独方法调用自己搭帧的那一段没做。新增任何"自己搭帧"的调用路径，都要先问一遍
   "生成器怎么办、箭头函数的 `this` 怎么办、捕获变量怎么办" —— 最后干脆把取 `func_id` 抽成
   `callable_func_id` 复用。
2. **`expect` 只该用在"真的不可能"上**：生成器状态机在这里并不可靠（抛出路径会漏状态），
   一个 `expect` 就把"一条测试失败"变成"整轮回归 abort"。凡是能从外部数据（生成器状态）推出来的
   条件，都该给一条普通的错误路径。

**残留**（下一批的输入）

1. 生成器与 **`try`/`finally` + `yield`** 的交互（`delegate_stack`、`generator_return`）仍是细活；
   本批只处理了"抛出后收尾"。
2. `class` 两套件里剩下的主要是**私有字段**（`#x`、`#m() {}`）与**早期错误**（36 条
   `Expected a SyntaxError`：重复 `constructor`、`#x` 重名、`super` 用在字段初始化器等）。

---

#### B25 `Array.prototype` 的泛型（array-like）语义 —— 已完成（2026-09-27）

**起点**：通过 15204 / 18277（83.19%），失败 3073，跳过 8133。

**背景**：`Array.prototype.<m>.call(x, …)` 有 **1215** 个测试文件，其中 123 条当前失败。
泛型机制（`array_like_entries`）本来就在 VM 里，三个细节让它在这条路径上不生效：

1. **`length` 用 `internal_get` 读**，不触发原型链上的**访问器**。于是
   `Array.prototype.indexOf.call(new Con(), true)`（构造器的 prototype 定义 `get length()`）看到的
   长度是 0，`entries` 直接是空的。
2. **`receiver_is_array_like` 只认对象**（字符串除外）。原始值接收者（`reduce.call(false, …)`）被判成
   "不是 array-like" 而落到只认真数组的 builtin。
3. **回调的第三个参数传的是原始接收者**，而规范要求 `O` —— `ToObject(this value)`。
   测试正是用 `obj instanceof Boolean` 检查这一点。

**改动（三处）**：`length` 改走完整的 `[[Get]]`（`get_member`）；`receiver_is_array_like` 对原始值先
`ToObject` 再判；回调第三个参数改用 `callback_receiver`（`reduce` 分支自己那一处也一并改）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15204 | **15306** | **+102** |
| 失败 | 3073 | 2971 | −102 |
| 通过率 | 83.19% | **83.74%** | +0.55pp |
| 单元 / feature / 护栏 | 190 / 487 / 7 | 190 / **488** / 7 | +1 用例（6 条断言） |

逐套件：`built-ins/Array` 1943 → 2045，**其余持平、无回退**。

与 node 逐项对比 5 个形状全部一致：自有 `length` 的 array-like、**继承的 `length` 访问器**、
`Boolean.prototype` 上的元素与长度、`forEach`/`map` 的 array-like 迭代、字符串接收者
（`Array.prototype.indexOf.call('ab', 'b')` 仍是子串语义 1，而不是逐字符扫描 0）。

**三条踩出来的规矩**

1. **`internal_get` 与 `[[Get]]` 不是一回事**：前者不走访问器（也不走完整的原型链语义），
   在"读用户可见的属性"处用它，症状是"数组看起来是空的"。凡是按规范算法读属性，都该走
   `get_member`。
2. **`ToObject` 是算法的一部分**，不只是"防御性包装"：原始值接收者要经过它才有 `length`/索引，
   回调看到的第三个参数也必须是它。
3. **同一个语义有多条调用路径**（`CallMethod` 快路径 / `invoke` → `call_native_by_name` /
   builtin 层）。修一条不代表修了全部：这次的 `reduce` 分支就自带了第二处回调调用，
   漏掉它则 probe 只对一半。

**残留**（下一批的输入）

1. `Array.prototype` 类里剩下的失败以**其余方法**为主（`sort` 33、`flat` 18、`splice` 13 等，
   其中 `flat`/`toSpliced` 是后 ES6）；`length` 的 `ToLength` 细节（`-0`、超长 `2^53-1`）仍需逐条核对。
2. `language/expressions/arrow-function/dstr/*`（115 条）——多为**闭包捕获债**（计数器不动），
   与生成器 `closure_maps` 同一处机制。

---

#### B26 侦察：闭包捕获的现状与下一步 —— 未提交（2026-09-27）

本批只落了**中性接线**：VM 侧的 `MakeFuncObj` 已经会收 `ClosureVar`（有捕获时新建对象而不是复用
按 `func_id` 记忆化的那个），`MakeArrowFuncObj` 与它共用 `take_pending_captured_vars`。
降级侧**没有**接上（函数表达式不捕获那半被回退，见下），所以全量与 B25 **逐套件完全一致**
（15306 / 2971，零回退、零收益）—— 接线在那里等着，真正的收益要等共享单元落地。
把"闭包共享可变单元"这团债拆清楚了，下一批可以直接按这份结论开工。

**症状**（都能一条命令复现）

```js
function counter() { var i = 5; return function () { return i; }; }  counter()()      // 引擎: undefined（node: 5）
function counter() { var i = 0; return function () { i = i + 1; return i; }; }        // 引擎: 0,0,0（node: 1,2,3）
function outer() { var n = 1; function inner() { return n; } return inner() + n; }    // 引擎: NaN（node: 2）
for (let i = 0; i < 3; i = i + 1) fns.push(function () { return i; });                // 引擎: 2,2,2（node: 0,1,2）
```

**根因分三层，代价递增**

1. **函数表达式与函数声明根本不捕获**（最容易，也是最大的一块）。捕获只有箭头那条路径做了：
   `lower_arrow_function` 算自由标识符 → 发 `ClosureVar` → `MakeArrowFuncObj` 收集。
   而 `lower_function_expr` 传 `&[]`、返回裸 `Value::Function`（靠 `as_object_value` 惰性物化，
   物化走的是按 `func_id` 记忆化的 `materialize_function`，**根本不带捕获**）；
   `collect_function_declaration` 同样传 `&[]`。VM 侧 `Opcode::MakeFuncObj` 也没有
   `MakeArrowFuncObj` 那段"收 `ClosureVar`"的代码。
   → 修法已探明：算自由标识符 + `emit_closure_vars` + 显式 `make_func_obj`（有捕获时**新建**对象，
   不能复用记忆化的那个）；VM 侧把收集逻辑抽成 `take_pending_captured_vars` 给两条路径共用。
2. **捕获是"创建时快照"，不是共享单元**。即使第 1 层修好，`i = i + 1` 写的仍是内层那份拷贝
   （实测 `counter=0,0,0`：内层读到 0、写进自己的 map，外层寄存器纹丝不动）。
   真正的修法是**共享可变单元**：被捕获的绑定在**两侧**都变成 `cell`（`LoadCell`/`StoreCell`），
   与 B21 给脚本层做的"声明记录"同构 —— 区别只是作用域从脚本换成函数。
3. **函数声明连"共享单元"都救不了**：它的闭包在**外层函数体执行之前**（提升时）创建，
   而规范要求它捕获的是*绑定*不是*值*。快照模型在这里必错（会冻结声明前的值），
   必须等第 2 层的 cell 落地。

**为什么这一批回退了**：把第 1 层按"箭头同款"接上之后，`arrow-function` +27，
但 `expressions/function` **−94**、`statements/function` −10。做减法的过程中修掉两个真实错误
（脚本级 `var`/`let` 等**环境绑定不该被捕获** —— 快照会冻结副本，而
`var callCount = 0; var f = function () { callCount += 1; }` 是 test262 里最常见的形状；
`arguments` 不该被当成外部自由变量 —— 否则函数自己的 `arguments` 绑定被剔掉），
但剩余 −19 的根因（16 条 `Cannot read properties of undefined (reading 'Symbol(...)')`
集中在**参数默认值 + 解构**）没能在本轮定位。按"零回退"纪律，本批**全部回退**，
工作区停在 B25 的良好状态（15306 / 82.74%… 见 A8）。

**下一批的建议顺序**

1. 先做第 2 层（**共享 cell**）：它同时解决 `counter` 的写、`for (let …)` 的 per-iteration 绑定、
   以及生成器 `SuspendedFrame::closure_maps` 的切片口径 —— 这三条本来就是同一处机制。
2. 第 1 层（函数表达式/声明的捕获）作为 cell 的接线一起做，**不要**单独上快照版本：
   那只会把"读到 undefined"换成"读到创建时的旧值"，而且会像本批一样碰到 −94 那类回归。
3. 第 3 层（提升的函数声明的捕获时机）在第 2 层之后自然成立。

---

#### B27 设计稿：共享 cell（待执行）—— 2026-09-27

B26 的侦察给出了三层根因与顺序建议；这份稿子是**动手前**的接口约定，避免边写边试。

**目标契约**：闭包与它的外层对同一个函数级绑定**读写同一处存储**。等价地：一条绑定只要可能
"活得比栈帧久"（被内层函数引用），它的值就必须放在 cell 里，两侧都通过 cell 访问。

**1. cell 的表示**

一个"单值盒子"，用户代码看不见（cell 只存在于闭包捕获表与寄存器里，从不作为 JS 值逃逸）：

- `LoadCell(cell)` = `PropertyGet(cell, "__cell__")`
- `StoreCell(cell, v)` = `PropertySet(cell, "__cell__", v)`

不新增 `Value` 变体、不新增指令 —— 复用现成的属性读写；将来若要静态可查，再换成
`ObjectKind::Cell`。**注意**：cell 的键必须在任何用户可见枚举中不可达（本引擎的 cell 不逃逸，
所以普通对象即可）。

**2. 哪些名字需要 cell（预扫描）**

在降级一个函数体**之前**，先收集"本函数体内任意嵌套闭包（箭头/函数表达式/函数声明）引用到的
自由标识符"（`collect_free_idents_from_body` 已经能算单个函数的自由标识符，需要加一层遍历
所有嵌套函数节点的走查）。落在**本函数作用域内**的那些名字 → 该绑定为 cell-backed。

必须预扫描而不能"边降级边升级"：外层函数体的指令在那之前就可能已经发过（读的是裸寄存器），
事后升级只会让前后不一致（B26 实测过 −94 的形态）。

**3. 降级侧的接入点**

| 位置 | 变化 |
|------|------|
| `Variable` | 加 `cell: bool` 字段 |
| 绑定的**创建**（`var` 提升、参数绑定、`let`/`const` 声明、解构绑定、函数声明名） | cell-backed 时先 `make_object()` 再 `StoreCell(初始值)`，寄存器里存 **cell** 而不是值 |
| `lower_identifier` | cell-backed 时读 cell：`LoadCell(寄存器)` |
| `store_into_identifier` / `lower_assignment` 的三处标识符目标 / `bind_pattern` 的初始化 | cell-backed 时写 cell：`StoreCell(寄存器, 值)` |
| `emit_closure_vars` | cell-backed 的名字传 **cell**；环境绑定（脚本 `var`/脚本词法）继续**跳过**（它们本来就共享：`LoadEnv`/`StoreEnv`） |
| 内层函数读/写捕获名 | 读：`LoadCell(LoadEnv(name))`；写：`StoreCell(LoadEnv(name), v)`（`LoadEnv` 现在给的是 cell） |

`initialized`（TDZ）语义不变：cell 存在 ≠ 已初始化，读未初始化的 cell 仍抛 `ReferenceError`。

**4. 顺序与陷阱（B18/B20/B21 的教训）**

- cell 必须在**绑定点**分配，且闭包必须在之后创建（`ClosureVar` 传的是 cell 的寄存器值，天然满足）。
- 跨块传值仍要走槽位（B18）：不要把"另一个块里定义的 cell 寄存器"直接当操作数（phi 无人喂）。
- 提升的**函数声明**（B26 第 3 层）：它的闭包在函数体执行前创建 —— cell 之后这条自然成立，
  因为创建时传的是 cell 本身而不是当时的值。
- 与 B21 的脚本声明记录**同构**：最终模型统一成"活过栈帧的绑定 = cell"，
  脚本层已经是（`script_env` 的槽位），函数层是本批要补的。

**5. 落地前的门禁（B26 未解的回归）**

B26 把第 1 层按快照接上时，`expressions/function` 掉了 94。B27 的第 1 轮又实测了一次，
结论更精确：

- **14 条新失败**（路径级 diff 得到，全部在 `dstr/`），报错一律是
  `TypeError: Cannot read properties of undefined (reading 'Symbol(…)')` —— 即
  `GetMethod(undefined, @@iterator)`：**参数里的数组模式拿到了 `undefined` 作为源**。
  形状是 `f = function ([]) {…}` / `function ([,])` / `function ([...[]])`，且测试体内有
  `assert.sameValue`（见下）。
- **只在真实 harness 下复现**。把这些用例改写成等价的独立脚本（自建 `assert`、生成器实参、
  同样模式、同样的 `callCount += 1`）**全部通过** —— 触发点在 harness 自身的某个结构里，
  不在用例本身。
- 已排除的方向（逐个试过，都不成立）：模式本身（空/elision/rest）、生成器对象作实参、
  脚本级 `var` 的写入、函数声明的同名形状、`function ()` 与 `var f = function ()` 两种写法。

B27 的第 2 轮用**二分**把它钉死了（只跑 `language/expressions/function`，基线 218 通过 / 20 失败）：

| 接上的部分 | 通过 / 失败 | 结论 |
|-----------|------------|------|
| 只接"显式 `make_func_obj`"（捕获关掉） | 204 / **34** | 回归来自**这里** |
| 显式 `make_func_obj` + 捕获 | 206 / **32** | 捕获本身是 **+2**（把这 14 条中的 2 条修回来） |

也就是说：**触发点是"把函数表达式的值从裸 `Value::Function(id)` 改成显式物化的对象"**，
与捕获逻辑无关。失败形状（参数里的数组模式拿到 `undefined` 作为源）指向
**被装箱的 callee 那条调用路径在该形状下把实参丢了**（`GetMethod(undefined, @@iterator)`）。

**下一批的入口**：先对比 `Call`/`CallMethod` 里 `Value::Function(id)` 与 `Value::Object(FunctionObject)`
两个分支的实参收集（`enter_frame(arg_count)` + `[rbp-i-1]`），用一个"装载 harness 的 dstr 用例"
当靶子；两者等价之后，第 1 层才能安全接上。**不要**先接上再查——本批两轮都证明了那是无归因的回归。
顺带记一条：cell 落地后，捕获表可以挂在 **cell** 上（每次求值共享同一批 cell），
也许根本不需要"每次求值一个新函数对象"这条改动 —— 这条捷径要在实参路径查清之后再定。

**6. 验证方案（固定）**

```
probe A  function counter(){var i=0;return function(){i=i+1;return i;}}       → 1,2,3
probe B  function counter(){var i=5;return function(){return i;}}             → 5（读）
probe C  function outer(){var n=1;function inner(){return n;} return inner()+n;} → 2（含提升声明）
probe D  for (let i=0;i<3;i=i+1) fns.push(function(){return i;})              → 0,1,2
probe E  var c=function(){var n=0;return {inc:function(){n=n+1;},get:function(){return n;}};}
```
再加**路径级 diff**：把 `language/expressions/function`、`language/statements/function`、
`language/expressions/arrow-function`、`language/statements/for`、`language/statements/let`
五套件在"改动前/改动后"各跑一次，用 `comm` 对比失败清单（只允许"修好"方向的变化），
最后全量确认零回退。

---

#### B28 闭包捕获第一层 + 装箱 callee 的生成器判定 —— 已完成（2026-09-27）

**起点**：通过 15306 / 18277（83.74%），失败 2971，跳过 8133。

这一批是 B27 设计稿"门禁"清掉之后的第 1 层落地，两个根因：

**根因一：装箱 callee 的调用路径漏了生成器判定**（B26/B27 门禁的真凶）。

`Opcode::Call` 的 `Value::Function(id)` 分支有 `module.generators` 检查，而**`Value::Object`
分支没有** —— 于是"生成器被物化成对象"之后再调用，生成器体会**立即执行**、返回它（通常为空）的
`rv`。触发这条路径有两种现成写法：`var f = function* () {}; f()`（`MakeFuncObj` 物化）与
**从对象上取下生成器方法**（`var g = C.prototype.m;`）—— 与 B24 修的 `CallMethod` 是同一类漏洞，
只是换了个分支。

**定位过程有意思**：最初以为触发点在 harness（"只有装载 harness 才复现"），于是把
`sta.js + assert.js + 用例`拼成一个脚本脱离 runner 复现，结果发现**只拼 stub 就复现** ——
真正的条件是把用例二分出来的：`f(iter)` 里那个 `iter` 是生成器对象，而 `typeof iter` 本身就是
`undefined`。也就是说不是"实参丢失"，是**生成器对象根本没造出来**。

**根因二：函数表达式不捕获**（B26 三层根因的第 1 层，门禁清掉后安全落地）。

`lower_function_expr` 传 `&[]` 并返回裸 `Value::Function`（惰性物化走按 `func_id` 记忆化的
`materialize_function`，不带捕获）。现在：算自由标识符 → 发 `ClosureVar` → 显式 `make_func_obj`
（有捕获时**新建**对象，不复用记忆化的那个）。落地时带上了 B26 摸出来的两条修正：
**环境绑定（脚本 `var` / 脚本词法）跳过捕获**（它们本来就共享，捕获只会冻结副本 —— 这条差了
−94），**`arguments` 不算外部自由变量**（否则函数自己的绑定被剔掉）。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15306 | **15410** | **+104** |
| 失败 | 2971 | 2867 | −104 |
| 通过率 | 83.74% | **84.31%** | +0.57pp |
| 单元 / feature / 护栏 | 190 / 488 / 7 | 190 / **490** / 7 | +2 用例（7 条断言） |

**21 个套件提升、零回退**，最大的几个：`expressions/arrow-function` +33、`expressions/new` +11、
`statements/class` +8、`expressions/class` +8、`statements/function` +7、`statements/let` +6、
`for-of` +3、`generators`（两套件）+2、`expressions/function` +2。`guards` 里 timeout 由 3 升到 7
（逐套件无一下降，说明新增的 4 条都是基线里本已失败的用例）。

**两条经验**

1. **"只在 X 下复现"要先怀疑复现成本身**：`sta.js + assert.js + 用例` 这条拼装没有把 runner 的
   全部行为带进来，反而误导了一轮；把依赖剥离到"stub + 用例"才看见真条件。**最小化复现比猜测
   harness 里哪个结构更有效**。
2. **同一个语义的每个"自己搭帧"分支都要问一遍生成器**（B24 的规矩在 `Value::Object` 上再应验一次）。
   本批之后，`Opcode::Call` 的两个分支、`CallMethod`、`invoke`、`CallSpread` 都对生成器一致了。

**残留**（下一批的输入）

1. **写**的那一半仍是债：`function counter() { var i = 0; return function () { i = i + 1; return i; }; }`
   仍是 `0,0,0` —— 需要 B27 设计稿里的**共享 cell**（读那半已经好了）。
2. **提升的函数声明**仍不捕获（快照模型在它身上必错，等 cell）。
3. `statements/function` 剩下的 30 条集中在 `dstr/*` 与参数默认值族。

**B28 之后的下一批候选（本轮的侦察结果）**

`class` 两套件剩 270 条，构成：`dstr` 88、`elements` 74、`builtin-objects` 19、`definition` 17。
`elements` 里最大的一块（36 条 `Expected a SyntaxError but got a ReferenceError`）全部是
`*-indirect-eval-*` / `*-eval-*` 用例 —— 需要 `eval`，**范围外**，不要再花时间。

`dstr` 的 88 条集中在**生成器方法的形参**（`gen-meth-static-*` 28、`gen-meth-dflt-*` 16、
`meth-static-dflt-*` 8 …）。已定位到一个**可复现的独立缺陷**：

```js
var first = 0, second = 0, cc = 0;
function* g() { first += 1; yield; second += 1; }
var C = class { static *m([[,] = g()]) { cc = cc + 1; } };
C.m([]).next();
// node: cc=1 first=1 second=0     引擎: cc=2 first=2 second=0   ← 生成器体与默认值各跑了两次
```

已排除（都正确，不要再从这些方向找）：无参的类/对象/静态生成器方法、普通生成器函数、
方法 + 简单形参、方法 + 默认值形参、方法 + 数组模式形参。触发需要
**"嵌套模式里的默认值，且该默认值调用一个生成器"** 这个组合。
下一批从"这条最小复现为什么跑两遍"入手（怀疑点：生成器方法的*参数前导*与 `PrologueEnd`
屏障在这条组合上被越过；B17 的前导语义在方法路径上可能只对简单默认值成立）。

---

#### B29 生成器前导的可重入性 —— 已完成（2026-09-27）

**起点**：通过 15410 / 18277（84.31%），失败 2867，跳过 8133。

**根因**：`running_generator_prologue` 是一个**单个布尔量**，而"跑前导"这条路径是**可重入**的 ——
参数的默认值可以自己再创建一个生成器。B17 的设计里，创建生成器只跑到 `PrologueEnd` 屏障就停
（参数在那儿绑好、体留到第一次 `next()`）。但内层 `g()` 的前导跑完后把这个标志**无条件置回
`false`**，于是外层方法的屏障看到 `false`、什么都不做：

```
[prologue_end] pc=62 flag=true     ← g() 自己的前导（正常）
[prologue_end] pc=82 flag=false    ← 方法的屏障，标志已被内层踩掉 ✗
```

后果是**体在创建时就被执行了一遍**，`next()` 再跑第二遍：

```js
var first = 0, second = 0, cc = 0;
function* g() { first += 1; yield; second += 1; }
var C = class { static *m([[,] = g()]) { cc = cc + 1; } };
C.m([]).next();
// node: cc=1 first=1 second=0      引擎: cc=2 first=2 second=0
```

**改动**：`run_generator_prologue` 保存并**恢复**这个标志（而不是置回 `false`）—— 一行语义 + 注释。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15410 | **15452** | **+42** |
| 失败 | 2867 | 2825 | −42 |
| 通过率 | 84.31% | **84.54%** | +0.23pp |
| 单元 / feature / 护栏 | 190 / 490 / 7 | 190 / **491** / 7 | +1 用例（4 条断言） |

5 个套件提升、**零回退**：`statements/class` +12、`expressions/class` +12、`statements/generators` +6、
`expressions/generators` +6、`expressions/object` +6。

**定位过程与一条规矩**

- 先是用"路径级 diff"圈出族（`class/dstr` 88 条集中在生成器方法的形参），再用**最小化复现**
  把条件缩到"嵌套模式里的默认值 + 该默认值调用生成器"，最后在 `PrologueEnd` 上打一条
  `flag=` 的打印，一眼看见 `false`。**最小化复现 + 一条目标明确的打印**比猜快得多（本批从
  复现到根因只用了三轮）。
- **规矩**：VM 里任何"模式标志"（现在正跑前导、正跑委托、在某次调用边界内）都必须按
  **可重入**来写 —— 用户代码可以在该模式的中间再触发同一条路径（参数的默认值、getter、
  迭代器协议、`toString`）。单个布尔量要么保存/恢复，要么改成计数器/深度。

**残留**（下一批的输入）

1. `class/dstr` 还剩下约 76 条（本批减了 24），仍是生成器方法 × 形参那一块 —— 若还有同类症状，
   先怀疑**另一个**没被保存/恢复的模式状态（`generator_yielded` / `generator_yield_pc` /
   `delegate_stack` 深度）。
2. 写那半的闭包债（共享 cell）与提升函数声明的捕获，仍未动。

---

#### B30 转向「广度」—— 日常可用性（已完成，2026-09-28）

**方向调整**：此前十几批都是"就着一族 test262 往深里啃"。本批起改为**先铺面**：把日常脚本真正会
用到的表面补齐，让引擎能拿来写东西，而不是只对测试集好看。第一步是**实测**一份覆盖清单（不靠
文档，直接对着二进制问），再按「日常频率 ÷ 实现成本」排序。

**实测清单**（`typeof` + 方法探测，逐项跑出来的）

- **语言层**：`let/const/var`、class（实例/静态字段、getter、extends）、箭头、模板串、解构
  （数组/对象 + 默认值）、spread/rest（调用/数组/对象）、`for...of`（数组与 Map/Set）、生成器、
  `??`、`**`、计算键、getter/setter、`?.` —— 都在。**两处缺口**：带标签的模板串（`tag\`a${1}b\``
  返回 `null`）与 **`async/await`（能解析，但没有作业队列，语义未落地）**。
- **内建层**：Object（含 `keys/values/entries/assign/create/freeze/…`）、Array（除 `flat/flatMap`
  外常用方法齐）、String（除 `replace/match/search` 外齐）、Number、Boolean、Math（ES6 全）、
  JSON、Map/Set（含 ES2024 集合运算）、WeakMap/WeakSet、Symbol（含 well-known）、Error 家族、
  Function（call/apply/bind）。
- **没有的（这才是日常的拦路石）**：`console`、`Date`、`RegExp`、`Promise`、`globalThis`、
  `ArrayBuffer`/TypedArray、`Proxy`/`Reflect`、`encodeURI*`、模块系统、`e.stack`。
  `Intl` 判为范围外。

**本批改动**（挑"用得上且便宜"的四个面）

| 新增 | 说明 |
|------|------|
| `console.log/info/debug/trace/warn/error` | 宿主对象。之前**唯一的输出手段是 `throw`** —— 这一条就让日常脚本从"没法用"变成能用。字符串按原样打印、对象回落到 `JSON.stringify`；`warn/error` 走 stderr |
| `String.prototype.replace` / `replaceAll` | 仅**字符串**模式（正则还没进引擎）；函数式 replacer 明确报错而不是静默错渲染 |
| `Array.prototype.flat` | 深度判定**逐个元素**做（在入口判 `depth == 0` 会让 `flat()` 退化成 `flat(0)`）；洞被跳过 |
| `Object.fromEntries` | `Object.entries` 的逆；非对象条目是 TypeError |
| `encodeURI` / `encodeURIComponent` / `decodeURI` / `decodeURIComponent` | UTF-8 百分号编码，两套 unescaped 集合；畸形转义抛 URIError（用 `RuntimeError::Thrown` 构造真正的 URIError 值） |

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15452 | **15484** | **+32** |
| 失败 | 2825 | 2793 | −32 |
| 通过率 | 84.54% | **84.70%** | +0.16pp |
| 单元 / feature / 护栏 | 190 / 491 / 7 | 190 / **492** / 7 | +1 用例（13 条断言） |

3 个套件提升、**零回退**：`built-ins/String` +14、`built-ins/Array` +11、`built-ins/Object` +7。

**下一批（按同一把尺子排序）**

1. **`Date`** —— `Date.now()`、`new Date(...)`、getter/setter、`toISOString`。日常脚本里出现频率
   仅次于字符串处理，且是独立的一个 builtin 文件（不需要动 VM）。**推荐下一个做它。**
2. **`RegExp`** —— 顺带把 `String.prototype.replace/match/search/split` 的正则形式、`matchAll`
   接上。需要一处正则实现（可考虑引入 `regex` crate 做模式编译）。
3. **`Promise` + 微任务队列 + `async/await`** —— 需要一个作业队列（VM 侧），是三块里唯一要动
   执行模型的。
4. **小面一批**：`globalThis`（要有真正的全局对象，现全局是个 `HashMap`，脚本层 `this` 也因此
   不是全局对象）、`Array.prototype.flatMap`（回调要 VM 拦截，与 `map`/`filter` 同机制）、
   `e.stack`、数组字面量的 elision 应为**洞**（现在落成 `undefined`，于是 `[1,,2].flat()` 是
   `[1,null,2]` 而 node 是 `[1,2]`）、`ArrayBuffer`/TypedArray、`Proxy`/`Reflect`。

**一条规矩**：新增原型方法是**两处**（`set_prototype_method` 注册 + `call_prototype_method` 的
裸名分发）—— `set_prototype_method` 的闭包参数是 `_f`，**不会被调用**，只看名字。忘了第二处会静
默变成 `undefined`。

---

#### B31 `Date` —— 已完成（2026-09-28）

**决策**：B30 排的第一项。日常脚本里出现频率仅次于字符串处理，且是一个独立的 builtin 文件
（不需要动 VM 的执行模型 —— 只有 `new Date(...)` 的构造入口要接一下）。

**实现**

- `vm::object::DateObject`：普通对象 + `[[DateValue]]` 内部槽（毫秒数，`NaN` 是 Invalid Date）。
  槽必须放在专用类型上：`OrdinaryObject` 没有内部槽，存在属性里会让
  `Object.getOwnPropertyNames(new Date())` 不再为空。
- `builtins/date.rs`：构造器的四种形态（无参 / 时间值 / 日期串 / 年月日分量）、
  `Date.now` / `parse` / `UTC`、本地与 UTC 两组分量 getter、`getTimezoneOffset`、
  `valueOf`、`getTime`、`setTime` / `setFullYear` / `setMonth` / `setDate`，以及
  `toISOString` / `toJSON` / `toString` / `toDateString` / `toTimeString` / `toUTCString`。
- 本地时间来自 **`chrono`**（新增依赖）：进程时区只在那里拿得到。这是本批唯一的外部依赖变化。
- 两处已知偏差：`toString` 不带 node 那个时区名后缀（`(China Standard Time)`）；
  `toLocale*` 系列没有语言数据，退化为各自的标准形态。

**两处坑**

1. `valueOf` / `toString` 是**每个对象都有的名字** —— 原型方法分发必须按接收者分流，
   否则 `new Date('nope').toString()` 会走 `Object.prototype.toString` 打出 `[object Date]`。
2. 原型方法要改**两处**（注册 + `call_prototype_method` 的裸名分发）—— 我自己刚写下的规矩，
   本批就漏了 `setFullYear` / `setMonth` / `setDate`，feature 测试直接报
   `unknown prototype method`。

**顺带发现（已记入残留）**：`a - b` 这种**二元算术没有走 ToPrimitive**，于是
`date1 - date2` 是 `NaN`（`Pow` 早就走了，其余四个没有）。本批给 `Sub/Mul/Div/Rem`
补上后 `date1 - date2 === 200`、`new Number(3) * 2 === 6` 都与 node 一致，
**但整轮验证的内存峰值会因此上去**，故先撤回、记为残留（见下）。

**验证**

- 单元 190 / feature **493** / 护栏 7，全绿（`date_is_usable_for_everyday_time_handling`：
  构造、分量、静态、Invalid Date、`JSON.stringify`、排序、`setFullYear` 共 15 条断言）。
- 与 node 逐项对比一致：`toISOString`、`Date.parse`、`Date.UTC`、分量 getter、
  `getTimezoneOffset`、`toDateString` / `toUTCString` / `toTimeString`、`Date()` 返回字符串。
- **test262 计数无变化**：`built-ins/Date` 不在 `SUITES` 清单里（curated 清单不含它），
  所以本批对通过数是 +0 —— 这是"广度"批次的特点，收益不在测试集上。

**⚠️ 流水线问题（本批暴露，未解决）**

`scripts/phase2-status.sh` 的**整轮**跑不完了：进程级 OOM（`memory allocation … failed` +
SIGABRT），崩在 `built-ins/Object` 处。已排除的方向：

| 试过 | 结果 |
|------|------|
| 收紧单例堆上限到 96MB | 仍崩（不是单个测试吃内存） |
| 不保留失败明细（`TEST262_FAILURES=0`） | 仍崩 |
| 拆成两半跑（`language` / `built-ins` 两个进程） | **各自都跑完，逐套件数字与基线完全一致** |
| 用 `git stash` 回到 B30 基线跑整轮 | **能跑完（15484）** |

结论：整轮峰值本来就贴在可用内存的边沿，本批的静态增量把它顶过去了 —— 是**流水线脆弱**，
不是语义回退（两半的逐套件数字与基线逐位相同）。**已修（同批补的 B31b）**：`scripts/phase2-status.sh` 把整轮拆成 `language` / `built-ins`
两块各起一个进程跑，每块结束进程退出、内存随之释放；头条数字改成两块求和（`TOTAL`、
`guards` 分别累加），摘要表把两块的套件行直接合并。修好后整轮跑出 **15484 / 8133 / 2793**，
与 B30 基线逐位一致 —— 反过来证明了 B31 的 test262 计数确实是 +0。

踩到的两个坑（都出在"分块"这层胶水上）：块输出文件名带连字符（`full-built-ins.txt`），
手写字面名写成 `full-builtins.txt` 会让第二块静默缺席 —— 于是 `--update` 曾把**只剩
language 行**的快照写回去（少 17 行），只能从 HEAD 还原；改为全部用变量保存文件列表。
另外旧的头条数字块没删干净，会再打印一遍并覆盖变量（把块级数字当成整轮）。

**下一批**：`RegExp`（顺带 `replace/match/search/split` 的正则形式、`matchAll`）。
之后 `Promise` + 微任务队列 + `async/await`。小面一批仍在原处：`globalThis`、`flatMap`、
`e.stack`、elision 应为洞、TypedArray、`Proxy`/`Reflect`，以及上面退回的**二元算术
ToPrimitive**。

---

#### B32 `RegExp` —— 已完成（2026-09-28）

**决策**：B30 排的第二项。此前 `/a+/` 字面量直接是 `undefined`（整族缺失），
`String.prototype.replace/match/search/split` 也都不认正则。

**实现**

| 改动 | 说明 |
|------|------|
| `MakeRegExp` 指令 | 正则字面量没法降级成 `new RegExp(...)`（要凭空造 callee 与实参），所以单独一条指令：两个常量（pattern、flags）进常量池，VM 直接造对象 |
| `vm::object::RegExpObject` | `[[RegExpMatcher]]` + source/flags/lastIndex。`lastIndex` 在 `property_get`/`property_set` 里拦截 —— 做成属性则 `re.lastIndex = 3` 到不了匹配器 |
| `builtins/regexp.rs` | 构造器（含"传 RegExp 作实参"的复制语义）、`test`/`exec`/`toString`、四个正则感知的 String 方法（`match`/`search`/`replace`/`split`）与 `$` 替换（`$$ $& $` $' $n`） |
| `regex` crate | 新增依赖。引擎不自造模式编译器 |

**范围与偏差**：字符类、量词、分组、锚点、`i/m/s/u` 旗标、`g`/`y` 的 `lastIndex` 语义都在。
**不支持** lookaround 与反向引用（`regex` crate 表达不了），这类模式报
"pattern … is not supported" 而不是静默错配。函数的 replacer、正则的 `matchAll`
仍未做。ES 走 `Symbol.replace/match/search/split` 派发，这里按**实参形状**分流。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15484 | **15558** | **+74** |
| 失败 | 2793 | 2719 | −74 |
| 通过率 | 84.72% | **85.12%** | +0.40pp |
| 单元 / feature / 护栏 | 190 / 493 / 7 | 190 / **494** / 7 | +1 用例（18 条断言） |

3 个套件提升、**零回退**：`built-ins/String` +71、`built-ins/Object` +2、`built-ins/JSON` +1。
与 node 逐项对比一致（字面量、`source/flags`、`test/exec`、`toString`、旗标、`lastIndex`、
`match/search/replace/split`、`$1` 替换、`$$` 转义、`g` 循环计数、`split` 的 limit）。

**踩到的坑（同一条规矩，第三次）**

1. `set_prototype_method` 的闭包**不会被执行**（参数是 `_f`）。我把"实参是正则就走正则实现"
   的路由写进了闭包，`replace`/`split` 毫无变化 —— 路由必须写在 `call_prototype_method` 的
   分发里。
2. SSA 重命名里 `MakeRegExp` 只写了 `{}`：`dst` 是真正的定义，漏了 `rename_definition`
   会让 codegen 写到 SSA 之前的槽位（字节码里写 `[rbp+2]`、后面读 `[rbp+3]`），字面量因此
   是 `undefined`。与 B21 的 `InitLexical` 同一个坑。

**下一批**：`Promise` + 微任务队列 + `async/await`（B30 排的第三项，唯一要动执行模型的）。
之后的小面一批：`globalThis`、`flatMap`、`e.stack`、elision 应为洞、TypedArray、
`Proxy`/`Reflect`、`matchAll`、函数式 replacer、二元算术的 ToPrimitive。

---

#### B33 小面一批 —— 已完成（2026-09-28）

**为什么先做这个**：`Promise` 是 B30 排的第三项，但它要动执行模型。整轮 OOM 修好之后，
被它挡回来的那块应当立刻回到位，顺带把几个"用得上且便宜"的面补掉。

| 改动 | 说明 |
|------|------|
| **二元算术的 ToPrimitive** | `-` `*` `/` `%` 与 `Pow` 一致，先 ToPrimitive(hint number)。B31 补过又撤回（当时整轮会 OOM），现在整轮分块跑、峰值下来了，重新落地。`date1 - date2`、`new Number(5) - 2`、`{valueOf(){…}} - 1` 从此正确 |
| `Array.prototype.flatMap` | 回调由 VM 派发（与 `map`/`filter` 同一条通路），注册之外还要把方法名加进 `CALLBACK_METHODS` —— 否则报 `unknown prototype method` |
| `console.time` / `timeEnd` | 宿主计时器，标签存在 thread-local 里（跨一次宿主会话，不是 per-VM 状态） |

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15558 | **15568** | **+10** |
| 失败 | 2719 | 2709 | −10 |
| 通过率 | 85.12% | **85.18%** | +0.06pp |
| 单元 / feature / 护栏 | 190 / 494 / 7 | 190 / **495** / 7 | +1 用例（10 条断言） |

5 个套件提升、**零回退**：`expressions/{subtraction,multiplication,division,modulus}` 各 +2、
`built-ins/Array` +2。

**下一批**：`Promise` + 微任务队列 + `async/await`（唯一要动执行模型的）。之后的小面：
`globalThis`、`e.stack`（字节码里没有行列信息，只能给函数名）、elision 应为洞、`matchAll`、
函数式 replacer、TypedArray、`Proxy`/`Reflect`。

---

#### B34 `Promise` 与微任务队列 —— 已完成（部分；2026-09-28）

**决策**：B30 排的第三项，也是唯一要动执行模型的一件。本轮先落地**对象 + 队列 + 构造 +
`then`/`catch` + 两个静态**；`async/await`、`all/race/allSettled/any` 留下一轮。

**实现**

| 改动 | 说明 |
|------|------|
| `vm::object::PromiseObject` | `[[PromiseState]]` / `[[PromiseResult]]` / 反应队列 + `id`（注册表用） |
| `State::promise_jobs` | 微任务队列（FIFO）。引擎没有事件循环，所以它在**顶层 `run` 结束时**排空 |
| `promise_construct` | `new Promise(executor)`：造对象 → 铸 `resolve`/`reject` → 调 executor；executor 抛出即 reject（ES 27.2.3.1 step 11） |
| `try_promise_method` | `then`/`catch`/`finally` 由 VM 派发（要调用户函数）。**两处**方法调用路径都要挂 |
| `Promise.resolve/reject` | VM 侧拦截（要登记进 promise 注册表）；`resolve(promise)` 原样返回该 promise |

`resolve` / `reject` 是宿主函数按名字派发的，所以它们把目标的 `id` 编码进函数名
（`__promise_resolve__<id>`），VM 收到调用时按 id 找回 promise —— 与 Map 的迭代器、
`Symbol.replace` 那类"只有 VM 能造"的对象同一种手法。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15568 | **15571** | **+3** |
| 失败 | 2709 | 2706 | −3 |
| 通过率 | 85.18% | **85.19%** | +0.01pp |
| 单元 / feature / 护栏 | 190 / 495 / 7 | 190 / **496** / 7 | +1 用例（7 条断言） |

2 个套件提升、**零回退**（`language/statements/class` +2、`built-ins/Object` +1）。
与 node 逐项对比一致的部分（探针在 /tmp/p2.js 一类脚本里）：

```
order: then:1 | resolve:2 | catch:boom | reject:bad | resolved-promise:3     ← 引擎
order: then:1 | resolve:2 | catch:boom | reject:bad | exec:exec-throw | resolved-promise:3  ← node
```

顺序、链式传递（`then` 返回值喂给下一个 `then`）、reject 走 `then` 的第二参或 `catch`、
`Promise.resolve(p) === p`、同步代码先于微任务 —— 都对。

**残留（下一轮的输入）**

1. **链式 `new Promise(…抛出…).catch(cb)` 的 handler 收不到**：实测在该路径上
   `args.first()` 是 `undefined`（同一段代码改成 `var p = new Promise(…); p.catch(cb);` 就正常）。
   已确认不是 handler 装箱的问题（装箱修过、仍 `undefined`）。看起来是**已 reject 的 promise**
   + 链式方法调用这条组合上的实参丢失，需要顺着 `Opcode::CallMethod` 的实参收集查。
2. `async/await` 未做。设计已定：`Module.asyncs` 集合（照 `generators`）+ `Await` 指令；
   `await` 对未 settle 的 promise **就地排空队列**直到它 settle —— 引擎里 promise 只能靠 job
   settle，没有外部源（没有定时器/IO），所以这样是收敛的；代价是没有真正的并发交错。
3. `all/race/allSettled/any`、`finally` 的返回 promise 语义、thenable 采纳未做。
4. 下一批顺序：先修 1，再做 `async/await`，然后 `all/race`。

---

#### B35 `async/await` + `Promise` 组合子 + `Promise` 套件入册 —— 已完成（2026-09-29）

**这批有一部分是用户手写的**（`async/await` 的管线、四个组合子、species/tag、runner 的
`$DONE` shim），我接手做的是：核对、修一处规范错误、把验证跑通、把编号入册。

**实现（照 B34 记录里定下的设计）**

| 改动 | 说明 |
|------|------|
| `Module.asyncs` | 照 `generators` 的写法；五个调用点都挂了钩子 |
| `Opcode::Await` + IR/builder/codegen/ssa | `await expr` |
| `await_value` | 就地排空微任务队列直到目标 promise 不再 pending，然后取值 / 抛原因 |
| `drive_bytecode_frame` | 从 `invoke` 里抽出的"推帧、跑到 `Ret`、恢复调用者上下文" |
| `promise_aggregate` | `Promise.all` / `race` / `allSettled` / `any`；每个元素两个原生 handler 把 `(聚合 id, 下标)` 编码进函数名（与 `__promise_resolve__` 同一种手法） |
| `Promise[Symbol.species]` + `Promise.prototype[Symbol.toStringTag]` | 与 Map/Set/Array 一致 |
| `VM::global(name)` | 宿主读回顶层 `run` 之后（含已排空的队列）留下的全局 —— 这是 runner 判定 `flags: [async]` 用例的手段 |
| runner：`$DONE` shim + 解除 `async`/`await` 门控 + `built-ins/Promise` 入册 | 见下 |

**修掉的一处规范错误（本批的关键）**：`IteratorClose` 在**正常完成**时也把
`iterator.return()` 抛出的异常吞掉了。ES 7.4.6 只在"正在传播的 throw 完成"里吞（step 7），
正常完成时那个异常必须传播（step 8）。`if let Ok(result) = self.invoke(...)` 正好把它丢掉 ——
在 `invoke` 改成把错误作为 `Err` 返回之后，这一点暴露成 4 条用例失败
（`assignment/dstr/*iter-nrml-close-err`）。改成 `?` 之后与基线**逐条一致**（该套件失败集
固定为 60 条，与 HEAD 相同），复现脚本也与 node 一致。

**验证基建**：整轮拆成 **4 片**（runner 新增 `TEST262_CHUNKS` / `TEST262_CHUNK_INDEX`，
按 `position % chunks` 分），每片一个进程、各自 `ulimit`；内存上限默认降到 **5GB**
（机器 7.7GB，OS 不会来 kill）。顺带量出一个事实：**3GB 不够**，而按套件分片并不减少用例
总数 —— 峰值是**按"每条用例"累积**的，这是一处待查的 per-test 内存增长。

**效果**（分母变了：`built-ins/Promise` 入册，执行 18277 → 18914）

| 指标 | 起点（B34） | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15571 | **15972** | **+401** |
| 失败 | 2706 | 2942 | +236 |
| 跳过 | 8133 | 8173 | +40（新套件的门控用例） |
| 执行 | 18277 | 18914 | +637 |
| 通过率 | 85.19% | **84.45%** | 分母不同，不可直比 |

`built-ins/Promise` 入册后 238 通过 / 301 跳过 / 138 失败。**零逐套件回退**。
feature 499（+3 用例）。

**残留**

1. `await` 是"就地排空队列"，所以它与先前排队的 job **不交错**：探针里
   `rejected` 与 `awaited` 的先后与 node 相反。语义上仍在"微任务在同步代码之后"的框架内，
   但没有真正的并发交错 —— 要真交错就得让 `await` 挂起帧（生成器那套机制）。
2. **per-test 内存增长**（新增待查项）：整轮峰值随用例数增长，而不是随套件数。
   3GB 上限会在单片内 OOM，5GB 才行。修掉它能把上限往下调、也让长跑更稳。
3. `drive_bytecode_frame` 退出时 `self.state.frame_argc.truncate(saved_this_depth)` 用的是
   `this_stack` 的长度 —— 两条栈的长度未必同步，这里值得单独看一眼（记为疑点）。
4. B34 留下的"链式 `new Promise(…抛出…).catch` 收不到 handler" —— **随 B37 的 `VM::run`
   补齐重置一并好了**（B37 之后那条用例与 node 逐字一致）。已加防回归断言：
   `chained_reactions_run_including_when_the_executor_throws`，用 `VM::global` 在 drain 之后
   读回结果（微任务只能这样观察）。

---

#### B36 逐用例内存增长：查清根因，并否掉一条看似可行的路 —— 已完成（2026-09-29）

**起点**：B35 量出"整轮峰值按**每条用例**累积"：3GB 上限在单片内就 OOM，5GB 才够。

**根因（查清了）**：runner 每个测试都 `guarded_vm()` 新建一个 `VM`，于是每用例新建一整套
`Builtins` 图 —— 而图里全是 `Rc` 环（`prototype.constructor` ↔ `constructor.prototype`，
每个方法对象又指回自己的原型）。**没有 GC，环就永不回收**，于是每用例漏掉整套图（约几百 KB，
正好对上 3GB / ~5000 用例的量级）。

**试过的路（以及为什么否掉）**：改成复用一个 VM —— 引擎自己的注释就写着
"a host reuses the VM across tests"，`VM::run` 也会 `state = State::new()` 并重新注册
builtins。内存问题**当场解决**：同一分片在 **1GB** 上限下跑完（此前 5GB 才勉强够）。

但**结果变了**：

| | 每用例新建 VM | 复用同一个 VM |
|---|---|---|
| 通过 | 15972 | **14002** |
| 失败 | 2942 | 4912 |

`结论：有逐套件回退`。也就是说 **`VM::run` 并没有重置一次运行触及的全部状态**，用例之间会
互相污染 —— 引擎文档里那条"宿主复用同一 VM"的契约**其实没有兑现**。这是本次最有价值的产出：
一个被掩盖已久的**真债**。复用已撤回，逐用例仍新建 VM。

**留下的基建**（都会保留）

- 整轮 **4 片**（`TEST262_CHUNKS` / `TEST262_CHUNK_INDEX`，按 `position % chunks` 分），每片
  独立进程 + 独立 `ulimit`（B35 已入库，本批继续用）。
- 整轮跑在**一个 256MB 栈的工作线程**上：深递归用例在默认 2MB 测试线程栈上没有余量
  （`MAX_CALL_DEPTH` 512 层 Rust 帧）。
- 内存上限默认 5GB，注释里写清了为什么现在还降不下来、以及降到多少是目标。

**效果**：通过 15972 / 跳过 8173 / 失败 2942，与 B35 **逐位一致**，零逐套件回退（本批是诊断
不是优化，没有测试集收益）。

**残留（下一批的输入）**

1. **`VM::run` 的状态重置不完整** —— 复用 VM 会让 15972 掉到 14002。这是比内存更值钱的一条：
   它同时意味着"宿主长时间复用同一 VM"（真实的日常用法）会有跨程序污染。修它需要逐项核对
   `VM` 上所有跨 run 的字段与 `State`，以及 builtins 注册是否真的幂等。
2. **逐用例内存增长**：根因是环 + 无 GC。正道是给引擎一个**拆卸/断环**的路径（或让
   `Builtins`/`State` 在下次 `run` 时显式释放上一轮的图），而不是靠宿主绕开。修好 1 之后再修它，
   否则又会掉进"复用但污染"的坑。
3. `drive_bytecode_frame` 退出时 `frame_argc.truncate(saved_this_depth)` 用的是 `this_stack`
   长度（B35 记的疑点，仍在）。
4. B34 的"链式 `new Promise(…抛出…).catch` 收不到 handler"仍在。

---

#### B37 `VM::run` 的重置与 realm 的断环 —— 已完成（2026-09-29）

**同一条根因，两个表面症状**（B36 的两条残留其实是同一件事）：

1. `VM::run` 只重置了 `state` / `func_objs` / `bound_functions` / `invoke_boundaries` /
   计数器，**没有**重置 `iterator_registry`、`generator_registry`、`delegate_stack`、
   `generator_send`/`generator_yielded`/`generator_yield_pc`、`running_generator_prologue`。
   前两者持有一轮里造过的**全部**迭代器与生成器对象 —— 于是宿主复用同一个 VM 时，每轮的对象图
   都被钉住（内存增长），而且下一轮能看见上一轮的残留（"复用就变结果"）。
2. realm 的 `Rc` 环没人断：`prototype.constructor` ↔ `constructor.prototype`，以及每个函数对象
   与它的 `prototype` 对象互指。没有 GC，环就永不回收 —— 于是"每个程序一个 VM"（runner 必须
   如此，见下）每程序漏掉整套 `Builtins` 图。

**改动**

| 位置 | 改动 |
|------|------|
| `VM::run` | 补齐上面那五处重置 |
| `VM::run` | 清 `func_objs` **之前**先删掉每个已装箱函数对象的 `prototype` 属性（断环） |
| `Builtins::teardown` | 删掉 21 个原型的 `constructor` 属性，并清空线程本地的包装原型表（它按名字索引，会钉住上一个 realm） |
| `impl Drop for VM` | 调 `teardown()` |

**实测**

| 测量 | 修前 | 修后 |
|------|------|------|
| 分片 0 在 **1GB** `ulimit` 下 | OOM（`memory allocation … failed`） | **跑完**（TOTAL 3658） |
| 整轮默认内存上限 | 5GB | **2.5GB**（实测下限：分片 1GB 即可；`built-ins/Array` 里有一条**合法**的 ~384MB 单例分配，所以留余量） |

test262：**15972 / 8173 / 2942**，与 B35 逐位一致、零逐套件回退（本批是修债，不改语义）。
feature **500**（+1 条防回归用例：同一个 VM 连跑两个程序，后者看不到前者的 `var` 与注册表）。

**一个要写清楚的区分**：状态重置修好之后，"宿主复用同一个 VM"这条契约在**状态**层面成立了；
但 runner **仍然**每用例新建 VM —— 因为 test262 用例会**改动内建对象**（`Array.prototype[Symbol.iterator] = …`、
`Object.defineProperty`、删除属性……）并且每条都期望一个**干净 realm**。实测：即使重置补齐，
复用同一个 VM 也只有 14002 通过。所以"realm 新鲜"是比"状态重置"更硬的要求，两者不能混为一谈。

**残留**

1. `built-ins/Array` 有一条合法的大分配（~384MB），决定了单片上限的下限。
2. `drive_bytecode_frame` 退出时 `frame_argc.truncate(saved_this_depth)` 用的是 `this_stack`
   长度（B35 记的疑点，仍在）。
3. B34 的"链式 `new Promise(…抛出…).catch` 收不到 handler"仍在。

---

#### B38 函数式 replacer —— 已完成（2026-09-29）

**为什么要它**：`s.replace(/re/g, fn)` 是日常写法里最常见的一种，此前直接报
"a function replacer is not supported yet"（B30 落地字符串 replace 时就留下的口子）。

**实现**：替换要**调用用户函数**，所以整段替换在 VM 里跑（`try_string_callback_method`），
与数组回调、`then/catch` 同一条路子：找匹配 → 以 `(match, group…, offset, input)` 调函数 →
把返回值 `ToString` 后拼回去。搜索值可以是 RegExp（带 `g` 则全部匹配，否则只第一个）或字符串；
`replaceAll` 一律全部匹配。

**又一次踩到同一条规矩**：拦截要挂在**两处**方法调用路径上（`try_array_callback_method`
旁边那个只有一处不够），而且传进来的实参可能是未装箱的 `Value::Function(id)` ——
`is_callable()` 对它返回 false，得先 `as_object_value`。这两个坑在数组回调与 promise 上都
出现过，这是第三次。

**效果**

| 指标 | 起点 | 终点 | 变化 |
|------|------|------|------|
| 通过 | 15972 | **15978** | **+6** |
| 失败 | 2942 | 2936 | −6 |
| 单元 / feature / 护栏 | 190 / 501 / 7 | 190 / **502** / 7 | +1 用例（7 条断言） |

1 个套件提升（`built-ins/String` 702 → 708）、**零回退**。与 node 逐字一致的 7 种形状：
全局替换、分组交换、`offset`/`input`、字符串模式、无匹配（不调函数）、以及字符串 replacer
仍走老路径。

**下一批**：铺面继续 —— `globalThis`、`e.stack`（字节码无行列信息，只能给函数名）、
elision 应为洞、`matchAll`、TypedArray、`Proxy`/`Reflect`。另外两条债仍在：
`built-ins/Array` 那条 ~384MB 的大分配（决定单片上限下限），以及
`drive_bytecode_frame` 里 `frame_argc.truncate(saved_this_depth)` 的疑点。

---

#### B39 `globalThis` —— 已完成（2026-09-29）

**为什么要它**：脚本里最常见的全局引用，此前完全没有（引擎的全局只是个 `HashMap` 放在 `State`
上，脚本层 `this` 也因此不是全局对象）。

**实现**：`State.globals` 改成 `Rc<RefCell<HashMap<..>>>`，再加一个 `GlobalObject` 与它共享同一
个 `Rc` —— `globalThis` 是这个 map 的**视图**而不是副本，因此不需要在任何写入点做同步。
`class_name` 给 `global`，于是 `Object.prototype.toString.call(globalThis) === "[object global]"`。

脚本语义下与 node 一致的点：`var` 会作为属性出现、写入可见、`Object.keys` 列出全局
（含内建）、`delete` 有效；而脚本层 `let` **不会**出现（B21 的声明记录，本来就该如此）。
两处与 `node file.js` 不同是**脚本 vs 模块**的差别：node 把文件包成 CommonJS 模块，所以它的
`var gv` 不是全局属性 —— test262 跑的是脚本，引擎是对的。

**踩到的两个坑（同一个环，两种躲法）**

1. 把 `globalThis` 作为属性放进 globals map 会形成 `map → 对象 → map` 的环；没有 GC，于是
   **每个 run 的整个 `State` 都被钉住** —— 实测表现为分片撞穿自己的 `ulimit`（一次 240MB 的
   分配失败）。
2. 想绕开它、改成"环境查找时特判 `globalThis` 这个名字"，环没了，但
   `globalThis.globalThis === globalThis` 就变成 `undefined` 了。

最后的做法是保留这个属性，在**丢弃 State 之前**手动摘掉自引用（`run` 开头 + `Drop for VM`），
与 B37 断 `prototype`⇄`constructor` 是同一种手写断环。

**效果**：test262 计数 **+0**（curated 清单里没有套件考它），feature **503**（+1 用例 /
7 条断言），内存无退步（分片仍能在 1GB 上限下跑完）。

**下一批**：铺面继续 —— `e.stack`（只能给函数名，字节码无行列信息）、elision 应为洞、
`matchAll`、TypedArray、`Proxy`/`Reflect`。两条债仍在：`built-ins/Array` 那条 ~384MB 的大分配
（决定单片上限下限），以及 `drive_bytecode_frame` 里 `frame_argc.truncate(saved_this_depth)`。

---

---

---

## 7. 验证与回归策略（本阶段增补）

沿用第一阶段的 §6（三级验证、跳过表治理、失败原因聚合、性能护栏、单元测试、内存护栏），本阶段增补三条：

1. **零回退带逐套件核对**。全量通过数不低于上一提交是必要不充分条件 —— 新语义可能同时修好 A 与弄坏 B。每次提交前用脚本对比逐套件通过数，出现下降必须解释或回退。
2. **范围外杂音单列**。报告里 RegExp / ES2022 公开字段等**不可能通过**的失败要单独成列，否则 M7 的"清零"目标不可测（M8-V3）。
3. **崩溃优先于失败**。引擎里的无界递归/栈溢出会以 SIGABRT 打断整轮回归且不留线索（已发生过一次）。`BIUJS_TEST262_TRACE=1` 逐条打印测试路径是标准排查手段；任何新批次的第一次全量回归都要留意进程是否走完全程。
4. **挂死同样优先于失败**。三层执行护栏（§7.1）把"跑不完"变成"一条失败"，并且让全量耗时不再取决于有没有撞上无界循环。

### 7.1 三层执行护栏（2026-09-26 落地）

第 3 条的下半句是"挂死同样优先于失败"。实测那条幽灵用例
（`built-ins/Set/prototype/symmetricDifference/set-like-class-mutation.js`）单条烧 60s ——
最坏的一次全量因此从 3 分钟变成 15 分钟，而它在失败明细里只占一行，很容易被当成"正常的慢"。

| 护栏 | 环境变量（0 = 关） | 默认 | 抓什么 |
|------|-------------------|------|--------|
| 步数 | `TEST262_STEP_LIMIT` | `DEFAULT_STEP_LIMIT`（2×10^7） | 热循环 |
| 墙钟 | `TEST262_TIMEOUT_MS` | `15000` | 慢循环、native 卡顿 |
| 堆 | `TEST262_MEMORY_MB` | `256`（**单条用例增量**） | 失控分配 |

四条踩出来的规矩：

1. **护栏触发后必须"粘住"**。JS 能 `catch` 到那个 `RangeError`（test262 里遍地是
   `assert.throws` / `try`），不粘住的话用例会从刚被拦下的循环里爬回去，把步数预算也烧光 ——
   最后报出来的元凶还是错的。`GuardFired` 就是为此存在的。
2. **堆预算是"增量"而不是进程绝对上限**。套件自己持有几百 MB 已解析的测试，绝对阈值会把
   套件的内存算到当时正在跑的那条用例头上：实测误伤 **49 条**，分散在
   `Array` / `Math` / `identifiers` / `String` 等互不相关的套件里。改成"以用例开始时存活字节为基线"
   后误伤 **0 条**（14360 = 关掉护栏时的 14360）。
3. **分配器里不能读环境变量**。`env::var` 自己会分配内存 → 分配器重入；
   `OnceLock::get_or_init` 重入会死锁，表现是"设了 `TEST262_MEMORY_MB` 就一行输出都没有地卡住"。
   现在预算由 runner 在用例开始前写进原子变量，分配器只读原子。
4. **护栏本身要有测试**（`tests/guards.rs`，独立二进制 + 自带分配器）：三条各自触发、可关闭、
   每次 `run` 重新计时与清标志、"被 catch 后不能继续跑"。它内部用一把互斥锁串行化 ——
   堆预算与标志是进程级的，`cargo test` 默认多线程会互相污染（实测过一个用例报"内存超预算"、
   另一个报"成功"）。

定稿实测（2026-09-26，18k 用例）：

```
TOTAL 14360 / 8133 / 3906          ← 与关掉护栏时逐套件一致（零误伤）
guards: timeout 1, step-limit 0, memory 8    ← 9 条越线用例在基线里本已失败
最慢用例 15.00s（被墙钟截断，原本 60s+）
```

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
| `Rc` 下 `WeakMap`/`WeakSet` 不是真弱引用 | 低 | **已登记**（B3）：条目不会被回收，其余可观察面（对象/Symbol 键规则、无 `size`/无迭代/无 `clear`）都按规范实现 |

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
