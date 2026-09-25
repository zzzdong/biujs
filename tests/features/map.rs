//! Feature tests for `Map` (M5-C1 / B2a): insertion order, `SameValueZero`
//! keys, live iterators, the iterable constructor and the receiver checks.
//!
//! Assertions are written as `throw` in the JS source and run through
//! `eval_js(..).unwrap()`: the script completion value is not reliable (see
//! `architecture.md` §4), and a panic on failure would not say *what* broke.
//! Scripts stay at the top level on purpose — a closure that assigns to an
//! outer variable does not see it in this engine (capture is a value snapshot
//! at creation time), so callbacks only ever `push` into a captured array.

use crate::helpers::eval_js;

#[test]
fn entries_keep_insertion_order_and_upsert_in_place() {
    eval_js(
        "var m = new Map();
         m.set('z', 1).set('a', 2).set('m', 3);
         m.set('a', 9);
         if (m.size !== 3) throw new Error('size ' + m.size);
         if (m.get('a') !== 9) throw new Error('upsert lost the value');
         var order = [];
         var it = m.keys();
         var step = it.next();
         while (!step.done) { order.push(step.value); step = it.next(); }
         if (order.join(',') !== 'z,a,m') throw new Error('order ' + order.join(','));
         if (m.delete('z') !== true || m.delete('z') !== false) throw new Error('delete');
         if (m.has('z')) throw new Error('deleted key still present');
         m.clear();
         if (m.size !== 0) throw new Error('clear');",
    )
    .unwrap();
}

#[test]
fn keys_use_same_value_zero() {
    eval_js(
        "var m = new Map();
         m.set(-0, 'zero');
         if (m.get(0) !== 'zero' || m.size !== 1) throw new Error('-0 is not +0');
         m.set(NaN, 'nan');
         m.set(NaN, 'nan2');
         if (m.size !== 2) throw new Error('NaN was stored twice');
         if (m.get(NaN) !== 'nan2') throw new Error('NaN lookup');
         var o = {};
         m.set(o, 'object');
         if (m.get(o) !== 'object' || m.get({}) !== undefined) throw new Error('identity');",
    )
    .unwrap();
}

#[test]
fn iterators_are_live_and_entries_are_pairs() {
    eval_js(
        "var m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]);
         var seen = [];
         var it = m.entries();
         seen.push(it.next().value[0]);
         m.delete(2);
         seen.push(it.next().value[0]);
         var done = it.next();
         if (seen.join(',') !== '1,3' || done.done !== true) {
           throw new Error('delete during iteration: ' + seen.join(','));
         }
         // An entry added while iterating is visited too.
         var m2 = new Map([[1, 'a']]);
         var it2 = m2.keys();
         var seen2 = [];
         seen2.push(it2.next().value);
         m2.set(2, 'b');
         var next2 = it2.next();
         seen2.push(next2.value);
         if (seen2.join(',') !== '1,2' || next2.done) throw new Error('add during iteration');
         // `keys` / `values` project the same live entries.
         var vals = [];
         var it3 = m2.values();
         var s3 = it3.next();
         while (!s3.done) { vals.push(s3.value); s3 = it3.next(); }
         if (vals.join(',') !== 'a,b') throw new Error('values ' + vals.join(','));",
    )
    .unwrap();
}

#[test]
fn for_each_passes_value_key_and_map() {
    eval_js(
        "var m = new Map([['k', 'v'], ['k2', 'v2']]);
         var seen = [];
         m.forEach(function (value, key, self) {
           seen.push(key + '=' + value + (self === m ? '!' : '?'));
         });
         if (seen.join(',') !== 'k=v!,k2=v2!') throw new Error('forEach ' + seen.join(','));
         var bad = 'none';
         try { m.forEach(null); } catch (e) { bad = e.name; }
         if (bad !== 'TypeError') throw new Error('non-callable callback: ' + bad);",
    )
    .unwrap();
}

#[test]
fn constructor_consumes_the_iteration_protocol() {
    eval_js(
        // The iterator drains a captured *array*: neither a captured scalar
        // (`i += 1`) nor a property of a captured object (`state.i += 1`) is
        // writable from inside a closure in this engine — closures capture a
        // value snapshot — but mutating a captured array works
        // (`architecture.md` §4 / the M7-P5 debt).
        "var calls = [];
         var pending = [['a', 1], ['b', 2]];
         var iterable = {};
         iterable[Symbol.iterator] = function () {
           return {
             next: function () {
               if (pending.length === 0) return { value: undefined, done: true };
               return { value: pending.shift(), done: false };
             }
           };
         };
         var m = new Map(iterable);
         if (m.size !== 2 || m.get('a') !== 1 || m.get('b') !== 2) throw new Error('custom iterable');
         // The adder is looked up through [[Get]], so a patched `set` is used.
         var patched = new Map();
         var original = Map.prototype.set;
         Map.prototype.set = function (k, v) { calls.push(k); return original.call(this, k, v); };
         try { new Map([['x', 1]]); } finally { Map.prototype.set = original; }
         if (calls.join(',') !== 'x') throw new Error('set was not used: ' + calls.join(','));
         var threw = 'none';
         try { new Map([1]); } catch (e) { threw = e.name; }
         if (threw !== 'TypeError') throw new Error('primitive item: ' + threw);",
    )
    .unwrap();
}

#[test]
fn receiver_checks_and_metadata() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         var out = [
           name_of(function () { Map(); }),
           name_of(function () { Map.prototype.get.call({}, 'x'); }),
           name_of(function () { Map.prototype.keys.call(1); }),
           name_of(function () { Map.prototype.forEach.call([], function () {}); })
         ];
         if (out.join(',') !== 'TypeError,TypeError,TypeError,TypeError') {
           throw new Error('receivers: ' + out.join(','));
         }
         var m = new Map();
         if (Object.getPrototypeOf(m) !== Map.prototype) throw new Error('prototype');
         if (m.constructor !== Map) throw new Error('constructor');
         if (Object.prototype.toString.call(m) !== '[object Map]') throw new Error('class tag');
         var size = Object.getOwnPropertyDescriptor(Map.prototype, 'size');
         if (typeof size.get !== 'function' || size.set !== undefined) throw new Error('size is not a getter');
         if (size.get.name !== 'get size') throw new Error('getter name ' + size.get.name);
         if (Map.length !== 0 || Map.prototype.set.length !== 2 || Map.prototype.get.length !== 1) {
           throw new Error('arity');
         }
         if (Map.prototype[Symbol.iterator] !== Map.prototype.entries) throw new Error('@@iterator');",
    )
    .unwrap();
}

#[test]
fn get_or_insert_and_group_by() {
    eval_js(
        "var m = new Map();
         if (m.getOrInsert('k', 1) !== 1) throw new Error('insert');
         if (m.getOrInsert('k', 2) !== 1) throw new Error('existing value must win');
         if (m.size !== 1) throw new Error('size');
         if (m.getOrInsertComputed('c', function (key) { return key + '!'; }) !== 'c!') {
           throw new Error('computed');
         }
         if (m.getOrInsertComputed('c', function () { return 'no'; }) !== 'c!') {
           throw new Error('computed must not rerun');
         }
         var bad = 'none';
         try { m.getOrInsertComputed('z', 1); } catch (e) { bad = e.name; }
         if (bad !== 'TypeError') throw new Error('non-callable callback: ' + bad);
         var groups = Map.groupBy([1, 2, 3, 4], function (v, i) { return v % 2 === 0 ? 'e' : 'o'; });
         if (groups.get('o').join(',') !== '1,3' || groups.get('e').join(',') !== '2,4') {
           throw new Error('groupBy');
         }
         if (!(groups instanceof Map)) throw new Error('groupBy must return a Map');",
    )
    .unwrap();
}
