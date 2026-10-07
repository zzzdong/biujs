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
