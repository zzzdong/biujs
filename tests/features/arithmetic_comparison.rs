use crate::helpers::{eval_bool, eval_number};

// ============================================================
// Arithmetic
// ============================================================

#[test]
fn addition_numbers() {
    assert_eq!(eval_number("1 + 2"), 3.0);
    assert_eq!(eval_number("0 + 0"), 0.0);
    assert_eq!(eval_number("-1 + 1"), 0.0);
    assert_eq!(eval_number("100 + 200"), 300.0);
    assert_eq!(eval_number("0.1 + 0.2"), 0.1 + 0.2);
}

#[test]
fn subtraction_numbers() {
    assert_eq!(eval_number("5 - 3"), 2.0);
    assert_eq!(eval_number("0 - 1"), -1.0);
    assert_eq!(eval_number("10 - 10"), 0.0);
}

#[test]
fn multiplication_numbers() {
    assert_eq!(eval_number("3 * 4"), 12.0);
    assert_eq!(eval_number("0 * 100"), 0.0);
    assert_eq!(eval_number("-2 * 3"), -6.0);
    assert_eq!(eval_number("1 * 1"), 1.0);
}

#[test]
fn division_numbers() {
    assert_eq!(eval_number("10 / 2"), 5.0);
    assert_eq!(eval_number("7 / 2"), 3.5);
    assert_eq!(eval_number("0 / 5"), 0.0);
    assert_eq!(eval_number("-6 / 3"), -2.0);
}

#[test]
fn modulus_numbers() {
    assert_eq!(eval_number("10 % 3"), 1.0);
    assert_eq!(eval_number("7 % 7"), 0.0);
    assert_eq!(eval_number("5 % 2"), 1.0);
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(eval_number("2 + 3 * 4"), 14.0);
    assert_eq!(eval_number("(2 + 3) * 4"), 20.0);
    assert_eq!(eval_number("10 - 2 * 3"), 4.0);
    assert_eq!(eval_number("10 / 2 + 3"), 8.0);
}

#[test]
fn arithmetic_chained() {
    assert_eq!(eval_number("1 + 2 + 3 + 4"), 10.0);
    assert_eq!(eval_number("2 * 3 * 4"), 24.0);
    assert_eq!(eval_number("100 / 10 / 2"), 5.0);
}

// ============================================================
// Comparison operators
// ============================================================

#[test]
fn greater_than() {
    assert_eq!(eval_bool("5 > 3"), true);
    assert_eq!(eval_bool("3 > 5"), false);
    assert_eq!(eval_bool("3 > 3"), false);
}

#[test]
fn less_than() {
    assert_eq!(eval_bool("3 < 5"), true);
    assert_eq!(eval_bool("5 < 3"), false);
    assert_eq!(eval_bool("3 < 3"), false);
}

#[test]
fn greater_than_or_equal() {
    assert_eq!(eval_bool("5 >= 3"), true);
    assert_eq!(eval_bool("3 >= 5"), false);
    assert_eq!(eval_bool("3 >= 3"), true);
}

#[test]
fn less_than_or_equal() {
    assert_eq!(eval_bool("3 <= 5"), true);
    assert_eq!(eval_bool("5 <= 3"), false);
    assert_eq!(eval_bool("3 <= 3"), true);
}

#[test]
fn equals() {
    assert_eq!(eval_bool("1 == 1"), true);
    assert_eq!(eval_bool("1 == 2"), false);
    assert_eq!(eval_bool("0 == 0"), true);
}

#[test]
fn not_equals() {
    assert_eq!(eval_bool("1 != 2"), true);
    assert_eq!(eval_bool("1 != 1"), false);
}

#[test]
fn strict_equals() {
    assert_eq!(eval_bool("1 === 1"), true);
    assert_eq!(eval_bool("1 === 2"), false);
    assert_eq!(eval_bool("'hello' === 'hello'"), true);
    assert_eq!(eval_bool("true === true"), true);
    assert_eq!(eval_bool("false === false"), true);
}

#[test]
fn strict_not_equals() {
    assert_eq!(eval_bool("1 !== 2"), true);
    assert_eq!(eval_bool("1 !== 1"), false);
}

// ============================================================
// Bitwise operators
// ============================================================

#[test]
fn bitwise_not() {
    // ~x = -(x + 1) for 32-bit signed integers
    assert_eq!(eval_number("~0"), -1.0);
    assert_eq!(eval_number("~1"), -2.0);
    assert_eq!(eval_number("~-1"), 0.0);
    assert_eq!(eval_number("~5"), -6.0);
    assert_eq!(eval_number("~-5"), 4.0);
    
    // Test with expressions
    assert_eq!(eval_number("~(1 + 2)"), -4.0);
    assert_eq!(eval_number("~~5"), 5.0);  // Double negation
}