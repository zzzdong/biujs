//! Feature tests for `Object` builtin completeness (M3-B1): prototype
//! predicates, `ToObject` handling in the key/enumeration helpers, and
//! descriptor conversion through `[[Get]]`.

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn is_prototype_of_walks_the_chain() {
    assert_eq!(
        eval_bool(
            "var b = { x: 1 };
             var d = Object.create(b);
             var e = Object.create(d);
             b.isPrototypeOf(d) && b.isPrototypeOf(e) && d.isPrototypeOf(e) && !e.isPrototypeOf(b)"
        ),
        true
    );
    assert_eq!(eval_bool("Object.prototype.isPrototypeOf({})"), true);
    assert_eq!(eval_bool("({}).isPrototypeOf(1)"), false);
}

#[test]
fn property_is_enumerable_reports_own_attributes() {
    assert_eq!(
        eval_string(
            "var o = { a: 1 };
             Object.defineProperty(o, 'b', { value: 2, enumerable: false });
             o.propertyIsEnumerable('a') + ',' + o.propertyIsEnumerable('b') + ',' +
             o.propertyIsEnumerable('toString')"
        ),
        "true,false,false"
    );
}

#[test]
fn keys_values_entries_accept_primitives() {
    // ToObject: a string exposes its index keys, other primitives have none.
    assert_eq!(eval_string("Object.keys('abc').join(',')"), "0,1,2");
    assert_eq!(eval_number("Object.keys(42).length"), 0.0);
    assert_eq!(eval_string("Object.values('ab').join('')"), "ab");
    assert_eq!(
        eval_string("Object.entries('ab').map(function (e) { return e[0] + e[1]; }).join('')"),
        "0a1b"
    );
    assert_eq!(
        eval_string("try { Object.keys(null); 'no'; } catch (e) { 'threw'; }"),
        "threw"
    );
}

#[test]
fn define_properties_validates_its_arguments() {
    assert_eq!(
        eval_string(
            "try { Object.defineProperties({}, null); 'no'; } catch (e) { 'threw'; }"
        ),
        "threw"
    );
    assert_eq!(
        eval_string(
            "try { Object.defineProperties(0, {}); 'no'; } catch (e) { 'threw'; }"
        ),
        "threw"
    );
}

#[test]
fn descriptors_are_read_through_get() {
    // A descriptor field may be an accessor; it runs with the descriptor
    // object as `this`, and the properties object may itself hold accessors.
    assert_eq!(
        eval_bool(
            "var props = [];
             var seen = false;
             Object.defineProperty(props, 'p', {
               get: function () { seen = this === props; return { value: 5 }; },
               enumerable: true
             });
             var target = {};
             Object.defineProperties(target, props);
             seen && target.p === 5"
        ),
        true
    );
}

#[test]
fn empty_descriptor_defines_a_default_property() {
    assert_eq!(
        eval_bool(
            "var o = {};
             Object.defineProperty(o, 'x', {});
             var d = Object.getOwnPropertyDescriptor(o, 'x');
             d.value === undefined && d.writable === false &&
             d.enumerable === false && d.configurable === false"
        ),
        true
    );
}

#[test]
fn array_non_index_properties_and_length_attributes() {
    assert_eq!(
        eval_string(
            "var a = [1, 2];
             a.x = 3;
             Object.keys(a).join(',') + '|' + a.x + '|' + a.length"
        ),
        "0,1,x|3|2"
    );
    assert_eq!(
        eval_string(
            "var d = Object.getOwnPropertyDescriptor([1, 2], 'length');
             d.enumerable + ',' + d.configurable + ',' + d.writable"
        ),
        "false,false,true"
    );
}

#[test]
fn get_own_property_symbols_lists_creation_order() {
    assert_eq!(
        eval_string(
            "var a = Symbol('a'), b = Symbol('b');
             var o = {};
             o[b] = 1;
             o[a] = 2;
             var syms = Object.getOwnPropertySymbols(o);
             syms.length + ':' + (syms[0] === b) + (syms[1] === a) + ':' +
             Object.getOwnPropertyNames(o).length"
        ),
        "2:truetrue:0"
    );
}

// ── M3-B1 second batch ────────────────────────────────────────────────

#[test]
fn object_constructor_boxes_primitives() {
    // `Object(value)` is ToObject: primitives get a wrapper whose prototype is
    // the corresponding built-in prototype.
    assert_eq!(
        eval_string(
            "var n = Object(1.5);
             typeof n + ',' + (n.constructor === Number) + ',' + (n == 1.5) + ',' +
             (Object.getPrototypeOf(n) === Number.prototype)"
        ),
        "object,true,true,true"
    );
    assert_eq!(
        eval_string(
            "var s = Object('ab');
             (Object.getPrototypeOf(s) === String.prototype) + ',' +
             Object.keys(s).join(',') + ',' + s.length + ',' + s[0]"
        ),
        "true,0,1,2,a"
    );
    assert_eq!(eval_string("typeof Object(true)"), "object");
    // ES2015 19.1.1.1: `Object(null/undefined)` still returns an empty object
    // (ToObject is only applied to non-nullish values).
    assert_eq!(
        eval_string("typeof Object(undefined) + ',' + typeof Object(null)"),
        "object,object"
    );
    // String wrappers keep their read-only index keys.
    assert_eq!(
        eval_string(
            "var s = Object('ab');
             var d = Object.getOwnPropertyDescriptor(s, '0');
             d.writable + ',' + d.enumerable + ',' + d.configurable"
        ),
        "false,true,false"
    );
}

#[test]
fn define_property_redefine_rules() {
    // Same-value redefinition of a non-writable property is allowed (even on a
    // frozen object); a value change is not.
    assert_eq!(
        eval_string(
            "var o = Object.freeze({ a: 1 });
             var r1 = 'ok';
             try { Object.defineProperty(o, 'a', { value: 1 }); } catch (e) { r1 = 'threw'; }
             var r2 = 'ok';
             try { Object.defineProperty(o, 'a', { value: 2 }); } catch (e) { r2 = 'threw'; }
             r1 + ',' + r2 + ',' + o.a"
        ),
        "ok,threw,1"
    );
    // writable true → false is allowed on a non-configurable writable property;
    // the reverse is not.
    assert_eq!(
        eval_string(
            "var o = {};
             Object.defineProperty(o, 'p', { value: 1, writable: true, enumerable: true, configurable: false });
             Object.defineProperty(o, 'p', { writable: false });
             var r = 'ok';
             try { Object.defineProperty(o, 'p', { writable: true }); } catch (e) { r = 'threw'; }
             o.p + ',' + r"
        ),
        "1,threw"
    );
    // An accessor cannot be redefined over a non-configurable data property.
    assert_eq!(
        eval_string(
            "var o = {};
             Object.defineProperty(o, 'p', { value: 1, configurable: false });
             try {
               Object.defineProperty(o, 'p', { get: function () { return 9; } });
               'nothrow';
             } catch (e) { 'threw'; }"
        ),
        "threw"
    );
}

#[test]
fn array_element_accessors_and_attributes() {
    // Accessors can be defined on array elements and the getter runs.
    assert_eq!(
        eval_string(
            "var a = [1, 2, 3];
             var seen = 0;
             Object.defineProperty(a, 1, {
               get: function () { seen = 42; return seen; },
               enumerable: true, configurable: true
             });
             a[1] + ',' + seen + ',' + a.length"
        ),
        "42,42,3"
    );
    // A non-writable element stays read-only through the descriptor — and
    // because the engine is strict-only, the blocked assignment surfaces as a
    // TypeError instead of a silent no-op.
    assert_eq!(
        eval_string(
            "var a = [1, 2];
             Object.defineProperty(a, 0, { value: 9, writable: false });
             var thrown = 'none';
             try { a[0] = 99; } catch (e) { thrown = e.name; }
             a[0] + ',' + thrown + ',' + Object.getOwnPropertyDescriptor(a, 0).writable"
        ),
        "9,TypeError,false"
    );
    // A non-enumerable element is hidden from Object.keys but keeps its value.
    assert_eq!(
        eval_string(
            "var a = [1, 2, 3];
             Object.defineProperty(a, 1, { value: 20, enumerable: false });
             Object.keys(a).join(',') + '|' + a.join('|')"
        ),
        "0,2|1|20|3"
    );
}

#[test]
fn object_assign_copies_enumerable_own_keys() {
    // Symbol keys are copied; a string source contributes its index keys; a
    // source's prototype getter runs through [[Get]].
    assert_eq!(
        eval_string(
            "var sym = Symbol('s');
             var target = {};
             var result = Object.assign(target, { a: 1 }, 'bc');
             var r1 = result === target;
             var r2 = sym in Object.assign(target, (o = {}, o[sym] = 7, o));
             r1 + ',' + target.a + ',' + target[0] + target[1] + ',' + r2 + ',' + target[sym]"
        ),
        "true,1,bc,true,7"
    );
    // A getter on the source (own accessor) is invoked during the copy;
    // inherited properties are not copied.
    assert_eq!(
        eval_string(
            "var accessed = false;
             var source = {};
             Object.defineProperty(source, 'g', { get: function () { accessed = true; return 5; }, enumerable: true });
             var target = Object.assign({}, source);
             accessed + ',' + target.g"
        ),
        "true,5"
    );
    assert_eq!(
        eval_bool(
            "var proto = {};
             Object.defineProperty(proto, 'h', { value: 5 });
             var target = Object.assign({}, Object.create(proto));
             'h' in target"
        ),
        false
    );
    // undefined/null sources are skipped; undefined/null targets throw.
    assert_eq!(
        eval_string(
            "var t = { a: 1 };
             Object.assign(t, undefined, null, { b: 2 });
             var threw = 'nothrow';
             try { Object.assign(null, {}); } catch (e) { threw = 'threw'; }
             t.a + ',' + t.b + ',' + threw"
        ),
        "1,2,threw"
    );
}

#[test]
fn define_properties_skips_non_enumerable_props() {
    assert_eq!(
        eval_string(
            "var props = {};
             props.a = { value: 1 };
             Object.defineProperty(props, 'hidden', { value: { value: 2 }, enumerable: false });
             var o = {};
             Object.defineProperties(o, props);
             o.a + ',' + ('hidden' in o) + ',' + ('hidden' in props)"
        ),
        "1,false,true"
    );
}

#[test]
fn get_own_property_descriptor_accepts_string_primitives() {
    assert_eq!(
        eval_string(
            "var d = Object.getOwnPropertyDescriptor('ab', '1');
             d.value + ',' + d.writable + ',' + d.enumerable + ',' + d.configurable + ',' +
             (Object.getOwnPropertyDescriptor('ab', 'length').value)"
        ),
        "b,false,true,false,2"
    );
    assert_eq!(
        eval_string("Object.getOwnPropertyNames('ab').join(',')"),
        "0,1,length"
    );
}
