//! `Array.prototype` 的**读族**：`join` / `slice` / `includes` / `at` /
//! `concat` / `reverse`。
//!
//! 这一族原来在 `ArrayObject` 的密集存储上工作，于是三件事同时错了：访问器元素
//! 读成空位、类数组接收者走不通、原型链上的 `length` 看不到。改成走 `[[Get]]`
//! 之后要锁住这几种形态，同时不得把 `String.prototype` / TypedArray /
//! `ArrayBuffer.prototype` 上的同名方法截胡。

use crate::helpers::{eval_bool, eval_number, eval_string};

// ── 访问器元素 ──────────────────────────────────────────

#[test]
fn the_read_family_invokes_element_accessors() {
    // 只读那半边：`Object.defineProperty(a, 0, {get})` 之后所有读族都要看得见。
    assert_eq!(
        eval_string(
            r#"
            var a = [1, 2];
            Object.defineProperty(a, 0, { get: function () { return 99; }, configurable: true });
            [a.join(','), a.slice(0, 1).join(','), a.includes(99), a.at(0),
             JSON.stringify(a.concat([4])), String([...a])].join('|')
            "#
        ),
        "99,2|99|true|99|[99,2,4]|99,2"
    );
}

#[test]
fn spread_reads_through_get() {
    // `[...a]` 与 `for-of` 共用 `GetIterator` 的数组快路径。
    assert_eq!(
        eval_string(
            r#"
            var a = [1, 2];
            Object.defineProperty(a, 1, { get: function () { return 7; }, configurable: true });
            var out = [];
            for (var v of a) out.push(v);
            out.join(',') + '|' + [...a].join(',')
            "#
        ),
        "1,7|1,7"
    );
}

// ── 通用（类数组）接收者 ────────────────────────────────

#[test]
fn the_read_family_accepts_array_likes() {
    assert_eq!(
        eval_string(
            r#"
            var like = { length: 3, 0: 'a', 1: 'b' };
            [Array.prototype.join.call(like, '-'),
             Array.prototype.slice.call(like, 0, 2).length,
             Array.prototype.includes.call(like, 'b'),
             Array.prototype.at.call(like, -1)].join('|')
            "#
        ),
        // `join` 把 `undefined` 渲染成空串（`at(-1)` 取的是不存在的下标 2）。
        "a-b-|2|true|"
    );
}

#[test]
fn a_length_accessor_is_honoured() {
    assert_eq!(
        eval_number(
            r#"
            var o = { 0: 'x', 1: 'y' };
            Object.defineProperty(o, 'length', { get: function () { return 2; } });
            Array.prototype.join.call(o, '').length
            "#
        ),
        2.0
    );
}

// ── holes ───────────────────────────────────────────────

#[test]
fn holes_render_as_empty_strings_and_survive_reverse() {
    assert_eq!(
        eval_string(
            r#"
            var a = [1, , 3];
            var joined = a.join('-');
            a.reverse();
            joined + '|' + a.hasOwnProperty(1)
            "#
        ),
        // `[1, , 3].join('-')` 是 "1--3"；反转之后洞落在下标 1。
        "1--3|false"
    );
}

#[test]
fn slice_keeps_holes() {
    assert_eq!(
        eval_bool(
            r#"
            var s = [1, , 3].slice(0, 3);
            s.hasOwnProperty(1) === false && s.length === 3
            "#
        ),
        true
    );
}

// ── concat 的 spreadable 判定 ───────────────────────────

#[test]
fn concat_honours_is_concat_spreadable() {
    assert_eq!(
        eval_string(
            r#"
            var o = { length: 2, 0: 'x', 1: 'y', [Symbol.isConcatSpreadable]: true };
            var a = [1, 2];
            a[Symbol.isConcatSpreadable] = false;
            JSON.stringify([0].concat(o)) + '|' + JSON.stringify([0].concat(a))
            "#
        ),
        r#"[0,"x","y"]|[0,[1,2]]"#
    );
}

#[test]
fn concat_appends_a_plain_array_like_as_one_item() {
    assert_eq!(
        eval_string(
            r#"
            JSON.stringify([1].concat({ length: 2, 0: 'p', 1: 'q' }))
            "#
        ),
        r#"[1,{"0":"p","1":"q","length":2}]"#
    );
}

// ── species 与写失败 ────────────────────────────────────

#[test]
fn slice_uses_the_species_constructor() {
    assert_eq!(
        eval_bool(
            r#"
            class C extends Array {}
            var c = new C();
            c.push(1, 2);
            Object.getPrototypeOf(c.slice()) === C.prototype
            "#
        ),
        true
    );
}

#[test]
fn a_blocked_species_target_is_a_type_error() {
    // `CreateDataPropertyOrThrow`：目标拒绝写入就必须抛，不能静默丢掉元素。
    assert_eq!(
        eval_string(
            r#"
            var Blocked = function () { Object.preventExtensions(this); };
            var a = [1, 2];
            a.constructor = {};
            a.constructor[Symbol.species] = Blocked;
            var n = 'none';
            try { a.slice(0); } catch (e) { n = e.name; }
            n
            "#
        ),
        "TypeError"
    );
}

// ── 不得截胡同名方法 ────────────────────────────────────

#[test]
fn string_methods_win_for_string_receivers() {
    assert_eq!(
        eval_string(
            r#"
            ['abc'.slice(1), 'abc'.includes('b'), 'abc'.at(-1),
             new String('abc').slice(1), new Object(42).concat(false, true)].join('|')
            "#
        ),
        "bc|true|c|bc|42falsetrue"
    );
}

#[test]
fn arraybuffer_slice_stays_its_own() {
    // `ArrayBuffer.prototype.slice` 走 species，不能被数组读族截胡。
    assert_eq!(
        eval_bool(
            r#"
            class AB extends ArrayBuffer {}
            var ab = new AB(4);
            var sliced = ab.slice(0, 1);
            sliced instanceof AB && sliced instanceof ArrayBuffer && sliced.byteLength === 1
            "#
        ),
        true
    );
}

#[test]
fn typed_array_slice_stays_its_own() {
    assert_eq!(
        eval_string(
            r#"
            var ta = new Uint8Array([1, 2, 3]);
            var s = ta.slice(1);
            (s instanceof Uint8Array) + '|' + s.length + '|' + s[0]
            "#
        ),
        "true|2|2"
    );
}

// ── 参数转换 ────────────────────────────────────────────

#[test]
fn a_symbol_argument_to_at_is_a_type_error() {
    // `ToIntegerOrInfinity` 的 `ToNumber` 对 Symbol 抛错（不是当 0）。
    assert_eq!(
        eval_bool(
            r#"
            try { [1, 2].at(Symbol()); false; } catch (e) { e instanceof TypeError; }
            "#
        ),
        true
    );
}
