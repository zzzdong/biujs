//! Direct feature tests for biujs
//!
//! These tests exercise biujs's currently supported features by running
//! JavaScript code and checking the resulting value. Each test returns
//! a numeric result that we verify in Rust.

use biujs::{Compiler, VM, Value};

/// Compile and run JavaScript, returning the last expression's value.
fn eval_js(source: &str) -> Result<Value, String> {
    let mut compiler = Compiler::new();
    let module = compiler
        .compile(source)
        .map_err(|e| format!("Compile error: {}", e))?;
    let mut vm = VM::new();
    vm.run(&module).map_err(|e| format!("Runtime error: {}", e))
}

/// Helper to run JS and expect a number result
fn eval_number(source: &str) -> f64 {
    match eval_js(source) {
        Ok(Value::Number(n)) => n,
        Ok(other) => panic!("Expected Number, got {:?}", other),
        Err(e) => panic!("{}", e),
    }
}

/// Helper to run JS and expect a boolean result
fn eval_bool(source: &str) -> bool {
    match eval_js(source) {
        Ok(Value::Bool(b)) => b,
        Ok(other) => panic!("Expected Bool, got {:?}", other),
        Err(e) => panic!("{}", e),
    }
}

/// Helper to run JS and expect a string result
fn eval_string(source: &str) -> String {
    match eval_js(source) {
        Ok(Value::String(s)) => s,
        Ok(other) => panic!("Expected String, got {:?}", other),
        Err(e) => panic!("{}", e),
    }
}

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
// if/else
// ============================================================

#[test]
fn if_statement() {
    assert_eq!(eval_number("let x = 5; if (x > 3) { x = 10; } x"), 10.0);
    assert_eq!(eval_number("let x = 1; if (x > 3) { x = 10; } x"), 1.0);
}

#[test]
fn if_else_statement() {
    assert_eq!(eval_number("let x = 5; if (x > 3) { x = 10; } else { x = 20; } x"), 10.0);
    assert_eq!(eval_number("let x = 1; if (x > 3) { x = 10; } else { x = 20; } x"), 20.0);
}

#[test]
fn if_else_if() {
    let js = r#"
        let x = 5;
        let result = 0;
        if (x > 10) {
            result = 1;
        } else if (x > 3) {
            result = 2;
        } else {
            result = 3;
        }
        result
    "#;
    assert_eq!(eval_number(js), 2.0);
}

// ============================================================
// while loop
// ============================================================

#[test]
fn while_loop() {
    let js = r#"
        let i = 0;
        let sum = 0;
        while (i < 5) {
            sum = sum + i;
            i = i + 1;
        }
        sum
    "#;
    assert_eq!(eval_number(js), 10.0);
}

#[test]
fn while_loop_zero_iterations() {
    let js = r#"
        let x = 10;
        while (x < 0) {
            x = x + 1;
        }
        x
    "#;
    assert_eq!(eval_number(js), 10.0);
}

// ============================================================
// for loop
// ============================================================

#[test]
fn for_loop() {
    let js = r#"
        let sum = 0;
        for (let i = 0; i < 10; i = i + 1) {
            sum = sum + i;
        }
        sum
    "#;
    assert_eq!(eval_number(js), 45.0);
}

#[test]
fn for_loop_nested() {
    let js = r#"
        let total = 0;
        for (let i = 0; i < 3; i = i + 1) {
            for (let j = 0; j < 3; j = j + 1) {
                total = total + 1;
            }
        }
        total
    "#;
    assert_eq!(eval_number(js), 9.0);
}

#[test]
fn for_loop_with_break() {
    let js = r#"
        let sum = 0;
        for (let i = 0; i < 100; i = i + 1) {
            if (i == 5) {
                break;
            }
            sum = sum + i;
        }
        sum
    "#;
    assert_eq!(eval_number(js), 10.0);
}

#[test]
fn for_loop_with_continue() {
    let js = r#"
        let sum = 0;
        for (let i = 0; i < 10; i = i + 1) {
            if (i % 2 == 0) {
                continue;
            }
            sum = sum + i;
        }
        sum
    "#;
    assert_eq!(eval_number(js), 25.0); // 1 + 3 + 5 + 7 + 9 = 25
}

// ============================================================
// Function declarations
// ============================================================

#[test]
fn function_declaration() {
    let js = r#"
        function add(a, b) {
            return a + b;
        }
        add(3, 4)
    "#;
    assert_eq!(eval_number(js), 7.0);
}

#[test]
fn function_no_args() {
    let js = r#"
        function getVal() {
            return 42;
        }
        getVal()
    "#;
    assert_eq!(eval_number(js), 42.0);
}

#[test]
fn function_recursion() {
    let js = r#"
        function factorial(n) {
            if (n <= 1) {
                return 1;
            }
            return n * factorial(n - 1);
        }
        factorial(5)
    "#;
    assert_eq!(eval_number(js), 120.0);
}

#[test]
fn function_hoisting() {
    let js = r#"
        let result = getValue();
        function getValue() {
            return 99;
        }
        result
    "#;
    assert_eq!(eval_number(js), 99.0);
}

#[test]
fn function_nested() {
    let js = r#"
        function outer(x) {
            function inner(y) {
                return y * 2;
            }
            return inner(x) + 1;
        }
        outer(5)
    "#;
    assert_eq!(eval_number(js), 11.0);
}

// ============================================================
// Combined features
// ============================================================

#[test]
fn fizzbuzz_lite() {
    let js = r#"
        let count = 0;
        for (let i = 1; i <= 30; i = i + 1) {
            if (i % 3 == 0) {
                count = count + 1;
            }
            if (i % 5 == 0) {
                count = count + 1;
            }
        }
        count
    "#;
    // multiples of 3: 10, multiples of 5: 6, multiples of 15: 2 (counted twice)
    // total increments: 10 + 6 = 16
    assert_eq!(eval_number(js), 16.0);
}

#[test]
fn fibonacci() {
    let js = r#"
        function fib(n) {
            if (n <= 1) {
                return n;
            }
            let a = 0;
            let b = 1;
            for (let i = 2; i <= n; i = i + 1) {
                let temp = b;
                b = a + b;
                a = temp;
            }
            return b;
        }
        fib(10)
    "#;
    assert_eq!(eval_number(js), 55.0);
}

#[test]
fn is_even_function() {
    let js = r#"
        function isEven(n) {
            return n % 2 == 0;
        }
        let result = 0;
        for (let i = 0; i < 10; i = i + 1) {
            if (isEven(i)) {
                result = result + i;
            }
        }
        result
    "#;
    // 0 + 2 + 4 + 6 + 8 = 20
    assert_eq!(eval_number(js), 20.0);
}

#[test]
fn string_typeof_combination() {
    let js = r#"
        let x = typeof 42;
        let y = typeof 'hello';
        let z = typeof true;
        let w = typeof undefined;
        let count = 0;
        if (x == 'number') { count = count + 1; }
        if (y == 'string') { count = count + 1; }
        if (z == 'boolean') { count = count + 1; }
        if (w == 'undefined') { count = count + 1; }
        count
    "#;
    assert_eq!(eval_number(js), 4.0);
}

#[test]
fn double_negation_to_boolean() {
    assert_eq!(eval_bool("!!1"), true);
    assert_eq!(eval_bool("!!0"), false);
    assert_eq!(eval_bool("!!''"), false);
    assert_eq!(eval_bool("!!'x'"), true);
}

#[test]
fn complex_expression() {
    let js = r#"
        let a = 2;
        let b = 3;
        let c = 4;
        (a + b) * c - a * b + c / 2
    "#;
    // (2+3)*4 - 2*3 + 4/2 = 20 - 6 + 2 = 16
    assert_eq!(eval_number(js), 16.0);
}

#[test]
fn comparison_in_conditional() {
    let js = r#"
        let x = 42;
        let label = '';
        if (x > 100) {
            label = 'big';
        } else if (x > 10) {
            label = 'medium';
        } else {
            label = 'small';
        }
        let len = 0;
        if (label == 'medium') { len = 6; }
        len
    "#;
    assert_eq!(eval_number(js), 6.0);
}
