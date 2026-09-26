//! Feature tests for `WeakMap` / `WeakSet` (M5-C2 / B3).
//!
//! Top-level scripts with `throw`-based assertions, as everywhere else. The
//! interesting surface is the *key rule*: only objects and non-registered
//! symbols may be held weakly (`CanBeHeldWeakly`), `set`/`add` reject anything
//! else, and `get`/`has`/`delete` answer instead of raising.

use crate::helpers::eval_js;

#[test]
fn weak_map_round_trip_and_identity() {
    eval_js(
        "var k1 = {};
         var k2 = {};
         var m = new WeakMap();
         m.set(k1, 'a').set(k2, 'b');
         if (m.get(k1) !== 'a' || m.get(k2) !== 'b') throw new Error('get');
         if (!m.has(k1) || m.has({})) throw new Error('identity');
         if (m.delete(k1) !== true || m.delete(k1) !== false) throw new Error('delete');
         if (m.get(k1) !== undefined) throw new Error('deleted key still readable');
         m.set(k1, 'c');
         if (m.get(k1) !== 'c') throw new Error('re-insert');",
    )
    .unwrap();
}

#[test]
fn weak_map_rejects_primitives_for_set_only() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         var m = new WeakMap();
         var rejected = [
           name_of(function () { m.set(1, 'v'); }),
           name_of(function () { m.set('k', 'v'); }),
           name_of(function () { m.set(null, 'v'); }),
           name_of(function () { m.set(Symbol.for('registered'), 'v'); })
         ];
         if (rejected.join(',') !== 'TypeError,TypeError,TypeError,TypeError') {
           throw new Error('set: ' + rejected.join(','));
         }
         // get / has / delete must be quiet instead.
         if (m.get(1) !== undefined || m.has('x') || m.delete(null)) throw new Error('quiet answers');",
    )
    .unwrap();
}

#[test]
fn weak_set_round_trip_and_primitive_rule() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         var v = {};
         var w = {};
         var s = new WeakSet();
         s.add(v).add(v);
         if (!s.has(v) || s.has(w)) throw new Error('add/has');
         if (s.delete(v) !== true || s.has(v)) throw new Error('delete');
         if (name_of(function () { s.add(1); }) !== 'TypeError') throw new Error('add 1');
         if (s.has('x') !== false || s.delete(null) !== false) throw new Error('quiet answers');",
    )
    .unwrap();
}

#[test]
fn symbols_may_be_held_weakly_unless_registered() {
    eval_js(
        "var local = Symbol('local');
         var m = new WeakMap();
         m.set(local, 'v');
         if (m.get(local) !== 'v' || !m.has(local)) throw new Error('non-registered symbol key');
         var s = new WeakSet();
         s.add(local);
         if (!s.has(local)) throw new Error('non-registered symbol value');",
    )
    .unwrap();
}

#[test]
fn constructors_consume_an_iterable() {
    eval_js(
        "var k = {};
         var m = new WeakMap([[k, 7]]);
         var s = new WeakSet([k]);
         if (m.get(k) !== 7 || !s.has(k)) throw new Error('iterable constructor');
         function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         if (name_of(function () { WeakMap(); }) !== 'TypeError') throw new Error('WeakMap()');
         if (name_of(function () { WeakSet(); }) !== 'TypeError') throw new Error('WeakSet()');",
    )
    .unwrap();
}

#[test]
fn shape_matches_the_spec_subset() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         var m = new WeakMap();
         var s = new WeakSet();
         // No size, no iteration, no clear.
         if (WeakMap.prototype.size !== undefined) throw new Error('WeakMap size');
         if (WeakSet.prototype.size !== undefined) throw new Error('WeakSet size');
         if (WeakMap.prototype[Symbol.iterator] !== undefined) throw new Error('WeakMap iterator');
         if (WeakSet.prototype[Symbol.iterator] !== undefined) throw new Error('WeakSet iterator');
         if (WeakMap.prototype.clear !== undefined || WeakSet.prototype.clear !== undefined) {
           throw new Error('clear');
         }
         // Receiver checks are exact, and the names survive the dispatch prefix.
         var bad = [
           name_of(function () { WeakMap.prototype.get.call({}, {}); }),
           name_of(function () { WeakMap.prototype.set.call(new Set(), {}, 1); }),
           name_of(function () { WeakSet.prototype.add.call(new Map(), {}); })
         ];
         if (bad.join(',') !== 'TypeError,TypeError,TypeError') throw new Error('receivers');
         if (WeakMap.prototype.set.name !== 'set' || WeakSet.prototype.add.name !== 'add') {
           throw new Error('names');
         }
         if (Object.prototype.toString.call(m) !== '[object WeakMap]') throw new Error('tag');
         if (Object.prototype.toString.call(s) !== '[object WeakSet]') throw new Error('tag');
         if (!(m instanceof WeakMap) || !(s instanceof WeakSet)) throw new Error('instanceof');
         if (m.constructor !== WeakMap || s.constructor !== WeakSet) throw new Error('constructor');",
    )
    .unwrap();
}
