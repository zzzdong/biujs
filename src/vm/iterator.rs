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

use crate::vm::gc::{Gc, Heap, Object};
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

/// Prefix for a generator's `next` method (`__gen_next__<id>`) — the method is
/// a native because resuming the body needs the VM.
pub const GENERATOR_NEXT_PREFIX: &str = "__gen_next__";

/// Prefix for a generator's `return` method (`__gen_return__<id>`).
pub const GENERATOR_RETURN_PREFIX: &str = "__gen_return__";

/// Prefix for a generator's `throw` method (`__gen_throw__<id>`).
pub const GENERATOR_THROW_PREFIX: &str = "__gen_throw__";

/// Prefix for a generator's `[Symbol.iterator]` factory (`__gen_iterator__<id>`),
/// which hands the generator itself back.
pub const GENERATOR_ITERATOR_PREFIX: &str = "__gen_iterator__";

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

/// Which half of a Map entry a Map iterator yields (ES 23.1.5.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MapIterKind {
    Key,
    Value,
    /// `[key, value]` pairs — what `Map.prototype.entries` and
    /// `Map.prototype[Symbol.iterator]` produce.
    Entry,
}

impl MapIterKind {
    /// Render one live entry as the value this iterator kind yields.
    fn project(&self, heap: &mut crate::vm::gc::Heap, key: Value, value: Value) -> Value {
        match self {
            MapIterKind::Key => key,
            MapIterKind::Value => value,
            MapIterKind::Entry => Value::Object(heap.alloc(Object::Array(
                crate::vm::object::ArrayObject::from_vec(vec![key, value]),
            ))),
        }
    }
}

/// Which shape a Set iterator yields (ES 23.2.5.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SetIterKind {
    /// `Set.prototype.keys` / `values` / `[Symbol.iterator]` — the value.
    Value,
    /// `Set.prototype.entries` — a `[value, value]` pair.
    Entry,
}

impl SetIterKind {
    fn project(&self, heap: &mut crate::vm::gc::Heap, value: Value) -> Value {
        match self {
            SetIterKind::Value => value,
            SetIterKind::Entry => Value::Object(heap.alloc(Object::Array(
                crate::vm::object::ArrayObject::from_vec(vec![value.clone(), value]),
            ))),
        }
    }
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
    /// A *live* iterator over a `Map`: it walks the map itself, so entries
    /// added during the iteration are visited and deleted ones are skipped.
    ///
    /// The map is held as `dyn JSObject` because an `Rc<RefCell<dyn …>>` cannot
    /// be downcast to a concrete `Rc`; each step downcasts through `as_any`.
    Map {
        map: Gc<Object>,
        kind: MapIterKind,
        idx: usize,
        /// Once the walk has run off the end, the iterator stays done even if
        /// new entries appear (ES 23.1.5.2.1 step 12: `[[MapIteratorMap]]` is
        /// cleared, so a later `next()` answers `undefined` immediately).
        done: bool,
    },
    /// A *live* iterator over a `Set` — same discipline as the `Map` one.
    Set {
        set: Gc<Object>,
        kind: SetIterKind,
        idx: usize,
        /// See the `Map` arm: exhaustion is permanent (`values-iteration-mutable`).
        done: bool,
    },
}

/// A `JSObject` wrapper around [`NativeIteratorState`].
#[derive(Debug)]
pub struct NativeIteratorObject {
    /// Registry id, so `next()`/`return()` natives can find this iterator in
    /// the VM's registry (user-facing methods, e.g. `it.next()`).
    pub id: u64,
    state: RefCell<NativeIteratorState>,
    prototype: Option<Gc<Object>>,
    /// 预先构造的 `[Symbol.iterator]` / `next` / `return` 原生方法 Value。
    ///
    /// `property_get` 是 `&self`、拿不到堆，无法现分配 `NativeFunctionObject`；
    /// 因此这三个句柄在构造点（有堆）一次性生成并存起来。
    iterator_fn: Value,
    next_fn: Value,
    return_fn: Value,
}

impl NativeIteratorObject {
    pub fn new(
        heap: &mut crate::vm::gc::Heap,
        id: u64,
        state: NativeIteratorState,
    ) -> Self {
        let mk = |heap: &mut crate::vm::gc::Heap, name: String| {
            Value::Object(heap.alloc(Object::NativeFunction(NativeFunctionObject::new(&name))))
        };
        let iterator_fn = mk(heap, ITERATOR_NATIVE_NAME.to_string());
        let next_fn = mk(heap, format!("{ITERATOR_NEXT_PREFIX}{id}"));
        let return_fn = mk(heap, format!("{ITERATOR_RETURN_PREFIX}{id}"));
        Self {
            id,
            state: RefCell::new(state),
            prototype: None,
            iterator_fn,
            next_fn,
            return_fn,
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


    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        // A native iterator is itself iterable: `it[Symbol.iterator]` is the
        // standard iterator factory (which hands the same iterator back).
        if *key == crate::vm::iterator::iterator_symbol_key() {
            return Some(PropertyDescriptor::data_descriptor(self.iterator_fn.clone()));
        }
        if key.is("next") {
            Some(PropertyDescriptor::data_descriptor(self.next_fn.clone()))
        } else if key.is("return") {
            Some(PropertyDescriptor::data_descriptor(self.return_fn.clone()))
        } else {
            None
        }
    }

    fn property_set(&mut self, _key: PropertyKey, _value: Value) -> Result<bool, String> {
        Err("Cannot add properties to an iterator".to_string())
    }

    fn define_property(
        &mut self,
        _key: PropertyKey,
        _desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        Err("Cannot add properties to an iterator".to_string())
    }

    fn property_delete(&mut self, _key: &PropertyKey) -> bool {
        false
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        *key == crate::vm::iterator::iterator_symbol_key()
            || key.is("next") || key.is("return")
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        vec![
            PropertyKey::well_known("next").expect("well-known name"),
            PropertyKey::well_known("return").expect("well-known name"),
        ]
    }

    fn get_prototype(&self) -> Option<Gc<Object>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Gc<Object>>) {
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

/// Every remaining value of a native iterator, drained in one go.
///
/// `Array.from` needs this twice over: when the source *is* an iterator
/// (`str.matchAll(re)`, `arr.keys()`) and when it merely has to be turned into
/// one (a Set or a Map). `None` when `iter_val` is not a native iterator — a
/// generator, whose steps can run user code and so need the VM's `iterator_next`.
pub fn drain_native_iterator(heap: &mut crate::vm::gc::Heap, iter_val: &Value) -> Option<Vec<Value>> {
    let Value::Object(obj_ref) = iter_val else {
        return None;
    };
    let is_native = obj_ref
        .borrow().as_native_iterator()
        .is_some();
    if !is_native {
        return None;
    }
    let mut out = Vec::new();
    loop {
        // One step per borrow: `native_next` mutates the state, and a `RefMut`
        // held across the call would panic on the next turn of the loop.
        let next = {
            let borrowed = obj_ref.borrow();
            let iterator = borrowed.as_native_iterator()
                .expect("checked above");
            native_next(heap, &mut iterator.state().borrow_mut())
        };
        match next {
            Some(value) => out.push(value),
            None => break,
        }
    }
    Some(out)
}

/// Fast-path step for array/string iterators. `None` means exhausted.
pub fn native_next(
    heap: &mut crate::vm::gc::Heap,
    state: &mut NativeIteratorState,
) -> Option<Value> {
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
        NativeIteratorState::Set { set, kind, idx, done } => {
            if *done {
                return None;
            }
            let borrowed = set.borrow();
            let step = borrowed.as_set()
                .and_then(|set| set.entry_from(*idx));
            match step {
                Some((i, value)) => {
                    *idx = i + 1;
                    Some(kind.project(heap, value))
                }
                None => {
                    *done = true;
                    None
                }
            }
        }
        NativeIteratorState::Map { map, kind, idx, done } => {
            if *done {
                return None;
            }
            // The map is borrowed for one step only: whatever runs between two
            // `next()` calls is allowed to mutate it (that is the whole point of
            // a Map iterator being live).
            let borrowed = map.borrow();
            let step = borrowed.as_map()
                .and_then(|map| map.entry_from(*idx));
            match step {
                Some((i, key, value)) => {
                    *idx = i + 1;
                    Some(kind.project(heap, key, value))
                }
                None => {
                    *done = true;
                    None
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────
// GC：迭代器的子引用（INV-3）
// ─────────────────────────────────────────────────────────

use crate::vm::gc::{Trace, Worklist};

impl Trace for NativeIteratorState {
    fn trace(&self, out: &mut Worklist) {
        match self {
            NativeIteratorState::Array { items, .. } => items.trace(out),
            NativeIteratorState::String { chars, .. } => chars.trace(out),
            NativeIteratorState::Js { iterator } => iterator.trace(out),
            NativeIteratorState::Map { map, .. } => map.trace(out),
            NativeIteratorState::Set { set, .. } => set.trace(out),
        }
    }
}

impl Trace for NativeIteratorObject {
    fn trace(&self, out: &mut Worklist) {
        self.prototype.trace(out);
        self.iterator_fn.trace(out);
        self.next_fn.trace(out);
        self.return_fn.trace(out);
        // 绝不 panic：正在被可变借用时跳过（INV-5）。
        if let Ok(state) = self.state.try_borrow() {
            state.trace(out);
        }
    }
}
