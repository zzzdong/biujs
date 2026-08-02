use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::JSObject;
use crate::vm::value::Value;

use super::number_to_string;
use super::set_prototype_method;
use super::set_static_method;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_number_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    set_prototype_method(proto, "valueOf", |this, _args| number_value_of(this));
    set_prototype_method(proto, "toString", |this, _args| {
        number_to_string_proto(this)
    });
    set_prototype_method(proto, "toFixed", |this, args| number_to_fixed(this, args));
    set_prototype_method(proto, "toExponential", |this, args| {
        number_to_exponential(this, args)
    });
    set_prototype_method(proto, "toPrecision", |this, args| {
        number_to_precision(this, args)
    });
}

fn number_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(obj.to_number()))
}

fn number_to_string_proto(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&number_to_string(obj.to_number())))
}

// ─────────────────────────────────────────────────────────
// Number constructor
// ─────────────────────────────────────────────────────────

pub fn number_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        Ok(Value::Number(0.0))
    } else {
        Ok(Value::Number(args[0].to_number()))
    }
}

// ─────────────────────────────────────────────────────────
// Static methods
// ─────────────────────────────────────────────────────────

pub fn register_number_statics(number_fn: &Value, _proto: &Rc<RefCell<dyn JSObject>>) {
    set_static_method(number_fn, "isNaN", |args| number_is_nan(args));
    set_static_method(number_fn, "isFinite", |args| number_is_finite(args));
    set_static_method(number_fn, "isInteger", |args| number_is_integer(args));
}

pub fn number_is_nan(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::Bool(false));
    }
    match args[0] {
        Value::Number(n) => Ok(Value::Bool(n.is_nan())),
        _ => Ok(Value::Bool(false)),
    }
}

pub fn number_is_finite(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::Bool(false));
    }
    match args[0] {
        Value::Number(n) => Ok(Value::Bool(n.is_finite())),
        _ => Ok(Value::Bool(false)),
    }
}

pub fn number_is_integer(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::Bool(false));
    }
    match args[0] {
        Value::Number(n) => {
            if !n.is_finite() || n.is_nan() {
                return Ok(Value::Bool(false));
            }
            Ok(Value::Bool(n.fract() == 0.0))
        }
        _ => Ok(Value::Bool(false)),
    }
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

pub fn number_to_fixed(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = obj.to_number();
    if n.is_nan() {
        return Ok(Value::string("NaN"));
    }
    if n.is_infinite() {
        return if n.is_sign_positive() {
            Ok(Value::string("Infinity"))
        } else {
            Ok(Value::string("-Infinity"))
        };
    }
    let digits = if args.is_empty() {
        0
    } else {
        args[0].to_number() as usize
    };
    let digits = digits.min(100);
    Ok(Value::string(&format!("{n:.digits$}")))
}

pub fn number_to_exponential(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = obj.to_number();
    if n.is_nan() {
        return Ok(Value::string("NaN"));
    }
    if n.is_infinite() {
        return if n.is_sign_positive() {
            Ok(Value::string("Infinity"))
        } else {
            Ok(Value::string("-Infinity"))
        };
    }
    if args.is_empty() {
        Ok(Value::string(&format!("{n:e}")))
    } else {
        let digits = args[0].to_number() as usize;
        let digits = digits.min(100);
        Ok(Value::string(&format!("{n:.digits$e}")))
    }
}

pub fn number_to_precision(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = obj.to_number();
    if n.is_nan() {
        return Ok(Value::string("NaN"));
    }
    if n.is_infinite() {
        return if n.is_sign_positive() {
            Ok(Value::string("Infinity"))
        } else {
            Ok(Value::string("-Infinity"))
        };
    }
    if args.is_empty() {
        return Ok(Value::string(&number_to_string(n)));
    }
    let prec = args[0].to_number() as usize;
    let prec = prec.min(100).max(1);
    Ok(Value::string(&format!("{n:.prec$}")))
}
