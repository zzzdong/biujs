//! `Proxy` (ES 28.2) — a target plus a handler, where every operation on the
//! former is routed through a trap on the latter.
//!
//! The exotic object itself lives in `vm::object::ProxyObject`; the traps are
//! JavaScript, so *dispatching* them is the VM's job (`VM::proxy_*` in
//! `vm/mod.rs`). What is left here needs no JS: `new Proxy`'s two argument
//! checks and the `Proxy` constructor object.
//!
//! The organising rule is ES 10.5: an operation with **no** trap is not an
//! error, it is the same operation on the target. That is why `ProxyObject`'s
//! object-layer methods simply forward — they cannot run JavaScript, so the
//! traps that must (`get` / `set` / `has` / `deleteProperty` / `apply` /
//! `construct`) are dispatched by the VM before control ever reaches them.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{NativeFunctionObject, ProxyObject};
use crate::vm::value::Value;

/// The thirteen traps (ES 10.5), so that the VM's dispatch and this file agree
/// on the spelling. A trap the handler does not have is "forward to the target",
/// which is why an empty handler is a working (if pointless) proxy.
pub const TRAPS: [&str; 13] = [
    "get",
    "set",
    "has",
    "deleteProperty",
    "defineProperty",
    "getOwnPropertyDescriptor",
    "ownKeys",
    "getPrototypeOf",
    "setPrototypeOf",
    "isExtensible",
    "preventExtensions",
    "apply",
    "construct",
];

/// `new Proxy(target, handler)` (ES 28.2.1.1).
///
/// Both arguments must be objects and neither is coerced — that is the whole of
/// the constructor's own logic; the object it answers is inert until something
/// operates on it, and every such operation is a trap lookup.
pub fn proxy_construct(target: Value, handler: Value) -> Result<Value, RuntimeError> {
    if !target.is_object() {
        return Err(RuntimeError::TypeError(
            "Cannot create proxy with a non-object as target".to_string(),
        ));
    }
    if !handler.is_object() {
        return Err(RuntimeError::TypeError(
            "Cannot create proxy with a non-object as handler".to_string(),
        ));
    }
    // A proxy has no `[[Prototype]]` of its own: `Object.getPrototypeOf(p)`
    // answers the *target's* (ES 10.5.20). The base therefore only matters once
    // the proxy is revoked — and then every operation throws anyway.
    let proto = crate::builtins::wrapper_prototype("Object");
    Ok(Value::Object(Rc::new(RefCell::new(ProxyObject::new(
        target, handler, proto,
    )))))
}

/// The `Proxy` constructor object (ES 28.2.2): `length` 2, and **no**
/// `prototype` property — instances inherit from their target, so there is
/// nothing for them to inherit from here.
///
/// `revocable` is registered as a name only: the revoke function has to
/// remember *which* proxy it belongs to, so the VM mints it (the same trick
/// `Function.prototype.bind` uses for its wrapper).
pub fn create_proxy_constructor() -> Value {
    let ctor = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new("Proxy"))));
    crate::builtins::set_static_method(&ctor, "revocable", |_args: &[Value]| {
        Err(RuntimeError::InternalError(
            "Proxy.revocable is dispatched by the VM".to_string(),
        ))
    });
    ctor
}
