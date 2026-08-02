use crate::helpers::{eval_bool, eval_number, eval_string};

// ============================================================
// Complex / combined expressions
// ============================================================

#[test]
fn complex_expression() {
    assert_eq!(eval_number("(2 + 3) * (10 - 4) / 3"), 10.0);
}

#[test]
fn fibonacci() {
    let js = r#"
        let a = 0, b = 1, temp = 0;
        let i = 0;
        while (i < 10) {
            temp = a + b;
            a = b;
            b = temp;
            i = i + 1;
        }
        a
    "#;
    assert_eq!(eval_number(js), 55.0);
}

#[test]
fn nested_combined_control_flow() {
    let js = r#"
        let sum = 0;
        for (let i = 0; i < 100; i = i + 1) {
            if (i >= 50) {
                break;
            }
            if (i % 2 == 0) {
                sum = sum + i;
            }
        }
        sum
    "#;
    assert_eq!(eval_number(js), 600.0); // 0+2+4+...+48 = 600
}

#[test]
fn string_concatenation_with_numbers() {
    assert_eq!(eval_string("'hello' + ' ' + 'world'"), "hello world");
    assert_eq!(eval_string("'a' + 1"), "a1");
    assert_eq!(eval_string("1 + 'a'"), "1a");
}

#[test]
fn typeof_complex() {
    assert_eq!(eval_string("typeof (1 + 2)"), "number");
    assert_eq!(eval_string("typeof ('a' + 'b')"), "string");
    assert_eq!(eval_string("typeof (1 + 'a')"), "string");
}

/// Type coercion with == (abstract equality)
#[test]
fn combined_type_coercion() {
    assert_eq!(eval_bool("1 == '1'"), true);
    assert_eq!(eval_bool("0 == ''"), true);
    assert_eq!(eval_bool("0 == ' '"), true);
}

/// Object vs Primitive with == (ToPrimitive coercion)
#[test]
fn object_vs_primitive_coercion() {
    assert_eq!(eval_bool("[1] == 1"), true);
    assert_eq!(eval_bool("[1,2,3] == '1,2,3'"), true);
    assert_eq!(eval_bool("[] == ''"), true);
    assert_eq!(eval_bool("[1,2] == '1,2'"), true);
}

/// User-defined valueOf / toString must be honoured during coercion (ToPrimitive).
#[test]
fn user_defined_valueof_toString() {
    assert_eq!(eval_number("({ valueOf: () => 42 }) + 1"), 43.0);
    assert_eq!(eval_string("({ toString: () => 'hi' }) + '!'"), "hi!");
    assert_eq!(eval_bool("({ valueOf: () => 7 }) == 7"), true);
    assert_eq!(eval_bool("({ toString: () => '7' }) == 7"), true);
    // `Add` uses the "default" hint, so `valueOf` is tried first -> 1 -> "1".
    assert_eq!(
        eval_string("'' + ({ toString: () => 'a', valueOf: () => 1 })"),
        "1"
    );
}

/// Object + string uses ToPrimitive (not a raw "[object Object]" fallback).
#[test]
fn object_plus_string_concat() {
    assert_eq!(eval_string("[1,2] + '-end'"), "1,2-end");
    assert_eq!(eval_string("'x' + ({ toString: () => 'y' })"), "xy");
}

/// Relational comparison coerces objects via ToPrimitive.
#[test]
fn object_relational_coercion() {
    assert!(eval_bool("({ valueOf: () => 5 }) < 10"));
    assert!(eval_bool("10 > ({ valueOf: () => 5 })"));
}

/// A very deep expression (far more than the register count) must compile and run
/// without panicking (register allocator spills to the stack instead).
#[test]
fn deep_expression_no_panic() {
    let mut js = String::from("0");
    for i in 1..200 {
        js.push_str(" + ");
        js.push_str(&i.to_string());
    }
    let expected: f64 = (0..200).map(|i| i as f64).sum();
    assert_eq!(eval_number(&js), expected);
}

/// A function call with many (>8) arguments must not panic.
#[test]
fn many_arguments_no_panic() {
    let params: Vec<String> = (0..12).map(|i| format!("a{i}")).collect();
    let args: Vec<String> = (1..=12).map(|i| i.to_string()).collect();
    let js = format!(
        "function f({}) {{ return a11; }}\nf({})",
        params.join(","),
        args.join(",")
    );
    assert_eq!(eval_number(&js), 12.0);
}

#[test]
fn chained_comparisons() {
    assert!(eval_bool("0 <= 5"));
    assert!(eval_bool("5 <= 10"));
}

#[test]
fn conditional_assignment() {
    let js = r#"
        let x = 5;
        let y = 10;
        let max = x;
        if (y > x) {
            max = y;
        }
        max
    "#;
    assert_eq!(eval_number(js), 10.0);
}

// ============================================================
// Prototype chain
// ============================================================

/// Array.prototype.toString joins elements with comma
#[test]
fn array_to_string() {
    assert_eq!(eval_string("[1,2,3].toString()"), "1,2,3");
    assert_eq!(eval_string("[].toString()"), "");
    assert_eq!(eval_string("[1].toString()"), "1");
    assert_eq!(eval_string("['a','b','c'].toString()"), "a,b,c");
}

/// Object.prototype.toString returns [object ClassName]
#[test]
fn object_to_string() {
    let js = r#"
        let obj = {};
        obj.toString()
    "#;
    assert_eq!(eval_string(js), "[object Object]");
}

/// Array.prototype.toString is inherited via prototype chain
#[test]
fn array_to_string_via_prototype() {
    let js = r#"
        let arr = [1, 2, 3];
        arr.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3");
}

/// Dynamic computed property call (bracket notation)
#[test]
fn computed_method_call() {
    assert_eq!(eval_string("[1,2,3][\"toString\"]()"), "1,2,3");
    assert_eq!(eval_string("[] [\"toString\"]()"), "");
}

/// Dynamic variable as property name
#[test]
fn dynamic_property_name_call() {
    let js = r#"
        let arr = [10, 20, 30];
        let method = "toString";
        arr[method]()
    "#;
    assert_eq!(eval_string(js), "10,20,30");
}
