//! 已确认、但**尚未修**的语义 bug —— 每条都记着 node 的行为与我们的实际行为。
//!
//! 这些用例断言的是**正确**的（node 的）行为，因此现在全部 `#[ignore]`：
//! 套件保持绿，同时把"正确语义是什么"钉死。修掉一条就把那条上的 `ignore` 去掉，
//! 它就会变成一条普通的回归测试。
//!
//! 为什么先钉测试再动结构：见 `docs/architecture-redesign.md` §7/§8。重写一旦开始，
//! 连"它原来是什么行为"都会失去参照 —— 这三条就是那三个参照。
//!
//! 每条都标注了：现象 / node / 我们 / 已知的分析位置。

use crate::helpers::{eval_bool, eval_string};

/// #1 生成器闭包槽位错位。
///
/// ```js
/// function mk() { var x = 7; return function* () { yield x; }; }
/// mk()().next().value
/// ```
/// - node：`7`
/// - 我们：`function() { [native code] }`（`it` 本身是对的，只是 `value` 拿到了错槽位里的东西）
///
///   注意读出来的是**原生函数**（不是随便一个函数）—— 这条线索限定了根因的范围：
///   闭包槽位读到的是某个装着原生函数的槽，而不是"某个随机的值"。
///
/// 注意它**不经过任何方法调用** —— 最早以为是 `CallMethod` 的生成器分支传
/// `Vec::new()` 丢了闭包变量，那是读代码得出的错判（`interpreter-refactor.md` §3.3.6
/// 已订正）。真根因在闭包槽位本身：`create_generator` 之后（或生成器恢复时）的作用域装配。
#[test]
#[ignore = "已知 bug #1：生成器闭包槽位错位（architecture-redesign.md §8）"]
fn generator_sees_the_variables_it_closes_over() {
    // 变体 1：生成器函数 + 外层变量（不经任何方法调用）
    assert_eq!(
        eval_string(
            "function mk() { var x = 7; return function* () { yield x; }; }
             String(mk()().next().value)"
        ),
        "7"
    );
    // 变体 2：对象字面量上的生成器方法 + 外层变量
    assert_eq!(
        eval_string(
            "function outer() { var x = 42; return { *g() { yield x; } }; }
             String(outer().g().next().value)"
        ),
        "42"
    );
    // 变体 3：先取出再调用（走另一条 opcode 路径）
    assert_eq!(
        eval_string(
            "function outer2() { var y = 7; return { *h() { yield y; }; }; }
             var g = outer2().h;
             String(g().next().value)"
        ),
        "7"
    );
    // 不捕获变量时两条路径都对 —— 这一条现在是绿的，用来证明"丢不丢"只在有闭包时才可见
    assert_eq!(
        eval_string("var o = { *g() { yield 1; } }; String(o.g().next().value)"),
        "1"
    );
}

/// #2 `[[Construct]]` 帧把调用方的 `this` 冲掉了。
///
/// ```js
/// var o = { tag: 'outer', m: function () { var c = new C(); return this.tag; } };
/// o.m()
/// ```
/// - node：`outer`（`this` 在 `new` 之后仍然是调用方的）
/// - 我们：`inner`（调用方的 `this` 被新对象覆盖了）
///
/// 根因：`New` 的开帧顺序与其它调用路径**相反** —— 它先
/// `this_val = new_obj; function_val = new_target`，**再** `enter_frame`；而
/// `enter_frame` 会把 `self.this_val.clone()` 快照进 `this_stack`，于是这一帧的快照是
/// 新对象而不是调用方，`Ret` 的收尾（`vm/mod.rs` 里 `this_stack.pop()`）恢复出来的
/// `this` 就错了。登记见 `interpreter-refactor.md` §3.3.3。
#[test]
#[ignore = "已知 bug #2：new 之后调用方的 this 被冲掉（interpreter-refactor.md §3.3.3）"]
fn this_survives_a_new_expression_inside_a_method() {
    assert_eq!(
        eval_string(
            "function C() { this.tag = 'inner'; }
             var o = { tag: 'outer', m: function () { var c = new C(); return String(this.tag); } };
             o.m()"
        ),
        "outer"
    );
    // 同一件事的另一种写法：`this` 上没有那个属性，node 给 undefined，我们给 'inner'。
    assert_eq!(
        eval_string(
            "function C() { this.tag = 'inner'; }
             var p = { n: function () { new C(); return String(this.tag); } };
             p.n()"
        ),
        "undefined"
    );
    // 普通调用（非构造）不受影响 —— 这条现在是绿的，用来界定 bug 的范围。
    assert!(eval_bool(
        "function f() { return this === undefined; }
         var o = { m: function () { return f(); } };
         o.m()"
    ));
}

/// #3 调用一个不存在的方法**不抛**。
///
/// ```js
/// var q = {};
/// q.missing()
/// ```
/// - node：抛 `TypeError`
/// - 我们：返回 `undefined`（`CallMethod` 里 `Value::Undefined` 分支只写 `Rv`）
#[test]
#[ignore = "已知 bug #3：obj.missing() 应抛 TypeError（CallMethod 的 Undefined 分支）"]
fn calling_a_missing_method_throws_type_error() {
    assert_eq!(
        eval_string("var q = {}; try { q.missing(); 'no throw' } catch (e) { e.name }"),
        "TypeError"
    );
}

/// #4 内层 catch **实际执行过**之后，外层的 handler 就丢了。
///
/// ```js
/// function f() {
///   try {
///     try { throw 1; } catch (e) {}   // 这一次真的被内层抓住了
///     throw 2;                        // 这个却逃到了顶层
///   } catch (e2) { return 'caught:' + e2; }
/// }
/// f()
/// ```
/// - node：`caught:2`
/// - 我们：抛到顶层（`Runtime error: 2`）
///
/// **根因（2026-10-07 定位）**：`handle_throw` 在 **catch 路径**上
/// `self.state.seh_stack.pop()` 之后**没有把记录压回去**就跳进 `handler_pc`
/// （`vm/mod.rs` 的 `handle_throw`：`pop()` → `handler_pc != 0` 分支只写 `Rv` + `jump`）。
/// 而 `lower_try` 在 catch 末尾**还会发射一条 `EndTry`**（也 pop 一次）。
/// 于是这次 catch **多弹了一层**，把**外层**的记录吃掉了。
///
/// 为什么常见写法侥幸正确：当被抓住的区域就是最外层时，多出来的那次 pop 是空操作
/// （空栈 `pop()` 返回 `None`，被忽略）。所以它只在**嵌套**且**内层真的捕获过**时才现形。
///
/// 与 `lower_try` 里"catch 处不 pop SEH 记录"的注释直接矛盾 —— 那条注释描述的才是设计意图：
/// 记录应当活过整个 catch，由 catch 末尾的 `EndTry` 收尾（一次 push 对一次 pop）。
/// `SehRecord.catch_executed` 这个字段在当前行为下也几乎不会置位。
///
/// **归属**：这是运行期 `seh_stack` 记账的缺陷，`architecture-redesign.md` §4.6 的
/// M2（`Frame.try_stack` + completion 传递）按构造消除它（没有"什么时候 pop"这个问题，
/// 因为 handler 查找不再依赖一条栈）。在此之前它是已知偏差。
#[test]
#[ignore = "已知 bug #4：内层 catch 执行后外层 handler 丢失（handle_throw 在 catch 路径多弹一次）"]
fn an_executed_inner_catch_does_not_swallow_the_outer_handler() {
    // 最小复现：内层真的捕获过一次之后，外层 catch 必须还能接住后续的 throw。
    assert_eq!(
        eval_string(
            "function f() {
               try {
                 try { throw 1; } catch (e) { 'inner'; }
                 throw 2;
               } catch (e2) { return 'caught:' + e2; }
             }
             f()"
        ),
        "caught:2"
    );
    // 同一件事在有 finally 的形状下（内层 catch + finally，外层 catch）：
    assert_eq!(
        eval_string(
            "function g() {
               var log = [];
               try {
                 try { throw 'a'; } catch (e) { log.push('c'); }
                 throw 'b';
               } catch (e2) { log.push('outer'); }
               log.join(',')
             }
             g()"
        ),
        "c,outer"
    );
    // 内层 catch 里抛出的值要继续走外层（而不是被吞掉）：
    assert_eq!(
        eval_string(
            "function h() {
               try {
                 try { throw 'first'; } catch (e) { throw 'second'; }
               } catch (e2) { return 'caught:' + e2; }
             }
             h()"
        ),
        "caught:second"
    );
    // 界定范围：**没有嵌套**时一切正常（所以这条现在是绿的 —— 它证明问题只在嵌套下出现）。
    assert_eq!(
        eval_string(
            "function k() {
               try { throw 1; } catch (e) { return 'caught:' + e; }
             }
             k()"
        ),
        "caught:1"
    );
}
