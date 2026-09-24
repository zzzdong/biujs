//! `ArraySpeciesCreate` (ES 9.4.2.3) and the methods that use it.
//!
//! `map` / `filter` / `slice` / `splice` / `concat` are specified as building
//! their result through `ArraySpeciesCreate`, which reads `O.constructor` and
//! `C[Symbol.species]` through real `[[Get]]`s and then `Construct`s. Those two
//! steps can run user code, which is why the whole operation lives in the VM.

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn array_has_species_accessor() {
    // `Array[Symbol.species]` is an accessor returning its receiver, with the
    // attributes every built-in shares (non-enumerable, configurable).
    assert_eq!(
        eval_string(
            "var d = Object.getOwnPropertyDescriptor(Array, Symbol.species);
             var out = [];
             out.push(typeof d.get);
             out.push(d.get.name);
             out.push(d.get.length);
             out.push(d.enumerable);
             out.push(d.configurable);
             out.push(d.get.call(Array) === Array);
             out.join(',')"
        ),
        "function,get [Symbol.species],0,false,true,true"
    );
    // The well-known symbol itself is registered and globally shared.
    assert!(eval_bool("Symbol.species === Symbol.species;"));
}

#[test]
fn map_uses_species_constructor() {
    assert_eq!(
        eval_string(
            "var calls = 0, seenThis = null, seenArgs = null, instance = [];
             var Ctor = function() {
               calls += 1;
               seenThis = this;
               seenArgs = arguments;
               return instance;
             };
             var a = [1, 2, 3, 4, 5];
             a.constructor = {};
             a.constructor[Symbol.species] = Ctor;
             var result = a.map(function(x) { return x * 2; });
             [calls, seenArgs.length, seenArgs[0], result === instance,
              Object.getPrototypeOf(seenThis) === Ctor.prototype,
              instance.length, instance[0]].join(',')"
        ),
        "1,1,5,true,true,5,2"
    );
}

#[test]
fn species_lookup_runs_before_the_algorithm() {
    // A poisoned `@@species` getter is abrupt, and the callback must not have
    // run by then.
    assert_eq!(
        eval_string(
            "var calls = 0;
             var a = [1, 2];
             a.constructor = {};
             Object.defineProperty(a.constructor, Symbol.species, {
               get: function() { throw new TypeError('poisoned'); }
             });
             var caught = 'none';
             try { a.map(function() { calls += 1; }); } catch (e) { caught = e.message; }
             caught + ',' + calls"
        ),
        "poisoned,0"
    );
}

#[test]
fn nullish_species_and_non_array_receiver_fall_back() {
    // `@@species` = null → ArrayCreate; a non-array receiver never consults
    // species at all.
    assert_eq!(
        eval_number(
            "var a = [1, 2, 3];
             a.constructor = {};
             a.constructor[Symbol.species] = null;
             a.filter(function() { return true; }).length"
        ),
        3.0
    );
    assert_eq!(
        eval_number(
            "var calls = 0;
             var Ctor = function() { calls += 1; return []; };
             var arrayLike = { length: 2, 0: 'a', 1: 'b', constructor: {} };
             arrayLike.constructor[Symbol.species] = Ctor;
             Array.prototype.filter.call(arrayLike, function() { return true; }).length
             + (calls === 0 ? 0 : 100)"
        ),
        2.0
    );
}

#[test]
fn non_constructible_species_throws() {
    assert_eq!(
        eval_string(
            "var a = [1, 2];
             a.constructor = {};
             a.constructor[Symbol.species] = 42;
             var n = 'none';
             try { a.slice(0); } catch (e) { n = e.name; }
             n"
        ),
        "TypeError"
    );
}

#[test]
fn result_writes_are_abrupt_when_the_target_blocks_them() {
    // CreateDataPropertyOrThrow: a non-extensible target makes every copy
    // method raise instead of quietly dropping elements.
    assert_eq!(
        eval_string(
            "var Blocked = function() { Object.preventExtensions(this); };
             var a = [1, 2];
             a.constructor = {};
             a.constructor[Symbol.species] = Blocked;
             var out = [];
             ['slice', 'concat', 'map', 'filter'].forEach(function(m) {
               var n = 'none';
               try { a[m](function() { return true; }); } catch (e) { n = e.name; }
               out.push(m + ':' + n);
             });
             out.join(',')"
        ),
        "slice:TypeError,concat:TypeError,map:TypeError,filter:TypeError"
    );
}

#[test]
fn species_target_receives_the_elements() {
    assert_eq!(
        eval_string(
            "var Custom = function() { this.made = true; };
             var a = [1, 2, 3];
             a.constructor = {};
             a.constructor[Symbol.species] = Custom;
             var sliced = a.slice(1);
             var mapped = a.map(function(x) { return x * 2; });
             var filtered = a.filter(function(x) { return x !== 2; });
             var concat = a.concat(4);
             var spliced = a.splice(1, 1);
             [sliced.made, mapped.made, filtered.made, concat.made, spliced.made,
              sliced[0], mapped[0], filtered[0], concat[3], spliced[0],
              a.join('')].join(',')"
        ),
        "true,true,true,true,true,2,2,1,4,2,13"
    );
}

#[test]
fn default_path_still_produces_holey_arrays() {
    // No species override ⇒ the ordinary array, and `map` keeps `length` and
    // holes instead of compacting them.
    assert_eq!(
        eval_string(
            "var a = [1, 2, 3];
             delete a[1];
             var m = a.map(function(x) { return x; });
             var f = a.filter(function() { return true; });
             [m.length, (1 in m), f.length, f.join(',')].join(',')"
        ),
        "3,false,2,1,3"
    );
}
