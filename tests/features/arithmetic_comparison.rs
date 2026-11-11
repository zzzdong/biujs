use crate::helpers::{eval_bool, eval_js, eval_number};

// ============================================================
// Arithmetic
// ============================================================

#[test]
fn addition_numbers() {
    assert_eq!(eval_number("1 + 2"), 3.0);
    assert_eq!(eval_number("0 + 0"), 0.0);
    assert_eq!(eval_number("-1 + 1"), 0.0);
    assert_eq!(eval_number("100 + 200"), 300.0);
    assert_eq!(eval_number("0.1 + 0.2"), 0.1 + 0.2);
}

#[test]
fn subtraction_numbers() {
    assert_eq!(eval_number("5 - 3"), 2.0);
    assert_eq!(eval_number("0 - 1"), -1.0);
    assert_eq!(eval_number("10 - 10"), 0.0);
}

#[test]
fn multiplication_numbers() {
    assert_eq!(eval_number("3 * 4"), 12.0);
    assert_eq!(eval_number("0 * 100"), 0.0);
    assert_eq!(eval_number("-2 * 3"), -6.0);
    assert_eq!(eval_number("1 * 1"), 1.0);
}

#[test]
fn division_numbers() {
    assert_eq!(eval_number("10 / 2"), 5.0);
    assert_eq!(eval_number("7 / 2"), 3.5);
    assert_eq!(eval_number("0 / 5"), 0.0);
    assert_eq!(eval_number("-6 / 3"), -2.0);
}

#[test]
fn modulus_numbers() {
    assert_eq!(eval_number("10 % 3"), 1.0);
    assert_eq!(eval_number("7 % 7"), 0.0);
    assert_eq!(eval_number("5 % 2"), 1.0);
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(eval_number("2 + 3 * 4"), 14.0);
    assert_eq!(eval_number("(2 + 3) * 4"), 20.0);
    assert_eq!(eval_number("10 - 2 * 3"), 4.0);
    assert_eq!(eval_number("10 / 2 + 3"), 8.0);
}

#[test]
fn arithmetic_chained() {
    assert_eq!(eval_number("1 + 2 + 3 + 4"), 10.0);
    assert_eq!(eval_number("2 * 3 * 4"), 24.0);
    assert_eq!(eval_number("100 / 10 / 2"), 5.0);
}

// ============================================================
// Comparison operators
// ============================================================

#[test]
fn greater_than() {
    assert_eq!(eval_bool("5 > 3"), true);
    assert_eq!(eval_bool("3 > 5"), false);
    assert_eq!(eval_bool("3 > 3"), false);
}

#[test]
fn less_than() {
    assert_eq!(eval_bool("3 < 5"), true);
    assert_eq!(eval_bool("5 < 3"), false);
    assert_eq!(eval_bool("3 < 3"), false);
}

#[test]
fn greater_than_or_equal() {
    assert_eq!(eval_bool("5 >= 3"), true);
    assert_eq!(eval_bool("3 >= 5"), false);
    assert_eq!(eval_bool("3 >= 3"), true);
}

#[test]
fn less_than_or_equal() {
    assert_eq!(eval_bool("3 <= 5"), true);
    assert_eq!(eval_bool("5 <= 3"), false);
    assert_eq!(eval_bool("3 <= 3"), true);
}

#[test]
fn equals() {
    assert_eq!(eval_bool("1 == 1"), true);
    assert_eq!(eval_bool("1 == 2"), false);
    assert_eq!(eval_bool("0 == 0"), true);
}

#[test]
fn not_equals() {
    assert_eq!(eval_bool("1 != 2"), true);
    assert_eq!(eval_bool("1 != 1"), false);
}

#[test]
fn strict_equals() {
    assert_eq!(eval_bool("1 === 1"), true);
    assert_eq!(eval_bool("1 === 2"), false);
    assert_eq!(eval_bool("'hello' === 'hello'"), true);
    assert_eq!(eval_bool("true === true"), true);
    assert_eq!(eval_bool("false === false"), true);
}

#[test]
fn strict_not_equals() {
    assert_eq!(eval_bool("1 !== 2"), true);
    assert_eq!(eval_bool("1 !== 1"), false);
}

// ============================================================
// Bitwise operators
// ============================================================

#[test]
fn bitwise_not() {
    // ~x = -(x + 1) for 32-bit signed integers
    assert_eq!(eval_number("~0"), -1.0);
    assert_eq!(eval_number("~1"), -2.0);
    assert_eq!(eval_number("~-1"), 0.0);
    assert_eq!(eval_number("~5"), -6.0);
    assert_eq!(eval_number("~-5"), 4.0);

    // Test with expressions
    assert_eq!(eval_number("~(1 + 2)"), -4.0);
    assert_eq!(eval_number("~~5"), 5.0); // Double negation
}

/// `>>>` 的结果是 **u32**（`-1 >>> 0` 是 4294967295），且位运算的操作数要先走
/// ToNumeric（`ToPrimitive` 之后拒绝 Symbol）。
///
/// 以前 `>>>` 把 u32 结果 `as i32` 折回有符号，整条无符号右移语义都是错的；
/// 位运算还直接用 `Value::to_number()`（对象 → NaN、Symbol → NaN），
/// 跳过 valueOf 也不抛。
#[test]
fn unsigned_right_shift_and_bitwise_are_to_numeric() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         if ((-1 >>> 0) !== 4294967295) throw new Error('-1 >>> 0: ' + (-1 >>> 0));
         if ((2147483648 >>> 0) !== 2147483648) throw new Error('2^31 >>> 0');
         if (({ valueOf: function () { return -4; } } >>> 1) !== 2147483646) throw new Error('object >>> 1');
         // `>>` 仍是算术右移，`<<` 仍按有符号 32 位回绕。
         if ((-1 >> 0) !== -1) throw new Error('-1 >> 0');
         if ((1 << 31) !== -2147483648) throw new Error('1 << 31');
         if (({ valueOf: function () { return 1; } } | 0) !== 1) throw new Error('object | 0');
         if (name_of(function () { ({ valueOf: function () { return Symbol('s'); } }) | 0; }) !== 'TypeError') {
           throw new Error('symbol | 0');
         }
         if (name_of(function () { Symbol('s') + 1; }) !== 'TypeError') throw new Error('symbol + 1');
         if (name_of(function () { Symbol('s') + ''; }) !== 'TypeError') throw new Error('symbol + string');
         if (-{ valueOf: function () { return 2; } } !== -2) throw new Error('unary minus object');",
    )
    .unwrap();
}

/// `Date` 的 ToPrimitive 在 "default" hint 下按 "string" 走（ES 20.3.4.2），
/// 所以 `d + d` 是字符串拼接，而 `d - d` 仍是数值相减。
#[test]
fn date_default_hint_is_string() {
    eval_js(
        "var d = new Date(0);
         if (d + d !== d.toString() + d.toString()) throw new Error('date + date: ' + (d + d));
         if (d - d !== 0) throw new Error('date - date: ' + (d - d));",
    )
    .unwrap();
}
