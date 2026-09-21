use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::JSObject;
use crate::vm::value::Value;

use super::number_to_string;
use super::number_to_string_radix;
use super::set_prototype_method;
use super::set_static_method;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_number_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    set_prototype_method(proto, "valueOf", |this, _args| number_value_of(this));
    set_prototype_method(proto, "toString", |this, args| {
        number_to_string_with_radix(this, args)
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
    // Primitive wrappers (`Object(1.1)`) unwrap via ToPrimitive.
    Ok(Value::Number(obj.to_primitive("number").to_number()))
}

fn number_to_string_proto(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&number_to_string(obj.to_number())))
}

/// `Number.prototype.toString([radix])`.
///
/// `radix` defaults to 10; outside `[2, 36]` a `RangeError` is raised, as the
/// spec requires (`toString` on a number is the only numeric conversion that
/// honours a radix).
pub fn number_to_string_with_radix(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = obj.to_number();
    let radix = match args.first() {
        None | Some(Value::Undefined) => 10,
        Some(v) => {
            let r = v.to_number();
            if r != 10.0 && (r < 2.0 || r > 36.0 || r.is_nan()) {
                return Err(RuntimeError::RangeError(
                    "toString() radix argument must be between 2 and 36".to_string(),
                ));
            }
            r as u32
        }
    };
    Ok(Value::string(&number_to_string_radix(n, radix)))
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
    set_static_method(number_fn, "isSafeInteger", |args| {
        Ok(Value::Bool(
            args.first()
                .map(|v| {
                    let n = v.to_number();
                    n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0
                })
                .unwrap_or(false),
        ))
    });
    set_static_method(number_fn, "parseInt", |args| {
        super::global_parse_int(args)
    });
    set_static_method(number_fn, "parseFloat", |args| {
        super::global_parse_float(args)
    });

    // Numeric constants live as own properties of the constructor.
    if let Value::Object(obj_ref) = number_fn {
        let constants: [(&str, f64); 8] = [
            ("MAX_SAFE_INTEGER", 9007199254740991.0),
            ("MIN_SAFE_INTEGER", -9007199254740991.0),
            ("MAX_VALUE", f64::MAX),
            ("MIN_VALUE", 5e-324),
            ("EPSILON", f64::EPSILON),
            ("POSITIVE_INFINITY", f64::INFINITY),
            ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
            ("NaN", f64::NAN),
        ];
        for (name, value) in constants {
            let _ = obj_ref.borrow_mut().property_set(
                crate::vm::PropertyKey::from_str(name),
                Value::Number(value),
            );
        }
    }
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
