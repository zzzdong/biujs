//! ES6 generators (`function*` / `yield`) — M4-G1 subset.
//!
//! The subset covers `next()`: the body starts on the first `next()`, suspends
//! at each `yield`, and the value sent back in becomes the value of the `yield`
//! expression. `yield*`, `return()` / `throw()` and a `try` around a `yield`
//! are deliberately still missing (see §2.1p of the conformance plan).

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn body_does_not_run_until_first_next() {
    assert_eq!(
        eval_string(
            "var started = false;
             function* g() { started = true; yield 1; }
             var it = g();
             var before = started;
             it.next();
             var after = started;
             before + ',' + after"
        ),
        "false,true"
    );
}

#[test]
fn yields_are_reported_one_at_a_time() {
    assert_eq!(
        eval_string(
            "function* g() { yield 'a'; yield 'b'; }
             var it = g();
             var out = [];
             var r = it.next();
             while (!r.done) { out.push(r.value); r = it.next(); }
             out.join('|') + '/' + r.done"
        ),
        "a|b/true"
    );
}

#[test]
fn return_value_terminates_with_done() {
    assert_eq!(
        eval_string(
            "function* g() { yield 1; return 'end'; }
             var it = g();
             var a = it.next();
             var b = it.next();
             var c = it.next();
             // `Array.join` renders `undefined` as the empty string.
             [a.value, a.done, b.value, b.done, typeof c.value, c.done].join(',')"
        ),
        "1,false,end,true,undefined,true"
    );
}

#[test]
fn sent_value_becomes_the_yield_expression() {
    assert_eq!(
        eval_string(
            "function* g() {
               var first = yield 'ready';
               var second = yield first;
               yield second;
             }
             var it = g();
             it.next();
             var a = it.next(10).value;
             var b = it.next(20).value;
             it.next().value;
             a + ',' + b"
        ),
        "10,20"
    );
    // A `next()` without an argument sends `undefined`.
    assert_eq!(
        eval_string(
            "function* g() { var v = yield 1; yield typeof v; }
             var it = g();
             it.next();
             it.next().value"
        ),
        "undefined"
    );
}

#[test]
fn bare_yield_yields_undefined() {
    assert_eq!(
        eval_string(
            "function* g() { yield; yield 1; }
             var it = g();
             var a = it.next();
             var b = it.next();
             [typeof a.value, a.done, b.value].join(',')"
        ),
        "undefined,false,1"
    );
}

#[test]
fn locals_and_arguments_survive_suspension() {
    // Both the parameter and the counter live in the frame slice that gets
    // copied out on suspension and back on resume.
    assert_eq!(
        eval_string(
            "function* count(from, step) {
               var i = from;
               while (i < from + step * 3) { yield i; i = i + step; }
             }
             var out = [];
             for (var v of count(2, 3)) { out.push(v); }
             out.join(',')"
        ),
        "2,5,8"
    );
}

#[test]
fn generators_are_iterable() {
    assert!(eval_bool(
        "function* g() { yield 1; }
         var it = g();
         it[Symbol.iterator]() === it"
    ));
    assert_eq!(
        eval_string(
            "function* g() { yield 'x'; yield 'y'; }
             var s = '';
             for (var c of g()) { s += c; }
             s"
        ),
        "xy"
    );
}

#[test]
fn instances_have_independent_state() {
    assert_eq!(
        eval_string(
            "function* g() { yield 1; yield 2; }
             var a = g();
             var b = g();
             a.next();
             var out = [a.next().value, b.next().value, b.next().value];
             out.join(',')"
        ),
        "2,1,2"
    );
}

#[test]
fn generator_can_call_another_function() {
    // A nested ordinary call inside a generator body must not disturb the
    // suspended frame.
    assert_eq!(
        eval_number(
            "function twice(n) { return n * 2; }
             function* g() { yield twice(1); yield twice(2); }
             var it = g();
             it.next().value + it.next().value"
        ),
        6.0
    );
}

#[test]
fn yield_delegates_to_an_iterable() {
    assert_eq!(
        eval_string(
            "function* g() { yield* [1, 2, 3]; }
             var s = '';
             for (var v of g()) { s += v; }
             s"
        ),
        "123"
    );
    assert_eq!(
        eval_string(
            "function* inner() { yield 'a'; yield 'b'; }
             function* outer() { yield* inner(); yield 'c'; }
             var s = '';
             for (var v of outer()) { s += v; }
             s"
        ),
        "abc"
    );
    assert_eq!(
        eval_string("function* g() { yield* 'xy'; } var s = ''; for (var v of g()) { s += v; } s"),
        "xy"
    );
}

#[test]
fn yield_delegate_value_is_the_inner_return() {
    // `yield* inner()` evaluates to the value the delegate returned, and that
    // value travels through the iterator result even though `done` is true.
    assert_eq!(
        eval_string(
            "function* inner() { return 'R'; }
             function* outer() { var r = yield* inner(); yield r; }
             var it = outer();
             var a = it.next();
             var b = it.next();
             [a.value, a.done, b.done].join('|')"
        ),
        "R|false|true"
    );
    // An exhausted array iterator returns `undefined`.
    assert_eq!(
        eval_string(
            "function* g() { var r = yield* [1]; yield typeof r; }
             var it = g();
             it.next();
             it.next().value"
        ),
        "undefined"
    );
}

#[test]
fn sent_values_reach_the_delegate() {
    assert_eq!(
        eval_string(
            "function* inner() {
               var v = yield 'ready';
               yield 'got:' + v;
             }
             function* outer() { yield* inner(); }
             var it = outer();
             it.next();
             it.next(7).value"
        ),
        "got:7"
    );
}

#[test]
fn generator_instances_inherit_from_the_function_prototype() {
    assert!(eval_bool(
        "function* g() {}
         Object.getPrototypeOf(g()) === g.prototype"
    ));
    assert!(eval_bool("function* g() {} g() instanceof g"));
    // Methods hung off `g.prototype` are visible on every instance.
    assert_eq!(
        eval_string(
            "function* g() { yield 1; }
             g.prototype.tag = 'mine';
             g().tag"
        ),
        "mine"
    );
}

#[test]
fn generator_functions_are_not_constructors() {
    assert_eq!(
        eval_string(
            "function* g() {}
             var n = 'none';
             try { new g(); } catch (e) { n = e.name; }
             n"
        ),
        "TypeError"
    );
}

#[test]
fn iterator_completion_value_survives_done() {
    // Not generator-specific, but `yield*` depends on it: the `value` of a
    // finished iterator result is the value its body returned.
    assert_eq!(
        eval_string(
            "function* inner() { yield 1; return 'end'; }
             var it = inner();
             var r = it.next();
             var last = it.next();
             [r.value, last.value, last.done].join('|')"
        ),
        "1|end|true"
    );
}

#[test]
fn return_completes_the_generator_with_the_given_value() {
    assert_eq!(
        eval_string(
            "function* g() { yield 1; yield 2; }
             var it = g();
             it.next();
             var r = it.return(9);
             [r.value, r.done, it.next().done].join('|')"
        ),
        "9|true|true"
    );
    // `return()` on a generator that has not started yet still completes it.
    assert_eq!(
        eval_string(
            "function* g() { yield 1; }
             var it = g();
             var r = it.return('early');
             [r.value, r.done].join('|')"
        ),
        "early|true"
    );
}

#[test]
fn throw_completes_the_generator_and_reaches_the_caller() {
    assert_eq!(
        eval_string(
            "function* g() { yield 1; }
             var it = g();
             var seen = 'none';
             try { it.throw('boom'); } catch (e) { seen = e; }
             seen + '/' + it.next().done"
        ),
        "boom/true"
    );
}

#[test]
fn yield_delegate_is_closed_on_an_abrupt_completion() {
    // ES 14.4.14: `g.return(v)` closes the iterator `yield*` opened, passing
    // `v` to its `return` method with the iterator as the receiver.
    assert_eq!(
        eval_string(
            "var calls = 0, args, receiver;
             var spy = {
               next: function() { return { done: false }; },
               return: function() {
                 calls += 1; args = arguments; receiver = this;
                 return { done: true };
               }
             };
             var iterable = {};
             iterable[Symbol.iterator] = function() { return spy; };
             function* g() { yield* iterable; }
             var it = g();
             it.next();
             it.return(7777);
             [calls, args.length, args[0], receiver === spy].join('|')"
        ),
        "1|1|7777|true"
    );
}

#[test]
fn breaking_out_of_for_of_closes_the_generator() {
    assert_eq!(
        eval_string(
            "var closed = 0;
             function* g() {
               try { yield 1; yield 2; } finally { closed += 1; }
             }
             var seen = '';
             for (var v of g()) { seen += v; break; }
             seen + '/' + typeof closed"
        ),
        "1/number"
    );
    assert_eq!(
        eval_string(
            "function* g() { yield 1; yield 2; yield 3; }
             var seen = '';
             for (var v of g()) { seen += v; if (v === 2) break; }
             seen"
        ),
        "12"
    );
}
