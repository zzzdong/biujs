//! Feature tests for the three runaway-execution guards (§7.2).
//!
//! They live in their own test binary because one of them needs a
//! `#[global_allocator]`, and a global allocator is per-binary: putting it in
//! `test262_runner` would tie the conformance numbers to this file's budget
//! fiddling, and putting it in the library would impose a policy on every
//! embedder.
//!
//! Guards are the difference between "this test fails" and "the whole run has
//! to be killed by hand" — they matter most exactly when the engine is broken,
//! which is why they are tested rather than trusted.

use biujs::{Compiler, RuntimeError, VM, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// The heap budget and its flag are *process-global*, so two of these tests
/// running in parallel fight over them (measured: a timeout test reported
/// "memory budget exceeded" because another test had lowered the budget, and
/// the heap test reported success because another test's `run` had cleared the
/// flag). `cargo test` runs test functions in threads by default, so the
/// serialization has to be in the code rather than in a flag on the command
/// line that a caller can forget.
static SERIAL: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ─────────────────────────────────────────────────────────
// A budget-watching allocator (same shape as the runner's)
// ─────────────────────────────────────────────────────────

static LIVE: AtomicUsize = AtomicUsize::new(0);
static BUDGET: AtomicUsize = AtomicUsize::new(usize::MAX);
static BUDGET_READ: AtomicBool = AtomicBool::new(false);

struct Watcher;

unsafe impl GlobalAlloc for Watcher {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if live > watch_budget() {
            biujs::vm::note_memory_pressure();
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static WATCHER: Watcher = Watcher;

/// The budget is only read from the environment once, and only when the test
/// asks for it: `env::var` allocates, and this runs inside the allocator (see
/// the long comment on the runner's copy of this function — a `OnceLock` here
/// deadlocks the process before its first test).
fn watch_budget() -> usize {
    if !BUDGET_READ.swap(true, Ordering::Relaxed) {
        let mb: usize = std::env::var("BIUJS_TEST_MEMORY_MB")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0);
        BUDGET.store(
            if mb == 0 { usize::MAX } else { mb * 1024 * 1024 },
            Ordering::Relaxed,
        );
    }
    BUDGET.load(Ordering::Relaxed)
}

fn set_budget(bytes: usize) {
    BUDGET_READ.store(true, Ordering::Relaxed);
    BUDGET.store(bytes, Ordering::Relaxed);
}

/// Compile and run, making sure the VM starts from a clean guard state.
fn run(source: &str, vm: VM) -> Result<Value, RuntimeError> {
    let mut compiler = Compiler::new();
    let module = compiler.compile(source).expect("compilation failed");
    let mut vm = vm;
    vm.run(&module)
}

fn expect_guard_error(source: &str, vm: VM, needle: &str) {
    match run(source, vm) {
        Err(RuntimeError::RangeError(msg)) => {
            assert!(
                msg.contains(needle),
                "expected a message containing {needle:?}, got {msg:?}"
            );
        }
        Ok(value) => panic!("expected a guard error, program returned {value:?}"),
        Err(other) => panic!("expected a RangeError, got {other}"),
    }
}

// ─────────────────────────────────────────────────────────
// Step budget
// ─────────────────────────────────────────────────────────

#[test]
fn step_budget_stops_a_hot_loop() {
    let _serial = exclusive();
    expect_guard_error(
        "var i = 0; while (true) { i = i + 1; }",
        VM::new().with_step_limit(Some(50_000)),
        "step limit exceeded",
    );
}

#[test]
fn step_budget_can_be_turned_off() {
    let _serial = exclusive();
    // The loop is finite, so with no budget it must simply finish — this is the
    // `TEST262_STEP_LIMIT=0` path used when hunting a hot loop.
    let value = run(
        "var i = 0; while (i < 1000) { i = i + 1; } i;",
        VM::new().with_step_limit(None),
    )
    .expect("expected the loop to finish");
    assert_eq!(value, Value::Number(1000.0));
}

// ─────────────────────────────────────────────────────────
// Wall-clock budget
// ─────────────────────────────────────────────────────────

#[test]
fn clock_budget_stops_a_slow_loop() {
    let _serial = exclusive();
    // Few enough steps to slip past a step budget, but slow enough (each
    // iteration touches a growing array) to run out of clock.
    expect_guard_error(
        "var a = []; for (var i = 0; i < 100000; i = i + 1) { a.push('x' + i); }",
        VM::new()
            .with_step_limit(None)
            .with_timeout(Some(Duration::from_millis(50))),
        "execution timeout exceeded after 50 ms",
    );
}

#[test]
fn clock_budget_is_armed_per_run() {
    let _serial = exclusive();
    // A host reuses the VM across tests, so a spent deadline must not leak into
    // the next `run` — otherwise every test after the first slow one would come
    // back as a timeout.
    let mut vm = VM::new()
        .with_step_limit(None)
        .with_timeout(Some(Duration::from_millis(50)));
    let mut compiler = Compiler::new();
    let module = compiler.compile("1 + 1;").expect("compilation failed");
    assert_eq!(vm.run(&module).unwrap(), Value::Number(2.0));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(vm.run(&module).unwrap(), Value::Number(2.0));
}

#[test]
fn a_spent_guard_cannot_be_caught_and_resumed() {
    let _serial = exclusive();
    // The guards are sticky on purpose. JS may catch the `RangeError` (test262
    // does that all the time), and without stickiness the very next instruction
    // would let the test crawl back into the loop the guard just stopped —
    // burning the *step* budget as well and reporting the wrong culprit.
    expect_guard_error(
        "var caught = false;
         try { while (true) {} } catch (e) { caught = true; }
         while (true) {}",
        VM::new()
            .with_step_limit(None)
            .with_timeout(Some(Duration::from_millis(50))),
        "execution timeout exceeded",
    );
}

// ─────────────────────────────────────────────────────────
// Heap budget
// ─────────────────────────────────────────────────────────

#[test]
fn heap_budget_stops_a_runaway_allocation() {
    let _serial = exclusive();
    // 4 MiB of budget against ~100k array elements: the allocation crosses the
    // line first, and the VM reports it as an ordinary failure instead of letting
    // the OS kill the process (which loses every result collected so far).
    set_budget(4 * 1024 * 1024);
    expect_guard_error(
        "var a = []; for (var i = 0; i < 200000; i = i + 1) { a.push('item-' + i); }",
        VM::new().with_step_limit(None),
        "memory budget exceeded",
    );
    // Restore a permissive budget: this binary's other tests share the process.
    set_budget(usize::MAX);
}

#[test]
fn heap_budget_flag_is_cleared_between_runs() {
    let _serial = exclusive();
    // The flag is process-global, so it has to be cleared when a run starts —
    // otherwise every test after a runaway one would fail for the wrong reason.
    set_budget(4 * 1024 * 1024);
    expect_guard_error(
        "var a = []; for (var i = 0; i < 200000; i = i + 1) { a.push('item-' + i); }",
        VM::new().with_step_limit(None),
        "memory budget exceeded",
    );
    set_budget(usize::MAX);
    let value = run("1 + 1;", VM::new()).expect("next run must succeed");
    assert_eq!(value, Value::Number(2.0));
}
