use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{NativeFunctionObject, OrdinaryObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;

/// Build the `Math` namespace object.
///
/// `Math` is a plain object (not a constructor), so it is deliberately *not*
/// backed by a `NativeFunctionObject`. Its methods carry qualified names
/// (`"Math.floor"`) which the VM resolves through `call_static_method`.
pub fn create_math_object() -> Value {
    let mut math = OrdinaryObject::new();

    let methods: &[&str] = &[
        "abs", "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "cbrt", "ceil", "clz32",
        "cos", "cosh", "exp", "expm1", "floor", "fround", "hypot", "imul", "log", "log10", "log1p",
        "log2", "max", "min", "pow", "random", "round", "sign", "sin", "sinh", "sqrt", "tan",
        "tanh", "trunc",
    ];
    // Built-in methods are writable, non-enumerable and configurable (ES 17).
    for name in methods {
        let _ = math.define_property(
            PropertyKey::from_str(name),
            super::method_descriptor(Value::Object(Rc::new(RefCell::new(
                NativeFunctionObject::new(&format!("Math.{name}")),
            )))),
        );
    }

    let constants: [(&str, f64); 8] = [
        ("E", std::f64::consts::E),
        ("LN10", std::f64::consts::LN_10),
        ("LN2", std::f64::consts::LN_2),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("PI", std::f64::consts::PI),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", std::f64::consts::SQRT_2),
    ];
    // `Math` constants are read-only, non-enumerable, non-configurable.
    for (name, value) in constants {
        let _ = math.define_property(
            PropertyKey::from_str(name),
            super::constant_descriptor(Value::Number(value)),
        );
    }

    Value::Object(Rc::new(RefCell::new(math)))
}

/// Dispatch `Math.<name>(...)`.
pub fn call_math_method(name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    let arg = |i: usize| args.get(i).map(|v| v.to_number()).unwrap_or(f64::NAN);
    let one = |f: fn(f64) -> f64| Ok(Value::Number(f(arg(0))));

    match name {
        "abs" => one(f64::abs),
        "acos" => one(f64::acos),
        "acosh" => one(f64::acosh),
        "asin" => one(f64::asin),
        "asinh" => one(f64::asinh),
        "atan" => one(f64::atan),
        "atanh" => one(f64::atanh),
        "cbrt" => one(f64::cbrt),
        "ceil" => one(f64::ceil),
        "cos" => one(f64::cos),
        "cosh" => one(f64::cosh),
        "exp" => one(f64::exp),
        "expm1" => one(f64::exp_m1),
        "floor" => one(f64::floor),
        "log" => one(f64::ln),
        "log10" => one(f64::log10),
        "log1p" => one(f64::ln_1p),
        "log2" => one(f64::log2),
        "round" => Ok(Value::Number(js_round(arg(0)))),
        "sign" => Ok(Value::Number(if arg(0).is_nan() {
            f64::NAN
        } else if arg(0) == 0.0 {
            arg(0)
        } else {
            arg(0).signum()
        })),
        "sin" => one(f64::sin),
        "sinh" => one(f64::sinh),
        "sqrt" => one(f64::sqrt),
        "tan" => one(f64::tan),
        "tanh" => one(f64::tanh),
        "trunc" => one(f64::trunc),
        "fround" => Ok(Value::Number(arg(0) as f32 as f64)),
        "clz32" => {
            let n = crate::vm::to_uint32(arg(0));
            Ok(Value::Number(n.leading_zeros() as f64))
        }
        "imul" => {
            let a = crate::vm::to_int32(arg(0));
            let b = crate::vm::to_int32(arg(1));
            Ok(Value::Number(a.wrapping_mul(b) as f64))
        }
        "atan2" => Ok(Value::Number(arg(0).atan2(arg(1)))),
        "pow" => Ok(Value::Number(js_pow(arg(0), arg(1)))),
        "max" => {
            let mut best = f64::NEG_INFINITY;
            for v in args {
                let n = v.to_number();
                if n.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                // `-0` loses to `+0`; `NaN` already handled above.
                if n > best || (n == 0.0 && best == 0.0 && n.is_sign_positive()) {
                    best = n;
                }
            }
            Ok(Value::Number(best))
        }
        "min" => {
            let mut best = f64::INFINITY;
            for v in args {
                let n = v.to_number();
                if n.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                if n < best || (n == 0.0 && best == 0.0 && n.is_sign_negative()) {
                    best = n;
                }
            }
            Ok(Value::Number(best))
        }
        "hypot" => {
            let mut sum = 0.0f64;
            for v in args {
                let n = v.to_number();
                if n.is_infinite() {
                    return Ok(Value::Number(f64::INFINITY));
                }
                sum += n * n;
            }
            Ok(Value::Number(sum.sqrt()))
        }
        "random" => Ok(Value::Number(pseudo_random())),
        _ => Err(RuntimeError::TypeError(format!(
            "Math.{name} is not implemented"
        ))),
    }
}

/// ES `Math.round`: halves round toward `+∞` (and `-0.5` → `-0`).
fn js_round(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() || x == 0.0 {
        return x;
    }
    let rounded = (x + 0.5).floor();
    if rounded == 0.0 && x < 0.0 {
        -0.0
    } else {
        rounded
    }
}

/// ES `Math.pow` differs from Rust for `|base| == 1` with an infinite exponent.
fn js_pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() {
        return f64::NAN;
    }
    if exponent == 0.0 {
        return 1.0;
    }
    if base.abs() == 1.0 && exponent.is_infinite() {
        return f64::NAN;
    }
    base.powf(exponent)
}

/// Deterministic, dependency-free PRNG (xorshift64*).
///
/// `Math.random` must not be relied upon for statistics by the conformance
/// suite, but it should at least be cheap, in [0, 1), and reproducible.
fn pseudo_random() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0x2545_F491_4F6C_DD1D) };
    }
    STATE.with(|state| {
        let mut x = state.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        state.set(x);
        let bits = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // 53 significand bits → [0, 1).
        (bits >> 11) as f64 / (1u64 << 53) as f64
    })
}
