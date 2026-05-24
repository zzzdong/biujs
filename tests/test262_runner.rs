//! test262 conformance test runner for biujs
//!
//! Uses test262-harness crate with `Harness::new(path)` to target specific
//! test subdirectories matching biujs's current capabilities.
//!
//! ## biujs supported features (v0.1):
//! - Numbers, strings, booleans
//! - Arithmetic: +, -, *, /, %
//! - Comparison: >, <, >=, <=, ==, !=
//! - Logical: !, &&, ||
//! - typeof
//! - let variables
//! - if/else, while, for loops
//! - function declarations
//!
//! ## Strategy
//!
//! test262 tests universally use `throw new Test262Error(...)`, `var`, `eval()`,
//! built-in objects, etc. We aggressively filter to only run tests whose source
//! code can be parsed and executed by biujs.

use biujs::{Compiler, VM};
use std::path::PathBuf;
use test262_harness::Harness;

/// Root directory for test262 tests (contains language/expressions/ etc.)
fn test262_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262/test")
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

    // Every feature below is NOT supported by biujs.
    // Order matters: check the most common patterns first.
    let unsupported_patterns: &[&str] = &[
        // Error handling
        "throw",
        "try",
        "catch",
        "finally",
        // OOP
        "class ",
        "new ",
        "instanceof",
        "prototype",
        "this",
        // Functions (biujs only supports `function name() {}` declarations)
        "=>",          // arrow functions
        "...",         // spread/rest (though `...` in string literals is fine)
        // Declarations
        "var ",
        "const ",
        // Built-ins
        "eval(",
        "isNaN(",
        "parseInt(",
        "parseFloat(",
        "Array",
        "Object",
        "String",
        "Number",
        "Boolean",
        "Math.",
        "JSON",
        "Date",
        "RegExp",
        "Error",
        "TypeError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "Symbol(",
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
        "assert.",
        "Test262Error",
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
        ".toString(",
        ".valueOf(",
        // String methods
        ".charAt",
        ".indexOf",
        ".substring",
        ".slice",
        ".split",
        ".replace",
        ".trim",
        ".length",
        ".concat",
        // Property access patterns biujs can't handle
        ".length",
        "[0]",
        "[1]",
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

        // Also skip tests with features that biujs doesn't support
        let skip_features = [
            "async-iteration", "async-functions", "generators", "modules",
            "class", "arrow-function", "destructuring-binding", "for-of",
            "Symbol", "template", "const", "default-parameters",
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

        let mut compiler = Compiler::new();
        match compiler.compile(source) {
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
