# ES6 Feature Support Status

> **Last updated**: 2026-09-22
> **Engine version**: 0.1.0
> **Total tests**: 190 unit + 28 feature files (359 assertions passing of 364) + test262 10552 executed / **7677 passing** (72.75%; was 4611 at the M3-B1 second batch) — per-suite table in `docs/es6-conformance-plan.md` §2.1m
>
> Roadmap: see `docs/es6-conformance-plan.md` (M2' done → M3 mostly done: B1/B2/B3/B4/B5/B6-`JSON` delivered; `Date` open).

## Project Goal

**biujs** is a JavaScript engine implemented in Rust, targeting **strict mode ES6** features.

### Scope
- ✅ **Strict mode only** - All code runs in strict mode by default
- ❌ **No `eval()`** - Not supported (incompatible with static compilation)
- ❌ **No `with` statement** - Not supported (incompatible with static compilation)
- ✅ **`arguments` object** - Supported (materialized per frame from the argument count)
- ✅ **`var`** - Supported (function-scoped binding alongside `let`/`const`)

### Design Philosophy
This engine uses a **static compilation model** with register-based VM:
- Static frame indexing replaces dynamic scope chain lookups
- Closure capture uses **create-time value snapshot** semantics — captured variables (including Objects via `Rc` sharing) are copied at closure creation, not referenced via a scope chain. This is a deliberate simplification compatible with the static model.
- Features incompatible with this architecture are excluded:
  - `eval()` - dynamic code evaluation breaks static analysis
  - `with` - dynamic property lookup breaks static binding

---

## Legend

| Icon | Meaning |
|------|---------|
| ✅ | Full support |
| ⚠️ | Partial / basic support |
| 🚧 | In progress |
| ❌ | Not implemented |

---

## 1. Basic Language Features

### Literals

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Numeric literals | ✅ | M0 | f64 unified |
| String literals | ✅ | M0 | Single/double quotes |
| Boolean literals | ✅ | M0 | `true` / `false` |
| Null literal | ✅ | M0 | `null` |
| Array literals | ✅ | recent | `[1, 2, 3]` |
| Object literals | ✅ | recent | `{a: 1, b: 2}` |
| Template literals | ⚠️ | M1 | Interpolation (`${expr}`) works; tagged templates are basic (no frozen `raw`) |

### Variables

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `let` declaration | ✅ | M1 | |
| `const` declaration | ✅ | M1 | Block-scoped, re-assignment check enforced |
| Identifier reference | ✅ | M0 | |
| Assignment (`=`) | ✅ | M1 | |
| Compound assignment (`+=`, `-=`, etc.) | ✅ | M1 | `+=`, `-=`, `*=`, `/=`, `%=` |

### Types & typeof

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Undefined | ✅ | M0 | |
| Null | ✅ | M0 | `typeof null` → `"object"` |
| Boolean | ✅ | M0 | |
| Number (f64) | ✅ | M0 | |
| String | ✅ | M0 | Rc<String> |
| Symbol | ✅ | M1 | Constructor implemented, `Symbol.for()` basic support |
| Object | ✅ | recent | `Rc<RefCell<dyn JSObject>>` |
| `typeof` operator | ✅ | M1 | |

---

## 2. Expressions

### Arithmetic

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `+` (addition) | ✅ | M0 | Also string concatenation |
| `-` (subtraction) | ✅ | M0 | |
| `*` (multiplication) | ✅ | M0 | |
| `/` (division) | ✅ | M0 | |
| `%` (remainder) | ✅ | M0 | |
| `++` (increment) | ✅ | M1 / M3 | Prefix & postfix; operates on `ToNumeric(old)` and the postfix form yields that converted value (`var s = "5"; s++` → `5`, `s` → `6`) |
| `--` (decrement) | ✅ | M1 / M3 | Same as `++` |
| Unary `+` | ✅ | M1 / M3 | `+expr` → `ToNumber` opcode (it used to lower to `0 + expr`, which concatenated strings: `+"5"` was `"05"`) |
| Unary `-` | ✅ | M0 | |
| Unary `!` | ✅ | M1 | |
| Unary `~` | ✅ | M1 | Bitwise NOT (32-bit signed integer) |

### Comparison

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `==` (abstract) | ✅ | M1 | Full ToPrimitive + type coercion |
| `!=` (abstract) | ✅ | M1 | |
| `===` (strict) | ✅ | M1 | |
| `!==` (strict) | ✅ | M1 | |
| `<`, `<=`, `>`, `>=` | ✅ | M1 | Abstract relational comparison |

### Logical & Bitwise

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `&&` | ✅ | M1 | Short-circuit |
| `\|\|` | ✅ | M1 | Short-circuit |
| `??` | ✅ | M2' | Nullish short-circuit (`0 ?? x` is `0`) |
| `**` / `**=` | ✅ | M2' | Right-associative; unary LHS requires parentheses (parser-enforced) |
| `!` | ✅ | M1 | |
| `&`, `\|`, `^`, `<<`, `>>`, `>>>` | ⚠️ | test262 | Parsed + VM, lowering may be simplified |

### Property Access

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Dot access (`obj.prop`) | ✅ | recent | Static property name |
| Bracket access (`obj[expr]`) | ✅ | recent | Dynamic property name |
| Method call (`obj.method()`) | ✅ | recent | Static + dynamic method names |
| Prototype chain lookup | ✅ | recent | `[[Get]]` via `internal_get` |
| `in` operator | ✅ | M2' | Prototype-chain `[[HasProperty]]` |
| `instanceof` | ✅ | recent | Prototype chain traversal (`Symbol.hasInstance`: M2' residual) |

### Function Expressions

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Function declaration | ✅ | recent | Hoisting, recursion, nesting |
| Function expression | ✅ | recent | Anonymous, named |
| Arrow functions | ✅ | M1 | Expression/block body, lexical `this` capture |
| IIFE | ⚠️ | recent | Works if no `this` reference |

### Other Expressions

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Parenthesized | ✅ | M0 | |
| Conditional (`?:`) | ✅ | M1 | |
| Sequence (`,`) | ✅ | M1 | |
| `new` expression | ✅ | recent | Constructor calls with `this` binding |
| `this` | ✅ | recent | Strict mode only |
| `delete` | ✅ | M2' | Honours `[[Configurable]]` |
| `void` | ✅ | M1 | Returns `undefined` |

---

## 3. Statements

### Declarations

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `let` | ✅ | M1 | |
| Function declaration | ✅ | recent | Hoisting supported |

### Control Flow

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Block statement | ✅ | M1 | |
| `if` / `else` | ✅ | M1 | |
| `while` | ✅ | M1 | |
| `for` | ✅ | M1 | |
| `break` | ✅ | M1 | Loop context |
| `continue` | ✅ | M1 | Loop context |
| `return` | ✅ | M1 | In functions |

### Exception Handling

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `throw` | ✅ | M1 | SEH wired |
| `try` / `catch` | ✅ | M1 | Basic support |
| `finally` | ✅ | M1 | Full support (try-catch-finally, try-finally, nested) |
| Error types | ✅ | recent | Error, TypeError, ReferenceError, RangeError, URIError, EvalError |

---

## 4. Objects & Prototypes

### Core Object Features

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Object literals | ✅ | recent | |
| Property read/write | ✅ | recent | |
| Dynamic property access | ✅ | recent | Bracket notation |
| Property deletion | ✅ | M2' | Honours `[[Configurable]]` |
| Property order | ✅ | M2' | `OrdinaryOwnPropertyKeys`: indices ascending, then strings/symbols in creation order |
| Computed property names | ✅ | M2' | Object literals and class members, via `Object.defineProperty` |
| `Object.prototype.toString` | ✅ | recent | Returns `[object ClassName]` |
| `valueOf()` | ✅ | recent | Returns self |
| Prototype chain | ✅ | recent | Instances linked via `Builtins` |

### Object.prototype Methods

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `.toString()` | ✅ | recent / M3 | `[object Class]`; error instances report `[object Error]` and stringify through `Error.prototype.toString` |
| `.valueOf()` | ✅ | recent | |
| `.hasOwnProperty()` / `.isPrototypeOf()` / `.propertyIsEnumerable()` / `.toLocaleString()` | ✅ | M3 | Method attributes `{writable:true, enumerable:false, configurable:true}` |


### Static Object Methods

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `Object.defineProperty()` | ✅ | M2' | Full descriptor on every object kind; partial descriptors merge with the existing property |
| `Object.getOwnPropertyDescriptor()` | ⚠️ | recent | Basic implementation (symbol keys and bare-function receivers pending M3) |
| `Object.freeze()` | 🚧 | — | Object trait has methods |
| `Object.keys/values/entries` | ✅ | M2' | Own enumerable string keys only |

---

## 5. Arrays

### Array Features

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Array literals | ✅ | recent | |
| Element access (numeric index) | ✅ | recent | |
| Element set | ✅ | recent | |
| `.length` property | ✅ | recent | Readable, settable |
| Sparse arrays | ⚠️ | recent | `arr[5] = 1` extends |
| `typeof []` → `"object"` | ✅ | recent | |

### Array.prototype Methods

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `.toString()` | ✅ | recent | Elements joined by `,` |
| `.valueOf()` | ✅ | recent | |
| `.push()` / `.pop()` / `.shift()` / `.unshift()` | ✅ | recent | Fast array path; generic (array-like) receivers still `TypeError` for the mutating ones (open item of B2) |
| `.indexOf()` / `.lastIndexOf()` / `.includes()` / `.join()` / `.slice()` / `.concat()` / `.splice()` / `.reverse()` / `.fill()` | ✅ | recent / M3 | `indexOf`/`lastIndexOf` are hole-aware and fall back to `String.prototype` semantics for string receivers (M3-B2 first batch) |
| `.map()` / `.filter()` / `.forEach()` / `.some()` / `.every()` / `.reduce()` / `.reduceRight()` / `.find()` / `.findIndex()` / `.sort()` | ✅ | M3 | VM-side generic + hole-aware (`Array.prototype.map.call({length:2, 0:'a'}, …)`); `thisArg` is passed |
| `.at()` / `.copyWithin()` | ✅ | M3 | `at` supports negative indices and array-likes; `copyWithin` snapshots the source range, so overlapping copies match the spec |
| `.entries()` / `.keys()` / `.values()` | ✅ | M3 | Real iterator objects (VM iterator registry); the iterators are themselves iterable |
| `Array.from()` / `Array.of()` / `Array.isArray()` | ✅ | recent | `length = 1 / 0 / 1` |
| `Array.prototype[Symbol.iterator]` | ✅ | M1 / M3 | Non-enumerable, `length = 0`, `name === "[Symbol.iterator]"` |
| `Array` constructor | ✅ | M3 | `Array instanceof Function`, `Object.getPrototypeOf(Array) === Function.prototype`, `Array.prototype` is read-only/non-configurable, `new Array(...spread)` keeps argument order |

---

## 6. Functions & Closures

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Function declaration | ✅ | recent | |
| Function expression | ✅ | recent | |
| Return statement | ✅ | M1 | |
| Arguments | ✅ | recent | Named params |
| Recursion | ✅ | recent | Name registered in own scope |
| Nested functions | ✅ | recent | Function hoisting inside function |
| Arrow functions | ✅ | M1 | Lexical `this` capture + closure variable capture (value snapshot) |
| `this` binding | ✅ | recent | Strict mode only |

---

## 7. ES6 Classes

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `class` syntax | ✅ | recent | Class declarations and expressions |
| `constructor` | ✅ | recent | Default and custom constructors; `new` is required (plain call throws TypeError) |
| Methods | ✅ | recent | Instance methods on prototype |
| Static methods | ✅ | M2 | Defined on the constructor |
| Static fields (`static x = 1`) | ✅ | M2 | Evaluated once when the class is built |
| Instance fields (`x = 1`) | ✅ | M2 | Initialized at the start of the constructor |
| Getters / setters | ✅ | M2 | One merged `Object.defineProperty` descriptor per name |
| `prototype.constructor` back-reference | ✅ | M2 | |
| `extends` | ✅ | M2 | Prototype created with `Object.create(parent.prototype)`; static inheritance via `setPrototypeOf` |
| `super()` | ✅ | M2' | Resolves the running constructor's own parent; propagates `new.target` |
| `super.method()` / `super.prop` | ✅ | M2' | Home object recorded per member at class definition, so it is correct at any depth and inside arrows |
| `new.target` | ✅ | M2' | Per frame: the constructor under `new`, `undefined` otherwise; inherited by arrows; propagated through `super()` |
| Computed method names | ✅ | M2' | `[expr]() {}`, computed accessors and statics; keys evaluated in source order |
| Private fields / methods (`#x`) | ❌ | — | Out of scope (ES2022) |
| Static blocks | ❌ | — | Out of scope (ES2022) |

### ES6 Syntax Delivered Elsewhere

| Feature | Status | Notes |
|---------|--------|-------|
| `for...of` / `for...in` | ✅ | Dual-path iterator protocol |
| Iterators (`Symbol.iterator`) | ⚠️ | Fast path for arrays/strings; user iterators via protocol; `return()`/`throw()` incomplete |
| Destructuring (array/object/params/assignment targets) | ✅ | Defaults, elision, rest, nesting |
| Spread (call / `new` / array / object literal) | ✅ | `ArrayPushSpread` runs the loop in the VM |
| Rest parameters | ✅ | `MakeRest` |
| Default parameters | ✅ | Can reference earlier parameters |
| Template literal interpolation | ✅ | Tagged templates: basic form only (no frozen `raw`) |
| Generators (`function*`) | ❌ | Planned (M4) |

---

## 8. Built-in Objects

### Core

| Object | Status | Notes |
|--------|--------|-------|
| `Object` | ✅ | recent | Static: keys, values, entries, assign, defineProperty, defineProperties, getOwnPropertyDescriptor(s), getOwnPropertyNames, getOwnPropertySymbols, getPrototypeOf, setPrototypeOf, create, hasOwn, is, isExtensible/isFrozen/isSealed, preventExtensions/seal/freeze; prototype: toString, valueOf, hasOwnProperty, isPrototypeOf, propertyIsEnumerable, toLocaleString |
| `Array` | ✅ | recent / M3 | See the Array.prototype table above; non-index own properties, `length` attributes |
| `Function` | ✅ | recent / M3 | Constructor (no dynamic code evaluation); `call`/`apply`/`bind` incl. `new (f.bind(…))(…)`; user functions own `length`/`name`/`prototype` with spec attributes |
| `Boolean` | ✅ | recent | Constructor + prototype: valueOf, toString |
| `Number` | ✅ | recent / M3 | Static: isFinite, isInteger, isNaN, isSafeInteger, parseFloat, parseInt + 8 constants (read-only, non-enumerable, non-configurable); prototype: valueOf, toString (radix, `length = 1`), toFixed, toExponential, toPrecision; `Number::toString` follows ES 7.1.12.1 (`String(1e21)` → `"1e+21"`) |
| `String` | ✅ | recent / M3 | Constructor + statics `fromCharCode`/`fromCodePoint`/`raw`(open); prototype: charAt, charCodeAt, codePointAt (surrogate-aware), at, concat, includes, indexOf, lastIndexOf, slice, substring, toUpperCase/LowerCase, toLocaleUpperCase/LowerCase, trim/trimStart/trimEnd (ES whitespace set), padStart, padEnd, split, startsWith, endsWith, repeat, normalize (form validation only — no Unicode tables), localeCompare (code-unit order), valueOf/toString |
| `Symbol` | ⚠️ | M1/M2' | `Symbol()` non-constructable; well-known symbols defined with spec attributes; `description` getter, `toString`/`valueOf`, `Symbol.for`/`keyFor`; `Object.getOwnPropertySymbols` works |
| `Error` | ✅ | recent / M3 | All error types; `Error.prototype.name`/`message`, per-native prototypes own `name`/`message`, instance `[[Class]]` `"Error"`, `Error.prototype.toString` per ES 20.5.3.4, `Error.isError`, `new Error(msg, {cause})`; `stack` (ES2026 proposal) not implemented |
| `Math` | ✅ | recent / M3 | Abs…trunc incl. the ES6 additions (hypot, sign, clz32, imul, log2/log10, cbrt, trunc, fround, expm1, log1p, sinh…); methods `{writable:true, enumerable:false, configurable:true}` and constants read-only (ES 17) |
| `JSON` | ✅ | M3-B6 | `JSON.parse(text[, reviver])` (strict ECMA-404 parser, reviver walk with `CreateDataProperty` semantics) and `JSON.stringify(value[, replacer[, space]])` (`toJSON`, function/array replacer, number/string `space`, cycle detection, spec `finalize` indentation); `JSON[Symbol.toStringTag] === "JSON"`; `JSON.rawJSON`/`isRawJSON` (ES2025) and lone-surrogate escaping (ES2025) are out of scope |
| `Map` / `Set` / `WeakMap` / `WeakSet` | ❌ | — | Planned (M5) |

### Utility

| Object | Status | Notes |
|--------|--------|-------|


### Global Properties

| Feature | Status | Notes |
|---------|--------|-------|
| `NaN` | ⚠️ | Lowered as identifier |
| `Infinity` | ⚠️ | Lowered as identifier |
| `undefined` | ✅ | Lowered as keyword |

---

## 9. Type Coercion (Abstract Operations)

| Operation | Status | Since | Notes |
|-----------|--------|-------|-------|
| ToBoolean | ✅ | M0 | |
| ToNumber | ✅ | M0 / M3 | `""` → `0`, trimmed whitespace → `0`; `StringNumericLiteral` radix prefixes `0x`/`0o`/`0b` and `Infinity` (M3) |
| ToString | ✅ | M0 | |
| ToPrimitive | ✅ | recent | hint "default"/"number"/"string" |
| ToPropertyKey | ✅ | recent | |

### Abstract Equality (`==`)

| Scenario | Example | Status | Since |
|----------|---------|--------|-------|
| Same type | `1 == 1` | ✅ | M1 |
| null == undefined | `null == undefined` | ✅ | M1 |
| Number == String | `1 == '1'` | ✅ | M1 |
| Bool == Number | `true == 1` | ✅ | M1 |
| Object == Primitive | `[1] == 1` | ✅ | recent |
| Object == String | `[1,2] == "1,2"` | ✅ | recent |
| Symbol comparsion | `Symbol() == Symbol()` | ✅ | M1 |

---

## 10. Module System

| Feature | Status | Notes |
|---------|--------|-------|


---

## 11. Host Environment

| Feature | Status | Notes |
|---------|--------|-------|
| REPL | ✅ | Line-by-line execution |
| HostContext trait | 🚧 | Defined, not wired |
| NativeFunction trait | 🚧 | Defined, not wired |

---

## 12. Engine Internals

### Compiler Pipeline

| Component | Status | Notes |
|-----------|--------|-------|
| oxc parser integration | ✅ | |
| JSASTLower (AST → IR) | ✅ | Covers most expressions |
| IR definition | ✅ | Extended with JS semantics |
| CFG | ✅ | From evalit |
| SSA Builder | ✅ | From evalit |
| Register Allocator | ✅ | From evalit |
| Codegen (IR → Bytecode) | ✅ | |
| Bytecode module | ✅ | With symtab |

### VM

| Component | Status | Notes |
|-----------|--------|-------|
| Register-based execution | ✅ | |
| Control stack | ✅ | Call frames |
| Data stack | ✅ | |
| SEH (try/catch) | ✅ | |
| Prototype chain lookup | ✅ | `internal_get` / `internal_set` |
| Builtins prototype registry | ✅ | `Object.prototype`, `Array.prototype` |


### Memory

| Component | Status | Notes |
|-----------|--------|-------|
| Rc/RefCell-based GC | ⚠️ | Manual, no cycle collection |
| Arena allocator | ✅ | oxc parser |

---

## 13. Summary

### By Category

| Category | Total | ✅ | ⚠️/🚧 | ❌ |
|----------|-------|---|--------|---|
| Basic Language | ~20 | 16 | 3 | 1 |
| Expressions | ~35 | 28 | 6 | 1 |
| Statements | ~12 | 10 | 1 | 1 |
| Objects & Prototypes | ~10 | 10 | 0 | 0 |
| Arrays | ~6 | 6 | 0 | 0 |
| Functions & Closures | ~8 | 7 | 0 | 1 |
| ES6 Classes | ~3 | 2 | 0 | 1 |
| Built-in Objects | ~8 | 7 | 1 | 0 |
| Type Coercion | ~10 | 10 | 0 | 0 |
| Module System | ~0 | 0 | 0 | 0 |
| **Total** | **~112** | **93** | **13** | **5** |

### Out of Scope (By Design)

These features are intentionally **not supported** as they are incompatible with the static compilation model:

| Feature | Reason |
|---------|--------|
| `eval()` | Dynamic code evaluation breaks static analysis |
| `with` statement | Dynamic property lookup breaks static binding |
| `arguments.callee` | Deprecated in strict mode |
| `Function.prototype.caller` | Deprecated in strict mode |
| Octal literals (`0777`) | Deprecated in strict mode (use `0o777`) |
| Duplicate parameter names | Syntax error in strict mode |
| `this` boxing in primitive functions | Not applicable in strict mode |
| Regular expression literals | No regexp engine (independent sub-project) |
| Async functions (`async`/`await`), async iteration | Not ES6 |
| Class private fields/methods (`#x`), static blocks | Not ES6 (ES2022) |
| `BigInt`, optional chaining, nullish coalescing | Not ES6 |
| `Temporal`, `Intl`, `Atomics`, `SharedArrayBuffer` | Not ES6 and/or host dependent |
| ES modules (`import`/`export`) | Static-linking only possible; not committed in the current plan |
| CommonJS (`require`/`module.exports`) | Not part of the language |

### Planned But Not Yet Delivered

These are in scope for the ES6 goal and scheduled in `docs/es6-conformance-plan.md`:

| Feature | Milestone |
|---------|-----------|
| Well-known symbols (`toStringTag`, `toPrimitive`, `hasInstance`, `species`) | M2 residual |
| Tagged templates (tag invocation + strings array) | M2 residual |
| Built-ins: Object / Array / String / Number / Math / Function / Error completeness | M3 (B1/B2/B3/B4 delivered; B5 first pass) |
| `Date` | M3 |
| Generators (`function*`) and full iterator close semantics | M4 |
| `Map`, `Set`, `WeakMap`, `WeakSet`, `Promise` | M5 |
| `Proxy`, `Reflect` | M5 |
| `TypedArray` / `ArrayBuffer` / `DataView` | M6 |

### Test Counts

| Test Suite | Count |
|------------|-------|
| Unit tests (value, vm, etc.) | 190 |
| Feature integration test files | 28 (359 passing / 364 assertions, 5 pre-existing `return_in_try_finally` failures) |
| test262 executed / passing | 10552 / 7677 (72.75%) |

---

## Recent Milestones

| Date | Milestone |
|------|-----------|
| 2026-05-25 | ✅ Function declarations (hoisting, recursion, nesting) |
| 2026-05-25 | ✅ Object vs Primitive `==` coercion (ToPrimitive) |
| 2026-05-25 | ✅ Prototype chain + Array.prototype/Object.prototype.toString |
| 2026-05-25 | ✅ CallMethod opcode + built-in method dispatch |
| 2026-05-25 | ✅ Computed member method calls `arr[method]()` |
| 2026-05-25 | ✅ `PropGet`/`PropSet`/`IndexGet`/`IndexSet` with constant pool |
| 2026-05-25 | ✅ 22 new object/array integration tests (81 total) |
| 2026-05-26 | ✅ ES6 Classes (declarations, constructors, methods) |
| 2026-05-26 | ✅ `new` operator with constructor calls |
| 2026-05-26 | ✅ `this` binding in strict mode |
| 2026-05-26 | ✅ Function built-in object with prototype methods |
| 2026-05-26 | ✅ Function/class test262 suite enabled (53 tests) |
| 2026-05-26 | ✅ Error types (Error, TypeError, ReferenceError, RangeError, URIError, EvalError) |
| 2026-05-26 | ✅ Object.defineProperty() and Object.getOwnPropertyDescriptor() |
| 2026-05-26 | ✅ instanceof operator with prototype chain traversal |
| 2026-05-26 | ✅ Function.prototype property support for user-defined functions |
| 2026-05-26 | ✅ Boolean, Number, String constructors |
| 2026-05-26 | ✅ test262 try-catch-finally tests (54 tests, 2 skipped due to instanceof edge cases) |
| 2026-05-26 | ✅ Array.prototype methods: push, pop, shift, unshift, indexOf, includes, join, slice, concat, splice |
| 2026-05-26 | ✅ Boolean.prototype methods: valueOf, toString |
| 2026-05-26 | ✅ Number.prototype methods: valueOf, toString, toFixed, toExponential, toPrecision |
| 2026-05-26 | ✅ String.prototype methods: charAt, charCodeAt, concat, includes, indexOf, slice, substring, toUpperCase, toLowerCase, trim, split |
| 2026-05-26 | ✅ Object static methods: entries, getOwnPropertyNames, getPrototypeOf, setPrototypeOf, create, hasOwn, is |
| 2026-05-26 | ✅ 18 new test262 test suites (72 total tests) |
| 2026-05-26 | ✅ Arrow function closure variable capture with `ClosureVar` runtime stack |
| 2026-05-26 | ✅ 5 closure capture feature tests with value assertions |
| 2026-09-20 | ✅ M1: iterator protocol, for-of/for-in, template interpolation, default/rest params, spread (call/new/array/object), destructuring incl. assignment targets |
| 2026-09-20 | ✅ M2: class static members/fields, accessors, `extends`/`super`, `prototype.constructor`, class constructors require `new` |
| 2026-09-20 | 🐛 Implicit return now clears Rv (constructors returned a leftover object); accessors use the receiver as `this` |
| 2026-09-20 | 📈 test262 2444 → 3901 passing (10240 executed) |
| 2026-09-21 | ✅ M3-B1 (`Object`) batches 1-3: ToObject boxing, `[[DefineOwnProperty]]` validation, array element accessors, `Object.assign` in the VM, SEH cross-frame unwinding → test262 4611 passing |
| 2026-09-22 | ✅ M3-B1/B2/B3/B4/B5 (12 commits, `docs/es6-conformance-plan.md` §2.1f): `F.prototype.constructor`, function `length`/`name` metadata + builtin arity table, built-in property attributes, native-constructor prototype chain, real `ToNumber` opcode, `Number::toString`, String method dispatch + `codePointAt`/`at`/`normalize`/`toLocale*`/`pad*`/ES whitespace `trim`, built-in error propagation, `Error` semantics (`[[Class]]`, `toString`, `isError`, `cause`), bound-function construction, `Array.prototype.at`/`copyWithin`/`entries`/`keys`/`values` → test262 6015 → **7416 passing** (71.55%) |
| 2026-09-22 | 🐛 Fixed two latent correctness bugs found by the above: built-in errors were silently swallowed by the `CallMethod` fallback (`is_unknown_builtin`), and `new` on a built-in constructor passed arguments in reverse order |
| 2026-09-22 | ✅ M3-B6 `JSON` (`docs/es6-conformance-plan.md` §2.1m): namespace object, ECMA-404 parser, full serializer (`toJSON`/`replacer`/`space`/cycles) and reviver walk in the VM; `built-ins/JSON` enabled (112 passing) plus `SyntaxError`/`ToPrimitive`/array-hole/`join` fixes → test262 7481 → **7677 passing** (72.75%) |
