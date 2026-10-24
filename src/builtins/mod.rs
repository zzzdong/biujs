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
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::{JSObject, PropertyTable};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

pub use crate::vm::ObjectKind;

pub use array::{
    ARRAY_SPECIES_NATIVE, MAX_MATERIALIZED_LEN, array_constructor, validate_array_length,
};
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
    static WRAPPER_PROTOTYPES: std::cell::RefCell<HashMap<&'static str, Gc<Object>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Make `proto` the prototype of wrappers created by `to_object` for the
/// constructor `name` (`"Number"`, `"String"`, `"Boolean"`, `"Symbol"`,
/// `"Object"`).
pub fn register_wrapper_prototype(name: &'static str, proto: Gc<Object>) {
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

pub fn wrapper_prototype(name: &str) -> Option<Gc<Object>> {
    WRAPPER_PROTOTYPES.with(|map| map.borrow().get(name).cloned())
}

/// ES `ToObject` (7.1.13): primitives are boxed in a wrapper object whose
/// prototype is the corresponding built-in prototype; objects pass through;
/// `undefined`/`null` raise `TypeError`.
pub fn to_object(heap: &mut Heap, value: &Value) -> Result<Value, RuntimeError> {
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
            Ok(Value::Object(heap.alloc(Object::PrimitiveWrapper(
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
fn global_decode_uri(
    heap: &mut Heap,
    args: &[Value],
    _whole_uri: bool,
) -> Result<Value, RuntimeError> {
    let s = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return uri_error(heap, "URI malformed");
            }
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                _ => return uri_error(heap, "URI malformed"),
            }
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    match String::from_utf8(out) {
        Ok(text) => Ok(Value::string(&text)),
        Err(_) => uri_error(heap, "URI malformed"),
    }
}

fn uri_error(heap: &mut Heap, message: &str) -> Result<Value, RuntimeError> {
    let err = error::error_constructor(
        heap,
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
    pub object_prototype: Gc<Object>,
    pub array_prototype: Gc<Object>,
    pub error_prototype: Gc<Object>,
    pub type_error_prototype: Gc<Object>,
    pub reference_error_prototype: Gc<Object>,
    pub range_error_prototype: Gc<Object>,
    pub uri_error_prototype: Gc<Object>,
    pub eval_error_prototype: Gc<Object>,
    pub syntax_error_prototype: Gc<Object>,
    pub boolean_prototype: Gc<Object>,
    pub number_prototype: Gc<Object>,
    pub string_prototype: Gc<Object>,
    pub function_prototype: Gc<Object>,
    pub symbol_prototype: Gc<Object>,
    pub map_prototype: Gc<Object>,
    pub set_prototype: Gc<Object>,
    pub weakmap_prototype: Gc<Object>,
    pub weakset_prototype: Gc<Object>,
    pub date_prototype: Gc<Object>,
    pub regexp_prototype: Gc<Object>,
    pub promise_prototype: Gc<Object>,
    // 这两个也曾缺席过那份手工断环清单（`DataView` 漏了 ⇒ 每条 test262 用例漏一整个
    // realm ⇒ 撞穿 4 GB `ulimit`、进程 SIGABRT）。现在环由堆接管 —— 它们都活在同一个
    // `Heap` 里，随 VM 析构整片释放，清单已删除。
    pub arraybuffer_prototype: Gc<Object>,
    pub dataview_prototype: Gc<Object>,
}

/// 内建对象是根：它们构成整个 realm 的属性图起点。
///
/// 23 个 prototype 字段每一项都必须列到 —— 漏一个就是静默 UAF（INV-3）。
impl crate::vm::gc::Trace for Builtins {
    fn trace(&self, out: &mut crate::vm::gc::Worklist) {
        self.object_prototype.trace(out);
        self.array_prototype.trace(out);
        self.error_prototype.trace(out);
        self.type_error_prototype.trace(out);
        self.reference_error_prototype.trace(out);
        self.range_error_prototype.trace(out);
        self.uri_error_prototype.trace(out);
        self.eval_error_prototype.trace(out);
        self.syntax_error_prototype.trace(out);
        self.boolean_prototype.trace(out);
        self.number_prototype.trace(out);
        self.string_prototype.trace(out);
        self.function_prototype.trace(out);
        self.symbol_prototype.trace(out);
        self.map_prototype.trace(out);
        self.set_prototype.trace(out);
        self.weakmap_prototype.trace(out);
        self.weakset_prototype.trace(out);
        self.date_prototype.trace(out);
        self.regexp_prototype.trace(out);
        self.promise_prototype.trace(out);
        self.arraybuffer_prototype.trace(out);
        self.dataview_prototype.trace(out);
    }
}

impl crate::vm::gc::Trace for ProtoObject {
    fn trace(&self, out: &mut crate::vm::gc::Worklist) {
        self.prototype.trace(out);
        self.properties.trace(out);
    }
}

impl Builtins {
    pub fn new(heap: &mut Heap) -> Self {
        let object_proto = new_proto(heap, None, "Object");
        let array_proto = new_proto(heap, Some(object_proto), "Array");
        let error_proto = new_proto(heap, Some(object_proto), "Error");
        let type_error_proto = new_proto(heap, Some(error_proto), "TypeError");
        let reference_error_proto = new_proto(heap, Some(error_proto), "ReferenceError");
        let range_error_proto = new_proto(heap, Some(error_proto), "RangeError");
        let uri_error_proto = new_proto(heap, Some(error_proto), "URIError");
        let eval_error_proto = new_proto(heap, Some(error_proto), "EvalError");
        let syntax_error_proto = new_proto(heap, Some(error_proto), "SyntaxError");
        let boolean_proto = new_proto(heap, Some(object_proto), "Boolean");
        let number_proto = new_proto(heap, Some(object_proto), "Number");
        let string_proto = new_proto(heap, Some(object_proto), "String");
        let function_proto = new_proto(heap, Some(object_proto), "Function");
        let symbol_proto = new_proto(heap, Some(object_proto), "Symbol");
        let map_proto = new_proto(heap, Some(object_proto), "Map");
        let set_proto = new_proto(heap, Some(object_proto), "Set");
        let weakmap_proto = new_proto(heap, Some(object_proto), "WeakMap");
        let weakset_proto = new_proto(heap, Some(object_proto), "WeakSet");
        let date_proto = new_proto(heap, Some(object_proto), "Date");
        let regexp_proto = new_proto(heap, Some(object_proto), "RegExp");
        let promise_proto = new_proto(heap, Some(object_proto), "Promise");
        // Empty prototypes created up front and stored as fields so `teardown` can
        // break their `prototype ⇄ constructor` cycle. (See the comment on the
        // `arraybuffer_prototype` field.)
        let arraybuffer_proto = new_proto(heap, Some(object_proto), "ArrayBuffer");
        let dataview_proto = new_proto(heap, Some(object_proto), "DataView");

        // Needed before any built-in function object is created: every one of
        // them inherits from `Function.prototype`.
        register_wrapper_prototype("Function", function_proto);

        // Set up Function.prototype with standard methods
        setup_function_prototype(&function_proto, heap);

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
            arraybuffer_prototype: arraybuffer_proto,
            dataview_prototype: dataview_proto,
        }
    }

    /// Wire up the two sides of a constructor/prototype pair.
    ///
    /// `with_prototype` only sets the constructor's `[[Prototype]]` slot, so
    /// both cross-references have to be installed explicitly:
    /// `C.prototype === proto` (needed by `Object.prototype.toString.call(x)`)
    /// and `proto.constructor === C` (needed by `assert.throws`, which compares
    /// a caught error's `constructor` against `TypeError` and friends).
    fn link_constructor_prototype(fn_val: &Value, proto: &Gc<Object>) {
        if let Value::Object(obj_ref) = fn_val {
            let _ = obj_ref.borrow_mut().define_property(
                PropertyKey::from_str("prototype"),
                constructor_prototype_descriptor(Value::Object(*proto)),
            );
        }
        let _ = proto.borrow_mut().define_property(
            PropertyKey::from_str("constructor"),
            method_descriptor(fn_val.clone()),
        );
    }

    pub fn register(&self, globals: &mut HashMap<String, Value>, heap: &mut Heap) {
        use crate::vm::object::NativeFunctionObject;

        let object_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Object"),
        )));
        object::register_object_statics(&object_fn_val, self, heap);
        Self::link_constructor_prototype(&object_fn_val, &self.object_prototype);
        object::register_object_prototype(&self.object_prototype, &object_fn_val, heap);
        globals.insert("Object".to_string(), object_fn_val);

        let array_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Array"),
        )));
        array::register_array_prototype(&self.array_prototype, heap);
        array::register_array_statics(&array_fn_val, heap);
        Self::link_constructor_prototype(&array_fn_val, &self.array_prototype);
        globals.insert("Array".to_string(), array_fn_val);

        // Error constructors with prototype property setup
        let error_constructors = [
            ("Error", self.error_prototype),
            ("TypeError", self.type_error_prototype),
            ("ReferenceError", self.reference_error_prototype),
            ("RangeError", self.range_error_prototype),
            ("URIError", self.uri_error_prototype),
            ("EvalError", self.eval_error_prototype),
            ("SyntaxError", self.syntax_error_prototype),
        ];

        for (name, proto) in error_constructors {
            let fn_val = Value::Object(heap.alloc(Object::NativeFunction(
                NativeFunctionObject::new(name),
            )));

            Self::link_constructor_prototype(&fn_val, &proto);
            if name == "Error" {
                // `Error.isError(value)`: true for objects carrying
                // `[[ErrorData]]`, which the engine marks with the `[[Class]]`
                // "Error" (that is also what `Object.prototype.toString`
                // reports).
                set_static_method(&fn_val, "isError", heap, |args| {
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
        error::setup_error_prototype(&self.error_prototype, self, heap);
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
        let string_iterator_fn = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(crate::vm::iterator::ITERATOR_NATIVE_NAME),
        )));
        let _ = self.string_prototype.borrow_mut().define_property(
            crate::vm::iterator::iterator_symbol_key(),
            method_descriptor(string_iterator_fn),
        );

        let bool_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Boolean"),
        )));
        boolean::register_boolean_prototype(&self.boolean_prototype, heap);
        Self::link_constructor_prototype(&bool_fn_val, &self.boolean_prototype);
        globals.insert("Boolean".to_string(), bool_fn_val);

        let number_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Number"),
        )));
        number::register_number_statics(&number_fn_val, &self.number_prototype, heap);
        number::register_number_prototype(&self.number_prototype, heap);
        Self::link_constructor_prototype(&number_fn_val, &self.number_prototype);
        globals.insert("Number".to_string(), number_fn_val);

        let string_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("String"),
        )));
        string::register_string_prototype(&self.string_prototype, heap);
        string::register_string_statics(&string_fn_val, heap);
        Self::link_constructor_prototype(&string_fn_val, &self.string_prototype);
        globals.insert("String".to_string(), string_fn_val);

        let function_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Function"),
        )));
        function::register_function_statics(&function_fn_val, &self.function_prototype, heap);
        Self::link_constructor_prototype(&function_fn_val, &self.function_prototype);
        globals.insert("Function".to_string(), function_fn_val);

        // Symbol constructor
        let symbol_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Symbol"),
        )));
        symbol::register_symbol_statics(&symbol_fn_val, &self.symbol_prototype, heap);
        symbol::register_symbol_prototype(&self.symbol_prototype, heap);
        Self::link_constructor_prototype(&symbol_fn_val, &self.symbol_prototype);

        // `ArrayBuffer` and the TypedArray views (ES 25.1 / 23.2). The
        // constructors are dispatched by the VM: `new Uint8Array(buf)` builds a
        // *view* over an existing buffer, and `new Uint8Array(4)` one over a fresh
        // one — the prototype comes from `newTarget`, so the builtin layer gets it
        // handed in.
        let ab_proto = self.arraybuffer_prototype;
        let ab_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("ArrayBuffer"),
        )));
        typedarray::register_arraybuffer_prototype(&ab_proto, heap);
        Self::link_constructor_prototype(&ab_fn_val, &ab_proto);
        set_static_method(&ab_fn_val, "isView", heap, |args| {
            typedarray::arraybuffer_is_view(args)
        });
        globals.insert("ArrayBuffer".to_string(), ab_fn_val);

        // `DataView` (ES 25.3): a byte-addressable view over an `ArrayBuffer`.
        let dv_proto = self.dataview_prototype;
        let dv_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("DataView"),
        )));
        typedarray::register_dataview_prototype(&dv_proto, heap);
        Self::link_constructor_prototype(&dv_fn_val, &dv_proto);
        globals.insert("DataView".to_string(), dv_fn_val);

        // ── `%TypedArray%`（ES 22.2.1）──
        //
        // 八个视图构造器的 `[[Prototype]]`，其 `.prototype` 承载全部共享方法。
        // **它是内建，不是全局变量** —— ES 里没有 `TypedArray` 这个全局；test262
        // 通过 `Object.getPrototypeOf(Uint8Array)` 取到它
        // （`harness/testTypedArray.js` 就是这么定义的）。
        //
        // 以前这个基类根本不存在：每个视图 prototype 直接挂 `Object.prototype`，
        // 共享方法在八个 prototype 上各注册一份。于是 `built-ins/TypedArray` 整批
        // 用例拿不到 `%TypedArray%`（实测 882 条失败，`Symbol.species/*` 报
        // "Cannot read properties of undefined (reading 'get')"）。
        let ta_proto = new_proto(heap, Some(self.object_prototype), "TypedArray");
        // 共享方法在基类上注册一份；具体 prototype 仍各自持有一份（历史行为，
        // 与 `%TypedArray%.prototype` 上的重复，留待去重）。
        typedarray::register_typedarray_prototype(&ta_proto, heap);
        let ta_ctor = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("TypedArray"),
        )));
        Self::link_constructor_prototype(&ta_ctor, &ta_proto);
        // `%TypedArray%.from` / `%TypedArray%.of`（22.2.2.1 / 22.2.2.2）：要迭代、
        // 可能回调、还要 `new C(len)`，只有 VM 能做 —— 故登记在这里、由 VM 分派
        // （限定名 "TypedArray.from" / "TypedArray.of"）。八个具体构造器经
        // `[[Prototype]]` 继承即可，不必各自再登记一份。
        set_static_method(&ta_ctor, "from", heap, |_args| {
            Err(RuntimeError::TypeError(
                "TypedArray.from is dispatched by the VM".to_string(),
            ))
        });
        set_static_method(&ta_ctor, "of", heap, |_args| {
            Err(RuntimeError::TypeError(
                "TypedArray.of is dispatched by the VM".to_string(),
            ))
        });
        let ta_ctor_gc = match &ta_ctor {
            Value::Object(g) => *g,
            _ => unreachable!("刚分配的构造器"),
        };
        if let Value::Object(ta_obj) = &ta_ctor {
            // `%TypedArray%[Symbol.species]`（22.2.2.4）：返回接收者的取值器，
            // `length` 0、`configurable` true、`set` 为 undefined。
            let species_getter = Value::Object(heap.alloc(Object::NativeFunction(
                NativeFunctionObject::new(typedarray::TA_SPECIES_NATIVE),
            )));
            let _ = ta_obj.borrow_mut().define_property(
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

        for kind in typedarray::TYPED_ARRAY_KINDS {
            let name = kind.name();
            // 具体 prototype 继承 `%TypedArray%.prototype`，而非 `Object.prototype`。
            let proto = new_proto(heap, Some(ta_proto), name);
            let ctor_val = Value::Object(heap.alloc(Object::NativeFunction(
                NativeFunctionObject::new(name),
            )));
            // 构造器的 `[[Prototype]]` 指向 `%TypedArray%` —— `Object
            // .getPrototypeOf(Uint8Array)` 必须拿到它。
            if let Value::Object(ctor_gc) = &ctor_val {
                ctor_gc.borrow_mut().set_prototype(Some(ta_ctor_gc));
            }
            // 共享方法**不再**在这里注册 —— 它们在 `%TypedArray%.prototype` 上，
            // 具体 prototype 经原型链继承。这里只放与类型有关的 `BYTES_PER_ELEMENT`。
            typedarray::register_typedarray_bytes_per_element(kind, &proto);
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
                // `Uint8Array[Symbol.species]` (ES 23.2.2.2)：返回接收者的取值器。
                // `slice` / `subarray` / `map` 都要查它，缺了就整批用例失败
                // （实测 TypedArray 套件的 Symbol.species/* 全败，报
                // "Cannot read properties of undefined (reading 'get')"）。
                let species_getter = Value::Object(heap.alloc(Object::NativeFunction(
                    NativeFunctionObject::new(typedarray::TA_SPECIES_NATIVE),
                )));
                let _ = ctor_obj.borrow_mut().define_property(
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
            globals.insert(name.to_string(), ctor_val);
        }

        // `Map` (ES 23.1). The constructor itself is dispatched by the VM:
        // `new Map(iterable)` has to drive the iterator protocol, and calling
        // it without `new` is a TypeError.
        let map_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Map"),
        )));
        map::register_map_prototype(&self.map_prototype, heap);
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
        set_static_method(&map_fn_val, "groupBy", heap, |_args| {
            Err(RuntimeError::TypeError(
                "Map.groupBy is dispatched by the VM".to_string(),
            ))
        });
        register_wrapper_prototype("Map", self.map_prototype);
        // `Map[Symbol.species]` is an accessor returning its receiver
        // (ES 23.1.2.2); like `Array`'s, the getter is dispatched by the VM.
        let species_getter = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(map::MAP_SPECIES_NATIVE),
        )));
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
        let set_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Set"),
        )));
        set::register_set_prototype(&self.set_prototype, heap);
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
        register_wrapper_prototype("Set", self.set_prototype);
        // `Set[Symbol.species]` (ES 23.2.2.2), mirroring `Map`'s accessor.
        let set_species_getter = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(set::SET_SPECIES_NATIVE),
        )));
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
        let weakmap_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("WeakMap"),
        )));
        weak::register_weakmap_prototype(&self.weakmap_prototype, heap);
        Self::link_constructor_prototype(&weakmap_fn_val, &self.weakmap_prototype);
        register_wrapper_prototype("WeakMap", self.weakmap_prototype);
        globals.insert("WeakMap".to_string(), weakmap_fn_val);

        let weakset_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("WeakSet"),
        )));
        weak::register_weakset_prototype(&self.weakset_prototype, heap);
        Self::link_constructor_prototype(&weakset_fn_val, &self.weakset_prototype);
        register_wrapper_prototype("WeakSet", self.weakset_prototype);
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
        globals.insert("Math".to_string(), math::create_math_object(heap));
        // `console` — a host object, not part of ES, but the one thing a script
        // cannot do without: without it the only way to observe a value is to
        // `throw` it.
        globals.insert("console".to_string(), console::create_console_object(heap));
        // `Reflect` (ES 28.1) — the internal methods as functions. Registered
        // before `Proxy` exists on purpose: a proxy handler *is* this list, and
        // building it first is what keeps the proxy work from re-inventing it.
        globals.insert("Reflect".to_string(), reflect::create_reflect_object(heap));
        // `Proxy` (ES 28.2): the exotic object is inert until something operates on
        // it, and every operation is a trap lookup — so the constructor only checks
        // its two arguments and the VM does the rest (`new Proxy(t, h)` and
        // `Proxy.revocable`). No `prototype`: instances inherit from the target.
        globals.insert("Proxy".to_string(), proxy::create_proxy_constructor(heap));

        // `Date`: everyday scripts need wall-clock time. `new Date(...)` builds
        // the object in the VM (`Date()` without `new` answers a *string*, so
        // the two cannot share `call_native`).
        let date_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Date"),
        )));
        date::register_date_prototype(&self.date_prototype, heap);
        date::register_date_statics(&date_fn_val, heap);
        Self::link_constructor_prototype(&date_fn_val, &self.date_prototype);
        register_wrapper_prototype("Date", self.date_prototype);
        globals.insert("Date".to_string(), date_fn_val);

        // `RegExp`: the matcher lives in `vm::object::RegExpObject` (a
        // `regex::Regex`). `RegExp(...)` answers an object whether or not it is
        // called with `new`, so `call_native` can serve both paths.
        let regexp_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("RegExp"),
        )));
        regexp::register_regexp_prototype(&self.regexp_prototype, heap);
        Self::link_constructor_prototype(&regexp_fn_val, &self.regexp_prototype);
        register_wrapper_prototype("RegExp", self.regexp_prototype);
        globals.insert("RegExp".to_string(), regexp_fn_val);

        // `Promise`: the object lives in `vm::object::PromiseObject`, and the
        // methods that must *call* JS (`then`, `catch`) are dispatched by the
        // VM; the registration below is what makes the names resolvable.
        let promise_fn_val = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new("Promise"),
        )));
        promise::register_promise_prototype(&self.promise_prototype, heap);
        promise::register_promise_statics(&promise_fn_val, heap);
        // `Promise.prototype[Symbol.toStringTag] === "Promise"` (ES 27.2.5.5).
        define_string_tag(&self.promise_prototype, "Promise");
        // `Promise[Symbol.species]` (ES 27.2.2.3) is an accessor returning its
        // receiver, mirroring `Map`/`Set`/`Array`; the VM answers the getter.
        let promise_species_getter = Value::Object(heap.alloc(Object::NativeFunction(
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
        register_wrapper_prototype("Promise", self.promise_prototype);
        globals.insert("Promise".to_string(), promise_fn_val);
        globals.insert("JSON".to_string(), json::register_json(heap));

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
                Value::Object(heap.alloc(Object::NativeFunction(NativeFunctionObject::new(name)))),
            );
        }

        // Prototype registry for `Object(value)` / ToObject boxing, and for the
        // array objects the engine creates outside the `New` prologue.
        register_wrapper_prototype("Array", self.array_prototype);
        register_wrapper_prototype("Object", self.object_prototype);
        register_wrapper_prototype("Boolean", self.boolean_prototype);
        register_wrapper_prototype("Number", self.number_prototype);
        register_wrapper_prototype("String", self.string_prototype);
        register_wrapper_prototype("Symbol", self.symbol_prototype);
        // Error prototypes: `Error(msg)` without `new` still needs the right
        // `[[Prototype]]` on the object it returns.
        register_wrapper_prototype("Error", self.error_prototype);
        register_wrapper_prototype("TypeError", self.type_error_prototype);
        register_wrapper_prototype("ReferenceError", self.reference_error_prototype);
        register_wrapper_prototype("RangeError", self.range_error_prototype);
        register_wrapper_prototype("SyntaxError", self.syntax_error_prototype);
        register_wrapper_prototype("URIError", self.uri_error_prototype);
        register_wrapper_prototype("EvalError", self.eval_error_prototype);
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

pub fn call_native(name: &str, args: &[Value], heap: &mut Heap) -> Result<Value, RuntimeError> {
    match name {
        "Object" => object::object_constructor(heap, args),
        "Array" => array::array_constructor(heap, args),
        "Error" => error::error_constructor(heap, error::ErrorType::Error, args),
        "TypeError" => error::error_constructor(heap, error::ErrorType::TypeError, args),
        "ReferenceError" => error::error_constructor(heap, error::ErrorType::ReferenceError, args),
        "RangeError" => error::error_constructor(heap, error::ErrorType::RangeError, args),
        "SyntaxError" => error::error_constructor(heap, error::ErrorType::SyntaxError, args),
        "URIError" => error::error_constructor(heap, error::ErrorType::URIError, args),
        "EvalError" => error::error_constructor(heap, error::ErrorType::EvalError, args),
        // `Date()` without `new` returns a *string* (ES 21.4.2.1); `new Date()`
        // goes through `VM::date_construct` instead.
        "Date" => date::date_as_function(heap, args),
        "RegExp" => regexp::regexp_construct_value(heap, args, None),
        // `Promise(...)` without `new` is a TypeError (ES 27.2.1.1 step 1);
        // `[[Construct]]` is `VM::promise_construct`.
        "Promise" => Err(RuntimeError::TypeError(
            "Constructor Promise requires 'new'".to_string(),
        )),
        "parseInt" => global_parse_int(args),
        "encodeURIComponent" => global_encode_uri(args, false),
        "encodeURI" => global_encode_uri(args, true),
        "decodeURIComponent" => global_decode_uri(heap, args, false),
        "decodeURI" => global_decode_uri(heap, args, true),
        "parseFloat" => global_parse_float(args),
        "Boolean" => boolean::boolean_constructor(args),
        "Number" => number::number_constructor(args),
        "String" => string::string_constructor(args),
        "Function" => function::function_constructor(heap, args),
        "Symbol" => symbol::symbol_constructor(heap, args),
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

pub fn call_static_method(
    name: &str,
    args: &[Value],
    heap: &mut Heap,
) -> Result<Value, RuntimeError> {
    // `Math` is a namespace object, so its methods are dispatched by prefix.
    if let Some(method) = name.strip_prefix("Math.") {
        return math::call_math_method(method, args);
    }
    // `console` is dispatched the same way (see `console.rs` for why it exists).
    if let Some(method) = name.strip_prefix("console.") {
        return console::call_console_method(method, args, heap);
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
        "Reflect.has" => reflect::reflect_has(heap, args),
        "Reflect.deleteProperty" => reflect::reflect_delete_property(heap, args),
        "Reflect.ownKeys" => reflect::reflect_own_keys(heap, args),
        "Reflect.getOwnPropertyDescriptor" => {
            reflect::reflect_get_own_property_descriptor(heap, args)
        }
        "Reflect.getPrototypeOf" => reflect::reflect_get_prototype_of(heap, args),
        "Reflect.setPrototypeOf" => reflect::reflect_set_prototype_of(heap, args),
        "Reflect.isExtensible" => reflect::reflect_is_extensible(heap, args),
        "Reflect.preventExtensions" => reflect::reflect_prevent_extensions(heap, args),
        "Date.now" => date::date_now(args),
        "Date.parse" => date::date_parse(args),
        "Date.UTC" => date::date_utc(args),
        "Object.fromEntries" => object::object_from_entries(heap, args),
        "Object.keys" => object::object_keys(heap, args),
        "Object.values" => object::object_values(heap, args),
        "Object.entries" => object::object_entries(heap, args),
        "Object.assign" => object::object_assign(heap, args),
        "Object.defineProperty" => object::object_define_property(args),
        "Object.defineProperties" => object::object_define_properties(args),
        "Object.create" => object::object_create(heap, args),
        "Object.getPrototypeOf" => object::object_get_prototype_of(heap, args),
        "Object.setPrototypeOf" => object::object_set_prototype_of(args),
        "Object.getOwnPropertyNames" => object::object_get_own_property_names(heap, args),
        "Object.getOwnPropertySymbols" => object::object_get_own_property_symbols(heap, args),
        "Object.getOwnPropertyDescriptor" => {
            object::object_get_own_property_descriptor(heap, args)
        }
        "Object.hasOwn" => object::object_has_own(heap, args),
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
        "Array.of" => Ok(array::array_constructor(heap, args)?),
        "Array.from" => array::array_from(heap, args),
        "String.fromCharCode" => string::string_from_char_code(args),
        "String.fromCodePoint" => string::string_from_code_point(args),
        // Fallbacks for the direct-dispatch path; the VM intercepts both before
        // reaching here so that `toJSON`/`replacer`/`reviver` can run.
        "JSON.parse" => {
            let text = args
                .first()
                .map(|v| v.to_js_string())
                .unwrap_or_else(|| "undefined".to_string());
            json::json_parse(heap, &text)
        }
        "JSON.stringify" => match json::json_stringify(heap, args.first().unwrap_or(&Value::Undefined))? {
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
    heap: &mut Heap,
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
            "subarray" => return typedarray::ta_subarray(heap, obj, args),
            "slice" => return typedarray::ta_slice(heap, obj, args),
            "fill" => return typedarray::ta_fill(obj, args),
            // 只读 / 原地的一族：`Array.prototype` 有同名方法，故必须由接收者先判定。
            "indexOf" => return typedarray::ta_index_of(obj, args),
            "lastIndexOf" => return typedarray::ta_last_index_of(obj, args),
            "includes" => return typedarray::ta_includes(obj, args),
            "join" => return typedarray::ta_join(obj, args),
            "reverse" => return typedarray::ta_reverse(obj, args),
            "at" => return typedarray::ta_at(obj, args),
            "copyWithin" => return typedarray::ta_copy_within(obj, args),
            // `toString` / `toLocaleString` 都是 join(",")（23.2.3.37 / .38）。
            "toString" | "toLocaleString" => return typedarray::ta_join(obj, args),
            _ => {}
        }
    }
    if typedarray::is_arraybuffer_receiver(obj) && method_name == "slice" {
        return typedarray::ab_slice(heap, obj, args);
    }
    if typedarray::is_dataview_receiver(obj) {
        use typedarray::DataViewType::*;
        // Only the 8 `get*` / 8 `set*` are DataView-specific; every other method
        // (`hasOwnProperty`, `toString`, `valueOf`, …) is inherited from
        // `Object.prototype` and must fall through to the generic dispatch below.
        let handled = match method_name {
            "getInt8" => Some(typedarray::dv_get(obj, args, Int8)),
            "getUint8" => Some(typedarray::dv_get(obj, args, Uint8)),
            "getInt16" => Some(typedarray::dv_get(obj, args, Int16)),
            "getUint16" => Some(typedarray::dv_get(obj, args, Uint16)),
            "getInt32" => Some(typedarray::dv_get(obj, args, Int32)),
            "getUint32" => Some(typedarray::dv_get(obj, args, Uint32)),
            "getFloat32" => Some(typedarray::dv_get(obj, args, Float32)),
            "getFloat64" => Some(typedarray::dv_get(obj, args, Float64)),
            "setInt8" => Some(typedarray::dv_set(obj, args, Int8)),
            "setUint8" => Some(typedarray::dv_set(obj, args, Uint8)),
            "setInt16" => Some(typedarray::dv_set(obj, args, Int16)),
            "setUint16" => Some(typedarray::dv_set(obj, args, Uint16)),
            "setInt32" => Some(typedarray::dv_set(obj, args, Int32)),
            "setUint32" => Some(typedarray::dv_set(obj, args, Uint32)),
            "setFloat32" => Some(typedarray::dv_set(obj, args, Float32)),
            "setFloat64" => Some(typedarray::dv_set(obj, args, Float64)),
            _ => None,
        };
        if let Some(result) = handled {
            return result;
        }
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
        "match" => regexp::regexp_match(heap, obj, args),
        "search" => regexp::regexp_search(heap, obj, args),
        "test" => regexp::regexp_test(obj, args),
        "exec" => regexp::regexp_exec(heap, obj, args),
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
        // ES B.2.2.1 / B.2.2.2：需要装箱原语接收者，故带 heap。
        "__lookupGetter__" => object::object_lookup_accessor(heap, obj, args, true),
        "__lookupSetter__" => object::object_lookup_accessor(heap, obj, args, false),
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
                regexp::regexp_replace(heap, obj, args)
            } else {
                string::string_replace(obj, args, false)
            }
        }
        "replaceAll" => {
            if regexp::is_regexp_receiver(args.first().unwrap_or(&Value::Undefined)) {
                regexp::regexp_replace(heap, obj, args)
            } else {
                string::string_replace(obj, args, true)
            }
        }
        "flat" => array::array_flat(heap, obj, args),
        "at" => {
            if string_prototype_receiver(obj) {
                string::string_at(obj, args)
            } else {
                array::array_at(heap, obj, args)
            }
        }
        "copyWithin" => array::array_copy_within(obj, args),
        "normalize" => string::string_normalize(obj, args),
        "localeCompare" => string::string_locale_compare(obj, args),
        "split" => {
            if regexp::is_regexp_receiver(args.first().unwrap_or(&Value::Undefined)) {
                regexp::regexp_split(heap, obj, args)
            } else {
                string::string_split(heap, obj, args)
            }
        }
        "substring" => string::string_substring(obj, args),
        "startsWith" => string::string_starts_with(obj, args),
        "endsWith" => string::string_ends_with(obj, args),
        "repeat" => string::string_repeat(obj, args),
        "lastIndexOf" => string::string_last_index_of(obj, args),
        // Dispatched by type
        "concat" => dispatch_concat(heap, obj, args),
        "includes" => dispatch_includes(obj, args),
        "indexOf" => dispatch_index_of(obj, args),
        "slice" => dispatch_slice(heap, obj, args),
        // Array prototype
        "push" => array::array_push(heap, obj, args),
        "pop" => array::array_pop(heap, obj),
        "shift" => array::array_shift(heap, obj),
        "unshift" => array::array_unshift(heap, obj, args),
        "join" => array::array_join(heap, obj, args),
        "splice" => array::array_splice(heap, obj, args),
        "reverse" => array::array_reverse(heap, obj),
        "lastIndexOf" => array::array_last_index_of(obj, args),
        "fill" => array::array_fill(heap, obj, args),
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

fn dispatch_concat(
    heap: &mut Heap,
    obj: &Value,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if string_prototype_receiver(obj) {
        string::string_concat(obj, args)
    } else {
        array::array_concat(heap, obj, args)
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

fn dispatch_slice(
    heap: &mut Heap,
    obj: &Value,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if string_prototype_receiver(obj) {
        string::string_slice(obj, args)
    } else {
        array::array_slice(heap, obj, args)
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
                        .borrow().as_array()
                    {
                        Ok(Value::string(&arr.join_elements()))
                    } else {
                        Ok(Value::string(&format!("[object {class_name}]")))
                    }
                }
                ObjectKind::Boolean => {
                    // Boolean wrapper — unwrap
                    if let Some(inner) = obj_ref
                        .borrow().as_ordinary()
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

fn new_proto(heap: &mut Heap, parent: Option<Gc<Object>>, _class_name: &str) -> Gc<Object> {
    heap.alloc(Object::Proto(ProtoObject::new(parent)))
}

#[derive(Debug)]
pub(crate) struct ProtoObject {
    properties: PropertyTable,
    prototype: Option<Gc<Object>>,
}

impl ProtoObject {
    pub(crate) fn new(prototype: Option<Gc<Object>>) -> Self {
        Self {
            properties: PropertyTable::new(),
            prototype,
        }
    }
}

impl JSObject for ProtoObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Ordinary
    }    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
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
    fn get_prototype(&self) -> Option<Gc<Object>> {
        self.prototype.clone()
    }
    fn set_prototype(&mut self, proto: Option<Gc<Object>>) {
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

pub fn set_static_method<F>(obj: &Value, name: &str, heap: &mut Heap, _f: F)
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
            method_descriptor(Value::Object(heap.alloc(Object::NativeFunction(
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
pub fn define_string_tag(proto: &Gc<Object>, tag: &str) {
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
pub fn set_vm_method(proto: &Gc<Object>, heap: &mut Heap, prefix: &str, name: &str) {
    let method_val = Value::Object(heap.alloc(Object::NativeFunction(
        crate::vm::object::NativeFunctionObject::new(&format!("{prefix}{name}")),
    )));
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str(name), method_descriptor(method_val));
}

/// [`set_vm_method`] for `Map.prototype`.
pub fn set_map_method(proto: &Gc<Object>, heap: &mut Heap, name: &str) {
    set_vm_method(proto, heap, MAP_METHOD_PREFIX, name);
}

/// [`set_vm_method`] for `Set.prototype`.
pub fn set_set_method(proto: &Gc<Object>, heap: &mut Heap, name: &str) {
    set_vm_method(proto, heap, SET_METHOD_PREFIX, name);
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
        // ES B.2.2.1 / B.2.2.2：`__lookupGetter__(key)` 收一个键。
        "__lookupGetter__" | "__lookupSetter__" => 1,
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
pub fn set_prototype_method<F>(proto: &Gc<Object>, heap: &mut Heap, name: &str, _f: F)
where
    F: Fn(&Value, &[Value]) -> Result<Value, RuntimeError> + 'static,
{
    let key = PropertyKey::from_str(name);
    let method_val = Value::Object(heap.alloc(Object::NativeFunction(
        crate::vm::object::NativeFunctionObject::new(&format!("{PROTO_METHOD_PREFIX}{name}")),
    )));
    let _ = proto.borrow_mut().define_property(key, method_descriptor(method_val));
}

/// Like [`set_prototype_method`], but with an explicit `length`, for the few
/// built-ins whose arity cannot be derived from their name alone
/// (`Number.prototype.toString.length` is 1 while every other `toString` is 0).
pub fn set_prototype_method_arity<F>(
    proto: &Gc<Object>,
    heap: &mut Heap,
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
        method_descriptor(Value::Object(heap.alloc(Object::NativeFunction(method)))),
    );
}

/// Register a prototype method whose implementation lives in the VM
/// (because it has to invoke user callbacks).
pub fn mark_prototype_method(proto: &Gc<Object>, heap: &mut Heap, name: &str) {
    set_prototype_method(proto, heap, name, |_, _| {
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
                borrowed.as_native_function()
                    .map(|f| f.name.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

pub use object::{object_keys, object_values};
