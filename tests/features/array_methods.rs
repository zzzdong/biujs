//! Feature tests for `Array.prototype` methods on generic (array-like) and
//! primitive receivers, plus hole-aware callback semantics (M3-B2).

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn callback_methods_work_on_array_like_objects() {
    // `Array.prototype.*.call(x, …)` reads `length` and the index keys through
    // the prototype chain, not just own properties.
    assert_eq!(
        eval_string(
            "var o = { length: 2, 0: 'a', 1: 'b' };
             Array.prototype.map.call(o, function (v, i) { return i + v; }).join(',')"
        ),
        "0a,1b"
    );
    assert_eq!(
        eval_string(
            "var o = { length: 3, 0: 1, 1: 2, 2: 3 };
             Array.prototype.filter.call(o, function (v) { return v > 1; }).join(',')"
        ),
        "2,3"
    );
    assert_eq!(
        eval_number(
            "var o = { length: 3, 0: 2, 1: 3, 2: 4 };
             Array.prototype.reduce.call(o, function (a, b) { return a + b; })"
        ),
        9.0
    );
}

#[test]
fn callback_methods_work_on_primitive_receivers() {
    // A primitive receiver is boxed, so `length`/indices come from its wrapper
    // prototype (`Boolean.prototype[0] = true`).
    assert_eq!(
        eval_string(
            "Boolean.prototype[0] = true;
             Boolean.prototype.length = 1;
             Array.prototype.filter.call(false, function () { return true; }).join(',')"
        ),
        "true"
    );
    // Strings are array-like for the Array methods (per-character elements).
    assert_eq!(
        eval_string(
            "Array.prototype.map.call('abc', function (c, i) { return i + c; }).join('|')"
        ),
        "0a|1b|2c"
    );
}

#[test]
fn callbacks_receive_element_index_array_and_this_arg() {
    assert_eq!(
        eval_string(
            "var a = ['x', 'y'];
             var seen = [];
             a.forEach(function (v, i, arr) { seen.push(v + i + (arr === a) + this.tag); }, { tag: 'T' });
             seen.join(',')"
        ),
        "x0trueT,y1trueT"
    );
}

#[test]
fn holes_are_skipped_by_the_callbacks() {
    // `map` keeps the length and the holes; `filter`/`every` skip them.
    assert_eq!(
        eval_string(
            "var a = [1, 2, 3];
             delete a[1];
             var mapped = a.map(function (v) { return v * 2; });
             mapped.length + ',' + (1 in mapped) + ',' + mapped[0] + ',' + mapped[2]"
        ),
        "3,false,2,6"
    );
    assert_eq!(
        eval_number(
            "var a = [1, 2];
             delete a[0];
             var visits = 0;
             a.forEach(function () { visits += 1; });
             visits"
        ),
        1.0
    );
    assert_eq!(
        eval_bool(
            "var a = [1, 2];
             delete a[0];
             a.every(function (v) { return v === 2; })"
        ),
        true
    );
}

#[test]
fn reduce_honours_initial_value_and_holes() {
    assert_eq!(
        eval_number("[1, 2, 3].reduce(function (a, b) { return a - b; }, 10)"),
        4.0
    );
    assert_eq!(
        eval_number("[1, 2, 3].reduceRight(function (a, b) { return a - b; })"),
        0.0
    );
    // Without an initial value the accumulator is the first *present* element.
    assert_eq!(
        eval_number(
            "var a = [1, 2, 3];
             delete a[0];
             a.reduce(function (acc, v) { return acc + v; })"
        ),
        5.0
    );
}

#[test]
fn index_of_is_generic_and_string_receivers_keep_substring_search() {
    assert_eq!(
        eval_number("Array.prototype.indexOf.call({ length: 3, 0: 'a', 1: 'b' }, 'b')"),
        1.0
    );
    assert_eq!(
        eval_number("Array.prototype.lastIndexOf.call({ length: 3, 0: 'a', 1: 'b' }, 'a')"),
        0.0
    );
    assert_eq!(eval_number("'abcab'.indexOf('ab')"), 0.0);
    assert_eq!(eval_number("'abcab'.lastIndexOf('ab')"), 3.0);
    assert_eq!(eval_number("'abc'.indexOf('c', 5)"), -1.0);
    assert_eq!(eval_number("'abc'.indexOf('b', -1)"), 1.0);
    assert_eq!(eval_number("'abc'.lastIndexOf('')"), 3.0);
}
