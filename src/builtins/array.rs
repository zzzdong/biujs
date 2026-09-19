use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{ArrayObject, JSObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_array_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::{mark_prototype_method, set_prototype_method};

    set_prototype_method(proto, "push", |this, args| array_push(this, args));
    set_prototype_method(proto, "pop", |this, _args| array_pop(this));
    set_prototype_method(proto, "shift", |this, _args| array_shift(this));
    set_prototype_method(proto, "unshift", |this, args| array_unshift(this, args));
    set_prototype_method(proto, "indexOf", |this, args| array_index_of(this, args));
    set_prototype_method(proto, "includes", |this, args| array_includes(this, args));
    set_prototype_method(proto, "join", |this, args| array_join(this, args));
    set_prototype_method(proto, "slice", |this, args| array_slice(this, args));
    set_prototype_method(proto, "concat", |this, args| array_concat(this, args));
    set_prototype_method(proto, "splice", |this, args| array_splice(this, args));
    set_prototype_method(proto, "reverse", |this, _args| array_reverse(this));
    set_prototype_method(proto, "lastIndexOf", |this, args| array_last_index_of(this, args));
    set_prototype_method(proto, "fill", |this, args| array_fill(this, args));
    set_prototype_method(proto, "toString", |this, _args| array_to_string(this));

    // Callback-driven methods: the VM runs the algorithm so it can call the
    // user-supplied function for each element.
    for name in [
        "map",
        "filter",
        "forEach",
        "some",
        "every",
        "reduce",
        "reduceRight",
        "find",
        "findIndex",
        "sort",
    ] {
        mark_prototype_method(proto, name);
    }
}

/// `Array.from(arrayLike)` — copies array-like values / iterables we support.
///
/// A `mapFn` would have to call back into JavaScript, which the builtin layer
/// cannot do; such calls are handled by the VM before reaching here.
pub fn array_from(args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(source) = args.first() else {
        return Err(RuntimeError::TypeError(
            "Array.from requires an array-like object".to_string(),
        ));
    };
    let mut out: Vec<Value> = Vec::new();
    match source {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            if let Some(arr) = borrowed.as_any().downcast_ref::<ArrayObject>() {
                for i in 0..arr.len() {
                    out.push(arr.get(i).cloned().unwrap_or(Value::Undefined));
                }
            } else {
                // Generic array-like: read `length` and then each index.
                let len = borrowed
                    .property_get(&PropertyKey::from_str("length"))
                    .map(|d| d.value.to_number())
                    .unwrap_or(0.0);
                let len = len.max(0.0).min((1u32 << 20) as f64) as usize;
                for i in 0..len {
                    let key = PropertyKey::from_str(&i.to_string());
                    out.push(
                        borrowed
                            .property_get(&key)
                            .map(|d| d.value)
                            .unwrap_or(Value::Undefined),
                    );
                }
            }
        }
        Value::String(s) => {
            out.extend(s.chars().map(|c| Value::string(&c.to_string())));
        }
        _ => {
            return Err(RuntimeError::TypeError(format!(
                "Array.from: {} is not iterable",
                source.type_of()
            )));
        }
    }
    Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
        out,
    )))))
}

/// Register `Array` constructor statics.
pub fn register_array_statics(array_fn: &Value) {
    use super::set_static_method;

    set_static_method(array_fn, "isArray", |args| {
        Ok(Value::Bool(matches!(
            args.first(),
            Some(Value::Object(o)) if o.borrow().kind() == crate::vm::ObjectKind::Array
        )))
    });
    set_static_method(array_fn, "of", |args| array_constructor(args));
    set_static_method(array_fn, "from", |args| array_from(args));
}

/// `Array.prototype.reverse()`
pub fn array_reverse(obj: &Value) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let mut arr = obj_ref.borrow_mut();
        if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
            array_obj.reverse();
            return Ok(obj.clone());
        }
    }
    Err(RuntimeError::TypeError("not an array".to_string()))
}

/// `Array.prototype.lastIndexOf(searchElement[, fromIndex])`
pub fn array_last_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let arr = obj_ref.borrow();
        if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
            let needle = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_obj.len();
            if len == 0 {
                return Ok(Value::Number(-1.0));
            }
            // A negative `fromIndex` counts back from the end (undefined = end).
            let from = match args.get(1) {
                Some(v) => {
                    let n = v.to_number();
                    if n.is_nan() {
                        0
                    } else {
                        n as i64
                    }
                }
                None => len as i64 - 1,
            };
            let from = if from < 0 {
                (len as i64 + from).max(0)
            } else {
                from.min(len as i64 - 1)
            };
            for i in (0..=from).rev() {
                if array_obj.get(i as usize).is_some_and(|v| v.strict_eq(&needle)) {
                    return Ok(Value::Number(i as f64));
                }
            }
            return Ok(Value::Number(-1.0));
        }
    }
    Err(RuntimeError::TypeError("not an array".to_string()))
}

/// `Array.prototype.fill(value[, start[, end]])`
pub fn array_fill(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let mut arr = obj_ref.borrow_mut();
        if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_obj.len();
            let resolve = |arg: Option<&Value>, default: usize| -> usize {
                match arg {
                    Some(v) => {
                        let n = v.to_number();
                        if n < 0.0 {
                            ((len as f64) + n).max(0.0) as usize
                        } else {
                            (n as usize).min(len)
                        }
                    }
                    None => default,
                }
            };
            let start = resolve(args.get(1), 0);
            let end = resolve(args.get(2), len);
            for i in start..end {
                if let Some(slot) = array_obj.get_mut(i) {
                    *slot = value.clone();
                }
            }
            return Ok(obj.clone());
        }
    }
    Err(RuntimeError::TypeError("not an array".to_string()))
}

/// `Array.prototype.toString()`
pub fn array_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let arr = obj_ref.borrow();
        if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
            return Ok(Value::string(&array_obj.join_elements()));
        }
    }
    Ok(Value::string("[object Object]"))
}

// ─────────────────────────────────────────────────────────
// Array constructor
// ─────────────────────────────────────────────────────────

/// Largest length `Array(len)` will actually materialize. Anything above this
/// raises `RangeError` instead of trying to allocate gigabytes up front.
const MAX_MATERIALIZED_LEN: usize = 1 << 20;

pub fn array_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() == 1 {
        if let Value::Number(n) = args[0] {
            // ES `Array(len)`: `intLen = ToUint32(len)` and `len` must equal it.
            // This rejects NaN, ±Infinity, negatives, non-integers and anything
            // >= 2**32 (previously `new Array(2**32)` tried to allocate 96 GB).
            let int_len = to_uint32(n);
            if n != int_len as f64 {
                return Err(RuntimeError::RangeError("Invalid array length".to_string()));
            }
            let len = int_len as usize;
            if len > MAX_MATERIALIZED_LEN {
                return Err(RuntimeError::RangeError(
                    "Invalid array length".to_string(),
                ));
            }
            let mut arr = ArrayObject::with_capacity(len);
            for _ in 0..len {
                arr.push(Value::Undefined);
            }
            return Ok(Value::Object(Rc::new(RefCell::new(arr))));
        }
    }
    let mut arr = ArrayObject::new();
    for arg in args {
        arr.push(arg.clone());
    }
    Ok(Value::Object(Rc::new(RefCell::new(arr))))
}

/// Validate an array `length` assignment, returning the materialized length or
/// `Err` when the spec requires a `RangeError`.
pub fn validate_array_length(n: f64) -> Result<usize, RuntimeError> {
    let int_len = to_uint32(n) as usize;
    if n != int_len as f64 || int_len > MAX_MATERIALIZED_LEN {
        return Err(RuntimeError::RangeError(
            "Invalid array length".to_string(),
        ));
    }
    Ok(int_len)
}

/// ES `ToUint32` (modulo 2**32, saturating NaN to 0).
fn to_uint32(n: f64) -> u32 {
    if n.is_nan() || n.is_infinite() || n == 0.0 {
        return 0;
    }
    let truncated = n.trunc();
    let rem = truncated % 4_294_967_296.0; // 2**32
    (rem as i64 % 4_294_967_296i64) as u32
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

pub fn array_push(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                for arg in args {
                    array_obj.push(arg.clone());
                }
                Ok(Value::Number(array_obj.len() as f64))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_pop(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                Ok(array_obj.pop())
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_shift(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                Ok(array_obj.shift())
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_unshift(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                for arg in args.iter().rev() {
                    array_obj.unshift(arg.clone());
                }
                Ok(Value::Number(array_obj.len() as f64))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                if args.is_empty() {
                    return Ok(Value::Number(-1.0));
                }
                let from = if args.len() < 2 {
                    0
                } else {
                    args[1].to_number() as usize
                };
                Ok(Value::Number(array_obj.index_of(&args[0], from) as f64))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_includes(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                if args.is_empty() {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(array_obj.includes(&args[0])))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_join(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                let sep = if args.is_empty() {
                    ","
                } else {
                    &args[0].to_js_string()
                };
                Ok(Value::string(&array_obj.join(sep)))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                let len = array_obj.len();
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
                let sliced = array_obj.slice(start, end);
                Ok(Value::Object(Rc::new(RefCell::new(sliced))))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                let mut elements: Vec<Value> = (0..array_obj.len())
                    .map(|i| array_obj.get(i).cloned().unwrap_or(Value::Undefined))
                    .collect();
                for arg in args {
                    // Flatten array arguments
                    if let Value::Object(arg_ref) = arg {
                        let borrowed = arg_ref.borrow();
                        if borrowed.kind() == crate::vm::ObjectKind::Array {
                            if let Some(other_arr) = borrowed.as_any().downcast_ref::<ArrayObject>()
                            {
                                for i in 0..other_arr.len() {
                                    elements.push(
                                        other_arr.get(i).cloned().unwrap_or(Value::Undefined),
                                    );
                                }
                                continue;
                            }
                        }
                    }
                    elements.push(arg.clone());
                }
                Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
                    elements,
                )))))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_splice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                let len = array_obj.len();
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
                let delete_count = if args.len() < 2 {
                    len - start
                } else {
                    (args[1].to_number() as usize).min(len - start)
                };
                let insert_items: Vec<Value> = if args.len() > 2 {
                    args[2..].to_vec()
                } else {
                    vec![]
                };
                let removed = array_obj.splice(start, delete_count, &insert_items);
                Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
                    removed,
                )))))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}
