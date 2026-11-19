use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::gc::{Heap, Object};
use crate::vm::object::{JSObject, NativeFunctionObject, OrdinaryObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// Build the `Math` namespace object.
///
/// `Math` is a plain object (not a constructor); each of its methods is a
/// `NativeFunctionObject` that carries **its own implementation** — the
/// dispatch name exists only so `Function.prototype.toString` and `name` can
/// report it.
pub fn create_math_object(heap: &mut Heap) -> Value {
    let mut math = OrdinaryObject::new();
    // `Math` inherits from `Object.prototype` like any other object: without
    // this it had no `hasOwnProperty` / `toString` / `valueOf` at all
    // (`Math.hasOwnProperty("PI")` was `undefined`).
    if let Some(object_proto) = super::wrapper_prototype("Object") {
        math.set_prototype(Some(object_proto));
    }

    // 每个方法一条记录：名字 + 实现。实现是 `fn` 指针（非捕获闭包），
    // 所以「是哪个方法」由**注册**决定一次，调用点不再解析字符串。
    let methods: &[(&str, crate::vm::native::HeapImpl)] = &[
        ("abs", |_h, _t, a| unary(a, f64::abs)),
        ("acos", |_h, _t, a| unary(a, f64::acos)),
        ("acosh", |_h, _t, a| unary(a, f64::acosh)),
        ("asin", |_h, _t, a| unary(a, f64::asin)),
        ("asinh", |_h, _t, a| unary(a, f64::asinh)),
        ("atan", |_h, _t, a| unary(a, f64::atan)),
        ("atanh", |_h, _t, a| unary(a, f64::atanh)),
        ("cbrt", |_h, _t, a| unary(a, f64::cbrt)),
        ("ceil", |_h, _t, a| unary(a, f64::ceil)),
        ("cos", |_h, _t, a| unary(a, f64::cos)),
        ("cosh", |_h, _t, a| unary(a, f64::cosh)),
        ("exp", |_h, _t, a| unary(a, f64::exp)),
        ("expm1", |_h, _t, a| unary(a, f64::exp_m1)),
        ("floor", |_h, _t, a| unary(a, f64::floor)),
        ("log", |_h, _t, a| unary(a, f64::ln)),
        ("log10", |_h, _t, a| unary(a, f64::log10)),
        ("log1p", |_h, _t, a| unary(a, f64::ln_1p)),
        ("log2", |_h, _t, a| unary(a, f64::log2)),
        ("sin", |_h, _t, a| unary(a, f64::sin)),
        ("sinh", |_h, _t, a| unary(a, f64::sinh)),
        ("sqrt", |_h, _t, a| unary(a, f64::sqrt)),
        ("tan", |_h, _t, a| unary(a, f64::tan)),
        ("tanh", |_h, _t, a| unary(a, f64::tanh)),
        ("trunc", |_h, _t, a| unary(a, f64::trunc)),
        ("round", |_h, _t, a| Ok(Value::Number(js_round(to_arg(a, 0))))),
        ("fround", |_h, _t, a| Ok(Value::Number(to_arg(a, 0) as f32 as f64))),
        ("sign", |_h, _t, a| Ok(Value::Number(js_sign(to_arg(a, 0))))),
        ("clz32", |_h, _t, a| {
            let n = crate::vm::to_uint32(to_arg(a, 0));
            Ok(Value::Number(n.leading_zeros() as f64))
        }),
        ("imul", |_h, _t, a| {
            let x = crate::vm::to_int32(to_arg(a, 0));
            let y = crate::vm::to_int32(to_arg(a, 1));
            Ok(Value::Number(x.wrapping_mul(y) as f64))
        }),
        ("atan2", |_h, _t, a| {
            Ok(Value::Number(to_arg(a, 0).atan2(to_arg(a, 1))))
        }),
        ("pow", |_h, _t, a| {
            Ok(Value::Number(js_pow(to_arg(a, 0), to_arg(a, 1))))
        }),
        ("max", |_h, _t, a| Ok(Value::Number(js_max(a)))),
        ("min", |_h, _t, a| Ok(Value::Number(js_min(a)))),
        ("hypot", |_h, _t, a| Ok(Value::Number(js_hypot(a)))),
        ("random", |_h, _t, _a| Ok(Value::Number(pseudo_random()))),
    ];
    // Built-in methods are writable, non-enumerable and configurable (ES 17).
    for (name, entry) in methods {
        let key = PropertyKey::intern(heap, name);
        let value = super::method_descriptor(Value::Object(heap.alloc(
            Object::NativeFunction(
                NativeFunctionObject::new(&format!("Math.{name}"))
                    .with_entry(crate::vm::native::NativeEntry::Heap(*entry)),
            ),
        )));
        let _ = math.define_property(heap, key, value);
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
        let key = PropertyKey::intern(heap, name);

        let _ = math.define_property(heap, key,
            super::constant_descriptor(Value::Number(value)),
        );
    }

    // `Math[Symbol.toStringTag] === "Math"` (ES 20.2.2.36):
    // `Object.prototype.toString.call(Math)` must be `"[object Math]"`, which is
    // what the `Array.prototype` generics assert when they are applied to it.
    let _ = math.define_property(
        heap,
        super::to_string_tag_symbol_key(),
        PropertyDescriptor {
            value: Value::string("Math"),
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );

    Value::Object(heap.alloc(Object::Ordinary(math)))
}

/// `ToNumber` of the `i`-th argument; a missing argument is `NaN` (ES 20.2.2.x).
fn to_arg(args: &[Value], i: usize) -> f64 {
    args.get(i).map(|v| v.to_number()).unwrap_or(f64::NAN)
}

/// 一元数学方法：`Math.<name>(x)` ⇒ `f(ToNumber(x))`。
fn unary(args: &[Value], f: fn(f64) -> f64) -> Result<Value, RuntimeError> {
    Ok(Value::Number(f(to_arg(args, 0))))
}

/// ES `Math.sign`: keeps `±0` and `NaN` as they are.
fn js_sign(x: f64) -> f64 {
    if x.is_nan() || x == 0.0 {
        x
    } else {
        x.signum()
    }
}

/// ES `Math.max`: `NaN` wins; `-0` loses to `+0`.
fn js_max(args: &[Value]) -> f64 {
    let mut best = f64::NEG_INFINITY;
    for v in args {
        let n = v.to_number();
        if n.is_nan() {
            return f64::NAN;
        }
        if n > best || (n == 0.0 && best == 0.0 && n.is_sign_positive()) {
            best = n;
        }
    }
    best
}

/// ES `Math.min`: `NaN` wins; `+0` loses to `-0`.
fn js_min(args: &[Value]) -> f64 {
    let mut best = f64::INFINITY;
    for v in args {
        let n = v.to_number();
        if n.is_nan() {
            return f64::NAN;
        }
        if n < best || (n == 0.0 && best == 0.0 && n.is_sign_negative()) {
            best = n;
        }
    }
    best
}

/// ES `Math.hypot`: an infinity wins over everything, including `NaN`.
fn js_hypot(args: &[Value]) -> f64 {
    let mut sum = 0.0f64;
    for v in args {
        let n = v.to_number();
        if n.is_infinite() {
            return f64::INFINITY;
        }
        sum += n * n;
    }
    sum.sqrt()
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
