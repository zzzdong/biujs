use crate::helpers::{eval_bool, eval_string};

// ============================================================
// Logical operators
// ============================================================

#[test]
fn logical_not() {
    assert_eq!(eval_bool("!true"), false);
    assert_eq!(eval_bool("!false"), true);
    assert_eq!(eval_bool("!!true"), true);
    assert_eq!(eval_bool("!!false"), false);
}

#[test]
fn logical_and() {
    assert_eq!(eval_bool("true && true"), true);
    assert_eq!(eval_bool("true && false"), false);
    assert_eq!(eval_bool("false && true"), false);
    assert_eq!(eval_bool("false && false"), false);
}

#[test]
fn logical_or() {
    assert_eq!(eval_bool("true || true"), true);
    assert_eq!(eval_bool("true || false"), true);
    assert_eq!(eval_bool("false || true"), true);
    assert_eq!(eval_bool("false || false"), false);
}

// ============================================================
// typeof
// ============================================================

#[test]
fn typeof_number() {
    assert_eq!(eval_string("typeof 42"), "number");
    assert_eq!(eval_string("typeof 0"), "number");
    assert_eq!(eval_string("typeof -1"), "number");
    assert_eq!(eval_string("typeof 3.14"), "number");
}

#[test]
fn typeof_string() {
    assert_eq!(eval_string("typeof 'hello'"), "string");
    assert_eq!(eval_string("typeof \"world\""), "string");
    assert_eq!(eval_string("typeof ''"), "string");
}

#[test]
fn typeof_boolean() {
    assert_eq!(eval_string("typeof true"), "boolean");
    assert_eq!(eval_string("typeof false"), "boolean");
}

#[test]
fn typeof_undefined() {
    assert_eq!(eval_string("typeof undefined"), "undefined");
}

#[test]
fn double_negation_to_boolean() {
    assert_eq!(eval_bool("!!1"), true);
    assert_eq!(eval_bool("!!0"), false);
    assert_eq!(eval_bool("!!''"), false);
    assert_eq!(eval_bool("!!'x'"), true);
}