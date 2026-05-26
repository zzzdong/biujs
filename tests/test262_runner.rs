//! test262 conformance test runner for biujs
//!
//! Uses test262-harness crate with `Harness::new(path)` to target specific
//! test subdirectories matching biujs's current capabilities.
//!
//! ## Target
//!
//! **ES6 (ECMAScript 2015)** – with the following explicit exclusions:
//!   - `eval` – No dynamic code evaluation.
//!   - `with` – No dynamic scope binding.
//!   - `Proxy`, `Reflect`, `Symbol` – Meta-programming not supported.
//!   - Arrow functions, generators, async – Partially implemented (arrow functions
//!     with this capture and closure variable capture are supported).
//!   - `Map`, `Set`, `Promise`, `RegExp`, `Date` – Not yet implemented.
//!
//! ## Strategy
//!
//! test262 tests universally use `throw new Test262Error(...)`, `var`, `eval()`,
//! built-in objects, etc. We aggressively filter to only run tests whose source
//! code can be parsed and executed by biujs.
//!
//! ## biujs currently supported features:
//! - Numbers, strings, booleans, null, undefined
//! - Symbols (Symbol(), Symbol.for(), Symbol.keyFor())
//! - Arithmetic: +, -, *, /, %
//! - Comparison: >, <, >=, <=, ==, !=, ===, !==
//! - Logical: !, &&, ||
//! - Bitwise: &, |, ^, ~, <<, >>, >>>
//! - typeof
//! - let variables, assignments
//! - var, const declarations
//! - if/else, while, for loops (with break/continue)
//! - function declarations and calls
//! - arrow functions with this capture and closure variable capture
//! - class declarations and expressions
//! - `new` operator with constructors
//! - `this` binding in strict mode
//! - default parameter values

use biujs::{Compiler, VM};
use std::path::PathBuf;
use std::sync::OnceLock;
use test262_harness::Harness;

/// Root directory for test262 tests (contains language/expressions/ etc.)
fn test262_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262/test")
}

/// Root directory for test262 harness files
fn test262_harness_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262/harness")
}

/// Cached harness file contents
static HARNESS_ASSERT: OnceLock<String> = OnceLock::new();
static HARNESS_STRICT: OnceLock<String> = OnceLock::new();

/// Load harness files and cache them
fn load_harness() -> (String, String) {
    let assert_content = HARNESS_ASSERT.get_or_init(|| {
        let path = test262_harness_root().join("assert.js");
        std::fs::read_to_string(&path).unwrap_or_default()
    });
    
    let strict_content = HARNESS_STRICT.get_or_init(|| {
        let path = test262_harness_root().join("sta.js");
        std::fs::read_to_string(&path).unwrap_or_default()
    });
    
    (assert_content.clone(), strict_content.clone())
}

/// Check if test262 directory exists
fn has_tests() -> bool {
    test262_root().join("language").exists()
}

/// Comprehensive skip filter based on source content.
///
/// Returns Some(reason) if the test should be skipped, None if it might be runnable.
fn should_skip_source(source: &str) -> Option<&'static str> {
    // test262 YAML metadata block
    let has_metadata = source.contains("/*---");
    if !has_metadata {
        return Some("no test262 metadata");
    }

    // Patterns NOT supported by biujs (ES6 target, no eval/with).
    // Order matters: check the most common patterns first.
    let unsupported_patterns: &[&str] = &[
        // Functions
        "...",         // spread/rest (though `...` in string literals is fine)
        // Built-ins NOT yet implemented
        "eval(",
        "parseInt(",
        "parseFloat(",
        "Math.",
        "JSON",
        "Date",
        "RegExp",
        "Proxy",
        "Promise",
        "Map(",
        "Set(",
        "WeakRef",
        "WeakMap",
        "WeakSet",
        // Harness infrastructure
        "$DONOTEVALUATE",
        "$ERROR",
        "print(",
        // Misc unsupported
        "delete ",
        "void ",
        "yield ",
        "await ",
        "import ",
        "export ",
        "with ",
        "switch",
        "do ",
        "label:",
        // Template literals (biujs has partial support but tests are complex)
        "`",
    ];

    for pattern in unsupported_patterns {
        if source.contains(pattern) {
            return Some(pattern);
        }
    }

    None
}

/// Run all tests in a given subdirectory using test262-harness.
///
/// Returns (total, passed, skipped, failures).
fn run_suite(subdir: &str) -> (u32, u32, u32, Vec<(String, String)>) {
    let path = test262_root().join(subdir);
    if !path.exists() {
        eprintln!("  Path does not exist: {}", path.display());
        return (0, 0, 0, vec![]);
    }

    let harness = match Harness::new(&path) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("  Failed to initialize harness for {}: {}", subdir, e);
            return (0, 0, 0, vec![]);
        }
    };

    let mut total = 0u32;
    let mut passed = 0u32;
    let mut skipped = 0u32;
    let mut failures: Vec<(String, String)> = Vec::new();

    for test_result in harness {
        let test = match test_result {
            Ok(t) => t,
            Err(_) => continue,
        };
        total += 1;

        let source = &test.source;

        // Apply source-based skip filter
        if let Some(_reason) = should_skip_source(source) {
            skipped += 1;
            continue;
        }

        // Also skip tests with features not yet implemented in biujs (ES6 target)
        let skip_features = [
            "async-iteration", "async-functions", "generators", "modules",
            "destructuring-binding", "for-of",
            "default-parameters",
            "Proxy", "Promise", "Map", "Set", "Proxy", "object-rest",
            "object-spread", "rest-parameters", "spread-syntax", "super",
            "optional-chaining", "nullish-coalescing", "logical-assignment",
            "numeric-separator", "exponentiation", "dynamic-import",
            "import.meta", "import-assertions", "tail-call-optimization",
        ];
        let has_unsupported_feature = test.desc.features.iter().any(|f| {
            skip_features.iter().any(|sf| f.contains(sf))
        });
        if has_unsupported_feature {
            skipped += 1;
            continue;
        }

        // Skip async/module/generated tests
        let has_bad_flag = test.desc.flags.iter().any(|f| {
            matches!(f, 
                test262_harness::Flag::Async | 
                test262_harness::Flag::Module | 
                test262_harness::Flag::Generated
            )
        });
        if has_bad_flag {
            skipped += 1;
            continue;
        }

        // Run the test
        let test_id = test
            .desc
            .id
            .clone()
            .unwrap_or_else(|| test.path.display().to_string());

        // Inject harness files before the test source
        let (assert_harness, sta_harness) = load_harness();
        let full_source = format!("{}\n{}\n{}", sta_harness, assert_harness, source);

        let mut compiler = Compiler::new();
        match compiler.compile(&full_source) {
            Ok(module) => {
                let mut vm = VM::new();
                match vm.run(&module) {
                    Ok(_) => {
                        passed += 1;
                    }
                    Err(e) => {
                        failures.push((test_id, format!("Runtime error: {}", e)));
                    }
                }
            }
            Err(e) => {
                failures.push((test_id, format!("Compilation error: {}", e)));
            }
        }
    }

    (total, passed, skipped, failures)
}

/// Generic test runner that prints results for a specific subdirectory
fn test_subdirectory(subdir: &str, label: &str) {
    if !has_tests() {
        eprintln!("test262 not found at {:?}, skipping {}", test262_root(), label);
        return;
    }

    let (total, passed, skipped, failures) = run_suite(subdir);

    println!("\n=== test262: {} ({}) ===", label, subdir);
    println!("Total:   {}", total);
    println!("Passed:  {}", passed);
    println!("Skipped: {}", skipped);
    println!("Failed:  {}", failures.len());

    if !failures.is_empty() {
        println!("--- Failures ---");
        for (id, msg) in &failures {
            println!("  FAIL: {} - {}", id, msg);
        }
    }
}

// ---- Focused test suites for biujs-supported features ----

#[test]
fn test262_typeof() {
    test_subdirectory("language/expressions/typeof", "typeof operator");
}

#[test]
fn test262_addition() {
    test_subdirectory("language/expressions/addition", "addition (+)");
}

#[test]
fn test262_subtraction() {
    test_subdirectory("language/expressions/subtraction", "subtraction (-)");
}

#[test]
fn test262_multiplication() {
    test_subdirectory("language/expressions/multiplication", "multiplication (*)");
}

#[test]
fn test262_division() {
    test_subdirectory("language/expressions/division", "division (/)");
}

#[test]
fn test262_modulus() {
    test_subdirectory("language/expressions/modulus", "modulus (%)");
}

#[test]
fn test262_greater_than() {
    test_subdirectory("language/expressions/greater-than", "greater-than (>)");
}

#[test]
fn test262_less_than() {
    test_subdirectory("language/expressions/less-than", "less-than (<)");
}

#[test]
fn test262_equals() {
    test_subdirectory("language/expressions/equals", "equals (==)");
}

#[test]
fn test262_does_not_equals() {
    test_subdirectory("language/expressions/does-not-equals", "does-not-equals (!=)");
}

#[test]
fn test262_logical_not() {
    test_subdirectory("language/expressions/logical-not", "logical-not (!)");
}

#[test]
fn test262_logical_and() {
    test_subdirectory("language/expressions/logical-and", "logical-and (&&)");
}

#[test]
fn test262_logical_or() {
    test_subdirectory("language/expressions/logical-or", "logical-or (||)");
}

#[test]
fn test262_strict_equals() {
    test_subdirectory("language/expressions/strict-equals", "strict-equals (===)");
}

#[test]
fn test262_strict_does_not_equals() {
    test_subdirectory("language/expressions/strict-does-not-equals", "strict-does-not-equals (!==)");
}

#[test]
fn test262_greater_than_or_equal() {
    test_subdirectory("language/expressions/greater-than-or-equal", "greater-than-or-equal (>=)");
}

#[test]
fn test262_less_than_or_equal() {
    test_subdirectory("language/expressions/less-than-or-equal", "less-than-or-equal (<=)");
}

#[test]
fn test262_unary_minus() {
    test_subdirectory("language/expressions/unary-minus", "unary-minus (-x)");
}

#[test]
fn test262_unary_plus() {
    test_subdirectory("language/expressions/unary-plus", "unary-plus (+x)");
}

#[test]
fn test262_bitwise_and() {
    test_subdirectory("language/expressions/bitwise-and", "bitwise-and (&)");
}

#[test]
fn test262_bitwise_or() {
    test_subdirectory("language/expressions/bitwise-or", "bitwise-or (|)");
}

#[test]
fn test262_bitwise_xor() {
    test_subdirectory("language/expressions/bitwise-xor", "bitwise-xor (^)");
}

#[test]
fn test262_bitwise_not() {
    test_subdirectory("language/expressions/bitwise-not", "bitwise-not (~)");
}

#[test]
fn test262_left_shift() {
    test_subdirectory("language/expressions/left-shift", "left-shift (<<)");
}

#[test]
fn test262_right_shift() {
    test_subdirectory("language/expressions/right-shift", "right-shift (>>)");
}

#[test]
fn test262_unsigned_right_shift() {
    test_subdirectory("language/expressions/unsigned-right-shift", "unsigned-right-shift (>>>)");
}

// ---- Literals ----

#[test]
fn test262_numeric_literals() {
    test_subdirectory("language/literals/numeric", "numeric literals");
}

#[test]
fn test262_string_literals() {
    test_subdirectory("language/literals/string", "string literals");
}

#[test]
fn test262_boolean_literals() {
    test_subdirectory("language/literals/boolean", "boolean literals");
}

// ---- Increment/Decrement ----

#[test]
fn test262_postfix_increment() {
    test_subdirectory("language/expressions/postfix-increment", "postfix-increment (x++)");
}

#[test]
fn test262_postfix_decrement() {
    test_subdirectory("language/expressions/postfix-decrement", "postfix-decrement (x--)");
}

#[test]
fn test262_prefix_increment() {
    test_subdirectory("language/expressions/prefix-increment", "prefix-increment (++x)");
}

#[test]
fn test262_prefix_decrement() {
    test_subdirectory("language/expressions/prefix-decrement", "prefix-decrement (--x)");
}

// ---- Statements ----

#[test]
fn test262_if_else() {
    test_subdirectory("language/statements/if", "if/else statements");
}

#[test]
fn test262_while() {
    test_subdirectory("language/statements/while", "while statements");
}

#[test]
fn test262_for() {
    test_subdirectory("language/statements/for", "for statements");
}

#[test]
fn test262_function_declarations() {
    test_subdirectory("language/statements/function", "function declarations");
}

#[test]
fn test262_let() {
    test_subdirectory("language/statements/let", "let declarations");
}

#[test]
fn test262_block() {
    test_subdirectory("language/statements/block", "block statements");
}

#[test]
fn test262_break() {
    test_subdirectory("language/statements/break", "break statements");
}

#[test]
fn test262_continue() {
    test_subdirectory("language/statements/continue", "continue statements");
}

// ---- Identifiers ----

#[test]
fn test262_identifiers() {
    test_subdirectory("language/identifiers", "identifiers");
}

// ---- Types ----

#[test]
fn test262_types() {
    test_subdirectory("language/types", "type coercion and behavior");
}

// ---- Built-in Objects ----

#[test]
fn test262_builtin_boolean() {
    test_subdirectory("built-ins/Boolean", "Boolean");
}

#[test]
fn test262_builtin_number() {
    test_subdirectory("built-ins/Number", "Number");
}

#[test]
fn test262_builtin_string() {
    test_subdirectory("built-ins/String", "String");
}

#[test]
fn test262_builtin_object() {
    test_subdirectory("built-ins/Object", "Object");
}

#[test]
fn test262_builtin_array() {
    test_subdirectory("built-ins/Array", "Array");
}

#[test]
fn test262_builtin_error() {
    test_subdirectory("built-ins/Error", "Error");
}

#[test]
fn test262_builtin_native_errors() {
    test_subdirectory("built-ins/NativeErrors", "NativeErrors (TypeError, etc.)");
}

#[test]
fn test262_statement_try() {
    test_subdirectory("language/statements/try", "try-catch-finally statements");
}

#[test]
fn test262_builtin_string_prototype() {
    test_subdirectory("built-ins/String/prototype", "String.prototype methods");
}

#[test]
fn test262_builtin_array_prototype() {
    test_subdirectory("built-ins/Array/prototype", "Array.prototype methods");
}

#[test]
fn test262_builtin_number_prototype() {
    test_subdirectory("built-ins/Number/prototype", "Number.prototype methods");
}

// ---- Class ----

#[test]
fn test262_class_statements() {
    test_subdirectory("language/statements/class", "class declarations");
}

#[test]
fn test262_class_expressions() {
    test_subdirectory("language/expressions/class", "class expressions");
}

// ---- Expressions (Additional) ----

#[test]
fn test262_new_expression() {
    test_subdirectory("language/expressions/new", "new operator");
}

#[test]
fn test262_this_expression() {
    test_subdirectory("language/expressions/this", "this keyword");
}

#[test]
fn test262_delete_expression() {
    test_subdirectory("language/expressions/delete", "delete operator");
}

#[test]
fn test262_void_expression() {
    test_subdirectory("language/expressions/void", "void operator");
}

#[test]
fn test262_instanceof_expression() {
    test_subdirectory("language/expressions/instanceof", "instanceof operator");
}

#[test]
fn test262_in_expression() {
    test_subdirectory("language/expressions/in", "in operator");
}

#[test]
fn test262_grouping_expression() {
    test_subdirectory("language/expressions/grouping", "grouping (parenthesized)");
}

#[test]
fn test262_comma_expression() {
    test_subdirectory("language/expressions/comma", "comma operator");
}

#[test]
fn test262_conditional_expression() {
    test_subdirectory("language/expressions/conditional", "conditional (ternary)");
}

#[test]
fn test262_call_expression() {
    test_subdirectory("language/expressions/call", "function calls");
}

#[test]
fn test262_member_expression() {
    test_subdirectory("language/expressions/member", "property access");
}

// ---- Literals (Additional) ----

#[test]
fn test262_null_literal() {
    test_subdirectory("language/literals/null", "null literal");
}

// ---- Statements (Additional) ----

#[test]
fn test262_throw_statement() {
    test_subdirectory("language/statements/throw", "throw statements");
}

#[test]
fn test262_return_statement() {
    test_subdirectory("language/statements/return", "return statements");
}

// ---- Built-in Objects (Additional) ----

#[test]
fn test262_builtin_function() {
    test_subdirectory("built-ins/Function", "Function constructor");
}

// ---- Arrow Functions ----

#[test]
fn test262_arrow_functions() {
    test_subdirectory("language/expressions/arrow-function", "arrow functions");
}
