//! Feature tests for the M2' syntax tail: computed property names,
//! exponentiation (`**` / `**=`), nullish coalescing (`??`), plus the
//! property-order and descriptor semantics that came with them.

use crate::helpers::{eval_bool, eval_number, eval_string};

// ─────────────────────────────────────────────────────────
// Computed property names
// ─────────────────────────────────────────────────────────

#[test]
fn computed_object_literal_keys() {
    // A computed key is an arbitrary expression, evaluated in source order.
    assert_eq!(eval_number("({ [1 + 1]: 'b' })[2].length"), 1.0);
    assert_eq!(eval_string("var k = 'x'; ({ [k]: 'v' }).x"), "v");
    // Non-string keys go through ToPropertyKey.
    assert_eq!(eval_number("({ [42]: 'a' })[42].length"), 1.0);
    // A symbol key stays a symbol (it must not be stringified).
    assert_eq!(
        eval_bool("var s = Symbol('s'); ({ [s]: 1 })[s] === 1"),
        true
    );
}

#[test]
fn computed_object_literal_evaluation_order() {
    // Keys are converted to property keys in source order, interleaved with
    // the literal's other members.
    assert_eq!(
        eval_string(
            "var order = [];
             var key1 = { toString: function () { order.push('k1'); return 'a'; } };
             var key2 = { toString: function () { order.push('k2'); return 'b'; } };
             var o = { [key1]: 1, plain: 2, [key2]: 3 };
             order.join(',') + ':' + o.a + o.plain + o.b"
        ),
        "k1,k2:123"
    );
}

#[test]
fn computed_class_members() {
    // Computed method, accessor and static members all land as properties.
    assert_eq!(
        eval_string(
            "var s = Symbol('s');
             class C {
               ['m']() { return 'm'; }
               get ['g']() { return 'g'; }
               static ['sm']() { return 'sm'; }
               static get ['sg']() { return 'sg'; }
             }
             var c = new C();
             c.m() + c.g + C.sm() + C.sg"
        ),
        "mgsmsg"
    );
}

#[test]
fn computed_constructor_member_is_ordinary_method() {
    // `['constructor']() {}` is a prototype method, not the constructor.
    assert_eq!(
        eval_bool(
            "class C { ['constructor']() { return 1; } }
             var c = new C();
             C.prototype.constructor !== C && c.constructor() === 1"
        ),
        true
    );
}

#[test]
fn computed_accessor_pair_merges_into_one_descriptor() {
    assert_eq!(
        eval_string(
            "var o = { get ['p']() { return 'g'; }, set ['p'](v) { this.stored = v; } };
             o.p = 'x';
             o.p + ':' + o.stored"
        ),
        "g:x"
    );
}

// ─────────────────────────────────────────────────────────
// Exponentiation
// ─────────────────────────────────────────────────────────

#[test]
fn exponentiation_basics() {
    assert_eq!(eval_number("2 ** 10"), 1024.0);
    assert_eq!(eval_number("2 ** 3 ** 2"), 512.0); // right-associative
    assert_eq!(eval_number("(-2) ** 2"), 4.0);
    assert_eq!(eval_number("2 ** -1"), 0.5);
    assert_eq!(eval_number("2 ** 0.5"), 2f64.sqrt());
}

#[test]
fn exponentiation_assignment() {
    assert_eq!(eval_number("var a = 2; a **= 3; a"), 8.0);
    assert_eq!(eval_number("var a = 3; a **= 2; a"), 9.0);
}

#[test]
fn exponentiation_unary_lhs_is_syntax_error() {
    // `-2 ** 2` is a SyntaxError per spec (unary minus binds looser than **).
    let mut compiler = biujs::Compiler::new();
    assert!(compiler.compile("var x = -2 ** 2;").is_err());
}

// ─────────────────────────────────────────────────────────
// Nullish coalescing
// ─────────────────────────────────────────────────────────

#[test]
fn nullish_coalescing_only_nullish() {
    assert_eq!(eval_number("null ?? 5"), 5.0);
    assert_eq!(eval_number("undefined ?? 5"), 5.0);
    // Falsy-but-not-nullish values pass through untouched (unlike `||`).
    assert_eq!(eval_number("0 ?? 5"), 0.0);
    assert_eq!(eval_string("'' ?? 'd'"), "");
    assert_eq!(eval_bool("false ?? true"), false);
    assert_eq!(eval_number("NaN ?? 1").is_nan() as u8 as f64, 1.0);
}

#[test]
fn nullish_coalescing_short_circuits() {
    // The right operand is only evaluated when the left is nullish.
    assert_eq!(
        eval_string(
            "var calls = 0;
             function side() { calls += 1; return 'r'; }
             var a = 'l' ?? side();
             var b = null ?? side();
             calls + ':' + a + b"
        ),
        "1:lr"
    );
}

// ─────────────────────────────────────────────────────────
// Property order & descriptor semantics
// ─────────────────────────────────────────────────────────

#[test]
fn own_property_names_order() {
    // Array indices ascending, then other string keys in creation order.
    assert_eq!(
        eval_string(
            "var o = { b: 1, 2: 2, a: 3, 1: 4 };
             Object.getOwnPropertyNames(o).join(',')"
        ),
        "1,2,b,a"
    );
}

#[test]
fn keys_skip_non_enumerable() {
    // Class methods are non-enumerable: absent from Object.keys, present in
    // Object.getOwnPropertyNames.
    assert_eq!(
        eval_string(
            "class C { m() {} }
             Object.keys(C.prototype).length + ':' + Object.getOwnPropertyNames(C.prototype).join(',')"
        ),
        "0:constructor,m"
    );
}

#[test]
fn define_property_partial_descriptor_keeps_attributes() {
    // A partial descriptor only touches the attributes it mentions.
    assert_eq!(
        eval_bool(
            "var o = {};
             Object.defineProperty(o, 'x', { value: 1, writable: false, enumerable: false, configurable: true });
             Object.defineProperty(o, 'x', { value: 2 });
             var d = Object.getOwnPropertyDescriptor(o, 'x');
             o.x === 2 && d.writable === false && d.enumerable === false && d.configurable === true"
        ),
        true
    );
}

#[test]
fn define_property_accessor_merge() {
    // Defining only `set` keeps an already-installed getter.
    assert_eq!(
        eval_string(
            "var o = {};
             Object.defineProperty(o, 'x', { get: function () { return 'g'; }, configurable: true });
             Object.defineProperty(o, 'x', { set: function (v) { this.v = v; } });
             o.x = 1;
             o.x + ':' + o.v"
        ),
        "g:1"
    );
}

#[test]
fn define_property_symbol_key() {
    assert_eq!(
        eval_bool(
            "var s = Symbol('s');
             var o = {};
             Object.defineProperty(o, s, { value: 7, enumerable: true });
             o[s] === 7"
        ),
        true
    );
}
