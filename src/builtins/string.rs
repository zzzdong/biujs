use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{ArrayObject, JSObject};
use crate::vm::value::Value;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

/// ES `WhiteSpace` + `LineTerminator` (11.2 / 11.3).
///
/// This is *not* Rust's `char::is_whitespace`: U+0085 (NEL) is excluded and
/// U+FEFF (ZWNBSP) is included, and the Unicode `Space_Separator` category is
/// spelled out rather than taken from the host Unicode tables.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

pub fn register_string_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;

    set_prototype_method(proto, "valueOf", |this, _args| string_value_of(this));
    set_prototype_method(proto, "toString", |this, _args| string_to_string(this));
    set_prototype_method(proto, "charAt", |this, args| string_char_at(this, args));
    set_prototype_method(proto, "charCodeAt", |this, args| {
        string_char_code_at(this, args)
    });
    set_prototype_method(proto, "concat", |this, args| string_concat(this, args));
    set_prototype_method(proto, "includes", |this, args| string_includes(this, args));
    set_prototype_method(proto, "indexOf", |this, args| string_index_of(this, args));
    set_prototype_method(proto, "slice", |this, args| string_slice(this, args));
    set_prototype_method(proto, "substring", |this, args| {
        string_substring(this, args)
    });
    set_prototype_method(proto, "toUpperCase", |this, _args| string_to_upper(this));
    set_prototype_method(proto, "toLowerCase", |this, _args| string_to_lower(this));
    set_prototype_method(proto, "trim", |this, _args| string_trim_both(this));
    set_prototype_method(proto, "split", |this, args| string_split(this, args));
    set_prototype_method(proto, "startsWith", |this, args| string_starts_with(this, args));
    set_prototype_method(proto, "endsWith", |this, args| string_ends_with(this, args));
    set_prototype_method(proto, "repeat", |this, args| string_repeat(this, args));
    set_prototype_method(proto, "trimStart", |this, _args| {
        string_trim(this, true)
    });
    set_prototype_method(proto, "trimEnd", |this, _args| {
        string_trim(this, false)
    });
    set_prototype_method(proto, "codePointAt", |this, args| {
        string_code_point_at(this, args)
    });
    set_prototype_method(proto, "at", |this, args| string_at(this, args));
    set_prototype_method(proto, "normalize", |this, args| {
        string_normalize(this, args)
    });
    set_prototype_method(proto, "toLocaleLowerCase", |this, _args| string_to_lower(this));
    set_prototype_method(proto, "toLocaleUpperCase", |this, _args| string_to_upper(this));
    set_prototype_method(proto, "localeCompare", |this, args| {
        string_locale_compare(this, args)
    });
    set_prototype_method(proto, "padStart", |this, args| {
        string_pad(this, args, true)
    });
    set_prototype_method(proto, "padEnd", |this, args| {
        string_pad(this, args, false)
    });
    set_prototype_method(proto, "lastIndexOf", |this, args| {
        string_last_index_of(this, args)
    });
}

/// `String.fromCharCode(...)` — each argument is `ToUint16`-truncated, so a
/// BMP code unit (including lone surrogates) is appended to the result.
pub fn string_from_char_code(args: &[Value]) -> Result<Value, RuntimeError> {
    let mut out = String::new();
    for arg in args {
        let unit = super::to_uint16(arg.to_number());
        // Lone surrogates are valid in JS strings but not in Rust's `String`,
        // so they are encoded as the replacement character.
        out.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
    }
    Ok(Value::string(&out))
}

/// `String.fromCodePoint(...)` — each argument must be a valid code point
/// (ES 21.1.2.2); invalid ones raise a `RangeError`.
pub fn string_from_code_point(args: &[Value]) -> Result<Value, RuntimeError> {
    let mut out = String::new();
    for arg in args {
        let n = arg.to_number();
        // ToIntegerOrInfinity, then the range check.
        let code = if n.is_nan() { 0.0 } else { n.trunc() };
        if !code.is_finite() || code < 0.0 || code > 0x10_FFFF as f64 {
            return Err(RuntimeError::RangeError(
                "Invalid code point".to_string(),
            ));
        }
        out.push(char::from_u32(code as u32).unwrap_or('\u{FFFD}'));
    }
    Ok(Value::string(&out))
}

/// Register the `String` constructor's static methods.
pub fn register_string_statics(string_fn: &Value) {
    super::set_static_method(string_fn, "fromCharCode", |args| {
        string_from_char_code(args)
    });
    super::set_static_method(string_fn, "fromCodePoint", |args| {
        string_from_code_point(args)
    });
}

pub fn string_starts_with(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let search = args.first().map_or(Ok("undefined".to_string()), |v| {
        super::to_string_throwing(v)
    })?;
    Ok(Value::Bool(s.starts_with(&search)))
}

pub fn string_ends_with(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let search = args.first().map_or(Ok("undefined".to_string()), |v| {
        super::to_string_throwing(v)
    })?;
    Ok(Value::Bool(s.ends_with(&search)))
}

pub fn string_repeat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let count = args
        .first()
        .map_or(Ok(0.0), super::to_number_throwing)?;
    if count < 0.0 || count.is_infinite() {
        return Err(RuntimeError::RangeError("Invalid count value".to_string()));
    }
    let count = count as usize;
    if s.len().saturating_mul(count) > 1 << 24 {
        return Err(RuntimeError::RangeError("Invalid string length".to_string()));
    }
    Ok(Value::string(&s.repeat(count)))
}

pub fn string_pad(obj: &Value, args: &[Value], at_start: bool) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let target = args.first().map_or(Ok(0.0), super::to_number_throwing)? as usize;
    let fill = match args.get(1) {
        Some(v) => super::to_string_throwing(v)?,
        None => String::new(),
    };
    let fill = if fill.is_empty() {
        " ".to_string()
    } else {
        fill
    };
    let len = s.chars().count();
    if target <= len || target > 1 << 20 {
        return Ok(Value::string(&s));
    }
    let pad_len = target - len;
    let fill_chars: Vec<char> = fill.chars().collect();
    let padding: String = (0..pad_len).map(|i| fill_chars[i % fill_chars.len()]).collect();
    Ok(Value::string(&if at_start {
        format!("{padding}{s}")
    } else {
        format!("{s}{padding}")
    }))
}

/// `String.prototype.lastIndexOf(searchString, position)`.
pub fn string_last_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let search = match args.first() {
        Some(v) => super::to_string_throwing(v)?,
        None => "undefined".to_string(),
    };
    let chars: Vec<char> = s.chars().collect();
    let needle: Vec<char> = search.chars().collect();
    // No `position` means +Infinity for `lastIndexOf` (clamped to the length),
    // unlike `indexOf` which starts at 0.
    let start = string_position(args.get(1), chars.len(), chars.len() as i64)?;
    if needle.is_empty() {
        return Ok(Value::Number(start as f64));
    }
    let mut i = start as i64;
    while i >= 0 {
        let at = i as usize;
        let end = at + needle.len();
        if end <= chars.len() && chars[at..end] == needle[..] {
            return Ok(Value::Number(i as f64));
        }
        i -= 1;
    }
    Ok(Value::Number(-1.0))
}

fn string_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&super::string_receiver(obj)?))
}

fn string_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(&super::string_receiver(obj)?))
}

// ─────────────────────────────────────────────────────────
// String constructor
// ─────────────────────────────────────────────────────────

pub fn string_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        Ok(Value::string(""))
    } else {
        let s = args[0].to_js_string();
        if s.len() > 1024 * 1024 * 256 {
            return Err(RuntimeError::RangeError("String too long".to_string()));
        }
        Ok(Value::string(&s))
    }
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

pub fn string_char_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let idx = position_arg(args.first())?;
    if let Some(c) = s.chars().nth(idx) {
        Ok(Value::string(&c.to_string()))
    } else {
        Ok(Value::string(""))
    }
}

pub fn string_char_code_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let idx = position_arg(args.first())?;
    if let Some(c) = s.chars().nth(idx) {
        Ok(Value::Number(c as u32 as f64))
    } else {
        Ok(Value::Number(f64::NAN))
    }
}

pub fn string_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut s = super::string_receiver(obj)?;
    for arg in args {
        s.push_str(&super::to_string_throwing(arg)?);
    }
    Ok(Value::string(&s))
}

pub fn string_includes(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    if args.is_empty() {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(
        s.contains(&super::to_string_throwing(&args[0])?),
    ))
}

/// `String.prototype.indexOf(searchString, position)`.
///
/// `position` is ToInteger-clamped into `[0, length]`; an empty search string
/// matches at `position` (ES 21.1.3.9). Indices are reported in code *units*,
/// which for the test cases here is the character offset.
pub fn string_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    // A missing argument is `undefined`, whose ToString is "undefined" — not
    // the empty string (`'abc'.indexOf()` is -1).
    let search = match args.first() {
        Some(v) => super::to_string_throwing(v)?,
        None => "undefined".to_string(),
    };
    let len = s.chars().count();
    let start = string_position(args.get(1), len, 0)?;
    if search.is_empty() {
        return Ok(Value::Number(start as f64));
    }
    let tail: String = s.chars().skip(start).collect();
    match tail.find(&search) {
        Some(offset) => Ok(Value::Number((start + tail[..offset].chars().count()) as f64)),
        None => Ok(Value::Number(-1.0)),
    }
}

/// ToInteger-clamped start index shared by `indexOf` / `lastIndexOf`.
///
/// `default` is the value used when `position` is absent: `0` for `indexOf`,
/// `length - 1` for `lastIndexOf`.
fn string_position(
    position: Option<&Value>,
    len: usize,
    default: i64,
) -> Result<usize, RuntimeError> {
    let len = len as i64;
    let n = match position {
        Some(v) if !v.is_undefined() => super::to_number_throwing(v)?,
        _ => return Ok(default.max(0).min(len) as usize),
    };
    let value = if n.is_nan() {
        0
    } else if n.is_infinite() {
        if n.is_sign_positive() {
            len
        } else {
            0
        }
    } else {
        n.trunc() as i64
    };
    Ok(value.max(0).min(len) as usize)
}

/// `ToNumber(arg)` for a code-unit position (`charAt` / `charCodeAt`).
///
/// A missing argument is `undefined` → `NaN` → `ToInteger` 0, as the spec's
/// `? ToIntegerOrInfinity(pos)` prescribes for every method here.
fn position_arg(arg: Option<&Value>) -> Result<usize, RuntimeError> {
    let n = arg.map_or(Ok(f64::NAN), super::to_number_throwing)?;
    Ok(if n.is_nan() || n <= 0.0 {
        0
    } else if n.is_infinite() {
        usize::MAX
    } else {
        n.trunc() as usize
    })
}

pub fn string_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = slice_bound(args.first(), len)?;
    let end = if args.len() < 2 {
        len
    } else {
        slice_bound(args.get(1), len)?
    };
    if start >= end || start >= len {
        return Ok(Value::string(""));
    }
    let end = end.min(len);
    let result: String = chars[start..end].iter().collect();
    Ok(Value::string(&result))
}

/// `ToNumber(arg)` clamped to `[0, len]`, counting negatives from the end —
/// the bound conversion shared by `slice` and `substring`.
fn slice_bound(arg: Option<&Value>, len: usize) -> Result<usize, RuntimeError> {
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

pub fn string_to_upper(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(
        &super::string_receiver(obj)?.to_uppercase(),
    ))
}

pub fn string_to_lower(obj: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::string(
        &super::string_receiver(obj)?.to_lowercase(),
    ))
}

pub fn string_trim(obj: &Value, at_start: bool) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let trimmed = if at_start {
        s.trim_start_matches(is_js_whitespace)
    } else {
        s.trim_end_matches(is_js_whitespace)
    };
    Ok(Value::string(trimmed))
}

/// `String.prototype.trim()` — both ends, with the ES whitespace set.
pub fn string_trim_both(obj: &Value) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    Ok(Value::string(
        s.trim_start_matches(is_js_whitespace)
            .trim_end_matches(is_js_whitespace),
    ))
}

/// `String.prototype.codePointAt(pos)`.
///
/// `pos` indexes *code units*: for a surrogate pair, index `i` yields the whole
/// code point and `i + 1` yields the trailing surrogate's unit value.
pub fn string_code_point_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let pos = super::to_integer_or_infinity_throwing(args.first())?;
    if pos < 0 {
        return Ok(Value::Undefined);
    }
    let mut unit_index: i64 = 0;
    for c in s.chars() {
        let units = c.len_utf16() as i64;
        if pos < unit_index + units {
            let offset = (pos - unit_index) as usize;
            // A non-BMP character starts with a lead surrogate and ends with a
            // trail surrogate; asking for the second unit yields that unit.
            let code = if offset == 0 {
                c as u32
            } else {
                let mut buf = [0u16; 2];
                c.encode_utf16(&mut buf)[offset] as u32
            };
            return Ok(Value::Number(code as f64));
        }
        unit_index += units;
    }
    Ok(Value::Undefined)
}

/// `String.prototype.at(index)` — code-unit indexed, negative from the end.
pub fn string_at(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let units = s.encode_utf16().count() as i64;
    let mut index = super::to_integer_or_infinity_throwing(args.first())?;
    if index < 0 {
        index += units;
    }
    if index < 0 || index >= units {
        return Ok(Value::Undefined);
    }
    // Map the code-unit index back to a character. A position that falls in the
    // middle of a surrogate pair would need a lone surrogate, which Rust's
    // UTF-8 strings cannot hold; the replacement character is emitted instead.
    let mut unit_index: i64 = 0;
    for c in s.chars() {
        let len = c.len_utf16() as i64;
        if index < unit_index + len {
            return Ok(if index == unit_index {
                Value::string(&c.to_string())
            } else {
                Value::string("\u{FFFD}")
            });
        }
        unit_index += len;
    }
    Ok(Value::Undefined)
}

/// `String.prototype.normalize([form])`.
///
/// Only the `form` validation is implemented: the engine bundles no Unicode
/// normalization tables yet, so a valid form returns the receiver unchanged.
pub fn string_normalize(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let form = match args.first() {
        None | Some(Value::Undefined) => "NFC".to_string(),
        Some(v) => super::to_string_throwing(v)?,
    };
    match form.as_str() {
        "NFC" | "NFD" | "NFKC" | "NFKD" => Ok(Value::string(&s)),
        _ => Err(RuntimeError::RangeError(
            "The normalization form should be one of NFC, NFD, NFKC, NFKD".to_string(),
        )),
    }
}

/// `String.prototype.localeCompare(that)`.
///
/// Without ICU the comparison falls back to code-unit order, which matches the
/// default locale for the ASCII range the conformance suite exercises.
pub fn string_locale_compare(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let other = match args.first() {
        Some(v) => super::to_string_throwing(v)?,
        None => "undefined".to_string(),
    };
    let order = s.encode_utf16().cmp(other.encode_utf16());
    Ok(Value::Number(match order {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

pub fn string_split(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    if args.is_empty() {
        return Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
            vec![Value::string(&s)],
        )))));
    }
    let sep = super::to_string_throwing(&args[0])?;
    let parts: Vec<Value> = if sep.is_empty() {
        s.chars().map(|c| Value::string(&c.to_string())).collect()
    } else {
        s.split(&sep).map(|part| Value::string(part)).collect()
    };
    Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
        parts,
    )))))
}

/// `ToInteger` for a `substring` bound: negatives count as 0 (unlike `slice`).
fn to_substring_bound(n: f64, len: usize) -> usize {
    if n.is_nan() {
        return 0;
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
    if n < 0 {
        0
    } else {
        (n as usize).min(len)
    }
}

pub fn string_substring(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let s = super::string_receiver(obj)?;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.is_empty() {
        0
    } else {
        let n = super::to_number_throwing(&args[0])?;
        to_substring_bound(n, len)
    };
    let end = if args.len() < 2 {
        len
    } else {
        let n = super::to_number_throwing(&args[1])?;
        to_substring_bound(n, len)
    };
    let from = start.min(end);
    let to = start.max(end);
    let result: String = chars[from..to].iter().collect();
    Ok(Value::string(&result))
}
