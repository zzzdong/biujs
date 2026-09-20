//! Feature tests for `super` beyond the single-level case: multi-level
//! inheritance chains, static members, accessors, arrows and object literals.
//!
//! The home object of a class member is fixed when the class is defined and
//! travels on the member function, so `super` stays correct no matter how deep
//! the chain is.

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn super_method_resolves_one_level_up_at_any_depth() {
    assert_eq!(
        eval_string(
            "class Base { m() { return 'base'; } }
             class Parent extends Base { m() { return 'parent>' + super.m(); } }
             class Child extends Parent { m() { return 'child>' + super.m(); } }
             new Child().m()"
        ),
        "child>parent>base"
    );
}

#[test]
fn super_call_resolves_the_running_constructor() {
    // Every constructor links to its own parent, so a three-level chain
    // terminates instead of re-entering the same parent.
    assert_eq!(
        eval_string(
            "var log = [];
             class Base { constructor() { log.push('base'); } }
             class Parent extends Base { constructor() { log.push('parent'); super(); } }
             class Child extends Parent { constructor() { log.push('child'); super(); } }
             new Child();
             log.join(',')"
        ),
        "child,parent,base"
    );
}

#[test]
fn super_call_and_property_from_arrow_function() {
    assert_eq!(
        eval_number(
            "var count = 0;
             class A { constructor() { count++; } }
             class B extends A { constructor() { (() => super())(); } }
             new B();
             count"
        ),
        1.0
    );
    assert_eq!(
        eval_string(
            "class P { m() { return 'p'; } }
             class Q extends P { m() { return 'q>' + (() => super.m())(); } }
             new Q().m()"
        ),
        "q>p"
    );
}

#[test]
fn static_super_and_accessor_super() {
    assert_eq!(
        eval_string(
            "class Base { static s() { return 'base'; } get g() { return 'baseg'; } }
             class Child extends Base {
               static s() { return 'child>' + super.s(); }
               get g() { return 'child>' + super.g; }
             }
             Child.s() + ' ' + new Child().g"
        ),
        "child>base child>baseg"
    );
}

#[test]
fn object_literal_method_super_uses_literal_home_object() {
    // The home object of an object-literal method is the literal itself, so
    // `super` starts from its prototype (Object.prototype by default).
    assert_eq!(
        eval_string("var o = { m() { return typeof super.toString; } }; o.m()"),
        "function"
    );
}

#[test]
fn super_within_base_class_reads_object_prototype() {
    assert_eq!(
        eval_bool(
            "class C { m() { return super.hasOwnProperty('m'); } }
             new C().m() === false"
        ),
        true
    );
}
