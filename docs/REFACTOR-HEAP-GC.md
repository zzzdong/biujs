# 堆与句柄重构执行计划

配套设计文档：`docs/GC-DESIGN.md`。本文件只写**执行步骤、每步验收、回退方案**。
原方案在 `.trae/documents/P1-heap-gc-handles.md`（保留溯源，**已按评审结论修订**）。

---

## 0. 起点：做到哪了

| 步骤 | 状态 |
|---|---|
| 步骤 0 — `src/vm/gc.rs` 单文件试点 | ✅ 完成，`--lib` = 204 + 新增 |
| 步骤 1 — 原子切换 `Rc<RefCell<dyn JSObject>>` → `Gc<Object>` | ✅ 完成 |
| 步骤 2 — 删 23 项断环清单 | ✅ 完成（A3） |
| 步骤 3 — 文档 | ✅ 完成（A4） |

### 执行进度（2026-10-12）

| 步骤 | 结果 |
|---|---|
| **A1** 补完验证 | 三级全绿。全量 **16562 / 3146 / memory 30**（基线 16567 / 3141 / 22）—— 见下方"−5 的定位" |
| **A2** `Object` 瘦身 | `suspended` 装箱，`Object` **624 B → ≤256 B**；全量数字不变（该 5 条不是对象尺寸驱动的） |
| **A3** 删断环清单 | `teardown` 23 项 + `VM::run` / `Drop` 两处补丁全删；**16567 / 3141 / memory 25，纯删除零漂移** |
| **A4** 文档 | `ARCHITECTURE.md` §3 新增 §3.1、§5 #1 改写；`PLAN.md` §P1 标记完成并撤销 P2-4；`PROJECT.md` 数字与 §5.7 |
| **B1** 槽改造 | `Slot { state, cell }` + 64 KiB 块 + `try_borrow`；`--lib` 214（+3）；**16567 / 3141 / memory 25，零回退** |
| **B2** 安全点 + `try_borrow` 兜底 | 代码部分随 B1 落地；"GC 只在 `step()` 指令边界触发"要等收集器存在才有意义 ⇒ **归入阶段 C** |
| **B3** 根集形状 | `Trace` 协议 + `VM`/`State`/`Builtins` 实现（新 `src/vm/trace.rs`）+ `Scope`/`Root` RAII；`--lib` 223（+9）；**零回退**。落地时撞出 E0499 ⇒ 根栈必须与 `Heap` 分离持有 |
| **B4** 去 `as_any` / `downcast` | 57 处调用点（含 6 处 `is::<T>()`）全部换成具名访问器；`--lib` 223；**零回退**；警告回到 71 |

**阶段 B 已完成**（2026-10-12）。每一步都跑了三级 + 全量，头条与护栏逐项不变：
16567 / 7843 / 3141，timeout 4 / step-limit 0 / memory 25。

### 阶段 C 进度（2026-10-12）

| 步骤 | 结果 |
|---|---|
| **C1** `Object::trace` | 20 个变体 + `PropertyTable` / `SuspendedFrame` / `NativeIteratorState` 全部实现；单测验证 `a.b=b; b.a=a` 的环能被走通 |
| **C2** mark + sweep | 显式 worklist **非递归**；`sweep` 用 `mem::replace` 留占位对象（**不能**用 `drop_in_place` —— 槽归 `Vec<Slot>` 所有，堆析构时会再 drop 一次，实测 `free(): double free` → SIGABRT）；free list 复用；`threshold = max(live*2, 16 MiB)` |
| **C3** 安全点 | 只在 `step()` 指令边界轮询；`alloc()` 内绝不触发 |
| **验收** | **GC stress 全绿**：`BIUJS_GC_STRESS=1` 下 feature 538、护栏 7、单元 228 全过 |

**关键的两处修正（都是实测撞出来的）**：

1. **`invoke_boundaries` 非空时跳过收集**。原生内建（`Array.prototype.map` 回调、
   Map/Set 构造器的迭代协议、`JSON.parse` 的 reviver）把 `Value` 握在 **Rust 局部变量**
   里，而收集器看不见机器栈。不加这道闸门时 GC stress 直接炸出 **6 条失败**
   （`Map constructor: 'set' is not callable`）。加上后全绿。**正解是宿主侧入根
   （`Scope`），闸门是让收集器不踩空的过渡措施。**
2. **阈值 4 MiB 太频繁**：头条不变但超时护栏 4 → 17。提到 16 MiB 后配合闸门回到 7。

**结果**：头条 16567 / 7843 / 3141 不变、无逐套件回退；**护栏 timeout 7 / step-limit 0 /
memory 22 —— 精确回到项目基线**（阶段 A 记录的 timeout 4 / memory 25 已被吸收）。

**仍未完成**：`TEST262_MEMORY_MB` 还不能降回 256（实测 256 MB 下 memory 27、丢 5 条，
`built-ins/Array` −5）。原因是重分配的 Array 用例主要在**原生驱动**的路径里产生垃圾，
而那里正是被闸门跳过的区域 ⇒ 需要宿主侧入根（把 `Scope` 接进原生路径），再做一次。

**−5 的定位（A1 的关键结论，改变了后续判断）**：把 `TEST262_MEMORY_MB` 从 256 提到 384，
头条精确回到 **16567 / 3141** 且**无逐套件回退**（`built-ins/Array` 反而 +3）。
即 P1 **没有任何语义回退**，那 5 条是**预算**问题——堆只在 VM 析构时整片释放，
单次 `run` 内不回收，单用例峰值上升。故脚本默认预算改为 384 并记入成因；
**正解是阶段 C 的 mark-sweep**，做完后降回 256。

**评审发现的两处必修**（详见 `GC-DESIGN.md` §4）：

| # | 问题 | 证据 |
|---|---|---|
| **A2** | `size_of::<Object>()` = **624 B**，每个对象都付最大变体的账 ⇒ 约 **5× 内存回归** | 主流变体 56–168 B，唯一异常 `GeneratorObject` = 624（内联 `SuspendedFrame`，含 `[Value; 19]` = 304 B） |
| **B1** | 槽是裸 `RefCell<Object>`：无标记位、无空闲标记、无 free list ⇒ **P2 的 sweep 无法回收单个对象** | 留到 P2 改 = 对核心类型的**第二次迁移**，违反纪律 #6 |

**B1 现在做很便宜**：`Heap::alloc` / `Gc::borrow` / `Gc::ptr_eq` / `bytes_allocated`
的**签名全部不变**，改动完全局限在 `gc.rs` 内部，213 处分配点与 235 处 borrow 站点
一个字都不用动。

---

## 1. 阶段 A — 收尾与止血

> 目标：让 P1 真正落地并**回到基线数字**。阶段 A 不引入任何新机制。

### A1 — 补完步骤 1 的验证（先做，因为后面每一步都以它为基线）

```sh
cargo test --release --lib            # 期望 210
cargo test --release --test features  # 期望 538（4 ignored）
cargo test --release --test guards    # 期望 7
./scripts/phase2-status.sh            # 期望 19708 / 16567 / 7843 / 3141，护栏 7/0/22，≈450s
```

- **若全绿** → 提交步骤 1（`cargo check` 已绿、改动已原子化），进入 A2。
- **若头条回退** → 说明切换有语义偏差，逐套件快照定位，**先修完再动别的**。
- **若只有 `memory` 护栏上升**（基线 22）→ 这大概率就是 624 B 那个问题，直接进 A2 复测；
  **不要**先去调 `TEST262_MEMORY_MB`（那是掩盖，不是修复）。

**纪律**：三个数同时看（头条 / 护栏三计数 / 墙钟）。只看头条会漏掉"通过数不变但护栏漂了"。

### A2 — `Object` 瘦身

```rust
// src/vm/object.rs — GeneratorObject
- pub suspended: Option<SuspendedFrame>,
+ pub suspended: Option<Box<SuspendedFrame>>,
```

- 同步修所有构造/读取点（编译期会列全）。
- 收紧 `gc.rs::object_size_stays_within_budget` 的断言：`Object <= 704` → **`<= 256`**。
- **纯结构变化，不改语义**（纪律 #6）。
- 验收：三级全绿；`Object` 实测 ≈200 B；**重跑全量看 `memory` 护栏是否回落**。

> 衍生的根治项（`SuspendedFrame::registers` 不该按帧复制全局寄存器堆）
> **不在本步做**——那是语义变化，归生成器那条线。

### A3 — 删断环补丁（独立提交，纯删除）

- 删 `Builtins::teardown` 的 23 项循环（`src/builtins/mod.rs:528`），**保留**
  `clear_wrapper_prototypes()`。
- 删 `VM::run` 里 `func_objs` 的 `prototype` 删除循环与 `globals.remove("globalThis")`。
- 删 `Drop for VM` 里的 `globals.remove("globalThis")`。
- 保留 `self.state = State::new()`（换 realm 语义）。
- 验收：**全量数字与护栏必须与 A2 完全一致**——纯删除，不允许任何漂移。
  任何一项漂移都说明还有隐藏的环没被堆接管，先查清楚再删。

### A4 — 文档

更新 `docs/ARCHITECTURE.md` §3（值/对象）与 §5 #1、`docs/PLAN.md` §P1，
记录 P1 实际形态与 `GC-DESIGN.md` 的后续步。数字刷新到实测值。

---

## 2. 阶段 B — 为收集器铺路

> 目标：让 P2 的 mark-sweep 能在**不改核心类型**的前提下实现。
> 每一步都是独立提交、每步跑全量。

### B1 — 槽改造（局限在 `gc.rs`，签名不变）

按 `GC-DESIGN.md` §4.2 引入 `Slot { state: Cell<u8>, cell: RefCell<Object> }`：

- `Gc<T>` 改指向 `NonNull<Slot>`，访问器只实现在 `Gc<Object>` 上；
  `borrow` / `borrow_mut` / `ptr_eq` **签名与语义不变**。
- 块容量改为**按字节预算 / 几何增长**，去掉 `with_capacity(1024)` 的 640 KB 一次性预留。
- `bytes: Cell<usize>` → `usize`（`alloc` 已是 `&mut self`，`Cell` 是多余的）。
- 预留 `free: Vec<NonNull<Slot>>` 与 `mark_queue`（P2 才启用）。
- 新增单测：`state` 位于偏移 0（`offset_of!` 断言）；标记位**不 borrow 即可读写**。
- 验收：三级 + 全量零回退。**这是纯结构变化，不得夹带语义改动。**

### B2 — 安全点与 `try_borrow` 兜底

- `alloc()` 内部**不触发** GC；VM 只在 `step()` 指令边界轮询 `should_collect()`。
- `Gc::try_borrow()` 就位（P2 的 trace 专用）。
- 文档化安全点清单（`GC-DESIGN.md` §7），并把"trace 失败即放弃本轮、绝不 panic"
  写成代码里的注释约束。
- 验收：三级 + 全量零回退（本步 P2 尚未接线，行为应完全不变）。

### B3 — 根集形状

- 给 `State` / `VM` 实现 `Trace`（字段清单见 `GC-DESIGN.md` §6.1）。
- 加 `Scope`（根栈）+ RAII 的 `Root<'s>`（LIFO，`Drop` 里 `debug_assert`）。
- **先只落地形状与单测**，暂不接进 `step()`——接线属于阶段 C。
- 验收：新增单测全绿；三级 + 全量零回退。

> **B3 已做（2026-10-12）**，并且落地时撞出一处设计修正：根栈**不能放进 `Heap`**。
> 分配要 `&mut Heap`，而存活的根守卫若持有 `&mut Heap` 就会把堆锁死 ——
> "持有根的同时再分配"直接编译不过（**实测 E0499**），而这正是原生内建回调进 JS
> 时的常态。改为与 `Heap` **分离持有**的 `Scope` 之后，`&Scope` 与 `&mut Heap`
> 是两个不相交的借用，可以并存（`Scope::alloc(&self, heap, obj)` 同时拿两者）。
> 设计文档 §3 / §4.2 / §6.2 已同步。

### B4 — 去 `as_any` / `downcast`（机械替换，独立提交）

95 处 `obj.borrow().as_any().downcast_ref::<T>()` → `if let Object::T(x) = &*obj.borrow()`。
纯机械，与 B1 分开提交（纪律 #6）。

---

## 3. 阶段 C — P2 收集器

1. `mark`（显式 worklist、**非递归**） + `sweep`（**先全部 drop、再全部释放**）+ free list。
2. 阈值 `threshold = max(live_bytes * 2, MIN)`。
3. 接进 `step()` 安全点。
4. **`BIUJS_GC_STRESS=1`**（每次分配都收集 + 释放后填 `0xDD`）跑 feature 全套 —— **这是真正的验收**。
5. WeakMap / WeakSet 从"条目永不回收"改成 ephemeron 真弱引用。
6. Miri 跑 `GC-DESIGN.md` §10.1 的 U1–U4。
7. ⇒ 到此循环引用可自动回收，阶段 A3 若还有残留环补丁可一并确认清零。

**撤销项**：`PLAN.md` §P2 第 4 条（护栏预算改读 `Heap::bytes_allocated`）**不做**——
该口径不含对象内 `Vec`/`HashMap` 的堆外内存，比现有全局分配器代理更差
（`GC-DESIGN.md` §11）。

---

## 4. 验证协议（每步都执行）

```sh
cargo test --release --lib            # 单元
cargo test --release --test features  # feature 断言
cargo test --release --test guards    # 护栏
./scripts/phase2-status.sh            # 全量 + 逐套件对比基线（≈450s）
./scripts/phase2-status.sh --update   # 完成一批后刷新基线
```

必须看到：**无逐套件回退** 且 **护栏不增**（基线 timeout 7 / step-limit 0 / memory 22）。

基线头条：**19708 / 16567 / 7843 / 3141（84.06%）**。

---

## 5. 风险与回退

| # | 风险 | 对策 | 回退 |
|---|---|---|---|
| R1 | **内存护栏上升**（624 B 的直接后果，基线 memory 22） | A2 先修尺寸，再复测 | 若 A2 后仍涨：先确认是"arena 不回收临时垃圾"（R3）而非回退；**不要**先调 `TEST262_MEMORY_MB` 掩盖 |
| R2 | 步骤 1 有语义偏差导致头条回退 | A1 逐套件快照定位 | `git stash` 回到切换前，分批查 |
| R3 | arena 在单次 run 内不回收，长跑用例内存单调增长 | 阶段 C 的收集器是正解 | 临时：每次 `run` 换新堆；或提前做最小 mark-sweep |
| R4 | `WRAPPER_PROTOTYPES`（TLS 缓存 `Gc<Object>`）跨 VM 钉住已释放槽 | `Drop for VM` 体里先 `clear_wrapper_prototypes()`（drop 体先于字段析构，必然早于堆释放） | 硬化：给堆加 `epoch`，缓存按 epoch 失效 |
| R5 | B1 改 `Gc` 内部表示引入 UB | 单测：偏移断言、跨块地址稳定、整堆释放 | 回退到 `NonNull<RefCell<Object>>`（当前形态） |
| R6 | trace 撞上 `borrow_mut` ⇒ panic ⇒ **进程 abort** | trace 只用 `try_borrow`，失败放弃本轮（INV-5） | — |
| R7 | 一次性切换过大 | 已按"步骤 0 试点 → 原子切换"分解；`cargo check` 驱动列全位点 | — |

---

## 6. 纪律（从事故里长出来的，不是风格偏好）

1. **崩溃优先于失败**：一条用例把进程带崩会带走整个分片的结果。
   `release` 是 `panic = "abort"`，引擎里能返回 `Err` 就不要 panic。
2. **每步路径上的东西只读一次**：历史教训——`step()` 里每步读环境变量，
   29% 墙钟回归、护栏 7/0/22 → 11/0/18，而通过数完全没变。
3. **验证必须同时看三个数**：头条、护栏三计数、墙钟。
4. **改核心类型不要分批**（编译器会列全位点，半迁移成本更高）。
5. **结构变化与语义变化不要放同一个提交**——A2 / B1 / B4 都是纯结构。
6. 实现 + 解锁 + 入册同批做（若某步牵动 test262 入册）。
