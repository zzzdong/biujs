pub mod bytecode;
pub mod compiler;
pub mod vm;
pub mod builtins;
pub mod host;
pub mod module;

pub use compiler::Compiler;
pub use vm::VM;
pub use vm::Value;

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: compile and run JS code, return the result
    fn eval(js: &str) -> Value {
        let mut compiler = Compiler::new();
        let module = compiler.compile(js).expect("compilation failed");
        let mut vm = VM::new();
        vm.run(&module).expect("runtime error")
    }

    #[test]
    fn test_number_literal() {
        assert_eq!(eval("42;"), Value::Number(42.0));
    }

    #[test]
    fn test_float_literal() {
        assert_eq!(eval("3.14;"), Value::Number(3.14));
    }

    #[test]
    fn test_addition() {
        assert_eq!(eval("1 + 2;"), Value::Number(3.0));
    }

    #[test]
    fn test_subtraction() {
        assert_eq!(eval("10 - 3;"), Value::Number(7.0));
    }

    #[test]
    fn test_multiplication() {
        assert_eq!(eval("4 * 5;"), Value::Number(20.0));
    }

    #[test]
    fn test_division() {
        assert_eq!(eval("10 / 2;"), Value::Number(5.0));
    }

    #[test]
    fn test_remainder() {
        assert_eq!(eval("10 % 3;"), Value::Number(1.0));
    }

    #[test]
    fn test_negation() {
        assert_eq!(eval("-5;"), Value::Number(-5.0));
    }

    #[test]
    fn test_variable_declaration() {
        assert_eq!(eval("let x = 42; x;"), Value::Number(42.0));
    }

    #[test]
    fn test_variable_arithmetic() {
        assert_eq!(eval("let x = 10; let y = 20; x + y;"), Value::Number(30.0));
    }

    #[test]
    fn test_boolean_true() {
        assert_eq!(eval("true;"), Value::Bool(true));
    }

    #[test]
    fn test_boolean_false() {
        assert_eq!(eval("false;"), Value::Bool(false));
    }

    #[test]
    fn test_null() {
        assert_eq!(eval("null;"), Value::Null);
    }

    #[test]
    fn test_comparison_gt() {
        assert_eq!(eval("5 > 3;"), Value::Bool(true));
        assert_eq!(eval("3 > 5;"), Value::Bool(false));
    }

    #[test]
    fn test_comparison_lt() {
        assert_eq!(eval("3 < 5;"), Value::Bool(true));
        assert_eq!(eval("5 < 3;"), Value::Bool(false));
    }

    #[test]
    fn test_comparison_eq() {
        assert_eq!(eval("5 == 5;"), Value::Bool(true));
        assert_eq!(eval("5 == 3;"), Value::Bool(false));
    }

    #[test]
    fn test_comparison_neq() {
        assert_eq!(eval("5 != 3;"), Value::Bool(true));
        assert_eq!(eval("5 != 5;"), Value::Bool(false));
    }

    #[test]
    fn test_logical_not() {
        assert_eq!(eval("!true;"), Value::Bool(false));
        assert_eq!(eval("!false;"), Value::Bool(true));
    }

    #[test]
    fn test_typeof_number() {
        assert_eq!(eval("typeof 42;"), Value::String("number".to_string()));
    }

    #[test]
    fn test_typeof_boolean() {
        assert_eq!(eval("typeof true;"), Value::String("boolean".to_string()));
    }

    #[test]
    fn test_typeof_undefined() {
        assert_eq!(eval("typeof undefined;"), Value::String("undefined".to_string()));
    }

    #[test]
    fn test_chained_expressions() {
        assert_eq!(eval("1 + 2 + 3;"), Value::Number(6.0));
    }

    #[test]
    fn test_precedence() {
        assert_eq!(eval("2 + 3 * 4;"), Value::Number(14.0));
    }

    #[test]
    fn test_parenthesized() {
        assert_eq!(eval("(2 + 3) * 4;"), Value::Number(20.0));
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(eval("\"hello\";"), Value::String("hello".to_string()));
    }

    #[test]
    fn test_undefined() {
        assert_eq!(eval("undefined;"), Value::Undefined);
    }

    #[test]
    fn test_empty_program() {
        assert_eq!(eval(""), Value::Undefined);
    }
}
