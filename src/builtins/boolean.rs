use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::JSObject;
use crate::vm::value::Value;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_boolean_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    use super::set_prototype_method;

    set_prototype_method(proto, heap, "valueOf", |this, _args| boolean_value_of(this));
    set_prototype_method(proto, heap, "toString", |this, _args| boolean_to_string(this));
}

// ─────────────────────────────────────────────────────────
// Boolean constructor
// ─────────────────────────────────────────────────────────

pub fn boolean_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        Ok(Value::Bool(false))
    } else {
        Ok(Value::Bool(args[0].to_boolean()))
    }
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

pub fn boolean_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    // Primitive wrappers (`Object(true)`) unwrap via ToPrimitive.
    Ok(Value::Bool(obj.to_primitive("default").to_boolean()))
}

pub fn boolean_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&obj.to_boolean().to_string()))
}
