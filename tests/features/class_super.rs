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

#[test]
fn derived_class_needs_super_before_this() {
    // ES 9.2.2: a derived constructor's `this` is uninitialized until
    // `super()` runs — returning without it is a ReferenceError.
    assert_eq!(
        eval_string(
            "class Custom extends Error { constructor() {} }
             var n = 'none';
             try { new Custom('foo'); } catch (e) { n = e.name; }
             n"
        ),
        "ReferenceError"
    );
    // The same error is catchable from an enclosing frame, not just in place.
    assert_eq!(
        eval_string(
            "class Custom extends Error { constructor() {} }
             function attempt(cb) {
               try { cb(); return 'no-throw'; } catch (e) { return e.name; }
             }
             attempt(function() { new Custom(); })"
        ),
        "ReferenceError"
    );
    // Touching `this` before `super()` raises too.
    assert_eq!(
        eval_string(
            "class P {}
             class C extends P { constructor() { this.x = 1; } }
             var n = 'none';
             try { new C(); } catch (e) { n = e.name; }
             n"
        ),
        "ReferenceError"
    );
}

#[test]
fn default_derived_constructor_forwards_arguments() {
    // `class C extends P {}` is `constructor(...args) { super(...args); }`.
    assert_eq!(
        eval_number(
            "class P { constructor(x) { this.x = x; } }
             class C extends P {}
             new C(9).x"
        ),
        9.0
    );
    assert_eq!(
        eval_number(
            "class P { constructor(x, y) { this.s = x + y; } }
             class C extends P {}
             new C(1, 2).s"
        ),
        3.0
    );
    // ...and the parent's own initialisation reaches the derived `this`.
    assert_eq!(
        eval_string(
            "class P { constructor() { this.tag = 'p'; } }
             class C extends P { constructor() { super(); this.own = 'c'; } }
             var c = new C();
             c.tag + '/' + c.own"
        ),
        "p/c"
    );
}

#[test]
fn super_call_binds_this_in_arrow_bodies() {
    // An arrow that only calls `super()` must not have to read `this` first.
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
fn subclassing_builtins_uses_new_target_prototype() {
    // ES 9.1.14 GetPrototypeFromConstructor: the instance comes from
    // `newTarget.prototype`, not from the parent's intrinsic prototype.
    assert!(eval_bool(
        "class C extends Error {}
         Object.getPrototypeOf(new C()) === C.prototype"
    ));
    assert!(eval_bool("class C extends Error {} new C() instanceof C"));
    assert!(eval_bool(
        "class C extends Boolean {}
         Object.getPrototypeOf(new C(true)) === C.prototype"
    ));
    assert!(eval_bool(
        "class C extends String {}
         Object.getPrototypeOf(new C('hi')) === C.prototype"
    ));
}

#[test]
fn builtin_parent_initialises_the_derived_instance() {
    // `super(m)` puts the parent's own slots on the derived `this`.
    assert_eq!(
        eval_string("class C extends Error {} new C('boom').message"),
        "boom"
    );
    assert_eq!(
        eval_string(
            "class C extends Error { constructor(m) { super(m); this.tag = 'x'; } }
             var e = new C('boom');
             e.message + '/' + e.tag"
        ),
        "boom/x"
    );
    assert_eq!(
        eval_string("class C extends TypeError {} new C('m').name"),
        "TypeError"
    );
    // A plain `new Error(...)` is untouched by all of this.
    assert_eq!(
        eval_string(
            "var e = new Error('plain');
             [e.message, Object.getPrototypeOf(e) === Error.prototype].join('|')"
        ),
        "plain|true"
    );
}

#[test]
fn super_result_becomes_this() {
    // ES 12.3.5.1: whatever `super()` produced is the derived `this`, so a
    // parent that returns a different object replaces the pre-created one.
    assert_eq!(
        eval_string(
            "var owned = { tag: 'owned' };
             class P { constructor() { return owned; } }
             class C extends P { constructor() { super(); } }
             new C().tag"
        ),
        "owned"
    );
}

#[test]
fn derived_constructor_may_only_return_object_or_undefined() {
    // ES 9.2.2 step 13c.
    for snippet in [
        "class B { constructor() {} }
         class D extends B { constructor() { super(); return 0; } }
         var n = 'none';
         try { new D(); } catch (e) { n = e.name; }
         n",
        "class B { constructor() {} }
         class D extends B { constructor() { super(); return 's'; } }
         var n = 'none';
         try { new D(); } catch (e) { n = e.name; }
         n",
    ] {
        assert_eq!(eval_string(snippet), "TypeError");
    }
    // An object return is fine, and so is an implicit `undefined`.
    assert_eq!(
        eval_string(
            "class B { constructor() {} }
             class D extends B { constructor() { super(); return { tag: 'r' }; } }
             new D().tag"
        ),
        "r"
    );
    // A *base* class may return anything: the primitive is discarded.
    assert_eq!(
        eval_string(
            "class B { constructor() { return 5; } }
             typeof new B()"
        ),
        "object"
    );
}

#[test]
fn return_override_typeerror_is_not_catchable_inside_the_constructor() {
    // The TypeError belongs to `[[Construct]]`, not to the constructor body:
    // a `try`/`catch` around the `return` must not swallow it
    // (`derived-class-return-override-catch.js`).
    assert_eq!(
        eval_string(
            "class B { constructor() {} }
             class D extends B { constructor() { super(); try { return 0; } catch (e) { return; } } }
             var n = 'none';
             try { new D(); } catch (e) { n = e.name; }
             n"
        ),
        "TypeError"
    );
}
