# 深度重构方案：GC / Value 模型 / 帧与异常处理的重新设计

> 本文是 `docs/interpreter-refactor.md`（P0–P3 增量方案）的**上位方案**。
> 起因是三件事：(1) 参照实现 `ChakraCore` 就在本地，可以逐块核对而不是凭印象；
> (2) 增量做到 P1-2c 时发现，P3（帧归一 + 堆帧 + 单层循环）**本质上就是**
> ChakraCore 的 `InterpreterStackFrame`，再做一遍增量等于把同一套机器写两遍；
> (3) 重构途中连续挖出 3 个 test262 抓不到的语义 bug —— 说明现有数据模型本身在漏。
>
> **过程约束已变更**：重构未完成前允许破坏性变更（不再要求每一步零回退、零性能倒退）。
> 但"允许破坏"不等于"放任"，见 §7 的新验证协议。

---

## 1. 结论（先说结果）

**走深改（Track B），但分三步、按顺序，且第一步先把语义 bug 钉成测试。**

```
1. 语义 bug 钉成测试（3 个）+ 订正既有文档里的错判      ← 前置，必须先做
2. FunctionBody（per-function 字节码）+ EH 元数据化      ← 无生命周期风险，先做
3. Value / GC：阶段 A（循环回收）→ 阶段 B（arena + Rooted 句柄）
4. 重写的解释器循环 + InterpreterStackFrame 式帧 + 寄存器模型   ← 收益在此
```

关键顺序理由：**GC 必须在帧模型之前**——帧的局部槽（`Var*` 数组）要活在托管堆上；
先做帧再做 GC，等于把 GC 重做一遍。而 **EH 元数据化排在最前**，因为它既解锁栈的删减、
又不碰生命周期，是唯一"低风险、高解锁"的一步。

---

## 2. 参照实现的实测事实（取自 `/home/alex/code/open_source/ChakraCore`，非印象）

| | ChakraCore | 我们 |
|---|---|---|
| 整体规模 | `lib/` 41MB（含 JIT / Backend / TTD） | `src/` **38,981 LOC** |
| GC 子系统 | `lib/Common/Memory` **55,836 LOC**（`Recycler.cpp` 9,338、`ArenaAllocator.cpp` 1,630） | 无（引用计数） |
| 解释器帧 | `InterpreterStackFrame.cpp` **9,663 LOC** | 散在 `vm/mod.rs` 各 arm |
| 字节码表 | `OpCodes.h` 905 行，**macro-list**：`MACRO(opcode, layout, attr)`，多次 include 生成 enum + 布局 | `define_instrs!` 表（**同构**） |
| 值模型 | `Var`（指针宽标记值）+ `RecyclableObject*`；`Type.h` 仅 117 行 | `Value` enum + `Rc<RefCell<dyn JSObject>>` |
| 调用入口 | `OP_CallCommon` / `OP_CallCommonI` / `OP_NewCommon`（**统一入口**） | 6 个 opcode × 各自分支（P1 正在收敛） |
| 帧字段 | `Var* m_localSlots`（locals+temps 一段）、`Var* m_outParams`、`Var m_arguments`、`Var* innerScopeArray`、`RegSlot` 索引 | **9 条平行栈** + 4 处内联开帧 |
| 异常处理 | `ProcessCatch` / `ProcessFinally` / `ProcessTryFinally` / `ProcessTryHandlerBailout` + `EHBailoutData` 嵌套链，**由字节码元数据驱动** | 运行时 `seh_stack` + `DelayedJump` |

两条直接观察：

- **我们的 P0 表与他们的 `OpCodes.h` 是同一个想法**（一处声明、多次生成）。我们额外派生了
  arity / 角色 / kind，比他们那个独立的 `OpCodeAttr` 表少一处知识。这条路走对了，保留。
- **我们的 P3 就是他们的 `InterpreterStackFrame`**。所以"要不要移植帧模型"不是"要不要"，
  而是**"增量做（P3）还是重写做"**。

---

## 3. 现状的代价（具体，可核对）

### 3.1 引用计数 `Rc<RefCell<dyn JSObject>>`

1. **循环必然泄漏**：原型链、闭包、`obj.self = obj` 都是环，引用计数收不掉。
2. **借用冲突是一类运行时崩溃**：`src/` 里 **376 处** `.borrow()` / `.borrow_mut()`，
   `Rc<RefCell` 在 `object.rs` + builtins 里出现 **130 次**。这类错误**不是编译错误**。
3. 每次访问对象两层间接 + 引用计数增减。
4. 没有"堆"，也就没有真正的 GC 指标——现在"堆预算"护栏只是个代理。

### 3.2 执行模型

- **9 条平行栈**：`this_stack` / `this_state` / `function_stack` / `frame_argc` /
  `construct_stack` / `new_target_stack` / `closure_var_stack` / `seh_stack` / `ctrl_stack`。
  开一帧要逐个保存、收尾时逐个 `truncate`（已由 P1-1 收敛到一处，但栈本身还在）。
- **4 处内联开帧**（`CallEx`×2、`CallMethod`×2、`New`×1、`NewSpread`×1），为的是省掉一层
  嵌套 `step` 循环——这个动机是对的，代价是同一段开帧逻辑与记账被抄了 5 遍。
- **两层循环**：普通调用内联在分派循环里，`invoke` 另起一层嵌套 `step` 循环（P1 的
  `enter_call` + `Control` 协议正是为了把这两条合起来而不牺牲内联）。
- **异常靠运行时栈**：`seh_stack` 记录 handler/finally 的 pc，`DelayedJump` 负责跨帧跳转；
  而 ChakraCore 把它放进**函数体的 EH 元数据**。

---

## 4. 问题一：要不要换掉 `Rc<RefCell<>>`，做真 GC

**要。** 但有两个必须先讲清楚的真相，否则会低估代价：

### 4.1 难点不在收集器，在 rooting 纪律

- 那 55,836 LOC 里，很大一块是**给 JIT 的写屏障、并发/后台 GC、TTD 支持**。
  **非移动式 mark-sweep 收集器本身只要一两千行。**
- 真正贵的是"**Rust 局部变量持有 `Value`**"：收集器一旦真的回收不可达对象，栈上 Rust
  局部变量里的 `Value` 就是悬挂的。ChakraCore 靠 `Field()` / `FieldNoBarrier()` /
  `RecyclerRootPtr` 的**全代码库标注纪律**解决；Rust 里没有可移植的栈扫描，所以只能：
  (a) 引入**显式 rooting**（builtins 全面改用 `Rooted<Value>` 句柄），或
  (b) 继续用引用计数兜底（阶段 A 的做法）。

### 4.2 换 GC ≠ 消灭 `RefCell`

原型链本身就是**循环共享图**，Rust 里仍然需要共享可变（ChakraCore 直接用裸指针）。
所以真收益是**生命周期 / 分配 / 循环泄漏**，不是借用安全。

### 4.3 分两阶段

| | 做什么 | 收益 | 代价 |
|---|---|---|---|
| **阶段 A（先做）** | 保留 `Rc<RefCell>`，加**循环回收器**（定期 trace，试删 / Bacon–Rajan 找环并断开） | 止住泄漏；第一次能真正统计"堆里有多少对象" | 小：收集器独立，builtins 不用改，Rust 局部变量仍安全 |
| **阶段 B（决定深改才做）** | arena + `Gc<T>` 句柄 + mark-sweep，builtins 全面改用 `Rooted<>` | 分配变 bump-pointer；无环泄漏；为帧模型铺路 | **大**：376 处借点 + 全部 builtins 调用约定，本项目最大一次改动 |

---

## 5. 问题二：要不要直接移植 frame / 寄存器 / EH 模型

| 部分 | 判断 | 说明 |
|---|---|---|
| **帧模型** | **是** | 9 条平行栈 + 4 处内联开帧 + 嵌套循环，正是 `InterpreterStackFrame` 要消灭的东西 |
| **EH** | **是，且最该先做** | 把 try/catch/finally 从 `seh_stack` 挪到字节码元数据，**不碰生命周期**，风险最低，却能把一条平行栈直接删掉 |
| **寄存器模型** | **是，但排第三** | 它要求 `Value` 变成定长标记值（他们的 `Var`）→ 而 `Value` 改造依赖 GC → 所以顺序上在 GC 之后 |

---

## 6. 对现有 P0–P3 计划的裁决

| 阶段 | 与 ChakraCore 的关系 | 裁决 |
|---|---|---|
| **P0**（指令表 / enum / 命名字段 / `desc()` / 元测试） | 与 `OpCodes.h` 同构 | ✅ **已完成，保留** —— 任何新字节码（含寄存器编码）都靠它 |
| **P2**（`CodeBlock` / FunctionBody per function） | 对应 `FunctionBody` + `ByteCodeBlock`（含 EH 表、语句映射） | ✅ **该做，并且提前** —— EH 元数据与 per-function 字节码都在这一层，是后两步的先决，且无生命周期风险 |
| **P1**（调用归一 → `enter_call`） | 正在往 `OP_CallCommon` 收敛 | ⚠️ **分析保留、代码暂停** —— 种类判定 / 参数约定 / `this` 约定这三条结论要留（已写进 §3.3.4），剩下的实现在重写路线里是重复劳动 |
| **P3**（帧归一 + 堆帧 + 单层循环） | **就是** `InterpreterStackFrame` | ⚠️ **增量版作废，改走重写版** —— 见第 4 步 |

---

## 7. 新验证协议（因为"允许破坏性变更"）

允许破坏不等于放任。每一步改为**"快照 + 允许回归清单"**：

1. **开工前记录快照**：`docs/phase2-status.tsv` 的基线（通过 / 失败 / 跳过），
   外加一组**对照探针的输出**（同一份 JS 分别跑 biujs 与 node，逐行对比 —— 这轮
   就是用这个方法抓到生成器闭包 bug 的，而当时 test262 全绿）。
2. **每一步允许回归**，但必须：把回归项列进该步的提交信息，并标注"预期在哪一步消掉"。
3. **每一步的结束点**必须回到"行为快照等价"（探针逐行一致 + test262 不比开工前差），
   否则不许开始下一步。
4. **语义 bug 一律先钉测试再动结构**（见第 1 步）—— 否则重写会把旧行为连 bug 一起搬走，
   那时连"它原来是什么行为"都没有参照了。

---

## 8. 第 1 步（前置）：3 个语义 bug 钉成测试

| # | 现象 | node | 我们 | 状态 |
|---|---|---|---|---|
| 1 | **生成器闭包槽位错位**<br>`function mk(){var x=7;return function*(){yield x}}; mk()().next().value` | `7` | `[object Function]` | 已定位到"闭包槽位读错"，**未修**；且我先前在 `interpreter-refactor.md` §3.3.6 把根因写成"`CallMethod` 传 `Vec::new()`"——**那是读代码得出的错判**，实测普通生成器函数（不经过任何方法调用）同样坏 |
| 2 | **`New` 的 `this_stack` 快照顺序异常**（`this_val` 先被覆盖、`enter_frame` 后跑） | —— | —— | 已登记（`interpreter-refactor.md` §3.3.3），未查清是否有意 |
| 3 | **`obj.missing()` 返回 `undefined` 而不抛 TypeError** | TypeError | `undefined` | 改道 `CallMethod` 时发现，未查 |

**教训（写进流程）**：这轮唯一"抓到" #1 的是**和 node 的逐行对照探针**，而当时全量
test262 是绿的。差异核对（同一个语义、两条路径各写一遍）比事后补测试更早暴露问题——
**它应该成为常规动作，而不是偶然**。

---

## 9. 两条路的取舍（如何反悔）

| | Track A（继续增量） | **Track B（深改，本文方案）** |
|---|---|---|
| 节奏 | 每步可独立提交、持续拿 test262 分数 | 有回归期；分 4 步，每步结束点验收 |
| 作废风险 | P1/P3 的代码在将来的重写里会作废一部分 | 一次做完，不作废 |
| 上限 | 仍是 `Rc<RefCell>` + 平行栈，循环泄漏与借用崩溃两类问题**不解决** | 解决生命周期/分配，为帧与寄存器模型铺路 |
| 适合 | 优先"稳定地涨分" | 优先"架构到正确的地方" |

**当前选择：Track B。** 依据：38,981 LOC 正是重写窗口；这轮已连续挖出 3 个 test262
抓不到的语义 bug，说明现有数据模型本身在漏。

---

## 10. 第 2 步的实测发现（开工前必读）

### 10.1 已落地：per-function 代码范围（派生视图）

`Module` 里 `symtab`（入口 pc）与 `exit_pc`（尾 `Ret` 的 pc）**早就都算好了**，所以
"一个函数的字节码体"可以做成它们的**派生视图**，不需要新增状态、也就不可能与那两张表
不一致：`Module::body_of(func_id) -> Option<FunctionBody { start, end }>`，`end = exit_pc + 1`。

附带一条守卫测试 `function_bodies_are_well_formed`：编译一段含普通函数 / 生成器 / async /
闭包 / 类方法的程序，断言每个体 `start < end`、最后一条是 `Ret`、范围两两不重叠。
**这三条看着显而易见，却是把 EH 元数据挂进 `FunctionBody` 的前提** —— 在那之前必须先确认
"函数代码范围"这个概念本身可靠（已确认）。

### 10.2 哨兵返回地址不能提前改成 per-function（试过，一片红）

想顺手让 `drive_bytecode_frame` 用 `body.end` 当哨兵返回地址，结果
**array / async / promise / date 一大片 feature 测试失败**。原因：

- 嵌套 `step` 循环的终止条件是 `module.instructions.get(pc) == None`，即 **pc 越界**，
  **不是**"pc == 某个返回地址"；
- 所以哨兵必须是一个**不存在的 pc**（现为 `instructions.len()`）。换成 `body.end` ——
  那是个**合法的 pc**，属于下一段函数的代码 —— `Ret` 之后循环就接着跑下一段函数去了。

⇒ **per-function 哨兵必须配套改循环契约**：让循环在"pc == 本帧哨兵"时停止。
这是**第 4 步（帧模型重写）**的内容，不该在 P2 先做。已把这段结论写在代码注释里
（`vm/mod.rs` 的 `return_pc` 处），免得下一个人再试一次。

---

### 10.3 待查：有一条 `EndTry` 不在派生表的任何区域终点上

给 `EndTry` 加 `debug_assert!(eh_region_ending_at(pc).is_some())` 时，在
"嵌套 try，且 catch 里再 throw" 的程序上**直接响**：

```
EndTry(pc=73) 关掉的不是 EH 表里的任何区域
```

含义：**运行时的进出配对，与静态派生的区域，在某处不一致**。两种可能：

- 有 `EndTry` 没有配对的 `Try`（发射端多发了一条）；
- 我的区域 `end` 算法在"处理器块里再嵌 try"时算错了（深度计数把内外的 `EndTry` 配错，
  或那条 `EndTry` 落在我算出的 `body.end` 之外）。

**这一条必须先查清，才能把 `seh_stack` 退掉** —— 否则退掉就等于把异常路径悄悄改一遍。
断言已撤（不能留一条会响的断言），现象与位置写在 `vm/mod.rs` 的 `EndTry` arm 里。

## 11. 分支与提交约定

- 深改开**独立分支** `refactor/deep-arch`，与 `refactor/interpreter-p0a`（P0/P1 已落地部分）
  分开，便于随时回到可发布状态。
- 第 1 步（bug 钉测试 + 文档订正）**留在当前分支**做，因为它同时服务于 Track A。
- 每步一次提交，提交信息按 §7 列出"允许的回归项"。
