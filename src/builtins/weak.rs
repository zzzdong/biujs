//! `WeakMap` / `WeakSet` builtins (ES 23.3 / 23.4).
//!
//! Both are small: the storage is `vm::object::WeakMapObject` /
//! `WeakSetObject`, which reuse the `Map`/`Set` entry vectors with an
//! object-key rule and **no** `size` / iteration / `clear` at all. Every method
//! is dispatched through the VM under its own prefix (`WEAKMAP_METHOD_PREFIX`,
//! `WEAKSET_METHOD_PREFIX`) so the receiver check is exact — `get` / `set` /
//! `has` / `delete` / `add` are ordinary property names elsewhere.
//!
//! **Registered deviation (plan §8)**: entries live in `Rc`, so a key that
//! becomes unreachable is *not* collected. The observable surface this batch
//! covers is the spec's own: object-only keys, the non-throwing answers of
//! `get`/`has`/`delete` for primitive keys, and the absence of enumeration.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{JSObject, WeakMapObject, WeakSetObject};
use crate::vm::property::ObjectKind;
use crate::vm::value::Value;

/// ES `CanBeHeldWeakly(v)` (24.2.1): an object, or a symbol that is *not*
/// registered in the global registry (`Symbol.for` keys are permanent, so they
/// are not held weakly).
///
/// A bare `Value::Function(id)` counts as an object: the VM boxes such values
/// before dispatch.
pub fn can_be_held_weakly(value: &Value) -> bool {
    match value {
        Value::Object(_) | Value::Function(_) => true,
        Value::Symbol(sym) => !sym.registered,
        _ => false,
    }
}

/// Kept as a short alias for the call sites below.
fn is_object_like(value: &Value) -> bool {
    can_be_held_weakly(value)
}

fn as_weak_map<'a>(
    this: &'a Value,
    method: &str,
) -> Result<std::cell::RefMut<'a, WeakMapObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "WeakMap.prototype.{method} called on a non-object"
        )));
    };
    let borrowed = obj_ref.borrow_mut();
    if borrowed.kind() != ObjectKind::WeakMap {
        return Err(RuntimeError::TypeError(format!(
            "WeakMap.prototype.{method} called on an incompatible receiver"
        )));
    }
    Ok(std::cell::RefMut::map(borrowed, |obj| {
        obj.as_any_mut()
            .downcast_mut::<WeakMapObject>()
            .expect("ObjectKind::WeakMap implies WeakMapObject")
    }))
}

fn as_weak_set<'a>(
    this: &'a Value,
    method: &str,
) -> Result<std::cell::RefMut<'a, WeakSetObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "WeakSet.prototype.{method} called on a non-object"
        )));
    };
    let borrowed = obj_ref.borrow_mut();
    if borrowed.kind() != ObjectKind::WeakSet {
        return Err(RuntimeError::TypeError(format!(
            "WeakSet.prototype.{method} called on an incompatible receiver"
        )));
    }
    Ok(std::cell::RefMut::map(borrowed, |obj| {
        obj.as_any_mut()
            .downcast_mut::<WeakSetObject>()
            .expect("ObjectKind::WeakSet implies WeakSetObject")
    }))
}

/// `WeakMap.prototype.set(key, value)` — a primitive key is a TypeError
/// (ES 23.3.3.5 step 3), unlike `get`/`has`/`delete`, which answer without
/// raising.
pub fn weakmap_set(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
    if !is_object_like(&key) {
        return Err(RuntimeError::TypeError(
            "Invalid value used as weak map key".to_string(),
        ));
    }
    as_weak_map(this, "set")?.set(key, value);
    Ok(this.clone())
}

pub fn weakmap_get(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let map = as_weak_map(this, "get")?;
    if !is_object_like(&key) {
        return Ok(Value::Undefined);
    }
    Ok(map.get(&key).unwrap_or(Value::Undefined))
}

pub fn weakmap_has(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let map = as_weak_map(this, "has")?;
    Ok(Value::Bool(is_object_like(&key) && map.has(&key)))
}

pub fn weakmap_delete(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut map = as_weak_map(this, "delete")?;
    Ok(Value::Bool(is_object_like(&key) && map.delete(&key)))
}

/// `WeakSet.prototype.add(value)` — a primitive is a TypeError (ES 23.4.3.1).
pub fn weakset_add(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    if !is_object_like(&value) {
        return Err(RuntimeError::TypeError(
            "Invalid value used in weak set".to_string(),
        ));
    }
    as_weak_set(this, "add")?.add(value);
    Ok(this.clone())
}

pub fn weakset_has(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let set = as_weak_set(this, "has")?;
    Ok(Value::Bool(is_object_like(&value) && set.has(&value)))
}

pub fn weakset_delete(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let mut set = as_weak_set(this, "delete")?;
    Ok(Value::Bool(is_object_like(&value) && set.delete(&value)))
}

/// `WeakMap.prototype.getOrInsert(key, value)` / `getOrInsertComputed(key, cb)`
/// (ES2026 `upsert`, already in the pinned test262). `getOrInsert` is pure; the
/// computed form needs the VM for the callback, so both are dispatched there.
pub fn register_weakmap_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    for name in [
        "set",
        "get",
        "has",
        "delete",
        "getOrInsert",
        "getOrInsertComputed",
    ] {
        super::set_vm_method(proto, super::WEAKMAP_METHOD_PREFIX, name);
    }
    super::define_string_tag(proto, "WeakMap");
}

/// Populate `WeakSet.prototype` (ES 23.4.3): `add` / `has` / `delete`.
pub fn register_weakset_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    for name in ["add", "has", "delete"] {
        super::set_vm_method(proto, super::WEAKSET_METHOD_PREFIX, name);
    }
    super::define_string_tag(proto, "WeakSet");
}
