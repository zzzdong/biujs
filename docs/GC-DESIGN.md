# 堆与 GC 设计

照**当前实现 + 下一步目标形态**写（2026-10-12）。取代 `.trae/documents/gc-design.md`
那份通用草案中与本引擎不符的部分；执行步骤在 `docs/REFACTOR-HEAP-GC.md`。

---

## 1. 起点：现在长什么样

P1 的 `Value::Object` 已从 `Rc<RefCell<dyn JSObject>>` 换成 `Gc<Object>`，实测：

| 量 | 实测值 | 说明 |
|---|---|---|
| `size_of::<Value>()` | **16 B**（原 24 B） | ✅ P1 核心收益已兑现 |
| `size_of::<Gc<Object>>()` | 8 B | ✅ 瘦句柄 |
| **`size_of::<Object>()`** | **624 B** | ❌ 见 §4.1 |
| 分配点 | 213 处 `heap.alloc` | |
| `&mut Heap` 形参 | 117 处 | 显式传堆，无 TLS |
| `borrow()` / `borrow_mut()` 站点 | 235 处 | 文本不变（`Gc` 提供同名方法） |
| `Gc::ptr_eq` | 14 处 | 取代 `Rc::ptr_eq` |
| `as_any` / `downcast_ref` | 95 处 | 待去（见 §9） |

- `dyn` 已消失：`Object` 是**具体枚举**（20 个变体，对应原 `JSObject` 的 20 个实现体）。
- **`RefCell` 没有消失**，只是从各对象类型内部**上移到堆槽**：`chunks: Vec<Vec<RefCell<Object>>>`。
- **P1 不回收**。堆归 `VM` 持有，VM 析构时整片释放。环的回收留给 P2。

---

## 2. 目标与非目标

### 目标

- **G1** 自动回收循环引用，彻底删除 `Builtins::teardown` 的 23 项手工断环清单
  （该清单漏过一次 ⇒ DataView 没进 ⇒ 每条用例漏一整个 realm ⇒ 撞穿 4 GB `ulimit` ⇒ SIGABRT）。
- **G2** 宿主 Rust 侧与 VM 侧共用同一套引用类型，不引入 `Rooted` / `Unrooted` 双指针体系。
- **G3** unsafe 面**可枚举、可审计**，每处有安全论证或不变量。
- **G4** 根集**精确枚举**，不扫机器栈。
- **G5** 收集器上线后，崩溃面不扩大：`release` 是 `panic = "abort"`，
  引擎里能返回 `Err` 就不要 panic，**GC 路径尤其不能 panic**。

### 非目标

- **NG1** 不做保守栈扫描（LLVM 可把指针存寄存器/复用栈槽，保守扫描下指针会"凭空消失"）。
- **NG2** 不做移动 / 复制 / 压缩 / 分代。
- **NG3** 不做增量、并发、并行。
- **NG4** 不支持多线程 VM（`Gc: !Send` 编译期保证）。
- **NG5** 不做 NaN-boxing（纯性能项，且依赖"先有堆与句柄"——已判定后置，见 `PLAN.md` §5）。

---

## 3. 总体架构

```
┌──────────────────────────────────────────────────────────┐
│  VM（= 本引擎的 runtime，不另设 Runtime/Context）          │
│    heap: Heap          ← 堆是 VM 的字段，随 VM 整片释放    │
│    state: State        ← data_stack / registers / frames  │
│    builtins: Builtins                                     │
│    iterator_registry / generator_registry / …             │
│         │ 实现 Trace，收集时吐出全部根（§6.1）              │
└─────────┼────────────────────────────────────────────────┘
          │ roots
┌─────────▼────────────────────────────────────────────────┐
│  Heap                                                     │
│    chunks: Vec<Vec<Slot>>   ← 分块、不移动、永不 realloc    │
│    bytes                                                  │
│    （P2 再加 free list / mark_queue / threshold）           │
└───────────────────────────────────────────────────────────┘

┌───────────────────────────────────────────────────────────┐
│  Scope（宿主侧根栈，**与 Heap 分离持有**，见 §6.2）          │
│    roots: RefCell<Vec<Gc<Object>>>   ← LIFO，RAII 注销      │
└───────────────────────────────────────────────────────────┘
```

**根集 = VM/State 的全部值容器 ∪ 宿主侧 `Scope`。**

本引擎的运行时状态**全部是自己管理的数据结构**（值栈、寄存器数组、帧、闭包变量栈、
SEH 栈、全局表），所以根集**可 100% 精确枚举**，无需触碰机器栈。这是"给脚本引擎做
GC"比"给任意 Rust 程序做 GC"容易一个数量级的根本原因，必须吃下这个便宜。

---

## 4. 对象布局

### 4.1 先修一个问题：枚举尺寸 = 最大变体

`Object` 目前 **624 B**，而各变体实测是：

| 变体 | 尺寸 | 变体 | 尺寸 |
|---|---|---|---|
| `OrdinaryObject` | 80 | `RegExpObject` | 168 |
| `ArrayObject` | 112 | `PromiseObject` | 136 |
| `FunctionObject` | 144 | `DataViewObject` | 104 |
| `TypedArrayObject` | 112 | `ProxyObject` | 120 |
| `ProtoObject` | 56 | **`GeneratorObject`** | **624** |

**唯一异常值是 `GeneratorObject`**：它的 `suspended: Option<SuspendedFrame>` 内联了
`SuspendedFrame`，而后者含 `registers: [Value; 19]`（304 B）+ 4 个 `Vec`（96 B）。

后果是**每个对象**（哪怕只是 `{}`）都为生成器付账：

| | 改前（每对象） | 现在 |
|---|---|---|
| 普通对象 | 16（Rc 计数）+ 8（borrow flag）+ ~100 ≈ **124 B** | **640 B** |

≈ **5× 内存回归**。这正顶在项目第一号纪律上（内存护栏 256 MB/用例、分片 4 GB `ulimit`、
`release` 下 `panic = "abort"`、进程被杀带走整片结果）。

**修法（一行，语义不变）**：`suspended: Option<Box<SuspendedFrame>>`。
注释本身就写着"`Some` only while suspended"——它本就是稀有状态，装箱是语义正确的表达。
改完 `GeneratorObject` ≈ 192 B，最大变体回到 `RegExpObject`（168），`Object` 落到 ~200 B。

> 根治项另案（**不与 P1 同批**，属语义变化）：`SuspendedFrame::registers` 的注释自己说
> "寄存器堆是全局的，不是每帧的"——把全局寄存器堆拷进每个挂起帧，既撑大对象又让每次
> `yield` 复制 304 B。归到生成器那条线单独处理。

### 4.2 槽：header 与对象分离

**这是 P2 能成立的前提**。当前槽是裸 `RefCell<Object>`：没有标记位、没有空闲标记、
没有 free list ⇒ **sweep 无法回收其中某一个**（既不能从 `Vec` 中段摘除，标记位也无处放）。
若留到 P2 再改，就是对核心类型的**第二次迁移**，违反纪律 #6「改核心类型不要分批」。

```rust
#[repr(C)]
pub struct Slot {
    /// 偏移 0：Free / Unmarked / Marked。
    /// **不需要 borrow 即可读写** —— 这是 GC 能避开"对象正被 borrow_mut"的关键。
    pub state: Cell<u8>,
    pub(crate) cell: RefCell<Object>,
}

pub struct Heap {
    chunks: Vec<Vec<Slot>>,        // 沿用已验证的"块只 push、永不 realloc"不变量
    bytes: usize,                  // 已去掉 Cell —— alloc 已是 &mut self
    // —— P2 再加：free: Vec<NonNull<Slot>>、mark_queue: Vec<Gc<Object>>、threshold: usize
}
```

> 宿主侧根栈**不在这里**，见 §6.2。

块容量改为**按字节预算 / 几何增长**，不再 `with_capacity(1024)` 一次性预留 640 KB。

### 4.3 句柄

```rust
pub struct Gc<T> { ptr: NonNull<Slot>, _t: PhantomData<*const T> }
impl<T> Copy for Gc<T> {}
impl Gc<Object> {
    pub fn borrow(&self)     -> Ref<'_, Object>;    // (*ptr).cell.borrow()
    pub fn borrow_mut(&self) -> RefMut<'_, Object>;
    pub fn try_borrow(&self) -> Result<Ref<'_, Object>, BorrowError>;  // trace 专用
    pub fn ptr_eq(a: &Self, b: &Self) -> bool;
}
```

- **不实现 `Deref`**：`Deref` 让 `&T` 随手可得且无生命周期约束，可跨越 GC 点形成 UAF。
  统一走 `borrow()` 拿到守卫，至少在文本上是有标记的。
- **不实现 `Send`**（`PhantomData<*const T>`）：编译期保证单线程（NG4）。
- **非移动**（NG2）⇒ 地址终身稳定 ⇒ 句柄可以是裸指针，无需索引/句柄表。
- 保留 `T` 泛型参数只为将来，访问器只实现在 `Gc<Object>` 上。

### 4.4 契约（R1 的正式表述）

`Gc` 无 `Drop`、无引用计数。**任何 `Gc`（及其所在的 `Value`）不得活过它所属的 `Heap`/`VM`**，
否则读悬垂指针是静默 UB（`release` 下 `panic = "abort"`，连 panic 都看不到）。

已知触发点：`src/lib.rs::eval` 的返回值要活过 VM，故在 `eval` 内 `std::mem::forget(vm)`。
API 健全性归为**已登记契约**，而非类型保证——这是与 `Rc` 时代不同的、可接受的 P1 权衡。

---

## 5. 分配路径

```rust
pub fn alloc(&mut self, obj: Object) -> Gc<Object>   // 签名不变 ⇒ 213 处分配点不动
```

- `&mut self` 让"分配"与"借用已有对象"在类型层面互斥（`borrow` 走 `Gc`、不碰堆），
  因而**不需要 `thread_local` 堆，也不需要 fallback 堆**。
- 分配可达性一律**显式传 `&mut Heap`**：有 `&mut VM` 的位点直接 `self.heap.alloc(obj)`；
  自由函数与 native 闭包加 `heap: &mut Heap` 形参（现有 117 处）。
- 同一函数内要同时访问 `self.heap` 与 `self.state` 时用**拆分借用**
  （`let Self { heap, state, .. } = self;`），不要给整个 `self` 加锁。
- 释放（P2 sweep）：`drop_in_place(cell)` → 置 `state = Free` → 入 free list。
  > 待确认：`RefCell::as_ptr` 的稳定状态；若不可用，改为自管 `UnsafeCell<Object>` +
  > 槽头借用标志（把 `RefCell` 的 flag 提到 `Slot` 上，`borrow` 语义对调用方不变）。

---

## 6. 根集

### 6.1 VM 侧根（自动，精确）

给 `State` / `VM` 实现 `Trace`，收集时自动吐出全部根：

| 来源 | 字段 |
|---|---|
| 值栈 | `State::data_stack: Vec<Value>` |
| 寄存器堆 | `State::registers: [Value; 19]` |
| 调用帧 | `State::frames: Vec<Frame>`（`this`、`func`、`new_target`…） |
| 闭包变量 | `State::closure_var_stack: Vec<HashMap<String, Value>>` |
| 异常处理 | `State::seh_stack: Vec<SehRecord>` |
| 全局 | `State::globals`（`Rc<RefCell<HashMap<String, Value>>>`）、`global_object`、`script_env` |
| Promise | `promise_jobs`、`promises: HashMap<u64, Gc<Object>>`、`aggregations`、`finally_handlers` |
| VM 直属 | `iterator_registry`、`generator_registry`、`generator_send`、`generator_yielded` |
| 内建 | `Builtins` 的 23 个 prototype 字段 |

### 6.2 宿主侧根（显式）：`Scope` + RAII —— **必须与 `Heap` 分离持有**

Rust 局部变量里的 `Value` 对收集器**不可见**。危险路径是**原生内建回调进 JS** 的地方
（`try_array_callback_method`、`enter_call`、Promise job drain）——它们持有 `Value` 局部量，
同时又驱动字节码、到达安全点。

```rust
pub struct Scope { roots: RefCell<Vec<Gc<Object>>> }   // 与 Heap 分离持有的值
impl Scope {
    pub fn root(&self, g: Gc<Object>) -> Root<'_>;                       // 只拿 &self
    pub fn root_value(&self, v: &Value) -> Root<'_>;
    pub fn alloc(&self, heap: &mut Heap, obj: Object) -> (Gc<Object>, Root<'_>);
}
pub struct Root<'s> { scope: &'s Scope, ptr: Option<Gc<Object>> }        // RAII，Drop 注销
```

**根栈为什么不能放进 `Heap`**（这是实现时撞出来的，不是先验设计）：分配要 `&mut Heap`，
而一个存活的根守卫若持有 `&mut Heap`，就会把堆锁死 —— "持有根的同时再分配"直接编译不过
（实测 **E0499**）。而这恰恰是上面那些回调路径的常态。把根栈做成与 `Heap` **分离持有**
的值之后，"登记根（`&Scope`）"与"分配（`&mut Heap`）"是两个**不相交**的借用，可以并存。
`Scope::alloc(&self, heap: &mut Heap, …)` 同时拿两者，正是这个设计的直接体现。

- **不引入 `Rooted<'s, T>` 第二套值类型**（G2）：根的类型仍是 `Gc<Object>` / `Value`，
  只是多了一层"已登记"的 RAII 担保。
- 分配 + 入根一步到位：`let (g, _r) = scope.alloc(&mut heap, obj)`。
  仅在"立即塞进 VM 结构、中间不可能触发 GC"时才用裸 `heap.alloc`。

---

## 7. 安全点

**`alloc()` 内部绝不触发 GC**——触发点必须是 VM 安全点，否则调用方栈上尚未入根的
临时 `Gc` 会被误回收。

VM 在 `step()` 的**指令边界**轮询 `heap.should_collect()`。**禁止**在以下位置触发：

- `alloc()` 内部；
- 任何持有 `Ref` / `RefMut` 守卫的 unsafe 块内；
- finalize 回调内。

### 7.1 trace 绝不能 panic

`release` 是 `panic = "abort"`，一次 trace 撞上正在 `borrow_mut()` 的对象就是整片进程没了。

⇒ **trace 一律用 `try_borrow()`；失败则放弃本轮收集并提高 `threshold`，绝不 panic。**

这是"句柄不经过堆"的必然代价：我们放弃了 gc-design.md D5（对象借用自 `&Heap`、
`collect` 取 `&mut Heap`，靠借用检查器在编译期杜绝活跃引用），因为那要求 235 处 borrow
都拿到 `&Heap`、可变访问还要 `&mut Heap`，与"同时持有两个对象引用"全面冲突，churn 远
大于现在的 117 个 `&mut Heap` 形参。改用运行时兜底。

---

## 8. 标记-清除（P2）

### 8.1 标记：显式 worklist，**不递归**

深链表 / 长闭包链用递归会爆栈。

```
mark(roots):
    queue ← roots
    while let Some(g) = queue.pop():
        if slot(g).state == Marked: continue
        slot(g).state ← Marked              # 读标记位不需要 borrow（§4.2）
        match g.try_borrow():
            Ok(o)  => o.trace(&mut queue)    # 由 Object 各变体的 match 决定子引用
            Err(_) => { abandon_this_cycle(); return }   # 见 §7.1
```

### 8.2 清除：先全部 drop，再全部释放

```
sweep():
    for slot in occupied:
        if slot.state == Unmarked:
            doomed.push(slot)
    # 第一遍：跑 drop glue。此时**所有对象内存仍然有效**，
    #   故 Drop 里读到别的 Gc 字段是安全的（避免"析构访问已释放对象"）。
    for slot in doomed { drop_in_place(slot.cell) }
    # 第二遍：置 Free、入 free list、扣 bytes
    for slot in doomed { slot.state ← Free; free.push(slot) }
    # 复位：Marked → Unmarked（供下一轮）
```

**两遍顺序是刻意的安全设计**：单遍交替 drop/free 会让先释放的对象被后释放对象的
`Drop` 读到。

### 8.3 阈值

```rust
self.threshold = (live_bytes * 2).max(MIN_THRESHOLD);
```

按存活量的倍数增长，避免"存活率高时频繁触发"的经典病态行为。

### 8.4 复杂度

| 项 | 值 |
|---|---|
| 时间 | O(存活对象数 + 根集大小)，与堆总量无关 |
| 空间 | O(worklist 深度) |
| 停顿 | 全停顿（stop-the-world） |

---

## 9. 去 `as_any` / `downcast`（顺带）

`Object` 已是具体枚举，95 处 `obj.borrow().as_any().downcast_ref::<T>()` 应改成
`if let Object::T(x) = &*obj.borrow()`。这是**纯机械替换**，不夹带语义变化——
但要与槽改造（§4.2）**分开提交**（纪律 #6）。

---

## 10. 不变量清单

| # | 不变量 | 违反后果 | 验证手段 |
|---|---|---|---|
| INV-1 | `Slot` 头 `state` 位于偏移 0 | 读错内存 | `#[repr(C)]` + `offset_of!` 编译期断言 |
| INV-2 | 块只 push、永不 realloc | 句柄悬垂 | 已有单测：跨块后旧句柄仍读到正确对象 |
| INV-3 | 所有含 `Gc` 字段的类型正确实现 `trace` | 静默 UAF | 每类型一个"构造不可达对象 → 验证被回收"单测 |
| INV-4 | GC 只在 VM 安全点触发 | 临时 `Gc` 被误回收 | `alloc` 不自动触发；`should_collect` 由 VM 轮询 |
| INV-5 | trace 不 panic | 进程 abort | trace 只用 `try_borrow`，失败即放弃本轮 |
| INV-6 | `Scope` 根栈严格 LIFO | 根集错乱 | `Drop for Root` 里 `debug_assert`（Rust 局部变量逆序 drop 天然满足） |
| INV-7 | 对象地址终身不变 | 悬垂指针 | 非移动（NG2），架构保证 |
| INV-8 | `Heap` 与 VM 单线程独占 | 数据竞争 | `Gc: !Send`，编译期 |
| INV-9 | `Object` 尺寸由**最大变体**决定，大变体必须装箱 | 内存 5× 回归 | 单测断言上限（见 `gc.rs::object_size_stays_within_budget`） |
| INV-10 | sweep 先全部 drop、再全部释放 | 析构读到已释放对象 | §8.2 两遍顺序 + GC stress |

### 10.1 unsafe 清单（完整）

| # | 位置 | 操作 | 安全性来源 |
|---|---|---|---|
| U1 | `alloc` | 取块内槽地址 | INV-2（块不 realloc） |
| U2 | `Gc::borrow` / `try_borrow` | 裸指针解引用 | INV-7（地址稳定）+ §4.4 契约（堆存活） |
| U3 | `sweep` | `ptr::drop_in_place` | 对象确已标记不可达，且不在根集 |
| U4 | `sweep` | 槽置 `Free` / 入 free list | §8.2 两遍顺序（先 drop 后释放） |

**共 4 处**，全部集中在 §5 与 §8.2，审计面可控。

---

## 11. 内存口径（一个必须纠正的结论）

`Heap::bytes_allocated` 现在是 `size_of::<Object>()` 的**只增累加**，且它**不含**对象内
`Vec` / `HashMap` 持有的堆外内存——而数组元素和属性表才是大头。

⇒ **撤销 `PLAN.md` §P2 第 4 条**（"护栏的堆预算改读 `Heap::bytes_allocated`"）。
现有的**全局分配器代理**反而更准。真要换，得让 `Object` 额外上报 `heap_size()`
（外挂 `Vec` / `HashMap` 容量 × 元素尺寸）。

---

## 12. 验证

| 层级 | 手段 | 覆盖 |
|---|---|---|
| 单元 | 尺寸守门（`Object` 上限、`Value` = 16） | INV-9 |
| 单元 | 循环引用（`obj.self = obj`、长原型链、闭包环）显式收集后被回收 | G1 |
| 单元 | `Root` 保护验证（入根存活 / 未入根被回收） | INV-6 |
| 单元 | 10 万节点深链收集不爆栈 | §8.1 |
| **GC stress** | `BIUJS_GC_STRESS=1`（每次分配都收集 + 释放后填 `0xDD`）跑 feature 全套 | **这是 P2 真正的验收** |
| 内存 | Miri 跑 §10.1 的 U1–U4 | unsafe 路径 |
| 断言 | `debug_assert` 校验 free list LIFO、标记位复位、对象数守恒 | INV-6 |

**每步都必须同时看三个数**：头条（passed/skipped/failed）、护栏三计数（timeout 7 /
step-limit 0 / memory 22）、墙钟（≈450 s）。只看头条会漏掉"通过数不变但护栏漂了"
——历史上真实发生过（`step()` 里每步读环境变量：29% 墙钟回归、护栏 7/0/22 → 11/0/18，
通过数完全没变）。

---

## 13. 演进路线

| 阶段 | 内容 |
|---|---|
| **P1（进行中）** | 堆 + 句柄 + 整堆释放；删 23 项断环清单 |
| **P1.5** | §4.1 对象瘦身 + §4.2 槽改造（**P2 前必须做完**，否则二次改核心类型） |
| **P2** | mark-sweep + free list + GC stress 全绿；WeakMap/WeakSet 转真弱引用（ephemeron） |
| **P2.5** | finalize 延迟队列（若确需） |
| **后置** | arena 分配器优化；NaN-boxing；移动式 GC（**不可逆，需单独评审**，届时 `Gc` 要改成句柄索引） |

## 14. 明确不做

- **写屏障**：单线程 stop-the-world，不需要。
- **分代 / 增量 / 并发**：需 safepoint 协议与手写屏障，Rust 中极易漏。
- **`thread_local` 全局堆 / fallback 堆**：`panic = "abort"` 下 `install` 与 `clear`
  之间任何 panic 都会把裸指针留在 TLS ⇒ 悬垂；fallback 堆把"忘了传堆"变成静默泄漏。
- **引入独立 `Runtime` / `Context` 类型**：本引擎里 VM 即 runtime，调用面
  （`lib.rs` / `test262_runner` / `guards` / `main.rs`）保持 `VM::new()` + `vm.run()` 不变。
