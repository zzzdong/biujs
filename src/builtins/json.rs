//! `JSON` (ES 25.5).
//!
//! The namespace object, the `JSON.parse` text parser and the string quoting
//! helpers live here. The *serializer* is split in two:
//!
//! * [`json_stringify`] works on plain data only (no `toJSON`, no `replacer`,
//!   no accessors). It is used when `JSON.stringify` is reached through the
//!   builtin dispatch table, which has no access to the VM.
//! * `VM::json_stringify` is the full algorithm: it reads through `[[Get]]`
//!   (so accessors run), honours `toJSON` and `replacer`, and detects cycles.
//!   That is the path a JS `JSON.stringify(...)` call takes.
//!
//! Both share [`quote_json_string`] / [`number_to_string`] for leaves.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{ArrayObject, JSObject, OrdinaryObject};
use crate::vm::ObjectKind;
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// Create the `JSON` namespace object (ES 25.5.1).
///
/// `JSON` is a plain object with two own methods and
/// `JSON[Symbol.toStringTag] === "JSON"`; it is neither callable nor a
/// constructor.
pub fn register_json() -> Value {
    let mut json = OrdinaryObject::new();
    if let Some(proto) = crate::builtins::wrapper_prototype("Object") {
        json.set_prototype(Some(proto));
    }

    let parse = Value::Object(Rc::new(RefCell::new(
        crate::vm::object::NativeFunctionObject::new("JSON.parse"),
    )));
    let stringify = Value::Object(Rc::new(RefCell::new(
        crate::vm::object::NativeFunctionObject::new("JSON.stringify"),
    )));
    let _ = json.define_property(PropertyKey::from_str("parse"), super::method_descriptor(parse));
    let _ = json.define_property(
        PropertyKey::from_str("stringify"),
        super::method_descriptor(stringify),
    );
    let _ = json.define_property(
        crate::builtins::to_string_tag_symbol_key(),
        PropertyDescriptor {
            value: Value::string("JSON"),
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );

    Value::Object(Rc::new(RefCell::new(json)))
}

// ─────────────────────────────────────────────────────────
// Quoting / numbers (shared with the VM serializer)
// ─────────────────────────────────────────────────────────

/// Append `value` as a JSON string literal (`QuoteJSONString`).
pub fn quote_json_string(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `SerializeJSONProperty`'s number branch: non-finite values become `null`.
pub fn json_number(value: f64) -> String {
    if value.is_finite() {
        super::number_to_string(value)
    } else {
        "null".to_string()
    }
}

// ─────────────────────────────────────────────────────────
// Parser (JSON.parse text)
// ─────────────────────────────────────────────────────────

/// `JSON.parse(text)` without a reviver.
///
/// Strictly ECMA-404: only `"`-quoted strings, no trailing commas, no comments,
/// no leading `+`/leading zeros, and only the four JSON whitespace characters.
/// Lone surrogates cannot be represented in a Rust `String`, so `\uD800` (and a
/// trail surrogate on its own) decode to U+FFFD.
pub fn json_parse(text: &str) -> Result<Value, RuntimeError> {
    let mut parser = JsonParser { text, pos: 0 };
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos != text.len() {
        return parser.error("Unexpected token after JSON value");
    }
    Ok(value)
}

struct JsonParser<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn error<T>(&self, message: &str) -> Result<T, RuntimeError> {
        Err(RuntimeError::SyntaxError(format!(
            "{message} in JSON at position {}",
            self.pos
        )))
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    /// JSON whitespace is only tab, LF, CR and space (ECMA-404 §2).
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.bump();
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), RuntimeError> {
        if self.peek() == Some(expected) {
            self.bump();
            Ok(())
        } else {
            self.error(&format!("Expected '{expected}'"))
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), RuntimeError> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            self.error("Unexpected token")
        }
    }

    fn parse_value(&mut self) -> Result<Value, RuntimeError> {
        self.skip_whitespace();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => Ok(Value::string(&self.parse_string()?)),
            Some('t') => {
                self.literal("true")?;
                Ok(Value::Bool(true))
            }
            Some('f') => {
                self.literal("false")?;
                Ok(Value::Bool(false))
            }
            Some('n') => {
                self.literal("null")?;
                Ok(Value::Null)
            }
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            _ => self.error("Unexpected token"),
        }
    }

    fn parse_object(&mut self) -> Result<Value, RuntimeError> {
        self.expect('{')?;
        let mut obj = OrdinaryObject::new();
        if let Some(proto) = crate::builtins::wrapper_prototype("Object") {
            obj.set_prototype(Some(proto));
        }
        self.skip_whitespace();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Value::Object(Rc::new(RefCell::new(obj))));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some('"') {
                return self.error("Expected property name");
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(':')?;
            let value = self.parse_value()?;
            // CreateDataProperty: a later duplicate key wins, and a `__proto__`
            // key becomes an ordinary own property.
            let _ = obj.define_property(
                PropertyKey::from_str(&key),
                PropertyDescriptor::data_descriptor(value),
            );
            self.skip_whitespace();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                _ => return self.error("Expected ',' or '}'"),
            }
        }
        Ok(Value::Object(Rc::new(RefCell::new(obj))))
    }

    fn parse_array(&mut self) -> Result<Value, RuntimeError> {
        self.expect('[')?;
        let mut arr = ArrayObject::new();
        self.skip_whitespace();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Value::Object(Rc::new(RefCell::new(arr))));
        }
        loop {
            let value = self.parse_value()?;
            arr.push(value);
            self.skip_whitespace();
            match self.bump() {
                Some(',') => continue,
                Some(']') => break,
                _ => return self.error("Expected ',' or ']'"),
            }
        }
        Ok(Value::Object(Rc::new(RefCell::new(arr))))
    }

    fn parse_number(&mut self) -> Result<Value, RuntimeError> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.bump();
        }
        match self.peek() {
            Some('0') => {
                self.bump();
            }
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.bump();
                }
            }
            _ => return self.error("Invalid number"),
        }
        if self.peek() == Some('.') {
            self.bump();
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return self.error("Invalid number");
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return self.error("Invalid number");
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        let text = &self.text[start..self.pos];
        // `1e999` overflows to +Infinity, which is what the spec's number
        // conversion produces too.
        Ok(Value::Number(text.parse::<f64>().unwrap_or(f64::NAN)))
    }

    fn parse_hex4(&mut self) -> Result<u32, RuntimeError> {
        let mut value = 0u32;
        for _ in 0..4 {
            match self.bump().and_then(|c| c.to_digit(16)) {
                Some(digit) => value = value * 16 + digit,
                None => return self.error("Invalid \\u escape"),
            }
        }
        Ok(value)
    }

    fn parse_string(&mut self) -> Result<String, RuntimeError> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return self.error("Unterminated string"),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let unit = self.parse_hex4()?;
                        if (0xD800..0xDC00).contains(&unit) {
                            // Try to combine a surrogate pair; a lone lead
                            // surrogate becomes U+FFFD.
                            let resume = self.pos;
                            let mut code = None;
                            if self.peek() == Some('\\') {
                                self.bump();
                                if self.peek() == Some('u') {
                                    self.bump();
                                    let low = self.parse_hex4()?;
                                    if (0xDC00..0xE000).contains(&low) {
                                        code = Some(0x1_0000 + ((unit - 0xD800) << 10) + (low - 0xDC00));
                                    } else {
                                        self.pos = resume;
                                    }
                                } else {
                                    self.pos = resume;
                                }
                            }
                            out.push(
                                code.and_then(char::from_u32)
                                    .unwrap_or('\u{FFFD}'),
                            );
                        } else if (0xDC00..0xE000).contains(&unit) {
                            out.push('\u{FFFD}');
                        } else {
                            out.push(char::from_u32(unit).unwrap_or('\u{FFFD}'));
                        }
                    }
                    _ => return self.error("Invalid escape"),
                },
                Some(c) if (c as u32) < 0x20 => {
                    return self.error("Invalid control character in string")
                }
                Some(c) => out.push(c),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────
// Data-only serializer (builtin dispatch path)
// ─────────────────────────────────────────────────────────

/// `JSON.stringify(value)` for plain data: no `toJSON`, no `replacer`, no
/// accessors. `None` means "the value is not serializable" (`undefined`, a
/// function or a symbol), which callers turn into `undefined` or `null`
/// depending on where the value sits.
pub fn json_stringify(value: &Value) -> Result<Option<String>, RuntimeError> {
    let mut stack = Vec::new();
    serialize(value, &mut stack)
}

fn serialize(value: &Value, stack: &mut Vec<Value>) -> Result<Option<String>, RuntimeError> {
    match value {
        Value::Undefined | Value::Function(_) | Value::Symbol(_) => Ok(None),
        Value::Null => Ok(Some("null".to_string())),
        Value::Bool(b) => Ok(Some(if *b { "true" } else { "false" }.to_string())),
        Value::Number(n) => Ok(Some(json_number(*n))),
        Value::String(s) => {
            let mut out = String::new();
            quote_json_string(s, &mut out);
            Ok(Some(out))
        }
        Value::Object(obj_ref) => {
            let kind = obj_ref.borrow().kind();
            match kind {
                // Wrapper objects serialize as their primitive.
                ObjectKind::Number => Ok(Some(json_number(value.to_number()))),
                // The wrapped primitive, not `to_boolean` (always true for an
                // object).
                ObjectKind::Boolean => Ok(Some(
                    if value.to_number() != 0.0 {
                        "true"
                    } else {
                        "false"
                    }
                    .to_string(),
                )),
                ObjectKind::Function => Ok(None),
                ObjectKind::String => {
                    let mut out = String::new();
                    quote_json_string(&value.to_js_string(), &mut out);
                    Ok(Some(out))
                }
                _ => {
                    if stack.iter().any(|v| v.strict_eq(value)) {
                        return Err(RuntimeError::TypeError(
                            "Converting circular structure to JSON".to_string(),
                        ));
                    }
                    stack.push(value.clone());
                    let result = if kind == ObjectKind::Array {
                        serialize_array(value, stack)
                    } else {
                        serialize_object(value, stack)
                    };
                    stack.pop();
                    result
                }
            }
        }
    }
}

/// `EnumerableOwnPropertyNames(value, key)`, in ordinary order (integer indices
/// ascending, then the remaining strings in insertion order).
pub fn own_enumerable_string_keys(value: &Value) -> Vec<String> {
    let Value::Object(obj_ref) = value else {
        return Vec::new();
    };
    let borrowed = obj_ref.borrow();
    borrowed
        .own_keys()
        .into_iter()
        .filter_map(|key| {
            let name = key.as_str()?.to_string();
            if borrowed.property_get(&key).is_some_and(|d| d.enumerable) {
                Some(name)
            } else {
                None
            }
        })
        .collect()
}

fn serialize_array(value: &Value, stack: &mut Vec<Value>) -> Result<Option<String>, RuntimeError> {
    let len = match value {
        Value::Object(obj_ref) => obj_ref
            .borrow()
            .property_get(&PropertyKey::from_str("length"))
            .map(|d| d.value.to_number())
            .unwrap_or(0.0),
        _ => 0.0,
    };
    let mut partial = Vec::new();
    for index in 0..(len.max(0.0) as u64) {
        let element = match value {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .property_get(&PropertyKey::from_str(&index.to_string()))
                .map(|d| d.value)
                .unwrap_or(Value::Undefined),
            _ => Value::Undefined,
        };
        partial.push(serialize(&element, stack)?.unwrap_or_else(|| "null".to_string()));
    }
    Ok(Some(format!("[{}]", partial.join(","))))
}

fn serialize_object(value: &Value, stack: &mut Vec<Value>) -> Result<Option<String>, RuntimeError> {
    let mut partial = Vec::new();
    for key in own_enumerable_string_keys(value) {
        let entry = match value {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .property_get(&PropertyKey::from_str(&key))
                .map(|d| d.value)
                .unwrap_or(Value::Undefined),
            _ => Value::Undefined,
        };
        if let Some(serialized) = serialize(&entry, stack)? {
            let mut quoted = String::new();
            quote_json_string(&key, &mut quoted);
            partial.push(format!("{quoted}:{serialized}"));
        }
    }
    Ok(Some(format!("{{{}}}", partial.join(","))))
}
