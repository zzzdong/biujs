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
//!
//! ## Guards (a runaway test must fail, not hang the run)
//!
//! Three limits, each catching a different failure mode. All of them turn into
//! an ordinary test *failure* with a recognisable message — the runner never
//! aborts the process, because that would throw away every result collected so
//! far (it used to happen: one runaway allocation under `ulimit -v` killed the
//! whole run).
//!
//! | Guard | Env var | Default | Catches |
//! |-------|---------|---------|---------|
//! | steps | `TEST262_STEP_LIMIT` (0 = off) | `DEFAULT_STEP_LIMIT` | hot infinite loops |
//! | clock | `TEST262_TIMEOUT_MS` (0 = off) | `10000` | slow loops, native stalls |
//! | heap | `TEST262_MEMORY_MB` (0 = off) | `256` MiB **per test** | runaway allocation |
//!
//! The heap budget is a per-test *delta* (armed against the live heap when the
//! test starts), because the harness itself holds hundreds of megabytes of
//! parsed tests; see `memory_allowance` for the measurement that forced this.
//!
//! `TEST262_TIMINGS=1` prints the slowest tests, which is how a sane default for
//! the clock guard gets chosen instead of guessed.

use biujs::{CompileError, Compiler, RuntimeError, VM, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use test262_harness::{Flag, Harness, Phase, Test};

// ─────────────────────────────────────────────────────────
// Memory guard
// ─────────────────────────────────────────────────────────

/// A `System` allocator that watches how much memory is *live* and flags the
/// engine when the budget is crossed.
///
/// It deliberately does not fail the allocation: `GlobalAlloc` has no way to
/// report an error, so the only alternatives would be aborting (kills the run's
/// results) or returning null (UB in Rust). Flagging instead lets the running
/// test fail with a `RangeError` while the harness stays alive — and because the
/// flag is cleared at the start of every `VM::run`, a single runaway test cannot
/// poison the rest of the suite.
struct BudgetAllocator {
    live: AtomicUsize,
}

impl BudgetAllocator {
    const fn new() -> Self {
        Self {
            live: AtomicUsize::new(0),
        }
    }
}

/// Absolute live-bytes ceiling the allocator compares against, armed per test
/// by `guarded_vm` (`usize::MAX` = no ceiling).
///
/// The allocator itself never touches the environment: it runs on every
/// allocation, and `env::var` allocates (see `guarded_vm` for why that matters).
static BUDGET_BYTES: AtomicUsize = AtomicUsize::new(usize::MAX);

unsafe impl GlobalAlloc for BudgetAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if live > BUDGET_BYTES.load(Ordering::Relaxed) {
            biujs::vm::note_memory_pressure();
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: BudgetAllocator = BudgetAllocator::new();

/// Live bytes right now (reported with the timings, to make a leak visible).
fn live_bytes() -> usize {
    ALLOCATOR.live.load(Ordering::Relaxed)
}

// ─────────────────────────────────────────────────────────
// Per-test guards
// ─────────────────────────────────────────────────────────

/// Heap a *single test* may allocate on top of what the harness already holds,
/// from `TEST262_MEMORY_MB` (default 256 MiB, `0` = off).
///
/// The budget is deliberately a per-test *delta*, not an absolute process cap:
/// the harness keeps every parsed test around, so an absolute cap fails whichever
/// test happens to run when the total crosses the line. Measured with an
/// absolute 1536 MiB cap: 57 failures spread across `Array`, `Math`,
/// `identifiers`, `String` … — all of them tests allocating a few kilobytes,
/// blamed for the harness's own hundreds of megabytes. What is worth flagging is
/// "this one test allocated 256 MiB", which is what a delta measures.
fn memory_allowance() -> usize {
    static ALLOWANCE: OnceLock<usize> = OnceLock::new();
    *ALLOWANCE.get_or_init(|| {
        let mb: usize = std::env::var("TEST262_MEMORY_MB")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(256);
        if mb == 0 {
            usize::MAX
        } else {
            // Leave room for the constant overhead by saturating, not wrapping.
            mb * 1024 * 1024
        }
    })
}

/// The VM every test runs in, carrying the three guards from the table above.
fn guarded_vm() -> VM {
    static STEP_LIMIT: OnceLock<Option<u64>> = OnceLock::new();
    static TIMEOUT: OnceLock<Option<Duration>> = OnceLock::new();

    // Read once and reuse: `env::var` per test would cost more than the guards.
    let limit = *STEP_LIMIT.get_or_init(|| match std::env::var("TEST262_STEP_LIMIT") {
        // An explicit 0 means "no limit" — useful when hunting a hot loop.
        Ok(raw) => match raw.parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => Some(biujs::vm::DEFAULT_STEP_LIMIT),
        },
        Err(_) => Some(biujs::vm::DEFAULT_STEP_LIMIT),
    });
    let timeout = *TIMEOUT.get_or_init(|| {
        let ms: u64 = std::env::var("TEST262_TIMEOUT_MS")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(10_000);
        if ms == 0 {
            None
        } else {
            Some(Duration::from_millis(ms))
        }
    });

    // Arm the heap guard relative to *now*, i.e. to this test's start.
    BUDGET_BYTES.store(
        live_bytes().saturating_add(memory_allowance()),
        Ordering::Relaxed,
    );

    VM::new().with_step_limit(limit).with_timeout(timeout)
}

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

/// The `tests/test262` submodule directory (the superproject's checkout).
fn test262_submodule_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262")
}

/// The revision the submodule is currently checked out at.
///
/// `None` when git is unavailable or the submodule is not initialised.
fn test262_revision() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(test262_submodule_root())
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The revision recorded in `tests/test262.pin`: the frozen conformance
/// baseline every published pass rate was measured against.
fn pinned_test262_revision() -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test262.pin");
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("rev") else {
            continue;
        };
        if let Some(value) = rest.trim().strip_prefix('=') {
            return Some(value.trim().to_string());
        }
    }
    None
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
    let source = parts.join("\n");

    // test262's `onlyStrict` tests carry no `"use strict"` of their own — the
    // runner supplies it, and it has to be the *first* statement of the
    // assembled program or it is an ordinary expression statement and the whole
    // program stays sloppy. Without this, a test whose entire point is a strict
    // early error (`for ({ eval } of [])`, `function f(a, a) {}`, …) was parsed
    // as sloppy code and the engine was asked to accept it.
    if test.desc.flags.contains(&Flag::OnlyStrict) {
        return format!("\"use strict\";\n{source}");
    }
    source
}

// ─────────────────────────────────────────────────────────
// Filtering
// ─────────────────────────────────────────────────────────

/// Two lists, because "skipped" has two different meanings. Conflating them
/// is how a skip table stops telling the truth (see §2.1y of
/// `es6-conformance-plan.md`):
///
/// * `OUT_OF_SCOPE_FEATURES` — outside the ES6 target, or excluded by G3
///   forever. These tests are expected to stay skipped indefinitely.
/// * `IN_SCOPE_PENDING` — **inside** the ES6 target but not implemented yet.
///   Skipped only because the feature is missing. The phase-2 plan
///   (`docs/es6-conformance-phase2.md` §2.2) makes removing an entry from
///   this list part of the feature's definition of done: implement the
///   feature, drop the tag here, and add the suite to `SUITES` if it is not
///   there yet. Doing only the first step is wasted work — the progress never
///   reaches a single number (measured: adding the M5/M6 suites without
///   unlocking anything moved 17477 executed / 13211 passed to 19029 / 13211).
const OUT_OF_SCOPE_FEATURES: &[&str] = &[
"async-iteration",
"async-functions",
"Atomics",
// ES2019 (`Array.prototype.flatMap`), unimplemented and outside the ES6
// target. It is listed *explicitly* even though the loose matching below used
// to hide it anyway — `feature.contains("Map")` matched
// `"Array.prototype.flatMap"`, so dropping `"Map"` from `IN_SCOPE_PENDING`
// (B2a) silently put 20 unimplemented tests back into the pool. An accidental
// skip is not a scope decision; this one is.
"Array.prototype.flatMap",
"BigInt",
"class-fields-private",
"class-methods-private",
"class-static-methods-private",
"class-static-fields-private",
"class-static-block",
"cross-realm",
"dynamic-import",
"import.meta",
"import-assertions",
"Intl",
"json-parse-with-source",
"logical-assignment",
"modules",
"numeric-separator",
"optional-chaining",
"reflect-metadata",
"regexp-",
"SharedArrayBuffer",
"String.prototype.replaceAll",
"symbol-description",
"tail-call-optimization",
"Temporal",
"WeakRef",
"u180e",
"well-formed-json-stringify",
];

/// In scope (§1.2), not implemented yet. Sizes as of 2026-09-25 (test262 files):
/// Map 204 / Set 383 / WeakMap 141 / WeakSet 85 / Promise 677 / Proxy 311 /
/// Reflect 153 / TypedArray 2184.
///
/// `Map` left this list with B2a, `Set` with B2b and the weak collections with
/// B3 (all four suites are in `SUITES`); the others stay gated until the
/// feature exists.
const IN_SCOPE_PENDING: &[&str] = &[
"Promise",
"Proxy",
"Reflect",
"TypedArray",
];

/// Every feature tag whose tests this engine currently cannot pass.
///
/// The match is `starts_with` **or** `contains`, which is deliberately loose
/// (a `regexp-` entry covers every `regexp-*` tag) but has a sharp edge:
/// `"Array.prototype.flatMap".contains("Map")` is true, so an entry for a
/// *shorter* name also skips unrelated tags that merely embed it. Adding or
/// removing an entry therefore has to be checked against the whole table —
/// see the comment on `"Array.prototype.flatMap"` in `OUT_OF_SCOPE_FEATURES`.
fn is_unsupported(feature: &str) -> bool {
    OUT_OF_SCOPE_FEATURES
        .iter()
        .chain(IN_SCOPE_PENDING.iter())
        .any(|f| feature.starts_with(f) || feature.contains(f))
}

/// Source patterns biujs cannot handle at all.
const UNSUPPORTED_PATTERNS: &[&str] = &[
    "eval(",
    // `JSON` used to be listed here while it was unimplemented; it is now a
    // real built-in, so tests that exercise it must run.
    "Date",
    "RegExp",
    // `$DONOTEVALUATE` is *not* unsupported: these are ordinary negative tests.
    // The harness makes `$DONOTEVALUATE` throw, so the call is only reached if
    // the engine wrongly accepted the syntax it was supposed to reject — and the
    // runner already handles `negative:` metadata (§2.1y).
    "new Function(",
    "Function(\"",
    "Function('",
    "$ERROR",
    "print(",
    // `yield ` used to be listed here while generators were unimplemented; the
    // subset in §2.1p handles them, so tests exercising `yield` must run.
    "await ",
    "import(",
    "import ",
    "export ",
    "with (",
    "with(",
    "label:",
    // `function*` used to be listed here while generators were unimplemented;
    // the M4-G1 subset handles them, so generator sources must run.
    "=>*",
    "async ",
];

fn should_skip(test: &Test) -> Option<String> {
    if !test.source.contains("/*---") {
        return Some("no test262 metadata".to_string());
    }

    for flag in &test.desc.flags {
        match flag {
            // `Async` and `Module` are real gating conditions: the source needs
            // a syntax this engine deliberately does not have (G3).
            //
            // `Generated` is *not*. It only records that the file was produced
            // by a tool rather than typed by hand; such tests are ordinary
            // tests and have to run. Skipping them silently dropped ~5k tests
            // (463 of the 556 generator tests alone) — see §2.1y.
            Flag::Async | Flag::Module => {
                return Some(format!("flag {flag:?}"));
            }
            _ => {}
        }
    }

    for feature in &test.desc.features {
        if is_unsupported(feature) {
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
                let mut vm = guarded_vm();
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
                        let mut vm = guarded_vm();
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
    /// Per-test wall-clock time, kept only when `TEST262_TIMINGS` is set.
    timings: Vec<(Duration, String)>,
}

/// Classify a failure message produced by one of the three guards.
///
/// The counts are reported next to the total: a suite whose failures are mostly
/// "step limit exceeded" is a different problem from one whose failures are
/// wrong values, and the difference decides whether a lower budget (fast signal)
/// or an engine fix is wanted.
fn guard_kind(reason: &str) -> Option<&'static str> {
    if reason.contains("step limit exceeded") {
        Some("step-limit")
    } else if reason.contains("execution timeout exceeded") {
        Some("timeout")
    } else if reason.contains("memory budget exceeded") {
        Some("memory")
    } else {
        None
    }
}

fn run_suite(subdir: &str) -> SuiteResult {
    let collect_timings = std::env::var("TEST262_TIMINGS").is_ok();
    let mut result = SuiteResult {
        total: 0,
        passed: 0,
        skipped: 0,
        failures: Vec::new(),
        timings: Vec::new(),
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

        // `BIUJS_TEST262_TRACE=1` prints each test as it starts. A crash inside
        // the engine (a Rust stack overflow, say) aborts the process and would
        // otherwise leave no clue which test did it.
        if std::env::var("BIUJS_TEST262_TRACE").is_ok() {
            println!("[run] {}", test.path.display());
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
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_test(&test)))
            .unwrap_or_else(|payload| {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".to_string());
                Err(format!("engine panic: {msg}"))
            });
        if collect_timings {
            let elapsed = started.elapsed();
            // Flag the guard hits in the timing list too: "slow *and* caught by
            // the clock" is the combination worth looking at first.
            let label = match guard_kind(outcome.as_ref().err().map_or("", |e| e.as_str())) {
                Some(kind) => format!("{id}  [{kind}]"),
                None => id.clone(),
            };
            result.timings.push((elapsed, label));
        }

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
    "language/statements/generators",
    "language/expressions/generators",
    "language/expressions/yield",
    "language/statements/for-in",
    "language/destructuring",
    "language/expressions/assignment/destructuring",
    // The rest-parameter tests live in `language/rest-parameters`; the old entry
    // named a directory that does not exist, so the suite silently reported
    // 0/0/0 — a coverage hole that was invisible because a missing directory is
    // not an error.
    "language/rest-parameters",
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
    // `built-ins/Map` and `built-ins/Set` are in the denominator from B2a/B2b
    // on (§2.2: implementing a feature and enrolling its suite happen in the
    // same batch).
    "built-ins/Map",
    "built-ins/Set",
    "built-ins/WeakMap",
    "built-ins/WeakSet",
    "built-ins/Math",
    "built-ins/NativeErrors",
    "built-ins/Number",
    "built-ins/Object",
    "built-ins/String",
    "built-ins/Symbol",
];

/// The submodule must stay at the revision the baselines were measured on.
///
/// Moving test262 silently changes the denominator (and the contents) of every
/// published pass rate, so bumping it is a deliberate change: checkout the new
/// revision, re-run the full suite, record the numbers, and update
/// `tests/test262.pin` plus the docs in the same commit (§6.7).
#[test]
fn test262_at_pinned_revision() {
    if !has_tests() {
        eprintln!("test262 submodule not initialised, skipping the revision check");
        return;
    }
    let Some(actual) = test262_revision() else {
        eprintln!("git unavailable, skipping the test262 revision check");
        return;
    };
    let pinned =
        pinned_test262_revision().expect("tests/test262.pin must record a `rev = <sha>` line");
    assert_eq!(
        actual,
        pinned,
        "tests/test262 drifted away from the pinned conformance baseline; \
         re-run the full suite, then update tests/test262.pin and the docs (§6.7)"
    );
}

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
    let mut guards: HashMap<&'static str, u32> = HashMap::new();
    let mut slowest: Vec<(Duration, String)> = Vec::new();
    let suite_clock = Instant::now();

    for suite in SUITES {
        if !filters.is_empty() && !filters.iter().any(|f| suite.contains(f)) {
            continue;
        }
        let r = run_suite(suite);
        total += r.total;
        passed += r.passed;
        skipped += r.skipped;
        failed += r.failures.len() as u32;
        for (_, reason) in &r.failures {
            if let Some(kind) = guard_kind(reason) {
                *guards.entry(kind).or_insert(0) += 1;
            }
        }
        if !r.timings.is_empty() {
            slowest.extend(r.timings.iter().cloned());
            slowest.sort_by(|a, b| b.0.cmp(&a.0));
            slowest.truncate(10);
        }
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
    // Print the revision the numbers belong to: a pass rate is only meaningful
    // together with the test suite it was measured on (§6.7).
    match (test262_revision(), pinned_test262_revision()) {
        (Some(actual), Some(pinned)) if actual == pinned => {
            println!("test262 revision: {actual} (pinned baseline)");
        }
        (Some(actual), Some(pinned)) => println!(
            "test262 revision: {actual} — NOT the pinned baseline {pinned} (see docs §6.7)"
        ),
        (Some(actual), None) => println!("test262 revision: {actual} (no tests/test262.pin)"),
        (None, _) => println!("test262 revision: unknown (git unavailable)"),
    }
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

    // The guards exist so that a runaway test becomes a reported failure. Say
    // how often each of them fired, otherwise "failed 3906" hides the fact that
    // part of it is the harness protecting itself rather than a spec mismatch.
    println!(
        "guards: timeout {}, step-limit {}, memory {}   \
         (TEST262_TIMEOUT_MS / TEST262_STEP_LIMIT / TEST262_MEMORY_MB, 0 = off)",
        guards.get("timeout").copied().unwrap_or(0),
        guards.get("step-limit").copied().unwrap_or(0),
        guards.get("memory").copied().unwrap_or(0),
    );

    if !slowest.is_empty() {
        println!(
            "\n-- slowest tests (wall clock; whole run {:.1}s, live heap {:.0} MiB) --",
            suite_clock.elapsed().as_secs_f64(),
            live_bytes() as f64 / (1024.0 * 1024.0)
        );
        for (elapsed, id) in &slowest {
            println!("  {:>8.2}s  {}", elapsed.as_secs_f64(), id);
        }
    }
    println!("=================================================");
}
