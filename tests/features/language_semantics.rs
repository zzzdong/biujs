//! Unit tests for the language semantics added/overhauled during the
//! architecture work: bitwise operators, `switch`, `do…while`, `delete`,
//! `in`, `typeof` on unresolvable names, script-level closures, primitive
//! property access, `this` frames and array-length validation.

use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};

// ─────────────────────────────────────────────────────────
// Bitwise operators
// ─────────────────────────────────────────────────────────

#[test]
fn bitwise_and_or_xor() {
    assert_eq!(eval_number("12 & 10"), 8.0);
    assert_eq!(eval_number("12 | 10"), 14.0);
    assert_eq!(eval_number("12 ^ 10"), 6.0);
    assert_eq!(eval_number("1 ^ 1"), 0.0);
    assert_eq!(eval_number("-1 & 0xFF"), 255.0);
}

#[test]
fn bitwise_not() {
    assert_eq!(eval_number("~5"), -6.0);
    assert_eq!(eval_number("~0"), -1.0);
    assert_eq!(eval_number("~-1"), 0.0);
}

#[test]
fn shift_operators() {
    assert_eq!(eval_number("1 << 4"), 16.0);
    assert_eq!(eval_number("-8 >> 2"), -2.0);
    assert_eq!(eval_number("-8 >>> 2"), 1073741822.0);
    // Shift counts are taken modulo 32
    assert_eq!(eval_number("1 << 32"), 1.0);
    assert_eq!(eval_number("1 << 33"), 2.0);
}

#[test]
fn bitwise_coerces_via_to_int32() {
    assert_eq!(eval_number("\"12\" & \"10\""), 8.0);
    assert_eq!(eval_number("1.9 | 0"), 1.0);
    assert_eq!(eval_number("-1.9 | 0"), -1.0);
}

#[test]
fn bitwise_compound_assignment() {
    assert_eq!(eval_number("var x = 12; x &= 10; x"), 8.0);
    assert_eq!(eval_number("var x = 1; x <<= 4; x"), 16.0);
}

// ─────────────────────────────────────────────────────────
// switch
// ─────────────────────────────────────────────────────────

#[test]
fn switch_basic() {
    let js = r#"
        function f(v) {
            switch (v) {
                case 1: return "one";
                case 2: return "two";
                default: return "other";
            }
        }
        f(2)
    "#;
    assert_eq!(eval_string(js), "two");
}

#[test]
fn switch_default_and_fallthrough() {
    let js = r#"
        var out = "";
        switch (3) {
            case 1: out += "a";
            case 2: out += "b";
            case 3: out += "c";
            case 4: out += "d";
        }
        out
    "#;
    assert_eq!(eval_string(js), "cd");
}

#[test]
fn switch_break_exits() {
    let js = r#"
        var out = "";
        switch (1) {
            case 1: out += "a"; break;
            case 2: out += "b";
        }
        out
    "#;
    assert_eq!(eval_string(js), "a");
}

#[test]
fn switch_default_position_independent() {
    let js = r#"
        function f(v) {
            switch (v) {
                default: return "d";
                case 1: return "one";
            }
        }
        f(9) + f(1)
    "#;
    assert_eq!(eval_string(js), "done");
}

#[test]
fn switch_uses_strict_equality() {
    // `1` must not match the string `"1"`.
    assert_eq!(eval_string("switch (1) { case \"1\": return \"str\"; default: return \"num\"; }"), "num");
}

#[test]
fn switch_inside_loop_break_only_leaves_switch() {
    let js = r#"
        var out = "";
        for (var i = 0; i < 3; i++) {
            switch (i) {
                case 1: break;
                default: out += "x";
            }
            out += "|";
        }
        out
    "#;
    assert_eq!(eval_string(js), "x||x|");
}

// ─────────────────────────────────────────────────────────
// do…while
// ─────────────────────────────────────────────────────────

#[test]
fn do_while_runs_body_once() {
    assert_eq!(eval_number("var i = 0; do { i += 1; } while (false); i"), 1.0);
    assert_eq!(eval_number("var i = 0; do { i += 1; } while (i < 5); i"), 5.0);
}

#[test]
fn do_while_break_continue() {
    let js = "var s = 0; var i = 0; do { i += 1; if (i == 2) continue; s += i; } while (i < 4); s";
    // i = 1 (s=1), 2 (skipped), 3 (s=4), 4 (s=8)
    assert_eq!(eval_number(js), 8.0);
}

// ─────────────────────────────────────────────────────────
// delete / in
// ─────────────────────────────────────────────────────────

#[test]
fn delete_object_property() {
    let js = r#"
        var o = { a: 1, b: 2 };
        var removed = delete o.a;
        removed + " " + (typeof o.a) + " " + o.b
    "#;
    assert_eq!(eval_string(js), "true undefined 2");
}

#[test]
fn delete_computed_key() {
    let js = r#"
        var o = { a: 1 };
        var k = "a";
        delete o[k]
    "#;
    assert_eq!(eval_bool(js), true);
}

#[test]
fn in_operator() {
    assert_eq!(eval_bool("var o = {a: 1}; \"a\" in o"), true);
    assert_eq!(eval_bool("var o = {a: 1}; \"b\" in o"), false);
    assert_eq!(eval_bool("var o = {a: 1}; delete o.a; \"a\" in o"), false);
    // Inherited properties count too
    assert_eq!(eval_bool("\"toString\" in {}"), true);
}

// ─────────────────────────────────────────────────────────
// typeof
// ─────────────────────────────────────────────────────────

#[test]
fn typeof_unresolvable_is_undefined() {
    assert_eq!(eval_string("typeof thisNameDoesNotExist"), "undefined");
    assert_eq!(
        eval_string("function f() { return typeof alsoMissing; } f()"),
        "undefined"
    );
}

#[test]
fn typeof_declared_still_works() {
    assert_eq!(eval_string("var x; typeof x"), "undefined");
    assert_eq!(eval_string("var x = 1; typeof x"), "number");
    assert_eq!(eval_string("typeof 1"), "number");
    assert_eq!(eval_string("typeof \"s\""), "string");
    assert_eq!(eval_string("typeof null"), "object");
}

// ─────────────────────────────────────────────────────────
// Script-level scope is visible to nested functions
// ─────────────────────────────────────────────────────────

#[test]
fn nested_function_reads_script_binding() {
    let js = r#"
        var counter = 10;
        function read() { return counter; }
        read()
    "#;
    assert_eq!(eval_number(js), 10.0);
}

#[test]
fn nested_function_sees_updated_script_binding() {
    let js = r#"
        var value = 1;
        function read() { return value; }
        var first = read();
        value = 42;
        var second = read();
        first * 100 + second
    "#;
    assert_eq!(eval_number(js), 142.0);
}

#[test]
fn nested_function_calls_sibling_function() {
    let js = r#"
        function helper(x) { return x * 2; }
        function outer(x) { return helper(x) + 1; }
        outer(5)
    "#;
    assert_eq!(eval_number(js), 11.0);
}

#[test]
fn method_on_script_level_function_object() {
    let js = r#"
        function ns(x) { return x; }
        ns.double = function (x) { return x * 2; };
        ns.double(21)
    "#;
    assert_eq!(eval_number(js), 42.0);
}

#[test]
fn two_anonymous_functions_do_not_collide() {
    // Regression: functions used to be de-duplicated by (name, params), so the
    // second definition silently overwrote the first.
    let js = r#"
        var ops = {};
        ops.add = function (a, b) { return a + b; };
        ops.sub = function (a, b) { return a - b; };
        ops.add(7, 3) * 100 + ops.sub(7, 3)
    "#;
    assert_eq!(eval_number(js), 1004.0);
}

#[test]
fn function_prototype_identity_is_stable() {
    let js = r#"
        function Animal() {}
        Animal.prototype.kind = "animal";
        var a = new Animal();
        var b = new Animal();
        a.kind + "/" + b.kind + "/" + (a instanceof Animal)
    "#;
    assert_eq!(eval_string(js), "animal/animal/true");
}

// ─────────────────────────────────────────────────────────
// Primitive property access
// ─────────────────────────────────────────────────────────

#[test]
fn string_length_and_index() {
    assert_eq!(eval_number("\"hello\".length"), 5.0);
    assert_eq!(eval_string("\"hello\"[1]"), "e");
}

#[test]
fn property_access_on_null_throws() {
    assert!(eval_js("var n = null; n.foo").is_err());
    assert!(eval_js("var u; u.foo").is_err());
}

// ─────────────────────────────────────────────────────────
// `this` binding is per frame
// ─────────────────────────────────────────────────────────

#[test]
fn this_survives_nested_call() {
    let js = r#"
        function helper() { return 0; }
        var obj = {
            value: 7,
            get: function () { helper(); return this.value; }
        };
        obj.get()
    "#;
    assert_eq!(eval_number(js), 7.0);
}

#[test]
fn this_is_undefined_for_plain_calls() {
    // Strict mode: `this` is undefined, so reading a property must throw.
    assert!(eval_js("function f() { return this.x; } f()").is_err());
}

// ─────────────────────────────────────────────────────────
// Update expressions write back through members
// ─────────────────────────────────────────────────────────

#[test]
fn update_expression_writes_back_to_member() {
    let js = r#"
        var o = { n: 1 };
        var arr = [5];
        o.n++;
        arr[0]++;
        o.n + arr[0]
    "#;
    assert_eq!(eval_number(js), 8.0);
}

// ─────────────────────────────────────────────────────────
// Array length validation
// ─────────────────────────────────────────────────────────

#[test]
fn array_length_must_be_a_valid_length() {
    assert!(eval_js("new Array(4294967296)").is_err());
    assert!(eval_js("new Array(-1)").is_err());
    assert!(eval_js("new Array(1.5)").is_err());
    assert_eq!(eval_number("new Array(3).length"), 3.0);
    assert_eq!(eval_number("new Array(0).length"), 0.0);
}

#[test]
fn runaway_recursion_reports_range_error() {
    let js = "function f() { return f(); } f()";
    match eval_js(js) {
        Err(msg) => assert!(msg.contains("RangeError"), "unexpected error: {msg}"),
        Ok(v) => panic!("expected a RangeError, got {v:?}"),
    }
}
