//! Feature tests for the well-known symbols that are wired into abstract
//! operations: `Symbol.toStringTag` (Object.prototype.toString),
//! `Symbol.toPrimitive` (ToPrimitive) and `Symbol.hasInstance` (instanceof).

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn to_string_tag_overrides_builtin_tag() {
    assert_eq!(
        eval_string(
            "var o = {};
             o[Symbol.toStringTag] = 'Custom';
             Object.prototype.toString.call(o)"
        ),
        "[object Custom]"
    );
    // An accessor tag is honoured too.
    assert_eq!(
        eval_string(
            "var o = { get [Symbol.toStringTag]() { return 'Getter'; } };
             Object.prototype.toString.call(o)"
        ),
        "[object Getter]"
    );
}

#[test]
fn to_string_tag_ignores_non_strings_and_keeps_builtin_tags() {
    // A non-string tag is ignored per spec.
    assert_eq!(
        eval_string(
            "var o = {};
             o[Symbol.toStringTag] = 42;
             Object.prototype.toString.call(o)"
        ),
        "[object Object]"
    );
    // Built-in tags are unaffected.
    assert_eq!(
        eval_string("Object.prototype.toString.call([])"),
        "[object Array]"
    );
    assert_eq!(
        eval_string(
            "Object.prototype.toString.call(function () {})"
        ),
        "[object Function]"
    );
    assert_eq!(
        eval_string("Object.prototype.toString.call(null)"),
        "[object Null]"
    );
}

#[test]
fn to_primitive_method_receives_the_hint() {
    assert_eq!(
        eval_string(
            "var o = {};
             o[Symbol.toPrimitive] = function (hint) { return 'hint:' + hint; };
             o + ''"
        ),
        "hint:default"
    );
    assert_eq!(
        eval_string(
            "var o = {};
             o[Symbol.toPrimitive] = function (hint) { return 'hint:' + hint; };
             String(o)"
        ),
        "hint:string"
    );
    assert_eq!(
        eval_number(
            "var o = {};
             o[Symbol.toPrimitive] = function () { return 42; };
             +o"
        ),
        42.0
    );
}

#[test]
fn to_primitive_method_wins_over_value_of_and_to_string() {
    assert_eq!(
        eval_string(
            "var o = {
               valueOf: function () { return 'valueOf'; },
               toString: function () { return 'toString'; }
             };
             o[Symbol.toPrimitive] = function () { return 'exotic'; };
             o + ''"
        ),
        "exotic"
    );
}

#[test]
fn to_primitive_object_result_is_a_type_error() {
    assert_eq!(
        eval_string(
            "var o = {};
             o[Symbol.toPrimitive] = function () { return {}; };
             try { o + ''; 'no throw'; } catch (e) { 'threw'; }"
        ),
        "threw"
    );
}

#[test]
fn has_instance_replaces_the_default_instanceof_check() {
    assert_eq!(
        eval_string(
            "function C() {}
             C[Symbol.hasInstance] = function (x) { return x === 42; };
             (42 instanceof C) + ',' + (7 instanceof C)"
        ),
        "true,false"
    );
}

#[test]
fn has_instance_does_not_break_ordinary_instanceof() {
    assert_eq!(
        eval_bool("[] instanceof Array && ({}) instanceof Object"),
        true
    );
    assert_eq!(
        eval_bool(
            "class A {}
             class B extends A {}
             (new B()) instanceof A && !((new A()) instanceof B)"
        ),
        true
    );
}

#[test]
fn well_known_symbols_exist_on_the_symbol_constructor() {
    assert_eq!(
        eval_string(
            "typeof Symbol.iterator + ',' + typeof Symbol.toPrimitive + ',' +
            typeof Symbol.toStringTag + ',' + typeof Symbol.hasInstance + ',' +
            typeof Symbol.species + ',' + typeof Symbol.isConcatSpreadable"
        ),
        "symbol,symbol,symbol,symbol,symbol,symbol"
    );
}
