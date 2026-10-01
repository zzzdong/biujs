mod array;
mod boolean;
mod console;
mod date;
mod promise;
mod regexp;
mod error;
mod function;
mod json;
mod map;
mod math;
mod number;
mod object;
mod set;
mod string;
mod weak;
mod symbol;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{JSObject, PropertyTable};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

pub use crate::vm::ObjectKind;

pub use array::{ARRAY_SPECIES_NATIVE, array_constructor, validate_array_length};
pub use date::date_construct_value;
pub use regexp::match_all_matches;
pub mod typedarray;
pub mod reflect;
pub mod proxy;
pub use regexp::regexp_construct_value;
pub use boolean::boolean_constructor;
pub use error::{
    ErrorType, create_error_object, error_constructor, runtime_error_to_js_error,
    setup_error_prototype, setup_native_error_prototype,
};
pub use function::{function_constructor, setup_function_prototype};
pub use json::{
    json_number, json_parse, json_stringify, own_enumerable_string_keys, quote_json_string,
    register_json,
};
pub use map::{
    MAP_SIZE_NATIVE, MAP_SPECIES_NATIVE, map_clear, map_delete, map_get, map_has, map_set,
    map_size, register_map_prototype,
};
pub use number::{
    number_constructor, number_is_finite, number_is_integer, number_is_nan, number_to_exponential,
    number_to_fixed, number_to_precision,
};
pub use object::{OBJECT_TO_STRING_NATIVE, object_constructor, object_prototype_to_string};
pub use promise::PROMISE_SPECIES_NATIVE;
pub use set::{
    SET_SIZE_NATIVE, SET_SPECIES_NATIVE, register_set_prototype, set_add, set_clear, set_delete,
    set_has, set_size,
};
pub use weak::{
    can_be_held_weakly, register_weakmap_prototype, register_weakset_prototype, weakmap_delete,
    weakmap_get, weakmap_has, weakmap_set, weakset_add, weakset_delete, weakset_has,
};

// ─────────────────────────────────────────────────────────
// Wrapper-object prototypes (for `Object(value)` / ToObject)
// ─────────────────────────────────────────────────────────

thread_local! {
    /// Constructor-name → prototype map used by `to_object`. Populated during
    /// `Builtins::register`; the latest registration wins (one VM per thread
    /// at a time is the norm, and each `VM::new` re-registers).
    static WRAPPER_PROTOTYPES: std::cell::RefCell<HashMap<&'static str, Rc<RefCell<dyn JSObject>>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Make `proto` the prototype of wrappers created by `to_object` for the
/// constructor `name` (`"Number"`, `"String"`, `"Boolean"`, `"Symbol"`,
/// `"Object"`).
pub fn register_wrapper_prototype(name: &'static str, proto: Rc<RefCell<dyn JSObject>>) {
    WRAPPER_PROTOTYPES.with(|map| {
        map.borrow_mut().insert(name, proto);
    });
}

/// Drop every entry of the thread-local wrapper-prototype registry.
///
/// Called from [`Builtins::teardown`]: the registry is keyed by name, so it
/// holds an `Rc` to the *last* realm's prototypes and would pin that realm.
pub fn clear_wrapper_prototypes() {
    WRAPPER_PROTOTYPES.with(|map| map.borrow_mut().clear());
}

pub fn wrapper_prototype(name: &str) -> Option<Rc<RefCell<dyn JSObject>>> {
    WRAPPER_PROTOTYPES.with(|map| map.borrow().get(name).cloned())
}

/// ES `ToObject` (7.1.13): primitives are boxed in a wrapper object whose
/// prototype is the corresponding built-in prototype; objects pass through;
/// `undefined`/`null` raise `TypeError`.
pub fn to_object(value: &Value) -> Result<Value, RuntimeError> {
    match value {
        Value::Object(_) | Value::Function(_) => Ok(value.clone()),
        Value::Undefined | Value::Null => Err(RuntimeError::TypeError(
            "Cannot convert undefined or null to object".to_string(),
        )),
        kind => {
            let name: &'static str = match kind {
                Value::Bool(_) => "Boolean",
                Value::Number(_) => "Number",
                Value::String(_) => "String",
                Value::Symbol(_) => "Symbol",
                _ => "Object",
            };
            let proto = wrapper_prototype(name);
            Ok(Value::Object(Rc::new(RefCell::new(
                crate::vm::object::PrimitiveWrapperObject::new(value.clone(), proto),
            ))))
        }
    }
}

/// ES `ToIntegerOrInfinity` (7.1.5), saturating `±Infinity` to `i64` bounds.
pub fn to_integer_or_infinity(value: Option<&Value>) -> i64 {
    let n = value.map(|v| v.to_number()).unwrap_or(f64::NAN);
    if n.is_nan() {
        return 0;
    }
    if n.is_infinite() {
        return if n.is_sign_positive() { i64::MAX } else { i64::MIN };
    }
    let truncated = n.trunc();
    if truncated >= i64::MAX as f64 {
        i64::MAX
    } else if truncated <= i64::MIN as f64 {
        i64::MIN
    } else {
        truncated as i64
    }
}

/// ES `ToUint16` (7.1.6): `ToNumber` first, then the value modulo 2**16.
pub fn to_uint16(n: f64) -> u16 {
    if n.is_nan() || n.is_infinite() || n == 0.0 {
        return 0;
    }
    let truncated = n.trunc();
    let rem = truncated % 65_536.0;
    (rem as i64 % 65_536) as u16
}

// ─────────────────────────────────────────────────────────
// Abstract operations with their abrupt completions
// ─────────────────────────────────────────────────────────
//
// `Value::to_number` / `Value::to_js_string` are the *total* implementations:
// they always produce something, because the places that use them (array `join`,
// `format!`-style diagnostics, the `ToString` opcode) have no `Result` channel.
// A built-in defined by the spec with `? ToNumber(x)` / `? ToString(x)` cannot
// use those: `ToNumber(Symbol)` and `ToString(Symbol)` are TypeErrors, and
// silently turning them into `NaN` / `"Symbol()"` is what made whole families of
// `return-abrupt-*` tests fail. The helpers below are the throwing counterparts.

/// ES `RequireObjectCoercible` (7.2.1).
///
/// `undefined` and `null` have no object form, so every built-in that starts
/// with `Let O be ? ToObject(this)` — or needs the *receiver itself* rather than
/// a wrapper — reports a TypeError here.
pub fn require_object_coercible(value: &Value) -> Result<&Value, RuntimeError> {
    match value {
        Value::Undefined | Value::Null => Err(RuntimeError::TypeError(
            "Cannot convert undefined or null to object".to_string(),
        )),
        other => Ok(other),
    }
}

/// ES `ToString` (7.1.12) as an operation that can be abrupt.
pub fn to_string_throwing(value: &Value) -> Result<String, RuntimeError> {
    match value {
        Value::Symbol(_) => Err(RuntimeError::TypeError(
            "Cannot convert a Symbol value to a string".to_string(),
        )),
        _ => Ok(value.to_js_string()),
    }
}

/// ES `ToNumber` (7.1.3) as an operation that can be abrupt.
pub fn to_number_throwing(value: &Value) -> Result<f64, RuntimeError> {
    match value {
        Value::Symbol(_) => Err(RuntimeError::TypeError(
            "Cannot convert a Symbol value to a number".to_string(),
        )),
        _ => Ok(value.to_number()),
    }
}

/// The preamble shared by nearly every `String.prototype` method:
/// `RequireObjectCoercible(this)` followed by `ToString(this)`.
pub fn string_receiver(value: &Value) -> Result<String, RuntimeError> {
    require_object_coercible(value)?;
    to_string_throwing(value)
}

/// ES `ToIntegerOrInfinity` (7.1.5) that propagates `ToNumber`'s abrupt
/// completion, saturating `±Infinity` to `i64` bounds.
pub fn to_integer_or_infinity_throwing(value: Option<&Value>) -> Result<i64, RuntimeError> {
    let n = match value {
        Some(v) => to_number_throwing(v)?,
        None => f64::NAN,
    };
    Ok(to_integer_or_infinity_from(n))
}

fn to_integer_or_infinity_from(n: f64) -> i64 {
    if n.is_nan() {
        return 0;
    }
    if n.is_infinite() {
        return if n.is_sign_positive() { i64::MAX } else { i64::MIN };
    }
    let truncated = n.trunc();
    if truncated >= i64::MAX as f64 {
        i64::MAX
    } else if truncated <= i64::MIN as f64 {
        i64::MIN
    } else {
        truncated as i64
    }
}

/// ES `ToLength` (7.1.19) that propagates `ToNumber`'s abrupt completion.
pub fn to_length_throwing(value: &Value) -> Result<u64, RuntimeError> {
    let n = to_number_throwing(value)?;
    Ok(to_length_from(n))
}

fn to_length_from(n: f64) -> u64 {
    if n.is_nan() || n <= 0.0 {
        0
    } else if n.is_infinite() {
        (1u64 << 53) - 1
    } else {
        n.trunc().min(((1u64 << 53) - 1) as f64) as u64
    }
}

/// `parseInt(string, radix)` — parses a leading integer in the given radix.
/// `encodeURI` / `encodeURIComponent` (ES 19.2.6.?) — percent-encoding of the
/// UTF-8 bytes, keeping the unescaped set the spec lists for each function.
fn global_encode_uri(args: &[Value], whole_uri: bool) -> Result<Value, RuntimeError> {
    let s = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
            || (whole_uri && matches!(b, b';' | b',' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'#'));
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(Value::string(&out))
}

/// `decodeURI` / `decodeURIComponent` — the inverse. A malformed escape is a
/// URIError per the spec; the engine has no `URIError` variant of
/// `RuntimeError`, so the error object is built and thrown as a value.
fn global_decode_uri(args: &[Value], _whole_uri: bool) -> Result<Value, RuntimeError> {
    let s = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return uri_error("URI malformed");
            }
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                _ => return uri_error("URI malformed"),
            }
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    match String::from_utf8(out) {
        Ok(text) => Ok(Value::string(&text)),
        Err(_) => uri_error("URI malformed"),
    }
}

fn uri_error(message: &str) -> Result<Value, RuntimeError> {
    let err = error::error_constructor(
        error::ErrorType::URIError,
        &[Value::string(message)],
    )?;
    Err(RuntimeError::Thrown(err))
}

pub fn global_parse_int(args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(input) = args.first() else {
        return Ok(Value::Number(f64::NAN));
    };
    let text = input.to_js_string();
    let trimmed = text.trim_start();
    let radix = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let radix = if radix == 0.0 {
        if digits.starts_with("0x") || digits.starts_with("0X") {
            16
        } else {
            10
        }
    } else if (2.0..=36.0).contains(&radix) {
        radix.trunc() as u32
    } else {
        return Ok(Value::Number(f64::NAN));
    };
    let body = if radix == 16 {
        digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
            .unwrap_or(digits)
    } else {
        digits
    };
    let end = body
        .find(|c: char| c.to_digit(radix).is_none())
        .unwrap_or(body.len());
    if end == 0 {
        return Ok(Value::Number(f64::NAN));
    }
    match i64::from_str_radix(&body[..end], radix) {
        Ok(n) => Ok(Value::Number(sign * n as f64)),
        Err(_) => {
            // Longer than i64: fall back to a floating-point accumulation.
            let mut acc = 0.0f64;
            for c in body[..end].chars() {
                acc = acc * radix as f64 + c.to_digit(radix).unwrap_or(0) as f64;
            }
            Ok(Value::Number(sign * acc))
        }
    }
}

/// `parseFloat(string)` — parses a leading decimal number.
pub fn global_parse_float(args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(input) = args.first() else {
        return Ok(Value::Number(f64::NAN));
    };
    let text = input.to_js_string();
    let trimmed = text.trim_start();
    // Longest prefix that still parses as a number.
    let mut best = f64::NAN;
    let candidate = if let Some(rest) = trimmed.strip_prefix("Infinity") {
        let _ = rest;
        return Ok(Value::Number(f64::INFINITY));
    } else if trimmed.starts_with("-Infinity") {
        return Ok(Value::Number(f64::NEG_INFINITY));
    } else {
        trimmed
    };
    for end in (1..=candidate.len()).rev() {
        if !candidate.is_char_boundary(end) {
            continue;
        }
        if let Ok(n) = candidate[..end].parse::<f64>() {
            best = n;
            break;
        }
    }
    Ok(Value::Number(best))
}
pub use string::string_constructor;
pub use symbol::symbol_constructor;
pub use symbol::{
    HAS_INSTANCE_SYMBOL_ID, SPECIES_SYMBOL_ID, SYMBOL_DESCRIPTION_NATIVE, TO_PRIMITIVE_SYMBOL_ID,
    TO_STRING_TAG_SYMBOL_ID, has_instance_symbol_key, is_concat_spreadable_symbol_key,
    register_symbol_value, species_symbol_key, symbol_description, symbol_value_by_id,
    to_primitive_symbol_key, to_string_tag_symbol_key,
};

// ─────────────────────────────────────────────────────────
// Builtins registry
// ─────────────────────────────────────────────────────────

pub struct Builtins {
    pub object_prototype: Rc<RefCell<dyn JSObject>>,
    pub array_prototype: Rc<RefCell<dyn JSObject>>,
    pub error_prototype: Rc<RefCell<dyn JSObject>>,
    pub type_error_prototype: Rc<RefCell<dyn JSObject>>,
    pub reference_error_prototype: Rc<RefCell<dyn JSObject>>,
    pub range_error_prototype: Rc<RefCell<dyn JSObject>>,
    pub uri_error_prototype: Rc<RefCell<dyn JSObject>>,
    pub eval_error_prototype: Rc<RefCell<dyn JSObject>>,
    pub syntax_error_prototype: Rc<RefCell<dyn JSObject>>,
    pub boolean_prototype: Rc<RefCell<dyn JSObject>>,
    pub number_prototype: Rc<RefCell<dyn JSObject>>,
    pub string_prototype: Rc<RefCell<dyn JSObject>>,
    pub function_prototype: Rc<RefCell<dyn JSObject>>,
    pub symbol_prototype: Rc<RefCell<dyn JSObject>>,
    pub map_prototype: Rc<RefCell<dyn JSObject>>,
    pub set_prototype: Rc<RefCell<dyn JSObject>>,
    pub weakmap_prototype: Rc<RefCell<dyn JSObject>>,
    pub weakset_prototype: Rc<RefCell<dyn JSObject>>,
    pub date_prototype: Rc<RefCell<dyn JSObject>>,
    pub regexp_prototype: Rc<RefCell<dyn JSObject>>,
    pub promise_prototype: Rc<RefCell<dyn JSObject>>,
}

impl Builtins {
    pub fn new() -> Self {
        let object_proto = new_proto(None, "Object");
        let array_proto = new_proto(Some(Rc::clone(&object_proto)), "Array");
        let error_proto = new_proto(Some(Rc::clone(&object_proto)), "Error");
        let type_error_proto = new_proto(Some(Rc::clone(&error_proto)), "TypeError");
        let reference_error_proto = new_proto(Some(Rc::clone(&error_proto)), "ReferenceError");
        let range_error_proto = new_proto(Some(Rc::clone(&error_proto)), "RangeError");
        let uri_error_proto = new_proto(Some(Rc::clone(&error_proto)), "URIError");
        let eval_error_proto = new_proto(Some(Rc::clone(&error_proto)), "EvalError");
        let syntax_error_proto = new_proto(Some(Rc::clone(&error_proto)), "SyntaxError");
        let boolean_proto = new_proto(Some(Rc::clone(&object_proto)), "Boolean");
        let number_proto = new_proto(Some(Rc::clone(&object_proto)), "Number");
        let string_proto = new_proto(Some(Rc::clone(&object_proto)), "String");
        let function_proto = new_proto(Some(Rc::clone(&object_proto)), "Function");
        let symbol_proto = new_proto(Some(Rc::clone(&object_proto)), "Symbol");
        let map_proto = new_proto(Some(Rc::clone(&object_proto)), "Map");
        let set_proto = new_proto(Some(Rc::clone(&object_proto)), "Set");
        let weakmap_proto = new_proto(Some(Rc::clone(&object_proto)), "WeakMap");
        let weakset_proto = new_proto(Some(Rc::clone(&object_proto)), "WeakSet");
        let date_proto = new_proto(Some(Rc::clone(&object_proto)), "Date");
        let regexp_proto = new_proto(Some(Rc::clone(&object_proto)), "RegExp");
        let promise_proto = new_proto(Some(Rc::clone(&object_proto)), "Promise");

        // Needed before any built-in function object is created: every one of
        // them inherits from `Function.prototype`.
        register_wrapper_prototype("Function", Rc::clone(&function_proto));

        // Set up Function.prototype with standard methods
        setup_function_prototype(&function_proto);

        Self {
            object_prototype: object_proto,
            array_prototype: array_proto,
            error_prototype: error_proto,
            type_error_prototype: type_error_proto,
            reference_error_prototype: reference_error_proto,
            range_error_prototype: range_error_proto,
            uri_error_prototype: uri_error_proto,
            eval_error_prototype: eval_error_proto,
            syntax_error_prototype: syntax_error_proto,
            boolean_prototype: boolean_proto,
            number_prototype: number_proto,
            string_prototype: string_proto,
            function_prototype: function_proto,
            symbol_prototype: symbol_proto,
            map_prototype: map_proto,
            set_prototype: set_proto,
            weakmap_prototype: weakmap_proto,
            weakset_prototype: weakset_proto,
            date_prototype: date_proto,
            regexp_prototype: regexp_proto,
            promise_prototype: promise_proto,
        }
    }

    /// Wire up the two sides of a constructor/prototype pair.
    ///
    /// `with_prototype` only sets the constructor's `[[Prototype]]` slot, so
    /// both cross-references have to be installed explicitly:
    /// `C.prototype === proto` (needed by `Object.prototype.toString.call(x)`)
    /// and `proto.constructor === C` (needed by `assert.throws`, which compares
    /// a caught error's `constructor` against `TypeError` and friends).
    fn link_constructor_prototype(fn_val: &Value, proto: &Rc<RefCell<dyn JSObject>>) {
        if let Value::Object(obj_ref) = fn_val {
            let _ = obj_ref.borrow_mut().define_property(
                PropertyKey::from_str("prototype"),
                constructor_prototype_descriptor(Value::Object(Rc::clone(proto))),
            );
        }
        let _ = proto.borrow_mut().define_property(
            PropertyKey::from_str("constructor"),
            method_descriptor(fn_val.clone()),
        );
    }

    /// Break the `prototype` ⇄ `constructor` cycles this realm created.
    ///
    /// There is no GC: a cycle between the prototype and its constructor keeps
    /// both — and everything they reach — alive even after the last external
    /// reference is gone. A host that makes a fresh `VM` per program (which is
    /// what the test262 runner must do: a test may mutate built-ins and expects a
    /// clean realm) therefore leaked one whole builtins graph per program.
    ///
    /// Removing the `constructor` property is enough: the constructor is only
    /// reachable through it once the realm's `globals` are gone.
    pub fn teardown(&self) {
        let prototypes: [&Rc<RefCell<dyn JSObject>>; 21] = [
            &self.object_prototype,
            &self.array_prototype,
            &self.error_prototype,
            &self.type_error_prototype,
            &self.reference_error_prototype,
            &self.range_error_prototype,
            &self.uri_error_prototype,
            &self.eval_error_prototype,
            &self.syntax_error_prototype,
            &self.boolean_prototype,
            &self.number_prototype,
            &self.string_prototype,
            &self.function_prototype,
            &self.symbol_prototype,
            &self.map_prototype,
            &self.set_prototype,
            &self.weakmap_prototype,
            &self.weakset_prototype,
            &self.date_prototype,
            &self.regexp_prototype,
            &self.promise_prototype,
        ];
        for proto in prototypes {
            let _ = proto
                .borrow_mut()
                .property_delete(&PropertyKey::from_str("constructor"));
        }
        // And drop the thread-local registry's `Rc`s: it keys by name, so it
        // would otherwise pin this realm's prototypes until the next realm is
        // built.
        clear_wrapper_prototypes();
    }

    pub fn register(&self, globals: &mut HashMap<String, Value>) {
        use crate::vm::object::NativeFunctionObject;

        let object_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Object"),
        )));
        object::register_object_statics(&object_fn_val, self);
        Self::link_constructor_prototype(&object_fn_val, &self.object_prototype);
        object::register_object_prototype(&self.object_prototype, &object_fn_val);
        globals.insert("Object".to_string(), object_fn_val);

        let array_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Array"),
        )));
        array::register_array_prototype(&self.array_prototype);
        array::register_array_statics(&array_fn_val);
        Self::link_constructor_prototype(&array_fn_val, &self.array_prototype);
        globals.insert("Array".to_string(), array_fn_val);

        // Error constructors with prototype property setup
        let error_constructors = [
            ("Error", Rc::clone(&self.error_prototype)),
            ("TypeError", Rc::clone(&self.type_error_prototype)),
            ("ReferenceError", Rc::clone(&self.reference_error_prototype)),
            ("RangeError", Rc::clone(&self.range_error_prototype)),
            ("URIError", Rc::clone(&self.uri_error_prototype)),
            ("EvalError", Rc::clone(&self.eval_error_prototype)),
            ("SyntaxError", Rc::clone(&self.syntax_error_prototype)),
        ];

        for (name, proto) in error_constructors {
            let fn_val = Value::Object(Rc::new(RefCell::new(
                NativeFunctionObject::new(name),
            )));

            Self::link_constructor_prototype(&fn_val, &proto);
            if name == "Error" {
                // `Error.isError(value)`: true for objects carrying
                // `[[ErrorData]]`, which the engine marks with the `[[Class]]`
                // "Error" (that is also what `Object.prototype.toString`
                // reports).
                set_static_method(&fn_val, "isError", |args| {
                    Ok(Value::Bool(matches!(
                        args.first(),
                        Some(Value::Object(o)) if o.borrow().class_name() == "Error"
                    )))
                });
            }
            globals.insert(name.to_string(), fn_val);
        }

        // Setup Error.prototype methods, then the `name` own property of each
        // native error prototype (`TypeError.prototype.name === "TypeError"`;
        // everything else is inherited from `Error.prototype`).
        error::setup_error_prototype(&self.error_prototype, self);
        for (name, proto) in [
            ("TypeError", &self.type_error_prototype),
            ("ReferenceError", &self.reference_error_prototype),
            ("RangeError", &self.range_error_prototype),
            ("URIError", &self.uri_error_prototype),
            ("EvalError", &self.eval_error_prototype),
            ("SyntaxError", &self.syntax_error_prototype),
        ] {
            error::setup_native_error_prototype(proto, name);
        }

        // Register `Symbol.iterator` on the natively iterable prototypes.
        //
        // For an array it *is* `Array.prototype.values` — the very same function
        // object (ES 23.1.3.30: "the initial value of the @@iterator property is
        // the %Array.prototype.values% intrinsic object"). Registering a
        // separate factory made `Array.prototype[Symbol.iterator] ===
        // Array.prototype.values` false and made
        // `Array.prototype[Symbol.iterator].call(o)` unreachable, since the
        // factory expects its receiver to already carry the method.
        //
        // A string gets its own function: it iterates code points, not indices.
        // Both are dispatched by name in `call_native_by_name` / recognized by
        // `make_iterator` as the built-in factory.
        // The read has to end before the write: `borrow()` in an `if let`
        // scrutinee lives for the whole body.
        let array_values_fn = self
            .array_prototype
            .borrow()
            .property_get(&PropertyKey::from_str("values"))
            .map(|desc| desc.value);
        if let Some(values_fn) = array_values_fn {
            let _ = self.array_prototype.borrow_mut().define_property(
                crate::vm::iterator::iterator_symbol_key(),
                method_descriptor(values_fn),
            );
        }
        let string_iterator_fn = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            crate::vm::iterator::ITERATOR_NATIVE_NAME,
        ))));
        let _ = self.string_prototype.borrow_mut().define_property(
            crate::vm::iterator::iterator_symbol_key(),
            method_descriptor(string_iterator_fn),
        );

        let bool_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Boolean"),
        )));
        boolean::register_boolean_prototype(&self.boolean_prototype);
        Self::link_constructor_prototype(&bool_fn_val, &self.boolean_prototype);
        globals.insert("Boolean".to_string(), bool_fn_val);

        let number_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Number"),
        )));
        number::register_number_statics(&number_fn_val, &self.number_prototype);
        number::register_number_prototype(&self.number_prototype);
        Self::link_constructor_prototype(&number_fn_val, &self.number_prototype);
        globals.insert("Number".to_string(), number_fn_val);

        let string_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("String"),
        )));
        string::register_string_prototype(&self.string_prototype);
        string::register_string_statics(&string_fn_val);
        Self::link_constructor_prototype(&string_fn_val, &self.string_prototype);
        globals.insert("String".to_string(), string_fn_val);

        let function_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Function"),
        )));
        function::register_function_statics(&function_fn_val, &self.function_prototype);
        Self::link_constructor_prototype(&function_fn_val, &self.function_prototype);
        globals.insert("Function".to_string(), function_fn_val);

        // Symbol constructor
        let symbol_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Symbol"),
        )));
        symbol::register_symbol_statics(&symbol_fn_val, &self.symbol_prototype);
        symbol::register_symbol_prototype(&self.symbol_prototype);
        Self::link_constructor_prototype(&symbol_fn_val, &self.symbol_prototype);

        // `ArrayBuffer` and the TypedArray views (ES 25.1 / 23.2). The
        // constructors are dispatched by the VM: `new Uint8Array(buf)` builds a
        // *view* over an existing buffer, and `new Uint8Array(4)` one over a fresh
        // one — the prototype comes from `newTarget`, so the builtin layer gets it
        // handed in.
        let ab_proto = new_proto(Some(Rc::clone(&self.object_prototype)), "ArrayBuffer");
        let ab_fn_val = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            "ArrayBuffer",
        ))));
        typedarray::register_arraybuffer_prototype(&ab_proto);
        Self::link_constructor_prototype(&ab_fn_val, &ab_proto);
        set_static_method(&ab_fn_val, "isView", |args| {
            typedarray::arraybuffer_is_view(args)
        });
        globals.insert("ArrayBuffer".to_string(), ab_fn_val);

        for kind in typedarray::TYPED_ARRAY_KINDS {
            let name = kind.name();
            let proto = new_proto(Some(Rc::clone(&self.object_prototype)), name);
            let ctor_val = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
                name,
            ))));
            typedarray::register_typedarray_prototype(kind, &proto);
            Self::link_constructor_prototype(&ctor_val, &proto);
            // `Uint8Array.BYTES_PER_ELEMENT` (ES 23.2.5.3), read-only.
            if let Value::Object(ctor_obj) = &ctor_val {
                let _ = ctor_obj.borrow_mut().define_property(
                    PropertyKey::from_str("BYTES_PER_ELEMENT"),
                    PropertyDescriptor {
                        value: Value::Number(kind.bytes_per_element() as f64),
                        writable: false,
                        enumerable: false,
                        configurable: false,
                        getter: None,
                        setter: None,
                    },
                );
            }
            globals.insert(name.to_string(), ctor_val);
        }

        // `Map` (ES 23.1). The constructor itself is dispatched by the VM:
        // `new Map(iterable)` has to drive the iterator protocol, and calling
        // it without `new` is a TypeError.
        let map_fn_val = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("Map"))));
        map::register_map_prototype(&self.map_prototype);
        Self::link_constructor_prototype(&map_fn_val, &self.map_prototype);
        // `%Map.prototype.entries%` and `Map.prototype[Symbol.iterator]` are
        // the same object (ES 23.1.3.4), which also makes `for (x of map)`
        // reach the VM through the ordinary `GetMethod` lookup.
        let entries_fn = self
            .map_prototype
            .borrow()
            .property_get(&PropertyKey::from_str("entries"))
            .map(|desc| desc.value);
        if let Some(entries_fn) = entries_fn {
            let _ = self.map_prototype.borrow_mut().define_property(
                crate::vm::iterator::iterator_symbol_key(),
                method_descriptor(entries_fn),
            );
        }
        // `Map.groupBy` (ES2024 `array-grouping`) — iterates + calls back, so
        // the implementation is the VM's (`call_static_method` never sees it).
        set_static_method(&map_fn_val, "groupBy", |_args| {
            Err(RuntimeError::TypeError(
                "Map.groupBy is dispatched by the VM".to_string(),
            ))
        });
        register_wrapper_prototype("Map", Rc::clone(&self.map_prototype));
        // `Map[Symbol.species]` is an accessor returning its receiver
        // (ES 23.1.2.2); like `Array`'s, the getter is dispatched by the VM.
        let species_getter = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            map::MAP_SPECIES_NATIVE,
        ))));
        if let Value::Object(map_obj) = &map_fn_val {
            let _ = map_obj.borrow_mut().define_property(
                species_symbol_key(),
                PropertyDescriptor {
                    value: Value::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    getter: Some(species_getter),
                    setter: None,
                },
            );
        }
        globals.insert("Map".to_string(), map_fn_val);

        // `Set` (ES 23.2) — same shape as `Map`; the constructor is the VM's
        // (`new Set(iterable)` drives the iteration protocol, `Set()` without
        // `new` is a TypeError).
        let set_fn_val = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("Set"))));
        set::register_set_prototype(&self.set_prototype);
        Self::link_constructor_prototype(&set_fn_val, &self.set_prototype);
        // `Set.prototype.keys` and `Set.prototype[Symbol.iterator]` are the
        // *same* function object, namely `Set.prototype.values` (ES 23.2.3.10).
        let set_values_fn = self
            .set_prototype
            .borrow()
            .property_get(&PropertyKey::from_str("values"))
            .map(|desc| desc.value);
        if let Some(values_fn) = set_values_fn {
            {
                let mut proto = self.set_prototype.borrow_mut();
                let _ = proto.define_property(
                    PropertyKey::from_str("keys"),
                    method_descriptor(values_fn.clone()),
                );
                let _ = proto.define_property(
                    crate::vm::iterator::iterator_symbol_key(),
                    method_descriptor(values_fn),
                );
            }
        }
        register_wrapper_prototype("Set", Rc::clone(&self.set_prototype));
        // `Set[Symbol.species]` (ES 23.2.2.2), mirroring `Map`'s accessor.
        let set_species_getter = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            set::SET_SPECIES_NATIVE,
        ))));
        if let Value::Object(set_obj) = &set_fn_val {
            let _ = set_obj.borrow_mut().define_property(
                species_symbol_key(),
                PropertyDescriptor {
                    value: Value::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    getter: Some(set_species_getter),
                    setter: None,
                },
            );
        }
        globals.insert("Set".to_string(), set_fn_val);

        // `WeakMap` / `WeakSet` (ES 23.3 / 23.4): four and three methods
        // respectively, no `size`, no iteration. The constructors are the VM's
        // (`new WeakMap(iterable)` drives the iteration protocol).
        let weakmap_fn_val =
            Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("WeakMap"))));
        weak::register_weakmap_prototype(&self.weakmap_prototype);
        Self::link_constructor_prototype(&weakmap_fn_val, &self.weakmap_prototype);
        register_wrapper_prototype("WeakMap", Rc::clone(&self.weakmap_prototype));
        globals.insert("WeakMap".to_string(), weakmap_fn_val);

        let weakset_fn_val =
            Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("WeakSet"))));
        weak::register_weakset_prototype(&self.weakset_prototype);
        Self::link_constructor_prototype(&weakset_fn_val, &self.weakset_prototype);
        register_wrapper_prototype("WeakSet", Rc::clone(&self.weakset_prototype));
        globals.insert("WeakSet".to_string(), weakset_fn_val);

        // Well-known symbols (ES6 §19.4.2): `Symbol.iterator` is required by
        // the iteration protocol; `toPrimitive` / `toStringTag` / `hasInstance`
        // are honoured by `ToPrimitive`, `Object.prototype.toString` and
        // `instanceof`; the rest are registered for property lookups.
        // The *properties of the Symbol constructor* are string-keyed
        // (`Symbol.iterator` reads the "iterator" property); their VALUES are
        // the well-known symbol values used as property keys elsewhere.
        let well_known: [(&str, u64, &str); 15] = [
            ("iterator", crate::vm::iterator::ITERATOR_SYMBOL_ID, "Symbol.iterator"),
            ("asyncIterator", 0xFFFF_FFFF_FFFF_0001, "Symbol.asyncIterator"),
            ("hasInstance", symbol::HAS_INSTANCE_SYMBOL_ID, "Symbol.hasInstance"),
            ("isConcatSpreadable", symbol::IS_CONCAT_SPREADABLE_SYMBOL_ID, "Symbol.isConcatSpreadable"),
            ("toPrimitive", symbol::TO_PRIMITIVE_SYMBOL_ID, "Symbol.toPrimitive"),
            ("toStringTag", symbol::TO_STRING_TAG_SYMBOL_ID, "Symbol.toStringTag"),
            ("species", symbol::SPECIES_SYMBOL_ID, "Symbol.species"),
            ("unscopables", 0xFFFF_FFFF_FFFF_0007, "Symbol.unscopables"),
            ("match", 0xFFFF_FFFF_FFFF_0008, "Symbol.match"),
            ("matchAll", 0xFFFF_FFFF_FFFF_0009, "Symbol.matchAll"),
            ("replace", 0xFFFF_FFFF_FFFF_000A, "Symbol.replace"),
            ("search", 0xFFFF_FFFF_FFFF_000B, "Symbol.search"),
            ("split", 0xFFFF_FFFF_FFFF_000C, "Symbol.split"),
            ("dispose", 0xFFFF_FFFF_FFFF_000D, "Symbol.dispose"),
            ("asyncDispose", 0xFFFF_FFFF_FFFF_000E, "Symbol.asyncDispose"),
        ];
        if let Value::Object(symbol_obj) = &symbol_fn_val {
            for (name, id, description) in well_known {
                let symbol = Value::Symbol(Rc::new(crate::vm::value::SymbolData::new(
                    Some(description.to_string()),
                    id,
                )));
                symbol::register_symbol_value(&symbol);
                // Well-known symbols are non-writable, non-enumerable and
                // non-configurable (ES 19.4.2.1).
                let desc = PropertyDescriptor {
                    value: symbol,
                    writable: false,
                    enumerable: false,
                    configurable: false,
                    getter: None,
                    setter: None,
                };
                let _ = symbol_obj
                    .borrow_mut()
                    .define_property(PropertyKey::from_str(name), desc);
            }
        }
        globals.insert("Symbol".to_string(), symbol_fn_val);

        // `Math` — a plain namespace object of numeric helpers.
        globals.insert("Math".to_string(), math::create_math_object());
        // `console` — a host object, not part of ES, but the one thing a script
        // cannot do without: without it the only way to observe a value is to
        // `throw` it.
        globals.insert("console".to_string(), console::create_console_object());
        // `Reflect` (ES 28.1) — the internal methods as functions. Registered
        // before `Proxy` exists on purpose: a proxy handler *is* this list, and
        // building it first is what keeps the proxy work from re-inventing it.
        globals.insert("Reflect".to_string(), reflect::create_reflect_object());
        // `Proxy` (ES 28.2): the exotic object is inert until something operates on
        // it, and every operation is a trap lookup — so the constructor only checks
        // its two arguments and the VM does the rest (`new Proxy(t, h)` and
        // `Proxy.revocable`). No `prototype`: instances inherit from the target.
        globals.insert("Proxy".to_string(), proxy::create_proxy_constructor());

        // `Date`: everyday scripts need wall-clock time. `new Date(...)` builds
        // the object in the VM (`Date()` without `new` answers a *string*, so
        // the two cannot share `call_native`).
        let date_fn_val =
            Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("Date"))));
        date::register_date_prototype(&self.date_prototype);
        date::register_date_statics(&date_fn_val);
        Self::link_constructor_prototype(&date_fn_val, &self.date_prototype);
        register_wrapper_prototype("Date", Rc::clone(&self.date_prototype));
        globals.insert("Date".to_string(), date_fn_val);

        // `RegExp`: the matcher lives in `vm::object::RegExpObject` (a
        // `regex::Regex`). `RegExp(...)` answers an object whether or not it is
        // called with `new`, so `call_native` can serve both paths.
        let regexp_fn_val =
            Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("RegExp"))));
        regexp::register_regexp_prototype(&self.regexp_prototype);
        Self::link_constructor_prototype(&regexp_fn_val, &self.regexp_prototype);
        register_wrapper_prototype("RegExp", Rc::clone(&self.regexp_prototype));
        globals.insert("RegExp".to_string(), regexp_fn_val);

        // `Promise`: the object lives in `vm::object::PromiseObject`, and the
        // methods that must *call* JS (`then`, `catch`) are dispatched by the
        // VM; the registration below is what makes the names resolvable.
        let promise_fn_val =
            Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("Promise"))));
        promise::register_promise_prototype(&self.promise_prototype);
        promise::register_promise_statics(&promise_fn_val);
        // `Promise.prototype[Symbol.toStringTag] === "Promise"` (ES 27.2.5.5).
        define_string_tag(&self.promise_prototype, "Promise");
        // `Promise[Symbol.species]` (ES 27.2.2.3) is an accessor returning its
        // receiver, mirroring `Map`/`Set`/`Array`; the VM answers the getter.
        let promise_species_getter = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new(promise::PROMISE_SPECIES_NATIVE),
        )));
        if let Value::Object(promise_obj) = &promise_fn_val {
            let _ = promise_obj.borrow_mut().define_property(
                species_symbol_key(),
                PropertyDescriptor {
                    value: Value::Undefined,
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    getter: Some(promise_species_getter),
                    setter: None,
                },
            );
        }
        Self::link_constructor_prototype(&promise_fn_val, &self.promise_prototype);
        register_wrapper_prototype("Promise", Rc::clone(&self.promise_prototype));
        globals.insert("Promise".to_string(), promise_fn_val);
        globals.insert("JSON".to_string(), json::register_json());

        // Global convenience functions (they are plain functions, not
        // constructors, so they have no `prototype` slot).
        for name in [
            "isNaN",
            "isFinite",
            "parseInt",
            "parseFloat",
            // URI handling (ES 19.2.6): everyday scripts encode query strings
            // and paths, and the engine had no way to do either.
            "encodeURI",
            "encodeURIComponent",
            "decodeURI",
            "decodeURIComponent",
        ] {
            globals.insert(
                name.to_string(),
                Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(name)))),
            );
        }

        // Prototype registry for `Object(value)` / ToObject boxing, and for the
        // array objects the engine creates outside the `New` prologue.
        register_wrapper_prototype("Array", Rc::clone(&self.array_prototype));
        register_wrapper_prototype("Object", Rc::clone(&self.object_prototype));
        register_wrapper_prototype("Boolean", Rc::clone(&self.boolean_prototype));
        register_wrapper_prototype("Number", Rc::clone(&self.number_prototype));
        register_wrapper_prototype("String", Rc::clone(&self.string_prototype));
        register_wrapper_prototype("Symbol", Rc::clone(&self.symbol_prototype));
        // Error prototypes: `Error(msg)` without `new` still needs the right
        // `[[Prototype]]` on the object it returns.
        register_wrapper_prototype("Error", Rc::clone(&self.error_prototype));
        register_wrapper_prototype("TypeError", Rc::clone(&self.type_error_prototype));
        register_wrapper_prototype("ReferenceError", Rc::clone(&self.reference_error_prototype));
        register_wrapper_prototype("RangeError", Rc::clone(&self.range_error_prototype));
        register_wrapper_prototype("SyntaxError", Rc::clone(&self.syntax_error_prototype));
        register_wrapper_prototype("URIError", Rc::clone(&self.uri_error_prototype));
        register_wrapper_prototype("EvalError", Rc::clone(&self.eval_error_prototype));
    }
}

// ─────────────────────────────────────────────────────────
// Built-in property attributes (ES 17)
// ─────────────────────────────────────────────────────────

/// Attribute set of every built-in *method*: writable, non-enumerable,
/// configurable. Applies to prototype methods, static methods and `constructor`
/// back-references.
pub fn method_descriptor(value: Value) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    }
}

/// Attribute set of a built-in *constant* (`Math.PI`, `Number.MAX_VALUE`, …):
/// read-only, non-enumerable, non-configurable.
pub fn constant_descriptor(value: Value) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: false,
        enumerable: false,
        configurable: false,
        getter: None,
        setter: None,
    }
}

/// Attribute set of a *built-in* `C.prototype`: read-only, non-enumerable,
/// non-configurable (ES 17). User functions' `F.prototype` is writable —
/// see `new_function_object`.
pub fn constructor_prototype_descriptor(value: Value) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: false,
        enumerable: false,
        configurable: false,
        getter: None,
        setter: None,
    }
}

// ─────────────────────────────────────────────────────────
// Dispatchers
// ─────────────────────────────────────────────────────────

/// True when `err` only means "there is no built-in with that name", as opposed
/// to a genuine failure raised by a built-in that does exist.
///
/// Both come back as a `TypeError` from the name-keyed dispatch tables, so the
/// message is the only discriminator. Call sites that must not swallow real
/// errors use this predicate; see `VM`'s `CallMethod`.
pub fn is_unknown_builtin(err: &RuntimeError) -> bool {
    matches!(
        err,
        RuntimeError::TypeError(msg)
            if msg.starts_with("unknown prototype method: ")
                || msg.starts_with("unknown static method: ")
                || msg.starts_with("unknown built-in: ")
    )
}

pub fn call_native(name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    match name {
        "Object" => object::object_constructor(args),
        "Array" => array::array_constructor(args),
        "Error" => error::error_constructor(error::ErrorType::Error, args),
        "TypeError" => error::error_constructor(error::ErrorType::TypeError, args),
        "ReferenceError" => error::error_constructor(error::ErrorType::ReferenceError, args),
        "RangeError" => error::error_constructor(error::ErrorType::RangeError, args),
        "SyntaxError" => error::error_constructor(error::ErrorType::SyntaxError, args),
        "URIError" => error::error_constructor(error::ErrorType::URIError, args),
        "EvalError" => error::error_constructor(error::ErrorType::EvalError, args),
        // `Date()` without `new` returns a *string* (ES 21.4.2.1); `new Date()`
        // goes through `VM::date_construct` instead.
        "Date" => date::date_as_function(args),
        "RegExp" => regexp::regexp_construct_value(args, None),
        // `Promise(...)` without `new` is a TypeError (ES 27.2.1.1 step 1);
        // `[[Construct]]` is `VM::promise_construct`.
        "Promise" => Err(RuntimeError::TypeError(
            "Constructor Promise requires 'new'".to_string(),
        )),
        "parseInt" => global_parse_int(args),
        "encodeURIComponent" => global_encode_uri(args, false),
        "encodeURI" => global_encode_uri(args, true),
        "decodeURIComponent" => global_decode_uri(args, false),
        "decodeURI" => global_decode_uri(args, true),
        "parseFloat" => global_parse_float(args),
        "Boolean" => boolean::boolean_constructor(args),
        "Number" => number::number_constructor(args),
        "String" => string::string_constructor(args),
        "Function" => function::function_constructor(args),
        "Symbol" => symbol::symbol_constructor(args),
        // `Map()` without `new` is a TypeError (ES 23.1.1.1 step 1); the
        // `[[Construct]]` path is `VM::map_construct`, which needs the VM's
        // iteration protocol for the optional iterable argument.
        "Map" => Err(RuntimeError::TypeError(
            "Constructor Map requires 'new'".to_string(),
        )),
        // `Set()` without `new` (ES 23.2.1.1 step 1); `[[Construct]]` is
        // `VM::set_construct`.
        "Set" => Err(RuntimeError::TypeError(
            "Constructor Set requires 'new'".to_string(),
        )),
        "WeakMap" | "WeakSet" => Err(RuntimeError::TypeError(format!(
            "Constructor {name} requires 'new'"
        ))),
        // ES `isNaN` / `isFinite` coerce their argument with ToNumber first.
        "isNaN" => Ok(Value::Bool(
            args.first().map(|v| v.to_number().is_nan()).unwrap_or(true),
        )),
        "isFinite" => Ok(Value::Bool(
            args.first()
                .map(|v| v.to_number().is_finite())
                .unwrap_or(false),
        )),
        _ => Err(RuntimeError::TypeError(format!("unknown built-in: {name}"))),
    }
}

pub fn call_static_method(name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    // `Math` is a namespace object, so its methods are dispatched by prefix.
    if let Some(method) = name.strip_prefix("Math.") {
        return math::call_math_method(method, args);
    }
    // `console` is dispatched the same way (see `console.rs` for why it exists).
    if let Some(method) = name.strip_prefix("console.") {
        return console::call_console_method(method, args);
    }
    match name {
        "Number.isNaN" => number::number_is_nan(args),
        "Number.isFinite" => number::number_is_finite(args),
        "Number.isInteger" => number::number_is_integer(args),
        "Number.isSafeInteger" => Ok(Value::Bool(
            args.first()
                .map(|v| {
                    let n = v.to_number();
                    n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0
                })
                .unwrap_or(false),
        )),
        "Number.parseInt" => global_parse_int(args),
        "Number.parseFloat" => Ok(Value::Number(
            args.first()
                .map(|v| {
                    let s = v.to_js_string();
                    s.trim()
                        .parse::<f64>()
                        .ok()
                        .or_else(|| {
                            // Accept a valid numeric prefix, e.g. "3.5abc" -> 3.5.
                            let trimmed = s.trim();
                            let end = trimmed
                                .find(|c: char| {
                                    !(c.is_ascii_digit()
                                        || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
                                })
                                .unwrap_or(trimmed.len());
                            trimmed[..end].parse::<f64>().ok()
                        })
                        .unwrap_or(f64::NAN)
                })
                .unwrap_or(f64::NAN),
        )),
        "ArrayBuffer.isView" => typedarray::arraybuffer_is_view(args),
        // `Reflect`'s builtin half. The five that run user code (`get`, `set`,
        // `apply`, `construct`, `defineProperty`) never reach here: the VM
        // intercepts them by name before this table is consulted.
        "Reflect.has" => reflect::reflect_has(args),
        "Reflect.deleteProperty" => reflect::reflect_delete_property(args),
        "Reflect.ownKeys" => reflect::reflect_own_keys(args),
        "Reflect.getOwnPropertyDescriptor" => reflect::reflect_get_own_property_descriptor(args),
        "Reflect.getPrototypeOf" => reflect::reflect_get_prototype_of(args),
        "Reflect.setPrototypeOf" => reflect::reflect_set_prototype_of(args),
        "Reflect.isExtensible" => reflect::reflect_is_extensible(args),
        "Reflect.preventExtensions" => reflect::reflect_prevent_extensions(args),
        "Date.now" => date::date_now(args),
        "Date.parse" => date::date_parse(args),
        "Date.UTC" => date::date_utc(args),
        "Object.fromEntries" => object::object_from_entries(args),
        "Object.keys" => object::object_keys(args),
        "Object.values" => object::object_values(args),
        "Object.entries" => object::object_entries(args),
        "Object.assign" => object::object_assign(args),
        "Object.defineProperty" => object::object_define_property(args),
        "Object.defineProperties" => object::object_define_properties(args),
        "Object.create" => object::object_create(args),
        "Object.getPrototypeOf" => object::object_get_prototype_of(args),
        "Object.setPrototypeOf" => object::object_set_prototype_of(args),
        "Object.getOwnPropertyNames" => object::object_get_own_property_names(args),
        "Object.getOwnPropertySymbols" => object::object_get_own_property_symbols(args),
        "Object.getOwnPropertyDescriptor" => object::object_get_own_property_descriptor(args),
        "Object.hasOwn" => object::object_has_own(args),
        "Object.is" => object::object_is(args),
        "Object.isExtensible" => object::object_is_extensible(args),
        "Object.isFrozen" => object::object_is_frozen(args),
        "Object.isSealed" => object::object_is_sealed(args),
        "Object.preventExtensions" => object::object_prevent_extensions(args),
        "Object.seal" => object::object_seal(args),
        "Object.freeze" => object::object_freeze(args),
        "Array.isArray" => Ok(Value::Bool(matches!(
            args.first(),
            Some(Value::Object(o)) if o.borrow().kind() == ObjectKind::Array
        ))),
        "Array.of" => Ok(array::array_constructor(args)?),
        "Array.from" => array::array_from(args),
        "String.fromCharCode" => string::string_from_char_code(args),
        "String.fromCodePoint" => string::string_from_code_point(args),
        // Fallbacks for the direct-dispatch path; the VM intercepts both before
        // reaching here so that `toJSON`/`replacer`/`reviver` can run.
        "JSON.parse" => {
            let text = args
                .first()
                .map(|v| v.to_js_string())
                .unwrap_or_else(|| "undefined".to_string());
            json::json_parse(&text)
        }
        "JSON.stringify" => match json::json_stringify(args.first().unwrap_or(&Value::Undefined))? {
            Some(text) => Ok(Value::string(&text)),
            None => Ok(Value::Undefined),
        },
        "Error.isError" => Ok(Value::Bool(matches!(
            args.first(),
            Some(Value::Object(o)) if o.borrow().class_name() == "Error"
        ))),
        "Symbol.for" => symbol::symbol_for(args),
        "Symbol.keyFor" => symbol::symbol_key_for(args),
        _ => Err(RuntimeError::TypeError(format!(
            "unknown static method: {name}"
        ))),
    }
}

pub fn call_prototype_method(
    obj: &Value,
    method_name: &str,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    // A TypedArray view: `set` / `subarray` / `slice` / `fill` collide with
    // `Array.prototype`'s, so the receiver has to decide before the shared
    // names are reached. (Registered on the prototype *and* dispatched here —
    // the closure a prototype method carries is never called, only its name is.)
    if typedarray::is_typedarray_receiver(obj) {
        match method_name {
            "set" => return typedarray::ta_set(obj, args),
            "subarray" => return typedarray::ta_subarray(obj, args),
            "slice" => return typedarray::ta_slice(obj, args),
            "fill" => return typedarray::ta_fill(obj, args),
            _ => {}
        }
    }
    if typedarray::is_arraybuffer_receiver(obj) && method_name == "slice" {
        return typedarray::ab_slice(obj, args);
    }
    match method_name {
        // `Number.prototype.toString([radix])` takes the radix argument.
        "toString" if matches!(obj, Value::Number(_)) => {
            number::number_to_string_with_radix(obj, args)
        }
        "toString" => {
            // Date and RegExp both override `toString`, and the name is on every
            // object — so the receiver decides.
            if date::is_date_receiver(obj) {
                date::date_to_string(obj, args)
            } else if regexp::is_regexp_receiver(obj) {
                regexp::regexp_to_string(obj)
            } else {
                dispatch_to_string(obj)
            }
        }
        "match" => regexp::regexp_match(obj, args),
        "search" => regexp::regexp_search(obj, args),
        "test" => regexp::regexp_test(obj, args),
        "exec" => regexp::regexp_exec(obj, args),
        // `Object.prototype.toLocaleString` delegates to `toString`
        // (ES 20.1.3.5); Array/Number inherit the same entry.
        // ES 20.1.3.5: unlike `Object.prototype.toString`, whose prologue
        // special-cases `undefined`/`null` into `[[object Undefined]]` /
        // `[[object Null]]`, `toLocaleString` starts with `ToObject(this)`.
        "toLocaleString" => {
            if date::is_date_receiver(obj) {
                return date::date_to_string(obj, args);
            }
            require_object_coercible(obj)?;
            dispatch_to_string(obj)
        }
        "valueOf" => {
            if date::is_date_receiver(obj) {
                date::date_value_of(obj)
            } else {
                dispatch_value_of(obj)
            }
        }
        "getTime" => date::date_value_of(obj),
        "toISOString" => date::date_to_iso_string(obj),
        "toJSON" => date::date_to_json(obj),
        "toDateString" => date::date_to_date_string(obj),
        "toTimeString" => date::date_to_time_string(obj),
        "toUTCString" => date::date_to_utc_string(obj),
        // No locale data: the `toLocale*` forms are their spec-shaped defaults.
        "toLocaleDateString" => date::date_to_date_string(obj),
        "toLocaleTimeString" => date::date_to_time_string(obj),
        "getFullYear" => date::date_component(obj, 0, false),
        "getMonth" => date::date_component(obj, 1, false),
        "getDate" => date::date_component(obj, 2, false),
        "getDay" => date::date_component(obj, 3, false),
        "getHours" => date::date_component(obj, 4, false),
        "getMinutes" => date::date_component(obj, 5, false),
        "getSeconds" => date::date_component(obj, 6, false),
        "getMilliseconds" => date::date_component(obj, 7, false),
        "getTimezoneOffset" => date::date_timezone_offset(obj),
        "getUTCFullYear" => date::date_component(obj, 0, true),
        "getUTCMonth" => date::date_component(obj, 1, true),
        "getUTCDate" => date::date_component(obj, 2, true),
        "getUTCDay" => date::date_component(obj, 3, true),
        "getUTCHours" => date::date_component(obj, 4, true),
        "getUTCMinutes" => date::date_component(obj, 5, true),
        "getUTCSeconds" => date::date_component(obj, 6, true),
        "setTime" => date::date_set_time(obj, args),
        "setFullYear" => date::date_set_full_year(obj, args),
        "setMonth" => date::date_set_month(obj, args),
        "setDate" => date::date_set_date(obj, args),
        // Map / Set prototype methods are deliberately *not* in this table:
        // `get`, `has`, `delete`, … are ordinary property names on any other
        // object (`{ get: function () { … } }`), and `Map.prototype.has` is not
        // `Set.prototype.has`. They carry a collection-specific dispatch prefix
        // instead (`MAP_METHOD_PREFIX` / `SET_METHOD_PREFIX`), which is what
        // lets `VM::map_method` / `VM::set_method` check the receiver kind.
        "hasOwnProperty" => object::object_has_own_property(obj, args),
        "isPrototypeOf" => object::object_is_prototype_of(obj, args),
        "propertyIsEnumerable" => object::object_property_is_enumerable(obj, args),
        // Number prototype
        "toFixed" => number::number_to_fixed(obj, args),
        "toExponential" => number::number_to_exponential(obj, args),
        "toPrecision" => number::number_to_precision(obj, args),
        // String prototype
        "charAt" => string::string_char_at(obj, args),
        "charCodeAt" => string::string_char_code_at(obj, args),
        "toUpperCase" => string::string_to_upper(obj),
        "toLowerCase" => string::string_to_lower(obj),
        // No locale data is bundled: the default-locale algorithms are the
        // locale-independent ones for the repertoire the suite exercises.
        "toLocaleUpperCase" => string::string_to_upper(obj),
        "toLocaleLowerCase" => string::string_to_lower(obj),
        "trim" => string::string_trim_both(obj),
        "trimStart" => string::string_trim(obj, true),
        "trimEnd" => string::string_trim(obj, false),
        "padStart" => string::string_pad(obj, args, true),
        "padEnd" => string::string_pad(obj, args, false),
        "codePointAt" => string::string_code_point_at(obj, args),
        // `at` exists on both Array.prototype and String.prototype.
        // A RegExp argument routes to the regex-aware implementation
        // (`Symbol.replace` / `Symbol.split` in ES); a plain string stays here.
        // The routing has to happen in the dispatch: the closure passed to
        // `set_prototype_method` is *not* what runs (`_f` is ignored).
        "replace" => {
            if regexp::is_regexp_receiver(args.first().unwrap_or(&Value::Undefined)) {
                regexp::regexp_replace(obj, args)
            } else {
                string::string_replace(obj, args, false)
            }
        }
        "replaceAll" => {
            if regexp::is_regexp_receiver(args.first().unwrap_or(&Value::Undefined)) {
                regexp::regexp_replace(obj, args)
            } else {
                string::string_replace(obj, args, true)
            }
        }
        "flat" => array::array_flat(obj, args),
        "at" => {
            if string_prototype_receiver(obj) {
                string::string_at(obj, args)
            } else {
                array::array_at(obj, args)
            }
        }
        "copyWithin" => array::array_copy_within(obj, args),
        "normalize" => string::string_normalize(obj, args),
        "localeCompare" => string::string_locale_compare(obj, args),
        "split" => {
            if regexp::is_regexp_receiver(args.first().unwrap_or(&Value::Undefined)) {
                regexp::regexp_split(obj, args)
            } else {
                string::string_split(obj, args)
            }
        }
        "substring" => string::string_substring(obj, args),
        "startsWith" => string::string_starts_with(obj, args),
        "endsWith" => string::string_ends_with(obj, args),
        "repeat" => string::string_repeat(obj, args),
        "lastIndexOf" => string::string_last_index_of(obj, args),
        // Dispatched by type
        "concat" => dispatch_concat(obj, args),
        "includes" => dispatch_includes(obj, args),
        "indexOf" => dispatch_index_of(obj, args),
        "slice" => dispatch_slice(obj, args),
        // Array prototype
        "push" => array::array_push(obj, args),
        "pop" => array::array_pop(obj),
        "shift" => array::array_shift(obj),
        "unshift" => array::array_unshift(obj, args),
        "join" => array::array_join(obj, args),
        "splice" => array::array_splice(obj, args),
        "reverse" => array::array_reverse(obj),
        "lastIndexOf" => array::array_last_index_of(obj, args),
        "fill" => array::array_fill(obj, args),
        _ => Err(RuntimeError::TypeError(format!(
            "unknown prototype method: {method_name}"
        ))),
    }
}

/// Names shared by `Array.prototype` and `String.prototype` (`at`, `concat`,
/// `includes`, `indexOf`, `slice`) are dispatched by name, so the receiver has
/// to pick the implementation. A string primitive, or a wrapper around a
/// primitive, always means the String one; every other object is treated as the
/// Array one (its generic path covers array-likes and non-array receivers).
fn string_prototype_receiver(obj: &Value) -> bool {
    matches!(obj, Value::String(_))
        || matches!(obj, Value::Object(o)
            if matches!(o.borrow().kind(), ObjectKind::String | ObjectKind::Number | ObjectKind::Boolean))
}

fn dispatch_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if string_prototype_receiver(obj) {
        string::string_concat(obj, args)
    } else {
        array::array_concat(obj, args)
    }
}

fn dispatch_includes(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if string_prototype_receiver(obj) {
        string::string_includes(obj, args)
    } else {
        array::array_includes(obj, args)
    }
}

fn dispatch_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if string_prototype_receiver(obj) {
        string::string_index_of(obj, args)
    } else {
        array::array_index_of(obj, args)
    }
}

fn dispatch_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    if string_prototype_receiver(obj) {
        string::string_slice(obj, args)
    } else {
        array::array_slice(obj, args)
    }
}

// ─────────────────────────────────────────────────────────
// Internal dispatch helpers — Number
// ─────────────────────────────────────────────────────────

fn dispatch_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let (kind, class_name) = {
                let borrowed = obj_ref.borrow();
                (borrowed.kind(), borrowed.class_name())
            };
            match kind {
                ObjectKind::Array => {
                    if let Some(arr) = obj_ref
                        .borrow()
                        .as_any()
                        .downcast_ref::<crate::vm::object::ArrayObject>()
                    {
                        Ok(Value::string(&arr.join_elements()))
                    } else {
                        Ok(Value::string(&format!("[object {class_name}]")))
                    }
                }
                ObjectKind::Boolean => {
                    // Boolean wrapper — unwrap
                    if let Some(inner) = obj_ref
                        .borrow()
                        .as_any()
                        .downcast_ref::<crate::vm::object::OrdinaryObject>()
                    {
                        if let Some(v) = inner.property_get(&PropertyKey::from_str("__value__")) {
                            return Ok(Value::string(&v.value.to_js_string()));
                        }
                    }
                    Ok(Value::string(&obj.to_js_string()))
                }
                ObjectKind::Number | ObjectKind::String | ObjectKind::Symbol => {
                    // Primitive wrappers stringify to the wrapped primitive.
                    Ok(Value::string(&obj.to_js_string()))
                }
                // `new Error("x").toString()` is "Error: x": an error instance
                // stringifies through `Error.prototype.toString`, which by-name
                // dispatch cannot see (any ordinary object would otherwise fall
                // through to `[object <Class>]`).
                _ if class_name == "Error" => error::error_prototype_to_string(obj),
                _ => Ok(Value::string(&format!("[object {class_name}]"))),
            }
        }
        Value::Bool(b) => Ok(Value::string(&b.to_string())),
        Value::Number(n) => Ok(Value::string(&number_to_string(*n))),
        Value::String(s) => Ok(Value::string(s.as_str())),
        Value::Symbol(sym) => {
            let desc = match &sym.description {
                Some(d) => format!("Symbol({})", d),
                None => "Symbol()".to_string(),
            };
            Ok(Value::string(&desc))
        }
        _ => Ok(Value::string(&obj.to_js_string())),
    }
}

/// ES 7.1.12.1 `Number::toString(x, 10)`.
///
/// The shortest round-trip digits come from Rust's `{:e}` formatting; the
/// choice between fixed and exponential notation is the spec's own rule
/// (exponents below -6 or at/above 21 switch to `e` notation), which plain
/// `f64::to_string` does not follow: `String(1e21)` is `"1e+21"`, and
/// `String(0.0000001)` is `"1e-7"`.
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n == 0.0 {
        // Also covers `-0`.
        return "0".to_string();
    }
    if n < 0.0 {
        return format!("-{}", number_to_string(-n));
    }
    if n.is_infinite() {
        return "Infinity".to_string();
    }

    // `s * 10^(n_es - k)` is exactly `n`, with `k = |s|` minimal.
    let scientific = format!("{n:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("`{:e}` always emits an exponent");
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n_es = exponent + 1;

    if k <= n_es && n_es <= 21 {
        // Integers: the digits, then `n_es - k` zeros.
        let mut out = digits;
        out.push_str(&"0".repeat((n_es - k) as usize));
        out
    } else if 0 < n_es && n_es <= 21 {
        // A decimal point inside the digits.
        let (head, tail) = digits.split_at(n_es as usize);
        format!("{head}.{tail}")
    } else if -6 < n_es && n_es <= 0 {
        // `0.` followed by `-n_es` zeros, then the digits.
        format!("0.{}{}", "0".repeat((-n_es) as usize), digits)
    } else {
        let e = n_es - 1;
        let sign = if e >= 0 { '+' } else { '-' };
        let e_abs = e.abs();
        if k == 1 {
            format!("{digits}e{sign}{e_abs}")
        } else {
            let (head, tail) = digits.split_at(1);
            format!("{head}.{tail}e{sign}{e_abs}")
        }
    }
}


pub fn number_to_string_radix(n: f64, radix: u32) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n.is_sign_positive() {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if radix < 2 || radix > 36 {
        // Default to base 10
        return number_to_string(n);
    }
    if radix == 10 {
        return number_to_string(n);
    }
    let int_part = n.trunc() as i64;
    match int_part.checked_abs() {
        Some(abs) => {
            let sign = if n.is_sign_negative() { "-" } else { "" };
            let converted = radix_format(abs as u64, radix);
            let frac = n.fract().abs();
            if frac > 0.0 {
                format!("{sign}{converted}.{}", radix_format_frac(frac, radix, 8))
            } else {
                format!("{sign}{converted}")
            }
        }
        None => format!("{}", n),
    }
}

fn radix_format(mut n: u64, radix: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let digits = "0123456789abcdefghijklmnopqrstuvwxyz".as_bytes();
    let mut result = Vec::new();
    while n > 0 {
        result.push(digits[(n % radix as u64) as usize]);
        n /= radix as u64;
    }
    result.reverse();
    String::from_utf8(result).unwrap_or_else(|_| "0".to_string())
}

fn radix_format_frac(mut frac: f64, radix: u32, max_digits: usize) -> String {
    let digits = "0123456789abcdefghijklmnopqrstuvwxyz".as_bytes();
    let mut result = Vec::new();
    for _ in 0..max_digits {
        frac *= radix as f64;
        let digit = frac.trunc() as usize;
        result.push(digits[digit.min(35)]);
        frac -= digit as f64;
        if frac.abs() < 1e-12 {
            break;
        }
    }
    String::from_utf8(result).unwrap_or_default()
}

fn dispatch_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    // ES 20.1.3.6: `Object.prototype.valueOf` starts with `ToObject(this)`, so
    // `const valueOf = Object.prototype.valueOf; valueOf()` is a TypeError
    // rather than the receiver echoed back.
    require_object_coercible(obj)?;
    // Primitive wrappers unwrap to the wrapped value; every other object is
    // its own valueOf result.
    if let Value::Object(obj_ref) = obj {
        let kind = obj_ref.borrow().kind();
        if matches!(
            kind,
            ObjectKind::Number | ObjectKind::String | ObjectKind::Boolean | ObjectKind::Symbol
        ) {
            return Ok(obj.to_primitive("default"));
        }
    }
    Ok(obj.clone())
}

// ─────────────────────────────────────────────────────────
// Prototype object backing
// ─────────────────────────────────────────────────────────

fn new_proto(
    parent: Option<Rc<RefCell<dyn JSObject>>>,
    _class_name: &str,
) -> Rc<RefCell<dyn JSObject>> {
    Rc::new(RefCell::new(ProtoObject::new(parent)))
}

#[derive(Debug)]
struct ProtoObject {
    properties: PropertyTable,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
}

impl ProtoObject {
    fn new(prototype: Option<Rc<RefCell<dyn JSObject>>>) -> Self {
        Self {
            properties: PropertyTable::new(),
            prototype,
        }
    }
}

impl JSObject for ProtoObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Ordinary
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        self.properties.get(key).cloned()
    }
    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        self.properties
            .insert(key, PropertyDescriptor::data_descriptor(value));
        Ok(true)
    }
    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        self.properties.insert(key, desc);
        Ok(true)
    }
    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        self.properties.remove(key).is_some()
    }
    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }
    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys()
    }
    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }
    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        self.prototype = proto;
    }
    fn is_extensible(&self) -> bool {
        true
    }
    fn prevent_extensions(&mut self) {}
    fn is_frozen(&self) -> bool {
        false
    }
    fn freeze(&mut self) {}
    fn is_sealed(&self) -> bool {
        false
    }
    fn seal(&mut self) {}
    fn class_name(&self) -> &'static str {
        "Object"
    }
}

// ─────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────

pub fn set_static_method<F>(obj: &Value, name: &str, _f: F)
where
    F: Fn(&[Value]) -> Result<Value, RuntimeError> + 'static,
{
    if let Value::Object(obj_ref) = obj {
        let owner = native_function_name(obj).unwrap_or_default();
        // The stored value is a real function object so `typeof Array.from`
        // reports "function"; the VM dispatches by the qualified name.
        let full = if owner.is_empty() {
            name.to_string()
        } else {
            format!("{owner}.{name}")
        };
        let key = PropertyKey::from_str(name);
        let _ = obj_ref.borrow_mut().define_property(
            key,
            method_descriptor(Value::Object(Rc::new(RefCell::new(
                crate::vm::object::NativeFunctionObject::new(&full),
            )))),
        );
    }
}

/// Prefix used to name prototype methods in `NativeFunctionObject::name`.
///
/// The VM strips it and dispatches to `call_prototype_method`, passing the
/// receiver as the first argument.
/// Property used to mark a class constructor (calling it without `new` throws).
pub const CLASS_CTOR_FLAG: &str = "__isClassCtor__";

pub const PROTO_METHOD_PREFIX: &str = "__proto_method__";

/// Prefixes for the `Map.prototype` / `Set.prototype` methods the VM
/// implements (`has`, `delete`, `clear`, `forEach`, `entries`, …).
///
/// They cannot ride on [`PROTO_METHOD_PREFIX`] even though they answer to the
/// same names: `keys` / `values` / `entries` / `forEach` also live on
/// `Array.prototype`, and the receiver rules disagree —
/// `Array.prototype.keys.call(1)` yields an empty iterator while
/// `Map.prototype.keys.call(1)` is a TypeError. Nor can `Map` and `Set` *share*
/// a prefix: `Set.prototype.has` and `Map.prototype.has` are different methods
/// with the same name, and `Set.prototype.has.call(new Map())` has to be a
/// TypeError — decidable only if the dispatch name says which prototype the
/// method came from.
pub const MAP_METHOD_PREFIX: &str = "__map_method__";
pub const SET_METHOD_PREFIX: &str = "__set_method__";
/// Same idea for the two weak collections (ES 23.3.3 / 23.4.3). They have
/// fewer methods and no iterator, but `get`/`set`/`add`/`has`/`delete` are
/// still names that exist on other objects.
pub const WEAKMAP_METHOD_PREFIX: &str = "__weakmap_method__";
pub const WEAKSET_METHOD_PREFIX: &str = "__weakset_method__";

/// Install the `Symbol.toStringTag` of a collection prototype
/// (`Map.prototype[Symbol.toStringTag] === "Map"`, ES 23.1.3.14 & friends).
pub fn define_string_tag(proto: &Rc<RefCell<dyn JSObject>>, tag: &str) {
    let _ = proto.borrow_mut().define_property(
        to_string_tag_symbol_key(),
        PropertyDescriptor {
            value: Value::string(tag),
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
}

/// Register a VM-implemented prototype method under `prefix`.
///
/// Nothing is stored but the marker function object; the VM does the work (a
/// re-entrant call for `forEach`, the iterator registry for the factories).
pub fn set_vm_method(proto: &Rc<RefCell<dyn JSObject>>, prefix: &str, name: &str) {
    let method_val = Value::Object(Rc::new(RefCell::new(
        crate::vm::object::NativeFunctionObject::new(&format!("{prefix}{name}")),
    )));
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str(name), method_descriptor(method_val));
}

/// [`set_vm_method`] for `Map.prototype`.
pub fn set_map_method(proto: &Rc<RefCell<dyn JSObject>>, name: &str) {
    set_vm_method(proto, MAP_METHOD_PREFIX, name);
}

/// [`set_vm_method`] for `Set.prototype`.
pub fn set_set_method(proto: &Rc<RefCell<dyn JSObject>>, name: &str) {
    set_vm_method(proto, SET_METHOD_PREFIX, name);
}

/// Declared arity (`length`) of a built-in, keyed by the name it is registered
/// under: `"Array"` (constructor), `"Object.keys"` (static), `"push"`
/// (prototype method, registered as `"__proto_method__push"`) or `"Math.atan"`.
///
/// Mirrors the ES clause headings, where optional parameters (brackets) and
/// rest parameters do *not* count towards `length`. `Number.prototype.toString`
/// is the only built-in whose `length` (1) differs from the same-named method
/// elsewhere (0), so it is registered explicitly by `register_number_prototype`.
///
/// Every entry is verified against test262's `**/length.js` tests by
/// `tests/features/builtin_metadata.rs`; a name missing from this table keeps
/// `length === 0`, which is what most predicates want anyway.
pub fn builtin_arity(registered_name: &str) -> usize {
    let name = registered_name
        .strip_prefix(PROTO_METHOD_PREFIX)
        .unwrap_or(registered_name);
    let name = name.strip_prefix(MAP_METHOD_PREFIX).unwrap_or(name);
    let name = name.strip_prefix(SET_METHOD_PREFIX).unwrap_or(name);
    let name = name.strip_prefix(WEAKMAP_METHOD_PREFIX).unwrap_or(name);
    let name = name.strip_prefix(WEAKSET_METHOD_PREFIX).unwrap_or(name);
    match name {
        // ── Constructors ──
        "Object" | "Array" | "Boolean" | "Number" | "String" | "Function" | "Error"
        | "TypeError" | "ReferenceError" | "RangeError" | "SyntaxError" | "URIError"
        | "EvalError" => 1,
        "Error.isError" => 1,
        // ── Proxy ──
        "Proxy" | "Proxy.revocable" => 2,
        // ── Reflect ──
        // Every `Reflect.*` is a plain function of its spec signature: the
        // optional `receiver` argument counts, and the list argument of
        // `apply` / `construct` is one argument, not many.
        "Reflect.apply" => 3,
        "Reflect.construct" => 2,
        "Reflect.defineProperty" | "Reflect.set" => 3,
        "Reflect.deleteProperty"
        | "Reflect.get"
        | "Reflect.getOwnPropertyDescriptor"
        | "Reflect.has"
        | "Reflect.setPrototypeOf" => 2,
        "Reflect.getPrototypeOf"
        | "Reflect.isExtensible"
        | "Reflect.ownKeys"
        | "Reflect.preventExtensions" => 1,
        // ── JSON ──
        "JSON.parse" => 2,
        "JSON.stringify" => 3,
        "Symbol" => 0,
        // ── Global functions ──
        "parseInt" => 2,
        "parseFloat" | "isNaN" | "isFinite" => 1,
        // ── Object ──
        "hasOwnProperty" | "isPrototypeOf" | "propertyIsEnumerable" => 1,
        "Object.keys" | "Object.values" | "Object.entries" | "Object.getPrototypeOf"
        | "Object.getOwnPropertyNames" | "Object.getOwnPropertySymbols"
        | "Object.isExtensible" | "Object.isFrozen" | "Object.isSealed"
        | "Object.preventExtensions" | "Object.seal" | "Object.freeze" => 1,
        "Object.assign" | "Object.create" | "Object.setPrototypeOf"
        | "Object.getOwnPropertyDescriptor" | "Object.hasOwn" | "Object.is" => 2,
        "Object.defineProperties" => 2,
        "Object.defineProperty" => 3,
        // ── Array ──
        "Array.isArray" | "Array.from" => 1,
        "Array.of" => 0,
        "push" | "unshift" | "indexOf" | "includes" | "join" | "concat" | "lastIndexOf"
        | "fill" | "at" | "every" | "some" | "filter" | "map" | "forEach" | "reduce"
        | "reduceRight" | "find" | "findIndex" | "sort" | "flat" | "flatMap"
        | "findLast" | "findLastIndex" => 1,
        "pop" | "shift" | "reverse" | "toString" | "toLocaleString" | "entries" | "keys"
        | "values" | "toReversed" => 0,
        "slice" | "splice" | "copyWithin" | "with" | "toSpliced" => 2,
        // ── String ──
        "String.fromCharCode" | "String.fromCodePoint" | "String.raw" => 1,
        "charAt" | "charCodeAt" | "codePointAt" | "startsWith" | "endsWith" | "repeat"
        | "padStart" | "padEnd" => 1,
        "substring" | "split" | "replace" | "replaceAll" => 2,
        "normalize" | "trim" | "trimStart" | "trimEnd" | "toUpperCase" | "toLowerCase"
        | "toLocaleUpperCase" | "toLocaleLowerCase" | "valueOf" => 0,
        "search" | "match" | "localeCompare" => 1,
        // ── Map (ES 23.1): `Map.length` is 0, and these are the only
        // prototype methods whose arity is not 0 by default ──
        "Map" | "Set" | "WeakMap" | "WeakSet" => 0,
        "Map.groupBy" => 2,
        "get" | "has" | "delete" | "add" => 1,
        // `set-methods` operators each take the set-like argument.
        "union" | "intersection" | "difference" | "symmetricDifference" | "isSubsetOf"
        | "isSupersetOf" | "isDisjointFrom" => 1,
        "set" | "getOrInsert" | "getOrInsertComputed" => 2,
        // ── Function ──
        "call" | "bind" => 1,
        "apply" => 2,
        // ── Number ──
        "Number.isNaN" | "Number.isFinite" | "Number.isInteger" | "Number.isSafeInteger"
        | "Number.parseFloat" => 1,
        "Number.parseInt" => 2,
        "toFixed" | "toExponential" | "toPrecision" => 1,
        // ── Symbol ──
        "Symbol.for" | "Symbol.keyFor" => 1,
        // ── Promise (ES 27.2) ──
        "Promise" => 1,
        "Promise.resolve" | "Promise.reject" | "Promise.all" | "Promise.race"
        | "Promise.allSettled" | "Promise.any" => 1,
        "then" => 2,
        "catch" | "finally" => 1,
        // ── Math ──
        "Math.atan2" | "Math.hypot" | "Math.imul" | "Math.max" | "Math.min" | "Math.pow" => 2,
        "Math.random" => 0,
        m if m.starts_with("Math.") => 1,
        _ => 0,
    }
}

/// Register a prototype method that the VM implements itself.
///
/// The closure is *not* stored: `call_prototype_method` (or the VM, for methods
/// that need to call back into user code) performs the actual dispatch. The
/// closure parameter only documents the intended signature at the call site.
pub fn set_prototype_method<F>(proto: &Rc<RefCell<dyn JSObject>>, name: &str, _f: F)
where
    F: Fn(&Value, &[Value]) -> Result<Value, RuntimeError> + 'static,
{
    let key = PropertyKey::from_str(name);
    let method_val = Value::Object(Rc::new(RefCell::new(
        crate::vm::object::NativeFunctionObject::new(&format!("{PROTO_METHOD_PREFIX}{name}")),
    )));
    let _ = proto.borrow_mut().define_property(key, method_descriptor(method_val));
}

/// Like [`set_prototype_method`], but with an explicit `length`, for the few
/// built-ins whose arity cannot be derived from their name alone
/// (`Number.prototype.toString.length` is 1 while every other `toString` is 0).
pub fn set_prototype_method_arity<F>(
    proto: &Rc<RefCell<dyn JSObject>>,
    name: &str,
    arity: usize,
    _f: F,
) where
    F: Fn(&Value, &[Value]) -> Result<Value, RuntimeError> + 'static,
{
    let mut method =
        crate::vm::object::NativeFunctionObject::new(&format!("{PROTO_METHOD_PREFIX}{name}"));
    method.set_length(arity);
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str(name),
        method_descriptor(Value::Object(Rc::new(RefCell::new(method)))),
    );
}

/// Register a prototype method whose implementation lives in the VM
/// (because it has to invoke user callbacks).
pub fn mark_prototype_method(proto: &Rc<RefCell<dyn JSObject>>, name: &str) {
    set_prototype_method(proto, name, |_, _| {
        Err(RuntimeError::NotImplemented(
            "method implemented by the VM".to_string(),
        ))
    });
}

pub fn is_native_function(val: &Value) -> bool {
    match val {
        Value::Object(obj_ref) => obj_ref.borrow().kind() == ObjectKind::NativeFunction,
        _ => false,
    }
}

/// 是不是原生内建函数 —— **只判种类，不复制名字**（已有实现，调用路径上用它，
/// 免得为了问一句"是不是原生"去 `clone` 一个名字串）。
pub fn native_function_name(val: &Value) -> Option<String> {
    match val {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            if borrowed.kind() == ObjectKind::NativeFunction {
                borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::NativeFunctionObject>()
                    .map(|f| f.name.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

pub use object::{object_keys, object_values};
