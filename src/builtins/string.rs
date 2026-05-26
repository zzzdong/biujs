use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::{ArrayObject, JSObject};
use crate::vm::value::Value;
use crate::RuntimeError;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_string_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;

    set_prototype_method(proto, "valueOf", |this, _args| string_value_of(this));
    set_prototype_method(proto, "toString", |this, _args| string_to_string(this));
    set_prototype_method(proto, "charAt", |this, args| string_char_at(this, args));
    set_prototype_method(proto, "charCodeAt", |this, args| string_char_code_at(this, args));
    set_prototype_method(proto, "concat", |this, args| string_concat(this, args));
    set_prototype_method(proto, "includes", |this, args| string_includes(this, args));
    set_prototype_method(proto, "indexOf", |this, args| string_index_of(this, args));
    set_prototype_method(proto, "slice", |this, args| string_slice(this, args));
    set_prototype_method(proto, "substring", |this, args| string_substring(this, args));
    set_prototype_method(proto, "toUpperCase", |this, _args| string_to_upper(this));
    set_prototype_method(proto, "toLowerCase", |this, _args| string_to_lower(this));
    set_prototype_method(proto, "trim", |this, _args| string_trim(this));
    set_prototype_method(proto, "split", |this, args| string_split(this, args));
}

fn string_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&obj.to_js_string()))
}

fn string_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&obj.to_js_string()))
}

// ─────────────────────────────────────────────────────────
// String constructor
// ─────────────────────────────────────────────────────────

pub fn string_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        Ok(Value::string(""))
    } else {
        let s = args[0].to_js_string();
        if s.len() > 1024 * 1024 * 256 {
            return Err(RuntimeError::RangeError("String too long".to_string()));
        }
        Ok(Value::string(&s))
    }
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

pub fn string_char_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let idx = if args.is_empty() { 0 } else { args[0].to_number() as usize };
    if let Some(c) = s.chars().nth(idx) {
        Ok(Value::string(&c.to_string()))
    } else {
        Ok(Value::string(""))
    }
}

pub fn string_char_code_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let idx = if args.is_empty() { 0 } else { args[0].to_number() as usize };
    if let Some(c) = s.chars().nth(idx) {
        Ok(Value::Number(c as u32 as f64))
    } else {
        Ok(Value::Number(f64::NAN))
    }
}

pub fn string_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut s = obj.to_js_string();
    for arg in args {
        s.push_str(&arg.to_js_string());
    }
    Ok(Value::string(&s))
}

pub fn string_includes(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    if args.is_empty() { return Ok(Value::Bool(false)); }
    Ok(Value::Bool(s.contains(&args[0].to_js_string())))
}

pub fn string_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    if args.is_empty() { return Ok(Value::Number(-1.0)); }
    match s.find(&args[0].to_js_string()) {
        Some(idx) => Ok(Value::Number(idx as f64)),
        None => Ok(Value::Number(-1.0)),
    }
}

pub fn string_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.is_empty() { 0 } else {
        let n = args[0].to_number() as i64;
        if n < 0 { ((len as i64) + n).max(0) as usize } else { (n as usize).min(len) }
    };
    let end = if args.len() < 2 { len } else {
        let n = args[1].to_number() as i64;
        if n < 0 { ((len as i64) + n).max(0) as usize } else { (n as usize).min(len) }
    };
    if start >= end || start >= len {
        return Ok(Value::string(""));
    }
    let end = end.min(len);
    let result: String = chars[start..end].iter().collect();
    Ok(Value::string(&result))
}

pub fn string_to_upper(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&obj.to_js_string().to_uppercase()))
}

pub fn string_to_lower(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&obj.to_js_string().to_lowercase()))
}

pub fn string_trim(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&obj.to_js_string().trim().to_string()))
}

pub fn string_split(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    if args.is_empty() {
        return Ok(Value::Object(Rc::new(RefCell::new(
            ArrayObject::from_vec(vec![Value::string(&s)]),
        ))));
    }
    let sep = args[0].to_js_string();
    let parts: Vec<Value> = if sep.is_empty() {
        s.chars().map(|c| Value::string(&c.to_string())).collect()
    } else {
        s.split(&sep).map(|part| Value::string(part)).collect()
    };
    Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(parts)))))
}

pub fn string_substring(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.is_empty() { 0 } else {
        let n = args[0].to_number() as i64;
        if n < 0 { 0 } else { (n as usize).min(len) }
    };
    let end = if args.len() < 2 { len } else {
        let n = args[1].to_number() as i64;
        if n < 0 { 0 } else { (n as usize).min(len) }
    };
    let from = start.min(end);
    let to = start.max(end);
    let result: String = chars[from..to].iter().collect();
    Ok(Value::string(&result))
}
