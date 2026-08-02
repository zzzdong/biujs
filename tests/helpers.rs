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

/// Run JS and expect a number result
pub fn eval_number(source: &str) -> f64 {
    match eval_js(source) {
        Ok(Value::Number(n)) => n,
        Ok(other) => panic!("Expected Number, got {:?}", other),
        Err(e) => panic!("{}", e),
    }
}

/// Run JS and expect a boolean result
pub fn eval_bool(source: &str) -> bool {
    match eval_js(source) {
        Ok(Value::Bool(b)) => b,
        Ok(other) => panic!("Expected Bool, got {:?}", other),
        Err(e) => panic!("{}", e),
    }
}

/// Run JS and expect a string result
pub fn eval_string(source: &str) -> String {
    match eval_js(source) {
        Ok(Value::String(s)) => s.to_string(),
        Ok(other) => panic!("Expected String, got {:?}", other),
        Err(e) => panic!("{}", e),
    }
}
