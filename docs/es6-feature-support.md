# ES6 Feature Support Status

> **Last updated**: 2026-09-20
> **Engine version**: 0.1.0
> **Total tests**: 188 unit + 17 feature files + test262 10240 executed (3901 passing)
>
> Roadmap: see `docs/es6-conformance-plan.md` (M2 residual → M6).

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
| Template literals | ⚠️ | M1 | Static only (no `${expr}` interpolation) |

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
| `++` (increment) | ✅ | M1 | Prefix & postfix |
| `--` (decrement) | ✅ | M1 | Prefix & postfix |
| Unary `+` | ✅ | M1 | `+expr` → ToNumber |
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
| `??` | ⚠️ | M1 | Lowered but may be simplified |
| `!` | ✅ | M1 | |
| `&`, `\|`, `^`, `<<`, `>>`, `>>>` | ⚠️ | test262 | Parsed + VM, lowering may be simplified |

### Property Access

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| Dot access (`obj.prop`) | ✅ | recent | Static property name |
| Bracket access (`obj[expr]`) | ✅ | recent | Dynamic property name |
| Method call (`obj.method()`) | ✅ | recent | Static + dynamic method names |
| Prototype chain lookup | ✅ | recent | `[[Get]]` via `internal_get` |
| `in` operator | 🚧 | — | VM opcode exists, stub |
| `instanceof` | 🚧 | recent | VM opcode implemented, prototype chain traversal working for built-ins |

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
| `delete` | 🚧 | — | Parsed, returns `false` |
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
| Property deletion | 🚧 | — | VM `[[Delete]]` exists |
| `Object.prototype.toString` | ✅ | recent | Returns `[object ClassName]` |
| `valueOf()` | ✅ | recent | Returns self |
| Prototype chain | ✅ | recent | Instances linked via `Builtins` |

### Object.prototype Methods

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `.toString()` | ✅ | recent | |
| `.valueOf()` | ✅ | recent | |


### Static Object Methods

| Feature | Status | Since | Notes |
|---------|--------|-------|-------|
| `Object.defineProperty()` | ⚠️ | recent | Basic implementation (simplified descriptor handling) |
| `Object.getOwnPropertyDescriptor()` | ⚠️ | recent | Basic implementation |
| `Object.freeze()` | 🚧 | — | Object trait has methods |

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
| `.push()` | ✅ | recent | Via MakeArray + ArrayPush |

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
| `super()` | ✅ | M2 | Parent constructor resolved at run time from the receiver |
| `super.method()` / `super.prop` | ⚠️ | M2 | Bound through `Function.prototype.call`; property read uses the parent prototype |
| `new.target` | ❌ | — | Planned (M2 residual) |
| Computed method names | ❌ | — | Planned (M2 residual, via `Object.defineProperty`) |
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
| `Object` | ✅ | recent | Static: keys, values, entries, defineProperty, getOwnPropertyDescriptor, getOwnPropertyNames, getPrototypeOf, setPrototypeOf, create, hasOwn, is |
| `Array` | ✅ | recent | Constructor + prototype: push, pop, shift, unshift, indexOf, includes, join, slice, concat, splice |
| `Function` | ✅ | recent | Constructor and prototype methods (no dynamic code evaluation) |
| `Boolean` | ✅ | recent | Constructor + prototype: valueOf, toString |
| `Number` | ✅ | recent | Constructor + static (isFinite, isInteger, isNaN) + prototype (valueOf, toString, toFixed, toExponential, toPrecision) |
| `String` | ✅ | recent | Constructor + prototype: charAt, charCodeAt, concat, includes, indexOf, slice, substring, toUpperCase, toLowerCase, trim, split |
| `Symbol` | ⚠️ | M1 | Type exists, `Symbol()` not constructable |
| `Error` | ✅ | recent | All error types implemented (Error, TypeError, ReferenceError, RangeError, URIError, EvalError) |

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
| ToNumber | ✅ | M0 | `""` → `0`, trimmed whitespace → `0` |
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
| Computed property names, `new.target`, `**` | M2 residual |
| Well-known symbols (`toStringTag`, `toPrimitive`, `hasInstance`, `species`) | M2 residual |
| Built-ins: Object / Array / String / Number / Math / Function / Error completeness | M3 |
| `JSON`, `Date` | M3 |
| Generators (`function*`) and full iterator close semantics | M4 |
| `Map`, `Set`, `WeakMap`, `WeakSet`, `Promise` | M5 |
| `Proxy`, `Reflect` | M5 |
| `TypedArray` / `ArrayBuffer` / `DataView` | M6 |

### Test Counts

| Test Suite | Count |
|------------|-------|
| Unit tests (value, vm, etc.) | 188 |
| Feature integration test files | 17 |
| test262 executed / passing | 10240 / 3901 |

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
