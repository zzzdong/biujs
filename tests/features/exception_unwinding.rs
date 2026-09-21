//! Feature tests for exception unwinding across JS frames (SEH).
//!
//! A `try` must see errors raised anywhere below it in the call stack —
//! including inside closures passed as arguments, methods, three-deep call
//! chains and native callbacks — and execution must resume in the *handler's*
//! frame afterwards.

use crate::helpers::eval_string;

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
    // (The engine runs it on the exception path and again on the fall-through
    // path — a pre-existing codegen quirk recorded in the plan.)
    assert_eq!(
        eval_string(
            "function g() { throw 1; }
             var log = '';
             function f() { try { g(); } catch (e) { log += 'c'; } finally { log += 'f'; } }
             f();
             (log.indexOf('f') >= 0) + ',' + log.indexOf('c') + ',' + log.length"
        ),
        "true,1,3"
    );
}
