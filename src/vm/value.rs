use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::vm::object::JSObject;

/// Symbol internal data
#[derive(Debug, Clone)]
pub struct SymbolData {
    pub description: Option<String>,
    pub id: u64,
}

impl SymbolData {
    pub fn new(description: Option<String>, id: u64) -> Self {
        Self { description, id }
    }
}

/// JavaScript Value type.
///
/// Represents all JS values. Objects (including arrays, functions, etc.)
/// use reference semantics via `Rc<RefCell<dyn JSObject>>`.
/// Numbers use unified f64 as per ECMAScript specification.
#[derive(Debug)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Rc<String>),
    Symbol(Rc<SymbolData>),
    /// All object types (ordinary, array, function, builtins, etc.)
    Object(Rc<RefCell<dyn JSObject>>),
    /// Internal: reference to a bytecode function by ID (not a JS-visible type)
    Function(u32),
}

// Manual PartialEq — dyn JSObject is not PartialEq, so Object variant compares by pointer
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) => true,
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => {
                if a.is_nan() || b.is_nan() {
                    false
                } else {
                    a == b
                }
            }
            (Value::String(a), Value::String(b)) => Rc::ptr_eq(a, b) || a.as_str() == b.as_str(),
            (Value::Symbol(a), Value::Symbol(b)) => a.id == b.id,
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Function(a), Value::Function(b)) => a == b,
            _ => false,
        }
    }
}

// Manual Clone — dyn JSObject is not Clone, so we manually clone the Object variant
impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Object(obj) => {
                // Clone by creating a new Rc pointing to the same RefCell
                Value::Object(Rc::clone(obj))
            }
            Value::Symbol(sym) => Value::Symbol(Rc::clone(sym)),
            Value::String(s) => Value::String(Rc::clone(s)),
            Value::Function(id) => Value::Function(*id),
            other => other.clone_enum(),
        }
    }
}

impl Value {
    // Helper for clones that are simple enum copies
    fn clone_enum(&self) -> Self {
        match self {
            Value::Undefined => Value::Undefined,
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Number(n) => Value::Number(*n),
            _ => unreachable!(),
        }
    }

    pub fn undefined() -> Self {
        Value::Undefined
    }

    /// Convenience: create a string Value
    pub fn string(s: &str) -> Self {
        Value::String(Rc::new(s.to_string()))
    }

    /// Convenience: create a number Value from i32
    pub fn int(n: i32) -> Self {
        Value::Number(n as f64)
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// Unwrap the object reference, panicking if not an Object variant
    pub fn as_object(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        match self {
            Value::Object(obj) => Some(Rc::clone(obj)),
            _ => None,
        }
    }

    /// Convert to a JS string key for property access
    pub fn to_property_key(&self) -> String {
        match self {
            Value::String(s) => s.to_string(),
            Value::Symbol(_) => format!("Symbol({self})"),
            other => other.to_string(),
        }
    }

    /// JS typeof operator
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Object(obj) => obj.borrow().type_of(),
            Value::Function(_) => "function",
        }
    }

    /// Convert to boolean (ToBoolean abstract operation)
    pub fn to_boolean(&self) -> bool {
        match self {
            Value::Undefined => false,
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::String(s) => !s.is_empty(),
            Value::Symbol(_) => true,
            Value::Object(_) => true,
            Value::Function(_) => true,
        }
    }

    /// Convert to number (ToNumber abstract operation)
    ///
    /// Per ECMAScript spec:
    /// - String → trim whitespace, if empty → 0, otherwise parse as number
    /// - `""` → 0, `" \t\n"` → 0, `"42"` → 42.0
    pub fn to_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => {
                if *b { 1.0 } else { 0.0 }
            }
            Value::Number(n) => *n,
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    0.0
                } else {
                    trimmed.parse::<f64>().unwrap_or(f64::NAN)
                }
            }
            Value::Symbol(_) => f64::NAN,
            Value::Object(_) => f64::NAN,
            Value::Function(_) => f64::NAN,
        }
    }

    /// Convert to string (ToString abstract operation)
    pub fn to_js_string(&self) -> String {
        match self {
            Value::Undefined => "undefined".to_string(),
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => format_number(*n),
            Value::String(s) => s.to_string(),
            Value::Symbol(sym) => {
                match &sym.description {
                    Some(desc) => format!("Symbol({desc})"),
                    None => "Symbol()".to_string(),
                }
            }
            Value::Object(obj) => {
                let kind = obj.borrow().kind();
                if kind == crate::vm::property::ObjectKind::Array {
                    if let Some(arr) = obj.borrow().as_any().downcast_ref::<crate::vm::object::ArrayObject>() {
                        arr.join_elements()
                    } else {
                        "[object Array]".to_string()
                    }
                } else {
                    format!("[object {}]", obj.borrow().class_name())
                }
            }
            Value::Function(_) => "function() { [native code] }".to_string(),
        }
    }

    /// ToPrimitive abstract operation (ES6 7.1.1).
    ///
    /// Converts a Value to a primitive. For non-objects, returns self.
    /// For objects, delegates to `JSObject::to_primitive`; falls back
    /// to `to_js_string()` if the object-level conversion is unavailable.
    ///
    /// The `hint` parameter is `"default"`, `"number"`, or `"string"`.
    pub fn to_primitive(&self, hint: &str) -> Value {
        match self {
            Value::Object(obj_ref) => {
                let result = obj_ref.borrow().to_primitive(hint);
                match result {
                    Ok(v) => v,
                    Err(_) => {
                        // Fallback: use string representation as primitive
                        Value::string(&self.to_js_string())
                    }
                }
            }
            other => other.clone(),
        }
    }

    /// JS strict equality (===)
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) => true,
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => {
                if a.is_nan() || b.is_nan() {
                    false
                } else {
                    a == b
                }
            }
            (Value::String(a), Value::String(b)) => a.as_str() == b.as_str(),
            (Value::Symbol(a), Value::Symbol(b)) => a.id == b.id,
            (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            (Value::Function(a), Value::Function(b)) => a == b,
            _ => false,
        }
    }

    /// JS abstract equality (==)
    pub fn abstract_eq(&self, other: &Value) -> bool {
        match (self, other) {
            // Same type: use strict equality
            (Value::Undefined, Value::Undefined) => true,
            (Value::Null, Value::Null) => true,
            (Value::Bool(_), Value::Bool(_)) => self.strict_eq(other),
            (Value::Number(_), Value::Number(_)) => self.strict_eq(other),
            (Value::String(_), Value::String(_)) => self.strict_eq(other),

            // null == undefined
            (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,

            // Number == String: convert string to number
            (Value::Number(_), Value::String(_)) => {
                let other_num = other.to_number();
                self.strict_eq(&Value::Number(other_num))
            }
            (Value::String(_), Value::Number(_)) => {
                let self_num = self.to_number();
                Value::Number(self_num).strict_eq(other)
            }

            // Bool == anything: convert bool to number first
            (Value::Bool(_), _) => {
                let self_num = Value::Number(self.to_number());
                self_num.abstract_eq(other)
            }
            (_, Value::Bool(_)) => {
                let other_num = Value::Number(other.to_number());
                self.abstract_eq(&other_num)
            }

            // Object == Primitive: convert object to primitive (ToPrimitive with hint "default")
            (Value::Object(_), _) => {
                let prim = self.to_primitive("default");
                prim.abstract_eq(other)
            }
            (_, Value::Object(_)) => {
                let prim = other.to_primitive("default");
                self.abstract_eq(&prim)
            }

            _ => false,
        }
    }

    /// Check if value is an object type (for typeof "object" null handling)
    pub fn is_object_type(&self) -> bool {
        matches!(self, Value::Object(_) | Value::Null)
    }
}

// ─────────────────────────────────────────────────────────
// Number formatting helper
// ─────────────────────────────────────────────────────────

fn format_number(n: f64) -> String {
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
            let trimmed = s.trim_end_matches('0').trim_end_matches('.');
            trimmed.to_string()
        } else {
            s
        }
    }
}

// ─────────────────────────────────────────────────────────
// Operator implementations
// ─────────────────────────────────────────────────────────

impl std::ops::Add for Value {
    type Output = Value;

    fn add(self, rhs: Value) -> Value {
        match (&self, &rhs) {
            (Value::String(a), _) => {
                let b = rhs.to_js_string();
                Value::String(Rc::new(format!("{a}{b}")))
            }
            (_, Value::String(b)) => {
                let a = self.to_js_string();
                Value::String(Rc::new(format!("{a}{b}")))
            }
            _ => Value::Number(self.to_number() + rhs.to_number()),
        }
    }
}

impl std::ops::Sub for Value {
    type Output = Value;

    fn sub(self, rhs: Value) -> Value {
        Value::Number(self.to_number() - rhs.to_number())
    }
}

impl std::ops::Mul for Value {
    type Output = Value;

    fn mul(self, rhs: Value) -> Value {
        Value::Number(self.to_number() * rhs.to_number())
    }
}

impl std::ops::Div for Value {
    type Output = Value;

    fn div(self, rhs: Value) -> Value {
        Value::Number(self.to_number() / rhs.to_number())
    }
}

impl std::ops::Rem for Value {
    type Output = Value;

    fn rem(self, rhs: Value) -> Value {
        Value::Number(self.to_number() % rhs.to_number())
    }
}

impl std::ops::Neg for Value {
    type Output = Value;

    fn neg(self) -> Value {
        Value::Number(-self.to_number())
    }
}

// ─────────────────────────────────────────────────────────
// Display + Debug
// ─────────────────────────────────────────────────────────

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{}", format_number(*n)),
            Value::String(s) => write!(f, "{s}"),
            Value::Symbol(sym) => match &sym.description {
                Some(desc) => write!(f, "Symbol({desc})"),
                None => write!(f, "Symbol()"),
            },
            Value::Object(_) => write!(f, "[object Object]"),
            Value::Function(_) => write!(f, "function() {{ [native code] }}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──────────────────────── type_of ────────────────────────

    #[test]
    fn test_typeof_undefined() {
        assert_eq!(Value::Undefined.type_of(), "undefined");
    }

    #[test]
    fn test_typeof_null() {
        assert_eq!(Value::Null.type_of(), "object");
    }

    #[test]
    fn test_typeof_boolean() {
        assert_eq!(Value::Bool(true).type_of(), "boolean");
        assert_eq!(Value::Bool(false).type_of(), "boolean");
    }

    #[test]
    fn test_typeof_number() {
        assert_eq!(Value::Number(0.0).type_of(), "number");
        assert_eq!(Value::Number(42.0).type_of(), "number");
        assert_eq!(Value::Number(f64::NAN).type_of(), "number");
    }

    #[test]
    fn test_typeof_string() {
        assert_eq!(Value::String(Rc::new("".to_string())).type_of(), "string");
        assert_eq!(Value::String(Rc::new("hello".to_string())).type_of(), "string");
    }

    #[test]
    fn test_typeof_symbol() {
        assert_eq!(
            Value::Symbol(Rc::new(SymbolData::new(Some("test".to_string()), 1))).type_of(),
            "symbol"
        );
    }

    #[test]
    fn test_typeof_function() {
        assert_eq!(Value::Function(0).type_of(), "function");
    }

    // ──────────────────────── to_boolean ────────────────────────

    #[test]
    fn test_to_boolean_falsy() {
        assert!(!Value::Undefined.to_boolean());
        assert!(!Value::Null.to_boolean());
        assert!(!Value::Bool(false).to_boolean());
        assert!(!Value::Number(0.0).to_boolean());
        assert!(!Value::Number(-0.0).to_boolean());
        assert!(!Value::Number(f64::NAN).to_boolean());
        assert!(!Value::String(Rc::new("".to_string())).to_boolean());
    }

    #[test]
    fn test_to_boolean_truthy() {
        assert!(Value::Bool(true).to_boolean());
        assert!(Value::Number(1.0).to_boolean());
        assert!(Value::Number(-1.0).to_boolean());
        assert!(Value::Number(f64::INFINITY).to_boolean());
        assert!(Value::String(Rc::new("hello".to_string())).to_boolean());
        assert!(Value::String(Rc::new("0".to_string())).to_boolean());
        assert!(Value::String(Rc::new("false".to_string())).to_boolean());
        assert!(Value::Symbol(Rc::new(SymbolData::new(None, 1))).to_boolean());
        assert!(Value::Function(0).to_boolean());
    }

    // ──────────────────────── to_number ────────────────────────

    #[test]
    fn test_to_number_undefined() {
        assert!(Value::Undefined.to_number().is_nan());
    }

    #[test]
    fn test_to_number_null() {
        assert_eq!(Value::Null.to_number(), 0.0);
    }

    #[test]
    fn test_to_number_boolean() {
        assert_eq!(Value::Bool(true).to_number(), 1.0);
        assert_eq!(Value::Bool(false).to_number(), 0.0);
    }

    #[test]
    fn test_to_number_number() {
        assert_eq!(Value::Number(42.0).to_number(), 42.0);
        assert_eq!(Value::Number(-3.14).to_number(), -3.14);
    }

    #[test]
    fn test_to_number_string() {
        assert_eq!(
            Value::String(Rc::new("42".to_string())).to_number(),
            42.0
        );
        assert_eq!(
            Value::String(Rc::new("3.14".to_string())).to_number(),
            3.14
        );
        assert!(Value::String(Rc::new("hello".to_string()))
            .to_number()
            .is_nan());
    }

    #[test]
    fn test_to_number_symbol() {
        assert!(Value::Symbol(Rc::new(SymbolData::new(None, 1)))
            .to_number()
            .is_nan());
    }

    #[test]
    fn test_to_number_empty_string() {
        assert_eq!(Value::string("").to_number(), 0.0);
        assert_eq!(Value::string("   ").to_number(), 0.0);
        assert_eq!(Value::string("\t\n").to_number(), 0.0);
    }

    // ──────────────────────── to_primitive ────────────────────────

    #[test]
    fn test_to_primitive_non_object() {
        // Non-object values return themselves
        assert!(matches!(Value::Undefined.to_primitive("default"), Value::Undefined));
        assert!(matches!(Value::Null.to_primitive("default"), Value::Null));
        assert!(matches!(Value::Bool(true).to_primitive("default"), Value::Bool(true)));
        assert!(matches!(Value::Number(42.0).to_primitive("default"), Value::Number(42.0)));
        assert!(matches!(Value::string("hello").to_primitive("default"), Value::String(_)));
    }

    #[test]
    fn test_to_primitive_array() {
        use crate::vm::object::new_array_object_from_vec;
        let arr = new_array_object_from_vec(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ]);
        let prim = arr.to_primitive("default");
        assert_eq!(prim.to_js_string(), "1,2,3");
    }

    #[test]
    fn test_to_primitive_empty_array() {
        use crate::vm::object::new_array_object;
        let arr = new_array_object();
        let prim = arr.to_primitive("default");
        assert_eq!(prim.to_js_string(), "");
    }

    #[test]
    fn test_to_primitive_array_mixed() {
        use crate::vm::object::new_array_object_from_vec;
        let arr = new_array_object_from_vec(vec![
            Value::Number(1.0),
            Value::string("hello"),
            Value::Bool(true),
        ]);
        let prim = arr.to_primitive("default");
        assert_eq!(prim.to_js_string(), "1,hello,true");
    }

    // ──────────────────────── abstract_eq ────────────────────────

    #[test]
    fn test_abstract_eq_object_vs_number() {
        use crate::vm::object::new_array_object_from_vec;

        // [1] == 1
        let arr = new_array_object_from_vec(vec![Value::Number(1.0)]);
        assert!(arr.abstract_eq(&Value::Number(1.0)));

        // [1,2] == 1 → ToPrimitive("1,2") → Number("1,2") = NaN → false
        let arr2 = new_array_object_from_vec(vec![
            Value::Number(1.0),
            Value::Number(2.0),
        ]);
        assert!(!arr2.abstract_eq(&Value::Number(1.0)));
    }

    #[test]
    fn test_abstract_eq_object_vs_string() {
        use crate::vm::object::new_array_object_from_vec;

        // [1,2,3] == "1,2,3"
        let arr = new_array_object_from_vec(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ]);
        assert!(arr.abstract_eq(&Value::string("1,2,3")));

        // [] == ""
        let empty = crate::vm::object::new_array_object();
        assert!(empty.abstract_eq(&Value::string("")));
    }

    // ──────────────────────── to_js_string ────────────────────────

    #[test]
    fn test_to_string_primitives() {
        assert_eq!(Value::Undefined.to_js_string(), "undefined");
        assert_eq!(Value::Null.to_js_string(), "null");
        assert_eq!(Value::Bool(true).to_js_string(), "true");
        assert_eq!(Value::Bool(false).to_js_string(), "false");
    }

    #[test]
    fn test_to_string_numbers() {
        assert_eq!(Value::Number(0.0).to_js_string(), "0");
        assert_eq!(Value::Number(42.0).to_js_string(), "42");
        assert_eq!(Value::Number(-5.0).to_js_string(), "-5");
        assert_eq!(Value::Number(3.14).to_js_string(), "3.14");
        assert_eq!(Value::Number(f64::NAN).to_js_string(), "NaN");
        assert_eq!(Value::Number(f64::INFINITY).to_js_string(), "Infinity");
        assert_eq!(Value::Number(f64::NEG_INFINITY).to_js_string(), "-Infinity");
    }

    // ──────────────────────── strict_eq ────────────────────────

    #[test]
    fn test_strict_eq_undefined() {
        assert!(Value::Undefined.strict_eq(&Value::Undefined));
        assert!(!Value::Undefined.strict_eq(&Value::Null));
    }

    #[test]
    fn test_strict_eq_null() {
        assert!(Value::Null.strict_eq(&Value::Null));
        assert!(!Value::Null.strict_eq(&Value::Undefined));
    }

    #[test]
    fn test_strict_eq_boolean() {
        assert!(Value::Bool(true).strict_eq(&Value::Bool(true)));
        assert!(!Value::Bool(true).strict_eq(&Value::Bool(false)));
    }

    #[test]
    fn test_strict_eq_number() {
        assert!(Value::Number(42.0).strict_eq(&Value::Number(42.0)));
        assert!(!Value::Number(42.0).strict_eq(&Value::Number(43.0)));
        assert!(!Value::Number(f64::NAN).strict_eq(&Value::Number(f64::NAN)));
        assert!(Value::Number(0.0).strict_eq(&Value::Number(-0.0)));
    }

    #[test]
    fn test_strict_eq_string() {
        assert!(Value::String(Rc::new("hello".to_string()))
            .strict_eq(&Value::String(Rc::new("hello".to_string()))));
        assert!(!Value::String(Rc::new("hello".to_string()))
            .strict_eq(&Value::String(Rc::new("world".to_string()))));
    }

    #[test]
    fn test_strict_eq_type_mismatch() {
        assert!(!Value::String(Rc::new("42".to_string())).strict_eq(&Value::Number(42.0)));
        assert!(!Value::Bool(true).strict_eq(&Value::Number(1.0)));
    }

    // ──────────────────────── abstract_eq ────────────────────────

    #[test]
    fn test_abstract_eq_null_undefined() {
        assert!(Value::Null.abstract_eq(&Value::Undefined));
        assert!(Value::Undefined.abstract_eq(&Value::Null));
    }

    #[test]
    fn test_abstract_eq_number_string() {
        assert!(Value::Number(42.0).abstract_eq(&Value::String(Rc::new("42".to_string()))));
        assert!(Value::String(Rc::new("42".to_string())).abstract_eq(&Value::Number(42.0)));
    }

    #[test]
    fn test_abstract_eq_boolean_number() {
        assert!(Value::Bool(true).abstract_eq(&Value::Number(1.0)));
        assert!(Value::Bool(false).abstract_eq(&Value::Number(0.0)));
    }

    #[test]
    fn test_abstract_eq_boolean_string() {
        assert!(Value::Bool(true).abstract_eq(&Value::String(Rc::new("1".to_string()))));
        assert!(Value::Bool(false).abstract_eq(&Value::String(Rc::new("0".to_string()))));
    }

    // ──────────────────────── arithmetic ────────────────────────

    #[test]
    fn test_add_numbers() {
        let a = Value::Number(1.0) + Value::Number(2.0);
        assert_eq!(a, Value::Number(3.0));
    }

    #[test]
    fn test_add_string_concatenation() {
        let a = Value::String(Rc::new("hello".to_string()))
            + Value::String(Rc::new(" world".to_string()));
        assert_eq!(a, Value::String(Rc::new("hello world".to_string())));
    }

    #[test]
    fn test_add_string_number_coercion() {
        let a = Value::String(Rc::new("hello".to_string())) + Value::Number(42.0);
        assert_eq!(a, Value::String(Rc::new("hello42".to_string())));
        let b = Value::Number(42.0) + Value::String(Rc::new("hello".to_string()));
        assert_eq!(b, Value::String(Rc::new("42hello".to_string())));
    }

    #[test]
    fn test_sub_numbers() {
        assert_eq!(Value::Number(10.0) - Value::Number(3.0), Value::Number(7.0));
    }

    #[test]
    fn test_mul_numbers() {
        assert_eq!(Value::Number(4.0) * Value::Number(5.0), Value::Number(20.0));
    }

    #[test]
    fn test_div_numbers() {
        assert_eq!(Value::Number(10.0) / Value::Number(2.0), Value::Number(5.0));
    }

    #[test]
    fn test_rem_numbers() {
        assert_eq!(Value::Number(10.0) % Value::Number(3.0), Value::Number(1.0));
    }

    #[test]
    fn test_neg_number() {
        assert_eq!(-Value::Number(5.0), Value::Number(-5.0));
        assert_eq!(-Value::Number(-3.0), Value::Number(3.0));
    }

    // ──────────────────────── Clone ────────────────────────

    #[test]
    fn test_value_clone_primitives() {
        let v = Value::Number(42.0);
        assert_eq!(v.clone(), v);
    }

    #[test]
    fn test_value_clone_string() {
        let v = Value::String(Rc::new("hello".to_string()));
        let cloned = v.clone();
        assert_eq!(cloned, v);
        // They share the same Rc
        assert!(Rc::ptr_eq(
            match &v { Value::String(s) => s, _ => unreachable!() },
            match &cloned { Value::String(s) => s, _ => unreachable!() },
        ));
    }
}