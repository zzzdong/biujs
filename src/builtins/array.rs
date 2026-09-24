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
    register_species_accessor(array_fn);
}

/// Native name of the `get Array[Symbol.species]` accessor.
pub const ARRAY_SPECIES_NATIVE: &str = "__array_species__";

/// ES 22.1.2.5: `Array[Symbol.species]` is an accessor whose getter returns
/// its receiver (`get [Symbol.species]() { return this; }`).
///
/// It is what makes `ArraySpeciesCreate` (VM side) able to tell "no override"
/// from "inherited default": without it every subclass instance would look like
/// it had no species and would silently get an `Array` back.
fn register_species_accessor(array_fn: &Value) {
    let Value::Object(obj) = array_fn else {
        return;
    };
    let getter = Value::Object(Rc::new(RefCell::new(
        crate::vm::object::NativeFunctionObject::new(ARRAY_SPECIES_NATIVE),
    )));
    let desc = crate::vm::property::PropertyDescriptor {
        value: Value::Undefined,
        writable: false,
        enumerable: false,
        configurable: true,
        getter: Some(getter),
        setter: None,
    };
    let _ = obj
        .borrow_mut()
        .define_property(super::species_symbol_key(), desc);
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
//
// The spec defines every Array.prototype method in terms of `length`,
// `HasProperty` and index `[[Get]]`/`[[Set]]`/`[[Delete]]`, so each method below
// has a fast path for real arrays and a generic fallback. Two properties of the
// fallback matter for the conformance suite:
//
// * It must not walk `length` indices one by one when the receiver is sparse
//   with a `length` near 2^53 (`slice`/`splice`/`reverse` all have such cases),
//   hence `own_index_keys` / `generic_window_move`.
// * It cannot run accessors: `[[Get]]` here reads the property table directly,
//   so an index whose value is defined by a getter is seen as `undefined`.

/// Largest valid array `length` (2^53 - 1).
const MAX_ARRAY_LENGTH: u64 = (1u64 << 53) - 1;

/// Upper bound on how many elements a *generic* (array-like) path will
/// materialize or touch one by one.
///
/// A receiver may legally claim `length = 2^53 - 1` — test262 exercises several
/// such cases — but an operation whose result is built element by element
/// cannot honour that: this engine's arrays are `Vec`-backed and top out at
/// `MAX_MATERIALIZED_LEN` entries. Those paths therefore raise `RangeError`
/// instead of attempting a gigantic allocation (the sparse-aware paths —
/// `shift`/`unshift`/`reverse`/`splice`'s window move — are unaffected, they
/// only touch indices that exist).
const MAX_GENERIC_ELEMENTS: u64 = 1 << 22;

/// Upper bound for the string `join` is allowed to build (see `array_join`).
const MAX_JOIN_LENGTH: usize = 1 << 20;

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

/// `ToLength(Get(O, "length"))` — saturating, so a huge `length` cannot make
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
        MAX_ARRAY_LENGTH
    } else {
        n.trunc().min(MAX_ARRAY_LENGTH as f64) as u64
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

/// `HasProperty(O, key)` — walks the prototype chain, as the spec requires.
fn generic_has(obj: &Value, index: u64) -> bool {
    match obj {
        Value::Object(o) => crate::vm::prototype::internal_has_property(
            Rc::clone(o),
            &PropertyKey::from_str(&index.to_string()),
        )
        .unwrap_or(false),
        _ => false,
    }
}

/// `[[Delete]](O, key)`.
fn generic_delete(obj: &Value, index: u64) -> Result<bool, RuntimeError> {
    match obj {
        Value::Object(o) => crate::vm::prototype::internal_delete(
            Rc::clone(o),
            &PropertyKey::from_str(&index.to_string()),
        )
        .map_err(RuntimeError::TypeError),
        _ => Ok(true),
    }
}

/// `Set(O, "length", len, true)`.
fn generic_set_length(obj: &Value, len: u64) -> Result<(), RuntimeError> {
    match obj {
        Value::Object(o) => o
            .borrow_mut()
            .property_set(PropertyKey::from_str("length"), Value::Number(len as f64))
            .map(|_| ())
            .map_err(RuntimeError::TypeError),
        _ => Ok(()),
    }
}

/// Own index keys of `obj` below `limit`, ascending.
fn own_index_keys_below(obj: &Value, limit: u64) -> Vec<u64> {
    let Value::Object(o) = obj else {
        return Vec::new();
    };
    let keys = o.borrow().own_keys();
    let mut indices: Vec<u64> = keys
        .iter()
        .filter_map(|key| key.as_str().and_then(|s| s.parse::<u64>().ok()))
        .filter(|index| *index < limit)
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Move the window `[from, to)` by `delta` indices (`delta > 0` moves towards
/// higher indices), touching only the indices that exist.
///
/// Mirrors "for each k: if HasProperty(from + k) then Set(to + k, Get(...))
/// else Delete(to + k)" without paying O(length) on a sparse receiver: the
/// destinations that do not receive a value are deleted, the rest are written
/// from a snapshot taken before anything is modified.
fn generic_window_move(obj: &Value, from: u64, to: u64, delta: i64, len: u64) -> Result<(), RuntimeError> {
    if to <= from || delta == 0 {
        return Ok(());
    }
    let present = own_index_keys_below(obj, len);
    let shift = |k: u64| -> u64 {
        if delta > 0 {
            k + delta as u64
        } else {
            k - (-delta) as u64
        }
    };

    let mut moves: Vec<(u64, Value)> = Vec::new();
    for k in &present {
        if *k >= from && *k < to {
            let value = generic_get(obj, *k);
            moves.push((shift(*k), value));
        }
    }
    let destinations: std::collections::HashSet<u64> = moves.iter().map(|(d, _)| *d).collect();

    let (dest_from, dest_to) = (shift(from), shift(to - 1) + 1);
    for k in &present {
        if *k >= dest_from && *k < dest_to && !destinations.contains(k) {
            generic_delete(obj, *k)?;
        }
    }
    for (dest, value) in moves {
        generic_set(obj, dest, value)?;
    }
    Ok(())
}

/// Delete every *present* index in `[from, to)`.
fn generic_delete_range(obj: &Value, from: u64, to: u64, len: u64) -> Result<(), RuntimeError> {
    if to <= from {
        return Ok(());
    }
    for k in own_index_keys_below(obj, len) {
        if k >= from && k < to {
            generic_delete(obj, k)?;
        }
    }
    Ok(())
}

/// `ToIntegerOrInfinity(arg)` resolved against a length: negatives count from
/// the end and the result is clamped into `[0, len]` (ES 23.1.3.3 steps 5-9).
///
/// `ToNumber`'s abrupt completion is propagated, so a `Symbol` argument raises
/// a TypeError instead of silently reading as `0`.
fn relative_index(arg: Option<&Value>, len: u64) -> Result<u64, RuntimeError> {
    let n = super::to_integer_or_infinity_throwing(arg)?;
    let len_i = len.min(i64::MAX as u64) as i64;
    let index = if n < 0 { n.saturating_add(len_i) } else { n };
    Ok(index.clamp(0, len_i) as u64)
}

/// `ToNumber(arg)` clamped into `[0, len]` with negatives counting from the end —
/// the bound shared by `slice` and the `start` of `splice`/`indexOf`.
fn clamped_index(arg: Option<&Value>, len: usize) -> Result<usize, RuntimeError> {
    let n = arg.map_or(Ok(f64::NAN), super::to_number_throwing)?;
    if n.is_nan() {
        return Ok(0);
    }
    let n = if n.is_infinite() {
        if n.is_sign_positive() {
            len as i64
        } else {
            0
        }
    } else {
        n.trunc() as i64
    };
    Ok(if n < 0 {
        ((len as i64) + n).max(0) as usize
    } else {
        (n as usize).min(len)
    })
}

/// `IsConcatSpreadable(value)` (ES 23.1.3.1.1): an Array is spread unless
/// `Symbol.isConcatSpreadable` says otherwise; a non-Array only when that
/// property is truthy.
fn is_concat_spreadable(value: &Value) -> bool {
    let Value::Object(obj_ref) = value else {
        return false;
    };
    let is_array = obj_ref.borrow().kind() == crate::vm::ObjectKind::Array;
    let flag = crate::vm::prototype::find_descriptor(
        Rc::clone(obj_ref),
        &super::is_concat_spreadable_symbol_key(),
    )
    .ok()
    .flatten()
    .map(|(_, desc)| desc.value);
    match flag {
        Some(Value::Undefined) | None => is_array,
        Some(v) => v.to_boolean(),
    }
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

/// `Array.prototype.at(index)` — negative indices count from the end
/// (ES 23.1.3.1).
pub fn array_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        if let Some(arr) = obj_ref.borrow().as_any().downcast_ref::<ArrayObject>() {
            let len = arr.len() as i64;
            let n = super::to_integer_or_infinity_throwing(args.first())?;
            let index = if n < 0 { n + len } else { n };
            if index < 0 || index >= len {
                return Ok(Value::Undefined);
            }
            return Ok(arr.get(index as usize).cloned().unwrap_or(Value::Undefined));
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    let n = super::to_integer_or_infinity_throwing(args.first())?;
    let len_i = len.min(i64::MAX as u64) as i64;
    let index = if n < 0 { n.saturating_add(len_i) } else { n };
    if index < 0 || index >= len_i {
        return Ok(Value::Undefined);
    }
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

    let to = relative_index(args.first(), len)?;
    let from = relative_index(args.get(1), len)?;
    let end = match args.get(2) {
        Some(v) if !v.is_undefined() => relative_index(Some(v), len)?,
        _ => len,
    };
    // ToLength(2**53) must not be looped over.
    let count = end.saturating_sub(from).min(len.saturating_sub(to));
    if count > MAX_GENERIC_ELEMENTS {
        return Err(RuntimeError::RangeError(
            "Invalid array length".to_string(),
        ));
    }

    let mut buffer: Vec<Value> = Vec::with_capacity(count as usize);
    for k in 0..count {
        buffer.push(generic_get(obj, from + k));
    }
    for (k, value) in buffer.into_iter().enumerate() {
        generic_set(obj, to + k as u64, value)?;
    }
    Ok(obj.clone())
}

/// ES `Set(O, "length", …, true)` (`Throw` = true) for the real-array fast
/// paths, which mutate the dense store directly and so skip `[[Set]]`.
///
/// `push`/`pop`/`shift`/`unshift` all perform this Set — `pop` and `shift` even
/// when the array is empty — hence `Object.freeze(a); a.push()` is a TypeError
/// rather than a silent no-op.
fn set_length_throwing(array_obj: &ArrayObject) -> Result<(), RuntimeError> {
    if !array_obj.length_is_writable() {
        return Err(RuntimeError::TypeError(
            "Cannot assign to read-only property 'length'".to_string(),
        ));
    }
    Ok(())
}

/// `Array.prototype.push(...items)` (ES 23.1.3.20).
pub fn array_push(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        if let Some(array_obj) = obj_ref.borrow_mut().as_any_mut().downcast_mut::<ArrayObject>() {
            set_length_throwing(array_obj)?;
            for arg in args {
                array_obj.push(arg.clone());
            }
            return Ok(Value::Number(array_obj.len() as f64));
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    let count = args.len() as u64;
    let new_len = len.saturating_add(count);
    if new_len > MAX_ARRAY_LENGTH {
        return Err(RuntimeError::TypeError("Invalid array length".to_string()));
    }
    for (i, arg) in args.iter().enumerate() {
        generic_set(obj, len + i as u64, arg.clone())?;
    }
    generic_set_length(obj, new_len)?;
    Ok(Value::Number(new_len as f64))
}

/// `Array.prototype.pop()` (ES 23.1.3.19).
pub fn array_pop(obj: &Value) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        if let Some(array_obj) = obj_ref.borrow_mut().as_any_mut().downcast_mut::<ArrayObject>() {
            // Step 3 performs `Set(O, "length", 0, true)` even for an empty
            // array, so a non-writable length aborts before anything changes.
            set_length_throwing(array_obj)?;
            return Ok(array_obj.pop());
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    if len == 0 {
        generic_set_length(obj, 0)?;
        return Ok(Value::Undefined);
    }
    let index = len - 1;
    let element = generic_get(obj, index);
    if !generic_delete(obj, index)? {
        return Err(RuntimeError::TypeError("Cannot delete property".to_string()));
    }
    generic_set_length(obj, index)?;
    Ok(element)
}

/// `Array.prototype.shift()` (ES 23.1.3.29).
pub fn array_shift(obj: &Value) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        if let Some(array_obj) = obj_ref.borrow_mut().as_any_mut().downcast_mut::<ArrayObject>() {
            set_length_throwing(array_obj)?;
            return Ok(array_obj.shift());
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    if len == 0 {
        generic_set_length(obj, 0)?;
        return Ok(Value::Undefined);
    }
    let first = generic_get(obj, 0);
    generic_window_move(obj, 1, len, -1, len)?;
    generic_delete_range(obj, len - 1, len, len)?;
    generic_set_length(obj, len - 1)?;
    Ok(first)
}

/// `Array.prototype.unshift(...items)` (ES 23.1.3.32).
pub fn array_unshift(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        if let Some(array_obj) = obj_ref.borrow_mut().as_any_mut().downcast_mut::<ArrayObject>() {
            set_length_throwing(array_obj)?;
            for arg in args.iter().rev() {
                array_obj.unshift(arg.clone());
            }
            return Ok(Value::Number(array_obj.len() as f64));
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    let count = args.len() as u64;
    let new_len = len.saturating_add(count);
    if new_len > MAX_ARRAY_LENGTH {
        return Err(RuntimeError::TypeError("Invalid array length".to_string()));
    }
    if count > 0 {
        generic_window_move(obj, 0, len, count as i64, len)?;
        for (i, arg) in args.iter().enumerate() {
            generic_set(obj, i as u64, arg.clone())?;
        }
    }
    generic_set_length(obj, new_len)?;
    Ok(Value::Number(new_len as f64))
}

/// `Array.prototype.reverse()` (ES 23.1.3.24).
///
/// Implemented over the present indices: a pair with exactly one side present
/// moves that value to the other side, a pair with both present swaps them.
/// That is what the spec's `lower`/`upper` loop produces, without iterating
/// `length / 2` times on a sparse receiver.
pub fn array_reverse(obj: &Value) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let mut borrowed = obj_ref.borrow_mut();
        if let Some(array_obj) = borrowed.as_any_mut().downcast_mut::<ArrayObject>() {
            array_obj.reverse();
            return Ok(obj.clone());
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    if len < 2 {
        return Ok(obj.clone());
    }
    let present = own_index_keys_below(obj, len);
    let present_set: std::collections::HashSet<u64> = present.iter().copied().collect();

    let mut swaps: Vec<(u64, u64)> = Vec::new();
    let mut moves: Vec<(u64, Value)> = Vec::new();
    let mut deletes: Vec<u64> = Vec::new();
    for k in &present {
        let other = len - 1 - *k;
        if other == *k {
            // Middle element of an odd-length window: nothing to do.
            continue;
        }
        if present_set.contains(&other) {
            if *k < other {
                swaps.push((*k, other));
            }
        } else {
            moves.push((other, generic_get(obj, *k)));
            deletes.push(*k);
        }
    }

    for (lo, hi) in swaps {
        let (low_value, high_value) = (generic_get(obj, lo), generic_get(obj, hi));
        generic_set(obj, lo, high_value)?;
        generic_set(obj, hi, low_value)?;
    }
    for (dest, value) in moves {
        generic_set(obj, dest, value)?;
    }
    for k in deletes {
        generic_delete(obj, k)?;
    }
    Ok(obj.clone())
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
                    clamped_index(Some(&args[1]), array_obj.len())?
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

/// `Array.prototype.lastIndexOf(searchElement[, fromIndex])`.
pub fn array_last_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let arr = obj_ref.borrow();
        if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
            let needle = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_obj.len();
            if len == 0 {
                return Ok(Value::Number(-1.0));
            }
            let from = match args.get(1) {
                Some(v) => {
                    let n = super::to_number_throwing(v)?;
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

/// `Array.prototype.fill(value[, start[, end]])` (ES 23.1.3.6).
pub fn array_fill(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let mut arr = obj_ref.borrow_mut();
        if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_obj.len();
            let resolve = |arg: Option<&Value>, default: usize| -> Result<usize, RuntimeError> {
                match arg {
                    Some(v) => {
                        let n = super::to_number_throwing(v)?;
                        Ok(if n < 0.0 {
                            ((len as f64) + n).max(0.0) as usize
                        } else {
                            (n as usize).min(len)
                        })
                    }
                    None => Ok(default),
                }
            };
            let start = resolve(args.get(1), 0)?;
            let end = resolve(args.get(2), len)?;
            for i in start..end {
                if let Some(slot) = array_obj.get_mut(i) {
                    *slot = value.clone();
                }
            }
            return Ok(obj.clone());
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let start = relative_index(args.get(1), len)?;
    let end = match args.get(2) {
        Some(v) if !v.is_undefined() => relative_index(Some(v), len)?,
        _ => len,
    };
    if end.saturating_sub(start) > MAX_GENERIC_ELEMENTS {
        return Err(RuntimeError::RangeError(
            "Invalid array length".to_string(),
        ));
    }
    for k in start..end {
        generic_set(obj, k, value.clone())?;
    }
    Ok(obj.clone())
}

/// `Array.prototype.join(separator)` (ES 23.1.3.15).
pub fn array_join(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let arr = obj_ref.borrow();
        if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
            let sep = if args.is_empty() {
                ","
            } else {
                &args[0].to_js_string()
            };
            return Ok(Value::string(&array_obj.join(sep)));
        }
    }
    to_object(obj)?;
    let separator = args
        .first()
        .map(|v| v.to_js_string())
        .unwrap_or_else(|| ",".to_string());
    let len = generic_length(obj);
    if len == 0 {
        return Ok(Value::string(""));
    }
    // Every index past the first contributes a separator, present or not; a
    // `length` near 2^53 must therefore fail fast instead of building a string
    // that cannot exist anyway.
    let separator_bytes = separator.len().max(1) as u64;
    if len.saturating_sub(1).saturating_mul(separator_bytes) > MAX_JOIN_LENGTH as u64 {
        // The reference engines raise a `TypeError` here (`join` has no
        // specified error for "the result cannot exist"); match them.
        return Err(RuntimeError::TypeError("Invalid string length".to_string()));
    }
    // A separator precedes every index but the first, whether or not that index
    // exists, so `written` counts the separators emitted so far (= the last
    // index written) and index `k` needs `k - written` more.
    let mut out = String::new();
    let mut written = 0u64;
    for k in own_index_keys_below(obj, len) {
        for _ in written..k {
            out.push_str(&separator);
        }
        written = k;
        let value = generic_get(obj, k);
        if !matches!(value, Value::Undefined | Value::Null) {
            out.push_str(&value.to_js_string());
        }
    }
    let separators_after = len - 1;
    for _ in written..separators_after {
        out.push_str(&separator);
    }
    Ok(Value::string(&out))
}

/// `Array.prototype.slice(start, end)` (ES 23.1.3.25).
pub fn array_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let arr = obj_ref.borrow();
        if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
            let len = array_obj.len();
            let start = clamped_index(args.first(), len)?;
            let end = if args.len() < 2 {
                len
            } else {
                clamped_index(args.get(1), len)?
            };
            let sliced = array_obj.slice(start, end);
            return Ok(Value::Object(Rc::new(RefCell::new(sliced))));
        }
    }
    to_object(obj)?;
    let len = generic_length(obj);
    let start = relative_index(args.first(), len)?;
    let end = match args.get(1) {
        Some(v) if !v.is_undefined() => relative_index(Some(v), len)?,
        _ => len,
    };
    let count = end.saturating_sub(start);
    if count > MAX_MATERIALIZED_LEN as u64 {
        return Err(RuntimeError::RangeError("Invalid array length".to_string()));
    }
    let mut result = ArrayObject::new();
    for k in 0..count {
        let index = start + k;
        if generic_has(obj, index) {
            result.push(generic_get(obj, index));
        } else {
            result.push(Value::Undefined);
            result.mark_hole(k as usize);
        }
    }
    Ok(Value::Object(Rc::new(RefCell::new(result))))
}

/// `Array.prototype.concat(...items)` (ES 23.1.3.1).
///
/// Items are spread when `IsConcatSpreadable` says so, which is what
/// `Symbol.isConcatSpreadable` is for.
pub fn array_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut result = ArrayObject::new();
    let mut append = |item: &Value,
                      result: &mut ArrayObject|
     -> Result<(), RuntimeError> {
        if is_concat_spreadable(item) {
            let len = generic_length(item);
            if len > MAX_MATERIALIZED_LEN as u64 {
                return Err(RuntimeError::RangeError("Invalid array length".to_string()));
            }
            for k in 0..len {
                result.push(generic_get(item, k));
            }
        } else {
            result.push(item.clone());
        }
        Ok(())
    };

    append(obj, &mut result)?;
    for arg in args {
        append(arg, &mut result)?;
    }
    Ok(Value::Object(Rc::new(RefCell::new(result))))
}

/// `Array.prototype.splice(start, deleteCount, ...items)` (ES 23.1.3.28).
pub fn array_splice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Value::Object(obj_ref) = obj {
        let mut arr = obj_ref.borrow_mut();
        if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
            let len = array_obj.len();
            let start = clamped_index(args.first(), len)?;
            // Step 5 of ES 23.1.3.28: with no arguments at all `start` is not
            // present, so nothing is deleted (only `splice(start)` deletes the
            // range `len - start`).
            let delete_count = if args.is_empty() {
                0
            } else if args.len() < 2 || args[1].is_undefined() {
                len - start
            } else {
                let n = super::to_number_throwing(&args[1])?;
                let n = if n.is_nan() || n < 0.0 {
                    0usize
                } else if n.is_infinite() {
                    len - start
                } else {
                    n.trunc() as usize
                };
                n.min(len - start)
            };
            let insert_items: Vec<Value> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                vec![]
            };
            let removed = array_obj.splice(start, delete_count, &insert_items);
            return Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
                removed,
            )))));
        }
    }

    to_object(obj)?;
    let len = generic_length(obj);
    let start = relative_index(args.first(), len)?;
    let delete_count = if args.is_empty() {
        // `splice()` with no arguments: `start` is not present, so nothing is
        // removed (ES 23.1.3.28 step 5).
        0
    } else {
        match args.get(1) {
            None | Some(Value::Undefined) => len - start,
            Some(v) => {
                let n = super::to_integer_or_infinity_throwing(Some(v))?;
                n.max(0).min((len - start) as i64) as u64
            }
        }
    };
    let items: &[Value] = if args.len() > 2 { &args[2..] } else { &[] };
    let item_count = items.len() as u64;
    let new_len = len - delete_count + item_count;
    if new_len > MAX_ARRAY_LENGTH {
        return Err(RuntimeError::TypeError("Invalid array length".to_string()));
    }

    // 1. Snapshot the removed range (holes stay holes in the result). The
    //    removed array's length is `delete_count`, which an array-like may have
    //    set far beyond what can be materialized.
    if delete_count > MAX_GENERIC_ELEMENTS {
        return Err(RuntimeError::RangeError(
            "Invalid array length".to_string(),
        ));
    }
    let mut removed = ArrayObject::new();
    for k in 0..delete_count {
        let index = start + k;
        if generic_has(obj, index) {
            removed.push(generic_get(obj, index));
        } else {
            removed.push(Value::Undefined);
            removed.mark_hole(k as usize);
        }
    }

    // 2. Move the tail, then write the inserted items.
    if item_count < delete_count {
        generic_window_move(obj, start + delete_count, len, -((delete_count - item_count) as i64), len)?;
        // Everything past the new length is dropped.
        generic_delete_range(obj, new_len, len, len)?;
    } else if item_count > delete_count {
        generic_window_move(obj, start + delete_count, len, (item_count - delete_count) as i64, len)?;
    }
    for (i, item) in items.iter().enumerate() {
        generic_set(obj, start + i as u64, item.clone())?;
    }
    generic_set_length(obj, new_len)?;
    Ok(Value::Object(Rc::new(RefCell::new(removed))))
}
