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
    set_prototype_method(proto, "at", |this, args| array_at(this, args));
    set_prototype_method(proto, "copyWithin", |this, args| {
        array_copy_within(this, args)
    });

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

    // `entries` / `keys` / `values` return iterators, which only the VM can
    // build: they are dispatched by `call_native_by_name` through the iterator
    // registry.
    for name in ["entries", "keys", "values"] {
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
// Generic (array-like) helpers
// ─────────────────────────────────────────────────────────

/// `ToObject(this)` for the generic Array.prototype paths.
fn to_object(obj: &Value) -> Result<Rc<RefCell<dyn JSObject>>, RuntimeError> {
    match super::to_object(obj) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(RuntimeError::TypeError(
            "Array.prototype method called on a non-object".to_string(),
        )),
        Err(err) => Err(err),
    }
}

/// `ToLength(Get(O, \"length\"))` — saturating, so a huge `length` cannot make
/// the caller allocate or loop forever.
fn generic_length(obj: &Value) -> u64 {
    let raw = match obj {
        Value::Object(o) => o
            .borrow()
            .property_get(&PropertyKey::from_str("length"))
            .map(|d| d.value.to_number()),
        _ => None,
    };
    let n = raw.unwrap_or(f64::NAN);
    if n.is_nan() || n <= 0.0 {
        0
    } else if n.is_infinite() {
        u64::MAX
    } else {
        n.trunc().min((1u64 << 53) as f64) as u64
    }
}

fn generic_get(obj: &Value, index: u64) -> Value {
    match obj {
        Value::Object(o) => o
            .borrow()
            .property_get(&PropertyKey::from_str(&index.to_string()))
            .map(|d| d.value)
            .unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    }
}

fn generic_set(obj: &Value, index: u64, value: Value) -> Result<(), RuntimeError> {
    match obj {
        Value::Object(o) => o
            .borrow_mut()
            .property_set(PropertyKey::from_str(&index.to_string()), value)
            .map(|_| ())
            .map_err(RuntimeError::TypeError),
        _ => Ok(()),
    }
}

/// `ToIntegerOrInfinity(arg)` resolved against a length: negatives count from
/// the end and the result is clamped into `[0, len]` (ES 23.1.3.3 steps 5-9).
fn relative_index(arg: Option<&Value>, len: u64) -> u64 {
    let n = super::to_integer_or_infinity(arg);
    let len_i = len.min(i64::MAX as u64) as i64;
    let index = if n < 0 { n.saturating_add(len_i) } else { n };
    index.clamp(0, len_i) as u64
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

/// `Array.prototype.at(index)` — code-unit free, negative indices count from
/// the end (ES 23.1.3.1).
pub fn array_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        if let Some(arr) = obj_ref.borrow().as_any().downcast_ref::<ArrayObject>() {
            let len = arr.len() as i64;
            let n = super::to_integer_or_infinity(args.first());
            let index = if n < 0 { n + len } else { n };
            if index < 0 || index >= len {
                return Ok(Value::Undefined);
            }
            return Ok(arr.get(index as usize).cloned().unwrap_or(Value::Undefined));
        }
    }
    let object = to_object(obj)?;
    let len = generic_length(obj);
    let n = super::to_integer_or_infinity(args.first());
    let len_i = len.min(i64::MAX as u64) as i64;
    let index = if n < 0 { n.saturating_add(len_i) } else { n };
    if index < 0 || index >= len_i {
        return Ok(Value::Undefined);
    }
    let _ = object;
    Ok(generic_get(obj, index as u64))
}

/// `Array.prototype.copyWithin(target, start, end)`.
///
/// The range is read into a buffer first, which makes overlapping source and
/// destination ranges behave as the spec requires (the algorithm is defined in
/// terms of a snapshot of the values, not of the live elements).
pub fn array_copy_within(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let len = if let Value::Object(obj_ref) = obj {
        if let Some(arr) = obj_ref.borrow().as_any().downcast_ref::<ArrayObject>() {
            arr.len() as u64
        } else {
            generic_length(obj)
        }
    } else {
        generic_length(obj)
    };

    let to = relative_index(args.first(), len);
    let from = relative_index(args.get(1), len);
    let end = match args.get(2) {
        Some(v) if !v.is_undefined() => relative_index(Some(v), len),
        _ => len,
    };
    // ToLength(2**53) must not be looped over.
    let count = end.saturating_sub(from).min(len.saturating_sub(to));

    let mut buffer: Vec<Value> = Vec::with_capacity(count.min(1 << 20) as usize);
    for k in 0..count {
        buffer.push(generic_get(obj, from + k));
    }
    for (k, value) in buffer.into_iter().enumerate() {
        generic_set(obj, to + k as u64, value)?;
    }
    Ok(obj.clone())
}

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
