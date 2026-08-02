use crate::helpers::{eval_js, eval_number, eval_string};
use biujs::Value;

// ============================================================
// Functions
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
        function five() {
            return 5;
        }
        five()
    "#;
    assert_eq!(eval_number(js), 5.0);
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
        function outer(a) {
            function inner(b) {
                return b * 2;
            }
            return inner(a) + 1;
        }
        outer(5)
    "#;
    assert_eq!(eval_number(js), 11.0);
}

#[test]
fn function_multi_args() {
    let js = r#"
        function sum4(a, b, c, d) {
            return a + b + c + d;
        }
        sum4(1, 2, 3, 4)
    "#;
    assert_eq!(eval_number(js), 10.0);
}

#[test]
fn function_typeof() {
    let js = r#"typeof function() {}"#;
    assert_eq!(eval_string(js), "function");
}

// ============================================================
// Class
// ============================================================

#[test]
fn class_simple_declaration() {
    let js = r#"
        class Foo {
            constructor(x) {
                this.x = x;
            }
        }
        let f = new Foo(42);
        f.x
    "#;
    assert_eq!(eval_number(js), 42.0);
}

#[test]
fn class_default_constructor() {
    let js = r#"
        class Foo {
            getVal() {
                return 42;
            }
        }
        let f = new Foo();
        f.getVal()
    "#;
    assert_eq!(eval_number(js), 42.0);
}

#[test]
fn class_method() {
    let js = r#"
        class Counter {
            constructor(init) {
                this.count = init;
            }
            increment() {
                this.count = this.count + 1;
            }
            getCount() {
                return this.count;
            }
        }
        let c = new Counter(10);
        c.increment();
        c.getCount()
    "#;
    assert_eq!(eval_number(js), 11.0);
}

#[test]
fn class_multiple_instances() {
    let js = r#"
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
    "#;
    assert_eq!(eval_number(js), 3.0);
}

#[test]
fn class_this_is_object() {
    let js = r#"
        class Foo {
            constructor() {
                this.type = typeof this;
            }
        }
        let f = new Foo();
        f.type
    "#;
    assert_eq!(eval_string(js), "object");
}

#[test]
fn typeof_class_is_function() {
    let js = r#"typeof class Foo {}"#;
    assert_eq!(eval_string(js), "function");
}

#[test]
fn new_on_function() {
    let js = r#"
        function Foo(x) {
            this.x = x;
        }
        let f = new Foo(42);
        f.x
    "#;
    assert_eq!(eval_number(js), 42.0);
}

#[test]
fn new_on_function_returns_object() {
    let js = r#"
        function Foo(x) {
            this.x = x;
        }
        let f = new Foo(42);
        typeof f
    "#;
    assert_eq!(eval_string(js), "object");
}

#[test]
fn class_with_multiple_methods() {
    let js = r#"
        class Calc {
            constructor(x) {
                this.value = x;
            }
            add(n) {
                this.value = this.value + n;
            }
            sub(n) {
                this.value = this.value - n;
            }
            result() {
                return this.value;
            }
        }
        let c = new Calc(100);
        c.add(50);
        c.sub(20);
        c.result()
    "#;
    assert_eq!(eval_number(js), 130.0);
}
