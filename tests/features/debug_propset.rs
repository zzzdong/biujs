//! Debug test for PropSet error

use crate::helpers::eval_js;

#[test]
fn debug_propset_error_step1() {
    // Step 1: Just define Test262Error
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 1 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step2() {
    // Step 2: Add instanceof check
    let js = r#"
        function Test262Error(message) {
          if (!(this instanceof Test262Error)) return new Test262Error(message);
          this.message = message || "";
        }
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 2 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step3a() {
    // Step 3a: Just assign to prototype
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        
        Test262Error.prototype.toString = function () {
          return "Test262Error: " + this.message;
        };
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 3a Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step3a1() {
    // Step 3a1: Just access prototype
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        
        let proto = Test262Error.prototype;
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 3a1 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step3a2() {
    // Step 3a2: Assign a simple value to prototype
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        
        Test262Error.prototype.x = 1;
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 3a2 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step3a3() {
    // Step 3a3: Assign a function to prototype
    let js = r#"
        function Test262Error(message) {
          this.message = message || "";
        }
        
        Test262Error.prototype.foo = function () {
          return "foo";
        };
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 3a3 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step3b() {
    // Step 3b: Check if instanceof is the problem
    let js = r#"
        function Test262Error(message) {
          if (!(this instanceof Test262Error)) return new Test262Error(message);
          this.message = message || "";
        }
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 3b Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step3c() {
    // Step 3c: Combine instanceof and prototype assignment
    let js = r#"
        function Test262Error(message) {
          if (!(this instanceof Test262Error)) return new Test262Error(message);
          this.message = message || "";
        }
        
        Test262Error.prototype.toString = function () {
          return "Test262Error: " + this.message;
        };
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 3c Result: {:?}", result);
    // This is expected to fail - let's see the error
}

#[test]
fn debug_propset_error_step4() {
    // Step 4: Add assert function
    let js = r#"
        function Test262Error(message) {
          if (!(this instanceof Test262Error)) return new Test262Error(message);
          this.message = message || "";
        }
        
        Test262Error.prototype.toString = function () {
          return "Test262Error: " + this.message;
        };
        
        function assert(mustBeTrue, message) {
          if (mustBeTrue === true) {
            return;
          }
          throw new Test262Error(message);
        }
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 4 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step5() {
    // Step 5: Add assert.sameValue
    let js = r#"
        function Test262Error(message) {
          if (!(this instanceof Test262Error)) return new Test262Error(message);
          this.message = message || "";
        }
        
        Test262Error.prototype.toString = function () {
          return "Test262Error: " + this.message;
        };
        
        function assert(mustBeTrue, message) {
          if (mustBeTrue === true) {
            return;
          }
          throw new Test262Error(message);
        }
        
        assert.sameValue = function (actual, expected, message) {
          if (actual === expected) {
            return;
          }
          throw new Test262Error(message);
        };
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 5 Result: {:?}", result);
    assert!(result.is_ok());
}

#[test]
fn debug_propset_error_step6() {
    // Step 6: Call assert.sameValue
    let js = r#"
        function Test262Error(message) {
          if (!(this instanceof Test262Error)) return new Test262Error(message);
          this.message = message || "";
        }
        
        Test262Error.prototype.toString = function () {
          return "Test262Error: " + this.message;
        };
        
        function assert(mustBeTrue, message) {
          if (mustBeTrue === true) {
            return;
          }
          throw new Test262Error(message);
        }
        
        assert.sameValue = function (actual, expected, message) {
          if (actual === expected) {
            return;
          }
          throw new Test262Error(message);
        };
        
        assert.sameValue(1, 1);
        "done"
    "#;

    let result = eval_js(js);
    println!("Step 6 Result: {:?}", result);
    // This should pass since 1 === 1
    assert!(result.is_ok(), "Step 6 failed: {:?}", result);
}
