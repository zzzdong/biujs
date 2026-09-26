//! Feature tests for `Set` (M5-C1 / B2b): insertion order, `SameValueZero`
//! values, live iterators, the iterable constructor, the receiver checks and
//! the `set-methods` operators (ES2024).
//!
//! Same discipline as `features/map.rs`: `throw`-based assertions run through
//! `eval_js(..).unwrap()` (the script completion value is not reliable), and
//! everything stays at the top level because a closure that *assigns* to an
//! outer variable does not see it in this engine — callbacks may only mutate a
//! captured object (`push`, `Set.prototype.add`, …).

use crate::helpers::eval_js;

#[test]
fn add_has_size_delete_clear_with_same_value_zero() {
    eval_js(
        "var s = new Set();
         s.add(1).add(2).add(1);
         if (s.size !== 2) throw new Error('size ' + s.size);
         if (!s.has(1) || s.has(3)) throw new Error('has');
         s.delete(1);
         if (s.size !== 1 || s.has(1)) throw new Error('delete');
         s.clear();
         if (s.size !== 0) throw new Error('clear');
         var z = new Set();
         z.add(-0);
         if (!z.has(0) || z.size !== 1) throw new Error('-0 must be +0');
         z.add(NaN);
         z.add(NaN);
         if (z.size !== 2 || !z.has(NaN)) throw new Error('NaN is a single value');
         var o = {};
         z.add(o);
         if (!z.has(o) || z.has({})) throw new Error('identity');",
    )
    .unwrap();
}

#[test]
fn iterators_are_live_and_exhaustion_is_permanent() {
    eval_js(
        "var s = new Set([1, 2, 3]);
         var it = s.values();
         var seen = [];
         seen.push(it.next().value);
         s.delete(2);
         seen.push(it.next().value);
         var done = it.next();
         if (seen.join(',') !== '1,3' || done.done !== true) {
           throw new Error('delete during iteration: ' + seen.join(','));
         }
         s.add(4);
         var after = it.next();
         if (after.done !== true || after.value !== undefined) {
           throw new Error('an exhausted iterator must stay done');
         }
         // An entry added while the walk is live *is* visited.
         var s2 = new Set([1]);
         var it2 = s2.values();
         var first = it2.next().value;
         s2.add(2);
         var second = it2.next();
         if (first !== 1 || second.value !== 2 || second.done) throw new Error('add during iteration');",
    )
    .unwrap();
}

#[test]
fn constructor_uses_the_iterator_protocol_and_its_adder() {
    eval_js(
        "var s = new Set([1, 2, 2, 3]);
         if (s.size !== 3) throw new Error('dedup ' + s.size);
         // `new Set(x)` uses x's own iterator, not an array-like snapshot.
         var pending = ['a', 'b'];
         var iterable = {};
         iterable[Symbol.iterator] = function () {
           return {
             next: function () {
               if (pending.length === 0) return { value: undefined, done: true };
               return { value: pending.shift(), done: false };
             }
           };
         };
         var fromIter = new Set(iterable);
         if (fromIter.size !== 2 || !fromIter.has('a') || !fromIter.has('b')) {
           throw new Error('custom iterable');
         }
         var threw = 'none';
         try { Set(); } catch (e) { threw = e.name; }
         if (threw !== 'TypeError') throw new Error('Set() without new: ' + threw);",
    )
    .unwrap();
}

#[test]
fn prototype_shape_and_receiver_checks() {
    eval_js(
        "function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         var s = new Set();
         var out = [
           name_of(function () { Set.prototype.add.call({}, 1); }),
           name_of(function () { Set.prototype.has.call(1, 1); }),
           name_of(function () { Set.prototype.forEach.call([], function () {}); }),
           name_of(function () { Set.prototype.delete.call(new Map(), 1); }),
           name_of(function () { Map.prototype.has.call(new Set(), 1); })
         ];
         if (out.join(',') !== 'TypeError,TypeError,TypeError,TypeError,TypeError') {
           throw new Error('receivers: ' + out.join(','));
         }
         if (Object.getPrototypeOf(s) !== Set.prototype) throw new Error('prototype');
         if (s.constructor !== Set) throw new Error('constructor');
         if (Object.prototype.toString.call(s) !== '[object Set]') throw new Error('class tag');
         if (Set.prototype.keys !== Set.prototype.values) throw new Error('keys === values');
         if (Set.prototype[Symbol.iterator] !== Set.prototype.values) throw new Error('@@iterator');
         var size = Object.getOwnPropertyDescriptor(Set.prototype, 'size');
         if (size.get.name !== 'get size' || size.set !== undefined) throw new Error('size getter');
         if (Set.length !== 0 || Set.prototype.add.length !== 1 || Set.prototype.has.length !== 1) {
           throw new Error('arity');
         }",
    )
    .unwrap();
}

#[test]
fn for_each_passes_the_value_twice() {
    eval_js(
        "var s = new Set(['a', 'b']);
         var seen = [];
         s.forEach(function (value, key, self) {
           seen.push(value + '/' + key + '/' + (self === s ? '=' : '!'));
         });
         if (seen.join(',') !== 'a/a/=,b/b/=') throw new Error('forEach ' + seen.join(','));
         var threw = 'none';
         try { s.forEach(1); } catch (e) { threw = e.name; }
         if (threw !== 'TypeError') throw new Error('non-callable: ' + threw);",
    )
    .unwrap();
}

#[test]
fn set_methods_operate_on_set_like_arguments() {
    eval_js(
        // Spread, not a `forEach` helper: a callback that pushes into an array
        // captured by an enclosing helper function does not work here yet (the
        // closure-capture debt, M7-P5).
        "var a = new Set([1, 2]);
         var b = new Set([2, 3]);
         if ([...a.union(b)].join(',') !== '1,2,3') throw new Error('union ' + [...a.union(b)].join(','));
         if ([...new Set([2, 3]).union(new Set([1, 2]))].join(',') !== '2,3,1') throw new Error('union order');
         if ([...a.intersection(b)].join(',') !== '2') throw new Error('intersection');
         if ([...a.difference(b)].join(',') !== '1') throw new Error('difference');
         if ([...a.symmetricDifference(b)].join(',') !== '1,3') throw new Error('symmetricDifference');
         if (!a.isSubsetOf(new Set([1, 2, 3]))) throw new Error('isSubsetOf');
         if (a.isSubsetOf(b)) throw new Error('isSubsetOf false');
         if (!new Set([1, 2, 3]).isSupersetOf(a)) throw new Error('isSupersetOf');
         if (!a.isDisjointFrom(new Set([3, 4]))) throw new Error('isDisjointFrom');
         if (a.isDisjointFrom(b)) throw new Error('isDisjointFrom false');
         if (a.union(b) === a) throw new Error('results are fresh sets');
         // A *set-like* object only needs `size`/`has`/`keys`.
         var setLike = {
           size: 2,
           has: function (v) { return v === 3 || v === 4; },
           keys: function () { return [3, 4][Symbol.iterator](); }
         };
         if ([...a.union(setLike)].join(',') !== '1,2,3,4') throw new Error('set-like union');
         if (!a.isDisjointFrom(setLike)) throw new Error('set-like isDisjointFrom');
         if (a.isSubsetOf(setLike)) throw new Error('set-like isSubsetOf');
         function name_of(fn) { try { fn(); return 'none'; } catch (e) { return e.name; } }
         var bad = [
           name_of(function () { a.union(1); }),
           name_of(function () { a.union({ size: undefined, has: function () {}, keys: function () {} }); }),
           name_of(function () { a.union({ size: 1, has: 1, keys: function () {} }); }),
           name_of(function () { a.union({ size: 1, has: function () {}, keys: 1 }); })
         ];
         if (bad.join(',') !== 'TypeError,TypeError,TypeError,TypeError') {
           throw new Error('GetSetRecord: ' + bad.join(','));
         }",
    )
    .unwrap();
}
