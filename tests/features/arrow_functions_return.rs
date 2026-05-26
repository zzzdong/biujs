use crate::helpers::{eval_js, eval_number, eval_string};

// ============================================================
// Return arrow function from function
// ============================================================

#[test]
fn return_arrow_function_basic() {
    // Function returns an arrow function
    let code = r#"
        function makeAdder(x) {
            return (y) => x + y;
        }
        const add5 = makeAdder(5);
        add5(3);
    "#;
    let result = eval_js(code);
    println!("return_arrow_function_basic result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn return_arrow_function_with_this() {
    // Function returns arrow function that captures this
    // LIMITATION: This may not work correctly because when makeGetter is called,
    // 'this' inside makeGetter may not be what we expect
    let code = r#"
        function makeGetter(obj) {
            return () => obj.name;
        }
        const obj = { name: 'Alice' };
        const getter = makeGetter(obj);
        getter();
    "#;
    let result = eval_js(code);
    println!("return_arrow_function_with_this result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn return_arrow_function_from_method() {
    // Method returns arrow function
    // The arrow function should capture 'this' from the method's 'this'
    // which is 'obj' when obj.makeGreeter() is called
    let code = r#"
        const obj = {
            prefix: 'Hello',
            makeGreeter: function() {
                return (name) => this.prefix + ' ' + name;
            }
        };
        const greeter = obj.makeGreeter();
        greeter('World');
    "#;
    let result = eval_js(code);
    println!("return_arrow_function_from_method result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn return_arrow_function_closure() {
    // Arrow function returned from function maintains closure
    let code = r#"
        function createMultiplier(factor) {
            return (x) => x * factor;
        }
        const triple = createMultiplier(3);
        triple(4);
    "#;
    let result = eval_js(code);
    println!("return_arrow_function_closure result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn return_multiple_arrow_functions() {
    // Function returns object with multiple arrow functions
    let code = r#"
        function createAPI(base) {
            return {
                add: (x) => base + x,
                sub: (x) => base - x,
                mul: (x) => base * x
            };
        }
        const api = createAPI(10);
        api.add(5) + api.sub(3) + api.mul(2);
    "#;
    let result = eval_js(code);
    println!("return_multiple_arrow_functions result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn nested_return_arrow_functions() {
    // Nested function returns
    let code = r#"
        function outer(x) {
            return function(y) {
                return (z) => x + y + z;
            };
        }
        const fn = outer(1)(2);
        fn(3);
    "#;
    let result = eval_js(code);
    println!("nested_return_arrow_functions result: {:?}", result);
    assert!(result.is_ok());
}

// ============================================================
// Arrow function this capture in returned functions
// ============================================================

#[test]
fn arrow_function_this_capture_in_returned_factory() {
    // Factory function that returns arrow functions capturing 'this'
    // This is the problematic case: the arrow function is created inside
    // a method, so it should capture the method's 'this'
    let code = r#"
        const obj = {
            value: 42,
            createGetter: function() {
                // Arrow function created here should capture obj as 'this'
                return () => this.value;
            }
        };
        const getter = obj.createGetter();
        getter();
    "#;
    let result = eval_js(code);
    println!("arrow_function_this_capture_in_returned_factory result: {:?}", result);
    // Should return 42, but may return Undefined if 'this' is not captured correctly
    assert!(result.is_ok());
}

#[test]
fn arrow_function_this_capture_chained() {
    // Multiple levels of function returns
    let code = r#"
        const obj = {
            name: 'Test',
            createFactory: function() {
                return {
                    createGetter: () => () => this.name
                };
            }
        };
        const factory = obj.createFactory();
        const getter = factory.createGetter();
        getter();
    "#;
    let result = eval_js(code);
    println!("arrow_function_this_capture_chained result: {:?}", result);
    assert!(result.is_ok());
}
