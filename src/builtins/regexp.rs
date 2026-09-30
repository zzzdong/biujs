//! `RegExp` (ES 22.2) and the regex-aware `String.prototype` methods.
//!
//! The engine has no pattern compiler of its own: `RegExpObject` holds a
//! `regex::Regex`. That covers the everyday shapes — character classes,
//! quantifiers, alternation, groups — but not the constructs JS has and the
//! crate cannot express (lookaround, backreferences); those are reported as
//! unsupported rather than silently mismatching.
//!
//! `lastIndex` is a real slot on the object (it drives `g`/`y` matching), so it
//! is intercepted in `property_get` / `property_set` instead of being an own
//! property — otherwise `re.lastIndex = 3` would never reach the matcher.

use std::cell::{RefCell, RefMut};
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{ArrayObject, JSObject, NativeFunctionObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// True when `this` is a `RegExp` instance.
pub fn is_regexp_receiver(this: &Value) -> bool {
    matches!(this, Value::Object(obj)
        if obj.borrow().kind() == crate::vm::property::ObjectKind::RegExp)
}

fn as_regexp<'a>(
    this: &'a Value,
    method: &str,
) -> Result<RefMut<'a, crate::vm::object::RegExpObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "RegExp.prototype.{method} called on a non-object"
        )));
    };
    let borrowed = obj_ref.borrow_mut();
    if borrowed.kind() != crate::vm::property::ObjectKind::RegExp {
        return Err(RuntimeError::TypeError(format!(
            "RegExp.prototype.{method} called on an incompatible receiver"
        )));
    }
    Ok(RefMut::map(borrowed, |obj| {
        obj.as_any_mut()
            .downcast_mut::<crate::vm::object::RegExpObject>()
            .expect("ObjectKind::RegExp implies RegExpObject")
    }))
}

/// Normalise flags: drop duplicates, keep the source order, reject anything
/// outside the ES set (ES 22.2.1.1 step 6 would throw; we throw too).
fn normalise_flags(flags: &str) -> Result<String, RuntimeError> {
    let mut out = String::new();
    for c in flags.chars() {
        if !matches!(c, 'd' | 'g' | 'i' | 'm' | 's' | 'u' | 'v' | 'y') {
            return Err(RuntimeError::SyntaxError(format!(
                "Invalid regular expression flags: {flags}"
            )));
        }
        if !out.contains(c) {
            out.push(c);
        }
    }
    Ok(out)
}

/// `new RegExp(pattern[, flags])`, and `RegExp(...)` without `new`.
///
/// A `RegExp` argument is copied (with the flags defaulting to the argument's),
/// which is the ES behaviour and what makes `new RegExp(re, 'g')` work.
pub fn regexp_construct_value(args: &[Value], proto: Option<Value>) -> Result<Value, RuntimeError> {
    let (source, flags) = match args.first() {
        None => (String::new(), String::new()),
        Some(Value::Object(obj))
            if obj.borrow().kind() == crate::vm::property::ObjectKind::RegExp =>
        {
            let (source, own_flags) = {
                let borrowed = obj.borrow();
                let re = borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::RegExpObject>()
                    .expect("ObjectKind::RegExp implies RegExpObject");
                (re.source.clone(), re.flags.clone())
            };
            // ES 22.2.1.1: with a RegExp argument, `flags` must be undefined.
            let flags = match args.get(1) {
                Some(v) if !v.is_undefined() => normalise_flags(&v.to_js_string())?,
                _ => own_flags,
            };
            (source, flags)
        }
        Some(v) => (
            v.to_js_string(),
            match args.get(1) {
                Some(f) if !f.is_undefined() => normalise_flags(&f.to_js_string())?,
                _ => String::new(),
            },
        ),
    };
    let proto = match proto {
        Some(Value::Object(p)) => Some(p),
        _ => None,
    };
    Ok(Value::Object(Rc::new(RefCell::new(
        crate::vm::object::RegExpObject::new(source, flags, proto),
    ))))
}

/// The matcher, or a TypeError for a pattern the crate cannot compile
/// (lookaround, backreferences, …).
fn compiled<'a>(
    this: &'a Value,
    method: &str,
) -> Result<RefMut<'a, crate::vm::object::RegExpObject>, RuntimeError> {
    let re = as_regexp(this, method)?;
    if re.compiled.is_none() {
        return Err(RuntimeError::TypeError(format!(
            "RegExp.prototype.{method}: pattern {:?} is not supported by this engine",
            re.source
        )));
    }
    Ok(re)
}

pub fn regexp_test(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let text = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    let mut re = compiled(this, "test")?;
    let matcher = re.compiled.clone().expect("checked above");
    let from = if re.is_global() || re.is_sticky() {
        re.last_index.max(0.0) as usize
    } else {
        0
    };
    let found = matcher.find_at(text.as_str(), from).map(|m| m.end());
    // A `y` match has to start exactly at `lastIndex`; `g` merely resumes there.
    let hit = match found {
        Some(_) if re.is_sticky() => {
            matcher.find_at(text.as_str(), from).map(|m| m.start()) == Some(from)
        }
        Some(_) => true,
        None => false,
    };
    if hit {
        if let Some(end) = found {
            if re.is_global() || re.is_sticky() {
                re.last_index = end as f64;
            }
        }
        Ok(Value::Bool(true))
    } else {
        // No match resets `lastIndex` (ES 22.2.5.2 step 11).
        if re.is_global() || re.is_sticky() {
            re.last_index = 0.0;
        }
        Ok(Value::Bool(false))
    }
}

/// `exec` answers the match array (`0`, `1`… plus `index` and `input`) or null.
pub fn regexp_exec(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let text = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    let mut re = compiled(this, "exec")?;
    let matcher = re.compiled.clone().expect("checked above");
    let from = if re.is_global() || re.is_sticky() {
        re.last_index.max(0.0) as usize
    } else {
        0
    };
    let captures = matcher.captures_at(text.as_str(), from);
    let Some(captures) = captures else {
        if re.is_global() || re.is_sticky() {
            re.last_index = 0.0;
        }
        return Ok(Value::Null);
    };
    // A `y` match must start exactly at `lastIndex`.
    if re.is_sticky() && captures.get(0).map(|m| m.start()) != Some(from) {
        re.last_index = 0.0;
        return Ok(Value::Null);
    }
    let whole = captures.get(0).expect("a match implies group 0");
    let mut elements: Vec<Value> = Vec::new();
    for i in 0..captures.len() {
        elements.push(match captures.get(i) {
            Some(m) => Value::string(m.as_str()),
            None => Value::Undefined,
        });
    }
    let mut array = ArrayObject::from_vec(elements);
    let _ = array.define_property(
        PropertyKey::from_str("index"),
        PropertyDescriptor {
            value: Value::Number(whole.start() as f64),
            writable: true,
            enumerable: true,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
    let _ = array.define_property(
        PropertyKey::from_str("input"),
        PropertyDescriptor {
            value: Value::string(&text),
            writable: true,
            enumerable: true,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
    if re.is_global() || re.is_sticky() {
        re.last_index = whole.end() as f64;
    }
    Ok(Value::Object(Rc::new(RefCell::new(array))))
}

/// Every match of a global pattern over `text`, as match arrays (ES 22.2.5.8).
///
/// Serves both entry points, which are the same algorithm with the arguments
/// swapped: `String.prototype.matchAll(re)` (receiver is the string) and
/// `RegExp.prototype[Symbol.matchAll](s)` (receiver is the regexp).
///
/// The matches are collected eagerly and the VM turns them into an iterator. The
/// spec's iterator is lazy and advances a *clone* of the regexp, so a snapshot
/// is observationally equivalent for every use except mutating the pattern
/// mid-iteration.
pub fn match_all_matches(
    this: &Value,
    args: &[Value],
    regexp_proto: Option<Value>,
) -> Result<Vec<Value>, RuntimeError> {
    let (regexp_val, text) = if is_regexp_receiver(this) {
        (
            this.clone(),
            args.first().map(|v| v.to_js_string()).unwrap_or_default(),
        )
    } else {
        let text = this.to_js_string();
        let pattern = args.first().cloned().unwrap_or(Value::Undefined);
        // A non-RegExp argument is coerced with a fresh `g` (step 2): the whole
        // point of `matchAll` is *all* matches, so the flag is added for the
        // caller instead of rejecting the argument.
        let re = if is_regexp_receiver(&pattern) {
            pattern
        } else {
            regexp_construct_value(&[pattern, Value::string("g")], regexp_proto)?
        };
        (re, text)
    };
    let re = as_regexp(&regexp_val, "matchAll")?;
    if !re.is_global() {
        return Err(RuntimeError::TypeError(
            "String.prototype.matchAll called with a non-global RegExp argument".to_string(),
        ));
    }
    let matcher = match re.compiled.clone() {
        Some(m) => m,
        None => {
            return Err(RuntimeError::TypeError(format!(
                "RegExp.prototype.matchAll: pattern {:?} is not supported by this engine",
                re.source
            )))
        }
    };
    // The iteration runs on a *clone* (step 3), so the receiver's own
    // `lastIndex` is left where it was.
    let mut from = re.last_index.max(0.0) as usize;
    let mut out: Vec<Value> = Vec::new();
    while from <= text.len() {
        let Some(captures) = matcher.captures_at(text.as_str(), from) else {
            break;
        };
        let whole = captures.get(0).expect("a match implies group 0");
        out.push(match_array(&captures, &text));
        // An empty match would never advance, so the spec steps `lastIndex` by
        // one code unit in that case (step 6.a.ii).
        from = if whole.end() == whole.start() {
            whole.end() + 1
        } else {
            whole.end()
        };
    }
    Ok(out)
}

/// The match array for one set of captures: the groups, plus `index` / `input`.
fn match_array(captures: &regex::Captures, text: &str) -> Value {
    let mut elements: Vec<Value> = Vec::new();
    for i in 0..captures.len() {
        elements.push(match captures.get(i) {
            Some(m) => Value::string(m.as_str()),
            None => Value::Undefined,
        });
    }
    let mut array = ArrayObject::from_vec(elements);
    let index = captures.get(0).map(|m| m.start()).unwrap_or(0) as f64;
    // Same shape `exec` produces, including `groups` being present and
    // `undefined` when the pattern has no named groups.
    for (key, value) in [
        ("index", Value::Number(index)),
        ("input", Value::string(text)),
        ("groups", Value::Undefined),
    ] {
        let _ = array.define_property(
            PropertyKey::from_str(key),
            PropertyDescriptor {
                value,
                writable: true,
                enumerable: true,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
    }
    Value::Object(Rc::new(RefCell::new(array)))
}

/// `Symbol.matchAll` — the id the well-known symbol table in `builtins::mod`
/// registers for it.
const MATCH_ALL_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0009;

/// `toString`: `/source/flags` (ES 22.2.5.14).
pub fn regexp_to_string(this: &Value) -> Result<Value, RuntimeError> {
    let re = as_regexp(this, "toString")?;
    Ok(Value::string(&format!("/{}/{}", re.source, re.flags)))
}

pub fn register_regexp_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;
    set_prototype_method(proto, "test", |this, args| regexp_test(this, args));
    set_prototype_method(proto, "exec", |this, args| regexp_exec(this, args));
    set_prototype_method(proto, "toString", |this, _| regexp_to_string(this));
    // `RegExp.prototype[Symbol.matchAll]` is the other entry point of the same
    // algorithm (`String.prototype.matchAll` delegates to it). Handled by the
    // VM for the same reason: the result is an iterator.
    //
    // It has to be defined under the *symbol key*: registering it as the method
    // name "Symbol.matchAll" put it on the wrong key, and `re[Symbol.matchAll]`
    // then found nothing.
    let _ = proto.borrow_mut().define_property(
        PropertyKey::Symbol(MATCH_ALL_SYMBOL_ID),
        super::method_descriptor(Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new(&format!(
                "{}{}",
                super::PROTO_METHOD_PREFIX,
                "matchAll"
            )),
        )))),
    );
}

pub fn register_regexp_statics(ctor: &Value) {
    // `RegExp` has no own static methods in ES6; nothing to do. The hook exists
    // so the registration site reads like the other constructors'.
    let _ = ctor;
}

// ── 正则感知的 String.prototype 方法 ───────────────────────────────────────
//
// ES routes these through `Symbol.match` / `Symbol.search` / `Symbol.replace` /
// `Symbol.split` on the RegExp; the engine has no symbol-method dispatch for
// them yet, so the four are recognised here by the *shape of the argument*.

/// The matcher and flags of `value`, coercing a non-RegExp the way the spec does
/// (`"abc".match("b")` builds `new RegExp("b")`).
fn matcher_for(value: &Value) -> Result<(regex::Regex, String), RuntimeError> {
    if is_regexp_receiver(value) {
        let Value::Object(obj_ref) = value else {
            return Err(RuntimeError::TypeError("not a RegExp".to_string()));
        };
        let borrowed = obj_ref.borrow();
        let obj = borrowed
            .as_any()
            .downcast_ref::<crate::vm::object::RegExpObject>()
            .expect("ObjectKind::RegExp implies RegExpObject");
        let Some(compiled) = obj.compiled.clone() else {
            return Err(RuntimeError::TypeError(format!(
                "pattern {:?} is not supported by this engine",
                obj.source
            )));
        };
        Ok((compiled, obj.flags.clone()))
    } else {
        let source = value.to_js_string();
        let built = regexp_construct_value(&[Value::string(&source)], None)?;
        matcher_for(&built)
    }
}

/// `String.prototype.match(regexp)`: every match for `g`, otherwise the capture
/// array (`0` plus the groups, with `index` and `input`).
pub fn regexp_match(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let text = super::string_receiver(this)?;
    let arg = args.first().unwrap_or(&Value::Undefined);
    let (matcher, flags) = matcher_for(arg)?;
    let mut array = ArrayObject::new();
    if flags.contains('g') {
        for m in matcher.find_iter(text.as_str()) {
            array.push(Value::string(m.as_str()));
        }
        return Ok(Value::Object(Rc::new(RefCell::new(array))));
    }
    let Some(captures) = matcher.captures(text.as_str()) else {
        return Ok(Value::Null);
    };
    let whole = captures.get(0).expect("a match implies group 0");
    for i in 0..captures.len() {
        array.push(match captures.get(i) {
            Some(m) => Value::string(m.as_str()),
            None => Value::Undefined,
        });
    }
    let _ = array.define_property(
        PropertyKey::from_str("index"),
        PropertyDescriptor {
            value: Value::Number(whole.start() as f64),
            writable: true,
            enumerable: true,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
    let _ = array.define_property(
        PropertyKey::from_str("input"),
        PropertyDescriptor {
            value: Value::string(&text),
            writable: true,
            enumerable: true,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
    Ok(Value::Object(Rc::new(RefCell::new(array))))
}

/// `String.prototype.search(regexp)`: the index of the first match, or -1.
/// It ignores `lastIndex` and does not advance it (ES 22.2.5.9).
pub fn regexp_search(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let text = super::string_receiver(this)?;
    let arg = args.first().unwrap_or(&Value::Undefined);
    let (matcher, _) = matcher_for(arg)?;
    Ok(Value::Number(
        matcher
            .find(text.as_str())
            .map(|m| m.start() as f64)
            .unwrap_or(-1.0),
    ))
}

/// `String.prototype.split(separator[, limit])` with a RegExp separator.
pub fn regexp_split(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let text = super::string_receiver(this)?;
    let (matcher, _) = matcher_for(args.first().unwrap_or(&Value::Undefined))?;
    let limit = match args.get(1) {
        Some(v) if !v.is_undefined() => v.to_number() as usize,
        _ => usize::MAX,
    };
    let parts: Vec<Value> = matcher
        .split(text.as_str())
        .take(limit)
        .map(Value::string)
        .collect();
    Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(
        parts,
    )))))
}

/// `String.prototype.replace(searchValue, replaceValue)` with a RegExp,
/// including the `$$` / `$&` / `` $` `` / `$'` / `$n` substitutions.
pub fn regexp_replace(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let text = super::string_receiver(this)?;
    let (matcher, flags) = matcher_for(args.first().unwrap_or(&Value::Undefined))?;
    if args.len() > 1 && matches!(args[1], Value::Object(_)) {
        return Err(RuntimeError::TypeError(
            "String.prototype.replace: a function replacer is not supported yet".to_string(),
        ));
    }
    let replacement = match args.get(1) {
        Some(v) => v.to_js_string(),
        None => "undefined".to_string(),
    };
    let mut out = String::new();
    let mut last = 0usize;
    let mut done = false;
    for captures in matcher.captures_iter(text.as_str()) {
        if done {
            break;
        }
        let whole = captures.get(0).expect("a match implies group 0");
        out.push_str(&text[last..whole.start()]);
        out.push_str(&expand_replacement(
            &replacement,
            &captures,
            &text,
            whole.start(),
            whole.end(),
        ));
        last = whole.end();
        // Without `g` only the first match is replaced.
        if !flags.contains('g') {
            done = true;
        }
    }
    out.push_str(&text[last..]);
    Ok(Value::string(&out))
}

/// The `$` substitutions of `Symbol.replace` (ES 22.2.5.8 step 15).
fn expand_replacement(
    template: &str,
    captures: &regex::Captures<'_>,
    input: &str,
    start: usize,
    end: usize,
) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('&') => {
                chars.next();
                out.push_str(&input[start..end]);
            }
            Some('`') => {
                chars.next();
                out.push_str(&input[..start]);
            }
            Some('\'') => {
                chars.next();
                out.push_str(&input[end..]);
            }
            Some(d) if d.is_ascii_digit() => {
                let mut index = String::new();
                while let Some(d) = chars.peek() {
                    if d.is_ascii_digit() {
                        index.push(*d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let n: usize = index.parse().unwrap_or(0);
                if let Some(m) = captures.get(n) {
                    out.push_str(m.as_str());
                }
            }
            _ => out.push('$'),
        }
    }
    out
}
