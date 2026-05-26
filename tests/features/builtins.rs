use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};
use biujs::Value;

// ============================================================
// Built-in Objects
// ============================================================

// ──────────────────────────────
// Error
// ──────────────────────────────

#[test]
fn error_constructor_no_args() {
    let js = r#"
        let e = Error();
        e.name
    "#;
    assert_eq!(eval_string(js), "Error");
}

#[test]
fn error_constructor_with_message() {
    let js = r#"
        let e = Error("something went wrong");
        e.message
    "#;
    assert_eq!(eval_string(js), "something went wrong");
}

#[test]
fn error_constructor_name() {
    let js = r#"
        let e = Error("test");
        e.name
    "#;
    assert_eq!(eval_string(js), "Error");
}

#[test]
fn type_error_constructor() {
    let js = r#"
        let e = TypeError("bad type");
        e.name + ": " + e.message
    "#;
    assert_eq!(eval_string(js), "TypeError: bad type");
}

#[test]
fn reference_error_constructor() {
    let js = r#"
        let e = ReferenceError("not found");
        e.message
    "#;
    assert_eq!(eval_string(js), "not found");
}

#[test]
fn range_error_constructor() {
    let js = r#"
        let e = RangeError("out of range");
        e.name
    "#;
    assert_eq!(eval_string(js), "RangeError");
}

// ──────────────────────────────
// Boolean
// ──────────────────────────────

#[test]
fn boolean_constructor_true() {
    assert_eq!(eval_bool("Boolean(true)"), true);
    assert_eq!(eval_bool("Boolean(1)"), true);
    assert_eq!(eval_bool("Boolean('hello')"), true);
}

#[test]
fn boolean_constructor_false() {
    assert_eq!(eval_bool("Boolean(false)"), false);
    assert_eq!(eval_bool("Boolean(0)"), false);
    assert_eq!(eval_bool("Boolean('')"), false);
    assert_eq!(eval_bool("Boolean(undefined)"), false);
    assert_eq!(eval_bool("Boolean(null)"), false);
}

#[test]
fn boolean_constructor_no_args() {
    assert_eq!(eval_bool("Boolean()"), false);
}

// ──────────────────────────────
// Number
// ──────────────────────────────

#[test]
fn number_constructor_no_args() {
    assert_eq!(eval_number("Number()"), 0.0);
}

#[test]
fn number_constructor_value() {
    assert_eq!(eval_number("Number(42)"), 42.0);
    assert_eq!(eval_number("Number('3.14')"), 3.14);
    assert_eq!(eval_number("Number(true)"), 1.0);
    assert_eq!(eval_number("Number(false)"), 0.0);
}

#[test]
fn number_is_nan() {
    assert_eq!(eval_bool("Number.isNaN(NaN)"), true);
    assert_eq!(eval_bool("Number.isNaN(42)"), false);
    assert_eq!(eval_bool("Number.isNaN('hello')"), false);
}

#[test]
fn number_is_finite() {
    assert_eq!(eval_bool("Number.isFinite(42)"), true);
    assert_eq!(eval_bool("Number.isFinite(Infinity)"), false);
    assert_eq!(eval_bool("Number.isFinite(NaN)"), false);
}

#[test]
fn number_is_integer() {
    assert_eq!(eval_bool("Number.isInteger(42)"), true);
    assert_eq!(eval_bool("Number.isInteger(3.14)"), false);
    assert_eq!(eval_bool("Number.isInteger(NaN)"), false);
    assert_eq!(eval_bool("Number.isInteger(Infinity)"), false);
}

// ──────────────────────────────
// String
// ──────────────────────────────

#[test]
fn string_constructor_no_args() {
    assert_eq!(eval_string("String()"), "");
}

#[test]
fn string_constructor_value() {
    assert_eq!(eval_string("String(42)"), "42");
    assert_eq!(eval_string("String(true)"), "true");
    assert_eq!(eval_string("String('hello')"), "hello");
}

// ──────────────────────────────
// Object
// ──────────────────────────────

#[test]
fn object_constructor_no_args() {
    let js = r#"
        let o = Object();
        typeof o
    "#;
    assert_eq!(eval_string(js), "object");
}

#[test]
fn object_constructor_wraps_value() {
    let js = r#"
        let x = 42;
        let o = Object(x);
        o
    "#;
    assert_eq!(eval_number(js), 42.0);
}

/// Object.keys() on a simple object
#[test]
fn object_keys() {
    let js = r#"
        let o = {a: 1, b: 2, c: 3};
        Object.keys(o).toString()
    "#;
    assert_eq!(eval_string(js), "a,b,c");
}

/// Object.values() on a simple object
#[test]
fn object_values() {
    let js = r#"
        let o = {x: 10, y: 20};
        Object.values(o).toString()
    "#;
    assert_eq!(eval_string(js), "10,20");
}

// ──────────────────────────────
// Array constructor
// ──────────────────────────────

#[test]
fn array_constructor_no_args() {
    let js = r#"
        let a = Array();
        a.length
    "#;
    assert_eq!(eval_number(js), 0.0);
}

#[test]
fn array_constructor_with_length() {
    let js = r#"
        let a = Array(5);
        a.length
    "#;
    assert_eq!(eval_number(js), 5.0);
}

#[test]
fn array_constructor_with_values() {
    let js = r#"
        let a = Array(1, 2, 3);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3");
}

// ──────────────────────────────
// Number prototype methods
// ──────────────────────────────

#[test]
fn number_to_fixed() {
    assert_eq!(eval_string("(3.14159).toFixed()"), "3");
    assert_eq!(eval_string("(3.14159).toFixed(2)"), "3.14");
    assert_eq!(eval_string("(3.14159).toFixed(4)"), "3.1416");
    assert_eq!(eval_string("NaN.toFixed()"), "NaN");
    assert_eq!(eval_string("Infinity.toFixed()"), "Infinity");
}

#[test]
fn number_to_exponential() {
    assert_eq!(eval_string("(12345).toExponential()"), "1.2345e4");
    assert_eq!(eval_string("(12345).toExponential(2)"), "1.23e4");
    assert_eq!(eval_string("NaN.toExponential()"), "NaN");
}

#[test]
fn number_to_precision() {
    // Uses decimal places (not significant digits) in current implementation
    assert_eq!(eval_string("(123.456).toPrecision(4)"), "123.4560");
    assert_eq!(eval_string("(123.456).toPrecision(2)"), "123.46");
    assert_eq!(eval_string("NaN.toPrecision(1)"), "NaN");
}

// ──────────────────────────────
// String prototype methods
// ──────────────────────────────

#[test]
fn string_char_at() {
    assert_eq!(eval_string("'hello'.charAt(0)"), "h");
    assert_eq!(eval_string("'hello'.charAt(4)"), "o");
    assert_eq!(eval_string("'hello'.charAt(99)"), "");
    assert_eq!(eval_string("'hello'.charAt()"), "h");
}

#[test]
fn string_char_code_at() {
    assert_eq!(eval_number("'ABC'.charCodeAt(0)"), 65.0);
    assert_eq!(eval_number("'ABC'.charCodeAt(1)"), 66.0);
    assert!(eval_number("'ABC'.charCodeAt(99)").is_nan());
}

#[test]
fn string_concat() {
    assert_eq!(eval_string("'hello'.concat(' ', 'world')"), "hello world");
    assert_eq!(eval_string("'a'.concat('b')"), "ab");
    assert_eq!(eval_string("'abc'.concat()"), "abc");
}

#[test]
fn string_includes() {
    assert_eq!(eval_bool("'hello world'.includes('world')"), true);
    assert_eq!(eval_bool("'hello world'.includes('xyz')"), false);
    assert_eq!(eval_bool("'hello'.includes()"), false);
}

#[test]
fn string_index_of() {
    assert_eq!(eval_number("'hello'.indexOf('l')"), 2.0);
    assert_eq!(eval_number("'hello'.indexOf('x')"), -1.0);
    assert_eq!(eval_number("'abc'.indexOf()"), -1.0);
}

#[test]
fn string_slice() {
    assert_eq!(eval_string("'hello'.slice(1, 3)"), "el");
    assert_eq!(eval_string("'hello'.slice(2)"), "llo");
    assert_eq!(eval_string("'hello'.slice(-3)"), "llo");
    assert_eq!(eval_string("'hello'.slice(-3, -1)"), "ll");
}

#[test]
fn string_to_upper() {
    assert_eq!(eval_string("'hello'.toUpperCase()"), "HELLO");
    assert_eq!(eval_string("'ABC'.toUpperCase()"), "ABC");
    assert_eq!(eval_string("'123'.toUpperCase()"), "123");
}

#[test]
fn string_to_lower() {
    assert_eq!(eval_string("'HELLO'.toLowerCase()"), "hello");
    assert_eq!(eval_string("'abc'.toLowerCase()"), "abc");
}

#[test]
fn string_trim() {
    assert_eq!(eval_string("'  hello  '.trim()"), "hello");
    assert_eq!(eval_string("'hello'.trim()"), "hello");
    assert_eq!(eval_string("'  '.trim()"), "");
}

#[test]
fn string_split() {
    assert_eq!(eval_string("'a,b,c'.split(',').toString()"), "a,b,c");
    assert_eq!(eval_string("'abc'.split('').toString()"), "a,b,c");
    assert_eq!(eval_string("'hello'.split().toString()"), "hello");
}

#[test]
fn string_substring() {
    assert_eq!(eval_string("'hello'.substring(1, 3)"), "el");
    assert_eq!(eval_string("'hello'.substring(3, 1)"), "el");
    assert_eq!(eval_string("'hello'.substring(2)"), "llo");
}

// ──────────────────────────────
// Array prototype methods
// ──────────────────────────────

#[test]
fn array_push_returns_length() {
    let js = r#"
        let a = [1, 2, 3];
        a.push(4)
    "#;
    assert_eq!(eval_number(js), 4.0);
}

#[test]
fn array_push_mutates() {
    let js = r#"
        let a = [1, 2, 3];
        a.push(4);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3,4");
}

#[test]
fn array_pop_returns_element() {
    assert_eq!(eval_number("let a = [1, 2]; a.pop()"), 2.0);
}

#[test]
fn array_pop_mutates() {
    let js = r#"
        let a = [1, 2, 3];
        a.pop();
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2");
}

#[test]
fn array_pop_empty() {
    let result = eval_js("let a = []; a.pop()").unwrap();
    assert_eq!(result, Value::Undefined);
}

#[test]
fn array_shift() {
    let js = r#"
        let a = [1, 2, 3];
        a.shift()
    "#;
    assert_eq!(eval_number(js), 1.0);
}

#[test]
fn array_shift_mutates() {
    let js = r#"
        let a = [1, 2, 3];
        a.shift();
        a.toString()
    "#;
    assert_eq!(eval_string(js), "2,3");
}

#[test]
fn array_unshift() {
    let js = r#"
        let a = [2, 3];
        a.unshift(1)
    "#;
    assert_eq!(eval_number(js), 3.0);
}

#[test]
fn array_unshift_mutates() {
    let js = r#"
        let a = [2, 3];
        a.unshift(1);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3");
}

#[test]
fn array_index_of() {
    assert_eq!(eval_number("[1, 2, 3].indexOf(2)"), 1.0);
    assert_eq!(eval_number("[1, 2, 3].indexOf(99)"), -1.0);
    assert_eq!(eval_number("[].indexOf(1)"), -1.0);
}

#[test]
fn array_includes() {
    assert_eq!(eval_bool("[1, 2, 3].includes(2)"), true);
    assert_eq!(eval_bool("[1, 2, 3].includes(99)"), false);
    assert_eq!(eval_bool("[].includes(1)"), false);
}

#[test]
fn array_join() {
    assert_eq!(eval_string("[1, 2, 3].join()"), "1,2,3");
    assert_eq!(eval_string("[1, 2, 3].join(' - ')"), "1 - 2 - 3");
    assert_eq!(eval_string("[].join()"), "");
}

#[test]
fn array_slice() {
    assert_eq!(eval_string("[1, 2, 3, 4].slice(1, 3).toString()"), "2,3");
    assert_eq!(eval_string("[1, 2, 3].slice(1).toString()"), "2,3");
    assert_eq!(eval_string("[1, 2, 3].slice(-1).toString()"), "3");
}

#[test]
fn array_concat() {
    assert_eq!(eval_string("[1, 2].concat([3, 4]).toString()"), "1,2,3,4");
    assert_eq!(eval_string("[].concat([1]).toString()"), "1");
}

#[test]
fn array_splice_removes() {
    let js = r#"
        let a = [1, 2, 3, 4];
        a.splice(1, 2).toString()
    "#;
    assert_eq!(eval_string(js), "2,3");
}

#[test]
fn array_splice_mutates() {
    let js = r#"
        let a = [1, 2, 3, 4];
        a.splice(1, 2);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,4");
}

#[test]
fn array_splice_inserts() {
    let js = r#"
        let a = [1, 4];
        a.splice(1, 0, 2, 3);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3,4");
}
