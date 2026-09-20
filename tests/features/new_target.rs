//! Feature tests for `new.target` (ES 14.2.3 / 12.3.5.1).
//!
//! `new.target` is per frame: the constructor for a `[[Construct]]` frame,
//! `undefined` for an ordinary call, inherited by arrows from the frame that
//! created them, and propagated through `super()`.

use crate::helpers::{eval_bool, eval_string};

#[test]
fn new_target_is_undefined_in_a_plain_call() {
    assert_eq!(
        eval_string("function f() { return String(new.target); } f()"),
        "undefined"
    );
    assert_eq!(
        eval_string("var o = { m: function () { return String(new.target); } }; o.m()"),
        "undefined"
    );
    // `Function.prototype.call` is still a plain call.
    assert_eq!(
        eval_string("function f() { return String(new.target); } f.call(null)"),
        "undefined"
    );
}

#[test]
fn new_target_is_the_constructor_under_new() {
    assert_eq!(
        eval_bool(
            "var seen;
             function F() { seen = new.target; }
             var f = new F();
             seen === F && f instanceof F"
        ),
        true
    );
    // `new F` without parentheses behaves the same.
    assert_eq!(
        eval_bool(
            "var seen;
             function F() { seen = new.target; }
             new F;
             seen === F"
        ),
        true
    );
}

#[test]
fn new_target_within_class_constructor() {
    assert_eq!(
        eval_bool(
            "var seen;
             class C { constructor() { seen = new.target; } }
             new C();
             seen === C"
        ),
        true
    );
}

#[test]
fn derived_class_sees_the_class_being_constructed() {
    // Every constructor in the chain observes the outermost `new` target.
    assert_eq!(
        eval_string(
            "var baseNt, parentNt, childNt;
             class Base { constructor() { baseNt = new.target.name; } }
             class Parent extends Base { constructor() { parentNt = new.target.name; super(); } }
             class Child extends Parent { constructor() { childNt = new.target.name; super(); } }
             new Child();
             childNt + ',' + parentNt + ',' + baseNt"
        ),
        "Child,Child,Child"
    );
}

#[test]
fn arrow_function_inherits_new_target() {
    // An arrow has no [[Construct]]: it sees the enclosing frame's new.target.
    assert_eq!(
        eval_bool(
            "var seen;
             function F() { var arrow = () => new.target; seen = arrow(); }
             new F();
             seen === F"
        ),
        true
    );
    assert_eq!(
        eval_string(
            "function F() { var arrow = () => new.target; return String(arrow()); }
             F()"
        ),
        "undefined"
    );
}

#[test]
fn new_target_in_class_method_is_undefined() {
    // A method is not a constructor frame, even when reached through an instance.
    assert_eq!(
        eval_string("class C { m() { return String(new.target); } } new C().m()"),
        "undefined"
    );
}
