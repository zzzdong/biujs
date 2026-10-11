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
    /// 已分配槽位的累计字节数（`alloc` 已是 `&mut self`，不需要 `Cell`）。
    bytes: usize,
}

impl Heap {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            bytes: 0,
        }
    }

    /// 每块的槽位数 = [`CHUNK_BYTES`] / `size_of::<Slot>()`，至少 1。
    fn slots_per_chunk() -> usize {
        (CHUNK_BYTES / std::mem::size_of::<Slot>()).max(1)
    }

    /// 唯一分配入口。`&mut self` 让"分配"与"借用已有对象"在类型层面互斥
    /// （borrow 走 `Gc`、不碰堆），因而不需要内部可变性。
    pub fn alloc(&mut self, obj: Object) -> Gc<Object> {
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

    /// 已分配槽位的累计字节数（近似值，用于调试）。
    ///
    /// 注意它**不含**对象内 `Vec` / `HashMap` 持有的堆外内存（数组元素、属性表才是
    /// 大头），所以**不能**拿来当内存护栏的预算 —— 见 `docs/GC-DESIGN.md` §11。
    pub fn bytes_allocated(&self) -> usize {
        self.bytes
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

impl<T: Trace, const N: usize> Trace for [T; N] {
    fn trace(&self, out: &mut Worklist) {
        for v in self {
            v.trace(out);
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
    pub fn root(&self, g: Gc<Object>) -> Root<'_> {
        self.roots.borrow_mut().push(g);
        Root {
            scope: self,
            ptr: Some(g),
        }
    }

    /// 登记 `v` 里的对象（非对象值不占根槽位）。
    pub fn root_value(&self, v: &Value) -> Root<'_> {
        let ptr = match v {
            Value::Object(g) => Some(*g),
            _ => None,
        };
        if let Some(g) = ptr {
            self.roots.borrow_mut().push(g);
        }
        Root { scope: self, ptr }
    }

    /// 分配 + 立即入根，消掉"已分配、还没入根"这个窗口（`GC-DESIGN.md` §6.2）。
    pub fn alloc(&self, heap: &mut Heap, obj: Object) -> (Gc<Object>, Root<'_>) {
        let g = heap.alloc(obj);
        self.roots.borrow_mut().push(g);
        (g, Root {
            scope: self,
            ptr: Some(g),
        })
    }

    /// 当前登记的根（收集时并入根集）。
    pub fn roots(&self) -> Vec<Gc<Object>> {
        self.roots.borrow().clone()
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
pub struct Root<'s> {
    scope: &'s Scope,
    /// 登记的那个句柄（非对象值为 `None`），只用于 `Drop` 时的 LIFO 断言。
    ptr: Option<Gc<Object>>,
}

impl Root<'_> {
    /// 被登记的那个句柄（非对象值为 `None`）。
    #[allow(dead_code)]
    pub fn handle(&self) -> Option<Gc<Object>> {
        self.ptr
    }
}

impl Drop for Root<'_> {
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

    /// 动态类型探测：把枚举内层具体对象作为 `&dyn Any` 暴露。
    ///
    /// `Object` 是具体枚举，但原先大量 `obj_ref.borrow().as_any().downcast_ref::<T>()`
    /// 探测位点仍按 trait 对象写法存在；转发到各变体的 `JSObject::as_any` 即可原样工作。
    pub fn as_any(&self) -> &dyn std::any::Any {
        forward!(self, as_any)
    }

    pub fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        forward!(self, as_any_mut)
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
        let s = Scope::new();
        let a = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        assert!(s.roots().is_empty());
        {
            let ra = s.root(a);
            let rb = s.root(b);
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
        let s = Scope::new();
        let (a, _ra) = s.alloc(&mut h, Object::Ordinary(OrdinaryObject::new()));
        // 守卫 `_ra` 还活着，分配仍然可用。
        let b = h.alloc(Object::Ordinary(OrdinaryObject::new()));
        let roots = s.roots();
        assert_eq!(roots.len(), 1);
        assert!(Gc::ptr_eq(&roots[0], &a));
        assert!(!Gc::ptr_eq(&a, &b));
    }

    /// 13. 非对象值不占根槽位（否则 LIFO 断言会对不上）。
    #[test]
    fn root_value_ignores_non_objects() {
        let s = Scope::new();
        let none = s.root_value(&Value::Number(1.0)).handle().is_none();
        assert!(none);
        assert!(s.roots().is_empty());
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
