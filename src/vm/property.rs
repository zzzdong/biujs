use std::rc::Rc;

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

/// PropertyKey — property access key supporting String, array index and Symbol
///
/// `Index` 是 1A 的第一步：数组索引键不再走 `Rc<String>` + 每次访问重新解析字符串。
/// 注意 `Index` 与 `Str` 是**两个不同的键** —— `a[1]` 与 `a["1"]` 在 ES 里是同一个
/// （因为 `"1"` 是数组索引 ⇒ 构造时就变成 `Index`），而 `a["01"]` 是 `Str`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropertyKey {
    Str(Rc<String>),
    /// 数组索引（规范形式，见 [`canonical_index`]）。
    Index(u32),
    Symbol(u64),
}

impl PropertyKey {
    pub fn from_str(s: &str) -> Self {
        match canonical_index(s) {
            Some(index) => PropertyKey::Index(index),
            None => PropertyKey::Str(Rc::new(s.to_string())),
        }
    }

    pub fn from_index(index: u32) -> Self {
        PropertyKey::Index(index)
    }

    /// 数组索引（无论键是 `Index` 还是字符串形式的规范索引）。
    ///
    /// 老代码里"先匹配 `Str`、再 `array_index_of(s)`"的形状一律改成问它，
    /// 于是 `Index` 与 `Str` 两条路在这里并成一条。
    pub fn array_index(&self) -> Option<u32> {
        match self {
            PropertyKey::Index(index) => Some(*index),
            PropertyKey::Str(s) => canonical_index(s.as_str()),
            PropertyKey::Symbol(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropertyKey::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        match self {
            PropertyKey::Str(s) => s.to_string(),
            PropertyKey::Index(index) => index.to_string(),
            PropertyKey::Symbol(id) => format!("Symbol({id})"),
        }
    }
}

impl From<String> for PropertyKey {
    fn from(s: String) -> Self {
        PropertyKey::from_str(&s)
    }
}

impl From<&str> for PropertyKey {
    fn from(s: &str) -> Self {
        PropertyKey::from_str(s)
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

    /// 构造时就分好：`PropertyKey::from_str` 对规范索引给出 `Index`。
    #[test]
    fn from_str_produces_index_keys() {
        assert_eq!(PropertyKey::from_str("1"), PropertyKey::Index(1));
        assert!(matches!(PropertyKey::from_str("01"), PropertyKey::Str(_)));
        assert!(matches!(PropertyKey::from_str("x"), PropertyKey::Str(_)));
    }

    /// `array_index()` 是唯一的判定入口：`Index` 与字符串形式的规范索引等价。
    #[test]
    fn array_index_is_the_single_judgement() {
        assert_eq!(PropertyKey::Index(7).array_index(), Some(7));
        assert_eq!(PropertyKey::from_str("7").array_index(), Some(7));
        assert_eq!(PropertyKey::from_str("07").array_index(), None);
        assert_eq!(PropertyKey::from_str("length").array_index(), None);
        assert_eq!(PropertyKey::Symbol(1).array_index(), None);
    }

    /// 索引键的名字就是十进制写法（`arr[1]` 与 `arr["1"]` 同一个键）。
    #[test]
    fn display_renders_index_keys_as_decimal() {
        assert_eq!(PropertyKey::Index(12).display(), "12");
        assert_eq!(PropertyKey::from_str("12").display(), "12");
        assert_eq!(PropertyKey::from_str("a").display(), "a");
    }
}
