//! Array 迭代族（`map` / `filter` / `forEach` / `some` / `every` / `find*` /
//! `reduce*` / `indexOf` / `lastIndexOf`）的**逐步**语义。
//!
//! 规范把 `HasProperty(O, Pk)` 放在循环**内部**（`Array.prototype.map`
//! step 6.a.ii）：回调删掉的元素会被跳过、新加进来的会被访问、访问器元素在
//! 那一步才被调用。此前这些方法先物化一份元素清单（快照），三件事都看不见，
//! 而且访问器会被调用两次（快照一次 + 使用一次）。

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn a_deleted_element_is_skipped() {
    assert_eq!(
        eval_string(
            r#"
            var a = [1, 2, 3];
            var seen = [];
            a.forEach(function (v, i) {
                seen.push(v);
                if (i === 0) { delete a[1]; }
            });
            seen.join(',')
            "#
        ),
        "1,3"
    );
}

#[test]
fn an_element_changed_mid_iteration_is_seen() {
    assert_eq!(
        eval_string(
            r#"
            var a = [1, 2, 3];
            var out = a.map(function (v, i) {
                if (i === 0) { a[1] = 99; }
                return v * 2;
            });
            out.join(',')
            "#
        ),
        "2,198,6"
    );
}

#[test]
fn a_shrinking_length_stops_the_loop() {
    // `len` 在循环前取一次，收缩之后后面的下标不再被访问。
    assert_eq!(
        eval_number(
            r#"
            var a = [1, 2, 3];
            var calls = 0;
            a.reduce(function (acc, v, i) {
                calls += 1;
                if (i === 0) { a.length = 1; }
                return acc + v;
            }, 0);
            calls
            "#
        ),
        1.0
    );
}

#[test]
fn some_skips_an_element_deleted_by_the_callback() {
    assert_eq!(
        eval_number(
            r#"
            var a = [1, 2, 3];
            var n = 0;
            a.some(function (v, i) { n += 1; if (i === 0) { delete a[1]; } return false; });
            n
            "#
        ),
        2.0
    );
}

#[test]
fn every_also_rechecks_presence() {
    assert_eq!(
        eval_number(
            r#"
            var a = [1, 2, 3];
            var n = 0;
            a.every(function (v, i) { n += 1; if (i === 0) { delete a[2]; } return true; });
            n
            "#
        ),
        2.0
    );
}

#[test]
fn an_accessor_element_runs_exactly_once_per_visit() {
    assert_eq!(
        eval_string(
            r#"
            var a = [1, 2];
            var gets = 0;
            Object.defineProperty(a, 0, {
                get: function () { gets += 1; return 7; },
                configurable: true
            });
            var seen = [];
            a.forEach(function (v) { seen.push(v); });
            seen.join(',') + '|' + gets
            "#
        ),
        "7,2|1"
    );
}

#[test]
fn index_of_reads_live() {
    assert_eq!(
        eval_string(
            r#"
            var a = [1, 2, 3];
            delete a[1];
            [a.indexOf(2), a.lastIndexOf(2), a.indexOf(3)].join(',')
            "#
        ),
        "-1,-1,2"
    );
}

#[test]
fn find_and_find_index_skip_holes() {
    assert_eq!(
        eval_string(
            r#"
            var a = [1, , 3];
            var seen = [];
            var found = a.find(function (v, i) { seen.push(i); return v === 3; });
            found + '|' + seen.join(',')
            "#
        ),
        "3|0,2"
    );
}

#[test]
fn a_hole_stays_a_hole_in_map() {
    assert_eq!(
        eval_bool(
            r#"
            var m = [1, , 3].map(function (v) { return v; });
            m.hasOwnProperty(1) === false && m.length === 3
            "#
        ),
        true
    );
}

#[test]
fn reduce_uses_the_first_present_element_as_the_accumulator() {
    assert_eq!(
        eval_number(
            r#"
            var a = [, 2, 3];
            a.reduce(function (acc, v) { return acc + v; })
            "#
        ),
        5.0
    );
}

#[test]
fn an_empty_reduce_without_initial_value_is_a_type_error() {
    assert_eq!(
        eval_string(
            r#"
            var n = 'none';
            try { [].reduce(function () {}); } catch (e) { n = e.name; }
            n
            "#
        ),
        "TypeError"
    );
}

#[test]
fn to_sorted_and_with_still_take_a_snapshot() {
    // 列表类方法（返回新数组）不看中途改动 —— 它们本来就该用一份清单。
    assert_eq!(
        eval_string(
            r#"
            var a = [3, 1, 2];
            a.toSorted().join(',') + '|' + a.with(0, 9).join(',') + '|' + a.join(',')
            "#
        ),
        "1,2,3|9,1,2|3,1,2"
    );
}
