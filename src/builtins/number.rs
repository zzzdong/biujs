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
    ];
    for (name, entry) in methods {
        set_prototype_method_impl(proto, heap, name, NativeEntry::Heap(*entry));
    }
    use crate::vm::native::VmSlot;
    for (name, slot) in [
        ("toFixed", VmSlot::NumberToFixed),
        ("toExponential", VmSlot::NumberToExponential),
        ("toPrecision", VmSlot::NumberToPrecision),
    ] {
        set_prototype_method_impl(proto, heap, name, slot.entry());
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

fn half_up(value: f64, digits: i64) -> f64 {
    let factor = 10f64.powi(digits as i32);
    (value.abs() * factor + 0.5).floor() / factor
}

/// `Number.prototype.toFixed`，参数已由 VM 做过可观察的 ToIntegerOrInfinity。
pub fn number_to_fixed_ready(n: f64, digits: Option<i64>) -> Result<Value, RuntimeError> {
    let d = digits.unwrap_or(0);
    if !(0..=100).contains(&d) {
        return Err(RuntimeError::RangeError(
            "toFixed: fractionDigits out of range".to_string(),
        ));
    }
    if n.is_nan() {
        return Ok(Value::string("NaN"));
    }
    if n.is_infinite() {
        return Ok(Value::string(if n > 0.0 { "Infinity" } else { "-Infinity" }));
    }
    if n.abs() >= 1e21 {
        return Ok(Value::string(&number_to_string(n)));
    }
    let rounded = half_up(n, d);
    let text = format!("{:.*}", d as usize, rounded);
    Ok(Value::string(&if n < 0.0 && rounded != 0.0 {
        format!("-{text}")
    } else {
        text
    }))
}

/// `Number.prototype.toExponential`；`None` 表示参数省略/undefined。
pub fn number_to_exponential_ready(n: f64, digits: Option<i64>) -> Result<Value, RuntimeError> {
    // NaN / Infinity 在范围检查之前返回（参数仍已由 VM 转换过）。
    if n.is_nan() {
        return Ok(Value::string("NaN"));
    }
    if n.is_infinite() {
        return Ok(Value::string(if n > 0.0 { "Infinity" } else { "-Infinity" }));
    }
    if let Some(d) = digits {
        if !(0..=100).contains(&d) {
            return Err(RuntimeError::RangeError(
                "toExponential: fractionDigits out of range".to_string(),
            ));
        }
    }
    let Some(d) = digits else {
        // Rust 的 `{:e}` 给最短十进制数字，对应 ES 的 undefined 分支。
        let s = format!("{n:e}");
        return Ok(Value::string(&if let Some((m, e)) = s.split_once('e') {
            let exp = e.parse::<i64>().unwrap_or(0);
            format!("{m}e{}{}", if exp < 0 { "-" } else { "+" }, exp.abs())
        } else {
            s
        }));
    };
    if n == 0.0 {
        return Ok(Value::string(&if d == 0 {
            "0e+0".to_string()
        } else {
            format!("0.{}e+0", "0".repeat(d as usize))
        }));
    }
    let mut exponent = n.abs().log10().floor() as i64;
    let mut mantissa = half_up(n / 10f64.powi(exponent as i32), d);
    if mantissa >= 10.0 {
        mantissa /= 10.0;
        exponent += 1;
    }
    let sign = if n < 0.0 { "-" } else { "" };
    Ok(Value::string(&format!(
        "{sign}{:.*}e{}{}",
        d as usize,
        mantissa,
        if exponent < 0 { "-" } else { "+" },
        exponent.abs()
    )))
}

/// `Number.prototype.toPrecision`；`None` 表示省略/undefined。
pub fn number_to_precision_ready(n: f64, prec: Option<i64>) -> Result<Value, RuntimeError> {
    if n.is_nan() {
        return Ok(Value::string("NaN"));
    }
    if n.is_infinite() {
        return Ok(Value::string(if n > 0.0 { "Infinity" } else { "-Infinity" }));
    }
    if let Some(p) = prec {
        if !(1..=100).contains(&p) {
            return Err(RuntimeError::RangeError(
                "toPrecision: precision out of range".to_string(),
            ));
        }
    }
    let Some(p) = prec else {
        return Ok(Value::string(&number_to_string(n)));
    };
    if n == 0.0 {
        return Ok(Value::string(&format!("{:.*}", (p - 1) as usize, 0.0)));
    }
    let exponent = n.abs().log10().floor() as i64;
    let sign = if n < 0.0 { "-" } else { "" };
    if exponent < -6 || exponent >= p {
        let v = number_to_exponential_ready(n, Some(p - 1))?;
        return Ok(v);
    }
    let digits = (p - 1 - exponent).max(0);
    let rounded = half_up(n, digits);
    Ok(Value::string(&format!("{sign}{:.*}", digits as usize, rounded)))
}


/// 纯内建兜底（无 VM，参数转换不跑用户代码；正常入口走 VmSlot）。
pub fn number_to_fixed(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = super::require_number(obj)?;
    let d = args.first().map(|v| v.to_number() as i64).unwrap_or(0);
    number_to_fixed_ready(n, Some(d))
}

pub fn number_to_exponential(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = super::require_number(obj)?;
    let d = args.first().filter(|v| !v.is_undefined()).map(|v| v.to_number() as i64);
    number_to_exponential_ready(n, d)
}

pub fn number_to_precision(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let n = super::require_number(obj)?;
    let p = args.first().filter(|v| !v.is_undefined()).map(|v| v.to_number() as i64);
    number_to_precision_ready(n, p)
}
