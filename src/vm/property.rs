use std::rc::Rc;

/// PropertyKey — property access key supporting String and Symbol
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropertyKey {
    Str(Rc<String>),
    Symbol(u64),
}

impl PropertyKey {
    pub fn from_str(s: &str) -> Self {
        PropertyKey::Str(Rc::new(s.to_string()))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropertyKey::Str(s) => Some(s.as_str()),
            PropertyKey::Symbol(_) => None,
        }
    }

    pub fn display(&self) -> String {
        match self {
            PropertyKey::Str(s) => s.to_string(),
            PropertyKey::Symbol(id) => format!("Symbol({id})"),
        }
    }
}

impl From<String> for PropertyKey {
    fn from(s: String) -> Self {
        PropertyKey::Str(Rc::new(s))
    }
}

impl From<&str> for PropertyKey {
    fn from(s: &str) -> Self {
        PropertyKey::Str(Rc::new(s.to_string()))
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
    /// Native (built-in) function
    NativeFunction,
}

// Forward-declare Value so property.rs can use it.
// The actual definition is in value.rs. This type alias bridges the crate-level import.
use crate::vm::Value;