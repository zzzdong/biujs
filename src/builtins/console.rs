//! The `console` host object.
//!
//! Deliberately *not* part of ES — it is a host extension — but it is the one
//! thing a script cannot do without: without it the only way to observe a value
//! is to `throw` it. `console.log` is what makes the engine usable for everyday
//! scripts, so it is registered as an ordinary global object whose methods carry
//! the `console.` prefix the VM dispatches on (exactly like `Math.`).
//!
//! Scope is the everyday surface: `log`/`info`/`debug`/`trace` go to stdout,
//! `warn`/`error` to stderr, and arguments are joined with a space the way the
//! familiar hosts do. Formatting is intentionally plain — strings print raw,
//! objects and arrays fall back to `JSON.stringify` — so this stays a debugging
//! aid rather than a spec surface.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{NativeFunctionObject, OrdinaryObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// The methods the object exposes.
const METHODS: &[&str] = &["log", "info", "debug", "trace", "warn", "error"];

/// Build the `console` object: a plain namespace object (not a constructor).
pub fn create_console_object() -> Value {
    let mut console = OrdinaryObject::new();
    for name in METHODS {
        let _ = console.define_property(
            PropertyKey::from_str(name),
            PropertyDescriptor {
                value: Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
                    &format!("console.{name}"),
                )))),
                writable: true,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
    }
    Value::Object(Rc::new(RefCell::new(console)))
}

/// Dispatch a `console.<method>` call. Each method prints its arguments joined
/// by a space and answers `undefined`.
pub fn call_console_method(name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
    if !METHODS.contains(&name) {
        return Err(RuntimeError::TypeError(format!(
            "console.{name} is not a function"
        )));
    }
    let line: Vec<String> = args.iter().map(format_arg).collect();
    let line = line.join(" ");
    match name {
        // Diagnostics belong on stderr so they survive `> out.txt`.
        "warn" | "error" => eprintln!("{line}"),
        _ => println!("{line}"),
    }
    Ok(Value::Undefined)
}

/// How one argument is rendered.
///
/// Strings print raw (that is what makes `console.log("hi")` read `hi` instead
/// of `"hi"`); anything structured falls back to `JSON.stringify` so objects
/// are inspectable rather than `[object Object]`. A value `stringify` refuses
/// (a cycle, a `BigInt`) degrades to the ordinary `ToString`.
fn format_arg(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string(),
        Value::Object(_) => match super::json::json_stringify(value) {
            Ok(Some(text)) => text,
            _ => value.to_js_string(),
        },
        _ => value.to_js_string(),
    }
}
