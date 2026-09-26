//! `Set` builtin (ES 23.2).
//!
//! Mirrors `builtins/map.rs`: the entry store is `vm::object::SetObject`
//! (insertion order + `SameValueZero`), the pure methods live here, and
//! everything that can run user code (`new Set(iterable)`, `forEach`, the three
//! iterator factories) is dispatched by the VM.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{JSObject, NativeFunctionObject, SetObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// Native name of the `Set.prototype.size` getter, dispatched by the VM.
pub const SET_SIZE_NATIVE: &str = "__set_size__";

/// Native name of the `Set[Symbol.species]` getter (ES 23.2.2.2).
pub const SET_SPECIES_NATIVE: &str = "__set_species__";

/// True when `this` is a `Set` instance — the guard that keeps `add` / `has` /
/// `delete` / `clear` from hijacking a same-named property on any other object.
pub fn is_set_receiver(this: &Value) -> bool {
    matches!(this, Value::Object(obj) if obj.borrow().kind() == crate::vm::property::ObjectKind::Set)
}

/// Downcast a receiver to `SetObject`, or raise the TypeError the spec asks for
/// when a `Set.prototype` method is applied to something else.
pub fn as_set<'a>(
    this: &'a Value,
    method: &str,
) -> Result<std::cell::RefMut<'a, SetObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "Set.prototype.{method} called on a non-object"
        )));
    };
    let borrowed = obj_ref.borrow_mut();
    if borrowed.kind() != crate::vm::property::ObjectKind::Set {
        return Err(RuntimeError::TypeError(format!(
            "Set.prototype.{method} called on an incompatible receiver"
        )));
    }
    Ok(std::cell::RefMut::map(borrowed, |obj| {
        obj.as_any_mut()
            .downcast_mut::<SetObject>()
            .expect("ObjectKind::Set implies SetObject")
    }))
}

/// `Set.prototype.size`.
pub fn set_size(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(as_set(this, "size")?.size() as f64))
}

/// `Set.prototype.add(value)` returns the set, so it chains.
pub fn set_add(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    as_set(this, "add")?.add(value);
    Ok(this.clone())
}

pub fn set_has(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Bool(
        as_set(this, "has")?.has(args.first().unwrap_or(&Value::Undefined)),
    ))
}

pub fn set_delete(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(as_set(this, "delete")?.delete(&value)))
}

pub fn set_clear(this: &Value, _args: &[Value]) -> Result<Value, RuntimeError> {
    as_set(this, "clear")?.clear();
    Ok(Value::Undefined)
}

/// Populate `Set.prototype`.
///
/// `keys`/`values`/`entries`/`forEach` are registered as Map-style VM methods:
/// they either call a user callback or mint an iterator object, and they need
/// the same "receiver must carry `[[SetData]]`" check.
pub fn register_set_prototype(proto: &Rc<RefCell<dyn JSObject>>) {

    // All of these carry the Set-specific dispatch prefix (see
    // `MAP_METHOD_PREFIX`): `has`/`delete`/`clear` also exist on
    // `Map.prototype` and are ordinary property names elsewhere, while
    // `forEach`/`entries`/`values` need the receiver-kind check.
    for name in [
        "add",
        "has",
        "delete",
        "clear",
        "forEach",
        "entries",
        "values",
        // `set-methods` (ES2024): the seven operators take a *set-like*
        // argument and drive its `has`/`keys` through user code, so they are
        // VM dispatching as well.
        "union",
        "intersection",
        "difference",
        "symmetricDifference",
        "isSubsetOf",
        "isSupersetOf",
        "isDisjointFrom",
    ] {
        super::set_set_method(proto, name);
    }

    // `Set.prototype.size` is an accessor (ES 23.2.3.9).
    let getter = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
        SET_SIZE_NATIVE,
    ))));
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("size"),
        PropertyDescriptor {
            value: Value::Undefined,
            writable: false,
            enumerable: false,
            configurable: true,
            getter: Some(getter),
            setter: None,
        },
    );

    let _ = proto.borrow_mut().define_property(
        super::to_string_tag_symbol_key(),
        PropertyDescriptor {
            value: Value::string("Set"),
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
}
