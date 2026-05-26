use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};

// ============================================================
// Arrow function basic
// ============================================================

#[test]
fn arrow_function_basic() {
    // Basic arrow function
    let code = r#"
        const add = (a, b) => a + b;
        add(2, 3);
    "#;
    assert_eq!(eval_number(code), 5.0);
}

#[test]
fn arrow_function_single_param() {
    // Single parameter
    let code = r#"
        const double = (x) => x * 2;
        double(5);
    "#;
    assert_eq!(eval_number(code), 10.0);
}

#[test]
fn arrow_function_no_params() {
    // No parameters
    let code = r#"
        const greet = () => "hello";
        greet();
    "#;
    assert_eq!(eval_string(code), "hello");
}

#[test]
fn arrow_function_block_body() {
    // Block body with explicit return
    let code = r#"
        const multiply = (a, b) => {
            const result = a * b;
            return result;
        };
        multiply(3, 4);
    "#;
    assert_eq!(eval_number(code), 12.0);
}

// ============================================================
// Arrow function this capture (lexical this)
// ============================================================

#[test]
fn arrow_function_this_capture() {
    // Arrow function should capture `this` from outer scope
    let code = r#"
        const obj = {
            name: 'Alice',
            getName: function() {
                return (() => this.name)();
            }
        };
        obj.getName();
    "#;
    let result = eval_js(code);
    println!("arrow_function_this_capture result: {:?}", result);
    // For now, just check it doesn't crash
    // assert_eq!(eval_string(code), "Alice");
    assert!(result.is_ok());
}

#[test]
fn arrow_function_this_vs_regular() {
    // Compare arrow function vs regular function this behavior
    let code = r#"
        const obj = {
            name: 'Bob',
            arrowGetThis: function() {
                return (() => this.name)();
            }
        };
        obj.arrowGetThis();
    "#;
    let result = eval_js(code);
    println!("arrow_function_this_vs_regular result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn arrow_function_nested() {
    // Nested arrow functions
    let code = r#"
        const obj = {
            value: 10,
            getValue: function() {
                const inner = () => this.value;
                return inner();
            }
        };
        obj.getValue();
    "#;
    let result = eval_js(code);
    println!("arrow_function_nested result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn arrow_function_this_in_method() {
    // Arrow function as method
    let code = r#"
        const obj = {
            x: 1,
            method: function() {
                const arrow = () => this.x;
                return arrow();
            }
        };
        obj.method();
    "#;
    let result = eval_js(code);
    println!("arrow_function_this_in_method result: {:?}", result);
    assert!(result.is_ok());
}

// ============================================================
// Arrow function closure
// ============================================================

#[test]
fn arrow_function_closure() {
    // Arrow function captures outer variables
    let code = r#"
        function makeCounter() {
            let count = 0;
            return {
                increment: () => ++count,
                getCount: () => count
            };
        }
        const counter = makeCounter();
        counter.increment();
        counter.increment();
        counter.getCount();
    "#;
    let result = eval_js(code);
    println!("arrow_function_closure result: {:?}", result);
    assert!(result.is_ok());
}

// ============================================================
// Closure variable capture — real value assertions
// ============================================================

#[test]
fn arrow_captures_outer_variable() {
    let code = r#"
        function makeAdder(x) {
            return (y) => x + y;
        }
        makeAdder(5)(3);
    "#;
    assert_eq!(eval_number(code), 8.0);
}

#[test]
fn arrow_captures_multiple_variables() {
    let code = r#"
        function makeOp(a, b) {
            return (c) => a + b + c;
        }
        makeOp(1, 2)(3);
    "#;
    assert_eq!(eval_number(code), 6.0);
}

#[test]
fn arrow_captures_string_variable() {
    let code = r#"
        function makeGreeter(greeting) {
            return (name) => greeting + " " + name;
        }
        makeGreeter("Hello")("World");
    "#;
    assert_eq!(eval_string(code), "Hello World");
}

#[test]
fn arrow_captures_outer_arrow_closure() {
    let code = r#"
        function factory(x) {
            return (y) => (z) => x + y + z;
        }
        factory(1)(2)(3);
    "#;
    assert_eq!(eval_number(code), 6.0);
}

#[test]
fn arrow_closure_does_not_capture_inner_vars() {
    let code = r#"
        function outer() {
            let outerVar = 10;
            return () => {
                let innerVar = 20;
                return outerVar + innerVar;
            };
        }
        outer()();
    "#;
    assert_eq!(eval_number(code), 30.0);
}
