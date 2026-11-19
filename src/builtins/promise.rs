//! `Promise` (ES 27.2) — the parts that need no user code.
//!
//! The object and the microtask queue live in the VM: `then` / `catch` have to
//! *call* the handlers, and `new Promise(executor)` has to call the executor
//! with two functions that settle the promise. What is left here is the
//! registration (so the names resolve) and the shape checks.

use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::JSObject;
use crate::vm::value::Value;

/// Dispatch name of `get Promise[Symbol.species]` (ES 27.2.2.3). The getter
/// returns its receiver, so the VM answers it (`Map`/`Set`/`Array` do the same).
pub const PROMISE_SPECIES_NATIVE: &str = "__promise_species__";

/// `Promise.prototype.then` / `catch` / `finally`: the VM intercepts all three
/// before the ordinary prototype dispatch, so these are registered for lookups
/// (`p.then` must be a function) but never reached.
pub fn register_promise_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    use super::set_prototype_method_impl;
    use crate::vm::native::VmSlot;

    // 三个方法都要回调用户函数 / 入微任务队列，走 `NativeEntry::Vm`。
    // 归属由函数身份决定：`obj.then` 若是用户自己的函数，根本不会到这里。
    for (name, slot) in [
        ("then", VmSlot::PromiseThen),
        ("catch", VmSlot::PromiseCatch),
        ("finally", VmSlot::PromiseFinally),
    ] {
        set_prototype_method_impl(proto, heap, name, slot.entry());
    }
}

/// `Promise.resolve` / `Promise.reject` as own properties of the constructor.
///
/// The names have to resolve before the VM's static-method interception can
/// claim them — without this they are `undefined` and the interception never
/// runs.
pub fn register_promise_statics(ctor: &Value, heap: &mut Heap) {
    use super::set_static_method_impl;
    use crate::vm::native::VmSlot;

    // 六个静态方法需要 VM（建 promise / 入微任务队列 / 读 `this.resolve`）。
    // 这里只登记符号，`VmSlot::entry()` 在 vm/native.rs 里解析成函数指针。
    for (name, slot) in [
        ("resolve", VmSlot::PromiseResolve),
        ("reject", VmSlot::PromiseReject),
        ("all", VmSlot::PromiseAll),
        ("race", VmSlot::PromiseRace),
        ("allSettled", VmSlot::PromiseAllSettled),
        ("any", VmSlot::PromiseAny),
    ] {
        set_static_method_impl(ctor, name, heap, slot.entry());
    }
}
