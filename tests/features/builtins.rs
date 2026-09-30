use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};
use biujs::Value;

// ============================================================
// Built-in Objects
// ============================================================

// ──────────────────────────────
// Error
// ──────────────────────────────

#[test]
fn error_constructor_no_args() {
    let js = r#"
        let e = Error();
        e.name
    "#;
    assert_eq!(eval_string(js), "Error");
}

#[test]
fn error_constructor_with_message() {
    let js = r#"
        let e = Error("something went wrong");
        e.message
    "#;
    assert_eq!(eval_string(js), "something went wrong");
}

#[test]
fn error_constructor_name() {
    let js = r#"
        let e = Error("test");
        e.name
    "#;
    assert_eq!(eval_string(js), "Error");
}

#[test]
fn type_error_constructor() {
    let js = r#"
        let e = TypeError("bad type");
        e.name + ": " + e.message
    "#;
    assert_eq!(eval_string(js), "TypeError: bad type");
}

#[test]
fn reference_error_constructor() {
    let js = r#"
        let e = ReferenceError("not found");
        e.message
    "#;
    assert_eq!(eval_string(js), "not found");
}

#[test]
fn range_error_constructor() {
    let js = r#"
        let e = RangeError("out of range");
        e.name
    "#;
    assert_eq!(eval_string(js), "RangeError");
}

// ──────────────────────────────
// Boolean
// ──────────────────────────────

#[test]
fn boolean_constructor_true() {
    assert_eq!(eval_bool("Boolean(true)"), true);
    assert_eq!(eval_bool("Boolean(1)"), true);
    assert_eq!(eval_bool("Boolean('hello')"), true);
}

#[test]
fn boolean_constructor_false() {
    assert_eq!(eval_bool("Boolean(false)"), false);
    assert_eq!(eval_bool("Boolean(0)"), false);
    assert_eq!(eval_bool("Boolean('')"), false);
    assert_eq!(eval_bool("Boolean(undefined)"), false);
    assert_eq!(eval_bool("Boolean(null)"), false);
}

#[test]
fn boolean_constructor_no_args() {
    assert_eq!(eval_bool("Boolean()"), false);
}

// ──────────────────────────────
// Number
// ──────────────────────────────

#[test]
fn number_constructor_no_args() {
    assert_eq!(eval_number("Number()"), 0.0);
}

#[test]
fn number_constructor_value() {
    assert_eq!(eval_number("Number(42)"), 42.0);
    assert_eq!(eval_number("Number('3.14')"), 3.14);
    assert_eq!(eval_number("Number(true)"), 1.0);
    assert_eq!(eval_number("Number(false)"), 0.0);
}

#[test]
fn number_is_nan() {
    assert_eq!(eval_bool("Number.isNaN(NaN)"), true);
    assert_eq!(eval_bool("Number.isNaN(42)"), false);
    assert_eq!(eval_bool("Number.isNaN('hello')"), false);
}

#[test]
fn number_is_finite() {
    assert_eq!(eval_bool("Number.isFinite(42)"), true);
    assert_eq!(eval_bool("Number.isFinite(Infinity)"), false);
    assert_eq!(eval_bool("Number.isFinite(NaN)"), false);
}

#[test]
fn number_is_integer() {
    assert_eq!(eval_bool("Number.isInteger(42)"), true);
    assert_eq!(eval_bool("Number.isInteger(3.14)"), false);
    assert_eq!(eval_bool("Number.isInteger(NaN)"), false);
    assert_eq!(eval_bool("Number.isInteger(Infinity)"), false);
}

// ──────────────────────────────
// String
// ──────────────────────────────

#[test]
fn string_constructor_no_args() {
    assert_eq!(eval_string("String()"), "");
}

#[test]
fn string_constructor_value() {
    assert_eq!(eval_string("String(42)"), "42");
    assert_eq!(eval_string("String(true)"), "true");
    assert_eq!(eval_string("String('hello')"), "hello");
}

// ──────────────────────────────
// Object
// ──────────────────────────────

#[test]
fn object_constructor_no_args() {
    let js = r#"
        let o = Object();
        typeof o
    "#;
    assert_eq!(eval_string(js), "object");
}

#[test]
fn object_constructor_wraps_value() {
    // ES 19.1.1.1: `Object(value)` returns ToObject(value) — a wrapper object,
    // abstract-equal to the original primitive.
    let js = r#"
        let x = 42;
        let o = Object(x);
        typeof o + " " + (o == x) + " " + (o.constructor === Number)
    "#;
    assert_eq!(eval_string(js), "object true true");
}

/// Object.keys() on a simple object
#[test]
fn object_keys() {
    let js = r#"
        let o = {a: 1, b: 2, c: 3};
        Object.keys(o).toString()
    "#;
    assert_eq!(eval_string(js), "a,b,c");
}

/// Object.values() on a simple object
#[test]
fn object_values() {
    let js = r#"
        let o = {x: 10, y: 20};
        Object.values(o).toString()
    "#;
    assert_eq!(eval_string(js), "10,20");
}

// ──────────────────────────────
// Array constructor
// ──────────────────────────────

#[test]
fn array_constructor_no_args() {
    let js = r#"
        let a = Array();
        a.length
    "#;
    assert_eq!(eval_number(js), 0.0);
}

#[test]
fn array_constructor_with_length() {
    let js = r#"
        let a = Array(5);
        a.length
    "#;
    assert_eq!(eval_number(js), 5.0);
}

#[test]
fn array_constructor_with_values() {
    let js = r#"
        let a = Array(1, 2, 3);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3");
}

// ──────────────────────────────
// Number prototype methods
// ──────────────────────────────

#[test]
fn number_to_fixed() {
    assert_eq!(eval_string("(3.14159).toFixed()"), "3");
    assert_eq!(eval_string("(3.14159).toFixed(2)"), "3.14");
    assert_eq!(eval_string("(3.14159).toFixed(4)"), "3.1416");
    assert_eq!(eval_string("NaN.toFixed()"), "NaN");
    assert_eq!(eval_string("Infinity.toFixed()"), "Infinity");
}

#[test]
fn number_to_exponential() {
    assert_eq!(eval_string("(12345).toExponential()"), "1.2345e4");
    assert_eq!(eval_string("(12345).toExponential(2)"), "1.23e4");
    assert_eq!(eval_string("NaN.toExponential()"), "NaN");
}

#[test]
fn number_to_precision() {
    // Uses decimal places (not significant digits) in current implementation
    assert_eq!(eval_string("(123.456).toPrecision(4)"), "123.4560");
    assert_eq!(eval_string("(123.456).toPrecision(2)"), "123.46");
    assert_eq!(eval_string("NaN.toPrecision(1)"), "NaN");
}

// ──────────────────────────────
// String prototype methods
// ──────────────────────────────

#[test]
fn string_char_at() {
    assert_eq!(eval_string("'hello'.charAt(0)"), "h");
    assert_eq!(eval_string("'hello'.charAt(4)"), "o");
    assert_eq!(eval_string("'hello'.charAt(99)"), "");
    assert_eq!(eval_string("'hello'.charAt()"), "h");
}

#[test]
fn string_char_code_at() {
    assert_eq!(eval_number("'ABC'.charCodeAt(0)"), 65.0);
    assert_eq!(eval_number("'ABC'.charCodeAt(1)"), 66.0);
    assert!(eval_number("'ABC'.charCodeAt(99)").is_nan());
}

#[test]
fn string_concat() {
    assert_eq!(eval_string("'hello'.concat(' ', 'world')"), "hello world");
    assert_eq!(eval_string("'a'.concat('b')"), "ab");
    assert_eq!(eval_string("'abc'.concat()"), "abc");
}

#[test]
fn string_includes() {
    assert_eq!(eval_bool("'hello world'.includes('world')"), true);
    assert_eq!(eval_bool("'hello world'.includes('xyz')"), false);
    assert_eq!(eval_bool("'hello'.includes()"), false);
}

#[test]
fn string_index_of() {
    assert_eq!(eval_number("'hello'.indexOf('l')"), 2.0);
    assert_eq!(eval_number("'hello'.indexOf('x')"), -1.0);
    assert_eq!(eval_number("'abc'.indexOf()"), -1.0);
}

#[test]
fn string_slice() {
    assert_eq!(eval_string("'hello'.slice(1, 3)"), "el");
    assert_eq!(eval_string("'hello'.slice(2)"), "llo");
    assert_eq!(eval_string("'hello'.slice(-3)"), "llo");
    assert_eq!(eval_string("'hello'.slice(-3, -1)"), "ll");
}

#[test]
fn string_to_upper() {
    assert_eq!(eval_string("'hello'.toUpperCase()"), "HELLO");
    assert_eq!(eval_string("'ABC'.toUpperCase()"), "ABC");
    assert_eq!(eval_string("'123'.toUpperCase()"), "123");
}

#[test]
fn string_to_lower() {
    assert_eq!(eval_string("'HELLO'.toLowerCase()"), "hello");
    assert_eq!(eval_string("'abc'.toLowerCase()"), "abc");
}

#[test]
fn string_trim() {
    assert_eq!(eval_string("'  hello  '.trim()"), "hello");
    assert_eq!(eval_string("'hello'.trim()"), "hello");
    assert_eq!(eval_string("'  '.trim()"), "");
}

#[test]
fn string_split() {
    assert_eq!(eval_string("'a,b,c'.split(',').toString()"), "a,b,c");
    assert_eq!(eval_string("'abc'.split('').toString()"), "a,b,c");
    assert_eq!(eval_string("'hello'.split().toString()"), "hello");
}

#[test]
fn string_substring() {
    assert_eq!(eval_string("'hello'.substring(1, 3)"), "el");
    assert_eq!(eval_string("'hello'.substring(3, 1)"), "el");
    assert_eq!(eval_string("'hello'.substring(2)"), "llo");
}

// ──────────────────────────────
// Array prototype methods
// ──────────────────────────────

#[test]
fn array_push_returns_length() {
    let js = r#"
        let a = [1, 2, 3];
        a.push(4)
    "#;
    assert_eq!(eval_number(js), 4.0);
}

#[test]
fn array_push_mutates() {
    let js = r#"
        let a = [1, 2, 3];
        a.push(4);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3,4");
}

#[test]
fn array_pop_returns_element() {
    assert_eq!(eval_number("let a = [1, 2]; a.pop()"), 2.0);
}

#[test]
fn array_pop_mutates() {
    let js = r#"
        let a = [1, 2, 3];
        a.pop();
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2");
}

#[test]
fn array_pop_empty() {
    let result = eval_js("let a = []; a.pop()").unwrap();
    assert_eq!(result, Value::Undefined);
}

#[test]
fn array_shift() {
    let js = r#"
        let a = [1, 2, 3];
        a.shift()
    "#;
    assert_eq!(eval_number(js), 1.0);
}

#[test]
fn array_shift_mutates() {
    let js = r#"
        let a = [1, 2, 3];
        a.shift();
        a.toString()
    "#;
    assert_eq!(eval_string(js), "2,3");
}

#[test]
fn array_unshift() {
    let js = r#"
        let a = [2, 3];
        a.unshift(1)
    "#;
    assert_eq!(eval_number(js), 3.0);
}

#[test]
fn array_unshift_mutates() {
    let js = r#"
        let a = [2, 3];
        a.unshift(1);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3");
}

#[test]
fn array_index_of() {
    assert_eq!(eval_number("[1, 2, 3].indexOf(2)"), 1.0);
    assert_eq!(eval_number("[1, 2, 3].indexOf(99)"), -1.0);
    assert_eq!(eval_number("[].indexOf(1)"), -1.0);
}

#[test]
fn array_includes() {
    assert_eq!(eval_bool("[1, 2, 3].includes(2)"), true);
    assert_eq!(eval_bool("[1, 2, 3].includes(99)"), false);
    assert_eq!(eval_bool("[].includes(1)"), false);
}

#[test]
fn array_join() {
    assert_eq!(eval_string("[1, 2, 3].join()"), "1,2,3");
    assert_eq!(eval_string("[1, 2, 3].join(' - ')"), "1 - 2 - 3");
    assert_eq!(eval_string("[].join()"), "");
}

#[test]
fn array_slice() {
    assert_eq!(eval_string("[1, 2, 3, 4].slice(1, 3).toString()"), "2,3");
    assert_eq!(eval_string("[1, 2, 3].slice(1).toString()"), "2,3");
    assert_eq!(eval_string("[1, 2, 3].slice(-1).toString()"), "3");
}

#[test]
fn array_concat() {
    assert_eq!(eval_string("[1, 2].concat([3, 4]).toString()"), "1,2,3,4");
    assert_eq!(eval_string("[].concat([1]).toString()"), "1");
}

#[test]
fn array_splice_removes() {
    let js = r#"
        let a = [1, 2, 3, 4];
        a.splice(1, 2).toString()
    "#;
    assert_eq!(eval_string(js), "2,3");
}

#[test]
fn array_splice_mutates() {
    let js = r#"
        let a = [1, 2, 3, 4];
        a.splice(1, 2);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,4");
}

#[test]
fn array_splice_inserts() {
    let js = r#"
        let a = [1, 4];
        a.splice(1, 0, 2, 3);
        a.toString()
    "#;
    assert_eq!(eval_string(js), "1,2,3,4");
}

#[test]
fn everyday_surface_that_had_no_way_to_be_used() {
    // `console` is a host object, not ES — but without it the only way to
    // observe a value is to `throw` it, which is what made the engine unusable
    // for everyday scripts.
    assert_eq!(eval_string("typeof console"), "object");
    assert_eq!(eval_string("typeof console.log"), "function");
    assert_eq!(eval_string("typeof console.error"), "function");
    // Logging answers `undefined` and prints (stdout for log/info, stderr for
    // warn/error), so it composes with the rest of a script.
    assert_eq!(eval_string("String(console.log(1))"), "undefined");

    // `String.prototype.replace` / `replaceAll` with a string pattern: the shape
    // everyday string handling is built on (regular expressions are still absent).
    assert_eq!(eval_string("'aab'.replace('a', 'c')"), "cab");
    assert_eq!(eval_string("'aab'.replaceAll('a', 'c')"), "ccb");
    assert_eq!(eval_string("'abc'.replace('', '-')"), "-abc");

    // `Array.prototype.flat`: the depth test is per element, so the default
    // depth of 1 spreads one level only.
    assert_eq!(eval_string("JSON.stringify([1, [2, [3]]].flat())"), "[1,2,[3]]");
    assert_eq!(eval_string("JSON.stringify([1, [2, [3]]].flat(2))"), "[1,2,3]");

    // `Object.fromEntries` is the inverse of `Object.entries`.
    assert_eq!(
        eval_string("JSON.stringify(Object.fromEntries([['a', 1], ['b', 2]]))"),
        "{\"a\":1,\"b\":2}"
    );
    assert_eq!(eval_number("Object.fromEntries([['a', 7]]).a"), 7.0);

    // URI handling: percent-encoding of UTF-8, with the spec's per-function
    // unescaped sets (`/` survives `encodeURI`, not `encodeURIComponent`).
    assert_eq!(eval_string("encodeURIComponent('a b')"), "a%20b");
    assert_eq!(eval_string("encodeURIComponent('a/b')"), "a%2Fb");
    assert_eq!(eval_string("encodeURI('http://a.b/c?d=1#e')"), "http://a.b/c?d=1#e");
    assert_eq!(eval_string("decodeURIComponent('a%20b')"), "a b");
    // A malformed escape is a URIError.
    assert_eq!(
        eval_string("try { decodeURIComponent('%zz'); } catch (e) { e.name; }"),
        "URIError"
    );
}

#[test]
fn date_is_usable_for_everyday_time_handling() {
    // Construction: time value, components, and a parsed string.
    assert_eq!(eval_string("typeof new Date()"), "object");
    assert_eq!(
        eval_string("Object.prototype.toString.call(new Date())"),
        "[object Date]"
    );
    assert_eq!(
        eval_string("new Date(0).toISOString()"),
        "1970-01-01T00:00:00.000Z"
    );
    assert_eq!(eval_number("new Date(2020, 0, 2, 3, 4, 5, 6).getFullYear()"), 2020.0);
    assert_eq!(eval_number("new Date(2020, 0, 2, 3, 4, 5, 6).getMonth()"), 0.0);
    assert_eq!(eval_number("new Date(2020, 0, 2, 3, 4, 5, 6).getDate()"), 2.0);
    assert_eq!(eval_number("new Date(2020, 0, 2, 3, 4, 5, 6).getHours()"), 3.0);
    assert_eq!(
        eval_number("new Date(2020, 0, 2, 3, 4, 5, 6).getMilliseconds()"),
        6.0
    );
    // A year below 100 is offset by 1900 (ES 21.4.1.1).
    assert_eq!(eval_number("new Date(90, 0, 1).getFullYear()"), 1990.0);

    // Statics.
    assert_eq!(eval_number("Date.parse('2020-01-02T03:04:05Z')"), 1577934245000.0);
    assert_eq!(eval_number("Date.UTC(2020, 0, 2)"), 1577923200000.0);
    assert_eq!(eval_string("typeof Date.now()"), "number");
    // `Date()` without `new` answers a string (ES 21.4.2.1).
    assert_eq!(eval_string("typeof Date()"), "string");

    // An unparsable string is the Invalid Date: `getTime` is NaN and `toString`
    // says so, while `toJSON` answers `null` so `JSON.stringify` survives it.
    assert_eq!(eval_string("new Date('nope').toString()"), "Invalid Date");
    assert_eq!(eval_string("String(new Date('nope').getTime())"), "NaN");
    assert_eq!(eval_string("JSON.stringify(new Date('nope'))"), "null");
    assert_eq!(
        eval_string("JSON.stringify({ d: new Date(0) })"),
        "{\"d\":\"1970-01-01T00:00:00.000Z\"}"
    );

    // Dates take part in arithmetic: `-` is ToNumeric, which starts with
    // ToPrimitive(hint number) — that is what makes sorting by subtraction work.
    assert_eq!(eval_number("new Date(300) - new Date(100)"), 200.0);
    assert_eq!(
        eval_string(
            "[new Date(300), new Date(100)].sort(function (a, b) { return a - b; })\n             .map(function (d) { return d.getTime(); }).join(',')"
        ),
        "100,300"
    );

    // Setters keep the time and answer the new time value.
    assert_eq!(eval_number("new Date(0).setTime(5000)"), 5000.0);
    assert_eq!(
        eval_number(
            "(function () { var d = new Date(0); d.setFullYear(2020); return d.getFullYear(); })()"
        ),
        2020.0
    );
}

#[test]
fn regular_expressions_work_for_everyday_patterns() {
    // A literal builds a real RegExp; the engine used to answer `undefined`.
    assert_eq!(eval_string("typeof /a+/"), "object");
    assert_eq!(eval_string("/ab+c/gi.source"), "ab+c");
    assert_eq!(eval_string("/ab+c/gi.flags"), "gi");
    assert_eq!(eval_string("String(/a+/g)"), "/a+/g");

    // `test` / `exec`, including groups and the `index`/`input` properties.
    assert_eq!(eval_string("String(/ab+c/.test('xxabbbcxx'))"), "true");
    assert_eq!(
        eval_string("JSON.stringify(/(\\d+)-(\\d+)/.exec('a12-34b'))"),
        "[\"12-34\",\"12\",\"34\"]"
    );
    assert_eq!(eval_string("String(/z/.exec('abc'))"), "null");
    assert_eq!(eval_string("new RegExp('a+', 'i').test('AAA') ? 'y' : 'n'"), "y");

    // Flags are readable, and `lastIndex` advances across `g` matches.
    assert_eq!(eval_string("String(/a/g.global)"), "true");
    assert_eq!(eval_string("String(/a/.global)"), "false");
    assert_eq!(
        eval_number("(function () { var r = /\\d/g; var n = 0; while (r.exec('a1b2c3')) { n++; } return n; })()"),
        3.0
    );

    // The four String methods that take a pattern.
    assert_eq!(eval_string("JSON.stringify('a1b2c3'.match(/\\d/g))"), "[\"1\",\"2\",\"3\"]");
    assert_eq!(eval_string("'a1b2'.replace(/\\d/g, '#')"), "a#b#");
    assert_eq!(eval_number("'abc123'.search(/\\d/)"), 3.0);
    assert_eq!(
        eval_string("JSON.stringify('a1b22c'.split(/\\d+/))"),
        "[\"a\",\"b\",\"c\"]"
    );
    // `$` substitutions in the replacement.
    assert_eq!(eval_string("'Doe, John'.replace(/(\\w+), (\\w+)/, '$2 $1')"), "John Doe");
    // … while a plain string argument still takes the string path.
    assert_eq!(eval_string("'a1b2'.replace('1', '#')"), "a#b2");
}

#[test]
fn objects_take_part_in_arithmetic_and_arrays_flat_map() {
    // Binary arithmetic runs ToNumeric, which starts with ToPrimitive(hint
    // number) — without it every object operand collapses to NaN.
    assert_eq!(eval_number("new Date(300) - new Date(100)"), 200.0);
    assert_eq!(eval_number("new Number(5) - 2"), 3.0);
    assert_eq!(eval_number("new Number(3) * 2"), 6.0);
    assert_eq!(eval_number("'6' - 1"), 5.0);
    assert_eq!(
        eval_number("(function () { var o = { valueOf: function () { return 10; } }; return o - 1; })()"),
        9.0
    );

    // `flatMap` maps and flattens one level; a non-array result is appended.
    assert_eq!(
        eval_string("JSON.stringify([1, 2].flatMap(function (x) { return [x, x * 2]; }))"),
        "[1,2,2,4]"
    );
    assert_eq!(
        eval_string("JSON.stringify([1, 2].flatMap(function (x) { return x + 1; }))"),
        "[2,3]"
    );
    assert_eq!(
        eval_string("JSON.stringify(['ab cd'].flatMap(function (s) { return s.split(' '); }))"),
        "[\"ab\",\"cd\"]"
    );

    // `console.time` / `timeEnd` are host additions; they answer `undefined`.
    assert_eq!(eval_string("typeof console.time"), "function");
    assert_eq!(eval_string("String(console.timeEnd('never-started'))"), "undefined");
}

#[test]
fn promises_are_constructible_and_expose_the_spec_surface() {
    // The spec surface is there: the constructor, its statics, and the
    // prototype methods. The queue's behaviour is covered by the probes in the
    // plan (B34) — observing it needs code that runs *after* the drain, which
    // `eval_string` cannot express.
    assert_eq!(eval_string("typeof Promise"), "function");
    assert_eq!(eval_string("typeof Promise.resolve"), "function");
    assert_eq!(eval_string("typeof Promise.reject"), "function");
    assert_eq!(eval_string("typeof new Promise(function () {}).then"), "function");
    assert_eq!(eval_string("typeof new Promise(function () {}).catch"), "function");
    assert_eq!(
        eval_string("Object.prototype.toString.call(new Promise(function () {}))"),
        "[object Promise]"
    );
    // Constructing without `new` is a TypeError (ES 27.2.1.1).
    assert_eq!(
        eval_string(
            "(function () { try { Promise(function () {}); return 'no-throw'; } catch (e) { return e.name; } })()"
        ),
        "TypeError"
    );
}

#[test]
fn promise_microtasks_settle_handlers_and_propagate_values() {
    // `run` drains the microtask queue at the end of the top-level program, and
    // the *last* handler's return value is what ends up in `Rv` — which is what
    // `eval_string` reads back. That makes the queue's observable behaviour
    // testable without an event loop.
    assert_eq!(
        eval_string("Promise.resolve(41).then(function (v) { return String(v + 1); })"),
        "42"
    );
    // `then` chains: the return value of one handler feeds the next.
    assert_eq!(
        eval_string(
            "new Promise(function (r) { r(1); }) \
             .then(function (a) { return 'A' + a; }) \
             .then(function (b) { return b + 'B'; })"
        ),
        "A1B"
    );
    // A rejected promise reaches `catch`'s handler with the reason.
    assert_eq!(
        eval_string("Promise.reject('boom').catch(function (e) { return 'caught:' + e; })"),
        "caught:boom"
    );
    // Regression: a chained `new Promise(…throw…).catch(cb)` used to lose `cb`.
    // The throwing executor unwound a nested frame without rewinding that
    // frame's control-stack entries, so the chain's arguments were read from the
    // wrong slots and the handler saw `undefined`.
    assert_eq!(
        eval_string(
            "new Promise(function (r, j) { throw 'x'; }) \
             .catch(function (e) { return 'c:' + e; })"
        ),
        "c:x"
    );
    // Synchronous code runs before the queue drains.
    assert_eq!(
        eval_string(
            "var log = ['sync']; \
             new Promise(function (r) { r(); }) \
               .then(function () { log.push('then'); return log.join(','); })"
        ),
        "sync,then"
    );
}

#[test]
fn promise_combinators_and_resolution_procedure() {
    // `Promise.all` fulfils with the element values in order once all settle.
    assert_eq!(
        eval_string(
            "Promise.all([1, Promise.resolve(2), 3]) \
               .then(function (v) { return v.join(','); })"
        ),
        "1,2,3"
    );
    // The first rejection wins.
    assert_eq!(
        eval_string(
            "Promise.all([1, Promise.reject('boom')]) \
               .catch(function (e) { return 'caught:' + e; })"
        ),
        "caught:boom"
    );
    // `Promise.all([])` fulfils with an empty array.
    assert_eq!(
        eval_string("Promise.all([]).then(function (v) { return 'len:' + v.length; })"),
        "len:0"
    );
    // `Promise.race` settles with the first element to settle.
    assert_eq!(
        eval_string(
            "Promise.race([new Promise(function () {}), Promise.resolve('fast')]) \
               .then(function (v) { return v; })"
        ),
        "fast"
    );
    // `Promise.race` also passes a rejection through.
    assert_eq!(
        eval_string(
            "Promise.race([Promise.reject('r')]) \
               .catch(function (e) { return 'race:' + e; })"
        ),
        "race:r"
    );
    // `Promise.allSettled` never rejects; it reports both statuses in order.
    assert_eq!(
        eval_string(
            "Promise.allSettled([Promise.resolve(1), Promise.reject('bad')]) \
               .then(function (v) { \
                 return v.map(function (r) { return r.status; }).join(','); \
               })"
        ),
        "fulfilled,rejected"
    );
    assert_eq!(
        eval_string(
            "Promise.allSettled([Promise.reject('bad')]) \
               .then(function (v) { return v[0].reason; })"
        ),
        "bad"
    );
    // `Promise.any` fulfils with the first fulfilment and ignores rejections.
    assert_eq!(
        eval_string(
            "Promise.any([Promise.reject('x'), Promise.resolve('ok')]) \
               .then(function (v) { return v; })"
        ),
        "ok"
    );
    // `finally` runs its callback and passes the settlement through to a fresh
    // promise.
    assert_eq!(
        eval_string(
            "Promise.resolve(5).finally(function () {}) \
               .then(function (v) { return 'v:' + v; })"
        ),
        "v:5"
    );
    assert_eq!(
        eval_string(
            "Promise.reject('n').finally(function () {}) \
               .catch(function (e) { return 'e:' + e; })"
        ),
        "e:n"
    );
    // A throwing `finally` callback rejects the derived promise.
    assert_eq!(
        eval_string(
            "Promise.resolve(1).finally(function () { throw 'f'; }) \
               .catch(function (e) { return 'f:' + e; })"
        ),
        "f:f"
    );
    // The resolution procedure adopts a foreign thenable.
    assert_eq!(
        eval_string(
            "Promise.resolve({ then: function (res) { res(7); } }) \
               .then(function (v) { return 't:' + v; })"
        ),
        "t:7"
    );
}

#[test]
fn async_functions_and_await() {
    // An `async` function answers a promise that fulfils with its return value.
    assert_eq!(
        eval_string(
            "async function f() { return 1; } \
               f().then(function (v) { return 'r:' + v; })"
        ),
        "r:1"
    );
    // `await` on an already-settled promise yields its value.
    assert_eq!(
        eval_string(
            "async function f() { return await Promise.resolve(2); } \
               f().then(function (v) { return 'r:' + v; })"
        ),
        "r:2"
    );
    // `await` on a non-promise wraps it (ES 27.7.5.3 resolution procedure).
    assert_eq!(
        eval_string("async function f() { return await 3; } f().then(function (v) { return 'n:' + v; })"),
        "n:3"
    );
    // A throw inside an `async` function rejects its promise.
    assert_eq!(
        eval_string(
            "async function f() { throw 'bad'; } \
               f().catch(function (e) { return 'e:' + e; })"
        ),
        "e:bad"
    );
    // A rejection awaited inside `try`/`catch` is observable synchronously.
    assert_eq!(
        eval_string(
            "async function f() { \
               try { await Promise.reject('x'); } catch (e) { return 'caught:' + e; } \
             } \
             f().then(function (v) { return v; })"
        ),
        "caught:x"
    );
    // Async arrow functions work the same way.
    assert_eq!(
        eval_string("var f = async () => 4; f().then(function (v) { return 'a:' + v; })"),
        "a:4"
    );
    // Async methods too.
    assert_eq!(
        eval_string(
            "var o = { m: async function () { return 5; } }; \
               o.m().then(function (v) { return 'm:' + v; })"
        ),
        "m:5"
    );
    // `await` sequences: the second await sees the first result.
    assert_eq!(
        eval_string(
            "async function f() { \
               var a = await Promise.resolve(1); \
               var b = await Promise.resolve(a + 1); \
               return a + b; \
             } \
             f().then(function (v) { return 'sum:' + v; })"
        ),
        "sum:3"
    );
}

#[test]
fn chained_reactions_run_including_when_the_executor_throws() {
    // The legacy symptom: `new Promise(function () { throw … }).catch(cb)` dropped
    // the handler, so the reaction never ran. It turned out to be left-over state
    // between runs (B37's `VM::run` resets), and the chained form is the one that
    // exposed it — `var p = …; p.catch(cb)` worked.
    //
    // Observing a microtask needs the same trick the test262 runner uses: run the
    // program (which drains the queue), then read a global back off the VM. (The
    // handler writes through an object, because writing a captured binding is the
    // engine's known closure debt.)
    use biujs::{Compiler, VM};

    let mut vm = VM::new();
    let mut compiler = Compiler::new();
    let module = compiler
        .compile(
            "var box = { seen: '' };
             new Promise(function (resolve) { resolve(1); })
                 .then(function (v) { box.seen = box.seen + 't:' + v + ';'; });
             new Promise(function () { throw new Error('exec'); })
                 .catch(function (e) { box.seen = box.seen + 'c:' + e.message + ';'; });
             Promise.resolve(2).then(function (v) { box.seen = box.seen + 'r:' + v + ';'; });
             box",
        )
        .expect("compiles");
    vm.run(&module).expect("runs");

    let boxed = vm.global("box").expect("the program's global");
    let Value::Object(obj) = &boxed else {
        panic!("box should be an object");
    };
    let seen = obj
        .borrow()
        .property_get(&biujs::vm::PropertyKey::from_str("seen"))
        .expect("seen property")
        .value;
    // Same order node reports: the reactions run in the order they were queued.
    assert_eq!(seen.to_js_string(), "t:1;c:exec;r:2;");
}

#[test]
fn replace_accepts_a_function_replacer() {
    // The everyday `s.replace(/re/g, fn)` shape. The replacer is user code, so
    // the substitution is driven by the VM (the builtin layer cannot call back).
    assert_eq!(
        eval_string("'a1b2c3'.replace(/\\d/g, function (m) { return '[' + m + ']'; })"),
        "a[1]b[2]c[3]"
    );
    // `(match, group…, offset, input)`, per ES 22.1.3.17 step 12.
    assert_eq!(
        eval_string("'Doe, John'.replace(/(\\w+), (\\w+)/, function (m, a, b) { return b + ' ' + a; })"),
        "John Doe"
    );
    assert_eq!(
        eval_string("'x=1'.replace(/(\\w)=(\\d)/g, function (m, k, v, off) { return k + ':' + v + '@' + off; })"),
        "x:1@0"
    );
    assert_eq!(
        eval_string("'a-b'.replace(/-/g, function (m, off, str) { return String(off) + '/' + str.length; })"),
        "a1/3b"
    );
    // A plain string pattern works the same way, and a miss leaves the string
    // alone without calling the replacer.
    assert_eq!(eval_string("'abc'.replace('b', function (m) { return m.toUpperCase(); })"), "aBc");
    assert_eq!(eval_string("'no-match'.replace(/z/, function () { return 'N'; })"), "no-match");
    // … while a string replacer still takes the old code path.
    assert_eq!(eval_string("'a1b2'.replace(/\\d/g, '#')"), "a#b#");
}

#[test]
fn global_this_is_the_script_s_global_object() {
    assert_eq!(eval_string("typeof globalThis"), "object");
    assert_eq!(eval_string("Object.prototype.toString.call(globalThis)"), "[object global]");
    // It is a *view* onto the script's globals, not a copy: a `var` shows up as a
    // property and a write through it is visible as a binding. (Script semantics —
    // node wraps a file in a module, so `var` there is not global; test262 runs
    // scripts.)
    assert_eq!(eval_string("var gv = 1; String(globalThis.gv)"), "1");
    assert_eq!(eval_string("globalThis.gw = 2; String(gw)"), "2");
    assert_eq!(eval_string("String(globalThis.globalThis === globalThis)"), "true");
    assert_eq!(eval_string("String(typeof globalThis.Math === 'object')"), "true");
    // A script-scope `let` is *not* a property of the global object (B21's whole
    // point: it lives in the script's declarative record).
    assert_eq!(
        eval_string("let gl = 5; String(typeof globalThis.gl)"),
        "undefined"
    );
    // Enumerating it lists the realm's globals (built-ins included). Each
    // `eval_string` is its own program, so this cannot look for the `gv` above.
    assert_eq!(eval_string("String(Object.keys(globalThis).indexOf('Math') >= 0)"), "true");
    assert_eq!(eval_string("String(Object.keys(globalThis).length > 10)"), "true");
}

#[test]
fn an_array_elision_is_a_hole_not_undefined() {
    // `[1,,2]` has three slots and the middle one is a *hole*: it is not an
    // element, so the array methods skip it. Pushing `undefined` instead made
    // `[1,,2].flat()` answer `[1,undefined,2]` where every host answers `[1,2]`.
    assert_eq!(eval_number("[1, , 2].length"), 3.0);
    assert_eq!(eval_string("String(1 in [1, , 2])"), "false");
    assert_eq!(eval_string("String([1, , 2][1])"), "undefined");
    // Skipped by the traversal methods, but kept (as a hole) by `map`.
    assert_eq!(eval_string("JSON.stringify([1, , 3].flat())"), "[1,3]");
    // (The counter goes through an object: writing a captured binding is the
    // engine's known closure debt.)
    assert_eq!(
        eval_string("(function () { var s = { seen: [] }; [1, , 3].forEach(function (v, i) { s.seen.push(i); }); return s.seen.join(''); })()"),
        "02"
    );
    assert_eq!(eval_string("JSON.stringify([1, , 3].join('-'))"), "\"1--3\"");
    assert_eq!(eval_string("JSON.stringify(Object.keys([1, , 3]))"), "[\"0\",\"2\"]");
    // A trailing comma is not an elision, and `[,,]` is two holes.
    assert_eq!(eval_number("[1, 2,].length"), 2.0);
    assert_eq!(eval_number("[,,].length"), 2.0);
}

#[test]
fn string_match_all_answers_an_iterator_of_match_arrays() {
    // Every match, in order, with its `index`.
    assert_eq!(
        eval_string(
            "(function () { var r = [];
             for (var m of 'a1b2c3'.matchAll(/[a-z]/g)) { r.push(m[0] + '@' + m.index); }
             return r.join(','); })()"
        ),
        "a@0,b@2,c@4"
    );
    // Capture groups come along as `1`, `2`…
    assert_eq!(
        eval_string(
            "(function () { var r = [];
             for (var m of 'a1b2'.matchAll(/([a-z])([0-9])/g)) { r.push(m[1] + m[2]); }
             return r.join(','); })()"
        ),
        "a1,b2"
    );
    // A non-global pattern is a TypeError: `matchAll` is *all* matches (ES
    // 22.2.5.8 step 4).
    assert_eq!(
        eval_string("try { 'abc'.matchAll(/[a-z]/); } catch (e) { e.name; }"),
        "TypeError"
    );
    // A non-RegExp argument is coerced with a fresh `g`.
    assert_eq!(
        eval_string(
            "(function () { var r = [];
             for (var m of 'aXa'.matchAll('a')) { r.push(m[0] + '@' + m.index); }
             return r.join(','); })()"
        ),
        "a@0,a@2"
    );
    // The receiver's `lastIndex` is untouched — the iteration runs on a clone.
    assert_eq!(
        eval_number(
            "(function () { var re = /[a-z]/g; 'abc'.matchAll(re); return re.lastIndex; })()"
        ),
        0.0
    );
    // An empty match steps forward by one code unit rather than looping for ever.
    assert_eq!(
        eval_number(
            "(function () { var n = 0;
             for (var m of 'ab'.matchAll(/x*/g)) { n += 1; }
             return n; })()"
        ),
        3.0
    );
    // `Array.from` drains the iterator — the canonical way to get the matches.
    assert_eq!(
        eval_string(
            "JSON.stringify(Array.from('a1'.matchAll(/[a-z][0-9]/g)).map(function (m) { return m[0]; }))"
        ),
        "[\"a1\"]"
    );
    // Each match array carries `index` and `input`, as `exec` does.
    assert_eq!(
        eval_string("(function () { var m = 'hey'.matchAll(/e/g).next().value; return m.index + '|' + m.input; })()"),
        "1|hey"
    );
    // `RegExp.prototype[Symbol.matchAll]` is the same algorithm with the
    // arguments swapped.
    assert_eq!(
        eval_string(
            "(function () { var r = [];
             for (var m of /[a-z]/g[Symbol.matchAll]('ab')) { r.push(m[0]); }
             return r.join(','); })()"
        ),
        "a,b"
    );
}

#[test]
fn array_from_drains_a_native_iterator() {
    // `Array.from` takes the @@iterator path first: a source that is iterable
    // without being array-like.
    assert_eq!(
        eval_string("JSON.stringify(Array.from([1, 2].values()))"),
        "[1,2]"
    );
    assert_eq!(
        eval_string("JSON.stringify(Array.from('a1'.matchAll(/[a-z]/g)).map(function (m) { return m[0]; }))"),
        "[\"a\"]"
    );
    // A string still goes through the array-like path (per code unit).
    assert_eq!(eval_string("JSON.stringify(Array.from('ab'))"), "[\"a\",\"b\"]");
    // NOTE: `Array.from(new Set([1, 2]))` is still a gap — a Set is iterable
    // without being array-like, and turning it into an iterator needs the VM.
    // `[...set]` works, which is what scripts can use meanwhile.
    assert_eq!(eval_string("JSON.stringify([...new Set([1, 2])])"), "[1,2]");
}

#[test]
fn typed_array_views_alias_the_buffer_they_were_built_on() {
    // A view sees the buffer's bytes — and another view of the same bytes sees
    // the writes. Little-endian, so 0xFF then 0x01 is 511 as a Uint32.
    assert_eq!(
        eval_number(
            "(function () { var buf = new ArrayBuffer(8);
             var u8 = new Uint8Array(buf); u8[0] = 255; u8[1] = 1;
             return new Uint32Array(buf)[0]; })()"
        ),
        511.0
    );
    assert_eq!(eval_number("new ArrayBuffer(8).byteLength"), 8.0);
    assert_eq!(eval_number("new Uint8Array(3).length"), 3.0);
    assert_eq!(
        eval_string("JSON.stringify(Array.from(new Uint8Array([1, 2, 3])))"),
        "[1,2,3]"
    );
    // The indices are own enumerable properties, and `length` is not one of
    // them — it is a getter on the prototype.
    assert_eq!(
        eval_string("JSON.stringify(Object.keys(new Uint8Array(2)))"),
        "[\"0\",\"1\"]"
    );
    assert_eq!(
        eval_string("JSON.stringify(new Uint8Array([1, 2]))"),
        "{\"0\":1,\"1\":2}"
    );
    // A view over part of a buffer keeps its own offset.
    assert_eq!(
        eval_string(
            "(function () { var v = new Uint8Array(new ArrayBuffer(8), 2, 3);
             return v.length + '|' + v.byteOffset + '|' + v.byteLength; })()"
        ),
        "3|2|3"
    );
    // `subarray` shares the buffer; `slice` copies it.
    assert_eq!(
        eval_number(
            "(function () { var a = new Uint8Array([1, 2, 3]);
             a.subarray(1)[0] = 9; return a[1]; })()"
        ),
        9.0
    );
    assert_eq!(
        eval_number(
            "(function () { var a = new Uint8Array([1, 2, 3]);
             a.slice(1)[0] = 9; return a[1]; })()"
        ),
        2.0
    );
    // `set` / `fill`.
    assert_eq!(
        eval_string(
            "(function () { var a = new Uint8Array(4); a.set([9, 8], 1);
             return Array.from(a).join(','); })()"
        ),
        "0,9,8,0"
    );
    assert_eq!(
        eval_string("Array.from(new Uint8Array(3).fill(7)).join(',')"),
        "7,7,7"
    );
    // Element types wrap the way they say they do.
    assert_eq!(
        eval_string(
            "(function () { var a = new Uint8ClampedArray(2); a[0] = -5; a[1] = 300;
             return a[0] + ',' + a[1]; })()"
        ),
        "0,255"
    );
    assert_eq!(eval_number("(function () { var a = new Int16Array(1); a[0] = -2; return a[0]; })()"), -2.0);
    assert_eq!(eval_number("(function () { var a = new Float64Array(1); a[0] = 1.5; return a[0]; })()"), 1.5);
    // A read or write past the end is not an element at all.
    assert_eq!(
        eval_string(
            "(function () { var a = new Uint8Array(2); a[5] = 9;
             return a.length + '|' + String(a[5]); })()"
        ),
        "2|undefined"
    );
    // Identity, the static, and iteration.
    assert_eq!(
        eval_string("Object.prototype.toString.call(new Uint8Array())"),
        "[object Uint8Array]"
    );
    assert_eq!(
        eval_string("String(ArrayBuffer.isView(new Uint8Array())) + ',' + String(ArrayBuffer.isView([]))"),
        "true,false"
    );
    assert_eq!(
        eval_number(
            "(function () { var s = 0;
             for (var x of new Uint8Array([1, 2, 3])) { s += x; }
             return s; })()"
        ),
        6.0
    );
    assert_eq!(eval_number("new Uint8Array().BYTES_PER_ELEMENT"), 1.0);
    assert_eq!(eval_number("Float64Array.BYTES_PER_ELEMENT"), 8.0);
}
