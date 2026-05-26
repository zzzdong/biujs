use crate::helpers::eval_js;

#[test]
fn test_nested_function_catch_scope() {
    let js = r#"
        function f(o) {
            function innerf(o, x) {
                try {
                    throw o;
                }
                catch (e) {
                    return x;
                }
            }
            return innerf(o, 42);
        }
        f({})
    "#;
    
    let result = eval_js(js);
    println!("Result: {:?}", result);
    assert!(result.is_ok(), "Test failed: {:?}", result);
}

#[test]
fn test_nested_function_catch_property() {
    let js = r#"
        function f(o) {
            function innerf(o) {
                try {
                    throw o;
                }
                catch (e) {
                    return e.x;
                }
            }
            return innerf(o);
        }
        f({x: 42})
    "#;
    
    let result = eval_js(js);
    println!("Result: {:?}", result);
    assert!(result.is_ok(), "Test failed: {:?}", result);
}

#[test]
fn test_with_assert_samevalue() {
    // Minimal test to find the issue
    let js = r#"
        function f(o) {
            function innerf(o, x) {
                try {
                    throw o;
                }
                catch (e) {
                    return x;
                }
            }
            return innerf(o, 42);
        }
        f({})
    "#;
    
    let result = eval_js(js);
    println!("Result: {:?}", result);
    assert!(result.is_ok(), "Test failed: {:?}", result);
}

#[test]
fn test_instanceof_basic() {
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        
        var err = new Test262Error("test");
        err.message
    "#;
    
    let result = eval_js(js);
    println!("instanceof test Result (no instanceof): {:?}", result);
    assert!(result.is_ok(), "Test failed: {:?}", result);
}

#[test]
fn test_instanceof_check() {
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        
        var err = new Test262Error("test");
        err instanceof Test262Error
    "#;
    
    let result = eval_js(js);
    println!("instanceof check Result: {:?}", result);
    assert!(result.is_ok(), "Test failed: {:?}", result);
}
