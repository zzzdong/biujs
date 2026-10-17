//! 带标签模板 `` tag`a${x}b` ``（ES 12.3.7 `TaggedTemplate`）。
//!
//! 这一族曾经整段缺失：`lower_expression` 没有 `TaggedTemplateExpression` 分支，
//! 于是掉进兜底 arm 编译成 `null` —— `String.raw` 作为内建存在，但
//! `` String.raw`a\nb` `` 返回 `null`。
//!
//! 这里的断言同时锁住两件容易漏的事：模板对象是**每站点一个**（同一个对象，
//! 冻结，带 `raw`），以及替换表达式的 `ToString` 抛错必须传播。

use crate::helpers::{eval_bool, eval_number, eval_string};

// ── 形状：cooked / raw ──────────────────────────────────

#[test]
fn cooked_and_raw_are_both_forwarded() {
    assert_eq!(
        eval_string(
            r#"
            function tag(s, v) {
                return JSON.stringify(s) + '|' + JSON.stringify(s.raw) + '|' + v;
            }
            tag`a\tb${1}c`
            "#
        ),
        // cooked 里 `\t` 是一个制表符（JSON 再转义成 `\t`）；raw 里是反斜杠 + t。
        "[\"a\\tb\",\"c\"]|[\"a\\\\tb\",\"c\"]|1"
    );
}

#[test]
fn string_raw_is_reachable_through_its_syntax() {
    assert_eq!(eval_string(r#"String.raw`a\nb`"#), "a\\nb");
    assert_eq!(eval_string(r#"String.raw`a=${1}`"#), "a=1");
}

#[test]
fn the_template_object_is_an_array() {
    assert_eq!(
        eval_number(
            r#"
            function tag(s) { return s.length * 100 + (Array.isArray(s) ? 1 : 0); }
            tag`a${1}b${2}c`
            "#
        ),
        // 3 段（a / b / c）→ 300，加上"是数组"的 1。
        301.0
    );
}

// ── `this` 与求值顺序 ───────────────────────────────────

#[test]
fn a_member_tag_keeps_its_receiver() {
    assert_eq!(
        eval_string(
            r#"
            var o = { k: 'OBJ', m(s) { return this.k + ':' + s[0]; } };
            o.m`hi`
            "#
        ),
        "OBJ:hi"
    );
}

#[test]
fn a_computed_member_tag_keeps_its_receiver() {
    assert_eq!(
        eval_string(
            r#"
            var key = 'm';
            var o = { k: 'K', m(s) { return this.k + ':' + s[0]; } };
            o[key]`yo`
            "#
        ),
        "K:yo"
    );
}

#[test]
fn substitutions_run_before_the_call() {
    // ES 12.3.7.1：tag 引用 → 模板对象 → 替换表达式（从左到右）→ 调用。
    assert_eq!(
        eval_string(
            r#"
            var log = [];
            function tag(s) { log.push('call'); return 'R'; }
            function side(v) { log.push(v); return v; }
            var r = tag`${side('a')}${side('b')}`;
            r + '|' + log.join(',')
            "#
        ),
        "R|a,b,call"
    );
}

// ── 站点缓存（`GetTemplateObject`）──────────────────────

#[test]
fn one_site_reuses_one_object() {
    assert_eq!(
        eval_bool(
            r#"
            function tag(s) { return s; }
            function site() { return tag`a${1}b`; }
            site() === site()
            "#
        ),
        true
    );
}

#[test]
fn one_site_is_shared_by_every_closure() {
    assert_eq!(
        eval_bool(
            r#"
            function tag(s) { return s; }
            function mk() { return function () { return tag`z`; }; }
            var f = mk(), g = mk();
            f() === g()
            "#
        ),
        true
    );
}

#[test]
fn different_sites_are_different_objects() {
    assert_eq!(
        eval_bool(
            r#"
            function tag(s) { return s; }
            function a() { return tag`x`; }
            function b() { return tag`x`; }
            a() !== b()
            "#
        ),
        true
    );
}

#[test]
fn the_template_object_is_frozen() {
    assert_eq!(
        eval_string(
            r#"
            function tag(s) { return s; }
            var s = tag`a${1}`;
            function attrs(o, k) {
                var d = Object.getOwnPropertyDescriptor(o, k);
                return [d.writable, d.enumerable, d.configurable].join('');
            }
            [Object.isFrozen(s), Object.isFrozen(s.raw),
             attrs(s, 'raw'), attrs(s, '0'), attrs(s, 'length')].join('|')
            "#
        ),
        // raw: 三个都不可写/不可枚举/不可配置；元素：可枚举但不可写不可配置；
        // length：三个都不可。`join('')` 拼的是 "true"/"false"，所以 `fff`
        // 那种简写在这里不成立。
        "true|true|falsefalsefalse|falsetruefalse|falsefalsefalse"
    );
}

// ── 替换表达式：ToString 的 abrupt completion ────────────

#[test]
fn a_throwing_to_string_propagates() {
    assert_eq!(
        eval_bool(
            r#"
            var obj = { toString: function () { throw new Error('boom'); } };
            function probe() { try { `${obj}`; return false; } catch (e) { return true; } }
            probe()
            "#
        ),
        true
    );
}

#[test]
fn a_throwing_value_of_propagates_from_to_number() {
    // 同一个 `ToPrimitive` 入口：一元 `+` 与算术运算符都必须把用户异常抛出去。
    assert_eq!(
        eval_string(
            r#"
            var obj = { valueOf: function () { throw new Error('boom'); } };
            function probe(fn) { try { fn(); return 'no-throw'; } catch (e) { return 'threw'; } }
            [probe(function () { return +obj; }),
             probe(function () { return obj - 1; }),
             probe(function () { return obj < 1; })].join(',')
            "#
        ),
        "threw,threw,threw"
    );
}
