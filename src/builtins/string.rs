use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{ArrayObject, JSObject};
use crate::vm::value::Value;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_string_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;

    set_prototype_method(proto, "valueOf", |this, _args| string_value_of(this));
    set_prototype_method(proto, "toString", |this, _args| string_to_string(this));
    set_prototype_method(proto, "charAt", |this, args| string_char_at(this, args));
    set_prototype_method(proto, "charCodeAt", |this, args| {
        string_char_code_at(this, args)
    });
    set_prototype_method(proto, "concat", |this, args| string_concat(this, args));
    set_prototype_method(proto, "includes", |this, args| string_includes(this, args));
    set_prototype_method(proto, "indexOf", |this, args| string_index_of(this, args));
    set_prototype_method(proto, "slice", |this, args| string_slice(this, args));
    set_prototype_method(proto, "substring", |this, args| {
        string_substring(this, args)
    });
    set_prototype_method(proto, "toUpperCase", |this, _args| string_to_upper(this));
    set_prototype_method(proto, "toLowerCase", |this, _args| string_to_lower(this));
    set_prototype_method(proto, "trim", |this, _args| string_trim(this));
    set_prototype_method(proto, "split", |this, args| string_split(this, args));
    set_prototype_method(proto, "startsWith", |this, args| string_starts_with(this, args));
    set_prototype_method(proto, "endsWith", |this, args| string_ends_with(this, args));
    set_prototype_method(proto, "repeat", |this, args| string_repeat(this, args));
    set_prototype_method(proto, "trimStart", |this, _args| {
        Ok(Value::string(&this.to_js_string().trim_start().to_string()))
    });
    set_prototype_method(proto, "trimEnd", |this, _args| {
        Ok(Value::string(&this.to_js_string().trim_end().to_string()))
    });
    set_prototype_method(proto, "padStart", |this, args| {
        string_pad(this, args, true)
    });
    set_prototype_method(proto, "padEnd", |this, args| {
        string_pad(this, args, false)
    });
    set_prototype_method(proto, "lastIndexOf", |this, args| {
        string_last_index_of(this, args)
    });
}

/// `String.fromCharCode(...)`
pub fn string_from_char_code(args: &[Value]) -> Result<Value, RuntimeError> {
    let mut out = String::new();
    for arg in args {
        let code = arg.to_number() as u32;
        if let Some(c) = char::from_u32(code & 0xFFFF) {
            out.push(c);
        }
    }
    Ok(Value::string(&out))
}

/// Register the `String` constructor's static methods.
pub fn register_string_statics(string_fn: &Value) {
    super::set_static_method(string_fn, "fromCharCode", |args| {
        string_from_char_code(args)
    });
}

pub fn string_starts_with(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    Ok(Value::Bool(
        s.starts_with(&args.first().map(|v| v.to_js_string()).unwrap_or_default()),
    ))
}

pub fn string_ends_with(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    Ok(Value::Bool(
        s.ends_with(&args.first().map(|v| v.to_js_string()).unwrap_or_default()),
    ))
}

pub fn string_repeat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let count = args.first().map(|v| v.to_number()).unwrap_or(0.0);
    if count < 0.0 || count.is_infinite() {
        return Err(RuntimeError::RangeError("Invalid count value".to_string()));
    }
    let count = count as usize;
    if s.len().saturating_mul(count) > 1 << 24 {
        return Err(RuntimeError::RangeError("Invalid string length".to_string()));
    }
    Ok(Value::string(&s.repeat(count)))
}

pub fn string_pad(obj: &Value, args: &[Value], at_start: bool) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let target = args.first().map(|v| v.to_number()).unwrap_or(0.0) as usize;
    let fill = args
        .get(1)
        .map(|v| v.to_js_string())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| " ".to_string());
    let len = s.chars().count();
    if target <= len || target > 1 << 20 {
        return Ok(Value::string(&s));
    }
    let pad_len = target - len;
    let fill_chars: Vec<char> = fill.chars().collect();
    let padding: String = (0..pad_len).map(|i| fill_chars[i % fill_chars.len()]).collect();
    Ok(Value::string(&if at_start {
        format!("{padding}{s}")
    } else {
        format!("{s}{padding}")
    }))
}

pub fn string_last_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let needle = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    match s.rfind(&needle) {
        Some(idx) => Ok(Value::Number(idx as f64)),
        None => Ok(Value::Number(-1.0)),
    }
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
    let idx = if args.is_empty() {
        0
    } else {
        args[0].to_number() as usize
    };
    if let Some(c) = s.chars().nth(idx) {
        Ok(Value::string(&c.to_string()))
    } else {
        Ok(Value::string(""))
    }
}

pub fn string_char_code_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let idx = if args.is_empty() {
        0
    } else {
        args[0].to_number() as usize
    };
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
    if args.is_empty() {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(s.contains(&args[0].to_js_string())))
}

pub fn string_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    if args.is_empty() {
        return Ok(Value::Number(-1.0));
    }
    match s.find(&args[0].to_js_string()) {
        Some(idx) => Ok(Value::Number(idx as f64)),
        None => Ok(Value::Number(-1.0)),
    }
}

pub fn string_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.is_empty() {
        0
    } else {
        let n = args[0].to_number() as i64;
        if n < 0 {
            ((len as i64) + n).max(0) as usize
        } else {
            (n as usize).min(len)
        }
    };
    let end = if args.len() < 2 {
        len
    } else {
        let n = args[1].to_number() as i64;
        if n < 0 {
            ((len as i64) + n).max(0) as usize
        } else {
            (n as usize).min(len)
        }
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
        return Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
            vec![Value::string(&s)],
        )))));
    }
    let sep = args[0].to_js_string();
    let parts: Vec<Value> = if sep.is_empty() {
        s.chars().map(|c| Value::string(&c.to_string())).collect()
    } else {
        s.split(&sep).map(|part| Value::string(part)).collect()
    };
    Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
        parts,
    )))))
}

pub fn string_substring(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = obj.to_js_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.is_empty() {
        0
    } else {
        let n = args[0].to_number() as i64;
        if n < 0 { 0 } else { (n as usize).min(len) }
    };
    let end = if args.len() < 2 {
        len
    } else {
        let n = args[1].to_number() as i64;
        if n < 0 { 0 } else { (n as usize).min(len) }
    };
    let from = start.min(end);
    let to = start.max(end);
    let result: String = chars[from..to].iter().collect();
    Ok(Value::string(&result))
}
