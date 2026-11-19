use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::JSObject;
use crate::vm::value::Value;

use super::number_to_string;
use super::number_to_string_radix;
use crate::vm::native::{HeapImpl, NativeEntry};

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_number_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    use super::{set_prototype_method_arity_impl, set_prototype_method_impl};

    // 归属（Number 而非 String/Array）现在由函数对象身份携带，`this` 只做
    // brand check（`require_number`），分派口不再按接收者形状猜。
    let methods: &[(&str, HeapImpl)] = &[
        ("valueOf", |_h, t, _a| number_value_of(t)),
        ("toLocaleString", |_h, t, _a| number_to_string_proto(t)),
        ("toFixed", |_h, t, a| number_to_fixed(t, a)),
        ("toExponential", |_h, t, a| number_to_exponential(t, a)),
        ("toPrecision", |_h, t, a| number_to_precision(t, a)),
    ];
    for (name, entry) in methods {
        set_prototype_method_impl(proto, heap, name, NativeEntry::Heap(*entry));
    }
    // `Number.prototype.toString.length` is 1（radix 算一个形参），
    // 其他所有 `toString` 都是 0，名字推不出来，只能显式给。
    set_prototype_method_arity_impl(
        proto,
        heap,
        "toString",
        1,
        NativeEntry::Heap(|_h, t, a| number_to_string_with_radix(t, a)),
    );
}

fn number_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    // `thisNumberValue`：包装对象解包，别的接收者（`{}` / `'1'`）抛 TypeError。
    Ok(Value::Number(super::require_number(obj)?))
}

fn number_to_string_proto(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&number_to_string(super::require_number(obj)?)))
}

/// `Number.prototype.toString([radix])`.
///
/// `radix` defaults to 10; outside `[2, 36]` a `RangeError` is raised, as the
/// spec requires (`toString` on a number is the only numeric conversion that
/// honours a radix).
pub fn number_to_string_with_radix(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = super::require_number(obj)?;
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

pub fn register_number_statics(number_fn: &Value, _proto: &Gc<Object>, heap: &mut Heap) {
    use super::set_static_method_impl;
    let pure: &[(&str, HeapImpl)] = &[
        ("isNaN", |_h, _t, a| number_is_nan(a)),
        ("isFinite", |_h, _t, a| number_is_finite(a)),
        ("isInteger", |_h, _t, a| number_is_integer(a)),
        ("isSafeInteger", |_h, _t, a| {
            // ES 20.1.2.5：非 Number 值直接 false，不做 ToNumber。
            Ok(Value::Bool(match a.first() {
                Some(Value::Number(n)) => {
                    n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0
                }
                _ => false,
            }))
        }),
        ("parseInt", |_h, _t, a| super::global_parse_int(a)),
        ("parseFloat", |_h, _t, a| super::global_parse_float(a)),
    ];
    for (name, f) in pure {
        set_static_method_impl(number_fn, name, heap, NativeEntry::Heap(*f));
    }

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
        // Numeric constants are read-only, non-enumerable, non-configurable.
        for (name, value) in constants {
            let desc = super::constant_descriptor(Value::Number(value));
            let key = crate::vm::PropertyKey::intern(heap, name);
            let _ = obj_ref.borrow_mut().define_property(heap, key, desc);
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
    let n = super::require_number(obj)?;
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
        super::to_integer_or_infinity_throwing(args.first())? as i64
    };
    if !(0..=100).contains(&digits) {
        return Err(RuntimeError::RangeError(
            "toFixed: fractionDigits out of range".to_string(),
        ));
    }
    // ES 20.1.3.3 第 5 步：|x| >= 1e21 时直接返回 ToString(x)，不按定点写。
    if n.abs() >= 1e21 {
        return Ok(Value::string(&number_to_string(n)));
    }
    let digits = digits as usize;
    Ok(Value::string(&format!("{n:.digits$}")))
}

pub fn number_to_exponential(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = super::require_number(obj)?;
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
    // Rust 的 `{:e}` 不写正号，JS 必须有 `e+2` 这样的形式。
    let with_sign = |s: String| -> String {
        if let Some((mantissa, exp)) = s.split_once('e') {
            if exp.starts_with('-') || exp.starts_with('+') {
                s
            } else {
                format!("{mantissa}e+{exp}")
            }
        } else {
            s
        }
    };
    if args.is_empty() {
        return Ok(Value::string(&with_sign(format!("{n:e}"))));
    }
    let digits = super::to_integer_or_infinity_throwing(args.first())? as i64;
    if !(0..=100).contains(&digits) {
        return Err(RuntimeError::RangeError(
            "toExponential: fractionDigits out of range".to_string(),
        ));
    }
    let digits = digits as usize;
    Ok(Value::string(&with_sign(format!("{n:.digits$e}"))))
}

pub fn number_to_precision(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = super::require_number(obj)?;
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
    let prec = super::to_integer_or_infinity_throwing(args.first())? as i64;
    if !(1..=100).contains(&prec) {
        return Err(RuntimeError::RangeError(
            "toPrecision: precision out of range".to_string(),
        ));
    }
    let prec = prec as usize;
    if n == 0.0 {
        // `0.toPrecision(3)` 是 "0.00"：精度决定小数位数（符号单独处理）。
        let digits = prec - 1;
        return Ok(Value::string(&format!("{:.*}", digits, n)));
    }
    let exponent = n.abs().log10().floor() as i64;
    if exponent < -6 || exponent >= prec as i64 {
        let mut s = format!("{:.*e}", prec - 1, n);
        if let Some((mantissa, exp)) = s.split_once('e') {
            if !exp.starts_with('-') && !exp.starts_with('+') {
                s = format!("{mantissa}e+{exp}");
            }
        }
        Ok(Value::string(&s))
    } else {
        let digits = (prec as i64 - 1 - exponent).max(0) as usize;
        Ok(Value::string(&format!("{:.*}", digits, n)))
    }
}
