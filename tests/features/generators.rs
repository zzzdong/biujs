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

#[test]
fn throw_is_forwarded_to_the_delegate() {
    // ES 14.4.14 step 5.b: while parked on a `yield*`, `throw(e)` calls the
    // delegate's own `throw` with `e`, and the delegate is the receiver.
    assert_eq!(
        eval_string(
            "var calls = 0, args, receiver;
             var spy = {
               next: function() { return { done: false }; },
               throw: function() {
                 calls += 1; args = arguments; receiver = this;
                 return { done: true };
               }
             };
             var iterable = {};
             iterable[Symbol.iterator] = function() { return spy; };
             function* g() { yield* iterable; }
             var it = g();
             it.next();
             it.throw(7777);
             [calls, args.length, args[0], receiver === spy].join('|')"
        ),
        "1|1|7777|true"
    );
    // A delegate that answers `{ done: false, value: v }` yields again, and the
    // generator stays suspended rather than completing.
    assert_eq!(
        eval_string(
            "var spy = {
               next: function() { return { done: false }; },
               throw: function() { return { done: false, value: 2222 }; }
             };
             var iterable = {};
             iterable[Symbol.iterator] = function() { return spy; };
             function* g() { yield* iterable; }
             var it = g();
             it.next();
             var r = it.throw(1);
             [r.value, r.done].join('|')"
        ),
        "2222|false"
    );
}

#[test]
fn a_try_finally_around_a_yield_survives_suspension() {
    // The frame's live exception handlers travel with it, so a `finally` that
    // has not run yet still runs when the body finishes.
    assert_eq!(
        eval_string(
            "var log = [];
             function* g() { try { yield 1; yield 2; } finally { log.push('fin'); } }
             var it = g();
             it.next(); it.next(); it.next();
             log.join(',')"
        ),
        "fin"
    );
    // A `catch` around the yield is restored too, and is not entered by a
    // plain `next()`.
    assert_eq!(
        eval_string(
            "var caught = 'none';
             function* g() { try { yield 1; } catch (e) { caught = e; } yield 2; }
             var it = g();
             var a = it.next();
             var b = it.next();
             var c = it.next();
             [a.value, b.value, c.done, caught].join('|')"
        ),
        "1|2|true|none"
    );
}

#[test]
fn return_resumes_the_body_so_finally_runs() {
    // ES 25.4.3.4: the return completion is delivered to the suspended body, so
    // a `finally` around the `yield` runs on the way out.
    assert_eq!(
        eval_string(
            "var log = [];
             function* g() { try { yield 1; yield 2; } finally { log.push('fin'); } }
             var it = g();
             it.next();
             var r = it.return(7);
             [r.value, r.done, log.join(',')].join('|')"
        ),
        "7|true|fin"
    );
    // The body is not resumed past the suspension point.
    assert!(!eval_bool(
        "var reached = false;
         function* g() { try { yield 1; } finally { } reached = true; }
         var it = g();
         it.next();
         it.return(1);
         reached"
    ));
}

#[test]
fn throw_is_delivered_to_the_generator_body() {
    // A `try` inside the generator catches what `throw()` delivers at the
    // `yield`, and the generator keeps going.
    assert_eq!(
        eval_string(
            "var caught = 'none';
             function* g() { try { yield 1; } catch (e) { caught = e; } yield 2; }
             var it = g();
             it.next();
             var r = it.throw('boom');
             [r.value, r.done, caught].join('|')"
        ),
        "2|false|boom"
    );
    // Uncaught: the `finally` still runs and the exception reaches the caller.
    assert_eq!(
        eval_string(
            "var log = [];
             function* g() { try { yield 1; } finally { log.push('fin'); } }
             var it = g();
             it.next();
             var seen = 'none';
             try { it.throw('boom'); } catch (e) { seen = e; }
             [seen, log.join(',')].join('|')"
        ),
        "boom|fin"
    );
    // A generator that never started has no body to run.
    assert_eq!(
        eval_string(
            "var started = false;
             function* g() { started = true; yield 1; }
             var it = g();
             var seen = 'none';
             try { it.throw('early'); } catch (e) { seen = e; }
             [seen, started].join('|')"
        ),
        "early|false"
    );
}

#[test]
fn a_delegate_close_error_reaches_the_generator_body() {
    // ES 14.4.14 step 5.c: `IteratorClose` happens *inside* the generator's
    // resumption, so an iterator whose `return` throws is caught by the body.
    assert_eq!(
        eval_string(
            "var caught = 'none';
             var thrown = new Error('from return');
             var iterable = {};
             iterable[Symbol.iterator] = function() {
               return {
                 next: function() { return { done: false }; },
                 return: function() { throw thrown; }
               };
             };
             function* g() { try { yield* iterable; } catch (e) { caught = e; } }
             var it = g();
             it.next();
             var r = it.return(5);
             [caught === thrown, r.done].join('|')"
        ),
        "true|true"
    );
}

// ============================================================
// Parameters bind at call time (ES 9.2.12)
// ============================================================
//
// A generator binds its parameters when it is *called*, not on the first
// `next()`: `function* f([[x]]) {}` must throw `TypeError` out of `f([null])`,
// before the caller ever sees the generator object. The VM now builds the frame
// eagerly and parks it at an `Opcode::PrologueEnd` barrier, which is also why
// the *body* still must not run until `next()`.

#[test]
fn generator_parameters_bind_when_the_generator_is_called() {
    // Destructuring a non-object, a non-iterable argument and a throwing default
    // all complete abruptly at call time.
    assert_eq!(
        eval_string(
            "function* f([[x]]) {}
             try { f([null]); 'no-throw'; } catch (e) { e.name; }"
        ),
        "TypeError"
    );
    assert_eq!(
        eval_string(
            "function* f({} = null) {}
             try { f(); 'no-throw'; } catch (e) { e.name; }"
        ),
        "TypeError"
    );
    assert_eq!(
        eval_string(
            "function* f([x]) {}
             try { f({}); 'no-throw'; } catch (e) { e.name; }"
        ),
        "TypeError"
    );
    assert_eq!(
        eval_string(
            "function* f(a = (function () { throw new Error('boom'); })()) {}
             try { f(); 'no-throw'; } catch (e) { e.message; }"
        ),
        "boom"
    );
}

#[test]
fn a_called_generator_still_has_not_started_its_body() {
    // The eager part is the *parameter* prologue only: the body waits for the
    // first `next()`. (A log array is used rather than a counter variable,
    // because captured scalars are still snapshot-on-capture in this engine.)
    assert_eq!(
        eval_string(
            "var log = [];
             function* g(a) { log.push('body:' + a); yield 1; log.push('body2'); yield 2; }
             var it = g(7);
             var before = log.length;
             var y1 = it.next().value;
             var mid = log.length;
             var y2 = it.next().value;
             [before, mid, log.length, y1, y2].join(',')"
        ),
        "0,1,2,1,2"
    );
}

#[test]
fn a_parked_generator_can_be_returned_before_it_starts() {
    // `return()` on a generator that was called but never resumed completes it
    // without running the body — and without leaking the parked frame.
    assert_eq!(
        eval_string(
            "var log = [];
             function* g(a) { log.push('body'); yield 1; }
             var it = g(1);
             var r = it.return('early');
             [r.value, r.done, it.next().done, log.length].join(',')"
        ),
        "early,true,true,0"
    );
    // Same for `throw()`.
    assert_eq!(
        eval_string(
            "var log = [];
             function* g(a) { log.push('body'); yield 1; }
             var it = g(1);
             try { it.throw(new Error('nope')); 'no-throw'; } catch (e) { e.message; }"
        ),
        "nope"
    );
}

#[test]
fn bound_parameters_are_visible_to_the_body() {
    // The parked frame carries the bound parameters, so the body sees them —
    // including a rest array and a destructured pattern.
    assert_eq!(
        eval_string(
            "function* g(x, ...rest) { yield x; yield rest.length; yield rest.join('-'); }
             var it = g(1, 'a', 'b');
             [it.next().value, it.next().value, it.next().value].join(',')"
        ),
        "1,2,a-b"
    );
    assert_eq!(
        eval_string(
            "function* g([a, b = 9]) { yield a; yield b; }
             var it = g([1]);
             [it.next().value, it.next().value].join(',')"
        ),
        "1,9"
    );
}

#[test]
fn a_generator_reached_as_a_method_still_builds_a_generator() {
    // Calling a generator *method* only builds the generator object; the body
    // runs at the first `next()`. The method call path used to run it eagerly,
    // so the body's `ret` returned into the caller's frame.
    assert_eq!(
        eval_number("class C { *m() { yield 1; } } new C().m().next().value"),
        1.0
    );
    assert_eq!(
        eval_number("({ *m() { yield 2; } }).m().next().value"),
        2.0
    );
    assert_eq!(
        eval_number("class C { static *s() { yield 3; } } C.s().next().value"),
        3.0
    );
    assert_eq!(
        eval_string("class C { *m() { yield 1; yield 2; } } [...new C().m()].join(',')"),
        "1,2"
    );
    // Destructured parameters of a generator method bind at call time.
    assert_eq!(
        eval_number("class C { *m([a]) { yield a; } } new C().m([9]).next().value"),
        9.0
    );
}

#[test]
fn a_generator_that_threw_is_completed() {
    // A body that lets an exception escape is finished for good: further
    // `next()` calls answer `{ value: undefined, done: true }` instead of
    // resuming a dead frame (which used to abort the whole process).
    assert_eq!(
        eval_bool(
            "function* g() { throw new Error('x'); }
             var it = g();
             try { it.next(); } catch (e) {}
             it.next().done"
        ),
        true
    );
    assert_eq!(
        eval_bool(
            "function* g() { throw new Error('x'); }
             var it = g();
             try { it.next(); } catch (e) {}
             it.next().value === undefined"
        ),
        true
    );
    assert_eq!(
        eval_bool("function* g() { yield 1; } var it = g(); it.next(); it.next().done"),
        true
    );
}

#[test]
fn a_generator_in_its_boxed_spelling_only_builds_the_object() {
    // Calling a generator function expression materialized by `MakeFuncObj` — or
    // a generator method taken off its object — must only build the generator,
    // exactly like the bare `Value::Function` spelling does.
    assert_eq!(
        eval_number("var it = (function* () { yield 1; })(); it.next().value"),
        1.0
    );
    assert_eq!(
        eval_number("var f = function* () { yield 2; }; f().next().value"),
        2.0
    );
    assert_eq!(
        eval_number(
            "class C { *m() { yield 3; } }
             var g = C.prototype.m;
             g.call(new C()).next().value"
        ),
        3.0
    );
}

#[test]
fn a_generator_prologue_inside_a_parameter_default_is_re_entrant() {
    // Creating a generator runs only its *parameter* prologue. A default that
    // itself creates a generator (`[[,] = g()]`) used to clobber the
    // "in a prologue" flag on the way out, so the outer method's barrier was
    // ignored: the body ran during creation and again at the first `next()`.
    assert_eq!(
        eval_number(
            "var cc = 0;
             function* g() { yield; }
             var o = { *m([[,] = g()]) { cc = cc + 1; } };
             var it = o.m([]);
             cc"
        ),
        0.0
    );
    assert_eq!(
        eval_number(
            "var cc = 0;
             function* g() { yield; }
             var o = { *m([[,] = g()]) { cc = cc + 1; } };
             o.m([]).next();
             cc"
        ),
        1.0
    );
    // The default's generator is advanced by exactly one step and left open.
    assert_eq!(
        eval_string(
            "var first = 0, second = 0;
             function* g() { first += 1; yield; second += 1; }
             var C = class { static *m([[,] = g()]) {} };
             C.m([]).next();
             first + '/' + second"
        ),
        "1/0"
    );
    // The same through an instance method.
    assert_eq!(
        eval_number(
            "var cc = 0;
             function* g() { yield; }
             class C { *m([[,] = g()]) { cc = cc + 1; } }
             new C().m([]).next();
             cc"
        ),
        1.0
    );
}
