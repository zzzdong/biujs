//! `Reflect` (ES 28.1) — the internal methods as functions.
//!
//! `Object.*` does the same operations with a *coercing* receiver and an
//! "answer the object" convention; `Reflect.*` insists on a real object, does no
//! coercion of the target, and answers a boolean where the operation can fail.
//! That is exactly what makes it the right basis for `Proxy` handlers, which is
//! why it goes in first.
//!
//! The methods that run user code (`get` / `set` / `apply` / `construct` /
//! `defineProperty`, whose descriptor may be an accessor) are dispatched by the
//! VM; the rest are here.

use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::{ArrayObject, JSObject, NativeFunctionObject, OrdinaryObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::{SymbolData, Value};
use crate::RuntimeError;

/// The methods the VM has to run, so both halves agree on the spelling.
pub const VM_METHODS: [&str; 5] = ["get", "set", "apply", "construct", "defineProperty"];

/// The builtin-only methods, in the order `Reflect` lists them.
pub const BUILTIN_METHODS: [(&str, fn(&[Value]) -> Result<Value, RuntimeError>); 8] = [
    ("has", reflect_has),
    ("deleteProperty", reflect_delete_property),
    ("ownKeys", reflect_own_keys),
    ("getOwnPropertyDescriptor", reflect_get_own_property_descriptor),
    ("getPrototypeOf", reflect_get_prototype_of),
    ("setPrototypeOf", reflect_set_prototype_of),
    ("isExtensible", reflect_is_extensible),
    ("preventExtensions", reflect_prevent_extensions),
];

/// `Reflect.*` addresses its target by index, and every method starts with "If
/// Type(target) is not Object, throw a TypeError" — no coercion, unlike
/// `Object.keys(1)` (ES 28.1.1).
pub fn target_of(args: &[Value], method: &str) -> Result<Rc<RefCell<dyn JSObject>>, RuntimeError> {
    match args.first() {
        Some(Value::Object(obj_ref)) => Ok(Rc::clone(obj_ref)),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "Reflect.{method} called on {}",
            other.type_of()
        ))),
        None => Err(RuntimeError::TypeError(format!(
            "Reflect.{method} requires an object"
        ))),
    }
}

/// `ToPropertyKey`: a symbol stays a symbol, everything else becomes a string.
pub fn to_key(value: &Value) -> PropertyKey {
    match value {
        Value::Symbol(sym) => PropertyKey::Symbol(sym.id),
        other => PropertyKey::from_str(&other.to_js_string()),
    }
}

/// `Reflect.has(target, key)` (ES 28.1.10) — the `in` operator.
pub fn reflect_has(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "has")?;
    let key = to_key(args.get(1).unwrap_or(&Value::Undefined));
    let has = crate::vm::prototype::internal_has_property(target, &key)
        .map_err(RuntimeError::TypeError)?;
    Ok(Value::Bool(has))
}

/// `Reflect.deleteProperty(target, key)` (ES 28.1.3) — `delete`, but it answers
/// `false` instead of throwing in strict mode.
pub fn reflect_delete_property(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "deleteProperty")?;
    let key = to_key(args.get(1).unwrap_or(&Value::Undefined));
    // `[[Delete]]` answers `true` for an absent property — the rule lives in
    // `internal_delete`, shared with the `delete` operator, which had answered
    // `false` there too.
    let deleted = crate::vm::prototype::internal_delete(target, &key)
        .map_err(RuntimeError::TypeError)?;
    Ok(Value::Bool(deleted))
}

/// `Reflect.ownKeys(target)` (ES 28.1.11): every own key — non-enumerable and
/// symbol ones included — as strings first, then symbols.
pub fn reflect_own_keys(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "ownKeys")?;
    let keys = target.borrow().own_keys();
    let mut strings: Vec<Value> = Vec::new();
    let mut symbols: Vec<Value> = Vec::new();
    for key in keys {
        match key {
            PropertyKey::Symbol(id) => {
                // A key carries only the id; the value is rebuilt from it. Two
                // symbols are equal exactly when their ids are, so the rebuilt
                // value is `===` the one used to create the property.
                symbols.push(Value::Symbol(Rc::new(SymbolData::new(None, id))));
            }
            PropertyKey::Str(text) => strings.push(Value::string(&text)),
        }
    }
    strings.extend(symbols);
    Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
        strings,
    )))))
}

/// `Reflect.getOwnPropertyDescriptor(target, key)` (ES 28.1.5) — the same answer
/// as `Object.getOwnPropertyDescriptor`, without the `ToObject` on the target.
pub fn reflect_get_own_property_descriptor(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "getOwnPropertyDescriptor")?;
    let key = to_key(args.get(1).unwrap_or(&Value::Undefined));
    let descriptor = target.borrow().property_get(&key);
    Ok(super::object::descriptor_to_value(descriptor))
}

/// `Reflect.getPrototypeOf(target)` (ES 28.1.6).
pub fn reflect_get_prototype_of(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "getPrototypeOf")?;
    let proto = target.borrow().get_prototype();
    Ok(match proto {
        Some(proto) => Value::Object(proto),
        None => Value::Null,
    })
}

/// `Reflect.setPrototypeOf(target, proto)` (ES 28.1.13) — a boolean, whereas
/// `Object.setPrototypeOf` throws.
pub fn reflect_set_prototype_of(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "setPrototypeOf")?;
    let proto = args.get(1).cloned().unwrap_or(Value::Undefined);
    let new_proto = match &proto {
        Value::Null => None,
        Value::Object(obj_ref) => Some(Rc::clone(obj_ref)),
        other => {
            return Err(RuntimeError::TypeError(format!(
                "Object prototype may only be an Object or null: {}",
                other.to_js_string()
            )))
        }
    };
    // Replacing the prototype of a non-extensible object is refused, not thrown
    // (ES 10.1.2.1 step 4).
    let changed = {
        let borrowed = target.borrow();
        let current = borrowed.get_prototype();
        let same = match (&current, &new_proto) {
            (None, None) => true,
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        };
        borrowed.is_extensible() || same
    };
    if !changed {
        return Ok(Value::Bool(false));
    }
    // A prototype chain that reaches the object itself would loop forever on
    // every later lookup. `[[SetPrototypeOf]]` *refuses* it (ES 10.1.2.1 step 6),
    // which `Reflect` reports as `false` and `Object.setPrototypeOf` throws.
    if let Some(proto) = &new_proto {
        let mut chain = Some(Rc::clone(proto));
        while let Some(link) = chain {
            if Rc::ptr_eq(&link, &target) {
                return Ok(Value::Bool(false));
            }
            chain = link.borrow().get_prototype();
        }
    }
    target.borrow_mut().set_prototype(new_proto);
    Ok(Value::Bool(true))
}

/// `Reflect.isExtensible(target)` (ES 28.1.7).
pub fn reflect_is_extensible(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "isExtensible")?;
    let extensible = target.borrow().is_extensible();
    Ok(Value::Bool(extensible))
}

/// `Reflect.preventExtensions(target)` (ES 28.1.12).
pub fn reflect_prevent_extensions(args: &[Value]) -> Result<Value, RuntimeError> {
    let target = target_of(args, "preventExtensions")?;
    target.borrow_mut().prevent_extensions();
    Ok(Value::Bool(true))
}

/// The `Reflect` namespace object (ES 28.1): an ordinary object with thirteen
/// methods, none of them enumerable, and `[Symbol.toStringTag] === "Reflect"`.
///
/// Every method is a name-only native: the VM dispatches five of them, and
/// `call_static_method` the other eight — the closures a prototype/method
/// descriptor carries are never called, only the name is read.
pub fn create_reflect_object() -> Value {
    let mut obj = OrdinaryObject::with_class_name("Reflect");
    // `Reflect` is a namespace object, not a constructor: it has no `prototype`
    // property of its own, but it *inherits* from `Object.prototype`
    // (`Object.getPrototypeOf(Reflect) === Object.prototype`).
    obj.set_prototype(crate::builtins::wrapper_prototype("Object"));
    let names: Vec<&str> = BUILTIN_METHODS
        .iter()
        .map(|(name, _)| *name)
        .chain(VM_METHODS.iter().copied())
        .collect();
    for name in names {
        let _ = obj.define_property(
            PropertyKey::from_str(name),
            super::method_descriptor(Value::Object(Rc::new(RefCell::new(
                NativeFunctionObject::new(&format!("Reflect.{name}")),
            )))),
        );
    }
    let _ = obj.define_property(
        PropertyKey::Symbol(super::symbol::TO_STRING_TAG_SYMBOL_ID),
        PropertyDescriptor {
            value: Value::string("Reflect"),
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
    Value::Object(Rc::new(RefCell::new(obj)))
}
