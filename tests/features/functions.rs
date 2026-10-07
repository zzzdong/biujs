use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};
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

// ============================================================
// Rest parameters (`...rest`)
// ============================================================
//
// `FormalParameters.rest` is a *separate* AST field from `items`, and the
// lowering only walked `items` — so the rest binding never happened at all and
// every function using one died with `ReferenceError: undefined variable`.
// That also left `language/rest-parameters` (11 tests) reporting 0/0/0, because
// `SUITES` named a directory that does not exist.

#[test]
fn rest_parameter_binds_a_fresh_array() {
    // Always an array, always a *new* one, and present even for a bare call.
    assert_eq!(
        eval_string(
            "function f(...a) { return [a.length, Array.isArray(a), typeof a[0]].join(','); }
             f()"
        ),
        "0,true,undefined"
    );
    // Each call gets its *own* array, so the identity check yields 0*100 + 2.
    assert_eq!(
        eval_number(
            "function f(...a) { return a; }
             var x = f(1, 2);
             var y = f(1, 2);
             (x === y ? 1 : 0) * 100 + x.length"
        ),
        2.0
    );
    assert_eq!(
        eval_bool(
            "function f(...a) { return a; }
             var x = f(1);
             x.push(2);
             f(1).length === 1"
        ),
        true
    );
}

#[test]
fn rest_parameter_follows_the_declared_parameters() {
    assert_eq!(
        eval_string("function f(x, ...r) { return x + ':' + r.join('-'); } f(1, 2, 3)"),
        "1:2-3"
    );
    // `fn.length` counts neither the rest element nor anything from the first
    // default onwards.
    assert_eq!(eval_number("function f(a, ...b) {} f.length"), 1.0);
    assert_eq!(eval_number("function g(...a) {} g.length"), 0.0);
    assert_eq!(eval_number("function h(a = 1, ...b) {} h.length"), 0.0);
}

#[test]
fn rest_parameter_may_be_a_pattern() {
    // `...[] { }` and `...{} { }`: the rest array is destructured like any other
    // value, so defaults and nesting come for free.
    assert_eq!(eval_string("function f(...[a, b]) { return a + '/' + b; } f(1, 2, 3)"), "1/2");
    assert_eq!(eval_number("function f(...[a, b = 9]) { return b; } f(1)"), 9.0);
    assert_eq!(eval_string("function f(...{length}) { return 'n' + length; } f(1, 2, 3)"), "n3");
}

#[test]
fn rest_parameter_works_in_every_function_shape() {
    // Arrow, method, class method, constructor and generator all share the
    // lowering that used to drop the rest element.
    assert_eq!(eval_number("var f = (...a) => a.length; f(1, 2, 3)"), 3.0);
    assert_eq!(eval_number("var o = { m(...a) { return a.length; } }; o.m(1, 2)"), 2.0);
    assert_eq!(
        eval_number(
            "class C { constructor(...a) { this.n = a.length; } m(...a) { return a.length; } }
             new C(1, 2, 3).n * 10 + new C().m(9)"
        ),
        31.0
    );
    assert_eq!(eval_number("function* g(...a) { yield a.length; } g(1, 2, 3, 4).next().value"), 4.0);
}

#[test]
fn rest_parameter_survives_call_and_apply() {
    assert_eq!(eval_string("function f(...a) { return a.join('-'); } f.apply(null, [1, 2, 3])"), "1-2-3");
    assert_eq!(eval_string("function f(...a) { return a.join('-'); } f.call(null, 4, 5)"), "4-5");
    // `arguments` and the rest array are independent bindings.
    assert_eq!(
        eval_string(
            "function f(a, ...r) { arguments[0] = 'changed'; r[0] = 'also'; return arguments[0] + '/' + r[0]; }
             f(1, 2)"
        ),
        "changed/also"
    );
}

#[test]
fn an_anonymous_function_takes_the_name_of_what_it_is_assigned_to() {
    // ES `NamedEvaluation`: the *binding* names an anonymous function …
    assert_eq!(eval_string("var f = function () {}; f.name"), "f");
    assert_eq!(eval_string("let l = () => {}; l.name"), "l");
    assert_eq!(eval_string("const c = class {}; c.name"), "c");
    // … and so does an assignment target.
    assert_eq!(eval_string("var a; a = function () {}; a.name"), "a");
    // … and a property key, including a computed one.
    assert_eq!(eval_string("({ m: function () {} }).m.name"), "m");
    assert_eq!(eval_string("({ m() {} }).m.name"), "m");
    assert_eq!(eval_string("({ ['k' + 1]: function () {} }).k1.name"), "k1");
    // …including a **generator** method, whose name is assigned at run time too
    // (the compile-time `CodeBlock::name` is empty for it — that is not a bug,
    // `SetFunctionName` fills it in; see `bytecode::CodeBlock::name`).
    assert_eq!(eval_string("({ *g() {} }).g.name"), "g");

    // A *named* function expression keeps its own name — that one is not a
    // NamedEvaluation site.
    assert_eq!(eval_string("var g = function inner() {}; g.name"), "inner");

    // A default value names the function after its target, in every pattern
    // position (`[a = fn] = []`, `({ o = fn } = {})`, `function p(x = fn) {}`).
    assert_eq!(eval_string("var a; [a = function () {}] = []; a.name"), "a");
    assert_eq!(eval_string("var o; ({ o = function () {} } = {}); o.name"), "o");
    assert_eq!(
        eval_string("function p(x = function () {}) { return x.name; } p()"),
        "x"
    );

    // With nothing to borrow a name from, it stays the empty string — not a
    // `<anonymous>` placeholder.
    assert_eq!(eval_string("(function () {}).name"), "");
    assert_eq!(eval_string("(class {}).name"), "");
    // An array element is not a NamedEvaluation site either.
    assert_eq!(eval_string("[function () {}][0].name"), "");
}

#[test]
fn a_function_expression_captures_its_enclosing_scope() {
    // `function` expressions now capture like arrows do: the inner function sees
    // the outer binding instead of falling through to the environment.
    assert_eq!(
        eval_number("function counter() { var i = 5; return function () { return i; }; } counter()()"),
        5.0
    );
    assert_eq!(
        eval_number("function outer() { var n = 1; return function () { return n + 1; }; } outer()()"),
        2.0
    );
    // Each evaluation captures its own copy.
    assert_eq!(
        eval_string(
            "function mk(v) { return function () { return v; }; }
             var a = mk(1), b = mk(2);
             a() + ',' + b()"
        ),
        "1,2"
    );
    // A *script-scope* binding is not captured — it already lives in the
    // environment, and both sides read/write it there. Capturing it would freeze
    // a copy (the single most common test262 shape reported 0 instead of 1).
    assert_eq!(
        eval_number("var callCount = 0; var f = function () { callCount = callCount + 1; }; f(); callCount"),
        1.0
    );
}
