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
