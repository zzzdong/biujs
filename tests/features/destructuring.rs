//! Feature tests for destructuring *assignment* patterns.
//!
//! The head of a `for-of` / `for-in` loop may be an `AssignmentPattern`
//! (`for ([a, b] of …)`), which is an assignment to existing targets — not a
//! declaration. These tests pin the parts that the lowering used to drop
//! entirely: before the fix the loop body ran with every target still
//! `undefined`.

use crate::helpers::eval_js;

/// Assertions live in JS and `throw`, so the checks cannot be fooled by the
/// (unreliable) script completion value.
fn assert_js(source: &str) {
    eval_js(source).unwrap();
}

#[test]
fn for_of_head_array_pattern_assigns_existing_targets() {
    // Identifier targets, a default that is only used for `undefined`, and a
    // rest element — all three have to run once per iteration, and they write
    // to the *outer* variables (no shadowing, no fresh binding).
    assert_js(
        "var out = [];
         var a, b, rest;
         for ([a, b = 7, ...rest] of [[1, undefined, 3, 4], [5]]) {
           out.push([a, b, rest.length].join(':'));
         }
         if (out.join('|') !== '1:7:2|5:7:0') throw new Error('got ' + out.join('|'));
         if (a !== 5) throw new Error('outer target not written: ' + a);",
    );
}

#[test]
fn for_of_head_object_pattern_assigns_existing_targets() {
    assert_js(
        "var key = 'dyn';
         var x, y, z, rest;
         for ({ x, y: y, [key]: z, ...rest } of [{ x: 1, y: 2, dyn: 3, extra: 4 }]) {}
         if (x !== 1 || y !== 2 || z !== 3) throw new Error([x, y, z].join(','));
         // `...rest` must keep the remaining *own enumerable* keys only.
         if (Object.keys(rest).join(',') !== 'extra' || rest.extra !== 4) {
           throw new Error('rest=' + JSON.stringify(rest));
         }",
    );
}

#[test]
fn for_of_head_pattern_supports_member_targets() {
    // The reference (base + key) is evaluated on every iteration, like any
    // other assignment target.
    assert_js(
        "var o = {};
         var arr = [];
         for ([o[0], arr[1]] of [[10, 20], [30, 40]]) {}
         if (o[0] !== 30 || arr[1] !== 40 || arr.length !== 2) {
           throw new Error(o[0] + ',' + arr[1] + ',' + arr.length);
         }",
    );
}

#[test]
fn for_in_head_pattern_assigns() {
    // `for-in` reuses the same head-binding path, so a pattern there destructures
    // the (string) key: `[k] = 'aa'` takes its first character.
    assert_js(
        "var k;
         var seen = [];
         for ([k] in { aa: 1 }) { seen.push(k); }
         if (seen.length !== 1 || k !== 'a') throw new Error(JSON.stringify(seen) + ':' + k);",
    );
}

#[test]
fn head_pattern_defaults_run_for_undefined_only() {
    assert_js(
        "var seen = [];
         var v;
         for ([v = (seen.push('dflt'), 9)] of [[1], [undefined]]) {}
         if (seen.length !== 1 || v !== 9) throw new Error(seen.length + '/' + v);
         // The default may read a target that a *later* element of the same
         // pattern will overwrite: evaluation is strictly left to right, so
         // `q1 = q2` still sees the value `q2` had on entry.
         var q1, q2 = 'initial';
         for ([q1 = q2, q2 = 'after'] of [[undefined, undefined]]) {}
         if (q1 !== 'initial' || q2 !== 'after') throw new Error(q1 + '/' + q2);",
    );
}

#[test]
fn head_declaration_forms_still_bind() {
    // The declaration heads (`var` / `let` / `const`) were already working and
    // share the loop lowering — a regression here would be silent.
    assert_js(
        "var got = [];
         for (var [p, q] of [[1, 2]]) { got.push(p + q); }
         for (let [r] of [[3]]) { got.push(r); }
         for (const [s] of [[4]]) { got.push(s); }
         for (var { t } of [{ t: 5 }]) { got.push(t); }
         if (got.join(',') !== '3,3,4,5') throw new Error(got.join(','));",
    );
}
