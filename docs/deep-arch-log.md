# 深改改造日志（deep-arch）

> **配套文档**：目标架构见 `docs/architecture-redesign.md`（v2）。本文件只记"实际做了什么、验到了什么、留下了什么"。
> **分支**：`refactor/deep-arch`（自 `refactor/interpreter-p0a` 的 `b52c517` 分叉）。
> **纪律**：每个子步一次提交；**不允许**同一提交里既有"结构变化"又有"语义变化"；
> 语义变化单独提交并标注"这是修 bug"。提交信息按 `architecture-redesign.md` §7.3 列出"允许的回归项"。

---

## 0. 记录格式（每个子步一段）

```
### <里程碑编号> <标题>（<日期>）
- 目标：
- 改了什么：（文件 → 改动）
- 与原设计文档的偏差：（若有，给理由）
- 验证：基线 → 现状（数字）+ 新增守卫
- 已知未解 / 下一步：
```

---

## 1. 开工基线（2026-10-07）

**分支**：`refactor/deep-arch`，自 `b52c517`（M0 收尾）分叉。
**工作树**：`docs/architecture-redesign.md` 已改为 v2（未提交）。

### 1.1 编译基线

```
cargo check --all-targets   → Finished, 26 warnings（全部为既有：未用方法 / 冗余 mut）
```

### 1.2 度量基线

取自 `docs/handover.md` §4 与 `docs/phase2-status.tsv`（M0 收尾时）：

| 指标 | 数值 |
|---|---|
| test262 执行 / 通过 / 失败 / 跳过 | 19708 / **16561** / 3147 / 7843 |
| 通过率 | 84.03% |
| 单元 / feature / 护栏 | >201（M0 后）/ 523 / 7 |

> 本文件在下一次跑齐三级验证后回填**实测**数字（§1.3）。

### 1.3 实测补记

```
cargo test --release --lib        → 203 passed
cargo test --release --test features → 524 passed / 3 ignored
cargo check --all-targets         → 26 warnings（lib test；与开工前一致）
```

全量三级验证（`MEM_LIMIT_KB=4000000 ./scripts/phase2-status.sh`）：

```
TOTAL 16561 / 7843 / 3147     执行 19708 = 16561 + 3147
guards: timeout 7, step-limit 0, memory 22
结论：无逐套件回退
```

> **注意（脚本环境问题，非本仓库缺陷）**：`scripts/phase2-status.sh` 默认
> `MEM_LIMIT_KB=2500000`，在全量第 2 片会被 `built-ins/Array` 的 ~384MB 合法分配打断
> （`memory allocation of 402653184 bytes failed` → SIGABRT）。这是 `handover.md` §4
> 已登记的既有状况（"整轮峰值贴在内存边沿……不修"）。**跑全量必须带
> `MEM_LIMIT_KB=4000000`**，与 `interpreter-refactor.md` §0.1 的口径一致。
---

## 2. 施工计划（M1）

M1 = **编译期自包含 + EH 表可信**。硬理由（`architecture-redesign.md` §5.1）：
M2 的 `run_until` 要在"离开帧 / 遇到 `Throw`"时找 handler，而 handler 只能来自 `EhTable`；
而 `EhTable` 目前**不可信**（`EndTry`/`Try` 不配对，§10.3 已实测证伪）。

| 子步 | 内容 | 是否改运行期行为 |
|---|---|---|
| **M1.1** | `PushSeh` 加 `body: BlockId`；`Try` 加第三操作数 `end_offset: jr`（arity 2→3）；lowering 回填"受保护范围 + 出口集合"到 `Module` 的新表 | **否**（只加数据 + 一致性守卫，沿用 `337461d` 的"先证等价再切换"纪律） |
| **M1.2** | `EhRegion` 从"扫 `EndTry` 派生"改为"读声明表"；补 `exits`/`normal_exit`/`parent`；VM 的 `debug_assert` 升级为可靠断言 | 否（表更准，运行期仍用指令偏移） |
| **M1.3** | `CodeBlock` per function；`generators`/`asyncs`/`derived_ctors` + `exit_pc` 映射删除；44 处 `module.` 收敛 | 否 |

**为什么 M1.1 与 M1.3 不合并**：M1.1 只碰 EH 数据通路（`lowering` → `codegen` → `bytecode`），
M1.3 是全库扇出（44 处 `module.` 访问）。混在一起时回归噪声无法归因。

---

## 3. M1.1 EH 区域：改为"编译期声明 + 结构化配对"（2026-10-07）

### 3.1 目标

把"哪条 `EndTry` 收的是哪条 `Try`"从**扫指令流 + 深度计数**（§10.3 已实测证伪）
换成**编译期声明 + 结构化配对**，使 `EhRegion` 的字段语义可信 —— 这是 M2 的硬前置。

### 3.2 改了什么

| 文件 | 改动 |
|---|---|
| `src/compiler/ir/instruction.rs` | `PushSeh` 加 `body: BlockId`（区域身份 = `try` 体入口块）；`PopSeh` → `PopSeh { region: BlockId }`；`defined_and_used` / `Display` 同步 |
| `src/compiler/ir/builder.rs` | `push_seh(handler, finally, body)` / `pop_seh(region)` |
| `src/compiler/lowering/mod.rs` | `lower_try` 传 `try_body`；两处 `pop_seh()` → `pop_seh(try_body)`（体的正常出口、catch 的正常出口） |
| `src/compiler/ir/ssabuilder.rs` | 两处 match 模式（`PushSeh { .. }` / `PopSeh { .. }`），**语义未动** |
| `src/compiler/codegen.rs` | 新增 `EhDecl` + `eh_decls` / `eh_exits`；`PushSeh` 记声明、`PopSeh` 记退出点；布局后 `resolve_eh_regions()` 用 `block_map` 解析成 pc；新增 `eh_regions()` |
| `src/compiler/mod.rs` | 收 `codegen.eh_regions()`，按函数 `offset` 平移到模块坐标系，装入 `Module` |
| `src/bytecode.rs` | `Module.eh_regions: HashMap<u32, Vec<EhRegion>>`（+`Module::new` 参数）；`EhIndex.by_end` → `by_exit`；`eh_regions_of` 从"扫指令流派生"改为**读表**；`EhRegion.end: usize` → `exits: Vec<usize>`（+`last_exit()`） |

**运行期行为零变化**：`Instr::Try` / `Instr::EndTry` 的发射一字未动，`codes` 不被新增逻辑触碰。
新增的只有"数据收集 + 守卫"。

### 3.3 与 `architecture-redesign.md` 的偏差（三处，都有理由）

| # | 文档原计划 | 实际做法 | 理由 |
|---|---|---|---|
| 1 | §5.1 改动点 4：`Try` 加第三操作数 `end_offset: jr`（arity 2→3） | **不做** | 单个 `BlockId` 字段表达不了多块的受保护范围（`try` 体常常跨多个块）；而**运行期不需要 `end`** —— "我现在在哪个区域"由帧上的 try 栈回答（§4.6），拿 pc 查区间这条路本来就不走。加它等于加一个没有消费者的操作数，同时把指令表、arity 分组、两条既有测试全动一遍 |
| 2 | §4.6 / §5.1：出口集合由 lowering 的 `SehFrameInfo.exit_edges` 收集 | **从 IR 的 `PopSeh` 直接收** | `PopSeh` 本身就是"正常退出区域"这个事件，是**权威来源**；从 lowering 绕一圈反而多一条需要保持同步的通道。实测也更完整（见下） |
| 3 | §4.6：`EhRegion { normal_exit, exits, parent }` | `EhRegion { start, exits, catch, finally }` | `normal_exit` 与 `parent` 目前**没有消费者**：前者被"catch 也退一次"这件事证伪了唯一性；后者由 M2 的 `Frame.try_stack` 天然表达。**不加没有消费者的字段** |

**实测出的两条新事实（都记进了测试，避免下次再踩）**

1. **一条区域可以有 0 个退出点**：`lower_try` 只在 `!current_block_is_terminated()` 时发 `PopSeh`。
   所以 `function g() { try { return 1 } catch (e) { return 2 } }` 里**一条 `EndTry` 都没有**
   —— 这就是"深度计数配对"必然算错的机制。`exits` 为空是**信息**，不是错误。
2. **pc 顺序不表达嵌套**（实测数据，`function d() { try { try{...}catch(e){} } finally {...} }`）：

   ```
   outer = { start: 92, exits: [101], catch/finally: 109 }
   inner = { start: 95, exits: [98, 106], catch: 104 }
   ```

   外层的退出点 **101 夹在内层两个退出点 98 与 106 之间**：布局是 RPO，
   内层的 `catch` 块靠**异常边**到达、被排在 101 之后。
   ⇒ 这是"用 `[start, end)` 表示区域"这条路**根本走不通**的第二个证据
   （第一个是 §10.3 的配对）。测试里把这段数据与结论写成了注释。
   （第一版我写的断言是 `inner.exits < outer.last_exit()`，**被这条实测数据打红** ——
   守卫起作用了，去掉的是我错误的假设，不是守卫。）

### 3.4 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 单元 | 203 | **204** | +1（新增 `a_region_may_never_be_popped_explicitly`） |
| feature | 524 / 3 ignored | **524 / 3 ignored** | 不变 |
| 护栏 | 7 | **7** | 不变 |
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | 逐字节一致 |
| 逐套件对比 | —— | —— | **无逐套件回退** |
| 定向 5 套件 | try 152/22/27、stmts/class 1410/2806/151、exprs/class 1258/2702/99、stmts/gen 237/9/20、exprs/gen 251/13/26 | 同上 | **逐字节一致** |
| 告警 | lib test 26 | **lib test 26** | 无新增 |

**assume 说明**：`guards:` 行的计数（timeout 7 / step-limit 0 / memory 22）**无法与基线自动 diff**
（`phase2-status.tsv` 不含该字段）。判它未变的依据是：本步不在任何执行路径上
（不改 `codes`、不改控制流），且总量与逐套件结果全等。**这是推断，不是实测** ——
已登记为"下次改到执行路径时必须实测 guards"。

**新守卫（`src/bytecode.rs` 单测）**

- `eh_regions_match_the_emitted_try_instructions`（重写）：区域数 == `Try` 条数；
  起点两两不同；处理器与退出点都在函数范围内；`by_start` 可查；
  **`已声明的退出点集合 == 实际发射的 `EndTry` pc 集合`**（这条取代了旧的深度计数规则，
  是配对正确性的**充要**守卫）；a/b/c/d 四个函数的语义逐条核对。
- `a_region_may_never_be_popped_explicitly`（新增）：钉住 §10.3 的根因 ——
  体与 catch 都以 `return` 结束时，`exits` 为空且程序里 0 条 `EndTry`，
  而 `catch` 地址仍然可信。

### 3.5 已知未解 / 下一步

1. **`ssabuilder` 的 `seh_scope` 仍"盲目 pop"**（`ssabuilder.rs` 注释自陈：
   "块的物理顺序并不等于 SEH 嵌套顺序，`pop_seh` 可能弹错作用域，导致漏掉真实存在的边"）。
   现在有了 region 身份，**可以改成按身份删** —— 但那会改变异常边集合，
   属于**语义变化**，必须单独提交、单独验证（记入 M1.2 的第一项）。
2. **VM 的 `EndTry` arm 仍未加断言**：运行期不查 pc，所以没东西可断。
   M1.2 切到"以表为准"时再加"try 栈顶的 `start` 与区域表一致"。
3. **EH 表还在 `Module.eh_regions`，没挂进 `FunctionBody`**：`FunctionBody` 仍是
   `{start, end}` 派生视图。合并是 M1.3（`CodeBlock`）的事。
4. `EhRegion.last_exit()` 目前只有测试用；M2 若确认无用应删掉（不留无人消费的 API）。

### 3.6 补记：debug 模式验证（M1.1 的验证缺口）

M1.1 的全量验证跑在 **release** 下，`debug_assert!` 被编译掉，
所以"EH 表 == 指令操作数"这条等价性证据**当时并未执行**。补跑：

```
cargo test --lib        → 204 passed
cargo test --test features → 524 passed / 3 ignored      （debug，跑真实 JS）
```

⇒ 两条断言都在真实 JS 上成立：`Try` 的指令操作数与表一致（两条独立回填路径不分叉），
`eh_index` 的"退出点唯一"成立。**教训记入流程：涉及 `debug_assert` 的改动，
验证必须包含一次 debug 运行**（release 跑绿不等于断言跑过）。

---

## 4. M1.2-a VM 的 EH 处理器地址改以表为准（2026-10-07）

### 4.1 目标

把 §10.3 留下的"等区域边界可信之后，再切到查表"这一刀切下去 ——
让**运行期只有一个权威来源**（EH 表），指令操作数只作互校。

### 4.2 改了什么

| 位置 | 改动 |
|---|---|
| `vm/mod.rs` 的 `Instr::Try` arm | 处理器地址从 `eh_region_starting_at(pc)` 取（**权威来源**）；找不到声明时返回 `RuntimeError::InternalError`（不静默兜底，避免出现"第二条路径"）；`#[cfg(debug_assertions)]` 下仍读指令操作数做**互校** |
| `vm/mod.rs` 的 `Instr::EndTry` arm | 加了 `debug_assert!(module.eh_region_ending_at(pc).is_some())` |

**为什么现在才切**：文档里记着退回用指令偏移的两条理由 ——
(a) 表的 `end` 不可靠；(b) 查表有开销。M1.1 把 (a) 修掉了（表由编译期声明、配对结构化）；
(b) 在这一步同时是**负收益**：省掉了两次偏移加法，只多一次 O(1) 哈希。

### 4.3 这一步的**证据**（不是"看起来对"）

`Instr::EndTry` 上那条断言**曾经是 §10.3 的证物**：当时它在这里响过
（`EndTry(pc=73)` 关掉的不是任何区域）。现在同一条断言在 **debug 模式、524 条真实 JS
feature 断言**下全绿。这是"根因已修"最直接的证据 —— 断言没被删、没被放松，条件变了。

### 4.4 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 单元（debug + release） | 204 | **204** | 不变 |
| feature（debug + release） | 524 / 3 | **524 / 3** | 不变 |
| 护栏 | 7 | **7** | 不变 |
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | 逐字节一致 |
| 逐套件对比 | —— | —— | **无逐套件回退** |
| guards 行 | timeout 7 / step-limit 0 / memory 22 | **同值** | 第二次独立测量取值相同 |
| 定向 7 套件 | try / class / exprs.class / generators×2 / function / switch | **逐字节一致** | 含 `function`（347/74/30）与 `switch`（51/54/6）扩大异常路径覆盖 |
| 告警 | lib 24 / lib test 26 | **同** | 无新增 |

**自评**：本步是**行为变化**（运行期不再读指令偏移），但结果与基线逐字节相同 ——
这正是"先证等价、再切换"要的结果：切换点本身不产生任何数字变化，
变化的是"以后这类 bug 不可能再靠两条路径分叉产生"。

### 4.5 已知未解 / 下一步

- `SehRecord` 仍然把 `handler_pc` / `finally_pc` 存成**绝对 pc**。
  M2 会把它换成 `region: RegionId`（或直接靠 `Frame.try_stack`），
  那时 `EhRegion` 的 pc 只用于 dump。
- **`ssabuilder` 的 `seh_scope` 仍盲目 pop**（M1.2 的第一项，尚未做）：
  现在有了区域身份，可以按身份删；但那会改变**异常边集合** ⇒ 是语义变化，
  要单独提交、单独验证。**这是下一步。**

---

## 5. M1.2-b 异常边集合改由 CFG 数据流给出（2026-10-07）—— 修掉一个静默错编

### 5.1 先测量，再动手

M1.1 让区域有了**身份**，于是"线性扫描的物理顺序 == SEH 嵌套顺序"这个假设**第一次可以被量**：
给旧的盲目 `pop()` 加一条断言 —— "栈顶应当就是 `PopSeh` 声称要关的那条区域"。

一跑就响，而且是**立刻**（一碰嵌套 try）：

```
PopSeh(region block5) 弹到的是 region block1 —— 线性扫描的位置顺序与 SEH 嵌套不一致
PopSeh(region block1) 弹到的是 region block5
```

⇒ 那段注释自陈的"`pop_seh` 可能弹错作用域"**是真的**，不是保守猜测。

### 5.2 为什么"按身份删"不够

第一直觉是"把盲目 `pop()` 换成按身份删"。**但那样也不对**：一条区域有 **0/1/2 个**
`PopSeh`（§3.3 实测），线性扫描遇到第一个就把区域删了，于是
**catch 里的代码在这条区域的保护之外**（它本来就该在内 —— `lower_try` 的注释明说
catch 处不 pop 记录）。扫描顺序是布局顺序（RPO），内层 `catch` 可能排在 `try` 体之前，
所以"早删"会漏边，"晚删"会留错边，两种顺序都可能出现。

**根因**：`Throw` 需要的是"**这个块在哪个区域内**"，而这是一个**CFG 性质**，
不是一条按块遍历就能维护的栈。

### 5.3 做法

`ssabuilder::compute_active_regions`：CFG 数据流

```
in[b]  = ∪ out[pred]                （入口块为空）
out[b] = transfer(b, in[b])         （块内：PushSeh 加、PopSeh 删）
```

- 单调（起步全空、逐轮只增）⇒ 必然收敛；区域数有限。
- **并集而不是交集是刻意的**：多一条边只会让 phi 参数多写一份（写进一个本来就会被
  覆盖的槽），**少一条边才是错的**（读数旧版本）。
- 每个块的运行集合从 `in[b]` 起算，**不再跨块携带** —— 这是与旧实现唯一的实质差别。
- 集合用 `BTreeSet` ⇒ 边的顺序确定 ⇒ 同一份源码生成同一份字节码
  （项目在 `determine_phi_nodes` 里已为同类问题排过序）。
- `finally_outer_scope`（`ResumeException` 用）机制不变，只是它的输入从"线性栈"
  换成"**此刻**已活跃的集合"，语义更准。

### 5.4 发现 1：这是一个**可观察的静默错编**（真 bug）

最小复现（**默认的常驻内存模式下**就错，不需要任何开关）：

```js
function f() {
  var x = 1;
  try {
    try { x = 2; } catch (e) {}
    throw 0;
  } catch (e2) { return x; }     // 应当返回 2
}
f()
```

| | 结果 |
|---|---|
| node | `2` |
| 修复前 | `1` ← 外层 catch 读到**旧版本**的 `x` |
| 修复后 | `2` |

机制：内层区域的 `PopSeh` 弹掉了**外层**区域 → 之后那个 `throw` 收不到"外层 catch"这条边 →
`throw_to_handlers` 里没有它 → **外层 catch 的 SSA phi 参数不被写入**。

**全量 test262 覆盖不到它**（修前修后 16561/7843/3147 逐字节一致）。
这是本项目第三次"test262 全绿但语义错"（前两次见 `interpreter-refactor.md` §3.3.6 与 §8）。
已钉成 `tests/features/control_flow.rs::a_throw_after_a_nested_try_reaches_the_outer_catch_with_the_current_value`
（**验证过：修复前红、修复后绿**）。

### 5.5 发现 2：另一个**独立既有 bug**（登记为 known bug #4）

写回归测试时撞上的，与本次改动**无关**（实测在 `2d80a6e` 上同样失败）：

```js
function f() {
  try {
    try { throw 1; } catch (e) {}   // 内层真的捕获过一次
    throw 2;                        // 这个却逃到顶层
  } catch (e2) { return 'caught:' + e2; }
}
f()
```

| | 结果 |
|---|---|
| node | `caught:2` |
| 我们（修前修后都一样） | 抛到顶层 |

**根因（已定位）**：`handle_throw` 在 **catch 路径**上
`self.state.seh_stack.pop()` 之后**没有把记录压回去**就跳进 `handler_pc`；
而 `lower_try` 在 catch 末尾**还会发射一条 `EndTry`**（也 pop 一次）
⇒ **多弹一层，吃掉外层记录**。

**为什么常见写法侥幸正确**：被抓住的区域就是最外层时，多出来的那次 pop 是空操作
（空栈 `pop()` 返回 `None`，被忽略）。所以它只在"**嵌套** + 内层**真的捕获过**"时现形。

这与 `lower_try` 里"catch 处不 pop SEH 记录"的注释**直接矛盾** ——
那条注释描述的才是设计意图（一次 push 对一次 pop，记录活过整个 catch）。
顺带：`SehRecord.catch_executed` 在当前行为下几乎不会置位。

**归属**：运行期 `seh_stack` 记账的缺陷。M2 按构造消除它（`Frame.try_stack` +
completion 传递里没有"什么时候该 pop 这条栈"这个问题）。
在此之前登记为 `tests/features/known_bugs.rs` 的 #4（`#[ignore]`，含 4 条断言，
含一条"没有嵌套时正常"用来界定范围）。

### 5.6 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 单元 | 204 | **204** | 不变 |
| feature | 524 / 3 ignored | **525 / 4 ignored** | +1 回归测试、+1 登记的已知 bug |
| 护栏 | 7 | **7** | 不变 |
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | 逐字节一致 |
| 逐套件对比 | —— | —— | **无逐套件回退** |
| guards | timeout 7 / step-limit 0 / memory 22 | **同值** | 第三次同值 |
| 告警 | lib 24 / lib test 26 | **同** | 无新增 |

**回归测试的"红→绿"证据**：`git stash` 掉 `ssabuilder.rs` 后该测试 FAILED，恢复后
passed（两个方向都实测过，不是只跑了一次绿）。

### 5.7 已知未解 / 下一步

1. **known bug #4 未修**。修法（"`handle_throw` 在 catch 路径上把记录压回去"）看起来
   只有几行，但它是运行期 EH 记账的核心，且 `architecture-redesign.md` §4.6 的 M2 会
   整体替换这段机制。**权衡后不在本步混做**：本步是编译器侧（异常边），
   那一条是运行期侧（`seh_stack`），混在一起会让回归噪声无法归因。
   若要提前修，应单独提交 + 单独验证（尤其要验 catch 里 `return`/`break` 时
   记录不泄漏）。
2. `compute_active_regions` 是 `O(块数 × 区域数)` 的迭代，CFG 小，
   但若将来 CFG 变大应换 worklist + 只重访受影响的块。
3. 数据流只服务 `throw_to_handlers`。M2 之后"某块在哪个区域里"会由
   `Frame.try_stack` 在运行期直接给出，这份编译期计算届时可删。

---

## 6. M1.3 `CodeBlock` per function（2026-10-07）—— 六张按函数建的表并成一个

### 6.1 勘察结论（先量再动）

文档里写的"44 处 `module.` 扇出"实测**只有 38 处**，而要迁移的四个字段
（`generators` / `asyncs` / `derived_ctors` / `exit_pc`）在 `vm/mod.rs` 里**一共只有 10 处引用**。
加上 `func_info`（1 处）与 `symtab`（5 处），工作量比预想小一个量级。

### 6.2 做到哪一步，以及为什么**没有**做得更多

**做了**：`Module` 上"按函数属性"的六张表（入口 pc、名字/arity、生成器 / async /
派生构造器三个 id 集合、尾 `Ret` 的 pc）**并成一个 `Vec<CodeBlock>`**（下标 = 函数 id）。
`Module` 的公开字段从 10 个降到 6 个；运行期一律走 `Module::code_block(id)` 与四个
具名谓词（`is_generator` / `is_async` / `is_derived_ctor` / `func_exit_pc`）。

**没做，以及理由**（都记下来，免得下次以为是漏了）：

| 没做 | 理由 |
|---|---|
| 把 `instructions` / `constants` 搬进 `CodeBlock` | **pc 现在是全局的**（`Frame.pc` 直接索引 `module.instructions`，哨兵是 `instructions.len()`）。搬进去就要先让 pc 变成函数内局部 —— 那是 M2 帧模型的内容（§4.3 / §4.10） |
| 彻底删掉 `module: &Module` 参数 | `materialize_function` 的 16 个调用点里，`as_object_value` / `get_member` / `set_member` / `delete_member` 都没有 `module`，穿进去会经 `get_member`/`set_member` 级联成几十处。文档 §3.4 许可的中间态是"先放一个 `current_code`，再删参数"，所以 VM 上留了一份 `current_code: Vec<CodeBlock>` 快照 —— **类型与 `Module::code_blocks` 完全一致**（不再是 `HashMap<u32,(String,usize)>` 那种另一种形状）。M2 让帧持有 `code` 之后删掉它 |
| 把 EH 表挂进 `FunctionBody` | `FunctionBody` 仍是 `{start,end}` 派生视图；EH 表在 `Module.eh_regions`。合并要等 M2 决定"区域在帧里长什么样" |

### 6.3 一个**不是 bug 的**观察（写进文档与测试，免得后人误改）

`CodeBlock.name` 对**对象字面量方法**是**空串**：

```js
var obj = { *m() { yield 2; } };
obj.m.name        // "m" ← 运行期 SetFunctionName 补的（实测）
```

即"编译期声明的名字"与"最终的 `fn.name`"不是一回事：NamedEvaluation（ES 15.2.3 的
`SetFunctionName`）在运行期给属性方法 / 赋值目标补名。所以 `CodeBlock.name` 为空的**两种**
含义必须分清：

- **空串是最终答案**：匿名函数表达式、数组元素（`[function(){}][0].name === ""`，实测）；
- **空串是中间态**：对象字面量方法（运行期会补成 `"m"`）。

已写进 `CodeBlock::name` 的文档注释，并在 `tests/features/functions.rs` 补了
生成器方法名与数组元素名两条断言（原本只覆盖普通方法）。

**新守卫**：`code_blocks_describe_every_function`（单元）—— 钉住"每一项都描述的是
**它自己那个**函数"。归并多张表最容易出的错不是漏搬，而是**搬错行**（把 A 的标志挂到 B 上），
而那类错在别处全绿。它验：三类标志各自只落在自己身上、方法形态的生成器也带标志
（B24 的根因类别）、入口 pc 两两不同且落在指令流内、`exit_pc` 有值 ⇔ 那个 pc 上真是 `Ret`。

### 6.4 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 单元 | 204 | **205** | +1（`code_blocks_describe_every_function`） |
| feature | 525 / 4 ignored | **525 / 4 ignored** | +2 条断言（并进既有测试，不新增 test 函数） |
| 护栏 | 7 | **7** | 不变 |
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | 逐字节一致 |
| 逐套件对比 | —— | —— | **无逐套件回退** |
| guards | timeout 7 / step-limit 0 / memory 22 | **同值** | 第四次同值 |
| 告警 | lib 24 / lib test 26 / features 8 | **同** | 无新增 |

本步是**纯数据布局**改动（同一份数据换了一种组织方式），运行期行为不应变 —— 数字印证了这一点。

### 6.5 已知未解 / 下一步

1. `VM::current_code` 是**过渡态**（见 6.2），M2 删。
2. `Module::code_blocks` 与 `symtab` 的职责已经分出：前者是"每个函数是什么"，
   后者连入口 pc 都在 `CodeBlock.start` 里了 —— 所以 `symtab` 这个名字已经不存在了。
   注意 `FunctionId` 仍在 `bytecode.rs` 里（`Display` 与 `FunctionBody` 的文档提到它），
   但运行期不再有"用 HashMap 查函数入口"这件事。
3. **M1 到此收口**。下一步是 **M2（执行模型）**：寄存器 arena + `Frame{base,len}` +
   单层主循环 + 唯一调用入口。它是深改里收益最大、也是唯一需要重写 `run_instruction`
   的一步。进入 M2 的前置清单（都已经就位）：
   - 区域表可信 + VM 以表为准（M1.1/M1.2-a）；
   - 异常边不是靠物理顺序猜的（M1.2-b）；
   - 每个函数的属性自包含（M1.3）；
   - 尚未处理：known bug #4（`handle_throw` 在 catch 路径多弹一次）——
     M2 的 `Frame.try_stack` 按构造消除它，但**要在 M2 里显式验**（那条 `#[ignore]` 测试
     就是验收标准：去掉 `ignore` 应当变绿）。

---

## 7. M2 施工子计划（2026-10-07 勘察后写成）

> 写成文档的理由：M2 是全深改里唯一要重写 `run_instruction` 的一步，
> 而它的**实际改动面**（哪些字段、多少处引用、几条独立的收帧路径）只能靠实测枚举，
> 不能靠印象。下面这份清单就是施工图；下个会话不必重做勘察。

### 7.1 实测的改动面（`grep -c`，`src/vm/mod.rs`）

| 字段 | 引用点 | 归属 |
|---|---|---|
| `self.state.seh_stack` | 29 | B（跨帧水位） |
| `self.state.rbp` | 26 | A（帧内）/ 共享栈水位 |
| `self.state.closure_var_stack` | 23 | B（以"空 map"为帧边界） |
| `self.state.rsp` | 22 | A / 共享栈水位 |
| `self.state.pc` | 18 | A（帧内） |
| `self.state.construct_stack` | 12 | B（平行栈） |
| `self.state.new_target_stack` | 12 | B（平行栈） |
| `self.state.this_val` | 9 | A（帧内） |
| `self.state.this_stack` | 8 | B（保存**调用方**的绑定） |
| `self.invoke_boundaries` | 8 | B（= ctrl 深度水位） |
| `self.state.this_state` | 7 | B（平行栈，且非纯栈操作，见 7.5） |
| `self.state.frame_argc` | 7 | B（平行栈 + 参数越界规则） |
| `self.state.ctrl_stack` | 6 | B（每帧 3 项） |
| `self.state.function_stack` | 4 | B（保存调用方） |
| `self.state.data_stack` | 4 | A / 共享值栈 |
| `self.state.registers` | 4 | **C（全局！）** |

⇒ **平行栈合计约 116 处 + `rsp`/`rbp`/`pc` 66 处**。规模是"机械但多"，不是"深"。

### 7.2 三条**独立**的收帧路径（必须同时改，漏一条就出诡异 bug）

| # | 路径 | 位置 | 触发 |
|---|---|---|---|
| 1 | `Ret` 的正常收帧 | `step` 的 `Ret` arm | 函数正常返回 |
| 2 | `drive_bytecode_frame` 的收尾 | `drive_bytecode_frame` 尾部 | 嵌套循环退出 / 异常**逃出**该帧 |
| 3 | 异常回退 + 生成器状态 | `unwind_frames_to` / `restore_execution_state` / `restore_generator_frame` | `handle_throw` 跨帧 / 挂起帧恢复 |

三条路径现在各写一遍"回退哪些栈到哪个水位"，字段清单**必须保持一致**。
`Frame` 化之后它们应当收成一个助手（`pop_frame_to(depth)` 之类），这是 M2 的
主要正确性收益之一。

### 7.3 目标的 `Frame`（M2 结束时的形状）

```rust
pub struct Frame {
    pub code: u32,              // 函数 id（M2 时先存 id；M3 之后是 Gc<CodeBlock>）
    pub func: Value,            // 当前帧的函数对象（`function_val` 的替代）
    pub pc: u32,                // 指令指针（**唯一**来源，取代 state.pc）
    pub base: u32,              // 寄存器窗口起点（取代 rbp）
    pub locals_len: u32,        // 窗口长度 = 本帧局部+临时+实参区
    pub argc: u32,              // 实参个数（`Arguments` / `MakeRest` / 越界规则）
    pub this: Value,
    pub this_state: u8,         // THIS_NONE / THIS_DERIVED_UNBOUND / THIS_DERIVED_BOUND
    pub new_target: Value,
    pub construct: bool,
    /// 返回后把 `Rv` 送给**调用方**的哪个寄存器（取代 return_pc 的一部分语义）
    pub dst: Register,
    pub return_pc: u32,         // M2 阶段仍是绝对 pc（哨兵还在，见 7.4）
    /// 水位的**绝对值**（不是长度）：收帧时 truncate 到它们
    pub closure_depth: u32,
    pub seh_depth: u32,
    /// 本帧的 try 嵌套（M2 先沿用 `seh_stack` 的切片，M2-3 再收进帧）
    pub try_top: u32,
}
```

`State`：`frames: Vec<Frame>` 取代 `ctrl_stack` + `this_stack` + `this_state` +
`function_stack` + `frame_argc` + `construct_stack` + `new_target_stack` +
`function_val` + `this_val` + `pc`。

### 7.4 分阶段（每阶段一次提交、一次全量验证）

| 阶段 | 内容 | 是否改行为 | 改动面 |
|---|---|---|---|
| **M2-1** | `Frame` + `frames` 落位；把"帧身份"整组字段搬进去（`pc` / `function_val` / `this_val` / `this_state` / `frame_argc` / `construct` / `new_target` / `return_pc` / `closure_depth` / `seh_depth`）；三条收帧路径收成一个助手。**保留**嵌套 `step` 循环与哨兵 pc | 否（纯搬运） | ~110 处 |
| **M2-2** | 单层主循环：`run_until(boundary)`；**删哨兵 pc**；`invoke` 改成"跑到 `frames.len()==boundary`"；删 `PushC`/`PopC`/`MovC` | **是** | 中 |
| **M2-3** | `registers` 从全局 `[Value;19]` 变成帧窗口的一部分（`stack[base + reg]`）；`data_stack`/`rsp`/`rbp` 并入 `stack` + `Frame{base,len}` | 部分 | 中 |
| **M2-4** | known bug #4 的验收：去掉 `known_bugs.rs` 那条 `#[ignore]`，应当变绿 | 是（修 bug） | 小 |

**顺序理由**：M2-1 是纯搬运（噪声可归因）；M2-2 才是行为变化，必须等 M2-1 把"帧的
字段"定死之后再做 —— 否则每改一次边界概念都要再动一遍 110 处。

### 7.5 四个必须小心的地方（勘察实测出来的，不是推测）

1. **`Ret` 要在 pop **之前**抓两样东西**：`frame_ctor_state`（`this_state.last()`）与
   `frame_this`（`this_val`）。顺序错了，构造器的返回值语义就错
   （`derived-class-return-override-catch.js` 一族）。
2. **`this_state` 不是纯栈操作**：`enter_frame` 压 `THIS_NONE`、
   `mark_this_uninitialized` 会**补齐到 `this_stack.len()`** 再写 UNBOUND、
   `CallSuperSpread` 会**逆序回填**找到 UNBOUND 槽改成 BOUND。
   ⇒ 搬进 `Frame` 时不能只做 `push/pop`，要保留"按帧回填"的语义。
3. **`registers` 现在是全局的**（代码与注释都明说）：所以嵌套运行（生成器体 /
   `invoke`）必须靠 `SavedExecutionState::registers` 与 `SuspendedFrame::registers`
   来回搬。M2-3 把它变成帧窗口的一部分之后，**这两处搬运可以整块删掉**
   （`extract/restore_generator_frame` 的清单少两项）。这是 M2 顺带拿到的收益。
4. **`closure_var_stack` 以"空 map"当帧边界**（`take_pending_captured_vars` 靠它停止）。
   搬进帧时要变成 `Frame.closure_depth` 的绝对值 + 一个显式的"本帧捕获表"，
   不能继续依赖"遇到空 map 就停"这种隐式约定。

### 7.6 M2-1 的切片顺序（按字段组，每片可独立验证）

| 片 | 字段组 | 引用点 | 备注 |
|---|---|---|---|
| **1a** | `pc` + `ctrl_stack` 的三项 + `function_val` | 18+6+11 | `Ret`/`open_frame`/`drive_bytecode_frame` 一起改；`ctrl_stack_reached_bottom()` → `frames.is_empty()` |
| 1b | `this_val` + `this_state` + `frame_argc` + `this_stack` + `function_stack` | 9+7+7+8+4 | 注意 7.5 第 2 条 |
| 1c | `construct_stack` + `new_target_stack` | 12+12 | 与 `Frame.construct`/`new_target` 一一对应 |
| 1d | `closure_depth` + `seh_depth` 水位 | 23+29 | 这一片之后三条收帧路径可以合并 |
| 1e | 三路收帧合并成一个助手 | —— | 正确性收口，独立提交 |

### 7.7 M2-1 的迁移机制（先证等价，再切换 —— 沿用 M1 的做法）

> **订正（B 步开工后）**：本节的 §7.6 切片顺序已被 **§9.5** 取代 ——
> B 步又证伪了两个字段（`func` / `pc`），并发现 **`pc` 必须与"顶层脚本变成帧"一起做**。
> 以 §9.5 的顺序为准。

> **订正（A 步实测后）**：下面把"可变字段"只列成 `pc` / `this_state`。实测 `this`
> 与 `func` 也在其中 —— `super()` 会按 ES `BindThisValue` 在**中帧**绑定派生 `this`
> （`CallSuperSpread` 里那处写 `this_val`）。详见 §8.3 第 3 条。

110 处搬运如果"改一处删一处"，中途任何一次提交都处在半迁移状态，回归噪声无法归因。
所以**每个切片走两步**：

- **A 步（镜面 + 断言）**：新建的 `frames` 与旧的平行栈**同时维护**，并在三条收帧路径的
  出口加 `debug_assert`：`frames.last()` 的每个字段都等于对应平行栈的栈顶。
  读点仍然读旧字段 ⇒ **行为零变化**，而"新结构被正确维护"这件事被真实数据检验过。
- **B 步（翻转读点）**：把该字段组的读点逐个改成读 `frames.last()/frame_mut()`，
  断言仍在（此时它校验的是"写点还没换"）。全组翻完后，删掉旧字段与断言。

这比"M1.1 先加数据再切换"更进一步：M1.1 里新数据是**新增**的，M2-1 里新数据是
**同一份数据的第二种存放**，所以必须在切换完成前一直互校。

> **勘察发现的额外隐患（顺带被 `Frame` 消除）**：`ctrl_stack` 里混着两种长度的条目 ——
> 帧组是**每帧 3 项**（`return_pc` / seh 深度 / closure 深度，见 `open_frame`、
> `Call`、`New`、`push_generator_frame`、`restore_generator_frame`），
> 而 `PushC`/`PopC` 两条**指令**（各压/弹 **1 项**，成对使用时共 2 项：`rsp` / `rbp`；
> codegen 发射 `PushC rsp` + `PushC rbp` 这样的一对）。
> 于是 `Ret` 判断"我是不是最外层帧"只能靠 `ctrl_stack.is_empty()`
> （`State::ctrl_stack_reached_bottom`），而它同时被两种记账影响 —— 这是"两条知识
> 混在一条栈上"。`frames` 化之后这个判断变成 `frames.is_empty()`，与 `PushC`/`PopC`
> 的簿记彻底分开（后者在 M2-2 随指令一起删）。

### 7.8 验收（每个切片）

- 全量 test262 **逐字节不变** + 零逐套件回退（M2-1 是纯搬运，这是硬要求）；
- `guards:` 三个计数不变；
- 定向：`language/statements/try`（SEH）、`generators` ×2、`class` ×2、
  `language/expressions/arrow`（`this` 捕获）、`language/statements/for-of`（`delegate_stack`）；
- **新增守卫**：`frames` 与已删除字段的"此刻一致"断言在 M2-1 期间可临时保留
  （旧的平行栈先不删、只在末尾断言与 `frames` 同步），最后一片再删旧字段 ——
  这是 M1 用过的"先证等价再切换"，本轮继续用。

---

## 8. M2-1a 的 A 步：`frames` 镜面 + 互校（2026-10-07）

### 8.1 做了什么

- 新增 `struct Frame { argc, construct, new_target, closure_depth, seh_depth, return_pc, ctrl_base }`
  与 `State::frames: Vec<Frame>`。
- 5 个开帧点（`open_frame` / `Call` / `New` / `push_generator_frame` /
  `restore_generator_frame`）在三连 `pushc` 与 `construct`/`new_target` 的 push **之后**
  调 `note_frame(closure_depth, seh_depth, return_pc)` —— **传的就是刚 push 进
  `ctrl_stack` 的那三个值**，不重新算一遍。
- 5 个收帧点（`Ret` / `unwind_frames_to` / `discard_generator_frame` /
  `drive_bytecode_frame` / `restore_execution_state`）同步 `frames`。
- `State::assert_frame_mirror()` 在 `step` 每条指令前跑（debug）：
  `frames.len() == this_stack.len()`、`argc` / `construct` / `new_target` /
  三连（按 `ctrl_base` 取）逐项相等。
- **读点一个没改** ⇒ 行为零变化。

### 8.2 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 单元（debug+release） | 205 | **205** | 不变 |
| feature（debug+release） | 525 / 4 | **525 / 4** | 不变；**debug 下互校在全部 525 条上成立**（覆盖生成器 / `super()` / proxy / try-finally） |
| 护栏 | 7 | **7** | 不变 |
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | 逐字节一致 |
| 逐套件对比 | —— | —— | **无逐套件回退** |
| guards | timeout 7 / step-limit 0 / memory 22 | **同值** | 第五次同值 |
| 告警 | lib 24 / lib test 26 / features 8 | **同** | 无新增 |

`assert_frame_mirror` 每次 `step` 都跑，debug 下 525 条 feature 约 6.2s
（不开时约 5.2s）—— 约 +20% 的 debug 时间，换来"开/收帧记账不分叉"的**全部**路径覆盖。
这个代价只在 debug，可以接受。

### 8.3 三个实测教训（都值得写下来）

**1. 帧组的三项不是 `ctrl_stack` 的末三项。**
第一版互校按"末三项"写，一跑就被打红：`镜面 return_pc 与 ctrl_stack 末项不一致
left: 4 / right: 2`。原因：`PushC`/`PopC` 这对**指令**会把 `rsp`/`rbp` 压在**本帧三连
之上**（它们是插在帧中间执行的）。所以镜面必须记下**本帧三连的起始下标** `ctrl_base`
（`PushC`/`PopC` 成对出现 ⇒ 只要本帧活跃，这个下标就仍然指向它的三连）。
⇒ 这是 §7.7 那条"`ctrl_stack` 里混着两种长度的条目"的具体后果。

**2. `debug_assert_eq!` 在失败时会格式化 `Value`，而 `Value` 的 `Debug` 会递归进对象图
—— 真正的失配被"栈溢出"掩盖了。**
现象：`class C extends Error` 的用例变成 `has overflowed its stack`（把测试线程栈加到
256MB 也照炸 ⇒ 无界递归）。实际原因只是互校断言失败，而**失败信息本身**把栈打爆。
二分（先关掉断言）才定位到是断言。

⇒ **通用教训（不只 M2）**：在这份代码里，任何可能失败并打印 `Value` 的断言都要写成
`debug_assert!(a == b, "…")`，不要用 `debug_assert_eq!(a, b)`。已写进
`assert_frame_mirror` 的文档注释。

**3. `this`（与 `func`）属于"中帧也会写"那一组，不能进 A 步的镜面。**
断言在 `class C extends Error` 与 `proxy::apply_and_construct_traps` 上报
"非帧边界的写入"。根因：`CallSuperSpread` 在 `super()` 返回后按 ES 12.3.5.1
`BindThisValue` 把派生类的 `this` 换成父构造器产出的对象 —— 它和 `this_state` 的
BOUND 回填是同一件事的两半。

⇒ 这**修正了 §7.7 的分类**：可变字段不只是 `pc` / `this_state`，`this` 也在其中。
所以 `Frame` 现在只放能严格互校的集合（水位 + 返回地址 + 每帧 flag/argc），
`pc` / `this` / `func` / `this_state` 留到 B 步**直接搬**（那时以字节码不变为证，
而不是靠镜面）。已在 `CallSuperSpread` 那处写 `this_val` 的地方留了注释：
**B 步要把这一处一起改**。

### 8.4 下一步

B 步：把读点从平行栈翻到 `frames`，按 §7.6 的切片顺序（1a 先做 `pc` + 三连 + `func`；
`pc`/`this`/`func`/`this_state` 直接搬，其余翻读点）。全组翻完后删旧字段与互校断言。

---

## 9. M2-1a/B 开工前：又两个字段被证伪进不了镜面，并**定位到一处潜伏记账分叉**（2026-10-07）

### 9.1 做了什么

按 §7.6 的切片 1a，先把 `function_val` 加进镜面试互校（它与 `this` 一起被移出过镜面，
但 `this` 才是那次失败的那个，`func` 从未被单独证实）。**它也不成立** —— 于是回退这次试探，
把结论与根因记下来，**不留半迁移状态**（树保持绿：单元 205 / feature 525）。

### 9.2 实测数据（不是推测）

`proxy::apply_and_construct_traps` 上互校报错，加临时诊断（不格式化 `Value`）打出函数号：

```
M2-1 诊断(func): pc=380 frames=1 function_stack=1
                 mirror=objFn#5   state=objFn#1   fn_stack_top=undef
```

即：`frames.last().func` 是 `func_id = 5` 的那个函数对象，而 `state.function_val` 是
`func_id = 1` 的，且 `function_stack` 栈顶是 `undefined`（说明这一帧是从**脚本层**开的）。

### 9.3 根因：`SavedExecutionState` 独立保存/恢复 `function_val`，却不保存帧身份

关键推理：`function_val` 在全文件**只有 9 处赋值，且全部在帧边界上**
（`Ret` / `open_frame` / `Call` / `New` / 两处生成器 / `unwind_frames_to` /
`drive_bytecode_frame` 的恢复 / `restore_execution_state`）。所以分叉**不可能**来自那些写点。

真正的原因在最后那一处：`save_execution_state` 把 `function_val` 快照进
`SavedExecutionState.function`，`restore_execution_state` 再把它写回来 ——
**但它只快照了这个值，没有快照帧身份**。于是一次嵌套运行（生成器体 / `invoke` /
`drive_bytecode_frame`）之后，`function_val` 可以与"当前帧是谁"对不上。

⇒ 这是一处既有的**潜伏记账分叉**：当前无观测影响（`function_val` 只被
`LoadCurrentFunction` / `MakeArrowFuncObj` / `current_function_id` 读，都不是控制流依据），
但它是真的，而且它正好说明**为什么 `func` 应该进 `Frame`** ——
搬完之后"调用方是谁"由帧决定，分叉按构造消失。

### 9.4 由此修正的施工口径

| 字段 | 能否进 A 步镜面 | 理由 | B 步怎么搬 |
|---|---|---|---|
| `argc` / `construct` / `new_target` / `closure_depth` / `seh_depth` / `return_pc` | **能**（已落，全绿） | 只在帧边界写 | 翻读点 |
| `this` | **不能** | `super()` 按 `BindThisValue` 中帧绑定 | **直接搬**（已在那处写好注释提醒） |
| `func` | **不能** | `SavedExecutionState` 的独立恢复（§9.3） | **直接搬**，顺带删掉 `function_stack`（调用方是谁改由 `frames[len-2]` 给） |
| `pc` | **不能** | 每条指令都改 | **直接搬**；前提是先把**顶层脚本也变成一个帧**（否则 pc 无处可放） |
| `this_state` | **不能** | `CallSuperSpread` 按帧回填 BOUND | **直接搬** |

⇒ **`pc` 与"顶层脚本也是一个帧"必须一起做**（§4.3 陷阱 5 早就写了这一条，现在有了硬理由：
不先把脚本变成帧，`pc` 就没有唯一的家）。

### 9.5 对切片顺序的影响（更新 §7.6）

原顺序是 1a（`pc`+三连+`func`）→ 1b → 1c → 1d → 1e。按上面的证伪结果，
**1a 应当拆开、并把"脚本帧"提到最前**：

| 新顺序 | 内容 | 说明 |
|---|---|---|
| **1a** | **顶层脚本变成帧** + `pc` 进 `Frame` | 两者必须同时做；`Ret` 的"是不是最外层"改判 `frames.len() == 1`；`ctrl_stack_reached_bottom()` 退役 |
| **1b** | `ctrl_stack` 的三连退役（只留 `PushC`/`PopC`） | 三连**已经**被镜面证明正确，所以这一片是纯翻读点；顺带把 `invoke_boundaries` / `SehRecord.saved_ctrl_depth` 改成帧深度 |
| **1c** | `func` 直接搬 + 删 `function_stack` + 删 `SavedExecutionState.function` | 顺带消掉 §9.3 那处分叉 |
| **1d** | `this` / `this_state` 直接搬（含 `super()` 那处中帧写） | 与 1c 同批也可，但 `this_state` 有"按帧回填"语义，单独更清楚 |
| **1e** | `construct` / `new_target` / `argc` 的读点翻到 `frames`，删这三条栈 | 已镜面证明 |
| **1f** | `closure_depth` / `seh_depth` 水位翻到 `frames`，删对应簿记 | 已镜面证明；这一片之后三路收帧可以合并成助手 |

---

## 10. M2-1a：顶层脚本变成帧（2026-10-07）—— 以及 `pc` 为什么**不能**单独搬

### 10.1 做了什么

**顶层脚本现在也是一个帧**（`architecture-redesign.md` §4.3 陷阱 5 那条，终于落地）：

- `State::open_script_frame(return_pc)`：与 5 个开帧点**完全同构**
  （`enter_frame(0)` + 控制栈三项 + `note_frame` + `jump(0)`），`run` 在任何指令之前调用它。
- `Ret` 的"是不是最外层"从 `ctrl_stack.is_empty()` 改成 `frames.len() == 1`；
  `State::ctrl_stack_reached_bottom()` **删除**（它问的是"控制栈空不空"，
  而控制栈上还混着 `PushC`/`PopC` 的条目 —— 两条知识混在一条栈上）。
- `enter_frame` 的递归深度从 `ctrl_stack.len() / 3` 改成 `frames.len() - 1`
  （**调用帧**数；脚本帧不是一次调用）。
  旧写法只有"`PushC`/`PopC` 最多贡献两项、整数除法恰好不出错"才成立 —— 是个侥幸，
  现在一并去掉。

**为什么抽成方法而不是让 `run` 内联**：单元测试也需要"一个已经有帧的 `State`"，
而它必须与运行期压的是**同一种**帧，否则测试验的是别的东西。

### 10.2 踩到的第一个坑：脚本帧必须与 5 个开帧点**逐字同构**

第一版只写了 `enter_frame` + `note_frame`，没压控制栈那三项
（那三项在 5 个开帧点里是内联的）。结果 `note_frame` 里的
`ctrl_base: self.ctrl_stack.len() - 3` **整型下溢**，在 debug 下当场炸。

⇒ 这不是"少写三行"，而是揭示了一条**拼图式的耦合**：镜面互校按"每帧三项"取
`ctrl_base`，所以**任何**帧都必须有那三项 —— 包括脚本帧。
切片 1b 会把这三项连同 `ctrl_stack` 一起退役，届时这条约束自然消失。

### 10.3 踩到的第二个坑（**重要**）：`pc` 不能单独搬 —— 它与哨兵 pc 是绑死的

按 §9.5 的计划，1a 应当"脚本帧 + `pc` 进 `Frame`"一起做。实际做了 `pc` 之后：

- 单元测试全绿（205），
- 但 feature **大面积失败**，症状是完成值变成对象：

  ```
  [1,2,3].map(function(v){return v*2;}).length   →  报 [object Object]（应为 3）
  ```

**根因**：`Ret` 用 `jump(return_pc)` 把控制权交回调用方，而 `return_pc` 在
`invoke` 那条路上是**哨兵**（`module.instructions.len()`）；嵌套 `step` 循环的出口
正是"pc 越界"。`pc` 一旦变成**每帧一个**：

1. `Ret` 先 `frames.pop()`（被调帧没了），紧接着 `jump(return_pc)` 就写进了
   **调用方那一帧** —— 把调用方的 pc 覆盖成哨兵；
2. 于是 `step` 下一次取指令就"越界"，**主循环直接退出**；
3. 程序在 `map(...)` 之后就停了，完成值留在 Rv 里正好是那个新数组。

补一句：`drive_bytecode_frame` 里那个 `saved_pc` 之所以存在，就是为了兜住这个
"调用方 pc 被被调帧破坏"的场景 —— 所以删它 + 搬 `pc` 是同一个决定的两半。

⇒ **结论：`pc` 必须与"循环改成 `frames.len() > boundary`"一起做**
（删哨兵 pc、删 `return_pc`、去掉 `drive_bytecode_frame`/`run_generator_frame` 的
"靠 pc 越界出口"）。这本来就是 §5.2 的第 4 步（M2-2 的核心），
现在有了硬理由说明它**不能推后**。已把这条写进 `State::pc` 的字段注释（就在代码里，
不会随文档漂走）。

**处置**：回退 `pc` 那一半，只提交**脚本帧**这一半（它自包含、可验证、全绿）。

### 10.4 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 单元 | 205 | **205** | `test_state_jump`/`test_state_jump_offset` 回到直接操作字段；新增 `the_script_runs_in_a_frame` |
| feature（debug，镜面互校激活） | 525 / 4 | **525 / 4** | 不变 |
| 护栏 | 7 | **7** | 不变 |
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | 逐字节一致 |
| 逐套件对比 | —— | —— | **无逐套件回退** |
| guards | timeout 7 / step-limit 0 / memory 22 | **同值** | 第六次同值 |
| 探针（本次两处坑的现场） | —— | `3,false,2,6` ✓ | 与期望一致 |

新增守卫：`the_script_runs_in_a_frame` —— 钉住"脚本帧是 `frames[0]`、
与其它帧同构（`this_stack` 也有一条）、不是一次调用（深度按 `len()-1`）"。

### 10.5 修正后的切片顺序（§9.5 再订正）

| 新顺序 | 内容 |
|---|---|
| **1a′** | **脚本帧 + `pc` 进 `Frame` + 循环改 `frames.len() > boundary`（删哨兵、删 `return_pc`）** —— 三件事是一个整体，拆不开 |
| 1b | `ctrl_stack` 三连退役（只留 `PushC`/`PopC`）；`invoke_boundaries` / `SehRecord.saved_ctrl_depth` 改帧深度 |
| 1c | `func` 直接搬 + 删 `function_stack` + 删 `SavedExecutionState.function`（消掉 §9.3 的分叉） |
| 1d | `this` / `this_state` 直接搬（含 `super()` 那处中帧写） |
| 1e | `construct` / `new_target` / `argc` 读点翻帧，删三条栈 |
| 1f | `closure_depth` / `seh_depth` 水位翻帧；三路收帧合并成助手 |

**脚本帧已经就位**（本步），所以 1a′ 只剩"`pc` + 边界循环"。

---

## 11. 1a′ 试做后**回退**：`pc` 进帧 + 边界循环（2026-10-07）—— 记录全部证据

### 11.1 做了什么（已回退）

1. `Frame.pc` + `State::pc()/set_pc()`（`pc` 进帧）；删 `State.pc`。
2. `Ret` 不再 `jump(return_pc)`；改由**调用点在开帧前自己 `jump_offset(1)`**
   （4 处：`Call` / `CallEx` / `CallMethod` / `New`）。
3. 两个嵌套循环改成边界驱动：`drive_bytecode_frame` 与 `run_generator_frame`
   （`loop { if frames.len() <= boundary { break } if !step()? { break } }`）。
4. 删 `drive_bytecode_frame` 的 `saved_pc` 与 `SavedExecutionState.pc`。

### 11.2 结果：单元/feature 全绿，但**全量 test262 出现 4 条回退**

| 项 | 结果 |
|---|---|
| 单元 | **206 通过**（新增 `pc_is_per_frame`） |
| feature（debug，镜面互校激活） | **525 / 4** 全绿 |
| 全量 test262 | 通过 16561 → **16557**；`language/statements/for-of` **621 → 617**（失败 97 → 101） |

4 条新失败全是 `RangeError: stack access out of bounds`：
`break.js` / `break-from-try.js` / `break-from-catch.js` / `break-from-finally.js`。

**归属已实测判定**：在父提交上跑同一个套件 —— 621 通过、且 `stack access out of bounds`
出现 **0 次**。⇒ 是本次改动引入的，不是既有问题。

### 11.3 最小复现（二分出来）

```js
function* values() { yield 1; throw new Error('u'); }   // 关键：yield 之后有 throw
var i = 0;
for (var x of values()) { try { i++; break; } catch (err) {} }   // 关键：try 里 break
i
```

- 去掉生成器里的 `throw`（`function* values() { yield 1; }`）→ **通过**
- 去掉 `try`（`for (var x of values()) { break; }`）→ **通过**
- 普通的 `for(...){ try { break } }`、数组 for-of + `try{break}` → **通过**
- 两个条件同时满足 → `RangeError: stack access out of bounds`

### 11.4 症状与实测数据（都是打印出来的，不是推测）

**帧与值栈同时泄漏**：

```
M2-1 诊断(note): 调用方 pc=13 frames=7 ... 各帧 pc=[21, 21, 21, 21, 21, 21, 13]
M2-1 诊断(note): 调用方 pc=19 frames=7 ... 各帧 pc=[21, 21, 21, 21, 21, 21, 19]
M2-1 诊断(note): 调用方 pc=21 frames=7 ... 各帧 pc=[21, 21, 21, 21, 21, 21, 21]
M2-1 诊断(note): 调用方 pc=13 frames=8 ... 各帧 pc=[21, 21, 21, 21, 21, 21, 21, 13]
```

- 每轮泄漏**一个帧**（7→8→9…，实测涨到 15）；
- `rsp` 每轮翻倍（1797 → 3566 → 7150 → 14318 → 28654 → 57326），最终
  `rbp = 262144 = STACK_MAX`、`rsp = 262150`，在 `ensure`/`set_value_to_stack` 上抛错；
- 帧栈里**一大串 pc 全是 21** —— 而 pc 21 是 `iter_close`。

对照 dump 出来的字节码（`BIUJS_DUMP=1`）：`values()` 的调用在 **pc 12**（循环之外），
循环体是 19 `iter_next` / 21 `iter_close` / 29 `try` / 35 `delayed_jump`（break）。

⇒ 所以"一串 pc=21"**不是**同一个函数在递归，而是**脚本帧自己停在 pc 21 被当成被调方
反复重入**：某个嵌套运行把调用方（脚本）当成了它要驱动的那一帧，而脚本的 pc 恰好停在
`iter_close` 上，于是它再执行一次 `iter_close` → 再回调 → 再泄漏一层。

### 11.5 排查过的、**排除**的假设

- **边界算错？** 两个循环的边界实测都对：`drive_bytecode_frame` 是"开帧前的深度"；
  `run_generator_frame` 用 `frames.len()-1` 与改用 `saved.this_depth` **实测等价**
  （打印出来都是 `frames=2 boundary=1`、`frames=3 boundary=2`）。**不是它。**
- **哨兵泊车？** `Yield`/`PrologueEnd` 把 pc 停在哨兵上，此时写的是**生成器自己**那一帧，
  逻辑上仍然成立（帧留着是刻意的）。**不是它。**

### 11.6 未收敛的假设（下一步从这里开始）

`iter_close` / `iter_next` 是**原生**：它们会**在调用方的 pc 还没推进时**回调 JS
（pc 的推进发生在 `run_instruction` 的尾部，而原生调用发生在 arm 中间）。
所以"嵌套运行期间，调用方帧的 pc 仍指着那条调用指令"是**常态**。

在旧的全局 pc 模型下这没关系：嵌套循环只有一种退出方式（pc 越界），而且**没有第二帧
可以跑**（`pc` 只有一个）。改成每帧一个 pc 之后，"调用方被当被调方驱动"第一次成为**可能** ——
只要有一个嵌套运行的边界取成"调用方的深度"，它就会从调用方的 pc 继续跑，
而那个 pc 正好在调用指令上 ⇒ 自递归 ⇒ 帧/栈泄漏。

**下一步该先做的不是继续找这一处，而是把"边界"这个概念做成取不错的形式**：
- 由**调用方**在开帧前把 boundary 传进去（像 `drive_bytecode_frame` 现在这样），
  而不是让每个嵌套运行自己从 `frames.len()` 推；
- 更好的形态是 `Frame.return_frame`（目标帧的**下标**）：嵌套循环的退出条件是
  "当前帧下标 < 我进来的下标"，而不是比较长度 —— 长度在异常回退/生成器摘帧时会变。

### 11.7 处置

**回退 1a′ 的全部改动**，保留已提交的脚本帧（`cd88dd3`）。
树回到绿：单元 205 / feature 525 / 无残留诊断代码（`grep -c 诊断 src/vm/mod.rs` = 0）。

**结论**：1a′ 不是"机械搬运"—— 它把"谁拥有 pc"换了主人，而**哨兵泊车**与
**指令中途回调原生**这两种用法都建立在旧的全局 pc 上。这一类改动必须一次做对
（因为它同时动了控制流的三个前提），所以它值得先花一轮把边界形式定死，再动手。

---

## 12. 假设验证 + 一个**真的潜伏 bug** 被修掉（2026-10-07）

### 12.1 只用诊断验证 §11.6 的假设（不动逻辑）

在**当前绿的代码**上，只在 `generator_abrupt` 的 `start_pc` 为 `None` 那一支加一句打印，
跑 §11.3 的最小复现。结果**一句话定案**：

```
M2-1 诊断(start_pc=None): 生成器没有尾 Ret，恢复后**不 jump**；当前 pc=21 frames=2
```

而 dump 也印证：`1: start 41 .. None name "values" [generator]` —— **`exit_pc` 是 `None`**。

### 12.2 根因：把**调用方**那一条指令当成生成器的代码执行了一次

`generator_abrupt` 的 `Ok(())`（`generator.return()`）分支原本：

```rust
Ok(()) => { set Rv; module.func_exit_pc(func_id) }   // 没有尾 Ret ⇒ None
...
if let Some(pc) = start_pc { self.state.jump(pc); }  // None ⇒ 不 jump
```

于是 `run_generator_frame` 的嵌套循环从**当前 pc** 开始跑 —— 而那是**调用方**停住的地方。
实测那条路是 `for-of` + `try{break}` 触发的 `IteratorClose`，pc = 21 = 调用方的
`iter_close`：**生成器帧里执行了调用方的一条指令**。

今天它没炸，是因为"再进一次 `generator_abrupt` 会发现已无挂起帧 → `mark_generator_completed`
→ 立刻返回"，于是自己终止了。**纯属巧合**——而这个巧合一旦 `pc` 进了帧就消失：
新帧的 pc 是 0，也就是**脚本入口**，于是它从脚本开头重跑 → §11.4 那一串
`pc=[21,21,21,…]` + 帧泄漏 + `rsp` 涨到 `STACK_MAX`。

⇒ §11 的 4 条 `for-of` 回退**不是 1a′ 的设计错，而是它踩中了一个既有的潜伏 bug**。

### 12.3 修法（把"pc 从哪来"从巧合变成契约）

```rust
Ok(()) => {
    set Rv;
    // 没有尾 `Ret` 时也要**显式**定下重入点：哨兵 = "这次恢复没有代码要跑"。
    Some(module.func_exit_pc(func_id).unwrap_or(module.instructions.len()))
}
Err(err) => match self.deliver_into_frame(err, saved.ctrl) {
    // 异常交付**进帧里**：`handle_throw` 已经把 pc 设到处理器上 ⇒ **不能**覆盖。
    // 这是 `None` 唯一合法的用法（原来它被两种语义共用了）。
    Ok(()) => None,
    ...
}
```

两点都重要：**补齐缺失的那一支**，并且**把 `None` 的两种含义分开**（"没有代码要跑"
vs "pc 已由别人设定"）。原代码把这两件事压成了同一个 `None`。

### 12.4 验证

| 项 | 基线 | 本步 | 结论 |
|---|---|---|---|
| 全量 test262 | 16561 / 7843 / 3147 | **16561 / 7843 / 3147** | **逐字节一致** |
| 逐套件对比 | —— | —— | 无逐套件回退 |
| 单元 / feature / 护栏 | 205 / 525·4 / 7 | **同** | 不变 |
| guards | timeout 7 / step-limit 0 / memory 22 | **同值** | 第七次同值 |
| 最小复现 | 现在也是 `1` ✓ | `1` ✓ | —— |

**注意这条验证的含义**：全量**逐字节不变**说明那次杂散执行确实自己就终止了 ——
也就是说本次修的是**偶然性**（约定），不是可观测行为。这不是"白改"：
它正是 1a′ 的前置，而且它把一条隐式约定变成了显式契约。

### 12.5 对 1a′ 的直接影响（设计结论）

1a′ 里 `note_frame` **不能把新帧的 pc 默认成 0** —— 那正是踩中本 bug 的方式。
**新帧的初始 pc 必须由调用点显式传入**：

| 开帧点 | 初始 pc |
|---|---|
| 4 个 opcode 调用点 + `open_frame` + `push_generator_frame` | 被调函数的**入口 pc**（它们手上都有 `location`） |
| `restore_generator_frame` | **哨兵**（"等调用方 `jump` 到真正的重入点"）—— `generator_next` / `generator_abrupt` 随后会 `jump`；而 §12.3 修好之后，那条路上 `jump` 一定会发生 |

⇒ 契约：**帧要么生在入口 pc 上，要么生在哨兵上；永远不会生在"碰巧是什么"。**
