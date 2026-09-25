//! `Map` builtin (ES 23.1).
//!
//! The entry store itself lives in `vm::object::MapObject` (insertion order +
//! `SameValueZero` keys). This layer holds the parts that need no user code:
//! `get` / `set` / `has` / `delete` / `clear` and the `size` getter prologue.
//! Everything that *can* run user code — the constructor's iterable argument,
//! `forEach`, and the three iterator factories — is dispatched by the VM
//! (`vm/mod.rs`), which is the only place with a re-entrant call.
//!
//! `WeakMap` is deliberately absent: `Rc` has no weak form here, so it cannot
//! be implemented honestly (§8 of the plan records that deviation).

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{JSObject, MapObject, NativeFunctionObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// Native name of the `Map.prototype.size` getter, dispatched by the VM.
pub const MAP_SIZE_NATIVE: &str = "__map_size__";

/// Native name of the `Map[Symbol.species]` getter (ES 23.1.2.2): an accessor
/// that answers its receiver.
pub const MAP_SPECIES_NATIVE: &str = "__map_species__";

/// True when `this` is a `Map` instance.
///
/// Needed as a dispatch *guard*: `get` / `set` / `has` / `delete` / `clear` are
/// ordinary property names on any other object, so the prototype-method
/// dispatch may only claim them when the receiver really is a Map.
pub fn is_map_receiver(this: &Value) -> bool {
    matches!(this, Value::Object(obj) if obj.borrow().kind() == crate::vm::property::ObjectKind::Map)
}

/// Downcast a receiver to `MapObject`, raising the TypeError the spec asks for
/// when a Map prototype method is applied to something else
/// (`Map.prototype.get.call({}, 'x')`).
pub fn as_map<'a>(
    this: &'a Value,
    method: &str,
) -> Result<std::cell::RefMut<'a, MapObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "Map.prototype.{method} called on a non-object"
        )));
    };
    // Borrow first, then downcast: `RefCell` hands out one mutable borrow at a
    // time and the receiver is not borrowed anywhere else down this path.
    let borrowed = obj_ref.borrow_mut();
    if borrowed.kind() != crate::vm::property::ObjectKind::Map {
        return Err(RuntimeError::TypeError(format!(
            "Map.prototype.{method} called on an incompatible receiver"
        )));
    }
    Ok(std::cell::RefMut::map(borrowed, |obj| {
        obj.as_any_mut()
            .downcast_mut::<MapObject>()
            .expect("ObjectKind::Map implies MapObject")
    }))
}

/// `Map.prototype.size` — the number of entries.
///
/// The getter is reached as a *call* (`__map_size__`) with the receiver as
/// `this`, so unlike a real getter it has to validate the receiver itself.
pub fn map_size(this: &Value) -> Result<Value, RuntimeError> {
    let map = as_map(this, "size")?;
    Ok(Value::Number(map.size() as f64))
}

pub fn map_get(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let map = as_map(this, "get")?;
    Ok(map
        .get(args.first().unwrap_or(&Value::Undefined))
        .unwrap_or(Value::Undefined))
}

/// `Map.prototype.set(key, value)` returns the map itself, so `new
/// Map().set(1,2).set(3,4)` chains.
pub fn map_set(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
    {
        let mut map = as_map(this, "set")?;
        map.set(key, value);
    }
    Ok(this.clone())
}

pub fn map_has(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let map = as_map(this, "has")?;
    Ok(Value::Bool(
        map.has(args.first().unwrap_or(&Value::Undefined)),
    ))
}

pub fn map_delete(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut map = as_map(this, "delete")?;
    Ok(Value::Bool(map.delete(&key)))
}

pub fn map_clear(this: &Value, _args: &[Value]) -> Result<Value, RuntimeError> {
    let mut map = as_map(this, "clear")?;
    map.clear();
    Ok(Value::Undefined)
}

/// Populate `Map.prototype`.
///
/// `forEach` and `entries` / `keys` / `values` are registered as ordinary
/// prototype methods, but the VM intercepts all four (see
/// `VM::try_map_method`): they either call a user callback or mint an iterator
/// object, and neither is possible from this layer.
pub fn register_map_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;

    set_prototype_method(proto, "get", map_get);
    set_prototype_method(proto, "has", map_has);
    set_prototype_method(proto, "set", map_set);
    set_prototype_method(proto, "delete", map_delete);
    set_prototype_method(proto, "clear", map_clear);
    // The VM implements these four: they either call a user callback
    // (`forEach`) or mint an iterator object. They carry the Map-specific
    // prefix so the receiver check cannot be confused with
    // `Array.prototype.keys` and friends.
    super::set_map_method(proto, "forEach");
    super::set_map_method(proto, "entries");
    super::set_map_method(proto, "keys");
    super::set_map_method(proto, "values");
    // `upsert` (ES2026): `getOrInsert` needs no user code but shares the
    // "receiver must carry [[MapData]]" rule with the rest, so it rides the
    // same dispatch.
    super::set_map_method(proto, "getOrInsert");
    super::set_map_method(proto, "getOrInsertComputed");

    // `size` is an accessor (ES 23.1.3.9): a getter with no setter.
    let getter = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
        MAP_SIZE_NATIVE,
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

    // `Map.prototype[Symbol.toStringTag] === "Map"` — and `new Map()` already
    // reports `[object Map]` through `class_name`.
    let _ = proto.borrow_mut().define_property(
        super::to_string_tag_symbol_key(),
        PropertyDescriptor {
            value: Value::string("Map"),
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
}
