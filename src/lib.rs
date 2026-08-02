pub mod builtins;
pub mod bytecode;
pub mod compiler;
pub mod host;
pub mod module;
pub mod vm;

pub use compiler::Compiler;
pub use compiler::error::CompileError;
pub use vm::RuntimeError;
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
        assert_eq!(eval("typeof 42;"), Value::string("number"));
    }

    #[test]
    fn test_typeof_boolean() {
        assert_eq!(eval("typeof true;"), Value::string("boolean"));
    }

    #[test]
    fn test_typeof_undefined() {
        assert_eq!(eval("typeof undefined;"), Value::string("undefined"));
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
        assert_eq!(eval("\"hello\";"), Value::string("hello"));
    }

    #[test]
    fn test_undefined() {
        assert_eq!(eval("undefined;"), Value::Undefined);
    }

    #[test]
    fn test_empty_program() {
        assert_eq!(eval(""), Value::Undefined);
    }

    #[test]
    fn test_boolean_factory() {
        assert_eq!(eval("Boolean(true)"), Value::Bool(true));
        assert_eq!(eval("Boolean(false)"), Value::Bool(false));
    }

    #[test]
    fn test_number_factory() {
        assert_eq!(eval("Number(42)"), Value::Number(42.0));
    }

    #[test]
    fn test_string_factory() {
        assert_eq!(eval("String(123)"), Value::string("123"));
    }

    #[test]
    fn test_type_error_factory() {
        let js = "let e = TypeError('bad'); e.name;";
        // Should not throw ReferenceError
        let result = eval(js);
        assert_eq!(result.to_js_string(), "TypeError");
    }

    #[test]
    fn test_class_debug() {
        // Test class with this
        let result = eval(
            r#"
            class Foo {
                constructor(x) {
                    this.x = x;
                }
                getX() {
                    return this.x;
                }
            }
            let f = new Foo(42);
            f.getX()
        "#,
        );
        assert_eq!(result, Value::Number(42.0));
    }

    #[test]
    fn test_class_debug2() {
        // Test a.getValue() only
        let result = eval(
            r#"
            class Box {
                constructor(v) {
                    this.value = v;
                }
                getValue() {
                    return this.value;
                }
            }
            let a = new Box(1);
            let b = new Box(2);
            a.getValue()
        "#,
        );
        assert_eq!(result, Value::Number(1.0));
    }

    #[test]
    fn test_class_debug3() {
        // Test b.getValue() only
        let result = eval(
            r#"
            class Box {
                constructor(v) {
                    this.value = v;
                }
                getValue() {
                    return this.value;
                }
            }
            let a = new Box(1);
            let b = new Box(2);
            b.getValue()
        "#,
        );
        assert_eq!(result, Value::Number(2.0));
    }

    #[test]
    fn test_class_debug4() {
        // Test two calls separately
        let result = eval(
            r#"
            class Box {
                constructor(v) {
                    this.value = v;
                }
                getValue() {
                    return this.value;
                }
            }
            let a = new Box(1);
            let b = new Box(2);
            a.getValue() + 0
        "#,
        );
        assert_eq!(result, Value::Number(1.0));
    }

    #[test]
    fn test_class_debug5() {
        // Test what new returns
        let result = eval(
            r#"
            class Box {
                constructor(v) {
                    this.value = v;
                }
            }
            let a = new Box(1);
            let b = new Box(2);
            a.value
        "#,
        );
        assert_eq!(result, Value::Number(1.0));
    }

    #[test]
    fn test_class_debug6() {
        // Exact same as failing feature test 'class_method'
        let result = eval(
            r#"
            class Counter {
                constructor(init) {
                    this.count = init;
                }
                increment() {
                    this.count = 999;
                }
                getCount() {
                    return this.count;
                }
            }
            let c = new Counter(10);
            c.increment();
            c.getCount()
        "#,
        );
        assert_eq!(result, Value::Number(999.0));
    }

    #[test]
    fn test_class_debug6a() {
        // Without increment, just direct this.count read
        let result = eval(
            r#"
            class Counter {
                constructor(init) {
                    this.count = init;
                }
                getCount() {
                    return this.count;
                }
            }
            let c = new Counter(10);
            c.getCount()
        "#,
        );
        assert_eq!(result, Value::Number(10.0));
    }

    #[test]
    fn test_class_debug7() {
        // Exact same as failing feature test 'class_multiple_instances'
        let result = eval(
            r#"
            class Box {
                constructor(v) {
                    this.value = v;
                }
                getValue() {
                    return this.value;
                }
            }
            let a = new Box(1);
            let b = new Box(2);
            a.getValue() + b.getValue()
        "#,
        );
        assert_eq!(result, Value::Number(3.0));
    }

    // ─────────────────────────────────────────────────────────
    // Error type tests
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_error_constructor_basic() {
        let result = eval(r#"new Error("test message")"#);
        assert!(result.is_object());
    }

    #[test]
    fn test_error_name_property() {
        let result = eval(r#"new Error("test").name"#);
        assert_eq!(result, Value::string("Error"));
    }

    #[test]
    fn test_error_message_property() {
        let result = eval(r#"new Error("hello world").message"#);
        assert_eq!(result, Value::string("hello world"));
    }

    #[test]
    fn test_type_error_constructor() {
        let result = eval(r#"new TypeError("type error msg").name"#);
        assert_eq!(result, Value::string("TypeError"));
    }

    #[test]
    fn test_type_error_message() {
        let result = eval(r#"new TypeError("bad type").message"#);
        assert_eq!(result, Value::string("bad type"));
    }

    #[test]
    fn test_reference_error_constructor() {
        let result = eval(r#"new ReferenceError("ref error").name"#);
        assert_eq!(result, Value::string("ReferenceError"));
    }

    #[test]
    fn test_range_error_constructor() {
        let result = eval(r#"new RangeError("range error").name"#);
        assert_eq!(result, Value::string("RangeError"));
    }

    #[test]
    fn test_error_empty_message() {
        let result = eval(r#"new Error().message"#);
        assert_eq!(result, Value::string(""));
    }

    #[test]
    fn test_error_catch_received_error_object() {
        // Test: create error object and access message directly
        let result = eval(
            r#"
            let err = new Error("test msg");
            err.message
        "#,
        );
        assert_eq!(result, Value::string("test msg"));

        // Test: throw an Error object and access its message
        // Note: This test is currently failing due to a known issue with
        // property access on catch block variables. The Error object
        // is correctly created and thrown, but e.message returns the
        // object itself instead of the message property.
        let result2 = eval(
            r#"
            try {
                throw new Error("caught error");
            } catch (e) {
                e.message
            }
        "#,
        );
        assert_eq!(result2, Value::string("caught error"));
    }

    #[test]
    fn test_catch_simple_string() {
        // Test: throw a string and catch it
        let result = eval(
            r#"
            try {
                throw "simple string";
            } catch (e) {
                e
            }
        "#,
        );
        assert_eq!(result, Value::string("simple string"));
    }

    #[test]
    fn test_error_catch_error_name() {
        // Test: create TypeError and access name directly
        let result = eval(
            r#"
            let err = new TypeError("type issue");
            err.name
        "#,
        );
        assert_eq!(result, Value::string("TypeError"));

        let result2 = eval(
            r#"
            try {
                throw new TypeError("type issue");
            } catch (e) {
                e.name
            }
        "#,
        );
        assert_eq!(result2, Value::string("TypeError"));
    }
}
