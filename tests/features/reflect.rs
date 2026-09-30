//! Feature tests for `Reflect` (ES 28.1 / M5-C4 / B46).
//!
//! `throw`-based assertions, as everywhere else. What is under test is the half
//! `Reflect` adds on top of the operations `Object.*` already exposes: the
//! **receiver** (the `this` an accessor runs with, and where a write lands), the
//! **boolean answer** instead of a throw, `new.target` on `construct`, and the
//! fact that keys and argument lists are read through real `[[Get]]`.

use crate::helpers::eval_js;

/// The name of the error `fn` throws, or `"none"`.
const NAME_OF: &str = "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }";

#[test]
fn get_runs_the_accessor_with_the_receiver() {
    eval_js(
        "var o1 = {};
         var receiver = { y: 42 };
         Object.defineProperty(o1, 'x', { get: function () { return this.y; } });
         if (Reflect.get(o1, 'x', receiver) !== 42) throw new Error('own accessor');
         // The receiver travels up the prototype chain unchanged.
         var o2 = Object.create(o1);
         if (Reflect.get(o2, 'x', receiver) !== 42) throw new Error('inherited accessor');
         // Without a receiver the target is the receiver, as always.
         if (Reflect.get(o1, 'x') !== undefined) throw new Error('default receiver');
         // A data property ignores the receiver.
         if (Reflect.get({ v: 7 }, 'v', receiver) !== 7) throw new Error('data property');",
    )
    .unwrap();
}

#[test]
fn set_writes_to_the_receiver_and_answers_a_boolean() {
    eval_js(
        "var target = {};
         var receiver = {};
         if (Reflect.set(target, 'p', 1, receiver) !== true) throw new Error('answer');
         if (receiver.p !== 1) throw new Error('lands on the receiver');
         if (target.p !== undefined) throw new Error('target untouched');
         // No receiver: the target is where it lands.
         if (Reflect.set(target, 'q', 2) !== true) throw new Error('answer 2');
         if (target.q !== 2) throw new Error('target');
         // A non-writable property is `false`, not a TypeError — that is the
         // whole difference from an assignment expression.
         var frozen = {};
         Object.defineProperty(frozen, 'p', { value: 1, writable: false, configurable: true });
         if (Reflect.set(frozen, 'p', 2) !== false) throw new Error('non-writable');
         if (Reflect.set({}, 'p', 1, 'not an object') !== false) throw new Error('non-object receiver');
         // An accessor without a setter likewise answers false.
         var getter = {};
         Object.defineProperty(getter, 'p', { get: function () { return 1; } });
         if (Reflect.set(getter, 'p', 2) !== false) throw new Error('no setter');
         // And with a setter, the receiver is its `this`.
         var seen = null;
         var with_setter = {};
         Object.defineProperty(with_setter, 'p', { set: function (v) { seen = this; } });
         var recv = {};
         if (Reflect.set(with_setter, 'p', 3, recv) !== true) throw new Error('setter answer');
         if (seen !== recv) throw new Error('setter this');",
    )
    .unwrap();
}

#[test]
fn construct_honours_new_target_and_rejects_non_constructors() {
    eval_js(&format!(
        "{NAME_OF}
         function Base() {{}}
         function Derived() {{}}
         Derived.prototype = Object.create(Base.prototype);
         var made = Reflect.construct(Base, [], Derived);
         if (!(made instanceof Derived)) throw new Error('prototype from newTarget');
         if (Object.getPrototypeOf(made) !== Derived.prototype) throw new Error('proto');
         // `new.target` inside the callee is the newTarget, not the callee.
         function Probe() {{ this.nt = new.target; }}
         if (Reflect.construct(Probe, [], Derived).nt !== Derived) throw new Error('new.target');
         if ((new Probe()).nt !== Probe) throw new Error('new.target without override');
         // `Reflect.construct(F, [], f)` is how the harness asks \"is f new-able?\";
         // `Reflect.*` are plain functions, so the answer is no.
         if (name_of(function () {{ Reflect.construct(function () {{}}, [], Reflect.get); }}) !== 'TypeError') {{
           throw new Error('Reflect.get is not a constructor');
         }}
         if (name_of(function () {{ return new Reflect.get({{}}, ''); }}) !== 'TypeError') {{
           throw new Error('new Reflect.get');
         }}
         if (Reflect.construct(Base, [], Base) instanceof Base !== true) throw new Error('same newTarget');"
    ))
    .unwrap();
}

#[test]
fn keys_and_argument_lists_are_read_through_get() {
    eval_js(&format!(
        "{NAME_OF}
         function Test262Error() {{}}
         // `name_of` answers the *name*, so a thrown plain object would look
         // empty; this asks whether the very object escaped.
         function abrupt_of(fn) {{
           try {{ fn(); return 'none'; }} catch (e) {{ return e instanceof Test262Error ? 'abrupt' : 'other'; }}
         }}
         function abrupt_key() {{ return {{ toString: function () {{ throw new Test262Error(); }} }}; }}
         // `ToPropertyKey` runs `toString`, so its throw is what escapes.
         var escaping = [
           abrupt_of(function () {{ Reflect.get({{}}, abrupt_key()); }}),
           abrupt_of(function () {{ Reflect.set({{}}, abrupt_key(), 1); }}),
           abrupt_of(function () {{ Reflect.has({{}}, abrupt_key()); }}),
           abrupt_of(function () {{ Reflect.deleteProperty({{}}, abrupt_key()); }})
         ];
         if (escaping.join(',') !== 'abrupt,abrupt,abrupt,abrupt') {{
           throw new Error('ToPropertyKey: ' + escaping.join(','));
         }}
         // `CreateListFromArrayLike` reads `length` through [[Get]] too.
         var calls = 0;
         var list = {{ get length() {{ calls++; return 1; }}, 0: 'x' }};
         var got = null;
         Reflect.apply(function (a) {{ got = a; }}, null, list);
         if (calls !== 1 || got !== 'x') throw new Error('arguments list: ' + calls + ' ' + got);
         // … and it insists on an object, unlike `Function.prototype.apply`.
         if (name_of(function () {{ Reflect.apply(function () {{}}, null, null); }}) !== 'TypeError') {{
           throw new Error('null list');
         }}"
    ))
    .unwrap();
}

#[test]
fn reflect_shape_and_the_two_remaining_failures_answers() {
    eval_js(
        "         if (Object.getPrototypeOf(Reflect) !== Object.prototype) throw new Error('proto');
         // It inherits from `Object.prototype`, but it carries its own tag.
         if (Object.prototype.toString.call(Reflect) !== '[object Reflect]') throw new Error('tag');
         if (Reflect[Symbol.toStringTag] !== 'Reflect') throw new Error('toStringTag');
         // Every `Reflect.*` has the spec's `length`.
         var lengths = [
           Reflect.apply.length, Reflect.construct.length, Reflect.defineProperty.length,
           Reflect.deleteProperty.length, Reflect.get.length, Reflect.getOwnPropertyDescriptor.length,
           Reflect.getPrototypeOf.length, Reflect.has.length, Reflect.isExtensible.length,
           Reflect.ownKeys.length, Reflect.preventExtensions.length, Reflect.set.length,
           Reflect.setPrototypeOf.length
         ];
         if (lengths.join(',') !== '3,2,3,2,2,2,1,2,1,1,1,3,2') throw new Error('lengths: ' + lengths.join(','));
         // A prototype chain that reaches the target is refused, not thrown.
         var o = {};
         if (Reflect.setPrototypeOf(o, o) !== false) throw new Error('self proto');
         if (Object.getPrototypeOf(o) !== Object.prototype) throw new Error('unchanged');
         if (Reflect.setPrototypeOf(o, null) !== true) throw new Error('null proto');
         if (Object.getPrototypeOf(o) !== null) throw new Error('now null');
         // Reflecting a proxy asks its traps.
         var trapped = new Proxy({}, { has: function () { return true; } });
         if (Reflect.has(trapped, 'anything') !== true) throw new Error('has trap');",
    )
    .unwrap();
}
