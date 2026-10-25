use crate::vm::atom::{well_known, Atom};
use crate::vm::gc::Heap;

/// ES 的**数组索引**（array index）：`[0, 2^32-2]` 内的整数，且十进制写法是
/// **规范形式**（除 `"0"` 本身外不以 `0` 开头）。
///
/// 这一条定义以前散落在各处（最明显的是 `object.rs` 的 `array_index_of`，它直接
/// `key.parse::<u32>()`，于是 `"01"` 被当成索引 1 —— `a["01"]` 读到了 `a[1]`，
/// 而 ES 里 `"01"` 是普通字符串键）。现在它是唯一的权威实现：
/// [`PropertyKey::Index`] 与 [`canonical_index`]。
pub fn canonical_index(s: &str) -> Option<u32> {
    // 长度先卡一道：`u32::MAX` 是 10 位，更长的必然越界（也顺带避开了溢出解析）。
    if s.is_empty() || s.len() > 10 {
        return None;
    }
    // 规范形式：全部是十进制数字（`u32::from_str` 会接受 `"+7"` 这类写法，
    // 但 ES 的索引键只认纯数字），且非 `"0"` 不得以 `0` 开头。
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    let index: u32 = s.parse().ok()?;
    // 2^32-1 不是数组索引（它留给数组长度上限的特殊处理）。
    if index == u32::MAX {
        return None;
    }
    Some(index)
}

/// PropertyKey — property access key supporting interned strings, array indices and symbols
///
/// 1A 第二步：字符串键是**驻留后的** [`Atom`]，不再每次访问分配一个 `Rc<String>`。
/// 于是属性存储的键、比较与哈希都是整数运算；把键变回字符串需要堆
/// （[`PropertyKey::resolve`]），只有真要输出字符串的地方才那么做。
///
/// `Index` 与 `Str` 是**两个不同的键** —— `a[1]` 与 `a["1"]` 在 ES 里是同一个
/// （因为 `"1"` 是数组索引 ⇒ 构造时就变成 `Index`），而 `a["01"]` 是 `Str`。
/// 这个归一发生在 [`PropertyKey::intern`] 里，所以**任何**从字符串来的键都一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropertyKey {
    Str(Atom),
    /// 数组索引（规范形式，见 [`canonical_index`]）。
    Index(u32),
    Symbol(u64),
}

impl PropertyKey {
    /// 常用属性名的键，**不需要堆**（查静态表 [`well_known`]）。
    ///
    /// `own_keys()` 这类拿不到 `&mut Heap` 的地方用它；返回 `None` 表示这个名字
    /// 没有预驻留，调用方应改走 [`PropertyKey::intern`]。
    pub fn well_known(name: &str) -> Option<Self> {
        well_known(name).map(PropertyKey::Str)
    }

    /// 字符串 → 键，**需要堆**（动态名字要驻留；规范索引归一成 `Index`）。
    ///
    /// 这是 `from_str` 的替代。字面量名字优先用 [`PropertyKey::well_known`]，
    /// 那样连堆都不用。
    pub fn intern(heap: &mut Heap, s: &str) -> Self {
        match canonical_index(s) {
            Some(index) => PropertyKey::Index(index),
            None => PropertyKey::Str(heap.intern(s)),
        }
    }

    pub fn from_index(index: u32) -> Self {
        PropertyKey::Index(index)
    }

    /// 由**长度内**的下标构造键（`arr[i]` / `arguments[i]` / TypedArray 下标…）。
    ///
    /// 下标一定小于数组长度（ES 上限 2^32-1），所以除 `u32::MAX` 外都能走 `Index`；
    /// 越界值退回字符串形式，与 `from_str` 的行为一致 —— 不会把键悄悄改掉。
    pub fn element(heap: &mut Heap, index: u64) -> Self {
        if index < u32::MAX as u64 {
            PropertyKey::Index(index as u32)
        } else {
            PropertyKey::intern(heap, &index.to_string())
        }
    }

    /// 数组索引（只有 `Index` 变体是索引键）。
    ///
    /// 字符串形式的规范索引在构造时就已经归一成 `Index`（见 [`PropertyKey::intern`]），
    /// 所以这里不需要再解析 —— 那正是"每次访问重新解析字符串"要消除的东西。
    pub fn array_index(&self) -> Option<u32> {
        match self {
            PropertyKey::Index(index) => Some(*index),
            _ => None,
        }
    }

    /// 键的字符串形式；`None` 表示这是索引或 Symbol 键。
    ///
    /// 需要堆（`Atom` 只是 id）。比较常用名请用 [`PropertyKey::is`] 而不是先解析再比。
    pub fn resolve<'a>(&self, heap: &'a Heap) -> Option<&'a str> {
        match self {
            PropertyKey::Str(atom) => Some(heap.resolve(*atom)),
            _ => None,
        }
    }

    /// 是不是某个**常用属性名**（整数比较，不需要堆）。
    ///
    /// `key.as_str() == Some("length")` 的形状一律改成 `key.is("length")` ——
    /// 这正是驻留要消除的"解析回字符串再比较"。
    pub fn is(&self, name: &str) -> bool {
        match self {
            PropertyKey::Str(atom) => well_known(name) == Some(*atom),
            _ => false,
        }
    }

    /// 字符串键的 `Atom`（索引 / Symbol 键没有）。
    pub fn as_atom(&self) -> Option<Atom> {
        match self {
            PropertyKey::Str(atom) => Some(*atom),
            _ => None,
        }
    }

    /// 键的字符串形式（错误消息 / `Object.keys` 输出用）。Symbol 渲染成
    /// `Symbol(id)`，与旧 `display()` 一致。
    pub fn display(&self, heap: &Heap) -> String {
        match self {
            PropertyKey::Str(atom) => heap.resolve(*atom).to_string(),
            PropertyKey::Index(index) => index.to_string(),
            PropertyKey::Symbol(id) => format!("Symbol({id})"),
        }
    }
}

/// PropertyDescriptor — describes a single property's attributes
/// Uses Value from the parent module (crate::vm::value)
#[derive(Debug, Clone)]
pub struct PropertyDescriptor {
    pub value: Value,
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub getter: Option<Value>,
    pub setter: Option<Value>,
}

impl PropertyDescriptor {
    pub fn data_descriptor(value: Value) -> Self {
        Self {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
            getter: None,
            setter: None,
        }
    }

    pub fn accessor_descriptor(getter: Option<Value>, setter: Option<Value>) -> Self {
        Self {
            value: Value::Undefined,
            writable: false,
            enumerable: true,
            configurable: true,
            getter,
            setter,
        }
    }

    pub fn is_data_descriptor(&self) -> bool {
        self.getter.is_none() && self.setter.is_none()
    }

    pub fn is_accessor_descriptor(&self) -> bool {
        self.getter.is_some() || self.setter.is_some()
    }

    /// The getter to *call*, if any.
    ///
    /// `{ get: undefined }` installs `undefined` in the slot: the field is
    /// *present* (so the descriptor stays an accessor one) but there is no
    /// function to invoke — reading the property yields `undefined`. Since
    /// `None` and `Some(undefined)` therefore behave identically at every
    /// call site, they are collapsed here.
    pub fn invoked_getter(&self) -> Option<&Value> {
        match &self.getter {
            Some(v) if !v.is_undefined() => Some(v),
            _ => None,
        }
    }

    /// The setter to *call*, if any (mirror of `invoked_getter`).
    ///
    /// An accessor installed with `set: undefined` has no setter, which in
    /// strict mode makes an assignment a TypeError.
    pub fn invoked_setter(&self) -> Option<&Value> {
        match &self.setter {
            Some(v) if !v.is_undefined() => Some(v),
            _ => None,
        }
    }

    pub fn writable_data_descriptor(value: Value, writable: bool) -> Self {
        Self {
            value,
            writable,
            enumerable: true,
            configurable: true,
            getter: None,
            setter: None,
        }
    }
}

/// ObjectKind — runtime classification of object types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Ordinary,
    Array,
    Function,
    String,
    Number,
    Boolean,
    Symbol,
    Date,
    RegExp,
    Error,
    Map,
    Set,
    WeakMap,
    WeakSet,
    Promise,
    Proxy,
    Arguments,
    Generator,
    Buffer,
    /// A TypedArray: an integer-indexed view over an `ArrayBuffer`.
    TypedArray,
    /// A `DataView`: a byte-addressable view over an `ArrayBuffer` (ES 25.3).
    DataView,
    /// Native (built-in) function
    NativeFunction,
}

// Forward-declare Value so property.rs can use it.
// The actual definition is in value.rs. This type alias bridges the crate-level import.
use crate::vm::Value;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::atom::WELL_KNOWN;

    /// 数组索引必须是**规范形式**：`"0"`、`"42"` 是索引，`"01"`、`"+7"`、
    /// `"1.0"`、`"4294967295"`、空串不是。
    #[test]
    fn canonical_index_accepts_only_the_canonical_spelling() {
        assert_eq!(canonical_index("0"), Some(0));
        assert_eq!(canonical_index("42"), Some(42));
        assert_eq!(canonical_index("4294967294"), Some(4294967294));
        assert_eq!(canonical_index("01"), None);
        assert_eq!(canonical_index("+7"), None);
        assert_eq!(canonical_index("1.0"), None);
        assert_eq!(canonical_index("-1"), None);
        assert_eq!(canonical_index(""), None);
        assert_eq!(canonical_index("4294967295"), None);
    }

    /// 构造时就分好：`intern` 对规范索引给出 `Index`，`"01"` 仍是字符串键。
    #[test]
    fn intern_produces_index_keys_for_canonical_indices() {
        let mut heap = Heap::new();
        assert_eq!(PropertyKey::intern(&mut heap, "1"), PropertyKey::Index(1));
        assert!(matches!(
            PropertyKey::intern(&mut heap, "01"),
            PropertyKey::Str(_)
        ));
        assert!(matches!(
            PropertyKey::intern(&mut heap, "x"),
            PropertyKey::Str(_)
        ));
    }

    /// 驻留是幂等的：同一名字（常用名或动态名）永远同一个键。
    #[test]
    fn interning_is_stable() {
        let mut heap = Heap::new();
        let a = PropertyKey::intern(&mut heap, "length");
        let b = PropertyKey::intern(&mut heap, "length");
        assert_eq!(a, b);
        assert_eq!(a, PropertyKey::well_known("length").expect("常用名"));

        let x = PropertyKey::intern(&mut heap, "a-dynamic-name");
        assert_eq!(x, PropertyKey::intern(&mut heap, "a-dynamic-name"));
        assert_ne!(x, PropertyKey::intern(&mut heap, "another-name"));
    }

    /// 常用名不需要堆 —— `own_keys()` 之类拿不到 `&mut Heap` 的地方靠它。
    #[test]
    fn well_known_names_need_no_heap() {
        assert_eq!(
            PropertyKey::well_known("length"),
            Some(PropertyKey::Str(
                crate::vm::atom::well_known("length").expect("常用名")
            ))
        );
        assert_eq!(PropertyKey::well_known("not-well-known"), None);
        // 整张表都能在不碰堆的情况下构造出来。
        for name in WELL_KNOWN {
            assert!(PropertyKey::well_known(name).is_some(), "{name}");
        }
    }

    /// `array_index()` 是唯一的判定入口，且只认 `Index` 变体。
    #[test]
    fn array_index_is_the_single_judgement() {
        let mut heap = Heap::new();
        assert_eq!(PropertyKey::Index(7).array_index(), Some(7));
        assert_eq!(PropertyKey::intern(&mut heap, "7").array_index(), Some(7));
        assert_eq!(PropertyKey::intern(&mut heap, "07").array_index(), None);
        assert_eq!(PropertyKey::intern(&mut heap, "length").array_index(), None);
        assert_eq!(PropertyKey::Symbol(1).array_index(), None);
    }

    /// 索引键的名字就是十进制写法（`arr[1]` 与 `arr["1"]` 同一个键）。
    #[test]
    fn display_renders_index_keys_as_decimal() {
        let heap = Heap::new();
        assert_eq!(PropertyKey::Index(12).display(&heap), "12");
        let mut heap = heap;
        assert_eq!(PropertyKey::intern(&mut heap, "12").display(&heap), "12");
        assert_eq!(PropertyKey::intern(&mut heap, "a").display(&heap), "a");
        assert_eq!(PropertyKey::Symbol(3).display(&heap), "Symbol(3)");
    }

    /// `element()`：长度内的下标走 `Index`，越界值退回字符串形式。
    #[test]
    fn element_uses_index_for_in_range_values() {
        let mut heap = Heap::new();
        assert_eq!(PropertyKey::element(&mut heap, 0), PropertyKey::Index(0));
        assert_eq!(PropertyKey::element(&mut heap, 7), PropertyKey::Index(7));
        assert_eq!(
            PropertyKey::element(&mut heap, u32::MAX as u64 - 1),
            PropertyKey::Index(u32::MAX - 1)
        );
        // 越界（≥ u32::MAX）不是数组索引，退回字符串形式，不把键悄悄改掉。
        assert!(matches!(
            PropertyKey::element(&mut heap, u32::MAX as u64),
            PropertyKey::Str(_)
        ));
    }
}
