//! String ES6 additions, built-in function metadata and number formatting
//! (M3-B3, plus the `Number::toString` and name/length work it depends on).

use crate::helpers::{eval_bool, eval_number, eval_string};

// ──────────────────────────────
// Built-in `name` / `length`
// ──────────────────────────────

#[test]
fn builtin_name_has_no_owner_prefix() {
    assert_eq!(eval_string("Object.keys.name"), "keys");
    assert_eq!(eval_string("Math.atan.name"), "atan");
    assert_eq!(eval_string("String.fromCharCode.name"), "fromCharCode");
    assert_eq!(eval_string("Array.prototype.push.name"), "push");
    // ES 23.1.3.30: the array's `@@iterator` *is* `Array.prototype.values`, so it
    // reports that function's name; only `String.prototype[Symbol.iterator]` is
    // its own function, defined with the `[Symbol.iterator]` property name.
    assert_eq!(eval_string("Array.prototype[Symbol.iterator].name"), "values");
    assert_eq!(
        eval_string("String.prototype[Symbol.iterator].name"),
        "[Symbol.iterator]"
    );
}

#[test]
fn builtin_length_matches_spec() {
    assert_eq!(eval_number("String.prototype.codePointAt.length"), 1.0);
    assert_eq!(eval_number("String.prototype.trimStart.length"), 0.0);
    assert_eq!(eval_number("String.prototype.padStart.length"), 1.0);
    assert_eq!(eval_number("String.fromCodePoint.length"), 1.0);
    assert_eq!(eval_number("Number.prototype.toString.length"), 1.0);
}

#[test]
fn function_length_skips_default_parameters() {
    assert_eq!(eval_number("(function (a, b = 1, c) {}).length"), 1.0);
    assert_eq!(eval_number("((a, b = 2) => {}).length"), 1.0);
    assert_eq!(eval_number("(function (a, ...rest) {}).length"), 1.0);
    assert_eq!(eval_number("(class { m(x = 1) {} }).prototype.m.length"), 0.0);
}

// ──────────────────────────────
// String iteration / whitespace
// ──────────────────────────────

#[test]
fn trim_uses_the_es_whitespace_set() {
    assert_eq!(eval_string("\"\\u00a0 x \".trim()"), "x");
    assert_eq!(eval_string("\"\\ufeffx\".trim()"), "x");
    assert_eq!(eval_string("\"\\n\\tx\\r\".trim()"), "x");
    // U+0085 (NEL) is *not* ES whitespace.
    assert_eq!(eval_number("\"\\u0085x\".trim().length"), 2.0);
    assert_eq!(eval_number("\"\\u0000x\".trim().length"), 2.0);
}

#[test]
fn trim_start_and_trim_end_are_callable() {
    assert_eq!(eval_string("\" x \".trimStart()"), "x ");
    assert_eq!(eval_string("\" x \".trimEnd()"), " x");
    assert_eq!(eval_string("String.prototype.trimStart.call(\"  y  \")"), "y  ");
}

// ──────────────────────────────
// New prototype methods
// ──────────────────────────────

#[test]
fn code_point_at_handles_surrogate_pairs() {
    assert_eq!(eval_number("\"\\u{1F600}a\".codePointAt(0)"), 0x1F600 as f64);
    assert_eq!(eval_number("\"\\u{1F600}a\".codePointAt(1)"), 0xDE00 as f64);
    assert_eq!(eval_number("\"\\u{1F600}a\".codePointAt(2)"), 97.0);
    assert!(eval_bool("\"\\u{1F600}\".codePointAt(2) === undefined"));
}

#[test]
fn at_supports_negative_index() {
    assert_eq!(eval_string("\"abc\".at(-1)"), "c");
    assert_eq!(eval_string("\"abc\".at(0)"), "a");
    assert!(eval_bool("\"abc\".at(3) === undefined"));
}

#[test]
fn pad_start_and_pad_end() {
    assert_eq!(eval_string("\"5\".padStart(3, \"0\")"), "005");
    assert_eq!(eval_string("\"5\".padEnd(3, \"0\")"), "500");
    assert_eq!(eval_string("\"5\".padStart(2)"), " 5");
}

#[test]
fn normalize_validates_the_form() {
    assert_eq!(eval_string("\"a\".normalize(\"NFKC\")"), "a");
    assert!(eval_bool(
        "(function () { try { \"a\".normalize(\"bad\"); return false; } \
           catch (e) { return e instanceof RangeError; } })()"
    ));
}

#[test]
fn locale_case_mapping_and_compare() {
    assert_eq!(eval_string("\"AbC\".toLocaleLowerCase()"), "abc");
    assert_eq!(eval_string("\"AbC\".toLocaleUpperCase()"), "ABC");
    assert_eq!(eval_number("\"a\".localeCompare(\"b\")"), -1.0);
    assert_eq!(eval_number("\"b\".localeCompare(\"a\")"), 1.0);
    assert_eq!(eval_number("\"a\".localeCompare(\"a\")"), 0.0);
}

#[test]
fn string_statics() {
    assert_eq!(eval_number("String.fromCharCode(-1).charCodeAt(0)"), 65535.0);
    assert_eq!(eval_number("String.fromCharCode(Infinity).charCodeAt(0)"), 0.0);
    assert_eq!(eval_number("String.fromCodePoint(65).charCodeAt(0)"), 65.0);
    assert!(eval_bool(
        "(function () { try { String.fromCodePoint(0x110000); return false; } \
           catch (e) { return e instanceof RangeError; } })()"
    ));
}

// ──────────────────────────────
// Number formatting (ES 7.1.12.1)
// ──────────────────────────────

#[test]
fn number_to_string_uses_the_spec_thresholds() {
    assert_eq!(eval_string("String(1e21)"), "1e+21");
    assert_eq!(eval_string("String(1e20)"), "100000000000000000000");
    assert_eq!(eval_string("String(0.0000001)"), "1e-7");
    assert_eq!(eval_string("String(0.000001)"), "0.000001");
    assert_eq!(eval_string("String(1.2345e-7)"), "1.2345e-7");
    assert_eq!(eval_string("String(1.5)"), "1.5");
    assert_eq!(eval_string("String(5e-324)"), "5e-324");
    assert_eq!(eval_string("String(-0)"), "0");
}

// ──────────────────────────────
// ToNumber
// ──────────────────────────────

#[test]
fn to_number_accepts_radix_prefixes() {
    assert_eq!(eval_number("+\"0x10\""), 16.0);
    assert_eq!(eval_number("+\"0b101\""), 5.0);
    assert_eq!(eval_number("Number(\"0o17\")"), 15.0);
    assert_eq!(eval_number("+\"\""), 0.0);
    assert!(eval_bool("Number(\"Infinity\") === Infinity"));
    assert!(eval_bool("+\"abc\" !== +\"abc\""));
}

#[test]
fn update_expressions_coerce_through_to_number() {
    assert_eq!(
        eval_string("(function () { var s = \"5\"; return s++ + \"|\" + s; })()"),
        "5|6"
    );
    assert_eq!(
        eval_string("(function () { var s = \"5\"; return --s + \"|\" + s; })()"),
        "4|4"
    );
    assert_eq!(eval_string("(function () { var x = new Number(1.1); return x++ + \"\"; })()"), "1.1");
}
