//! Shared helper functions for feature integration tests.

use biujs::{Compiler, VM, Value};

/// Compile and run JavaScript, returning the last expression's value.
pub fn eval_js(source: &str) -> Result<Value, String> {
    let mut compiler = Compiler::new();
    let module = compiler
        .compile(source)
        .map_err(|e| format!("Compile error: {}", e))?;
    let mut vm = VM::new();
    vm.run(&module).map_err(|e| format!("Runtime error: {}", e))
}

/// Render a value for a failed expectation.
///
/// `{:?}` on an object walks its `[[Prototype]]` graph, and the built-in
/// prototypes are cyclic (`Object.prototype.constructor` → `Object` → …), so
/// debugging an unexpected object result used to abort the test binary with a
/// host stack overflow. Objects and functions are therefore summarised by kind.
pub fn describe(value: &Value) -> String {
    match value {
        Value::Object(_) => "an object".to_string(),
        Value::Function(_) => "a function".to_string(),
        other => format!("{other:?}"),
    }
}

/// Run JS and expect a number result
pub fn eval_number(source: &str) -> f64 {
    match eval_js(source) {
        Ok(Value::Number(n)) => n,
        Ok(other) => panic!("Expected Number, got {}", describe(&other)),
        Err(e) => panic!("{}", e),
    }
}

/// Run JS and expect a boolean result
pub fn eval_bool(source: &str) -> bool {
    match eval_js(source) {
        Ok(Value::Bool(b)) => b,
        Ok(other) => panic!("Expected Bool, got {}", describe(&other)),
        Err(e) => panic!("{}", e),
    }
}

/// Run JS and expect a string result
pub fn eval_string(source: &str) -> String {
    match eval_js(source) {
        Ok(Value::String(s)) => s.to_string(),
        Ok(other) => panic!("Expected String, got {}", describe(&other)),
        Err(e) => panic!("{}", e),
    }
}
