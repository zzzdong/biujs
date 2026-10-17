//! JS 字符串的索引单位是 **UTF-16 码元**。
//!
//! 引擎内部存 Rust `String`（UTF-8、按码点遍历），所以所有"按位置"的操作都差
//! 一层：非 BMP 字符占两个码元。这一组断言锁住换算层（`vm::utf16`）覆盖到的
//! 语义。
//!
//! **已知残留**：Rust `String` 装不下孤立代理，`"\uD83D"` 这种值会被折成
//! U+FFFD。因此"切在代理对中间"的用例只验证长度与位置，不验证具体码元值。

use crate::helpers::{eval_number, eval_string};

#[test]
fn length_counts_code_units() {
    assert_eq!(
        eval_string(
            r#"
            ['abc'.length, 'éé'.length, '中文'.length, '\u{1F600}'.length,
             new String('\u{1F600}').length].join(',')
            "#
        ),
        "3,2,2,2,2"
    );
}

#[test]
fn char_code_at_splits_the_pair() {
    assert_eq!(
        eval_string(
            r#"
            var s = '\u{1F600}';
            [s.charCodeAt(0), s.charCodeAt(1), s.charCodeAt(2),
             s.codePointAt(0), s.codePointAt(1)].join(',')
            "#
        ),
        // 55296 是 D800 族的实测值 55357/56832；越界是 NaN。
        "55357,56832,NaN,128512,56832"
    );
}

#[test]
fn index_access_uses_code_units() {
    assert_eq!(
        eval_number(
            r#"
            // 取第 1 个码元 = 低代理（存储成 U+FFFD），长度仍是 1。
            '\u{1F600}'[1].length
            "#
        ),
        1.0
    );
}

#[test]
fn index_of_reports_unit_offsets() {
    assert_eq!(
        eval_string(
            r#"
            var s = 'a\u{1F600}b';
            [s.indexOf('b'), s.lastIndexOf('b'), s.indexOf('\u{1F600}'), s.includes('b')].join(',')
            "#
        ),
        // 'a' 0、高代理 1、低代理 2、'b' 3。
        "3,3,1,true"
    );
}

#[test]
fn slice_and_substring_use_code_units() {
    assert_eq!(
        eval_string(
            r#"
            var s = 'a\u{1F600}b';
            [s.slice(3), s.slice(0, 1), s.slice(0, 2).length,
             s.substring(3), s.substring(0, 1)].join('|')
            "#
        ),
        // slice(3) 是 "b"；slice(0,2) 是 "a"+半个代理 → 长度 2。
        "b|a|2|b|a"
    );
}

#[test]
fn split_empty_separator_splits_code_units() {
    assert_eq!(
        eval_string(
            r#"
            var parts = '\u{1F600}'.split('');
            parts.length + '|' + parts[0].length
            "#
        ),
        // 两个码元；第 0 个是半个代理（U+FFFD），长度 1。
        "2|1"
    );
}

#[test]
fn pad_and_repeat_count_code_units() {
    assert_eq!(
        eval_string(
            r#"
            ['\u{1F600}'.padStart(5, '-').length,
             '\u{1F600}'.repeat(2).length].join(',')
            "#
        ),
        "5,4"
    );
}

#[test]
fn bmp_strings_are_unaffected() {
    assert_eq!(
        eval_string(
            r#"
            ['abc'.charAt(1), 'abc'.charCodeAt(1), 'abc'.slice(1),
             'abc'.indexOf('bc'), 'abc'.split('').length].join(',')
            "#
        ),
        "b,98,bc,1,3"
    );
}

#[test]
fn code_point_at_merges_only_a_full_pair() {
    assert_eq!(
        eval_string(
            r#"
            // 单独一个高代理没有配对，读出来就是它自己（存成 U+FFFD 的那个码元）。
            ['\u{1F600}'.codePointAt(0), '\u{1F600}'.codePointAt(1),
             'a'.codePointAt(0)].join(',')
            "#
        ),
        "128512,56832,97"
    );
}
