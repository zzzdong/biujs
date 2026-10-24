//! 已确认、但**尚未修**的语义 bug —— 每条都记着 node 的行为与我们的实际行为。
//!
//! 这些用例断言的是**正确**的（node 的）行为。修掉一条就把那条上的 `ignore` 去掉，
//! 它就会变成一条普通的回归测试。
//!
//! **2026-10-18 更新**：#1–#4 全部修掉或不再复现，`ignore` 已全部解除：
//! #3（`obj.missing()` 抛 TypeError）与 #4（嵌套 catch）本轮修复；
//! #1 的测试本身有语法笔误（对象字面量里多了个 `;`，整个测试编译失败），
//! 笔误修掉后它直接通过 —— 真正的修复来自更早的"闭包共享可变单元"（捕获单元
//! 而非创建时快照）；#2 同样不再复现。四条留档作为回归测试。
//!
//! 为什么先钉测试再动结构：见 `docs/architecture-redesign.md` §7/§8。重写一旦开始，
//! 连"它原来是什么行为"都会失去参照 —— 这几条就是那几个参照。
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
/// - 我们（修复前）：`function() { [native code] }`
///
/// **修复**：来自"闭包共享可变单元"（捕获的是单元而不是创建时的快照）。
/// 注意本用例原来的变体 3 有语法笔误（`{ *h() { yield y; }; }` 里多余的 `;`），
/// 整个测试编译失败 ——  bug 是否还在，那条笔误一直挡着。已修。
#[test]
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
            "function outer2() { var y = 7; return { *h() { yield y; } }; }
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
/// - 我们（修复前）：`inner`（调用方的 `this` 被新对象覆盖了）
///
/// 历史根因：`New` 的开帧顺序与其它调用路径**相反**（先
/// `this_val = new_obj` 再 `enter_frame`，于是这一帧的快照是新对象）。
/// 现已不再复现（帧自带 `this` / `this_state` 之后该顺序不再能冲掉调用方），
/// 留档回归。
#[test]
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
/// - 我们（修复前）：返回 `undefined`（`CallMethod` 里 `Value::Undefined` 分支只写 `Rv`）
///
/// **修复（2026-10-18）**：`CallMethod` 的 `Value::Undefined` 分支改为抛
/// `TypeError("{method} is not a function")` —— 与 ES 的 CallMethod
/// （`IsCallable(func)` 为假即抛）一致，消息格式沿用本引擎原有的
/// "not a function" 写法。
#[test]
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
///
/// **修复（2026-10-18）**：`handle_throw` 改成 **catch 优先**分派（先 catch 再 finally，
/// 与规范顺序一致），分派进 catch 时把记录**push 回去**，由 catch 末尾的 `EndTry` 收尾；
/// 同时 `EndTry` 在有 finally 时改为"标记并保留"记录、由 finally 末尾按**区域身份**
/// （`ResumeExc` 的操作数）认领的 `ResumeExc` 弹出。同一轮修复还消除了附带发现的
/// finally 双重执行（`try{throw}catch{}finally{}` 的 finally 跑两遍）与
/// `DelayedJump` 绝对 pc 缺模块坐标系平移（非首函数里 break/continue 穿 try-finally
/// 静默跑偏）。
#[test]
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
    // 同一件事在有 finally 的形状下（内层 catch + finally，外层 catch）。
    // （尾表达式用 `return` 带回：函数完成值是另一个已知缺口，与 SEH 无关 ——
    // 普通函数尾表达式语句同样丢值，见 P2 的完成值语义任务。）
    assert_eq!(
        eval_string(
            "function g() {
               var log = [];
               try {
                 try { throw 'a'; } catch (e) { log.push('c'); }
                 throw 'b';
               } catch (e2) { log.push('outer'); }
               return log.join(',');
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
