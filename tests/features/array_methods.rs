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

// ──────────────────────────────
// ES6/ES2022 additions: at / copyWithin / entries / keys / values
// ──────────────────────────────

#[test]
fn array_at_supports_negative_index() {
    assert_eq!(eval_string("[1, 2, 3].at(-1) + ''"), "3");
    assert_eq!(eval_string("[1, 2, 3].at(0) + ''"), "1");
    assert!(eval_bool("[1].at(5) === undefined"));
    assert_eq!(eval_string("Array.prototype.at.call({ length: 2, 0: 'a', 1: 'b' }, -1)"), "b");
}

#[test]
fn array_copy_within_copies_ranges() {
    assert_eq!(eval_string("[1, 2, 3, 4, 5].copyWithin(0, 3).join(',')"), "4,5,3,4,5");
    assert_eq!(eval_string("[1, 2, 3, 4, 5].copyWithin(1, 3, 4).join(',')"), "1,4,3,4,5");
    // Overlapping ranges behave as if the source were snapshotted.
    assert_eq!(eval_string("[1, 2, 3, 4, 5].copyWithin(0, 1).join(',')"), "2,3,4,5,5");
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 3, 0: 1, 1: 2, 2: 3 }; \
               Array.prototype.copyWithin.call(o, 0, 1); \
               return o[0] + ',' + o[1] + ',' + o[2]; })()"
        ),
        "2,3,3"
    );
}

#[test]
fn array_entries_keys_values_are_iterators() {
    assert_eq!(
        eval_string(
            "(function () { var out = []; var it = ['a', 'b'].entries(); \
               var step = it.next(); \
               while (!step.done) { out.push(step.value[0] + ':' + step.value[1]); step = it.next(); } \
               return out.join(','); })()"
        ),
        "0:a,1:b"
    );
    assert_eq!(
        eval_string(
            "(function () { var out = []; for (var k of ['a', 'b'].keys()) { out.push(k); } \
               return out.join(','); })()"
        ),
        "0,1"
    );
    assert_eq!(
        eval_string(
            "(function () { var out = []; for (var v of ['a', 'b'].values()) { out.push(v); } \
               return out.join(','); })()"
        ),
        "a,b"
    );
    assert_eq!(
        eval_string(
            "(function () { var out = []; \
               for (var e of Array.prototype.entries.call({ length: 2, 0: 'x', 1: 'y' })) { \
                 out.push(e[0] + e[1]); } \
               return out.join(','); })()"
        ),
        "0x,1y"
    );
    assert!(eval_bool("typeof ['a'].keys()[Symbol.iterator] === 'function'"));
}

// ──────────────────────────────
// Generic (array-like) mutating methods
// ──────────────────────────────

#[test]
fn mutating_methods_work_on_array_likes() {
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 3, 0: 'a', 1: 'b', 2: 'c' }; \
               var v = Array.prototype.pop.call(o); \
               return v + '/' + o.length + '/' + (2 in o); })()"
        ),
        "c/2/false"
    );
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 1, 0: 'a' }; \
               var n = Array.prototype.push.call(o, 'b', 'c'); \
               return n + '/' + o[1] + o[2]; })()"
        ),
        "3/bc"
    );
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 2, 0: 'b', 1: 'c' }; \
               Array.prototype.unshift.call(o, 'a'); \
               return o.length + '/' + o[0] + o[1] + o[2]; })()"
        ),
        "3/abc"
    );
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 3, 0: 'a', 1: 'b', 2: 'c' }; \
               var v = Array.prototype.shift.call(o); \
               return v + '/' + o.length + '/' + o[0]; })()"
        ),
        "a/2/b"
    );
}

#[test]
fn reverse_keeps_sparse_shape() {
    // A pair with one side present moves that value to the other side.
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 4, 0: 'a' }; \
               Array.prototype.reverse.call(o); \
               return (0 in o) + ',' + (3 in o) + ',' + o[3]; })()"
        ),
        "false,true,a"
    );
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 3, 0: 'a', 1: 'b', 2: 'c' }; \
               Array.prototype.reverse.call(o); \
               return o[0] + o[1] + o[2]; })()"
        ),
        "cba"
    );
}

#[test]
fn join_and_slice_keep_holes() {
    // Every index past the first contributes a separator, present or not.
    assert_eq!(
        eval_string("Array.prototype.join.call({ length: 3, 0: 'a', 2: 'c' }, '-')"),
        "a--c"
    );
    assert_eq!(
        eval_string("Array.prototype.join.call({ length: 2, 0: null, 1: undefined })"),
        ","
    );
    assert_eq!(
        eval_string(
            "(function () { var out = Array.prototype.slice.call({ length: 3, 0: 'a', 2: 'c' }); \
               return out.length + '/' + (1 in out) + '/' + out[2]; })()"
        ),
        "3/false/c"
    );
}

#[test]
fn concat_honours_is_concat_spreadable() {
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 2, 0: 'x', 1: 'y' }; \
               o[Symbol.isConcatSpreadable] = true; \
               return [0].concat(o).join(','); })()"
        ),
        "0,x,y"
    );
    assert_eq!(
        eval_number(
            "(function () { var o = { length: 2, 0: 'x', 1: 'y' }; \
               return [0].concat(o).length; })()"
        ),
        2.0
    );
    assert_eq!(
        eval_number(
            "(function () { var a = [1, 2]; a[Symbol.isConcatSpreadable] = false; \
               return [0].concat(a).length; })()"
        ),
        2.0
    );
}

#[test]
fn splice_on_array_likes() {
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 4, 0: 'a', 1: 'b', 2: 'c', 3: 'd' }; \
               var rem = Array.prototype.splice.call(o, 1, 2, 'X'); \
               return rem.join(',') + '/' + o.length + '/' + o[0] + o[1] + o[2]; })()"
        ),
        "b,c/3/aXd"
    );
    // `splice()` with no arguments deletes nothing (ES 23.1.3.28 step 5).
    assert_eq!(
        eval_string(
            "(function () { var a = [1, 2, 3]; var rem = a.splice(); \
               return rem.length + '/' + a.length; })()"
        ),
        "0/3"
    );
    assert_eq!(
        eval_string(
            "(function () { var o = { length: 3, 0: 'a', 1: 'b', 2: 'c' }; \
               var rem = Array.prototype.splice.call(o, 1); \
               return rem.length + '/' + o.length; })()"
        ),
        "2/1"
    );
}

#[test]
fn absurd_lengths_do_not_allocate() {
    // A receiver may claim `length = 2^53 - 1`; the sparse paths must handle it
    // and the element-by-element paths must refuse instead of allocating.
    assert!(eval_bool(
        "(function () { var o = { length: Math.pow(2, 53) - 1 }; \
           Array.prototype.splice.call(o); \
           return o.length === Math.pow(2, 53) - 1; })()"
    ));
    assert!(eval_bool(
        "(function () { var o = { length: Math.pow(2, 53) - 1 }; \
           try { Array.prototype.splice.call(o, 0, 0, null); return false; } \
           catch (e) { return e instanceof TypeError; } })()"
    ));
    assert!(eval_bool(
        "(function () { var o = { length: Math.pow(2, 53) }; \
           try { Array.prototype.fill.call(o, 1); return false; } \
           catch (e) { return e instanceof RangeError; } })()"
    ));
    assert!(eval_bool(
        "(function () { var v = {}; var o = { length: Math.pow(2, 53) - 1 }; \
           Array.prototype.fill.call(o, v, Math.pow(2, 53) - 4, Math.pow(2, 53) - 1); \
           return o[Math.pow(2, 53) - 2] === v; })()"
    ));
}
