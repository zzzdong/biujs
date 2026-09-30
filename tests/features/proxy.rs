//! Feature tests for `Proxy` (ES 28.2 / M5-C4 / B45).
//!
//! `throw`-based assertions, as everywhere else. The contract under test is
//! ES 10.5's organising rule — **a handler without the trap is a pass-through**
//! — plus the three places where the answer is *not* the trap's: the invariant
//! checks (a proxy may not report a frozen target as unfrozen), `revoke`, and
//! the constructor's own argument checks.

use crate::helpers::eval_js;

/// The name of the error `fn` throws, or `"none"`.
const NAME_OF: &str =
    "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }";

#[test]
fn get_trap_and_the_no_trap_pass_through() {
    eval_js(
        "var target = { a: 1 };
         var seen = [];
         var p = new Proxy(target, {
           get: function (t, k, r) { seen.push(k); if (k === 'b') return 42; return t[k]; }
         });
         if (p.a !== 1) throw new Error('get: ' + p.a);
         if (p.b !== 42) throw new Error('get b: ' + p.b);
         if (seen.join(',') !== 'a,b') throw new Error('seen: ' + seen.join(','));
         // An empty handler forwards: the same operation on the target.
         var q = new Proxy(target, {});
         if (q.a !== 1) throw new Error('forward: ' + q.a);
         if (q.missing !== undefined) throw new Error('forward missing');
         // The third argument is the *proxy*, not the target.
         var receiver = null;
         var r = new Proxy(target, { get: function (t, k, rec) { receiver = rec; return 1; } });
         r.a;
         if (receiver !== r) throw new Error('receiver is not the proxy');",
    )
    .unwrap();
}

#[test]
fn set_trap_answers_and_a_falsish_answer_is_a_type_error() {
    eval_js(&format!(
        "{NAME_OF}
         var target = {{}};
         var log = [];
         var p = new Proxy(target, {{
           set: function (t, k, v) {{ log.push(k + '=' + v); t[k] = v * 2; return true; }}
         }});
         p.x = 5;
         if (log.join(',') !== 'x=5') throw new Error('log: ' + log.join(','));
         if (p.x !== 10) throw new Error('value: ' + p.x);
         // A trap that answers falsish is a failed assignment; the engine has
         // no sloppy mode, so it is a TypeError.
         var refusing = new Proxy({{}}, {{ set: function () {{ return false; }} }});
         if (name_of(function () {{ refusing.y = 1; }}) !== 'TypeError') throw new Error('falsish set');"
    ))
    .unwrap();
}

#[test]
fn get_and_set_invariants_of_a_frozen_target() {
    eval_js(&format!(
        "{NAME_OF}
         var frozen = {{}};
         Object.defineProperty(frozen, 'a', {{ value: 1, writable: false, configurable: false }});
         // Reporting a different value would make a frozen target look thawed.
         var lying = new Proxy(frozen, {{ get: function () {{ return 2; }} }});
         if (name_of(function () {{ lying.a; }}) !== 'TypeError') throw new Error('get invariant');
         var honest = new Proxy(frozen, {{ get: function () {{ return 1; }} }});
         if (honest.a !== 1) throw new Error('get invariant false positive');
         var writing = new Proxy(frozen, {{ set: function () {{ return true; }} }});
         if (name_of(function () {{ writing.a = 9; }}) !== 'TypeError') throw new Error('set invariant');"
    ))
    .unwrap();
}

#[test]
fn has_and_delete_property_traps() {
    eval_js(&format!(
        "{NAME_OF}
         var target = {{ a: 1 }};
         var p = new Proxy(target, {{
           has: function (t, k) {{ if (k === 'hidden') return true; return k in t; }}
         }});
         if (!('a' in p)) throw new Error('has a');
         if (!('hidden' in p)) throw new Error('has hidden');
         if ('missing' in p) throw new Error('has missing');
         var d = new Proxy(target, {{
           deleteProperty: function (t, k) {{ if (k === 'a') return false; delete t[k]; return true; }}
         }});
         if ((delete d.a) !== false) throw new Error('delete returned true');
         if (target.a !== 1) throw new Error('target lost a');
         if ((delete d.nope) !== true) throw new Error('delete nope');
         // A non-configurable property cannot be reported as deleted.
         var locked = {{}};
         Object.defineProperty(locked, 'c', {{ value: 3, configurable: false }});
         var cheater = new Proxy(locked, {{ deleteProperty: function () {{ return true; }} }});
         if (name_of(function () {{ delete cheater.c; }}) !== 'TypeError') throw new Error('delete invariant');"
    ))
    .unwrap();
}

#[test]
fn apply_and_construct_traps() {
    eval_js(
        "var seen = [];
         var target = function (a, b) { return a + b; };
         var p = new Proxy(target, {
           apply: function (t, thisArg, args) {
             seen.push(args.join('+'));
             return t.apply(thisArg, args) + 1;
           }
         });
         var applied = p(1, 2);
         if (applied !== 4) throw new Error('apply: ' + applied);
         if (seen.join(';') !== '1+2') throw new Error('args: ' + seen.join(';'));

         function C(x) { this.x = x; }
         var PC = new Proxy(C, {
           construct: function (t, args, newTarget) {
             if (newTarget !== PC) throw new Error('new.target is not the proxy');
             var o = new t(args[0] * 2);
             o.trapped = true;
             return o;
           }
         });
         var PC2 = new Proxy(C, {});
         var made = new PC(21);
         if (made.x !== 42 || made.trapped !== true) throw new Error('construct');
         if (!(made instanceof C)) throw new Error('instanceof');
         // A trap answering a primitive is a TypeError, not a coercion.
         var bad = new Proxy(C, { construct: function () { return 1; } });
         if (name_of_bad() !== 'TypeError') throw new Error('construct primitive');
         function name_of_bad() { try { new bad(1); return 'none'; } catch (e) { return e.name; } }

         // No traps at all: the proxy forwards to the target for both.
         var plain = new Proxy(target, {});
         if (plain(3, 4) !== 7) throw new Error('forward apply');
         if ((new PC2(5)).x !== 5) throw new Error('forward construct');",
    )
    .unwrap();
}

#[test]
fn a_proxy_of_a_function_is_a_function() {
    eval_js(
        "var fn = function () { return 7; };
         var p = new Proxy(fn, {});
         if (typeof p !== 'function') throw new Error('typeof: ' + typeof p);
         if (!p(0)) throw new Error('call');
         var o = new Proxy({}, {});
         if (typeof o !== 'object') throw new Error('typeof object');
         // `Object.prototype.toString` answers the *target's* class.
         if (Object.prototype.toString.call(new Proxy([], {})) !== '[object Array]') {
           throw new Error('class name');
         }
         if (Object.prototype.toString.call(o) !== '[object Object]') throw new Error('class name 2');
         if (!(new Proxy([], {}) instanceof Array)) throw new Error('instanceof');",
    )
    .unwrap();
}

#[test]
fn revocable_proxy_throws_on_every_operation() {
    eval_js(&format!(
        "{NAME_OF}
         var r = Proxy.revocable({{ a: 1 }}, {{}});
         if (typeof r.proxy !== 'object') throw new Error('proxy field');
         if (typeof r.revoke !== 'function') throw new Error('revoke field');
         if (r.proxy.a !== 1) throw new Error('before revoke');
         if (r.revoke() !== undefined) throw new Error('revoke returns undefined');
         // Revoking twice is a no-op, not an error.
         r.revoke();
         var after = [
           name_of(function () {{ r.proxy.a; }}),
           name_of(function () {{ r.proxy.a = 1; }}),
           name_of(function () {{ 'a' in r.proxy; }}),
           name_of(function () {{ delete r.proxy.a; }})
         ];
         if (after.join(',') !== 'TypeError,TypeError,TypeError,TypeError') {{
           throw new Error('after revoke: ' + after.join(','));
         }}
         // The target itself is untouched.
         if (Object.keys(r).join(',') !== 'proxy,revoke') throw new Error('shape');"
    ))
    .unwrap();
}

#[test]
fn constructor_argument_checks_and_shape() {
    eval_js(&format!(
        "{NAME_OF}
         var bad = [
           name_of(function () {{ return new Proxy({{}}, 1); }}),
           name_of(function () {{ return new Proxy(1, {{}}); }}),
           name_of(function () {{ return new Proxy({{}}); }}),
           name_of(function () {{ return Proxy({{}}, {{}}); }})
         ];
         if (bad.join(',') !== 'TypeError,TypeError,TypeError,TypeError') {{
           throw new Error('arguments: ' + bad.join(','));
         }}
         // No `prototype`: a proxy inherits from its target, so there is
         // nothing for instances to inherit from here.
         if (Proxy.prototype !== undefined) throw new Error('Proxy.prototype');
         if (typeof Proxy.revocable !== 'function') throw new Error('revocable');
         if (Proxy.length !== 2) throw new Error('length: ' + Proxy.length);
         if (Proxy.name !== 'Proxy') throw new Error('name: ' + Proxy.name);"
    ))
    .unwrap();
}

#[test]
fn own_keys_trap_feeds_every_key_listing() {
    eval_js(&format!(
        "{NAME_OF}
         var sym = Symbol('s');
         var target = {{ a: 1, b: 2 }};
         var calls = 0;
         var p = new Proxy(target, {{
           ownKeys: function (t) {{ calls++; return ['b', 'a', sym]; }}
         }});
         if (Object.keys(p).join(',') !== 'b,a') throw new Error('keys: ' + Object.keys(p).join(','));
         if (Object.getOwnPropertyNames(p).join(',') !== 'b,a') throw new Error('names');
         if (Object.getOwnPropertySymbols(p).length !== 1) throw new Error('symbols');
         if (Reflect.ownKeys(p).length !== 3) throw new Error('ownKeys');
         if (calls !== 4) throw new Error('trap calls: ' + calls);
         // No trap: the target's own keys, in the target's order.
         var plain = new Proxy(target, {{}});
         if (Object.keys(plain).join(',') !== 'a,b') throw new Error('forward: ' + Object.keys(plain).join(','));
         // Invariants: a non-configurable key cannot be hidden, and a
         // non-extensible target cannot appear to gain one.
         var locked = {{}};
         Object.defineProperty(locked, 'c', {{ value: 1, configurable: false, enumerable: true }});
         var hiding = new Proxy(locked, {{ ownKeys: function () {{ return []; }} }});
         if (name_of(function () {{ Object.keys(hiding); }}) !== 'TypeError') throw new Error('hide');
         var frozen = Object.freeze({{ d: 1 }});
         var growing = new Proxy(frozen, {{ ownKeys: function () {{ return ['d', 'e']; }} }});
         if (name_of(function () {{ Object.keys(growing); }}) !== 'TypeError') throw new Error('grow');
         // The list may only hold strings and symbols.
         var junk = new Proxy(target, {{ ownKeys: function () {{ return [1]; }} }});
         if (name_of(function () {{ Object.keys(junk); }}) !== 'TypeError') throw new Error('junk');"
    ))
    .unwrap();
}

#[test]
fn the_object_and_reflect_traps() {
    eval_js(&format!(
        "{NAME_OF}
         var proto = {{ tag: 'proto' }};
         var target = {{}};
         var p = new Proxy(target, {{
           getPrototypeOf: function () {{ return proto; }},
           setPrototypeOf: function (t, v) {{ return v === null; }},
           // The `isExtensible` trap must agree with the target: `{{}}` is
           // extensible, so it may not answer `false`.
           isExtensible: function () {{ return true; }},
           preventExtensions: function () {{ return false; }},
           getOwnPropertyDescriptor: function (t, k) {{ return {{ value: 7, enumerable: true, configurable: true }}; }},
           defineProperty: function (t, k, d) {{ return k !== 'no'; }}
         }});
         if (Object.getPrototypeOf(p) !== proto) throw new Error('getPrototypeOf');
         if (Reflect.getPrototypeOf(p) !== proto) throw new Error('Reflect.getPrototypeOf');
         if (Object.setPrototypeOf(p, null) !== p) throw new Error('setPrototypeOf');
         if (Reflect.setPrototypeOf(p, proto) !== false) throw new Error('Reflect.setPrototypeOf false');
         if (Object.isExtensible(p) !== true) throw new Error('isExtensible');
         if (Reflect.isExtensible(p) !== true) throw new Error('Reflect.isExtensible');
         if (name_of(function () {{ Object.preventExtensions(p); }}) !== 'TypeError') throw new Error('preventExtensions');
         if (Reflect.preventExtensions(p) !== false) throw new Error('Reflect.preventExtensions');
         if (Object.getOwnPropertyDescriptor(p, 'x').value !== 7) throw new Error('gOPD');
         Object.defineProperty(p, 'yes', {{ value: 1 }});
         if (name_of(function () {{ Object.defineProperty(p, 'no', {{ value: 1 }}); }}) !== 'TypeError') {{
           throw new Error('defineProperty false');
         }}
         // The `isExtensible` trap must agree with the target.
         var lying = new Proxy(Object.freeze({{}}), {{ isExtensible: function () {{ return true; }} }});
         if (name_of(function () {{ Object.isExtensible(lying); }}) !== 'TypeError') throw new Error('isExtensible lie');"
    ))
    .unwrap();
}
