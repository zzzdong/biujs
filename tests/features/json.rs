//! `JSON` (M3-B6): the namespace object, the text parser and the serializer,
//! including `toJSON`, `replacer`, `space` and the reviver walk.

use crate::helpers::{eval_bool, eval_number, eval_string};

// ──────────────────────────────
// JSON.parse
// ──────────────────────────────

#[test]
fn parse_handles_every_json_value() {
    assert_eq!(eval_string("JSON.parse('{\"a\":1,\"b\":[true,null,\"x\"]}').b.join('|')"), "true||x");
    assert_eq!(eval_number("JSON.parse('-1.5e2')"), -150.0);
    assert_eq!(eval_string("JSON.parse('\"\\\\u0041\"')"), "A");
    assert_eq!(eval_string("JSON.parse(' \\t\\r\\n[1] ')[0] + ''"), "1");
    // A later duplicate key wins, and `__proto__` stays an own property.
    assert_eq!(eval_number("JSON.parse('{\"a\":1,\"a\":2}').a"), 2.0);
    assert!(eval_bool("JSON.parse('{\"__proto__\":1}').hasOwnProperty('__proto__')"));
}

#[test]
fn parse_rejects_invalid_text_with_syntax_error() {
    for source in [
        "JSON.parse('01')",
        "JSON.parse(\"{'a':1}\")",
        "JSON.parse('[1,]')",
        "JSON.parse('undefined')",
        "JSON.parse('1 2')",
        "JSON.parse()",
    ] {
        assert!(
            eval_bool(&format!(
                "(function () {{ try {{ {source}; return false; }} \
                   catch (e) {{ return e instanceof SyntaxError; }} }})()"
            )),
            "{source} should throw a SyntaxError"
        );
    }
    // ToString(Symbol) throws before parsing even starts.
    assert!(eval_bool(
        "(function () { try { JSON.parse(Symbol('s')); return false; } \
           catch (e) { return e instanceof TypeError; } })()"
    ));
}

#[test]
fn parse_reviver_walks_bottom_up() {
    // Replacing and deleting members, including nested ones.
    assert_eq!(
        eval_string(
            "JSON.stringify(JSON.parse('{\"a\":1,\"b\":2}', function (k, v) { \
               return k === 'b' ? undefined : v; }))"
        ),
        "{\"a\":1}"
    );
    assert_eq!(
        eval_string(
            "JSON.stringify(JSON.parse('{\"a\":{\"b\":1}}', function (k, v) { \
               return typeof v === 'number' ? v * 2 : v; }))"
        ),
        "{\"a\":{\"b\":2}}"
    );
    // A deleted array element is reported as `undefined` to the reviver.
    assert_eq!(
        eval_string(
            "JSON.stringify(JSON.parse('[1,2]', function (k, v) { \
               return k === '0' ? undefined : v; }))"
        ),
        "[null,2]"
    );
    // `this` is the holder object, so the reviver can read the sibling
    // property: replacing `a` with 1 only succeeds when `this.a === 1`.
    assert_eq!(
        eval_string(
            "JSON.stringify(JSON.parse('{\"a\":1}', function (k, v) { \
               return k === 'a' ? (this.a === 1 ? 'yes' : 'no') : v; }))"
        ),
        "{\"a\":\"yes\"}"
    );
}

// ──────────────────────────────
// JSON.stringify
// ──────────────────────────────

#[test]
fn stringify_handles_primitives_and_omissions() {
    assert_eq!(eval_string("JSON.stringify({ a: 1, b: [true, null] })"), "{\"a\":1,\"b\":[true,null]}");
    assert!(eval_bool("JSON.stringify(undefined) === undefined"));
    assert!(eval_bool("JSON.stringify(function () {}) === undefined"));
    assert_eq!(eval_string("JSON.stringify([undefined, function () {}, 1])"), "[null,null,1]");
    assert_eq!(eval_string("JSON.stringify({ a: undefined, b: 1 })"), "{\"b\":1}");
    assert_eq!(eval_string("JSON.stringify([NaN, Infinity])"), "[null,null]");
    assert_eq!(eval_string("JSON.stringify('a\"b\\\\c')"), "\"a\\\"b\\\\c\"");
    // Boxed primitives serialize as the primitive they wrap.
    assert_eq!(
        eval_string(
            "JSON.stringify(new Number(3)) + '|' + JSON.stringify(new String('x')) + \
             '|' + JSON.stringify(new Boolean(false))"
        ),
        "3|\"x\"|false"
    );
}

#[test]
fn stringify_honours_tojson_replacer_and_space() {
    assert_eq!(
        eval_string("JSON.stringify({ d: { toJSON: function () { return 'T'; } } })"),
        "{\"d\":\"T\"}"
    );
    assert_eq!(
        eval_string(
            "JSON.stringify({ a: 1, b: 2 }, function (k, v) { \
               return typeof v === 'number' ? v * 10 : v; })"
        ),
        "{\"a\":10,\"b\":20}"
    );
    assert_eq!(eval_string("JSON.stringify({ a: 1, b: 2, c: 3 }, ['a', 'c'])"), "{\"a\":1,\"c\":3}");
    assert_eq!(
        eval_string("JSON.stringify({ a: [1, 2] }, null, 2)"),
        "{\n  \"a\": [\n    1,\n    2\n  ]\n}"
    );
    assert_eq!(eval_string("JSON.stringify({ a: 1 }, null, '--')"), "{\n--\"a\": 1\n}");
    assert_eq!(eval_number("JSON.stringify({ a: 1 }, null, 20).length"), 20.0);
    // Accessors run, and key order is the ordinary own-property order.
    assert_eq!(eval_string("JSON.stringify({ get a() { return 5; } })"), "{\"a\":5}");
    assert_eq!(
        eval_string("JSON.stringify({ 2: 'b', 1: 'a', x: 'c', 0: 'z' })"),
        "{\"0\":\"z\",\"1\":\"a\",\"2\":\"b\",\"x\":\"c\"}"
    );
}

#[test]
fn stringify_rejects_cycles() {
    assert!(eval_bool(
        "(function () { var o = {}; o.self = o; \
           try { JSON.stringify(o); return false; } \
           catch (e) { return e instanceof TypeError; } })()"
    ));
}

// ──────────────────────────────
// The namespace object
// ──────────────────────────────

#[test]
fn json_namespace_object_shape() {
    assert_eq!(eval_string("Object.prototype.toString.call(JSON)"), "[object JSON]");
    assert_eq!(eval_string("JSON[Symbol.toStringTag]"), "JSON");
    assert_eq!(eval_number("JSON.parse.length"), 2.0);
    assert_eq!(eval_number("JSON.stringify.length"), 3.0);
    assert_eq!(eval_string("JSON.parse.name + '/' + JSON.stringify.name"), "parse/stringify");
    assert!(eval_bool("Object.getOwnPropertyDescriptor(JSON, 'parse').enumerable === false"));
    assert!(eval_bool(
        "(function () { try { JSON(); return false; } \
           catch (e) { return e instanceof TypeError; } })()"
    ));
}
