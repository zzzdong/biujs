//! 闭包捕获：**共享单元**（cell）而非创建时的快照。
//!
//! 一个被嵌套函数捕获的绑定不再住在寄存器里，而是住在一个共享单元里
//! （`MakeCell` 在序言里建，读写走 `LoadEnv`/`StoreEnv`），所以两边看到的是
//! 同一个绑定 —— `counter()` 会真的往上数。
//!
//! 反面也一并锁住：块级绑定（`for (let …)` 的循环头）**保留**按值捕获，
//! 那正是"每次迭代一个值"的语义；被捕获的 `let` 仍有动态死区。

use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};

// ── 共享单元：写是可见的 ──────────────────────────────────

#[test]
fn a_captured_binding_counts() {
    // 最经典的形态：此前每个闭包各看一份创建时的快照，返回 1,1,1。
    assert_eq!(
        eval_string(
            r#"
            function mk() { var c = 0; return function () { return ++c; }; }
            var f = mk();
            f() + ',' + f() + ',' + f()
            "#
        ),
        "1,2,3"
    );
}

#[test]
fn two_closures_share_one_binding() {
    assert_eq!(
        eval_number(
            r#"
            function mk() { var c = 0; return [function () { return c++; }, function () { return c; }]; }
            var p = mk();
            p[0](); p[0](); p[0]();
            p[1]()
            "#
        ),
        3.0
    );
}

#[test]
fn writes_survive_the_defining_frame() {
    // 外层返回之后写入仍然生效（此前闭包拿到的是 undefined 的副本）。
    assert_eq!(
        eval_number(
            r#"
            function mk() { var v = 0; return function () { v += 1; return v; }; }
            var f = mk();
            f();
            f()
            "#
        ),
        2.0
    );
}

#[test]
fn a_nested_declaration_writes_the_outer_binding() {
    // 函数声明是被提升的：它的闭包在创建时就绑定，写必须落到同一个单元。
    assert_eq!(
        eval_number(
            r#"
            function outer() {
                var n = 0;
                function inc() { n += 1; }
                inc(); inc(); inc();
                return n;
            }
            outer()
            "#
        ),
        3.0
    );
}

#[test]
fn an_arrow_shares_the_binding_too() {
    assert_eq!(
        eval_number(
            r#"
            function mk() { var c = 0; var inc = () => { c += 1; return c; }; return inc; }
            var f = mk();
            f(); f();
            f()
            "#
        ),
        3.0
    );
}

#[test]
fn a_captured_parameter_counts() {
    assert_eq!(
        eval_number(
            r#"
            function mk(start) { return () => start++; }
            var f = mk(10);
            f();
            f()
            "#
        ),
        11.0
    );
}

#[test]
fn an_accumulator_inside_a_callback_works() {
    // `reduce` 那类形态：累加器活在外层函数的局部里。
    assert_eq!(
        eval_number(
            r#"
            function total() {
                var acc = 0;
                [1, 2, 3, 4].forEach(function (x) { acc += x; });
                return acc;
            }
            total()
            "#
        ),
        10.0
    );
}

#[test]
fn a_destructured_var_is_shared() {
    assert_eq!(
        eval_number(
            r#"
            function mk() { var [c] = [0]; return function () { c += 1; return c; }; }
            var f = mk();
            f(); f();
            f()
            "#
        ),
        3.0
    );
}

#[test]
fn a_captured_arguments_object_is_shared() {
    // 内层函数有自己的 `arguments`（空的）；外层那个是通过捕获共享的。
    assert_eq!(
        eval_number(
            r#"
            function outer() {
                var args = arguments;
                return function () { return args.length + arguments.length; };
            }
            outer(1, 2, 3)()
            "#
        ),
        3.0
    );
}

// ── 用户迭代器：闭包的直接受益者 ──────────────────────────

#[test]
fn a_user_defined_iterator_advances() {
    // 此前 `next()` 里的自增写不进共享单元，迭代器永远停在第一个值，
    // `for-of` / `[...x]` 因此死循环（撞步数或墙钟护栏）。
    assert_eq!(
        eval_string(
            r#"
            var o = {};
            o[Symbol.iterator] = function () {
                var i = 0;
                return { next: function () { return i < 3 ? { value: i++, done: false } : { done: true }; } };
            };
            var out = '';
            for (var v of o) { out += v; }
            out + '|' + [...o].join(',')
            "#
        ),
        "012|0,1,2"
    );
}

#[test]
fn a_class_instance_iterator_advances() {
    assert_eq!(
        eval_number(
            r#"
            class Iter {
                [Symbol.iterator]() { var i = 0; return { next: () => i < 4 ? { value: i++, done: false } : { done: true } }; }
            }
            var sum = 0;
            for (var v of new Iter()) { sum += v; }
            sum
            "#
        ),
        6.0
    );
}

// ── 类的方法也捕获外层 ────────────────────────────────────

#[test]
fn a_method_sees_an_enclosing_local() {
    assert_eq!(
        eval_number(
            r#"
            function mk() {
                var n = 7;
                class C { m() { return n; } }
                return new C().m();
            }
            mk()
            "#
        ),
        7.0
    );
}

#[test]
fn a_method_sees_the_class_name() {
    // 函数体里的类：类名是函数作用域的绑定，方法要能读到它。
    assert_eq!(
        eval_bool(
            r#"
            function mk() {
                class C { m() { return typeof C; } }
                return new C().m();
            }
            mk() === 'function'
            "#
        ),
        true
    );
}

#[test]
fn a_generator_shares_an_outer_binding() {
    // 生成器的每次恢复都写同一个单元：两次 `yield n++` 给出 0 和 1。
    assert_eq!(
        eval_string(
            r#"
            function mk() {
                var n = 0;
                function* g() { yield n++; yield n++; }
                return g;
            }
            var it = mk()();
            it.next().value + ',' + it.next().value
            "#
        ),
        "0,1"
    );
}

// ── 反面：按值捕获与死区 ──────────────────────────────────

#[test]
fn a_loop_head_still_captures_per_iteration() {
    // 块级绑定保留按值捕获 —— 这正是 ES 要的"每次迭代一个值"。
    assert_eq!(
        eval_string(
            r#"
            var fns = [];
            for (let i = 0; i < 3; i++) { fns.push(function () { return i; }); }
            fns[0]() + ',' + fns[1]() + ',' + fns[2]()
            "#
        ),
        "0,1,2"
    );
}

#[test]
fn a_captured_let_keeps_its_dead_zone() {
    // 闭包在声明之前读到被捕获的 `let`：动态死区，必须是 ReferenceError。
    let err = eval_js(
        r#"
        function f() {
            function peek() { return x; }
            var r = peek();
            let x = 1;
            return r;
        }
        f()
        "#,
    )
    .unwrap_err();
    assert!(
        err.contains("before initialization"),
        "expected a TDZ ReferenceError, got {err}"
    );
}

#[test]
fn a_parameter_default_does_not_see_the_body() {
    // ES 9.2.10 step 27：参数作用域与函数体的 var 环境是分开的，
    // 参数默认值里的闭包看到的是外层的 `x`。
    assert_eq!(
        eval_string(
            r#"
            var x = 'outside';
            var probe;
            function g(_ = (probe = function () { return x; })) { var x = 'inside'; }
            g();
            probe()
            "#
        ),
        "outside"
    );
}

#[test]
fn a_captured_let_read_after_its_declaration_is_fine() {
    assert_eq!(
        eval_number(
            r#"
            function f() {
                let x = 1;
                function peek() { return x; }
                x = 2;
                return peek();
            }
            f()
            "#
        ),
        2.0
    );
}
