//! Iterator protocol support (ES6 §25.1).
//!
//! Dual-path design:
//!
//! * **Native fast path** — arrays, strings and `arguments` objects iterate
//!   through a [`NativeIteratorObject`] that holds internal state directly.
//!   Stepping it allocates nothing and never calls user code.
//! * **Protocol slow path** — any other object goes through
//!   `src[Symbol.iterator]()`, and every step calls `iterator.next()`.
//!
//! The iterator state is wrapped in a real `JSObject` so it flows through
//! registers/stack like any other value; it is never exposed to user code
//! (only `MakeIterator`/`IterateNext`/`IteratorClose` consume it).

use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::{JSObject, NativeFunctionObject};
use crate::vm::property::{ObjectKind, PropertyDescriptor, PropertyKey};
use crate::vm::value::{SymbolData, Value};

/// Reserved id for the well-known `Symbol.iterator`.
///
/// The runtime counter (`SYMBOL_COUNTER`) starts at 1, so a value near
/// `u64::MAX` cannot collide with user-created symbols.
pub const ITERATOR_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0000;

/// Internal name under which the `Symbol.iterator` native factory is
/// registered; dispatched by `call_native_by_name`.
pub const ITERATOR_NATIVE_NAME: &str = "__iterator_factory__";

/// Prefix for the per-iterator `next` method (`__iter_next__<id>`).
pub const ITERATOR_NEXT_PREFIX: &str = "__iter_next__";

/// Prefix for the per-iterator `return` method (`__iter_return__<id>`).
pub const ITERATOR_RETURN_PREFIX: &str = "__iter_return__";

/// `PropertyKey` for the well-known `Symbol.iterator`.
pub fn iterator_symbol_key() -> PropertyKey {
    PropertyKey::Symbol(ITERATOR_SYMBOL_ID)
}

/// The well-known `Symbol.iterator` value.
pub fn iterator_symbol_value() -> Value {
    Value::Symbol(Rc::new(SymbolData::new(
        Some("Symbol.iterator".to_string()),
        ITERATOR_SYMBOL_ID,
    )))
}

/// Internal state of an iterator produced by `MakeIterator`.
#[derive(Debug)]
pub enum NativeIteratorState {
    /// Fast path: snapshot of the elements at `MakeIterator` time.
    Array { items: Vec<Value>, idx: usize },
    /// Fast path: pre-split code units of the string.
    String { chars: Vec<Value>, idx: usize },
    /// Slow path: a JS iterator object from `src[Symbol.iterator]()`.
    Js { iterator: Value },
}

/// A `JSObject` wrapper around [`NativeIteratorState`].
#[derive(Debug)]
pub struct NativeIteratorObject {
    /// Registry id, so `next()`/`return()` natives can find this iterator in
    /// the VM's registry (user-facing methods, e.g. `it.next()`).
    pub id: u64,
    state: RefCell<NativeIteratorState>,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
}

impl NativeIteratorObject {
    pub fn new(id: u64, state: NativeIteratorState) -> Self {
        Self {
            id,
            state: RefCell::new(state),
            prototype: None,
        }
    }

    pub fn state(&self) -> &RefCell<NativeIteratorState> {
        &self.state
    }
}

impl JSObject for NativeIteratorObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Ordinary
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        let native_name = match key.as_str() {
            Some("next") => format!("{ITERATOR_NEXT_PREFIX}{}", self.id),
            Some("return") => format!("{ITERATOR_RETURN_PREFIX}{}", self.id),
            _ => return None,
        };
        Some(PropertyDescriptor::data_descriptor(Value::Object(
            Rc::new(RefCell::new(NativeFunctionObject::new(&native_name))),
        )))
    }

    fn property_set(&mut self, _key: PropertyKey, _value: Value) -> Result<bool, String> {
        Err("Cannot add properties to an iterator".to_string())
    }

    fn property_delete(&mut self, _key: &PropertyKey) -> bool {
        false
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        matches!(key.as_str(), Some("next") | Some("return"))
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        vec![
            PropertyKey::from_str("next"),
            PropertyKey::from_str("return"),
        ]
    }

    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        self.prototype = proto;
    }

    fn is_extensible(&self) -> bool {
        false
    }

    fn prevent_extensions(&mut self) {}

    fn is_frozen(&self) -> bool {
        true
    }

    fn freeze(&mut self) {}

    fn is_sealed(&self) -> bool {
        true
    }

    fn seal(&mut self) {}

    fn type_of(&self) -> &'static str {
        "object"
    }

    fn class_name(&self) -> &'static str {
        "Iterator"
    }
}

/// Fast-path step for array/string iterators. `None` means exhausted.
pub fn native_next(state: &mut NativeIteratorState) -> Option<Value> {
    match state {
        NativeIteratorState::Array { items, idx } | NativeIteratorState::String { chars: items, idx } => {
            if *idx < items.len() {
                let item = items[*idx].clone();
                *idx += 1;
                Some(item)
            } else {
                None
            }
        }
        NativeIteratorState::Js { .. } => None,
    }
}
