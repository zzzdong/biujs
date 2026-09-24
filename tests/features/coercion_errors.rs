//! Error semantics of the abstract operations: which conversions are abrupt.
//!
//! `ToString(Symbol)`, `ToNumber(Symbol)`, `RequireObjectCoercible` and the
//! `Throw` variant of `Set` are the conversions that can fail. They used to
//! fall back silently — `String.prototype.trim.call(undefined)` produced
//! `"[object Object]"`-style noise and `ToInteger(Symbol())` read as `0` — so
//! whole families of `return-abrupt-*` conformance tests failed. Each check
//! below pins one of those paths.

use crate::helpers::{eval_number, eval_string};

/// Name of the error `source` throws, or `"none"` when it completes normally.
///
/// Going through `catch (e) { e.name }` keeps the assertion on the *kind* of
/// error, which is what the spec fixes, rather than on message wording.
fn thrown(source: &str) -> String {
    let caught = format!(
        "var name = 'none';\ntry {{ {source} }} catch (e) {{ name = e.name; }}\nname"
    );
    eval_string(&caught)
}

#[test]
fn string_receiver_requires_object_coercible() {
    // `String.prototype` methods start with RequireObjectCoercible, so a
    // nullish receiver is a TypeError even though ToString would succeed.
    assert_eq!(thrown("String.prototype.trim.call(undefined);"), "TypeError");
    assert_eq!(thrown("String.prototype.trim.call(null);"), "TypeError");
    assert_eq!(
        thrown("String.prototype.indexOf.call(undefined, 'a');"),
        "TypeError"
    );
    assert_eq!(
        thrown("var f = String.prototype.charAt; f.call(null, 0);"),
        "TypeError"
    );
}

#[test]
fn to_string_of_symbol_throws() {
    // `[2]` keeping ToString(Symbol) total made `String(Symbol())` succeed;
    // the copy inside the engine still uses the total version.
    assert_eq!(thrown("String.prototype.trim.call(Symbol());"), "TypeError");
    assert_eq!(
        thrown("'abc'.indexOf(Symbol('x'));"),
        "TypeError",
        "searchString is ? ToString(arg)"
    );
    assert_eq!(thrown("'abc'.startsWith(Symbol('x'));"), "TypeError");
}

#[test]
fn to_number_of_symbol_throws() {
    // `[2]` ToInteger(ToNumber(Symbol)) used to read as 0.
    assert_eq!(thrown("'abc'.charAt(Symbol('p'));"), "TypeError");
    assert_eq!(thrown("[1, 2].copyWithin(0, Symbol());"), "TypeError");
    assert_eq!(thrown("[1, 2].fill(0, Symbol());"), "TypeError");
    assert_eq!(thrown("[1, 2].at(Symbol());"), "TypeError");
    assert_eq!(thrown("[1, 2].indexOf(1, Symbol());"), "TypeError");
}

#[test]
fn object_prototype_receiver_requires_object_coercible() {
    assert_eq!(thrown("Object.prototype.valueOf.call(null);"), "TypeError");
    assert_eq!(
        thrown("const valueOf = Object.prototype.valueOf; valueOf();"),
        "TypeError"
    );
    assert_eq!(
        thrown("Object.prototype.hasOwnProperty.call(undefined, 'foo');"),
        "TypeError"
    );
    assert_eq!(
        thrown("Object.prototype.propertyIsEnumerable.call(null, 'foo');"),
        "TypeError"
    );
    assert_eq!(
        thrown("Object.prototype.isPrototypeOf.call(null, {});"),
        "TypeError"
    );
    assert_eq!(
        thrown("Object.prototype.toLocaleString.call(undefined);"),
        "TypeError"
    );
    // `toString` is the exception: its prologue special-cases nullish values.
    assert_eq!(
        thrown("Object.prototype.toString.call(undefined);"),
        "none"
    );
}

#[test]
fn object_static_receivers_box_through_to_object() {
    assert_eq!(
        thrown("Object.hasOwn(undefined, 'foo');"),
        "TypeError",
        "Object.hasOwn is ? ToObject(O)"
    );
    assert_eq!(thrown("Object.keys(null);"), "TypeError");
    assert_eq!(thrown("Object.getOwnPropertyDescriptor(null, 'x');"), "TypeError");
}

#[test]
fn array_length_set_with_throw_honours_attributes() {
    // `[3]` Every one of these ends with `? Set(O, "length", …, true)`.
    assert_eq!(
        thrown("var a = []; Object.freeze(a); a.push();"),
        "TypeError"
    );
    assert_eq!(
        thrown("var a = [1]; Object.freeze(a); a.pop();"),
        "TypeError"
    );
    assert_eq!(
        thrown("var a = [1]; Object.freeze(a); a.shift();"),
        "TypeError"
    );
    assert_eq!(
        thrown("var a = []; Object.freeze(a); a.unshift();"),
        "TypeError"
    );
    // Non-writable `length` without freezing fails the same Set.
    assert_eq!(
        eval_string(
            "var a = [];
             Object.defineProperty(a, 'length', { writable: false });
             var n = 'none';
             try { a.pop(); } catch (e) { n = e.name; }
             n + ',' + a.length"
        ),
        "TypeError,0"
    );
    // A writable `length` still allows the ordinary operation.
    assert_eq!(eval_number("var a = [1, 2]; a.pop(); a.length;"), 1.0);
}

#[test]
fn strict_assignment_failures_throw() {
    // The engine has no sloppy mode, so an assignment nothing can satisfy is
    // abrupt rather than silently discarded.
    assert_eq!(
        eval_string(
            "var o = {};
             Object.defineProperty(o, 'x', { value: 1, writable: false });
             var n = 'none';
             try { o.x = 2; } catch (e) { n = e.name; }
             n + ',' + o.x"
        ),
        "TypeError,1"
    );
    assert_eq!(
        eval_string(
            "var o = { get x() { return 7; } };
             var n = 'none';
             try { o.x = 2; } catch (e) { n = e.name; }
             n + ',' + o.x"
        ),
        "TypeError,7"
    );
    assert_eq!(
        eval_string(
            "var n = 'none';
             try { (1).x = 3; } catch (e) { n = e.name; }
             n"
        ),
        "TypeError"
    );
    // Assigning over an *inherited* read-only property fails too.
    assert_eq!(
        eval_string(
            "var proto = {};
             Object.defineProperty(proto, 'x', { value: 1, writable: false });
             var o = Object.create(proto);
             var n = 'none';
             try { o.x = 2; } catch (e) { n = e.name; }
             n + ',' + o.x"
        ),
        "TypeError,1"
    );
}
