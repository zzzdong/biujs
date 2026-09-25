//! Feature tests for `Object` builtin completeness (M3-B1): prototype
//! predicates, `ToObject` handling in the key/enumeration helpers, and
//! descriptor conversion through `[[Get]]`.

use crate::helpers::{eval_bool, eval_js, eval_number, eval_string};

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

#[test]
fn undefined_getter_and_setter_are_not_called() {
    // `{ get: undefined }` installs `undefined` in the slot: the descriptor is
    // an accessor one, but there is nothing to invoke — reading yields
    // `undefined` and writing is a TypeError (no setter).
    eval_js(
        "var o = {};
         Object.defineProperty(o, 'p', { get: undefined, set: undefined, enumerable: true });
         var d = Object.getOwnPropertyDescriptor(o, 'p');
         var out = [typeof o.p, typeof d.get];
         var threw = 'no';
         try { o.p = 1; } catch (e) { threw = e.name; }
         if (out.join(',') !== 'undefined,undefined' || threw !== 'TypeError') {
           throw new Error('got ' + out.join(',') + ' / ' + threw);
         }
         // Replacing a real getter with `undefined` must quiet it down again.
         var q = {};
         Object.defineProperty(q, 'r', { get: function () { return 42; }, configurable: true });
         Object.defineProperty(q, 'r', { get: undefined });
         if (typeof q.r !== 'undefined') throw new Error('getter still ran');",
    )
    .unwrap();
}

#[test]
fn partial_descriptor_preserves_an_accessor_property() {
    // A descriptor that mentions no data field is generic: the existing getter
    // and setter survive, even for `{}` (ES ValidateAndApplyPropertyDescriptor
    // step 3 / step 8).
    eval_js(
        "var arr = [];
         var got = false;
         Object.defineProperty(arr, '0', {
           get: function () { got = true; return 11; },
           set: function (v) { arr.stored = v; },
           enumerable: true,
           configurable: true
         });
         Object.defineProperty(arr, '0', {});
         if (arr[0] !== 11 || !got) throw new Error('empty descriptor lost the getter');
         Object.defineProperty(arr, '0', { enumerable: false });
         arr[0] = 'v';
         var d = Object.getOwnPropertyDescriptor(arr, '0');
         if (arr.stored !== 'v') throw new Error('setter lost');
         if (d.enumerable !== false || d.configurable !== true) throw new Error('attributes lost');",
    )
    .unwrap();
}

#[test]
fn mixed_accessor_and_data_descriptor_is_a_type_error() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'no-throw'; } catch (e) { return e.name; } }
         var out = [
           name_of(function () { Object.defineProperty({}, 'p', { get: function () {}, value: 1 }); }),
           name_of(function () { Object.defineProperty({}, 'p', { set: function () {}, writable: true }); }),
           name_of(function () { Object.defineProperties({}, { p: { get: function () {}, value: 1 } }); }),
           name_of(function () { Object.create({}, { p: { set: function () {}, value: 1 } }); }),
           name_of(function () { Object.create({}, { p: { set: {} } }); })
         ];
         if (out.join(',') !== 'TypeError,TypeError,TypeError,TypeError,TypeError') {
           throw new Error('got ' + out.join(','));
         }",
    )
    .unwrap();
}

#[test]
fn shrinking_length_stops_at_a_non_configurable_index() {
    // ES 10.4.2.4: elements are deleted from the top down, and the first index
    // that refuses deletion clamps `length` to just above itself *before* the
    // request reports failure — so `length` has moved even though a TypeError
    // is thrown.
    eval_js(
        "var a = [0, 1, 2];
         Object.defineProperty(a, '1', { configurable: false });
         var threw = 'no';
         try { Object.defineProperty(a, 'length', { value: 0, writable: false }); } catch (e) { threw = e.name; }
         if (threw !== 'TypeError') throw new Error('no TypeError: ' + threw);
         if (a.length !== 2) throw new Error('length ' + a.length);
         if (Object.getOwnPropertyDescriptor(a, 'length').writable !== false) throw new Error('writable kept');
         if (a.hasOwnProperty('2') !== false || a.hasOwnProperty('1') !== true) throw new Error('elements kept');

         // The same rule applies to the plain assignment path, and to a sealed
         // array (whose elements became non-configurable).
         var b = [0, 1, 2];
         Object.defineProperty(b, '2', { configurable: false });
         threw = 'no';
         try { b.length = 1; } catch (e) { threw = e.name; }
         if (threw !== 'TypeError' || b.length !== 3) throw new Error('assign: ' + threw + ' ' + b.length);
         var c = Object.seal([1, 2]);
         threw = 'no';
         try { c.length = 1; } catch (e) { threw = e.name; }
         if (threw !== 'TypeError' || c.length !== 2) throw new Error('seal: ' + threw + ' ' + c.length);",
    )
    .unwrap();
}

#[test]
fn index_beyond_a_non_writable_length_cannot_be_defined() {
    eval_js(
        "var a = [1, 2, 3];
         Object.defineProperty(a, 'length', { writable: false });
         var threw = 'no';
         try { Object.defineProperty(a, 3, { value: 'x' }); } catch (e) { threw = e.name; }
         if (threw !== 'TypeError') throw new Error('defineProperty: ' + threw);
         threw = 'no';
         try { a[3] = 'x'; } catch (e) { threw = e.name; }
         if (threw !== 'TypeError') throw new Error('set: ' + threw);
         if (a.length !== 3) throw new Error('length ' + a.length);",
    )
    .unwrap();
}

#[test]
fn integrity_levels_cover_wrappers_and_functions() {
    // `Object.freeze` / `seal` used to be answered per object kind, and every
    // kind but `OrdinaryObject` (and `ArrayObject`) reported `false` forever.
    eval_js(
        "var n = new Number(1); n.foo = 1; Object.freeze(n);
         var s = new String('ab'); s.foo = 1; Object.freeze(s);
         var f = function () {}; f.foo = 1; Object.freeze(f);
         var g = function () {}; g.foo = 1; Object.seal(g);
         var dn = Object.getOwnPropertyDescriptor(n, 'foo');
         var dg = Object.getOwnPropertyDescriptor(g, 'foo');
         var out = [
           Object.isFrozen(n), Object.isSealed(n), Object.isExtensible(n),
           Object.isFrozen(s), Object.isFrozen(f),
           Object.isSealed(g), Object.isFrozen(g),
           dn.writable, dn.configurable, dg.writable, dg.configurable
         ];
         if (out.join(',') !== 'true,true,false,true,true,true,false,false,false,true,false') {
           throw new Error('got ' + out.join(','));
         }
         // Primitives are neither objects nor errors: they answer the integrity
         // level they cannot violate, and the three verbs return them as-is.
         if (Object.isExtensible(1) !== false) throw new Error('isExtensible(1)');
         if (Object.isFrozen(1) !== true) throw new Error('isFrozen(1)');
         if (Object.isSealed('x') !== true) throw new Error('isSealed(x)');
         if (Object.freeze(3) !== 3 || Object.seal(4) !== 4 || Object.preventExtensions(5) !== 5) {
           throw new Error('verbs must return their argument');
         }
         // A non-extensible function refuses new properties.
         var h = function () {};
         Object.preventExtensions(h);
         var threw = 'no';
         try { Object.defineProperty(h, 'x', { value: 1 }); } catch (e) { threw = e.name; }
         if (threw !== 'TypeError' || Object.isExtensible(h) !== false) throw new Error('preventExtensions(fn)');",
    )
    .unwrap();
}

#[test]
fn set_prototype_of_rejects_frozen_targets_and_cycles() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'no-throw'; } catch (e) { return e.name; } }
         var o = {};
         Object.preventExtensions(o);
         var out = [name_of(function () { Object.setPrototypeOf(o, null); })];
         var p = {};
         Object.setPrototypeOf(p, null);
         out.push(name_of(function () { Object.setPrototypeOf(p, p); }));
         // Still works on an object that may change, `this`-free included.
         var child = {};
         var parent = { x: 1 };
         Object.setPrototypeOf(child, parent);
         out.push(child.x === 1 ? 'true' : 'false');
         Object.setPrototypeOf(child, null);
         out.push(Object.getPrototypeOf(child) === null ? 'true' : 'false');
         if (out.join(',') !== 'TypeError,TypeError,true,true') throw new Error('got ' + out.join(','));",
    )
    .unwrap();
}

#[test]
fn get_prototype_of_needs_an_object() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'no-throw'; } catch (e) { return e.name; } }
         var out = [
           name_of(function () { Object.getPrototypeOf(null); }),
           name_of(function () { Object.getPrototypeOf(undefined); }),
           Object.getPrototypeOf(1) === Number.prototype ? 'true' : 'false',
           Object.getPrototypeOf('x') === String.prototype ? 'true' : 'false',
           Object.getPrototypeOf(true) === Boolean.prototype ? 'true' : 'false',
           Object.getPrototypeOf(Object.prototype) === null ? 'true' : 'false'
         ];
         if (out.join(',') !== 'TypeError,TypeError,true,true,true,true') {
           throw new Error('got ' + out.join(','));
         }",
    )
    .unwrap();
}

#[test]
fn array_length_errors_are_range_errors() {
    // `ArraySetLength` (ES 10.4.2.4) throws a *RangeError* for a length that is
    // not a valid array index. The engine's `[[DefineOwnProperty]]` reports
    // failure through a `String` channel, which used to flatten every failure
    // into a TypeError — see `RuntimeError::from_property_error`.
    eval_js(
        "function name_of(fn) { try { fn(); return 'no-throw'; } catch (e) { return e.name; } }
         var out = [
           name_of(function () { var a = []; a.length = 4294967296; }),
           name_of(function () { var a = []; a.length = 4294967297; }),
           name_of(function () { var a = [1]; a.length = -1; }),
           name_of(function () { var a = [1]; a.length = 1.5; }),
           name_of(function () { Object.defineProperty([1], 'length', { value: -1 }); }),
           name_of(function () { Object.defineProperty([1], 'length', { value: 4294967296 }); }),
           name_of(function () { Object.defineProperties([1], { length: { value: -1 } }); }),
           name_of(function () { new Array(-1); })
         ];
         if (out.join(',') !== 'RangeError,RangeError,RangeError,RangeError,RangeError,RangeError,RangeError,RangeError') {
           throw new Error('got ' + out.join(','));
         }",
    )
    .unwrap();

    // Other property-write failures stay TypeErrors, and valid length writes
    // keep working.
    eval_js(
        "function name_of(fn) { try { fn(); return 'no-throw'; } catch (e) { return e.name; } }
         var o = {};
         Object.defineProperty(o, 'x', { value: 1 });
         var out = [
           name_of(function () { Object.defineProperty(o, 'x', { value: 2 }); }),
           name_of(function () { var a = [1]; Object.defineProperty(a, 'length', { writable: false }); a.length = 5; }),
           name_of(function () { var a = Object.freeze([1]); a[0] = 2; })
         ];
         if (out.join(',') !== 'TypeError,TypeError,TypeError') {
           throw new Error('got ' + out.join(','));
         }
         var a = [1, 2, 3];
         a.length = 1;
         if (a.join(',') !== '1') throw new Error('shrink');
         a.length = 3;
         if (a.length !== 3 || a[2] !== undefined) throw new Error('grow');",
    )
    .unwrap();
}
