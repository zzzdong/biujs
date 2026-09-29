//! `Promise` (ES 27.2) — the parts that need no user code.
//!
//! The object and the microtask queue live in the VM: `then` / `catch` have to
//! *call* the handlers, and `new Promise(executor)` has to call the executor
//! with two functions that settle the promise. What is left here is the
//! registration (so the names resolve) and the shape checks.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::JSObject;
use crate::vm::value::Value;

/// Dispatch name of `get Promise[Symbol.species]` (ES 27.2.2.3). The getter
/// returns its receiver, so the VM answers it (`Map`/`Set`/`Array` do the same).
pub const PROMISE_SPECIES_NATIVE: &str = "__promise_species__";

/// `Promise.prototype.then` / `catch` / `finally`: the VM intercepts all three
/// before the ordinary prototype dispatch, so these are registered for lookups
/// (`p.then` must be a function) but never reached.
pub fn register_promise_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;
    for name in ["then", "catch", "finally"] {
        set_prototype_method(proto, name, |_this, _args| {
            Err(RuntimeError::InternalError(
                "Promise methods are dispatched by the VM".to_string(),
            ))
        });
    }
    let _ = proto;
    let _ = Value::Undefined;
}

/// `Promise.resolve` / `Promise.reject` as own properties of the constructor.
///
/// The names have to resolve before the VM's static-method interception can
/// claim them — without this they are `undefined` and the interception never
/// runs.
pub fn register_promise_statics(ctor: &Value) {
    let Value::Object(obj_ref) = ctor else {
        return;
    };
    for name in ["resolve", "reject", "all", "race", "allSettled", "any"] {
        let _ = obj_ref.borrow_mut().define_property(
            crate::vm::property::PropertyKey::from_str(name),
            crate::vm::property::PropertyDescriptor {
                value: Value::Object(Rc::new(RefCell::new(
                    crate::vm::object::NativeFunctionObject::new(&format!("Promise.{name}")),
                ))),
                writable: true,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
    }
}
