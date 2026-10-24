//! Strict-mode and global-object semantics.
//!
//! The engine targets **strict mode only**, so these are not "extras": they are
//! the contract. Two of them (`this` at the top level, `globalThis` inheriting
//! `Object.prototype`) were outright missing before; `with` used to compile into
//! a program that silently read the wrong scope.

use crate::helpers::{eval_js, eval_string};

#[test]
fn top_level_this_is_the_global_object() {
    // ES: the top-level `this` of a script is the global object — in strict mode
    // included. It used to be `undefined`.
    assert_eq!(
        eval_string("\"use strict\"; String(this === globalThis)"),
        "true"
    );
    assert_eq!(
        eval_string("\"use strict\"; var legacy = this; String(legacy === globalThis)"),
        "true"
    );
}

#[test]
fn global_this_inherits_from_object_prototype() {
    assert_eq!(
        eval_string(
            "\"use strict\"; String(Object.getPrototypeOf(globalThis) === Object.prototype)"
        ),
        "true"
    );
    // Inherited, not own: `hasOwnProperty` and `toString` are Object.prototype's.
    assert_eq!(
        eval_string(
            "\"use strict\"; String(typeof globalThis.hasOwnProperty === 'function' && globalThis.hasOwnProperty('globalThis'))"
        ),
        "true"
    );
    // `[[Class]]` is `global`, as in every host.
    assert_eq!(
        eval_string("\"use strict\"; String(globalThis.toString())"),
        "[object global]"
    );
}

#[test]
fn strict_assignment_to_an_undeclared_name_throws() {
    // `SetMutableBinding` on an unresolvable reference: strict code throws
    // instead of creating a global property.
    assert_eq!(
        eval_string(
            "\"use strict\";
             try { undeclared = 1; 'no throw' } catch (e) { e.name }"
        ),
        "ReferenceError"
    );
    // …but reading it is still just `undefined`, as `typeof` must be safe.
    assert_eq!(
        eval_string("\"use strict\"; String(typeof undeclared)"),
        "undefined"
    );
    // An *existing* global property is a binding, so strict code may write it.
    assert_eq!(
        eval_string(
            "\"use strict\";
             globalThis.present = 1;
             present = 2;
             String(globalThis.present === 2)"
        ),
        "true"
    );
}

#[test]
fn strict_update_of_an_undeclared_name_throws() {
    assert_eq!(
        eval_string("\"use strict\"; try { undeclared++ ; 'no throw' } catch (e) { e.name }"),
        "ReferenceError"
    );
}

#[test]
fn with_statements_are_rejected() {
    // `with` is out of scope; oxc only rejects it under strict, so the sloppy
    // shape is rejected by the semantic analyzer. It used to compile into a
    // program whose body read the wrong scope.
    let result = eval_js("var o = { a: 1 }; with (o) { a; }");
    assert!(
        result.is_err(),
        "`with` must not compile, got {result:?}"
    );
}
