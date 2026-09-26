use crate::helpers::{eval_number, eval_string};

// ============================================================
// Variables (let)
// ============================================================

#[test]
fn let_declaration() {
    assert_eq!(eval_number("let x = 5; x"), 5.0);
    assert_eq!(eval_number("let a = 10; let b = 20; a + b"), 30.0);
}

#[test]
fn let_reassignment() {
    assert_eq!(eval_number("let x = 1; x = 2; x"), 2.0);
    assert_eq!(eval_number("let x = 0; x = x + 5; x"), 5.0);
}

#[test]
fn let_multiple_declarations() {
    assert_eq!(eval_number("let a = 1, b = 2, c = 3; a + b + c"), 6.0);
}

// ============================================================
// `var` is function-scoped and hoisted
// ============================================================
//
// A `var` belongs to the enclosing *function* (or the script), not to the block
// it is written in, and it exists from the moment that function starts. The
// lowering used to insert the binding into the block scope as the statement was
// walked, so `{ var a = 1; } a` was a `ReferenceError` and `typeof x` before
// `var x = 1` threw. Whole families of Sputnik-era tests read a `var` out of the
// block that declared it.

#[test]
fn var_declared_in_a_block_is_visible_afterwards() {
    assert_eq!(eval_number("var a = 0; { var b = 1; } a + b"), 1.0);
    assert_eq!(eval_number("var a = 0; if (true) { var b = 2; } a + b"), 2.0);
    assert_eq!(eval_number("var a = 0; for (var i = 0; i < 1; i = i + 1) { var b = 3; } a + b"), 3.0);
    assert_eq!(eval_number("var a = 0; switch (1) { case 1: var b = 4; } a + b"), 4.0);
    assert_eq!(
        eval_number("var a = 0; try { throw new Error('x'); } catch (e) { var b = 5; } a + b"),
        5.0
    );
    // Inside a function, the same rule applies to that function's scope.
    assert_eq!(eval_number("function f() { { var d = 6; } return d; } f()"), 6.0);
}

#[test]
fn var_is_hoisted_to_the_top_of_its_function() {
    // `typeof` before the declaration is `undefined`, not a ReferenceError. The
    // value is captured in a variable because the script's completion value is
    // not what a trailing declaration reports.
    assert_eq!(eval_string("var seen = typeof later; var later = 1; seen;"), "undefined");
    assert_eq!(
        eval_string("function f() { var seen = typeof later; var later = 1; return seen; } f()"),
        "undefined"
    );
    // A block that never runs still declares the name.
    assert_eq!(
        eval_string("while (false) { var never = 1; } var seen = typeof never; seen;"),
        "undefined"
    );
}

#[test]
fn var_in_a_for_head_survives_the_loop_scope() {
    // The head's bindings get a scope of their own (so a `let` head cannot leak
    // out); a `var` head must still outlive the loop.
    assert_eq!(eval_number("var total = 0; for (var i = 0; i < 3; i = i + 1) { total = i; } total"), 2.0);
    assert_eq!(
        eval_number("var sum = 0; for (var [a, b] = [1, 2]; sum < 1; sum = sum + 1) {} a + b"),
        3.0
    );
    assert_eq!(eval_string("var out = ''; for (var k of ['x']) {} out + k"), "x");
    assert_eq!(eval_string("var out = ''; for (var k in { y: 1 }) {} out + k"), "y");
}

#[test]
fn block_scoped_let_and_const_still_do_not_leak() {
    assert_eq!(eval_string("{ let a = 1; } typeof a"), "undefined");
    assert_eq!(eval_string("{ const b = 2; } typeof b"), "undefined");
    assert_eq!(eval_string("for (let i = 0; i < 1; i = i + 1) {} typeof i"), "undefined");
    assert_eq!(eval_string("for (const c of [1]) {} typeof c"), "undefined");
}

#[test]
fn an_inner_binding_shadows_the_script_scope_one() {
    // A script-level binding is read through the global environment (any function
    // may have written it), but an inner block that declares the same name must
    // win: reading the environment there returned the outer value.
    assert_eq!(eval_number("let x = 1; { let x = 2; } x"), 1.0);
    assert_eq!(eval_number("var y = 1; { let y = 2; } y"), 1.0);
    assert_eq!(
        eval_number("let z = 1; var seen = 0; { let z = 2; seen = z; } seen * 10 + z"),
        21.0
    );
}
