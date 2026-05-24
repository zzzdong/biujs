use std::collections::HashMap;
use std::fmt;

/// JavaScript Value type
///
/// Represents all JS values in the engine. Numbers use a unified f64
/// as per the ECMAScript specification.
#[derive(Debug, Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(HashMap<String, Value>),
    /// Internal: reference to a function by ID (not a JS-visible type)
    Function(u32),
}

impl Value {
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// JS typeof operator
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) | Value::Object(_) => "object",
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
            Value::Array(_) | Value::Object(_) | Value::Function(_) => true,
        }
    }

    /// Convert to number (ToNumber abstract operation)
    pub fn to_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => if *b { 1.0 } else { 0.0 },
            Value::Number(n) => *n,
            Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
            Value::Array(_) | Value::Object(_) | Value::Function(_) => f64::NAN,
        }
    }

    /// Convert to string (ToString abstract operation)
    pub fn to_string(&self) -> String {
        match self {
            Value::Undefined => "undefined".to_string(),
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => {
                if n.is_nan() {
                    "NaN".to_string()
                } else if *n == 0.0 {
                    "0".to_string()
                } else if n.is_infinite() {
                    if n.is_sign_positive() {
                        "Infinity".to_string()
                    } else {
                        "-Infinity".to_string()
                    }
                } else {
                    // Remove trailing zeros for integers
                    let s = n.to_string();
                    if s.contains('.') {
                        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
                        trimmed.to_string()
                    } else {
                        s
                    }
                }
            }
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                // JS Array toString: join with commas
                let parts: Vec<String> = arr.iter().map(|v| v.to_string()).collect();
                parts.join(",")
            }
            Value::Object(_) => "[object Object]".to_string(),
            Value::Function(_) => "function() { [native code] }".to_string(),
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
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                // Arrays are reference-equal in JS, but for simplicity we do value-equal
                a == b
            }
            (Value::Object(_), Value::Object(_)) => {
                // Objects are reference-equal in JS; for now, always false
                false
            }
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

            _ => false,
        }
    }
}

// JS addition with string priority
impl std::ops::Add for Value {
    type Output = Value;

    fn add(self, rhs: Value) -> Value {
        // If either operand is a string, do string concatenation
        match (&self, &rhs) {
            (Value::String(a), _) => {
                let b = rhs.to_string();
                Value::String(format!("{a}{b}"))
            }
            (_, Value::String(b)) => {
                let a = self.to_string();
                Value::String(format!("{a}{b}"))
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

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => {
                if n.is_nan() {
                    write!(f, "NaN")
                } else if *n == 0.0 {
                    write!(f, "0")
                } else {
                    write!(f, "{n}")
                }
            }
            Value::String(s) => write!(f, "{s}"),
            Value::Array(arr) => {
                let parts: Vec<String> = arr.iter().map(|v| format!("{v}")).collect();
                write!(f, "{}", parts.join(","))
            }
            Value::Object(_) => write!(f, "[object Object]"),
            Value::Function(_) => write!(f, "function() {{ [native code] }}"),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.strict_eq(other)
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
        assert_eq!(Value::String("".to_string()).type_of(), "string");
        assert_eq!(Value::String("hello".to_string()).type_of(), "string");
    }

    #[test]
    fn test_typeof_array() {
        assert_eq!(Value::Array(vec![]).type_of(), "object");
    }

    #[test]
    fn test_typeof_object() {
        assert_eq!(Value::Object(HashMap::new()).type_of(), "object");
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
        assert!(!Value::String("".to_string()).to_boolean());
    }

    #[test]
    fn test_to_boolean_truthy() {
        assert!(Value::Bool(true).to_boolean());
        assert!(Value::Number(1.0).to_boolean());
        assert!(Value::Number(-1.0).to_boolean());
        assert!(Value::Number(f64::INFINITY).to_boolean());
        assert!(Value::String("hello".to_string()).to_boolean());
        assert!(Value::String("0".to_string()).to_boolean());
        assert!(Value::String("false".to_string()).to_boolean());
        assert!(Value::Array(vec![]).to_boolean());
        assert!(Value::Object(HashMap::new()).to_boolean());
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
        assert_eq!(Value::String("42".to_string()).to_number(), 42.0);
        assert_eq!(Value::String("3.14".to_string()).to_number(), 3.14);
        assert_eq!(Value::String("-5".to_string()).to_number(), -5.0);
        assert!(Value::String("hello".to_string()).to_number().is_nan());
        assert!(Value::String("".to_string()).to_number().is_nan());
        // " " (whitespace) should also be NaN per JS spec for non-trimmed
    }

    #[test]
    fn test_to_number_object() {
        assert!(Value::Array(vec![]).to_number().is_nan());
        assert!(Value::Object(HashMap::new()).to_number().is_nan());
    }

    // ──────────────────────── to_string ────────────────────────

    #[test]
    fn test_to_string_primitives() {
        assert_eq!(Value::Undefined.to_string(), "undefined");
        assert_eq!(Value::Null.to_string(), "null");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Bool(false).to_string(), "false");
    }

    #[test]
    fn test_to_string_numbers() {
        assert_eq!(Value::Number(0.0).to_string(), "0");
        assert_eq!(Value::Number(42.0).to_string(), "42");
        assert_eq!(Value::Number(-5.0).to_string(), "-5");
        assert_eq!(Value::Number(3.14).to_string(), "3.14");
        assert_eq!(Value::Number(f64::NAN).to_string(), "NaN");
        assert_eq!(Value::Number(f64::INFINITY).to_string(), "Infinity");
        assert_eq!(Value::Number(f64::NEG_INFINITY).to_string(), "-Infinity");
    }

    #[test]
    fn test_to_string_array() {
        assert_eq!(Value::Array(vec![]).to_string(), "");
        assert_eq!(
            Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]).to_string(),
            "1,2"
        );
        assert_eq!(
            Value::Array(vec![Value::String("a".to_string()), Value::Bool(true)]).to_string(),
            "a,true"
        );
    }

    #[test]
    fn test_to_string_object() {
        assert_eq!(Value::Object(HashMap::new()).to_string(), "[object Object]");
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
        assert!(!Value::Bool(true).strict_eq(&Value::Number(1.0)));
    }

    #[test]
    fn test_strict_eq_number() {
        assert!(Value::Number(42.0).strict_eq(&Value::Number(42.0)));
        assert!(!Value::Number(42.0).strict_eq(&Value::Number(43.0)));
        // NaN !== NaN
        assert!(!Value::Number(f64::NAN).strict_eq(&Value::Number(f64::NAN)));
        // +0 === -0
        assert!(Value::Number(0.0).strict_eq(&Value::Number(-0.0)));
    }

    #[test]
    fn test_strict_eq_string() {
        assert!(Value::String("hello".to_string()).strict_eq(&Value::String("hello".to_string())));
        assert!(!Value::String("hello".to_string()).strict_eq(&Value::String("world".to_string())));
        assert!(!Value::String("hello".to_string()).strict_eq(&Value::Number(42.0)));
    }

    #[test]
    fn test_strict_eq_array() {
        assert!(Value::Array(vec![]).strict_eq(&Value::Array(vec![])));
        assert!(Value::Array(vec![Value::Number(1.0)]).strict_eq(&Value::Array(vec![Value::Number(1.0)])));
        assert!(!Value::Array(vec![Value::Number(1.0)]).strict_eq(&Value::Array(vec![Value::Number(2.0)])));
    }

    // ──────────────────────── abstract_eq ────────────────────────

    #[test]
    fn test_abstract_eq_null_undefined() {
        assert!(Value::Null.abstract_eq(&Value::Undefined));
        assert!(Value::Undefined.abstract_eq(&Value::Null));
    }

    #[test]
    fn test_abstract_eq_number_string() {
        assert!(Value::Number(42.0).abstract_eq(&Value::String("42".to_string())));
        assert!(Value::String("42".to_string()).abstract_eq(&Value::Number(42.0)));
        assert!(!Value::Number(42.0).abstract_eq(&Value::String("43".to_string())));
    }

    #[test]
    fn test_abstract_eq_boolean_number() {
        // true == 1
        assert!(Value::Bool(true).abstract_eq(&Value::Number(1.0)));
        // false == 0
        assert!(Value::Bool(false).abstract_eq(&Value::Number(0.0)));
        // true != 2
        assert!(!Value::Bool(true).abstract_eq(&Value::Number(2.0)));
    }

    #[test]
    fn test_abstract_eq_boolean_string() {
        // true == "1" (true -> 1, "1" -> 1)
        assert!(Value::Bool(true).abstract_eq(&Value::String("1".to_string())));
        // false == "0" (false -> 0, "0" -> 0)
        assert!(Value::Bool(false).abstract_eq(&Value::String("0".to_string())));
    }

    // ──────────────────────── arithmetic ────────────────────────

    #[test]
    fn test_add_numbers() {
        let a = Value::Number(1.0) + Value::Number(2.0);
        assert_eq!(a, Value::Number(3.0));
    }

    #[test]
    fn test_add_string_concatenation() {
        let a = Value::String("hello".to_string()) + Value::String(" world".to_string());
        assert_eq!(a, Value::String("hello world".to_string()));
    }

    #[test]
    fn test_add_string_number_coercion() {
        // "hello" + 42 → "hello42"
        let a = Value::String("hello".to_string()) + Value::Number(42.0);
        assert_eq!(a, Value::String("hello42".to_string()));
        // 42 + "hello" → "42hello"
        let b = Value::Number(42.0) + Value::String("hello".to_string());
        assert_eq!(b, Value::String("42hello".to_string()));
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
}
