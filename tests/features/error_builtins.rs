//! Error/Function built-in semantics (M3-B5): error prototypes, `[[Class]]`,
//! `Error.isError`/`cause`, built-in `C.prototype` attributes, bound-function
//! construction.

use crate::helpers::{eval_bool, eval_number, eval_string};

// ──────────────────────────────
// Error prototypes and instances
// ──────────────────────────────

#[test]
fn error_instances_report_error_class() {
    assert_eq!(
        eval_string("Object.prototype.toString.call(new TypeError('x'))"),
        "[object Error]"
    );
    assert_eq!(
        eval_string("Object.prototype.toString.call(Error('x'))"),
        "[object Error]"
    );
}

#[test]
fn native_error_prototypes_own_name_and_message() {
    assert_eq!(eval_string("TypeError.prototype.name"), "TypeError");
    assert!(eval_bool("TypeError.prototype.hasOwnProperty('message')"));
    assert_eq!(eval_string("TypeError.prototype.message"), "");
    assert_eq!(eval_string("Error.prototype.name"), "Error");
    // Instances inherit `name` instead of shadowing it.
    assert!(eval_bool("new TypeError('x').hasOwnProperty('name') === false"));
}

#[test]
fn error_to_string_follows_the_spec() {
    assert_eq!(eval_string("new Error('boom').toString()"), "Error: boom");
    assert_eq!(eval_string("new TypeError('boom').toString()"), "TypeError: boom");
    assert_eq!(eval_string("new Error().toString()"), "Error");
    assert_eq!(
        eval_string("(function () { var e = new Error('x'); e.name = ''; return e.toString(); })()"),
        "x"
    );
    assert!(eval_bool(
        "(function () { var e = new Error('x'); e.message = undefined; \
           return e.toString() === 'Error'; })()"
    ));
}

#[test]
fn error_is_error_and_cause() {
    assert!(eval_bool("Error.isError(new Error())"));
    assert!(eval_bool("Error.isError(new TypeError())"));
    assert!(eval_bool("Error.isError({}) === false"));
    assert!(eval_bool("Error.isError('x') === false"));
    assert_eq!(
        eval_string("(function () { var e = new Error('m', { cause: 42 }); return e.cause + ''; })()"),
        "42"
    );
    assert!(eval_bool(
        "(function () { var e = new Error('m', { cause: 1 }); \
           return e.hasOwnProperty('cause') && \
             Object.getOwnPropertyDescriptor(e, 'cause').enumerable === false; })()"
    ));
}

// ──────────────────────────────
// Built-in constructors
// ──────────────────────────────

#[test]
fn builtin_constructors_have_a_prototype_chain() {
    assert!(eval_bool("Object.getPrototypeOf(Array) === Function.prototype"));
    assert!(eval_bool("Array instanceof Function"));
    assert!(eval_bool("Array.prototype.isPrototypeOf(new Array())"));
    assert!(eval_bool("Array.prototype.isPrototypeOf(Array())"));
}

#[test]
fn builtin_prototype_property_is_read_only() {
    assert!(eval_bool(
        "(function () { var d = Object.getOwnPropertyDescriptor(Array, 'prototype'); \
           return d.writable === false && d.enumerable === false && d.configurable === false; })()"
    ));
    assert!(eval_bool(
        "(function () { var d = Object.getOwnPropertyDescriptor(Error, 'prototype'); \
           return d.writable === false && d.configurable === false; })()"
    ));
}

#[test]
fn new_with_builtin_constructors() {
    assert_eq!(eval_string("new String('x').length + ''"), "1");
    assert!(eval_bool("typeof new Number(5) === 'object'"));
    assert!(eval_bool("new Object() instanceof Object"));
    assert_eq!(eval_number("new Array(...[1, 2]).length"), 2.0);
    assert_eq!(eval_string("String(new Array(2, 4))"), "2,4");
}

#[test]
fn bound_functions_can_be_constructed() {
    let js = r#"
        function F(a, b) { this.sum = a + b; }
        var B = F.bind(null, 1);
        var o = new B(2);
        o.sum
    "#;
    assert_eq!(eval_number(js), 3.0);
    assert!(eval_bool(
        "(function () { function F(a, b) { this.sum = a + b; } \
           var B = F.bind(null, 1); \
           return new B(2) instanceof F; })()"
    ));
}
