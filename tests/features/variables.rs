use crate::helpers::eval_number;

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