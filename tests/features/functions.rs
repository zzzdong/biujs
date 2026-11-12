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

// ── 函数对象的属性存储（shape 槽位 / 字典模式）────────────────────────────

#[test]
fn function_metadata_keeps_its_attributes_and_order() {
    // `length` 是 {writable:false, enumerable:false, configurable:true}：以前的
    // 独立属性表会被一次普通赋值冲成默认描述符，现在和普通对象共用一套存储。
    assert_eq!(
        eval_bool(
            "'use strict';
             function f(a, b) {}
             var d = Object.getOwnPropertyDescriptor(f, 'length');
             d.value === 2 && d.writable === false && d.enumerable === false &&
             d.configurable === true"
        ),
        true
    );
    assert_eq!(
        eval_string("'use strict'; function f(a) {} Object.getOwnPropertyNames(f).join(',')"),
        "length,name,prototype"
    );
    assert_eq!(
        eval_number(
            "'use strict';
             function f(a, b) {}
             try { f.length = 9; } catch (e) {}
             f.length"
        ),
        2.0
    );
}

#[test]
fn function_property_delete_and_freeze_use_the_shared_storage() {
    // 删属性降级为字典模式后，剩下的非默认描述符不能被冲掉。
    assert_eq!(
        eval_bool(
            "'use strict';
             function f(a) {}
             f.extra = 1;
             delete f.extra;
             var d = Object.getOwnPropertyDescriptor(f, 'length');
             Object.getOwnPropertyNames(f).join(',') === 'length,name,prototype' &&
             d.writable === false && d.enumerable === false"
        ),
        true
    );
    // 冻结/密封的状态与属性位由同一套实现给出（旧实现的 seal 只关了可扩展）。
    assert_eq!(
        eval_bool(
            "'use strict';
             function f(a) {}
             Object.freeze(f);
             var d = Object.getOwnPropertyDescriptor(f, 'length');
             Object.isFrozen(f) && d.configurable === false && d.writable === false"
        ),
        true
    );
}

#[test]
fn class_static_accessor_descriptor_survives_shape_storage() {
    // 类构造器也是 FunctionObject，`static get x()` 走 define_property 存 accessor。
    assert_eq!(
        eval_bool(
            "'use strict';
             class C { static get x() { return 1; } }
             var d = Object.getOwnPropertyDescriptor(C, 'x');
             typeof d.get === 'function' && d.enumerable === false &&
             d.configurable === true"
        ),
        true
    );
}

#[test]
fn strict_functions_reject_caller_and_arguments() {
    // ES 9.2.7：Function.prototype 上的 caller / arguments 是 %ThrowTypeError%
    // 访问器，严格模式函数读到就抛 TypeError；描述符不可枚举、不可配置。
    assert_eq!(
        eval_bool(
            "'use strict';
             function f() {}
             var threw = 0;
             try { f.caller; } catch (e) { if (e instanceof TypeError) threw++; }
             try { f.arguments; } catch (e) { if (e instanceof TypeError) threw++; }
             var d = Object.getOwnPropertyDescriptor(Function.prototype, 'caller');
             threw === 2 && !!d && typeof d.get === 'function' &&
             typeof d.set === 'function' && !d.enumerable && !d.configurable"
        ),
        true
    );
}

/// `IsConstructor` 与 `new` 的边界（ES 7.3.20 / 9.1.13）。
///
/// 内建函数里只有构造器有 `[[Construct]]`：`new Math.max()`、`new (f.call)()`、
/// `new Symbol()` 都是 TypeError；`Reflect.construct` 的 newTarget 也按同一判据。
/// 以前 `is_constructor` 靠"名字里有没有点"猜，`Function.prototype.apply` 这种
/// 不带点的内建方法因此被当成构造器（每个内建目录的 `not-a-constructor.js`）。
#[test]
fn only_constructors_have_construct() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         if (name_of(function () { new Math.max(); }) !== 'TypeError') throw new Error('Math.max');
         if (name_of(function () { new (function () {}).call(); }) !== 'TypeError') throw new Error('call');
         if (name_of(function () { new Symbol(); }) !== 'TypeError') throw new Error('Symbol');
         if (name_of(function () { Reflect.construct(function () {}, [], Function.prototype.apply); }) !== 'TypeError') {
           throw new Error('newTarget');
         }
         // 构造器仍然可以 new。
         if (name_of(function () { new Array(2); }) !== 'none') throw new Error('Array');
         if (name_of(function () { new Map(); }) !== 'none') throw new Error('Map');
         if (name_of(function () { new (function () {})(); }) !== 'none') throw new Error('user function');
         // bound function 的可构造性跟 target 走，`instanceof` 也走 target。
         function F(a) { this.a = a; }
         var b = F.bind(null, 7);
         var x = new b();
         if (x.a !== 7) throw new Error('bound args: ' + x.a);
         if (!(x instanceof F) || !(x instanceof b)) throw new Error('bound instanceof');
         if (name_of(function () { new (Math.max.bind(null))(); }) !== 'TypeError') throw new Error('bound non-ctor');",
    )
    .unwrap();
}

/// `Function.prototype` **本身是函数对象**（ES 19.2.3）。
///
/// 它以前是普通 `ProtoObject`，于是 `typeof Function.prototype` 是 "object"、
/// `[] instanceof Function.prototype` 一律 "Right-hand side is not callable"。
#[test]
fn function_prototype_is_a_function_object() {
    eval_js(
        "if (typeof Function.prototype !== 'function') throw new Error('typeof');
         if (Function.prototype.name !== '') throw new Error('name: ' + Function.prototype.name);
         if (Function.prototype.length !== 0) throw new Error('length');
         if (Object.prototype.toString.call(Function.prototype) !== '[object Function]') throw new Error('tag');
         if (Function.prototype() !== undefined) throw new Error('call');
         if (Object.getPrototypeOf(Function.prototype) !== Object.prototype) throw new Error('proto');",
    )
    .unwrap();
}

/// `instanceof` 的三条规范细节（ES 12.9.4 / 7.3.19）。
#[test]
fn instanceof_follows_ordinary_has_instance() {
    eval_js(
        "function MyFunct() { return 0; }
         // 左侧是裸函数引用时也要先物化成对象（以前只有 RHS 物化）。
         if ((MyFunct instanceof Function) !== true) throw new Error('MyFunct instanceof Function');
         // 左侧不是对象 ⇒ false，且在 `Get(C, 'prototype')` **之前**。
         if ((0 instanceof Function.prototype) !== false) throw new Error('primitive lhs');
         // `prototype` 是 Get：getter 会跑，结果必须是对象。
         var target = {};
         Object.defineProperty(Function.prototype, 'prototype', {
           get: function () { return target; }
         });
         if ((Object.create(target) instanceof Function.prototype) !== true) {
           throw new Error('prototype getter');
         }",
    )
    .unwrap();
}

/// 内建函数对象（`NativeFunctionObject`）的自有属性走与 `{}` 完全相同的
/// `[[DefineOwnProperty]]` / `[[Set]]` 语义。
///
/// 它以前自带 `ObjectHeader` + `PropertyTable` 并手写 15 个 `JSObject` 方法，
/// 其中 `property_set` 无条件覆盖成默认数据描述符 —— `Array.isArray.length = 9`
/// 能把 arity 的只读位冲掉（在严格模式下静默失败，而不是抛 TypeError）。
/// 现在与 `OrdinaryObject` 共用一套实现（`length` / `name` 走字典模式安装）。
#[test]
fn builtin_function_own_properties_keep_their_attributes() {
    eval_js(
        "'use strict';
         var d = Object.getOwnPropertyDescriptor(Array.isArray, 'length');
         if (d.value !== 1 || d.writable !== false || d.enumerable !== false || d.configurable !== true) {
           throw new Error('descriptor: ' + JSON.stringify(d));
         }
         var threw = false;
         try { Array.isArray.length = 9; } catch (e) { threw = e.name === 'TypeError'; }
         if (!threw) throw new Error('assign to read-only length did not throw');
         if (Array.isArray.length !== 1) throw new Error('length changed');
         if (Object.getOwnPropertyNames(Array.isArray).join(',') !== 'length,name') {
           throw new Error('keys: ' + Object.getOwnPropertyNames(Array.isArray).join(','));
         }
         var frozen = Object.freeze(Array.isArray);
         if (Object.isFrozen(frozen) !== true) throw new Error('isFrozen');
         if (Object.isSealed(frozen) !== true) throw new Error('isSealed');",
    )
    .unwrap();
}
