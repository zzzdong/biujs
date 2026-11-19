use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::JSObject;
use crate::vm::value::Value;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_boolean_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    use super::set_prototype_method_impl;
    use crate::vm::native::{HeapImpl, NativeEntry};

    // 归属写在函数对象上，`thisBooleanValue` 留在实现里做 brand check。
    let methods: &[(&str, HeapImpl)] = &[
        ("valueOf", |_h, t, _a| boolean_value_of(t)),
        ("toString", |_h, t, _a| boolean_to_string(t)),
    ];
    for (name, entry) in methods {
        set_prototype_method_impl(proto, heap, name, NativeEntry::Heap(*entry));
    }
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
    // `thisBooleanValue`：Boolean 包装对象解包，别的接收者（`{}` / `0`）抛 TypeError。
    Ok(Value::Bool(super::require_boolean(obj)?))
}

pub fn boolean_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    let value = super::require_boolean(obj)?;
    Ok(Value::string(if value { "true" } else { "false" }))
}
