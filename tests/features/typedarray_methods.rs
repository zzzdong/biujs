//! Feature tests for `%TypedArray%.prototype` 的只读 / 原地方法族
//! （`indexOf` / `lastIndexOf` / `includes` / `join` / `reverse` / `at` /
//! `copyWithin`）。
//!
//! 这些方法与 `Array.prototype` 的同名方法**碰撞**（`Array.prototype.indexOf`
//! 等），所以必须由接收者先判定 —— 见 `call_prototype_method` 里的
//! `is_typedarray_receiver` 分支。回调那一族（`map` / `filter` …）需要 VM，另做。

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn index_of_uses_strict_equality() {
    // 严格相等：NaN 永远找不到自己（与 `includes` 的 SameValueZero 相对）。
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3, 2]).indexOf(2)"),
        1.0,
        "indexOf 返回首个匹配的下标"
    );
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3]).indexOf(9)"),
        -1.0,
        "找不到返回 -1"
    );
    assert_eq!(
        eval_number("new Float64Array([NaN]).indexOf(NaN)"),
        -1.0,
        "indexOf 用严格相等，NaN 不相等"
    );
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3]).indexOf('2')"),
        -1.0,
        "字符串与数字不严格相等"
    );
    // fromIndex：负数是 length + fromIndex，且夹到 0。
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3, 2]).indexOf(2, 2)"),
        3.0,
        "fromIndex 从指定下标开始"
    );
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3, 2]).indexOf(2, -2)"),
        3.0,
        "负 fromIndex 从末尾算"
    );
}

#[test]
fn last_index_of_searches_backwards() {
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3, 2]).lastIndexOf(2)"),
        3.0
    );
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3, 2]).lastIndexOf(2, 2)"),
        1.0,
        "fromIndex 往左找"
    );
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3]).lastIndexOf(9)"),
        -1.0
    );
    // fromIndex 大于 length-1 ⇒ 夹到 length-1（不是直接 -1）。
    assert_eq!(
        eval_number("new Uint8Array([1, 2, 3]).lastIndexOf(3, 99)"),
        2.0
    );
}

#[test]
fn includes_uses_same_value_zero() {
    assert!(eval_bool("new Uint8Array([1, 2, 3]).includes(2)"));
    assert!(!eval_bool("new Uint8Array([1, 2, 3]).includes(9)"));
    // 与 indexOf 唯一的分歧点：NaN 能找到自己。
    assert!(eval_bool("new Float64Array([NaN]).includes(NaN)"));
}

#[test]
fn join_defaults_to_comma() {
    assert_eq!(eval_string("new Uint8Array([1, 2, 3]).join()"), "1,2,3");
    assert_eq!(eval_string("new Uint8Array([1, 2, 3]).join('-')"), "1-2-3");
    assert_eq!(eval_string("new Uint8Array([]).join()"), "");
    // 单个元素不加分隔符。
    assert_eq!(eval_string("new Uint8Array([7]).join('-')"), "7");
}

#[test]
fn reverse_is_in_place() {
    assert_eq!(eval_string("new Uint8Array([1, 2, 3]).reverse().join()"), "3,2,1");
    // 原地：反转的是视图本身，不是副本。
    assert_eq!(
        eval_string(
            "var a = new Uint8Array([1, 2, 3]);
             var r = a.reverse();
             [r === a, a.join()].join('|')"
        ),
        "true|3,2,1"
    );
}

#[test]
fn at_handles_negative_and_out_of_range() {
    assert_eq!(eval_number("new Uint8Array([1, 2, 3]).at(1)"), 2.0);
    assert_eq!(eval_number("new Uint8Array([1, 2, 3]).at(-1)"), 3.0);
    assert_eq!(
        eval_string("String(new Uint8Array([1, 2, 3]).at(9))"),
        "undefined",
        "越界是 undefined"
    );
    assert_eq!(
        eval_string("String(new Uint8Array([1, 2, 3]).at(-9))"),
        "undefined"
    );
}

#[test]
fn copy_within_shifts_in_place() {
    assert_eq!(
        eval_string("new Uint8Array([1, 2, 3, 4, 5]).copyWithin(0, 3).join()"),
        "4,5,3,4,5"
    );
    // 区间重叠也要正确（先整段读出再写回）。
    assert_eq!(
        eval_string("new Uint8Array([1, 2, 3, 4, 5]).copyWithin(1, 0, 3).join()"),
        "1,1,2,3,5"
    );
    // 返回 this。
    assert_eq!(
        eval_string(
            "var a = new Uint8Array([1, 2, 3]);
             var r = a.copyWithin(0, 1);
             [r === a, a.join()].join('|')"
        ),
        "true|2,3,3"
    );
}

#[test]
fn typedarray_methods_win_over_the_array_ones() {
    // 同名方法在 `Array.prototype` 上也有 —— 接收者必须先按 TypedArray 判定，
    // 否则 `join` 会走数组那条路（而 TypedArray 不是数组对象）。
    assert_eq!(eval_string("new Int16Array([-1, 0, 1]).join('|')"), "-1|0|1");
    assert_eq!(
        eval_number("new Int16Array([-1, 0, 1]).indexOf(-1)"),
        0.0
    );
}
