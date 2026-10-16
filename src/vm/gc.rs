//! P1 试点：自建堆 + `Gc<T>` Copy 句柄。
//!
//! 本模块是 `docs/PLAN.md` §P1 的**单文件试点**（步骤 0）：先把四处形状钉死 ——
//! `Gc` 的 Copy 语义、分块不移动 arena 的地址稳定性、`Object` 枚举的转发、
//! 以及"整堆释放" —— 再在步骤 1 里把 `Value::Object` 从
//! `Gc<Object>` 整体切到这里。
//!
//! **堆的归属**：`Heap` 由 `VM` 持有（本引擎里 VM 即 runtime），随 VM 析构整片释放。
//! 不引入线程局部的全局堆，也不设 fallback 堆 —— 分配一律显式接收 `&mut Heap`；
//! 只有**分配**需要接触堆，`Gc::borrow` 直接解引用、不碰堆。
//!
//! **契约**：`Gc` 没有 `Drop`、没有引用计数。任何 `Gc`（及其所在的 `Value`）
//! **不得活过**它所属的 `Heap`/`VM`，否则就是悬垂（release 下 `panic = "abort"`，
//! 读悬垂指针是静默 UB）。

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::builtins::ProtoObject;
use crate::vm::iterator::NativeIteratorObject;
use crate::vm::object::{
    ArrayBufferObject, ArrayObject, DataViewObject, DateObject, FunctionObject, GeneratorObject,
    GlobalObject, JSObject, MapObject, NativeFunctionObject, OrdinaryObject,
    PrimitiveWrapperObject, PromiseObject, ProxyObject, RegExpObject, SetObject, TypedArrayObject,
    WeakMapObject, WeakSetObject,
};
use crate::vm::property::{ObjectKind, PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// 每块的目标字节数。块内只 push、到满即换新块，**永不 realloc**，
/// 因此已发出的 `Gc` 槽地址在堆存活期内恒定。
///
/// 按**字节**而不是按槽数定：槽的大小随 `Object` 变（枚举尺寸 = 最大变体），
/// 写死槽数会让"对象变大一点"直接放大成"每块预留变大很多"。
const CHUNK_BYTES: usize = 64 * 1024;

/// 槽的状态。放在 `RefCell` **外面**，所以 GC 读/写标记位**不需要借用对象** ——
/// 这是 mark 阶段能避开"对象正被 `borrow_mut`"的关键（`docs/GC-DESIGN.md` §4.2）。
// P2：`Free` / `Marked` 由 sweep / mark 使用，现阶段只有单测构造它们。
#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotState {
    /// 已释放的槽（P2 的 sweep 把它还给 free list）。
    Free = 0,
    /// 已分配，本轮尚未标记。
    Unmarked = 1,
    /// 本轮已标记（可达）。
    Marked = 2,
}

/// 堆槽：状态位与对象分离，且 `state` 位于**偏移 0**（INV-1）。
///
/// `#[repr(C)]` + `state` 在前，使 `offset_of!(Slot, state) == 0` 成为可断言的
/// 不变量：从 `Gc` 的裸指针取状态位不需要任何偏移换算。
#[repr(C)]
pub struct Slot {
    pub state: Cell<SlotState>,
    pub(crate) cell: RefCell<Object>,
}

/// Copy 句柄：指向堆里的一个 [`Slot`]。
///
/// 只存裸指针，`borrow` 直接解引用，**不需要 `&Heap`** —— 这正是"只有分配才需要
/// 接触堆"、从而无需线程局部堆的原因。
pub struct Gc<T> {
    ptr: NonNull<Slot>,
    /// 承载类型参数，同时使 `Gc` 为 `!Send`（单线程是收集器的前提，INV-8）。
    _t: PhantomData<*const T>,
}

impl<T> Copy for Gc<T> {}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Gc<T> {
    /// 身份比较：两个句柄是否指向同一槽。（取代 `Rc::ptr_eq`。）
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.ptr.as_ptr() == b.ptr.as_ptr()
    }

    /// 读槽的状态位。**不触碰 `RefCell`**，故 GC 可在任何时刻安全调用。
    // P2：mark 阶段接线后即被调用（现阶段只有单测用）。
    #[allow(dead_code)]
    pub(crate) fn state(&self) -> SlotState {
        // SAFETY: 句柄由 `Heap::alloc` 发出，槽地址在堆存活期内恒定。
        unsafe { &*self.ptr.as_ptr() }.state.get()
    }

    /// 写槽的状态位。同 [`Gc::state`]，不需要借用对象。
    #[allow(dead_code)]
    pub(crate) fn set_state(&self, new_state: SlotState) {
        unsafe { &*self.ptr.as_ptr() }.state.set(new_state);
    }
}

impl Gc<Object> {
    /// 借用槽内对象，返回守卫。契约：调用方须保证堆仍存活。
    pub fn borrow(&self) -> Ref<'_, Object> {
        // SAFETY: 句柄由 `Heap::alloc` 发出，槽地址在堆存活期内恒定。
        unsafe { &(*self.ptr.as_ptr()).cell }.borrow()
    }

    /// 可变借用槽内对象，返回守卫。契约同 [`Gc::borrow`]。
    pub fn borrow_mut(&self) -> RefMut<'_, Object> {
        unsafe { &(*self.ptr.as_ptr()).cell }.borrow_mut()
    }

    /// `borrow` 的不 panic 版本。**GC 的 trace 一律用它**：撞上正在进行的可变借用时
    /// 放弃本轮收集（INV-5），而不是 panic —— release 下 `panic = "abort"`，
    /// 一次 panic 会带走整个进程及其所在分片的全部结果。
    pub fn try_borrow(&self) -> Result<Ref<'_, Object>, std::cell::BorrowError> {
        unsafe { &(*self.ptr.as_ptr()).cell }.try_borrow()
    }
}

impl<T> std::fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gc({:p})", self.ptr.as_ptr())
    }
}

/// 分块、不移动的 slot arena。
///
/// 每块容量固定（按 [`CHUNK_BYTES`] 折算成槽数）；分配只往仍有空余容量的块里 push，
/// 容量用尽就开新块。因为 push 前已确认 `len < capacity`，块内**永不 realloc**，
/// 旧槽地址不动。
pub struct Heap {
    chunks: Vec<Vec<Slot>>,
    /// 已回收、可复用的槽（free list）。
    free: Vec<NonNull<Slot>>,
    /// mark 阶段的 worklist，**复用**（不每轮新建）。
    mark_queue: Worklist,
    /// 存活槽位的字节数（alloc 加、sweep 减）。
    bytes: usize,
    /// 达到此值即触发收集。按存活量的倍数增长。
    threshold: usize,
    /// `BIUJS_GC_STRESS=1`：每次分配都收集（用于把 rooting bug 炸出来）。
    /// **只读一次**环境变量（纪律 #2：路径上的东西不要每步读）。
    stress: bool,
}

/// 收集阈值的下限。
///
/// 4 MiB（≈ 2 万个槽）实测**太频繁**：头条不变但超时护栏从 4 涨到 17 —— 每次收集
/// 都要扫一遍全堆 + 遍历整个存活图，而多数脚本的存活量远小于此。16 MiB 是折中：
/// 仍然把内存护栏压下来，但不至于把墙钟烧在重复扫描上。
const MIN_THRESHOLD: usize = 16 * 1024 * 1024;

impl Heap {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            free: Vec::new(),
            mark_queue: Vec::new(),
            bytes: 0,
            threshold: MIN_THRESHOLD,
            stress: std::env::var_os("BIUJS_GC_STRESS").is_some(),
        }
    }

    /// 每块的槽位数 = [`CHUNK_BYTES`] / `size_of::<Slot>()`，至少 1。
    fn slots_per_chunk() -> usize {
        (CHUNK_BYTES / std::mem::size_of::<Slot>()).max(1)
    }

    /// 唯一分配入口。`&mut self` 让"分配"与"借用已有对象"在类型层面互斥
    /// （borrow 走 `Gc`、不碰堆），因而不需要内部可变性。
    pub fn alloc(&mut self, obj: Object) -> Gc<Object> {
        // 优先复用已回收的槽（否则内存不下降，等于没收集）。
        // 槽里留着 sweep 放进去的占位对象，赋值会把它正常 drop 掉。
        if let Some(ptr) = self.free.pop() {
            let slot = unsafe { &mut *ptr.as_ptr() };
            if let Ok(mut cell) = slot.cell.try_borrow_mut() {
                *cell = obj;
                slot.state.set(SlotState::Unmarked);
                self.bytes += std::mem::size_of::<Slot>();
                return Gc {
                    ptr,
                    _t: PhantomData,
                };
            }
            // 极罕见：刚回收的槽正被借用 —— 退回 free list，走下面的新槽路径。
            self.free.push(ptr);
        }
        let per_chunk = Self::slots_per_chunk();
        let chunk = match self.chunks.last_mut() {
            Some(c) if c.len() < c.capacity() => c,
            _ => {
                self.chunks.push(Vec::with_capacity(per_chunk));
                self.chunks.last_mut().expect("刚 push 过一块")
            }
        };
        chunk.push(Slot {
            state: Cell::new(SlotState::Unmarked),
            cell: RefCell::new(obj),
        });
        let slot: &Slot = chunk.last().expect("刚 push 过一个槽");
        self.bytes += std::mem::size_of::<Slot>();
        Gc {
            ptr: NonNull::from(slot),
            _t: PhantomData,
        }
    }

    /// 存活槽位的字节数（`docs/GC-DESIGN.md` §11：不含对象内 `Vec`/`HashMap`
    /// 的堆外内存，所以**不作**内存护栏的预算）。
    pub fn bytes_allocated(&self) -> usize {
        self.bytes
    }

    /// 是否该收集了。由 VM 在安全点轮询 —— **`alloc` 内部绝不触发**（INV-4）。
    pub(crate) fn should_collect(&self) -> bool {
        self.stress || self.bytes >= self.threshold
    }

    /// 把所有 `Marked` 复位为 `Unmarked`（放弃本轮 / 收集结束都用它）。
    fn reset_marks(&self) {
        for chunk in &self.chunks {
            for slot in chunk {
                if slot.state.get() == SlotState::Marked {
                    slot.state.set(SlotState::Unmarked);
                }
            }
        }
    }

    /// 精确标记：从 `roots` 出发传播可达性。**非递归**（worklist），深链不爆栈。
    ///
    /// 返回 `false` 表示本轮撞上了正在进行的可变借用 —— 调用方必须**放弃本轮**，
    /// 绝不能 panic（release 下 `panic = "abort"`，一次 panic 会带走整个进程及其
    /// 所在分片的全部结果；INV-5）。
    pub(crate) fn mark(&mut self, roots: Worklist) -> bool {
        self.mark_queue.clear();
        self.mark_queue.extend(roots);
        while let Some(g) = self.mark_queue.pop() {
            if g.state() == SlotState::Marked {
                continue;
            }
            // 读/写标记位**不需要借用**对象（标记位在 `RefCell` 外面）。
            g.set_state(SlotState::Marked);
            let Ok(obj) = g.try_borrow() else {
                return false;
            };
            obj.trace(&mut self.mark_queue);
        }
        true
    }

    /// Ephemeron 阶段：`WeakMap` / `WeakSet` 的**弱语义**（ES 23.3 / 23.4）。
    ///
    /// 主 mark **不** trace 弱容器的条目（`Trace for WeakMapObject` 只 trace 自身
    /// 属性与原型），否则键会被容器自己保活，"weak" 就不成立。故这里在 mark 之后、
    /// sweep 之前补一遍：
    ///
    /// - 键已 `Marked` ⇒ 该条目的**值**要标记（值可能牵出新的可达对象）
    /// - 键未 `Marked` ⇒ 条目已死，删掉（留墓碑，与 `delete` 一致）
    ///
    /// **必须迭代到不动点**：标记一个值可能让*另一个* `WeakMap` 的键变活
    /// （`wm1.set(a, wm2)` 而 `wm2` 只被这条引用 —— 单遍会漏掉）。标记是单调的，
    /// 故循环必然终止。
    ///
    /// 返回 `false` 表示撞上活跃借用 ⇒ 调用方必须放弃本轮（INV-5）。
    fn mark_ephemerons(&mut self) -> bool {
        loop {
            for chunk in &self.chunks {
                for slot in chunk {
                    // 容器本身不可达 ⇒ 条目随它一起消失，无需处理。
                    if slot.state.get() != SlotState::Marked {
                        continue;
                    }
                    let Ok(mut cell) = slot.cell.try_borrow_mut() else {
                        return false;
                    };
                    // 槽的标记位在 `RefCell` 外面，判定存活不需要借用对象。
                    let alive = |v: &Value| match v {
                        Value::Object(g) => g.state() == SlotState::Marked,
                        // ES 要求弱容器的键是对象；非对象不该出现，保活更安全。
                        _ => true,
                    };
                    match &mut *cell {
                        Object::WeakMap(wm) => {
                            wm.weak_ephemeron_step(&alive, &mut self.mark_queue);
                        }
                        Object::WeakSet(ws) => {
                            ws.weak_ephemeron_step(&alive);
                        }
                        _ => {}
                    }
                }
            }
            // 排空 worklist：值牵出的新对象要标记，它们又可能让别的弱条目变活。
            let mut progress = false;
            while let Some(g) = self.mark_queue.pop() {
                if g.state() == SlotState::Marked {
                    continue;
                }
                g.set_state(SlotState::Marked);
                progress = true;
                let Ok(obj) = g.try_borrow() else {
                    return false;
                };
                obj.trace(&mut self.mark_queue);
            }
            if !progress {
                break;
            }
        }
        true
    }

    /// 回收本轮未被标记的对象，返回回收的槽数。
    ///
    /// **为什么用 `mem::replace` 而不是 `drop_in_place`**：槽归 `Vec<Slot>` 所有，
    /// 堆析构时每个槽会被再 drop 一次。若把对象"就地挖空"，那第二次 drop 就是
    /// **双重释放**（实测 `free(): double free detected` → SIGABRT）。所以回收时
    /// 必须往槽里留下一个**有效**的占位对象，再把旧对象取出来 drop。
    ///
    /// 另外：先把所有待回收对象挑出来、再逐个 drop（`mem::replace` 之后槽里立刻
    /// 就是有效值），所以某个 `Drop` 读到别的 `Gc` 字段时读到的要么是原对象、
    /// 要么是占位对象 —— 都是合法内存，不会 UAF。
    pub(crate) fn sweep(&mut self) -> usize {
        // 单遍扫描：同时挑出待回收的、并把 Marked 复位为 Unmarked
        // （省掉一次全堆遍历 —— 回收是热的，全堆扫描是它的主要成本之一）。
        let mut doomed: Vec<NonNull<Slot>> = Vec::new();
        for chunk in &self.chunks {
            for slot in chunk {
                match slot.state.get() {
                    SlotState::Unmarked => doomed.push(NonNull::from(slot)),
                    SlotState::Marked => slot.state.set(SlotState::Unmarked),
                    SlotState::Free => {}
                }
            }
        }
        let mut freed = 0;
        for ptr in doomed {
            let slot = unsafe { &mut *ptr.as_ptr() };
            // 绝不 panic：正被借用就本轮先放着（下一轮再来）。
            let Ok(mut cell) = slot.cell.try_borrow_mut() else {
                continue;
            };
            let old = std::mem::replace(&mut *cell, Object::Ordinary(OrdinaryObject::new()));
            drop(old);
            slot.state.set(SlotState::Free);
            self.free.push(ptr);
            freed += 1;
        }
        self.bytes = self
            .bytes
            .saturating_sub(freed * std::mem::size_of::<Slot>());
        freed
    }

    /// 一次完整的 mark + sweep。`roots` 由调用方（VM）吐出。
    ///
    /// 返回回收的槽数；`None` = 本轮放弃（撞上活跃借用，INV-5）。
    pub(crate) fn collect(&mut self, roots: Worklist) -> Option<usize> {
        if !self.mark(roots) {
            self.reset_marks();
            // 放弃本轮时把阈值抬高，避免在同样的借用状态下反复空转。
            self.threshold = (self.bytes * 2).max(MIN_THRESHOLD);
            return None;
        }
        // 弱容器的条目要在 mark 之后、sweep 之前单独过一遍（ephemeron 语义）。
        if !self.mark_ephemerons() {
            self.reset_marks();
            self.threshold = (self.bytes * 2).max(MIN_THRESHOLD);
            return None;
        }
        let freed = self.sweep();
        self.threshold = (self.bytes * 2).max(MIN_THRESHOLD);
        Some(freed)
    }

    /// 已回收、等待复用的槽数（测试/调试用）。
    pub(crate) fn free_len(&self) -> usize {
        self.free.len()
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────
// 精确根集：Trace（`docs/GC-DESIGN.md` §6）
// ─────────────────────────────────────────────────────────

/// mark 阶段的 worklist。
///
/// `Trace` 只**追加**直接子引用，遍历由 worklist 驱动 —— **不递归**，否则深链表 /
/// 长闭包链会爆栈（`docs/GC-DESIGN.md` §8.1）。
// P2：收集器接线后即被调用（现阶段实现与单测先行）。
#[allow(dead_code)]
pub type Worklist = Vec<Gc<Object>>;

/// 精确根集协议：把**自己的直接子引用**追加到 `out`。
///
/// 契约：任何含 `Gc` / `Value` 字段的类型都必须实现它。**漏一个字段不会报错**，
/// 只会在某次收集之后读到一个已被回收的槽 —— 即静默 UAF（INV-3），所以每个实现
/// 都要配一条"构造不可达对象 → 验证它被回收"的单测。
#[allow(dead_code)]
pub trait Trace {
    fn trace(&self, out: &mut Worklist);
}

impl Trace for Gc<Object> {
    fn trace(&self, out: &mut Worklist) {
        out.push(*self);
    }
}

impl Trace for Value {
    fn trace(&self, out: &mut Worklist) {
        if let Value::Object(g) = self {
            out.push(*g);
        }
    }
}

impl<T: Trace> Trace for Option<T> {
    fn trace(&self, out: &mut Worklist) {
        if let Some(v) = self {
            v.trace(out);
        }
    }
}

impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, out: &mut Worklist) {
        for v in self {
            v.trace(out);
        }
    }
}

impl<T: Trace> Trace for std::collections::VecDeque<T> {
    fn trace(&self, out: &mut Worklist) {
        for v in self {
            v.trace(out);
        }
    }
}

impl<K, V: Trace> Trace for std::collections::HashMap<K, V> {
    fn trace(&self, out: &mut Worklist) {
        for v in self.values() {
            v.trace(out);
        }
    }
}

impl<T: Trace> Trace for [T] {
    fn trace(&self, out: &mut Worklist) {
        for v in self {
            v.trace(out);
        }
    }
}

impl<T: Trace, const N: usize> Trace for [T; N] {
    fn trace(&self, out: &mut Worklist) {
        for v in self {
            v.trace(out);
        }
    }
}

impl<T: Trace> Trace for Box<T> {
    fn trace(&self, out: &mut Worklist) {
        (**self).trace(out);
    }
}

/// 闭包捕获的共享单元（`vm::EnvCell`）。
///
/// 它不是堆对象，而是 VM / 函数对象 / 挂起生成器帧里的 `Rc`；收集器经由这些
/// 容器的 `Trace` 实现追到它，所以这里必须把**cell 里的值**继续往下传 ——
/// 漏了它，一个只活在闭包变量里的对象会在某次收集后变成悬垂句柄（INV-3）。
impl Trace for crate::vm::EnvCell {
    fn trace(&self, out: &mut Worklist) {
        // `try_borrow` 而不是 `borrow`：与对象同一契约（INV-5）——
        // 撞上一处正在进行的可变借用时放弃本轮传播，而不是 panic
        // （release 下 `panic = "abort"`，一次 panic 带走整个分片）。
        if let Ok(guard) = self.try_borrow()
            && let Some(value) = &*guard
        {
            value.trace(out);
        }
    }
}

/// 对象的**子引用** —— mark 阶段由它把可达性传播下去。
///
/// 各变体的具体字段实现见 `object.rs` / `iterator.rs` / `builtins/mod.rs`
/// （字段多为私有，所以实现必须写在各自的结构体所在模块）。
impl Trace for Object {
    fn trace(&self, out: &mut Worklist) {
        match self {
            Object::Ordinary(o) => o.trace(out),
            Object::Array(o) => o.trace(out),
            Object::Function(o) => o.trace(out),
            Object::NativeFunction(o) => o.trace(out),
            Object::PrimitiveWrapper(o) => o.trace(out),
            Object::Date(o) => o.trace(out),
            Object::RegExp(o) => o.trace(out),
            Object::Promise(o) => o.trace(out),
            Object::Global(o) => o.trace(out),
            Object::ArrayBuffer(o) => o.trace(out),
            Object::TypedArray(o) => o.trace(out),
            Object::DataView(o) => o.trace(out),
            Object::Proxy(o) => o.trace(out),
            Object::Map(o) => o.trace(out),
            Object::WeakMap(o) => o.trace(out),
            Object::WeakSet(o) => o.trace(out),
            Object::Set(o) => o.trace(out),
            Object::Generator(o) => o.trace(out),
            Object::NativeIterator(o) => o.trace(out),
            Object::Proto(o) => o.trace(out),
        }
    }
}

/// 宿主侧根栈。
///
/// **为什么它不放在 `Heap` 里**：分配要 `&mut Heap`，而一个存活的根守卫若持有
/// `&mut Heap`，就会把堆锁死 —— "持有根的同时再分配"直接编译不过（**实测 E0499**）。
/// 而这恰恰是原生内建回调进 JS 时的常态（`try_array_callback_method` / `enter_call` /
/// Promise job 排空：它们持有 `Value` 局部量，同时又驱动字节码到达安全点）。
/// 把根栈做成与 `Heap` **分离持有**的值之后，"登记根（`&Scope`）"与"分配
/// （`&mut Heap`）"是两个不相交的借用，可以并存。
///
/// GC 不扫机器栈（NG1），所以 Rust 局部变量里的 `Value` 对收集器**完全不可见**，
/// 必须显式登记在这里。
// P2：收集器接线后由 VM 持有（现阶段实现与单测先行）。
#[allow(dead_code)]
pub struct Scope {
    /// `RefCell` 是因为 `root` 只拿 `&self`：同一个 `Scope` 上要能同时挂多个守卫。
    roots: RefCell<Vec<Gc<Object>>>,
}

#[allow(dead_code)]
impl Scope {
    pub fn new() -> Self {
        Self {
            roots: RefCell::new(Vec::new()),
        }
    }

    /// 登记 `g` 为根。返回的守卫必须在 `g` 不再需要之前保持存活。
    ///
    /// 取 `Rc<Self>`（而不是 `&self`）是因为守卫要能跨越 `vm.invoke(..)` 这类
    /// `&mut self` 调用 —— 借用版会把整个 VM 锁住。克隆一个 `Rc` 很便宜。
    pub fn root(self: Rc<Self>, g: Gc<Object>) -> Root {
        self.roots.borrow_mut().push(g);
        Root {
            scope: Rc::clone(&self),
            ptr: Some(g),
        }
    }

    /// 登记 `v` 里的对象（非对象值不占根槽位）。
    pub fn root_value(self: Rc<Self>, v: &Value) -> Root {
        let ptr = match v {
            Value::Object(g) => Some(*g),
            _ => None,
        };
        if let Some(g) = ptr {
            self.roots.borrow_mut().push(g);
        }
        Root {
            scope: Rc::clone(&self),
            ptr,
        }
    }

    /// 分配 + 立即入根，消掉"已分配、还没入根"这个窗口（`GC-DESIGN.md` §6.2）。
    pub fn alloc(self: Rc<Self>, heap: &mut Heap, obj: Object) -> (Gc<Object>, Root) {
        let g = heap.alloc(obj);
        self.roots.borrow_mut().push(g);
        (g, Root {
            scope: Rc::clone(&self),
            ptr: Some(g),
        })
    }

    /// 当前登记的根（收集时并入根集）。
    pub fn roots(&self) -> Vec<Gc<Object>> {
        self.roots.borrow().clone()
    }

    /// 一次登记多个 `Value`（非对象值不占槽位）。`Drop` 时逆序注销。
    pub fn root_all(self: Rc<Self>, vs: &[Value]) -> Roots {
        let ptrs: Vec<Gc<Object>> = vs
            .iter()
            .filter_map(|v| match v {
                Value::Object(g) => Some(*g),
                _ => None,
            })
            .collect();
        {
            let mut roots = self.roots.borrow_mut();
            for g in &ptrs {
                roots.push(*g);
            }
        }
        Roots::new(Rc::clone(&self), ptrs)
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个已登记根的 RAII 守卫：构造即登记、`Drop` 即注销。
///
/// **不引入 `Rooted<'s, T>` 第二套值类型**（G2）：根的类型仍是 `Gc<Object>` / `Value`，
/// 这里只是多一层"已登记"的担保。
#[allow(dead_code)]
pub struct Root {
    scope: Rc<Scope>,
    /// 登记的那个句柄（非对象值为 `None`），只用于 `Drop` 时的 LIFO 断言。
    ptr: Option<Gc<Object>>,
}

impl Root {
    /// 被登记的那个句柄（非对象值为 `None`）。
    #[allow(dead_code)]
    pub fn handle(&self) -> Option<Gc<Object>> {
        self.ptr
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        // Rust 局部变量**逆序** drop ⇒ 天然 LIFO，故可以直接断言（INV-6）。
        if let Some(g) = self.ptr {
            let popped = self.scope.roots.borrow_mut().pop();
            debug_assert!(
                popped.is_some_and(|p| Gc::ptr_eq(&p, &g)),
                "根栈必须 LIFO 弹出"
            );
        }
    }
}

/// 一组根的守卫，用于一次登记多个 `Value`（例如某个原生存下来的**本地快照**）。
///
/// **`Drop` 必须逆序注销**：登记是正序压栈，而栈只能从顶上弹 —— 正序 drop 会
/// 先把别人的根弹掉，`debug_assert` 会当场报出来。
pub struct Roots {
    scope: Rc<Scope>,
    ptrs: Vec<Gc<Object>>,
}

impl Roots {
    fn new(scope: Rc<Scope>, ptrs: Vec<Gc<Object>>) -> Self {
        Self { scope, ptrs }
    }

    /// 增量入根：原生边算边累积时，每产出一个值就立刻登记。
    ///
    /// 之所以需要它：累加器（`map` 的 `values`、`groupBy` 的结果表…）是
    /// **Rust 局部量**，而每次调用用户回调都会跑到 GC 安全点。等到函数返回再
    /// 统一入根就晚了 —— 中间任何一次收集都会把已产出的那部分收走。
    pub fn push(&mut self, v: &Value) {
        if let Value::Object(g) = v {
            self.scope.roots.borrow_mut().push(*g);
            self.ptrs.push(*g);
        }
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        while let Some(g) = self.ptrs.pop() {
            let popped = self.scope.roots.borrow_mut().pop();
            debug_assert!(
                popped.is_some_and(|p| Gc::ptr_eq(&p, &g)),
                "根栈必须 LIFO 弹出"
            );
        }
    }
}

/// 所有 JS 对象的统一具体类型：`dyn` 消失、`Sized`。
///
/// 每个原 `JSObject` 实现体（`object.rs` 的 18 个 struct、`iterator.rs` 的
/// `NativeIteratorObject`、builtins 的 `ProtoObject`）在这里成为**一个变体**；
/// 各变体内部字段保持不变。`trait JSObject` 仍然保留，作为内部静态分发接口：
/// `forward!` 对每个变体调用 `JSObject::$method(o, …)`，靠具体类型在编译期选择实现。
#[derive(Debug)]
pub enum Object {
    Ordinary(OrdinaryObject),
    Array(ArrayObject),
    Function(FunctionObject),
    NativeFunction(NativeFunctionObject),
    PrimitiveWrapper(PrimitiveWrapperObject),
    Date(DateObject),
    RegExp(RegExpObject),
    Promise(PromiseObject),
    Global(GlobalObject),
    ArrayBuffer(ArrayBufferObject),
    TypedArray(TypedArrayObject),
    DataView(DataViewObject),
    Proxy(ProxyObject),
    Map(MapObject),
    WeakMap(WeakMapObject),
    WeakSet(WeakSetObject),
    Set(SetObject),
    Generator(GeneratorObject),
    NativeIterator(NativeIteratorObject),
    Proto(ProtoObject),
}

/// 把 `Object` 上的方法转发到各变体的 `JSObject` 实现。
/// 只读/可变接收者都适用：`&mut T` 会按需降级为 `&T`。
macro_rules! forward {
    ($this:expr, $method:ident $(, $arg:expr)*) => {
        match $this {
            Object::Ordinary(o) => JSObject::$method(o $(, $arg)*),
            Object::Array(o) => JSObject::$method(o $(, $arg)*),
            Object::Function(o) => JSObject::$method(o $(, $arg)*),
            Object::NativeFunction(o) => JSObject::$method(o $(, $arg)*),
            Object::PrimitiveWrapper(o) => JSObject::$method(o $(, $arg)*),
            Object::Date(o) => JSObject::$method(o $(, $arg)*),
            Object::RegExp(o) => JSObject::$method(o $(, $arg)*),
            Object::Promise(o) => JSObject::$method(o $(, $arg)*),
            Object::Global(o) => JSObject::$method(o $(, $arg)*),
            Object::ArrayBuffer(o) => JSObject::$method(o $(, $arg)*),
            Object::TypedArray(o) => JSObject::$method(o $(, $arg)*),
            Object::DataView(o) => JSObject::$method(o $(, $arg)*),
            Object::Proxy(o) => JSObject::$method(o $(, $arg)*),
            Object::Map(o) => JSObject::$method(o $(, $arg)*),
            Object::WeakMap(o) => JSObject::$method(o $(, $arg)*),
            Object::WeakSet(o) => JSObject::$method(o $(, $arg)*),
            Object::Set(o) => JSObject::$method(o $(, $arg)*),
            Object::Generator(o) => JSObject::$method(o $(, $arg)*),
            Object::NativeIterator(o) => JSObject::$method(o $(, $arg)*),
            Object::Proto(o) => JSObject::$method(o $(, $arg)*),
        }
    };
}

impl Object {
    pub fn kind(&self) -> ObjectKind {
        forward!(self, kind)
    }

    /// 若本对象是 Promise，返回其内部 `PromiseObject`（否则 `None`）。
    pub fn as_promise(&self) -> Option<&PromiseObject> {
        match self {
            Object::Promise(p) => Some(p),
            _ => None,
        }
    }

    /// `as_promise` 的可变版本。
    pub fn as_promise_mut(&mut self) -> Option<&mut PromiseObject> {
        match self {
            Object::Promise(p) => Some(p),
            _ => None,
        }
    }

    pub fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        forward!(self, property_get, key)
    }

    pub fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        forward!(self, property_set, key, value)
    }

    pub fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        forward!(self, define_property, key, desc)
    }

    pub fn property_delete(&mut self, key: &PropertyKey) -> bool {
        forward!(self, property_delete, key)
    }

    pub fn has_property(&self, key: &PropertyKey) -> bool {
        forward!(self, has_property, key)
    }

    pub fn own_keys(&self) -> Vec<PropertyKey> {
        forward!(self, own_keys)
    }

    pub fn get_prototype(&self) -> Option<Gc<Object>> {
        forward!(self, get_prototype)
    }

    pub fn set_prototype(&mut self, proto: Option<Gc<Object>>) {
        forward!(self, set_prototype, proto)
    }

    pub fn is_extensible(&self) -> bool {
        forward!(self, is_extensible)
    }

    pub fn prevent_extensions(&mut self) {
        forward!(self, prevent_extensions)
    }

    pub fn is_frozen(&self) -> bool {
        forward!(self, is_frozen)
    }

    pub fn freeze(&mut self) {
        forward!(self, freeze)
    }

    pub fn is_sealed(&self) -> bool {
        forward!(self, is_sealed)
    }

    pub fn seal(&mut self) {
        forward!(self, seal)
    }

    pub fn type_of(&self) -> &'static str {
        forward!(self, type_of)
    }

    pub fn class_name(&self) -> &'static str {
        forward!(self, class_name)
    }

    pub fn to_primitive(&self, hint: &str) -> Result<Value, String> {
        forward!(self, to_primitive, hint)
    }
}

// ─────────────────────────────────────────────────────────
// 具名类型访问器（取代 `as_any().downcast_ref::<T>()`）
// ─────────────────────────────────────────────────────────

/// 给每个变体生成 `as_<v>()` / `as_<v>_mut()`。
///
/// `Object` 是**具体枚举**，所以"取某个变体的内部值"就是一次判别匹配 —— 不需要
/// `dyn Any`，也不需要运行时类型 id 比较。调用点的形状与原先
/// `if let Some(x) = obj.as_any().downcast_ref::<T>()` **完全一致**，所以替换是机械的、
/// 且不会牵动多行链式调用的排版。
macro_rules! object_accessors {
    ( $( ($variant:ident, $ty:ty, $get:ident, $get_mut:ident) ),* $(,)? ) => {
        impl Object {
            $(
                pub fn $get(&self) -> Option<&$ty> {
                    match self {
                        Object::$variant(v) => Some(v),
                        _ => None,
                    }
                }
                pub fn $get_mut(&mut self) -> Option<&mut $ty> {
                    match self {
                        Object::$variant(v) => Some(v),
                        _ => None,
                    }
                }
            )*
        }
    };
}

object_accessors!(
    (Array, ArrayObject, as_array, as_array_mut),
    (Function, FunctionObject, as_function, as_function_mut),
    (Date, DateObject, as_date, as_date_mut),
    (RegExp, RegExpObject, as_regexp, as_regexp_mut),
    (Generator, GeneratorObject, as_generator, as_generator_mut),
    (ArrayBuffer, ArrayBufferObject, as_array_buffer, as_array_buffer_mut),
    (TypedArray, TypedArrayObject, as_typed_array, as_typed_array_mut),
    (DataView, DataViewObject, as_data_view, as_data_view_mut),
    (Proxy, ProxyObject, as_proxy, as_proxy_mut),
    (Map, MapObject, as_map, as_map_mut),
    (WeakMap, WeakMapObject, as_weak_map, as_weak_map_mut),
    (WeakSet, WeakSetObject, as_weak_set, as_weak_set_mut),
    (Set, SetObject, as_set, as_set_mut),
    (NativeIterator, NativeIteratorObject, as_native_iterator, as_native_iterator_mut),
);

impl Object {
    pub fn as_ordinary(&self) -> Option<&OrdinaryObject> {
        match self {
            Object::Ordinary(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_native_function(&self) -> Option<&NativeFunctionObject> {
        match self {
            Object::NativeFunction(v) => Some(v),
            _ => None,
        }
    }

    /// 判别谓词（取代 `as_any().is::<T>()`）。
    pub fn is_typed_array(&self) -> bool {
        matches!(self, Object::TypedArray(_))
    }
    pub fn is_array_buffer(&self) -> bool {
        matches!(self, Object::ArrayBuffer(_))
    }
    pub fn is_data_view(&self) -> bool {
        matches!(self, Object::DataView(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn key(s: &str) -> PropertyKey {
        PropertyKey::from_str(s)
    }

    /// 0. 尺寸守门：枚举尺寸 = 最大变体，`Object` 的大小直接决定 arena 块粒度与
    /// 内存曲线，所以钉住上限。
    ///
    /// 实测（2026-10-12）：`Value` 16 B（原 24 B，P1 的核心收益）、`Gc` 8 B、
    /// `Object` **624 B**。624 的来源是唯一异常变体 `GeneratorObject` —— 它内联了
    /// `SuspendedFrame`（含 `[Value; 19]` = 304 B），其余变体都在 56–168 B。
    /// 即"每个 `{}` 都为生成器付账"。见 `docs/REFACTOR-HEAP-GC.md` 阶段 A2。
    #[test]
    fn object_size_stays_within_budget() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::size_of::<Gc<Object>>(), 8);
        // A2 之后：`suspended` 装箱，实测 ~200 B。新增大变体必须先装箱，不要放宽此上限。
        assert!(
            std::mem::size_of::<Object>() <= 256,
            "Object = {} B；枚举尺寸由最大变体决定，新增大变体必须先装箱",
            std::mem::size_of::<Object>()
        );
    }

    /// 1. `Gc` 是 Copy：同一句柄可重复使用，且三者指向同一槽。
    #[test]
    fn gc_handle_is_copy() {
        let mut h = Heap::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = a; // Copy（非 move）
        let c = a; // a 仍然可用
        assert!(Gc::ptr_eq(&a, &b));
        assert!(Gc::ptr_eq(&a, &c));
    }

    /// 2. 借用守卫：`borrow_mut` 改字段后 `borrow` 能读回。
    #[test]
    fn borrow_guard_reads_back_writes() {
        let mut h = Heap::new();
        let g = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        assert_eq!(g.borrow().kind(), ObjectKind::Ordinary);
        g.borrow_mut()
            .property_set(key("x"), Value::Number(42.0))
            .unwrap();
        let desc = g.borrow().property_get(&key("x")).expect("property set");
        assert_eq!(desc.value, Value::Number(42.0));
    }

    /// 3. `ptr_eq` 真/假。
    #[test]
    fn ptr_eq_distinguishes_slots() {
        let mut h = Heap::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        assert!(Gc::ptr_eq(&a, &a));
        assert!(!Gc::ptr_eq(&a, &b));
    }

    /// `WeakMap` 的**弱**语义：条目不该把自己的键（和值）保活。
    ///
    /// 根集里只有 WeakMap 本身；键与值都只被这条弱引用指着 ⇒ 收集后它们应当消失，
    /// 条目本身也应当被 ephemeron 阶段清掉。
    #[test]
    fn weak_map_entries_do_not_keep_their_key_alive() {
        let mut h = Heap::new();
        let key = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let val = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let wm = h.alloc(Object::WeakMap(WeakMapObject::new(None)));
        {
            let mut b = wm.borrow_mut();
            match &mut *b {
                Object::WeakMap(m) => m.set(Value::Object(key), Value::Object(val)),
                _ => unreachable!("刚分配的 WeakMap"),
            }
        }
        // 根集只有 WeakMap —— key / val 都不在根里（收集器看不见机器栈上的它们）。
        let mut roots = Worklist::new();
        roots.push(wm);
        h.collect(roots);
        assert_eq!(key.state(), SlotState::Free, "WeakMap 的键不该被容器保活");
        assert_eq!(val.state(), SlotState::Free, "键死了 ⇒ 值也不该留着");
        let b = wm.borrow();
        if let Object::WeakMap(m) = &*b {
            assert!(
                !m.has(&Value::Object(key)),
                "键已死的条目应被 ephemeron 阶段删掉"
            );
        }
    }

    /// 反过来：键**存活**时，值必须被 ephemeron 阶段保住 ——
    /// 否则"只被 WeakMap 指向的值"会被误收（静默丢失）。
    #[test]
    fn weak_map_keeps_the_value_when_its_key_is_alive() {
        let mut h = Heap::new();
        let key = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let val = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let wm = h.alloc(Object::WeakMap(WeakMapObject::new(None)));
        {
            let mut b = wm.borrow_mut();
            match &mut *b {
                Object::WeakMap(m) => m.set(Value::Object(key), Value::Object(val)),
                _ => unreachable!("刚分配的 WeakMap"),
            }
        }
        // 根集是 WeakMap **和键**：值只被这条弱引用指着 ⇒ 必须靠 ephemeron 保住。
        let mut roots = Worklist::new();
        roots.push(wm);
        roots.push(key);
        h.collect(roots);
        assert_eq!(key.state(), SlotState::Unmarked, "键在根里 ⇒ 存活");
        assert_eq!(
            val.state(),
            SlotState::Unmarked,
            "键存活 ⇒ 值必须被 ephemeron 阶段标记保住"
        );
        let b = wm.borrow();
        if let Object::WeakMap(m) = &*b {
            assert!(m.has(&Value::Object(key)), "键存活 ⇒ 条目必须留着");
            assert_eq!(m.get(&Value::Object(key)), Some(Value::Object(val)));
        }
    }

    /// 4. 整堆释放：drop `Heap` 会析构堆内对象（用 `Rc` 强计数观测）。
    #[test]
    fn dropping_heap_drops_objects() {
        let payload = Rc::new("payload".to_string());
        let mut h = Heap::new();
        let arr = ArrayObject::from_vec(vec![Value::String(payload.clone())]);
        assert_eq!(Rc::strong_count(&payload), 2); // 本地 1 + 数组内 1
        let _g = h.alloc(Object::Array(arr));
        assert_eq!(Rc::strong_count(&payload), 2);
        drop(h);
        assert_eq!(Rc::strong_count(&payload), 1); // 堆内那份已随对象析构
    }

    /// 5. arena 地址稳定：跨块边界后，早先发出的句柄仍读到正确对象。
    #[test]
    fn arena_slots_survive_chunk_growth() {
        let mut h = Heap::new();
        let first = h.alloc(Object::Ordinary(OrdinaryObject::with_class_name("First")));
        // 再分配超过一整块，强制开新块（旧块若被 realloc/move 就会暴露）。
        for _ in 0..=Heap::slots_per_chunk() {
            h.alloc(Object::Ordinary(OrdinaryObject::new()));
        }
        assert_eq!(first.borrow().class_name(), "First");
        assert!(h.bytes_allocated() >= 2 * std::mem::size_of::<Slot>());
    }

    /// 7. INV-1：槽的状态位位于偏移 0 —— 从 `Gc` 的裸指针取状态位不需要偏移换算。
    #[test]
    fn slot_state_is_at_offset_zero() {
        assert_eq!(std::mem::offset_of!(Slot, state), 0);
    }

    /// 8. 标记位**不需要借用**对象：即使对象正被 `borrow_mut()` 持有，GC 仍能读写它。
    /// 这是 mark 阶段不会 panic 的前提（INV-5）。
    #[test]
    fn mark_bit_is_readable_while_mutably_borrowed() {
        let mut h = Heap::new();
        let g = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        assert_eq!(g.state(), SlotState::Unmarked);

        let _guard = g.borrow_mut(); // 可变借用仍在手上
        g.set_state(SlotState::Marked);
        assert_eq!(g.state(), SlotState::Marked);
        // 同一时刻 `borrow()` 会 panic，而 `try_borrow()` 只是报错 ——
        // trace 必须用后者，才能在撞上时放弃本轮而不是 abort 整个进程。
        assert!(g.try_borrow().is_err());
        drop(_guard);

        g.set_state(SlotState::Free);
        assert_eq!(g.state(), SlotState::Free);
    }

    /// 9. `alloc` 把槽置为 `Unmarked`（P2 的 sweep 以它为初值）。
    #[test]
    fn alloc_starts_slots_unmarked() {
        let mut h = Heap::new();
        let g = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        assert_eq!(g.state(), SlotState::Unmarked);
    }

    /// 10. `Trace` 只吐**直接子引用**，且非对象值不产生根。
    #[test]
    fn trace_collects_direct_children_only() {
        let mut h = Heap::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let mut wl = Worklist::new();
        Value::Object(a).trace(&mut wl);
        Value::Number(1.0).trace(&mut wl);
        Value::Undefined.trace(&mut wl);
        vec![Value::Object(a), Value::Bool(true)].trace(&mut wl);
        assert_eq!(wl.len(), 2);
        assert!(wl.iter().all(|g| Gc::ptr_eq(g, &a)));

        // 经容器嵌套同样成立（`Option` / `Vec` / `HashMap` / 定长数组）。
        let mut wl = Worklist::new();
        Some(Value::Object(a)).trace(&mut wl);
        std::collections::HashMap::from([("k", Value::Object(a))]).trace(&mut wl);
        [Value::Object(a), Value::Undefined].trace(&mut wl);
        assert_eq!(wl.len(), 3);
    }

    /// 11. 根栈：同一个 `Scope` 上可同时挂多个根；守卫 drop 后清空
    /// （逆序 drop ⇒ LIFO，`Drop` 里的 `debug_assert` 校验顺序）。
    #[test]
    fn scope_registers_and_unregisters_roots() {
        let mut h = Heap::new();
        let s = Rc::new(Scope::new());
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        assert!(s.roots().is_empty());
        {
            let ra = Rc::clone(&s).root(a);
            let rb = Rc::clone(&s).root(b);
            assert_eq!(s.roots().len(), 2);
            assert!(Gc::ptr_eq(&ra.handle().expect("已登记"), &a));
            assert!(Gc::ptr_eq(&rb.handle().expect("已登记"), &b));
        }
        assert!(s.roots().is_empty());
    }

    /// 12. **持有根的同时还能分配** —— 这是根栈不放在 `Heap` 里的全部理由
    /// （放进去就会撞 E0499：存活的守卫把 `&mut Heap` 锁死）。
    #[test]
    fn can_allocate_while_holding_a_root() {
        let mut h = Heap::new();
        let s = Rc::new(Scope::new());
        let (a, _ra) = Rc::clone(&s).alloc(&mut h, Object::Ordinary(OrdinaryObject::new()));
        // 守卫 `_ra` 还活着，分配仍然可用。
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let roots = s.roots();
        assert_eq!(roots.len(), 1);
        assert!(Gc::ptr_eq(&roots[0], &a));
        assert!(!Gc::ptr_eq(&a, &b));
    }

    /// 13b. 对象的**子引用**：`a.b = b; b.a = a` 这个环必须能被 trace 走通。
    /// 漏一个字段不会报错，只会让活对象被回收（UAF）—— 所以这条是 INV-3 的守卫。
    #[test]
    fn trace_follows_object_children_through_a_cycle() {
        let mut h = Heap::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        a.borrow_mut()
            .property_set(key("b"), Value::Object(b))
            .unwrap();
        b.borrow_mut()
            .property_set(key("a"), Value::Object(a))
            .unwrap();

        // 根 → a
        let mut wl = Worklist::new();
        Value::Object(a).trace(&mut wl);
        assert_eq!(wl.len(), 1);
        assert!(Gc::ptr_eq(&wl[0], &a));

        // a → b（属性表里的 `Value::Object`）
        let mut next = Worklist::new();
        wl[0].try_borrow().expect("a 未被借用").trace(&mut next);
        assert_eq!(next.len(), 1);
        assert!(Gc::ptr_eq(&next[0], &b));

        // b → a：环闭合
        let mut back = Worklist::new();
        next[0].try_borrow().expect("b 未被借用").trace(&mut back);
        assert_eq!(back.len(), 1);
        assert!(Gc::ptr_eq(&back[0], &a));
    }

    /// 13. 非对象值不占根槽位（否则 LIFO 断言会对不上）。
    #[test]
    fn root_value_ignores_non_objects() {
        let s = Rc::new(Scope::new());
        let none = Rc::clone(&s).root_value(&Value::Number(1.0)).handle().is_none();
        assert!(none);
        assert!(s.roots().is_empty());
    }

    /// 14. 收集：不可达的被回收，可达的活着（**没误回收**的证明）。
    #[test]
    fn collect_reclaims_unreachable_objects() {
        let mut h = Heap::new();
        let live = h.alloc(Object::Ordinary(OrdinaryObject::with_class_name("live")));
        let _garbage = h.alloc(Object::Ordinary(OrdinaryObject::with_class_name("garbage")));

        assert_eq!(h.collect(vec![live]).expect("本轮没有活跃借用"), 1);
        assert_eq!(h.free_len(), 1);
        // 活着的那个仍然可读。
        assert_eq!(live.borrow().class_name(), "live");
        // 标记位已复位，供下一轮。
        assert_eq!(live.state(), SlotState::Unmarked);
    }

    /// 15. **环能被回收** —— 这是整个 P1/P2 的动机
    /// （`Builtins::teardown` 那条 23 项手工断环清单就是为了它存在的）。
    #[test]
    fn collect_reclaims_a_cycle() {
        let mut h = Heap::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        a.borrow_mut()
            .property_set(key("b"), Value::Object(b))
            .unwrap();
        b.borrow_mut()
            .property_set(key("a"), Value::Object(a))
            .unwrap();

        // 只有 a 是根：b 经环可达 ⇒ 两个都活着。
        assert_eq!(h.collect(vec![a]).expect("本轮没有活跃借用"), 0);
        // 断掉根 ⇒ 整环不可达 ⇒ 两个都被回收。Rc 时代这是不可能的。
        assert_eq!(h.collect(Vec::new()).expect("本轮没有活跃借用"), 2);
    }

    /// 16. 回收后的槽会被**复用** —— 否则内存不下降，等于没收集。
    #[test]
    fn freed_slots_are_reused() {
        let mut h = Heap::new();
        let keep = h.alloc(Object::Ordinary(OrdinaryObject::with_class_name("keep")));
        let _tmp = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        h.collect(vec![keep]).expect("本轮没有活跃借用");

        let before = h.bytes_allocated();
        let reused = h.alloc(Object::Ordinary(OrdinaryObject::with_class_name("reused")));
        assert_eq!(h.bytes_allocated(), before + std::mem::size_of::<Slot>());
        assert_eq!(h.free_len(), 0);
        assert_eq!(reused.borrow().class_name(), "reused");
    }

    /// 17. trace 撞上正在进行的可变借用 ⇒ **放弃本轮**，绝不 panic（INV-5）。
    /// release 下 `panic = "abort"`，一次 panic 会带走整个进程及其分片的全部结果。
    #[test]
    fn collect_gives_up_instead_of_panicking() {
        let mut h = Heap::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        a.borrow_mut()
            .property_set(key("b"), Value::Object(b))
            .unwrap();

        let _guard = b.borrow_mut(); // b 正被可变借用
        assert!(h.collect(vec![a]).is_none());
        assert_eq!(h.free_len(), 0, "放弃的那一轮什么都不能回收");
        // 标记位已复位 ⇒ 下一轮可以正常进行。
        assert_eq!(a.state(), SlotState::Unmarked);
    }

    /// 6. 枚举转发在 3 个变体上一致。
    #[test]
    fn forwarding_works_on_all_variants() {
        let mut h = Heap::new();
        let g = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        g.borrow_mut()
            .property_set(key("a"), Value::Bool(true))
            .unwrap();
        assert!(g.borrow().has_property(&key("a")));

        let a = h.alloc(Object::Array(ArrayObject::new()));
        a.borrow_mut()
            .property_set(key("b"), Value::Number(1.0))
            .unwrap();
        assert_eq!(a.borrow().class_name(), "Array");
        assert!(a.borrow().has_property(&key("b")));

        let p = h.alloc(Object::Proto(crate::builtins::ProtoObject::new(None)));
        p.borrow_mut()
            .property_set(key("c"), Value::Number(2.0))
            .unwrap();
        assert!(p.borrow().has_property(&key("c")));
        assert_eq!(p.borrow().class_name(), "Object");
    }
}
