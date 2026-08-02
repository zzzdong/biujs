mod array;
mod boolean;
mod error;
mod function;
mod number;
mod object;
mod string;
mod symbol;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::JSObject;
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

pub use crate::vm::ObjectKind;

pub use array::array_constructor;
pub use boolean::boolean_constructor;
pub use error::{
    ErrorType, create_error_object, error_constructor, runtime_error_to_js_error,
    setup_error_prototype,
};
pub use function::{function_constructor, setup_function_prototype};
pub use number::{
    number_constructor, number_is_finite, number_is_integer, number_is_nan, number_to_exponential,
    number_to_fixed, number_to_precision,
};
pub use object::object_constructor;
pub use string::string_constructor;
pub use symbol::symbol_constructor;

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
        let boolean_proto = new_proto(Some(Rc::clone(&object_proto)), "Boolean");
        let number_proto = new_proto(Some(Rc::clone(&object_proto)), "Number");
        let string_proto = new_proto(Some(Rc::clone(&object_proto)), "String");
        let function_proto = new_proto(Some(Rc::clone(&object_proto)), "Function");
        let symbol_proto = new_proto(Some(Rc::clone(&object_proto)), "Symbol");

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
            boolean_prototype: boolean_proto,
            number_prototype: number_proto,
            string_prototype: string_proto,
            function_prototype: function_proto,
            symbol_prototype: symbol_proto,
        }
    }

    pub fn register(&self, globals: &mut HashMap<String, Value>) {
        use crate::vm::object::NativeFunctionObject;

        let object_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("Object", Rc::clone(&self.object_prototype)),
        )));
        object::register_object_statics(&object_fn_val, self);
        globals.insert("Object".to_string(), object_fn_val);

        let array_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("Array", Rc::clone(&self.array_prototype)),
        )));
        array::register_array_prototype(&self.array_prototype);
        globals.insert("Array".to_string(), array_fn_val);

        // Error constructors with prototype property setup
        let error_constructors = [
            ("Error", Rc::clone(&self.error_prototype)),
            ("TypeError", Rc::clone(&self.type_error_prototype)),
            ("ReferenceError", Rc::clone(&self.reference_error_prototype)),
            ("RangeError", Rc::clone(&self.range_error_prototype)),
            ("URIError", Rc::clone(&self.uri_error_prototype)),
            ("EvalError", Rc::clone(&self.eval_error_prototype)),
        ];

        for (name, proto) in error_constructors {
            let fn_val = Value::Object(Rc::new(RefCell::new(
                NativeFunctionObject::with_prototype(name, Rc::clone(&proto)),
            )));

            // Set Error.prototype property on the constructor
            if let Value::Object(ref obj_ref) = fn_val {
                let _ = obj_ref.borrow_mut().property_set(
                    PropertyKey::from_str("prototype"),
                    Value::Object(Rc::clone(&proto)),
                );
            }

            globals.insert(name.to_string(), fn_val);
        }

        // Setup Error.prototype methods
        error::setup_error_prototype(&self.error_prototype, self);

        let bool_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("Boolean", Rc::clone(&self.boolean_prototype)),
        )));
        boolean::register_boolean_prototype(&self.boolean_prototype);
        globals.insert("Boolean".to_string(), bool_fn_val);

        let number_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("Number", Rc::clone(&self.number_prototype)),
        )));
        number::register_number_statics(&number_fn_val, &self.number_prototype);
        number::register_number_prototype(&self.number_prototype);
        globals.insert("Number".to_string(), number_fn_val);

        let string_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("String", Rc::clone(&self.string_prototype)),
        )));
        string::register_string_prototype(&self.string_prototype);
        globals.insert("String".to_string(), string_fn_val);

        let function_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("Function", Rc::clone(&self.function_prototype)),
        )));
        function::register_function_statics(&function_fn_val, &self.function_prototype);
        globals.insert("Function".to_string(), function_fn_val);

        // Symbol constructor
        let symbol_fn_val = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::with_prototype("Symbol", Rc::clone(&self.symbol_prototype)),
        )));
        symbol::register_symbol_statics(&symbol_fn_val, &self.symbol_prototype);
        symbol::register_symbol_prototype(&self.symbol_prototype);
        globals.insert("Symbol".to_string(), symbol_fn_val);
    }
}

// ─────────────────────────────────────────────────────────
// Dispatchers
// ─────────────────────────────────────────────────────────

pub fn call_native(name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    match name {
        "Object" => object::object_constructor(args),
        "Array" => array::array_constructor(args),
        "Error" => error::error_constructor(error::ErrorType::Error, args),
        "TypeError" => error::error_constructor(error::ErrorType::TypeError, args),
        "ReferenceError" => error::error_constructor(error::ErrorType::ReferenceError, args),
        "RangeError" => error::error_constructor(error::ErrorType::RangeError, args),
        "Boolean" => boolean::boolean_constructor(args),
        "Number" => number::number_constructor(args),
        "String" => string::string_constructor(args),
        "Function" => function::function_constructor(args),
        "Symbol" => symbol::symbol_constructor(args),
        _ => Err(RuntimeError::TypeError(format!("unknown built-in: {name}"))),
    }
}

pub fn call_static_method(name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    match name {
        "Number.isNaN" => number::number_is_nan(args),
        "Number.isFinite" => number::number_is_finite(args),
        "Number.isInteger" => number::number_is_integer(args),
        "Object.keys" => object::object_keys(args),
        "Object.values" => object::object_values(args),
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
        "toString" => dispatch_to_string(obj),
        "valueOf" => dispatch_value_of(obj),
        // Number prototype
        "toFixed" => number::number_to_fixed(obj, args),
        "toExponential" => number::number_to_exponential(obj, args),
        "toPrecision" => number::number_to_precision(obj, args),
        // String prototype
        "charAt" => string::string_char_at(obj, args),
        "charCodeAt" => string::string_char_code_at(obj, args),
        "toUpperCase" => string::string_to_upper(obj),
        "toLowerCase" => string::string_to_lower(obj),
        "trim" => string::string_trim(obj),
        "split" => string::string_split(obj, args),
        "substring" => string::string_substring(obj, args),
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
                    Ok(Value::string("false"))
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

fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n == 0.0 || n == -0.0 {
        "0".to_string()
    } else if n.is_infinite() {
        if n.is_sign_positive() {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else {
        let s = n.to_string();
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    }
}

fn number_to_string_radix(n: f64, radix: u32) -> String {
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
    match obj {
        Value::Object(_) | _ => Ok(obj.clone()),
    }
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
    properties: BTreeMap<PropertyKey, PropertyDescriptor>,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
}

impl ProtoObject {
    fn new(prototype: Option<Rc<RefCell<dyn JSObject>>>) -> Self {
        Self {
            properties: BTreeMap::new(),
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
    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        self.properties.remove(key).is_some()
    }
    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }
    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys().cloned().collect()
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
        let key = PropertyKey::from_str(name);
        let _ = obj_ref
            .borrow_mut()
            .property_set(key, Value::string(&format!("__native_method__{name}")));
    }
}

/// Set a prototype method on an object (used for prototype methods like Error.prototype.toString)
pub fn set_prototype_method<F>(proto: &Rc<RefCell<dyn JSObject>>, name: &str, _f: F)
where
    F: Fn(&Value, &[Value]) -> Result<Value, RuntimeError> + 'static,
{
    let key = PropertyKey::from_str(name);
    let method_val = Value::Object(Rc::new(RefCell::new(
        crate::vm::object::NativeFunctionObject::new(&format!("__proto_method__{name}")),
    )));
    let _ = proto.borrow_mut().property_set(key, method_val);
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
