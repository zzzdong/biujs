mod array;
mod boolean;
mod error;
mod function;
mod math;
mod number;
mod object;
mod string;
mod symbol;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{JSObject, PropertyTable};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

pub use crate::vm::ObjectKind;

pub use array::{array_constructor, validate_array_length};
pub use boolean::boolean_constructor;
pub use error::{
    ErrorType, create_error_object, error_constructor, runtime_error_to_js_error,
    setup_error_prototype, setup_native_error_prototype,
};
pub use function::{function_constructor, setup_function_prototype};
pub use number::{
    number_constructor, number_is_finite, number_is_integer, number_is_nan, number_to_exponential,
    number_to_fixed, number_to_precision,
};
pub use object::{OBJECT_TO_STRING_NATIVE, object_constructor, object_prototype_to_string};

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

/// `parseInt(string, radix)` — parses a leading integer in the given radix.
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
    TO_STRING_TAG_SYMBOL_ID, has_instance_symbol_key, register_symbol_value, symbol_description,
    symbol_value_by_id, to_primitive_symbol_key, to_string_tag_symbol_key,
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
        // The factory is dispatched by name in `call_native_by_name`, which
        // builds an internal iterator over `this`.
        let iterator_fn = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            crate::vm::iterator::ITERATOR_NATIVE_NAME,
        ))));
        for proto in [&self.array_prototype, &self.string_prototype] {
            let _ = proto.borrow_mut().define_property(
                crate::vm::iterator::iterator_symbol_key(),
                method_descriptor(iterator_fn.clone()),
            );
        }

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

        // Global convenience functions (they are plain functions, not
        // constructors, so they have no `prototype` slot).
        for name in ["isNaN", "isFinite", "parseInt", "parseFloat"] {
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

/// Attribute set of `C.prototype`: writable, non-enumerable, non-configurable.
pub fn constructor_prototype_descriptor(value: Value) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: true,
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
        "parseInt" => global_parse_int(args),
        "parseFloat" => global_parse_float(args),
        "Boolean" => boolean::boolean_constructor(args),
        "Number" => number::number_constructor(args),
        "String" => string::string_constructor(args),
        "Function" => function::function_constructor(args),
        "Symbol" => symbol::symbol_constructor(args),
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
        "Object.isExtensible" => Ok(Value::Bool(match args.first() {
            Some(Value::Object(o)) => o.borrow().is_extensible(),
            Some(_) => false,
            None => false,
        })),
        "Object.isFrozen" => Ok(Value::Bool(match args.first() {
            Some(Value::Object(o)) => o.borrow().is_frozen(),
            _ => false,
        })),
        "Object.isSealed" => Ok(Value::Bool(match args.first() {
            Some(Value::Object(o)) => o.borrow().is_sealed(),
            _ => false,
        })),
        "Object.preventExtensions" => {
            if let Some(Value::Object(o)) = args.first() {
                o.borrow_mut().prevent_extensions();
            }
            Ok(args.first().cloned().unwrap_or(Value::Undefined))
        }
        "Object.seal" => {
            if let Some(Value::Object(o)) = args.first() {
                o.borrow_mut().seal();
            }
            Ok(args.first().cloned().unwrap_or(Value::Undefined))
        }
        "Object.freeze" => {
            if let Some(Value::Object(o)) = args.first() {
                o.borrow_mut().freeze();
            }
            Ok(args.first().cloned().unwrap_or(Value::Undefined))
        }
        "Array.isArray" => Ok(Value::Bool(matches!(
            args.first(),
            Some(Value::Object(o)) if o.borrow().kind() == ObjectKind::Array
        ))),
        "Array.of" => Ok(array::array_constructor(args)?),
        "Array.from" => array::array_from(args),
        "String.fromCharCode" => string::string_from_char_code(args),
        "String.fromCodePoint" => string::string_from_code_point(args),
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
    match method_name {
        // `Number.prototype.toString([radix])` takes the radix argument.
        "toString" if matches!(obj, Value::Number(_)) => {
            number::number_to_string_with_radix(obj, args)
        }
        "toString" => dispatch_to_string(obj),
        // `Object.prototype.toLocaleString` delegates to `toString`
        // (ES 20.1.3.5); Array/Number inherit the same entry.
        "toLocaleString" => dispatch_to_string(obj),
        "valueOf" => dispatch_value_of(obj),
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
        "at" if !matches!(obj, Value::Object(_)) => string::string_at(obj, args),
        "normalize" => string::string_normalize(obj, args),
        "localeCompare" => string::string_locale_compare(obj, args),
        "split" => string::string_split(obj, args),
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

fn dispatch_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            if obj_ref.borrow().kind() == ObjectKind::Array {
                array::array_concat(obj, args)
            } else {
                string::string_concat(obj, args)
            }
        }
        _ => string::string_concat(obj, args),
    }
}

fn dispatch_includes(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            if obj_ref.borrow().kind() == ObjectKind::Array {
                array::array_includes(obj, args)
            } else {
                string::string_includes(obj, args)
            }
        }
        _ => string::string_includes(obj, args),
    }
}

fn dispatch_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            if obj_ref.borrow().kind() == ObjectKind::Array {
                array::array_index_of(obj, args)
            } else {
                string::string_index_of(obj, args)
            }
        }
        _ => string::string_index_of(obj, args),
    }
}

fn dispatch_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            if obj_ref.borrow().kind() == ObjectKind::Array {
                array::array_slice(obj, args)
            } else {
                string::string_slice(obj, args)
            }
        }
        _ => string::string_slice(obj, args),
    }
}

// ─────────────────────────────────────────────────────────
// Internal dispatch helpers — Number
// ─────────────────────────────────────────────────────────

fn dispatch_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            match borrowed.kind() {
                ObjectKind::Array => {
                    if let Some(arr) = borrowed
                        .as_any()
                        .downcast_ref::<crate::vm::object::ArrayObject>()
                    {
                        Ok(Value::string(&arr.join_elements()))
                    } else {
                        Ok(Value::string(&format!(
                            "[object {}]",
                            borrowed.class_name()
                        )))
                    }
                }
                ObjectKind::Boolean => {
                    // Boolean wrapper — unwrap
                    if let Some(inner) = borrowed
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
                _ => Ok(Value::string(&format!(
                    "[object {}]",
                    borrowed.class_name()
                ))),
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
    match name {
        // ── Constructors ──
        "Object" | "Array" | "Boolean" | "Number" | "String" | "Function" | "Error"
        | "TypeError" | "ReferenceError" | "RangeError" | "SyntaxError" | "URIError"
        | "EvalError" => 1,
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
