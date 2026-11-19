//! The `console` host object.
//!
//! Deliberately *not* part of ES — it is a host extension — but it is the one
//! thing a script cannot do without: without it the only way to observe a value
//! is to `throw` it. `console.log` is what makes the engine usable for everyday
//! scripts, so it is registered as an ordinary global object whose methods
//! carry their own implementation (same shape as `Math`).
//!
//! Scope is the everyday surface: `log`/`info`/`debug`/`trace` go to stdout,
//! `warn`/`error` to stderr, and arguments are joined with a space the way the
//! familiar hosts do. Formatting is intentionally plain — strings print raw,
//! objects and arrays fall back to `JSON.stringify` — so this stays a debugging
//! aid rather than a spec surface.

use crate::RuntimeError;
use crate::vm::gc::{Heap, Object};
use crate::vm::native::{HeapImpl, NativeEntry};
use crate::vm::object::{NativeFunctionObject, OrdinaryObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// Labels handed to `console.time`, with the instant they were started.
///
/// Module-level (process-wide) on purpose: a timer is a debugging aid spanning
/// one host session, not per-VM state — the same reason `console` itself is a
/// host object rather than an ES one.
thread_local! {
    static TIMERS: std::cell::RefCell<std::collections::HashMap<String, std::time::Instant>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Build the `console` object: a plain namespace object (not a constructor).
pub fn create_console_object(heap: &mut Heap) -> Value {
    // 「是哪个方法」在注册时定一次（每条一个 `fn` 指针），调用点不再 match 名字。
    let methods: &[(&str, HeapImpl)] = &[
        ("log", |h, _t, a| print_line(h, a, Channel::Out)),
        ("info", |h, _t, a| print_line(h, a, Channel::Out)),
        ("debug", |h, _t, a| print_line(h, a, Channel::Out)),
        ("trace", |h, _t, a| print_line(h, a, Channel::Out)),
        ("warn", |h, _t, a| print_line(h, a, Channel::Err)),
        ("error", |h, _t, a| print_line(h, a, Channel::Err)),
        ("time", |_h, _t, a| time(a)),
        ("timeEnd", |_h, _t, a| time_end(a)),
    ];
    let mut console = OrdinaryObject::new();
    for (name, entry) in methods {
        let key = PropertyKey::intern(heap, name);

        let value = Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(&format!("console.{name}"))
                .with_entry(NativeEntry::Heap(*entry)),
        )));
        let _ = console.define_property(heap, key,
            PropertyDescriptor {
                value,
                writable: true,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
    }
    Value::Object(heap.alloc(Object::Ordinary(console)))
}

/// 诊断信息走 stderr，这样 `> out.txt` 不会把它们吞掉。
#[derive(Clone, Copy)]
enum Channel {
    Out,
    Err,
}

/// `console.log` / `info` / `debug` / `trace` / `warn` / `error`: arguments
/// joined by a space, answer `undefined`.
fn print_line(
    heap: &mut Heap,
    args: &[Value],
    channel: Channel,
) -> Result<Value, RuntimeError> {
    let line: Vec<String> = args.iter().map(|v| format_arg(heap, v)).collect();
    let line = line.join(" ");
    match channel {
        Channel::Err => eprintln!("{line}"),
        Channel::Out => println!("{line}"),
    }
    Ok(Value::Undefined)
}

/// `console.time(label)`: remember the instant under `label`.
fn time(args: &[Value]) -> Result<Value, RuntimeError> {
    let label = args
        .first()
        .map(|v| v.to_js_string())
        .unwrap_or_else(|| "default".to_string());
    TIMERS.with(|timers| {
        timers.borrow_mut().insert(label, std::time::Instant::now());
    });
    Ok(Value::Undefined)
}

/// `console.timeEnd(label)`: print the elapsed time (or report an unknown label).
fn time_end(args: &[Value]) -> Result<Value, RuntimeError> {
    let label = args
        .first()
        .map(|v| v.to_js_string())
        .unwrap_or_else(|| "default".to_string());
    match TIMERS.with(|timers| timers.borrow_mut().remove(&label)) {
        Some(instant) => println!("{label}: {:.3}ms", instant.elapsed().as_secs_f64() * 1000.0),
        None => eprintln!("Timer '{label}' does not exist"),
    }
    Ok(Value::Undefined)
}

/// How one argument is rendered.
///
/// Strings print raw (that is what makes `console.log("hi")` read `hi` instead
/// of `"hi"`); anything structured falls back to `JSON.stringify` so objects
/// are inspectable rather than `[object Object]`. A value `stringify` refuses
/// (a cycle, a `BigInt`) degrades to the ordinary `ToString`.
fn format_arg(heap: &mut Heap, value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string(),
        Value::Object(_) => match super::json::json_stringify(heap, value) {
            Ok(Some(text)) => text,
            _ => value.to_js_string(),
        },
        _ => value.to_js_string(),
    }
}
