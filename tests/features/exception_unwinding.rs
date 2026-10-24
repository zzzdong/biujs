//! Feature tests for exception unwinding across JS frames (SEH).
//!
//! A `try` must see errors raised anywhere below it in the call stack —
//! including inside closures passed as arguments, methods, three-deep call
//! chains and native callbacks — and execution must resume in the *handler's*
//! frame afterwards.

use crate::helpers::{eval_number, eval_string};

#[test]
fn caller_catches_error_from_a_closure_argument() {
    assert_eq!(
        eval_string(
            "function f(g) { try { g(); return 'nothrow'; } catch (e) { return 'caught'; } }
             f(function () { Symbol.keyFor({}); });"
        ),
        "caught"
    );
    assert_eq!(
        eval_string(
            "function f(g) { try { g({}); return 'nothrow'; } catch (e) { return 'caught'; } }
             f(function (x) { Symbol.keyFor(x); });"
        ),
        "caught"
    );
}

#[test]
fn caller_catches_error_from_method_and_deep_chain() {
    assert_eq!(
        eval_string(
            "var o = { m: function () { Symbol.keyFor({}); } };
             function f() { try { o.m(); return 'nothrow'; } catch (e) { return 'caught'; } }
             f();"
        ),
        "caught"
    );
    assert_eq!(
        eval_string(
            "function c() { Symbol.keyFor({}); }
             function b() { return c(); }
             function a() { try { return b(); } catch (e) { return 'caught'; } }
             a();"
        ),
        "caught"
    );
}

#[test]
fn native_callback_error_aborts_the_native_and_is_caught_outside() {
    // `Array.prototype.forEach` drives the callback from Rust: the error has to
    // abandon the loop and surface at the `try` around the whole call.
    assert_eq!(
        eval_string(
            "var a = [1, 2, 3];
             var seen = 0;
             var r = 'nothrow';
             try {
               a.forEach(function () { seen += 1; Symbol.keyFor({}); });
             } catch (e) { r = 'caught'; }
             r + ',' + seen"
        ),
        "caught,1"
    );
}

#[test]
fn execution_continues_in_the_handlers_frame() {
    assert_eq!(
        eval_string(
            "function g() { Symbol.keyFor({}); }
             function f() { var r = 'start'; try { g(); } catch (e) { r = 'caught'; } r += '-end'; return r; }
             f();"
        ),
        "caught-end"
    );
    // The innermost handler wins, and the outer one never fires.
    assert_eq!(
        eval_string(
            "function g() { try { Symbol.keyFor({}); return 'inner-nothrow'; } catch (e) { return 'inner-caught'; } }
             function f() { try { return g(); } catch (e) { return 'outer-caught'; } }
             f();"
        ),
        "inner-caught"
    );
}

#[test]
fn rethrow_propagates_to_an_outer_frame() {
    assert_eq!(
        eval_string(
            "function g() { Symbol.keyFor({}); }
             function f() { try { g(); } catch (e) { throw e; } }
             var r = 'nothrow';
             try { f(); } catch (e2) { r = 'caught-outer'; }
             r;"
        ),
        "caught-outer"
    );
}

#[test]
fn finally_of_an_outer_frame_runs_when_a_callee_throws() {
    // The finally block must run for an exception raised in a nested call.
    //
    // 2026-10-18 修正：本用例原来断言 `"true,1,3"`，即 finally 跑**两遍**
    // （log = `fcf`）—— 那是 SEH 分派顺序的 bug（`handle_throw` 先跑 finally
    // 再进 catch，catch 出口又跳进 finally）。修复后与 node 一致：catch 先跑、
    // finally 跑一次，log = `cf` ⇒ `"true,0,2"`。
    assert_eq!(
        eval_string(
            "function g() { throw 1; }
             var log = '';
             function f() { try { g(); } catch (e) { log += 'c'; } finally { log += 'f'; } }
             f();
             (log.indexOf('f') >= 0) + ',' + log.indexOf('c') + ',' + log.length"
        ),
        "true,0,2"
    );
}

// ── 2026-10-18 SEH 记账重构的回归批 ───────────────────────────────────────
//
// 该轮重构（`handle_throw` 改 catch 优先 + `EndTry` 保留记录 + `ResumeExc`
// 按区域身份认领 + `DelayedJump`/`ResumeExc` 绝对 pc 平移 + 延迟返回值寄存）
// 修掉了一整族问题。以下用例把每个形状钉死，全部与 node 的行为对齐。

#[test]
fn finally_runs_exactly_once_when_catch_handles() {
    // `try{throw}catch{}finally{}`：catch 先跑、finally 只跑一次。
    // 修复前 finally 跑两遍（`f,c,f`）且顺序错（f 在 c 前）。
    assert_eq!(
        eval_string(
            "var log = [];
             try { throw 1; } catch (e) { log.push('c'); } finally { log.push('f'); }
             log.join(',')"
        ),
        "c,f"
    );
}

#[test]
fn finally_after_catch_that_throws_runs_once_then_propagates() {
    // catch 里再抛：finally 跑一次，异常继续向外。
    assert_eq!(
        eval_string(
            "var log = [];
             function f() {
               try { throw 1; } catch (e) { log.push('c'); throw 2; } finally { log.push('f'); }
             }
             try { f(); } catch (e) { log.push('outer:' + e); }
             log.join(',')"
        ),
        "c,f,outer:2"
    );
}

#[test]
fn inner_finally_normal_completion_does_not_skip_outer_finally_rest() {
    // 外层 finally 因异常进入，内层 try-finally **正常完成**：外层 finally 的
    // 剩余代码必须接着跑。修复前 `ResumeExc` 靠"栈顶就是我的"推断，在这里
    // 直接跳到外层 catch，`after-inner` 被吞。
    assert_eq!(
        eval_string(
            "var log = [];
             try {
               try { throw 0; } finally {
                 try { } finally { log.push('inner-f'); }
                 log.push('after-inner');
               }
             } catch (e) { log.push('outer-catch:' + e); }
             log.join(',')"
        ),
        "inner-f,after-inner,outer-catch:0"
    );
}

#[test]
fn return_value_survives_a_finally_that_calls() {
    // 返回值住在 `Rv`，finally 体里的调用会覆写它 —— 必须寄存。
    // 修复前 `try { return x } finally { f() }` 返回 `undefined`。
    assert_eq!(
        eval_string(
            "function f() { try { return 'r'; } finally { console.log('side'); } }
             f()"
        ),
        "r"
    );
    // 同一形状穿过一次嵌套调用（调用方的 finally 不能替被调用方的 return 跑）。
    assert_eq!(
        eval_number(
            "function inner() { return 1; }
             function outer() { try { return inner(); } finally { console.log('side'); } }
             outer()"
        ),
        1.0
    );
}

#[test]
fn break_and_continue_through_try_finally_in_a_called_function() {
    // `DelayedJump` 的绝对 pc 目标必须平移到模块坐标系：非首个函数里
    // break/continue 穿过 try-finally，修复前跳进别人的指令流（静默跑偏，
    // 甚至 `SubC rsp` 下溢 panic）。
    assert_eq!(
        eval_string(
            "function pad() { return 1; }
             function f() {
               var out = [];
               for (var i = 0; i < 3; i++) {
                 try { out.push(i); if (i === 1) break; } finally { out.push('f'); }
               }
               return out.join(',');
             }
             pad(); f()"
        ),
        "0,f,1,f"
    );
    assert_eq!(
        eval_string(
            "function pad() { return 1; }
             function f() {
               var out = [];
               for (var i = 0; i < 3; i++) {
                 try { if (i === 1) continue; out.push(i); } finally { out.push('f'); }
               }
               return out.join(',');
             }
             pad(); f()"
        ),
        "0,f,f,2,f"
    );
}

#[test]
fn break_inside_finally_itself() {
    // break 发生在 finally 体里：finally 已在运行，剩下的 finally 代码被跳过，
    // 直接跳出循环。修复前 lowering 的作用域记账在 finally 体期间漏掉了本区域，
    // 记录会残留在 `seh_stack` 上。
    assert_eq!(
        eval_string(
            "var log = [];
             function f() {
               for (var i = 0; i < 3; i++) {
                 try { log.push('t'); } finally { log.push('f'); break; log.push('never'); }
               }
               return log.join(',');
             }
             f() + '|' + (function () {
               var out = [];
               outer: for (var i = 0; i < 2; i++) {
                 try { out.push('a'); } finally { for (var j = 0; j < 2; j++) { break outer; } }
               }
               return out.join(',');
             })()"
        ),
        "t,f|a"
    );
}

#[test]
fn recursive_finally_with_exception_in_flight() {
    // 递归 + 异常在飞的 finally：内层的 finally 正常收尾，外层的异常在
    // finally 体走完后才重抛。身份认领（而不是"栈顶"）在这里是关键。
    assert_eq!(
        eval_string(
            "function f(n) {
               try { throw 'a' + n; } finally {
                 if (n > 0) { f(n - 1); }
                 log.push('rest' + n);
               }
             }
             var log = [];
             try { f(1); } catch (e) { log.push('caught:' + e); }
             log.join(',')"
        ),
        "rest0,caught:a0"
    );
}
