//! test262 conformance test runner for biujs
//!
//! Uses the `test262-harness` crate to walk the test262 suite and executes each
//! test with biujs's own `Compiler` + `VM`.
//!
//! ## Running
//!
//! ```text
//! cargo test --release --test test262_runner
//! TEST262_SUITES=addition,typeof cargo test --release --test test262_runner
//! ```
//!
//! ## Conformance semantics
//!
//! * **Positive tests** pass when the whole program (harness + test) executes
//!   without an uncaught exception. test262 signals failure by throwing
//!   `Test262Error`, which surfaces as `RuntimeError::Thrown`.
//! * **Negative tests** (`negative:` in the YAML front-matter) pass only when
//!   biujs actually fails, and with the expected error kind — either a compile
//!   error (`phase: parse` / `phase: resolution`) or a runtime error whose
//!   `name` matches (`phase: runtime`).
//!
//! Tests requiring features biujs does not implement are *skipped* rather than
//! counted as failures, so the pass rate reflects real capability.

use biujs::{CompileError, Compiler, RuntimeError, VM, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use test262_harness::{Flag, Harness, Phase, Test};

// ─────────────────────────────────────────────────────────
// Paths / harness loading
// ─────────────────────────────────────────────────────────

fn test262_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262/test")
}

fn test262_harness_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262/harness")
}

fn has_tests() -> bool {
    test262_root().join("language").exists()
}

static HARNESS_CACHE: OnceLock<HashMap<String, String>> = OnceLock::new();

fn harness(name: &str) -> Option<&'static str> {
    let cache = HARNESS_CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        let dir = test262_harness_root();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(fname) = entry.file_name().to_str() {
                    if fname.ends_with(".js") {
                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                            map.insert(fname.to_string(), content);
                        }
                    }
                }
            }
        }
        map
    });
    cache.get(name).map(|s| s.as_str())
}

/// Harness files every test may rely on, whether or not it declares them.
///
/// Sputnik-era tests (`language/expressions/addition/S11.6.1_A2.1_T2.js`, …)
/// have no `includes` front-matter at all but still use `Test262Error`, so the
/// baseline harness must always be loaded.
const BASELINE_HARNESS: &[&str] = &["sta.js", "assert.js"];

/// Build the full source for a test: the baseline harness, its declared
/// includes, then the test itself.
fn build_source(test: &Test) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    let names: Vec<&str> = BASELINE_HARNESS
        .iter()
        .copied()
        .chain(test.desc.includes.iter().map(|s| s.as_str()))
        .collect();

    for name in names {
        if !seen.insert(name) {
            continue;
        }
        match harness(name) {
            Some(src) => parts.push(src),
            // Missing harness file: the test cannot run faithfully.
            None => return String::new(),
        }
    }

    parts.push(&test.source);
    parts.join("\n")
}

// ─────────────────────────────────────────────────────────
// Filtering
// ─────────────────────────────────────────────────────────

/// test262 feature tags biujs does not implement (ES6 subset, no eval/with).
const UNSUPPORTED_FEATURES: &[&str] = &[
    "async-iteration",
    "async-functions",
    "Atomics",
    "BigInt",
    "class-fields-private",
    "class-methods-private",
    "class-static-block",
    "cross-realm",
    "dynamic-import",
    "generators",
    "import.meta",
    "import-assertions",
    "Intl",
    "json-parse-with-source",
    "logical-assignment",
    "Map",
    "modules",
    "numeric-separator",
    "optional-chaining",
    "Promise",
    "Proxy",
    "Reflect",
    "reflect-metadata",
    "regexp-",
    "Set",
    "SharedArrayBuffer",
    "String.prototype.replaceAll",
    "symbol-description",
    "tail-call-optimization",
    "Temporal",
    "TypedArray",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "u180e",
    "well-formed-json-stringify",
];

/// Source patterns biujs cannot handle at all.
const UNSUPPORTED_PATTERNS: &[&str] = &[
    "eval(",
    // `JSON` used to be listed here while it was unimplemented; it is now a
    // real built-in, so tests that exercise it must run.
    "Date",
    "RegExp",
    "$DONOTEVALUATE",
    "new Function(",
    "Function(\"",
    "Function('",
    "$ERROR",
    "print(",
    "yield ",
    "await ",
    "import(",
    "import ",
    "export ",
    "with (",
    "with(",
    "label:",
    "=>*",
    "function*",
    "async ",
];

fn should_skip(test: &Test) -> Option<String> {
    if !test.source.contains("/*---") {
        return Some("no test262 metadata".to_string());
    }

    for flag in &test.desc.flags {
        match flag {
            Flag::Async | Flag::Module | Flag::Generated => {
                return Some(format!("flag {flag:?}"));
            }
            _ => {}
        }
    }

    for feature in &test.desc.features {
        if UNSUPPORTED_FEATURES
            .iter()
            .any(|f| feature.starts_with(f) || feature.contains(f))
        {
            return Some(format!("feature {feature}"));
        }
    }

    if !test.desc.locale.is_empty() {
        return Some("locale-dependent".to_string());
    }

    for pattern in UNSUPPORTED_PATTERNS {
        if test.source.contains(pattern) {
            return Some(format!("pattern {pattern:?}"));
        }
    }

    None
}

// ─────────────────────────────────────────────────────────
// Execution
// ─────────────────────────────────────────────────────────

/// The `name` property of a thrown JS value, when it is an Error-like object.
///
/// `name` lives on the error *prototype* for the standard error types, so the
/// whole prototype chain has to be walked.
fn thrown_error_name(val: &Value) -> Option<String> {
    let mut current = val.as_object();
    let mut depth = 0;
    while let Some(obj) = current {
        if let Some(desc) = obj.borrow().property_get(&biujs::vm::PropertyKey::from_str("name")) {
            if let Value::String(s) = desc.value {
                return Some(s.to_string());
            }
        }
        current = obj.borrow().get_prototype();
        depth += 1;
        if depth > 16 {
            break;
        }
    }
    None
}

/// Best-effort JS error `name` for a compile-time failure.
fn compile_error_kind(err: &CompileError) -> Option<&'static str> {
    match err {
        CompileError::SyntaxError { .. } => Some("SyntaxError"),
        CompileError::SemanticError { .. } => Some("TypeError"),
        _ => None,
    }
}

/// Run one test. Returns `Ok(())` when it conforms, `Err(reason)` otherwise.
fn run_test(test: &Test) -> Result<(), String> {
    let source = build_source(test);
    if source.is_empty() {
        return Err("missing harness include".to_string());
    }

    let mut compiler = Compiler::new();
    let compiled = compiler.compile(&source);

    match &test.desc.negative {
        // ── Positive test: the program must run to completion ──
        None => match compiled {
            Ok(module) => {
                let mut vm = VM::new();
                vm.run(&module).map(|_| ()).map_err(|e| e.to_string())
            }
            Err(err) => Err(format!("Compilation error: {err}")),
        },

        // ── Negative test: biujs must fail, with the expected error kind ──
        Some(neg) => {
            let expected = neg.kind.as_deref();
            let matches_kind = |actual: Option<&str>| match expected {
                None => true,
                Some(want) => actual == Some(want),
            };

            match neg.phase {
                Phase::Parse | Phase::Resolution => match compiled {
                    Err(err) => {
                        if matches_kind(Some("SyntaxError")) {
                            Ok(())
                        } else {
                            Err(format!("expected {expected:?}, got {err}"))
                        }
                    }
                    Ok(_) => Err(format!(
                        "expected {expected:?} at {:?} phase but compilation succeeded",
                        neg.phase
                    )),
                },
                Phase::Runtime => match compiled {
                    Err(err) => {
                        if matches_kind(compile_error_kind(&err)) {
                            Ok(())
                        } else {
                            Err(format!("expected {expected:?}, got {err}"))
                        }
                    }
                    Ok(module) => {
                        let mut vm = VM::new();
                        match vm.run(&module) {
                            Err(err) => {
                                let actual: Option<String> = match &err {
                                    RuntimeError::TypeError(_) => Some("TypeError".to_string()),
                                    RuntimeError::ReferenceError(_) => {
                                        Some("ReferenceError".to_string())
                                    }
                                    RuntimeError::RangeError(_) => Some("RangeError".to_string()),
                                    RuntimeError::SyntaxError(_) => Some("SyntaxError".to_string()),
                                    RuntimeError::Thrown(val) => {
                                        Some(thrown_error_name(val).unwrap_or_else(|| "Error".to_string()))
                                    }
                                    _ => None,
                                };
                                if matches_kind(actual.as_deref()) {
                                    Ok(())
                                } else {
                                    Err(format!("expected {expected:?}, got {err}"))
                                }
                            }
                            Ok(_) => Err(format!(
                                "expected {expected:?} to be thrown but nothing was"
                            )),
                        }
                    }
                },
                Phase::Early => Err("early-error tests are not modelled".to_string()),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────
// Suite driver
// ─────────────────────────────────────────────────────────

struct SuiteResult {
    total: u32,
    passed: u32,
    skipped: u32,
    failures: Vec<(String, String)>,
}

fn run_suite(subdir: &str) -> SuiteResult {
    let mut result = SuiteResult {
        total: 0,
        passed: 0,
        skipped: 0,
        failures: Vec::new(),
    };

    let path = test262_root().join(subdir);
    if !path.exists() {
        return result;
    }

    let harness = match Harness::new(&path) {
        Ok(h) => h,
        Err(e) => {
            result
                .failures
                .push((subdir.to_string(), format!("harness error: {e}")));
            return result;
        }
    };

    for test_result in harness {
        let test = match test_result {
            Ok(t) => t,
            Err(_) => continue,
        };
        result.total += 1;

        if let Some(_reason) = should_skip(&test) {
            result.skipped += 1;
            continue;
        }

        let id = test
            .desc
            .id
            .clone()
            .unwrap_or_else(|| test.path.display().to_string())
            .replace(
                &format!("{}/", env!("CARGO_MANIFEST_DIR")),
                "",
            );

        // An engine bug may panic (index out of bounds, unwrap, …). Catch it so
        // one broken test cannot abort the whole conformance run.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_test(&test)))
            .unwrap_or_else(|payload| {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".to_string());
                Err(format!("engine panic: {msg}"))
            });

        match outcome {
            Ok(()) => result.passed += 1,
            Err(reason) => result.failures.push((id, reason)),
        }
    }

    result
}

/// Every test262 subdirectory relevant to biujs's supported feature set.
const SUITES: &[&str] = &[
    "language/statements/for-of",
    "language/statements/for-in",
    "language/destructuring",
    "language/expressions/assignment/destructuring",
    "language/functions/rest-parameters",
    "built-ins/Array/prototype/Symbol.iterator",
    "built-ins/String/prototype/Symbol.iterator",
    // ── Expressions: operators ──
    "language/expressions/addition",
    "language/expressions/assignment",
    "language/expressions/bitwise-and",
    "language/expressions/bitwise-not",
    "language/expressions/bitwise-or",
    "language/expressions/bitwise-xor",
    "language/expressions/call",
    "language/expressions/class",
    "language/expressions/coalesce",
    "language/expressions/comma",
    "language/expressions/complement",
    "language/expressions/conditional",
    "language/expressions/delete",
    "language/expressions/division",
    "language/expressions/does-not-equals",
    "language/expressions/equals",
    "language/expressions/exponentiation",
    "language/expressions/function",
    "language/expressions/greater-than",
    "language/expressions/greater-than-or-equal",
    "language/expressions/grouping",
    "language/expressions/in",
    "language/expressions/instanceof",
    "language/expressions/left-shift",
    "language/expressions/less-than",
    "language/expressions/less-than-or-equal",
    "language/expressions/logical-and",
    "language/expressions/logical-not",
    "language/expressions/logical-or",
    "language/expressions/member",
    "language/expressions/modulus",
    "language/expressions/multiplication",
    "language/expressions/new",
    "language/expressions/new.target",
    "language/expressions/object",
    "language/expressions/postfix-decrement",
    "language/expressions/postfix-increment",
    "language/expressions/prefix-decrement",
    "language/expressions/prefix-increment",
    "language/expressions/property-accessors",
    "language/expressions/right-shift",
    "language/expressions/strict-does-not-equals",
    "language/expressions/strict-equals",
    "language/expressions/subtraction",
    "language/expressions/this",
    "language/expressions/typeof",
    "language/expressions/unary-minus",
    "language/expressions/unary-plus",
    "language/expressions/unsigned-right-shift",
    "language/expressions/void",
    "language/expressions/arrow-function",
    // ── Statements ──
    "language/statements/block",
    "language/statements/break",
    "language/statements/class",
    "language/statements/const",
    "language/statements/continue",
    "language/statements/do-while",
    "language/statements/empty",
    "language/statements/expression",
    "language/statements/for",
    "language/statements/function",
    "language/statements/if",
    "language/statements/let",
    "language/statements/return",
    "language/statements/switch",
    "language/statements/throw",
    "language/statements/try",
    "language/statements/variable",
    "language/statements/while",
    // ── Literals ──
    "language/literals/boolean",
    "language/literals/null",
    "language/literals/numeric",
    "language/literals/string",
    // ── Other language areas ──
    "language/computed-property-names",
    "language/identifiers",
    "language/types",
    // ── Built-ins ──
    "built-ins/Array",
    "built-ins/Boolean",
    "built-ins/Error",
    "built-ins/Function",
    "built-ins/JSON",
    "built-ins/Math",
    "built-ins/NativeErrors",
    "built-ins/Number",
    "built-ins/Object",
    "built-ins/String",
    "built-ins/Symbol",
];

#[test]
fn test262_report() {
    if !has_tests() {
        eprintln!(
            "test262 not found at {:?}, skipping (run `git submodule update --init`)",
            test262_root()
        );
        return;
    }

    let filter = std::env::var("TEST262_SUITES").unwrap_or_default();
    let filters: Vec<&str> = filter
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let mut total = 0u32;
    let mut passed = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;
    let mut rows: Vec<(String, u32, u32, u32)> = Vec::new();

    for suite in SUITES {
        if !filters.is_empty() && !filters.iter().any(|f| suite.contains(f)) {
            continue;
        }
        let r = run_suite(suite);
        total += r.total;
        passed += r.passed;
        skipped += r.skipped;
        failed += r.failures.len() as u32;
        // Print progress as we go: a hard abort (e.g. allocation failure) in a
        // later suite would otherwise lose all information about earlier ones.
        println!(
            "[{:<48}] passed {:>4}  skipped {:>4}  failed {:>4}",
            suite,
            r.passed,
            r.skipped,
            r.failures.len()
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
        rows.push((suite.to_string(), r.passed, r.skipped, r.failures.len() as u32));

        // Failure details are printed when a suite filter is given, or when
        // `TEST262_FAILURES` is set explicitly (useful to aggregate reasons
        // across the whole run).
        let want_failures =
            !filters.is_empty() || std::env::var("TEST262_FAILURES").is_ok();
        if want_failures && !r.failures.is_empty() {
            let limit: usize = std::env::var("TEST262_FAILURES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(15);
            println!("\n--- {} failures ---", suite);
            for (id, reason) in r.failures.iter().take(limit) {
                println!("  FAIL {id}: {reason}");
            }
            if r.failures.len() > limit {
                println!("  … {} more", r.failures.len() - limit);
            }
        }
    }

    println!("\n================ test262 summary ================");
    println!(
        "{:<48} {:>7} {:>7} {:>7}",
        "suite", "passed", "skipped", "failed"
    );
    for (name, p, s, f) in &rows {
        println!("{name:<48} {p:>7} {s:>7} {f:>7}");
    }
    println!("------------------------------------------------");
    println!("{:<48} {:>7} {:>7} {:>7}", "TOTAL", passed, skipped, failed);
    println!(
        "pass rate over executed tests: {:.2}% ({} / {})",
        if total > skipped {
            passed as f64 / (total - skipped) as f64 * 100.0
        } else {
            0.0
        },
        passed,
        total - skipped
    );
    println!("=================================================");
}
