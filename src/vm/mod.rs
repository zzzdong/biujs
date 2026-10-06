pub mod iterator;
pub mod object;
pub mod property;
pub mod prototype;
pub mod value;

pub use object::{
    ArrayObject, FunctionObject, JSObject, NativeFunctionObject, OrdinaryObject, new_array_object,
    new_array_object_from_vec, new_function_object, new_ordinary_object,
};
pub use property::{ObjectKind, PropertyDescriptor, PropertyKey};
pub use value::Value;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::builtins::Builtins;
use crate::vm::object::{PromiseObject, PromiseReaction, PromiseState};
use crate::vm::iterator::iterator_symbol_key;
use crate::bytecode::{
    Constant, FunctionId, Instr, Module, Opcode, Operand, Primitive, Register,
};

/// Size of the pre-allocated value stack. Slots are addressed directly, so the
/// whole frame area is materialized up front (it used to be a tiny 4 KiB stack,
/// which deep programs overflowed instantly).
const STACK_MAX: usize = 1 << 18;

/// Maximum JS call depth. Exceeding it raises `RangeError: Maximum call stack
/// size exceeded` instead of exhausting host memory.
const MAX_CALL_DEPTH: usize = 512;

/// Default instruction budget for a `run`. A runaway `while` loop therefore
/// reports an error instead of spinning forever. `None` disables the budget.
pub const DEFAULT_STEP_LIMIT: u64 = 20_000_000;

/// How many instructions may pass between two polls of the wall-clock and
/// memory guards. `Instant::now()` costs orders of magnitude more than a
/// bytecode step, so polling every step would tax every test; a runaway loop
/// still gets stopped after at most this many instructions.
const GUARD_CHECK_INTERVAL: u64 = 4096;

/// Set by a host-side allocation hook when a soft memory budget is crossed.
///
/// The engine never aborts the process on its own: a `GlobalAlloc` cannot
/// report failure, so the runner installs an allocator that *flags* the
/// overshoot and the VM turns the flag into an ordinary `RangeError` for the
/// running test (`VM::run` clears it on entry, so one bad test cannot poison
/// the next). Without this, a runaway allocation either got killed by the
/// OS/`ulimit` — losing the whole run's results — or paged the machine.
static MEMORY_PRESSURE: AtomicBool = AtomicBool::new(false);

/// Flag that the host's memory budget has been crossed (see `MEMORY_PRESSURE`).
pub fn note_memory_pressure() {
    MEMORY_PRESSURE.store(true, Ordering::Relaxed);
}

/// Whether the host reported memory pressure since the current `run` started.
pub fn memory_pressure() -> bool {
    MEMORY_PRESSURE.load(Ordering::Relaxed)
}

/// Clear the memory-pressure flag. `VM::run` does this on entry.
pub fn clear_memory_pressure() {
    MEMORY_PRESSURE.store(false, Ordering::Relaxed);
}

/// Which guard ended the current `run`.
///
/// The answer is *sticky*: once the clock or the heap budget is spent, every
/// later instruction fails with the same error instead of re-evaluating the
/// guard. Otherwise a test that catches the guard's `RangeError` in JS — common
/// in test262, where `assert.throws` and `try/finally` abound — would keep
/// running (and, for the heap guard, keep allocating) until the *step* budget
/// ran out as well: the failure then names the wrong culprit, and a tiny budget
/// turned a 0.3 s suite into a multi-minute one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GuardFired {
    Timeout,
    Memory,
}

impl GuardFired {
    fn message(self, budget: Option<Duration>) -> String {
        match self {
            GuardFired::Timeout => format!(
                "execution timeout exceeded after {} ms (possible infinite loop)",
                budget.map_or(0, |d| d.as_millis())
            ),
            GuardFired::Memory => {
                "memory budget exceeded (possible runaway allocation)".to_string()
            }
        }
    }
}

/// Prefix of the synthetic name given to bound-function wrappers.
const BOUND_PREFIX: &str = "__bound__";

/// Prefix of the synthetic name given to the `revoke` function of
/// `Proxy.revocable`. The id after it indexes [`VM::revocable_proxies`]: the
/// function has to know *which* proxy it revokes, and a native's only state is
/// its name.
const REVOKE_PREFIX: &str = "__revoke__";

/// Whether an operand denotes a writable storage location.
fn is_addressable(operand: Operand) -> bool {
    matches!(operand, Operand::Register(_) | Operand::Stack(_))
}

/// Whether `val` is a string primitive or a String wrapper object.
///
/// `Array.prototype.indexOf.call(value, …)` and `value.indexOf(…)` reach the
/// same dispatch site, so the string case has to be told apart: strings use
/// substring search, arrays use per-element comparison.
fn receiver_is_string(val: &Value) -> bool {
    match val {
        Value::String(_) => true,
        Value::Object(obj_ref) => {
            obj_ref.borrow().kind() == crate::vm::property::ObjectKind::String
        }
        _ => false,
    }
}



/// JavaScript Virtual Machine
///
/// Executes bytecode modules produced by the Compiler.
/// Ported from evalit's register-based VM with JS-specific Value semantics.
pub struct VM {
    state: State,
    builtins: Builtins,
    /// Live iterators created by `MakeIterator`, keyed by registry id. The
    /// user-facing `next()`/`return()` natives look iterators up here.
    iterator_registry: HashMap<u64, Value>,
    next_iterator_id: u64,
    /// Live generator objects, keyed by registry id. `gen.next()` is a native
    /// (`__gen_next__<id>`), so the resume path has to find the object here.
    generator_registry: HashMap<u64, Value>,
    next_generator_id: u64,
    /// The value the *current* resume supplied (`next(v)`), consumed by
    /// `Opcode::Yield` as the value of the `yield` expression.
    generator_send: Value,
    /// Set while a nested generator loop runs: `None` until the body suspends,
    /// then `Some(yielded value)`.
    generator_yielded: Option<Value>,
    /// Index of the `Yield` instruction that suspended the frame. Captured
    /// before the handler jumps past the last instruction, so the resume path
    /// can continue at `pc + 1`.
    generator_yield_pc: usize,
    /// Iterators opened by `yield*` in the frames currently running, innermost
    /// last. An abrupt completion of a generator closes everything it opened.
    delegate_stack: Vec<Value>,
    /// Function metadata (`name`, parameter count) of the module being run.
    /// Kept here so `materialize_function` can set `fn.name` / `fn.length`
    /// without threading the module through every call site.
    current_module_info: Option<HashMap<u32, (String, usize)>>,
    /// Memoized `FunctionObject` per bytecode function id.
    ///
    /// `Value::Function(id)` is a bare function *reference*; whenever it is used
    /// as a value (property access, `new`, `instanceof`, …) it must be boxed into
    /// a real object. Boxing must be idempotent: a function's `prototype` object
    /// is created once and shared, otherwise `F.prototype.x = 1` would be lost the
    /// next time `F` is materialized.
    func_objs: HashMap<u32, Value>,
    /// Instruction budget for the current run (`None` = unlimited).
    step_limit: Option<u64>,
    /// Instructions executed so far in the current run.
    steps: u64,
    /// Wall-clock budget for the current run (`None` = unlimited).
    timeout: Option<Duration>,
    /// Instant the current run must finish by (`None` = no clock guard).
    deadline: Option<Instant>,
    /// Step count at which the clock / memory guards are polled next.
    next_guard_check: u64,
    /// Sticky guard failure for the current run (see `GuardFired`).
    guard_fired: Option<GuardFired>,
    /// Set while a generator's *parameter* prologue runs eagerly (see
    /// `create_generator`): `Opcode::PrologueEnd` then parks the frame and stops
    /// the nested loop instead of being a no-op.
    running_generator_prologue: bool,
    /// Bound functions created by `Function.prototype.bind`, keyed by a
    /// synthetic id that is embedded in the wrapper's name.
    bound_functions: HashMap<u32, BoundFunction>,
    next_bound_id: u32,
    /// The proxy each `revoke` function of `Proxy.revocable` belongs to, keyed
    /// by the id embedded in the function's synthetic name.
    revocable_proxies: HashMap<u32, Value>,
    next_revoke_id: u32,
    /// Control-stack depth of the caller for each frame driven by
    /// `invoke_with_new_target` / `invoke_construct` (Rust-driven calls: native
    /// callbacks, `call`/`apply`, `new` from a native).
    ///
    /// An exception raised inside such a frame must *not* be dispatched to a
    /// handler in the outer frame from within the nested loop: the native that
    /// requested the call (e.g. `Array.prototype.forEach`) has to see the error
    /// so it can abandon its own work. Records at or below the boundary are
    /// therefore turned back into `RuntimeError::Thrown` and propagated.
    invoke_boundaries: Vec<usize>,
}

impl VM {
    /// Control-stack depth outside the innermost Rust-driven call frame, if any.
    fn invoke_boundary(&self) -> Option<usize> {
        self.invoke_boundaries.last().copied()
    }

    /// Whether `val` carries the `length` + integer-key shape the
    /// `Array.prototype` methods operate on (real arrays, array-likes, string
    /// wrappers). Strings are excluded: they use substring search.
    fn receiver_is_array_like(&self, val: &Value) -> bool {
        if receiver_is_string(val) {
            return false;
        }
        // Primitive receivers count too: they are boxed by ToObject, and the
        // wrapper's prototype may carry the `length` *and* the index properties
        // (`Boolean.prototype.length = 1` is exactly how
        // `Array.prototype.reduce.call(false, cb, 1)` is set up to have one
        // element).
        let boxed;
        let target = match val {
            Value::Object(_) | Value::Function(_) => val,
            Value::Undefined | Value::Null => return false,
            other => {
                boxed = crate::builtins::to_object(other).ok();
                match &boxed {
                    Some(value) => value,
                    None => return false,
                }
            }
        };
        match target {
            Value::Object(obj_ref) => crate::vm::prototype::internal_has_property(
                Rc::clone(obj_ref),
                &PropertyKey::from_str("length"),
            )
            .unwrap_or(false),
            _ => false,
        }
    }
}

/// Result of `Function.prototype.bind`: a target plus the pre-bound `this` and
/// leading arguments.
struct BoundFunction {
    target: Value,
    this_arg: Value,
    bound_args: Vec<Value>,
}

/// 一帧是怎么开的：`[[Call]]` 还是 `[[Construct]]`。
///
/// 差别只有四处，见 [`Self::drive_bytecode_frame`] 的说明。把"哪种模式"做成枚举
/// （而不是再抄一份 100 行的驱动代码）就是 P1 的目的。
enum FrameMode {
    Call {
        /// 传进来的 `this`（绑定函数/箭头函数解析之后的）。
        this_val: Value,
        /// 箭头函数捕获的 `new.target`（ES 9.2.2）。
        captured_new_target: Option<Value>,
        /// `super(...)` 带来的显式 `new.target`。
        new_target_override: Option<Value>,
    },
    Construct {
        /// 新建的实例，也就是构造器的 `this`。
        new_obj: Value,
        /// 帧的 `function_val` 与 `new.target`（`Reflect.construct` 的第三个参数）。
        new_target: Value,
    },
}

/// 一个可调用体的种类（P1-2a）。判定的**唯一**出口是 [`VM::callee_kind`]。
///
/// 以前"是不是原生 / 代理 / 生成器 / async"这套判断散在 `invoke_with_new_target`、
/// `CallEx`、`CallMethod`、`New`、`Call` 五处，**每处写法还不一样**。B45（代理可调用）
/// 与 B46（`Reflect.construct` 的 `new.target`）之所以要在好几处分别接线，就是从这些
/// 分叉漏出去的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalleeKind {
    /// 代理：`[[Call]]`/`[[Construct]]` 由陷阱决定（ES 10.5.12）。
    Proxy,
    /// 原生内建（含绑定函数：它们的名字带 `__bound__` 前缀），按名字分发。
    Native,
    /// 生成器函数：调用只造迭代器、不跑函数体，而且**不是构造器**。
    Generator,
    /// `async` 函数：调用答一个 promise。
    Async,
    /// 普通字节码函数。
    Bytecode,
    /// 不可调用。
    NotCallable,
}

impl VM {
    pub fn new() -> Self {
        let mut state = State::new();
        let builtins = Builtins::new();
        builtins.register(&mut state.globals.borrow_mut());
        // `globalThis.globalThis === globalThis`, as in every host.
        state
            .globals
            .borrow_mut()
            .insert("globalThis".to_string(), state.global_object.clone());
        Self {
            state,
            builtins,
            iterator_registry: HashMap::new(),
            next_iterator_id: 0,
            generator_registry: HashMap::new(),
            next_generator_id: 0,
            generator_send: Value::Undefined,
            generator_yielded: None,
            generator_yield_pc: 0,
            delegate_stack: Vec::new(),
            current_module_info: None,
            func_objs: HashMap::new(),
            step_limit: Some(DEFAULT_STEP_LIMIT),
            steps: 0,
            timeout: None,
            deadline: None,
            next_guard_check: GUARD_CHECK_INTERVAL,
            guard_fired: None,
            running_generator_prologue: false,
            bound_functions: HashMap::new(),
            next_bound_id: 0,
            revocable_proxies: HashMap::new(),
            next_revoke_id: 0,
            invoke_boundaries: Vec::new(),
        }
    }

    /// Builder-style override of the instruction budget.
    ///
    /// A budget turns a runaway loop (usually an engine bug) into a
    /// `RangeError` instead of an unbounded spin.
    pub fn with_step_limit(mut self, limit: Option<u64>) -> Self {
        self.step_limit = limit;
        self
    }

    /// Builder-style override of the wall-clock budget.
    ///
    /// A *counted* budget (steps) and a *timed* one catch different runaways:
    /// a loop that is slow because each iteration does real work still burns
    /// steps fast, but a test that blocks in a long native routine does not.
    /// This is the guard a test harness needs — without it a hung test simply
    /// never returns and the whole run has to be killed by hand.
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Execute a bytecode module and return the result
    pub fn run(&mut self, module: &Module) -> Result<Value, RuntimeError> {
        // Break the `globals` ⇄ `globalThis` cycle *before* dropping this run's
        // State: the map holds the global object and the object holds the map, so
        // with no GC the pair (and the whole State with it) would survive.
        self.state.globals.borrow_mut().remove("globalThis");
        self.state = State::new();
        // Break each boxed function's `prototype` ⇄ `constructor` cycle before
        // dropping the map: a function object and its `prototype` object point at
        // each other, so the pair survives the map being cleared (no GC) and
        // takes the whole program's graph with it.
        for value in self.func_objs.values() {
            if let Value::Object(obj_ref) = value {
                let _ = obj_ref
                    .borrow_mut()
                    .property_delete(&crate::vm::property::PropertyKey::from_str("prototype"));
            }
        }
        self.func_objs.clear();
        self.current_module_info = Some(module.func_info.clone());
        self.bound_functions.clear();
        self.next_bound_id = 0;
        // Same reason as above: a revoked proxy from the previous run must not
        // survive into the next one.
        self.revocable_proxies.clear();
        self.next_revoke_id = 0;
        self.invoke_boundaries.clear();
        // Everything else a run *touches* has to be reset too, or a reused VM
        // carries the previous program across. These five were the leak: the
        // registries hold every iterator / generator the program made, so a host
        // that reuses one VM (which the contract says it may) accumulated the
        // whole object graph of every run — measured as a shard that could not
        // finish under a 1 GiB `ulimit`, and as *different results* when the
        // runner was switched to reuse (15972 → 14002 passing).
        self.iterator_registry.clear();
        self.next_iterator_id = 0;
        self.generator_registry.clear();
        self.next_generator_id = 0;
        self.generator_send = Value::Undefined;
        self.generator_yielded = None;
        self.generator_yield_pc = 0;
        self.delegate_stack.clear();
        // A prologue that was interrupted left this set; the next program's
        // first generator would then stop at its own `PrologueEnd`.
        self.running_generator_prologue = false;
        self.steps = 0;
        self.next_guard_check = GUARD_CHECK_INTERVAL;
        self.guard_fired = None;
        // The clock guard is armed per `run`, not per `VM`: a host reuses the
        // VM across tests, and each test should get the full budget.
        self.deadline = self.timeout.map(|budget| Instant::now() + budget);
        // A flag left over from `run` *preparation* (harness setup, the previous
        // test's teardown) must not be blamed on the program about to run.
        clear_memory_pressure();
        // Re-register builtins into the fresh state
        self.builtins.register(&mut self.state.globals.borrow_mut());
        self.state
            .globals
            .borrow_mut()
            .insert("globalThis".to_string(), self.state.global_object.clone());

        while self.step(module)? {}

        // Promises settle through jobs, and there is no event loop: the end of
        // the top-level run is the point where the microtask queue drains.
        self.run_promise_jobs(module)?;

        // If we run out of instructions, return Rv
        Ok(self
            .state
            .get_register(Register::Rv)
            .unwrap_or(Value::Undefined))
    }

    /// Read a global by name from the state left behind by the last `run`.
    ///
    /// The state (and so every `var` / function declaration the program made) is
    /// reset at the *start* of a run, so after `run` returns this reflects the
    /// program that just finished — including mutations the drained microtask
    /// queue performed. That is how a host observes an async outcome: the test
    /// records it in a global, then the host reads that global.
    pub fn global(&self, name: &str) -> Option<Value> {
        self.state.globals.borrow().get(name).cloned()
    }

    /// Box a bare `Value::Function(id)` into a stable `FunctionObject`.
    ///
    /// The mapping is memoized for the lifetime of a `run` so that `F.prototype`
    /// and any properties hung off the function object remain observable.
    /// Pop the pending `ClosureVar` entries for the function object being
    /// created, in the order the lowering emitted them.
    ///
    /// `ClosureVar` pushes one-entry maps; an empty map is the boundary that
    /// says "the current frame's captures start here" (pushed when a function
    /// frame opens), so the walk stops there and never steals an outer frame's
    /// captures.
    fn take_pending_captured_vars(&mut self) -> Vec<(String, Value)> {
        let mut captured_vars: Vec<(String, Value)> = Vec::new();
        while let Some(vars) = self.state.closure_var_stack.last() {
            if vars.is_empty() {
                break;
            }
            let entry = self.state.closure_var_stack.pop().unwrap();
            for (k, v) in entry {
                captured_vars.push((k, v));
            }
        }
        captured_vars
    }

    /// A fresh (never memoized) function object carrying `captured_vars`.
    ///
    /// Only used when a `function` expression actually captures something: the
    /// memoized object (`materialize_function`) is keyed by `func_id` and stands
    /// for "the" object of a declaration, which cannot carry per-evaluation
    /// captures.
    fn make_capturing_function_object(
        &mut self,
        func_id: u32,
        captured_vars: Vec<(String, Value)>,
    ) -> Value {
        let (name, arity) = self
            .current_module_info
            .as_ref()
            .and_then(|info| info.get(&func_id))
            .cloned()
            .unwrap_or((String::new(), 0));
        let obj = crate::vm::object::new_function_object(func_id, &name, arity);
        if let Value::Object(ref obj_ref) = obj {
            let mut borrowed = obj_ref.borrow_mut();
            borrowed.set_prototype(Some(Rc::clone(&self.builtins.function_prototype)));
            if let Some(func_obj) = borrowed.as_any_mut().downcast_mut::<FunctionObject>() {
                func_obj.captured_vars = captured_vars;
            }
        }
        obj
    }

    fn materialize_function(&mut self, id: u32) -> Value {
        if let Some(v) = self.func_objs.get(&id) {
            return v.clone();
        }
        let name = self
            .current_module_info
            .as_ref()
            .and_then(|info| info.get(&id))
            .cloned();
        let (name, arity) = match name {
            Some((name, arity)) => (name, arity),
            None => (String::new(), 0),
        };
        // `fn.length` / `fn.name` / `fn.prototype` are installed by the
        // constructor (non-writable, non-enumerable, configurable).
        let obj = crate::vm::object::new_function_object(id, &name, arity);
        // Attach Function.prototype so `f.call`, `f.bind`, … resolve.
        if let Value::Object(ref obj_ref) = obj {
            obj_ref
                .borrow_mut()
                .set_prototype(Some(Rc::clone(&self.builtins.function_prototype)));
        }
        self.func_objs.insert(id, obj.clone());
        obj
    }

    /// Coerce a value that is about to be used as an object into a `Value::Object`
    /// where possible (currently: bare function references).
    /// ES `SetFunctionName(fn, name)` — redefine the `name` own property
    /// `{ writable: false, enumerable: false, configurable: true }`.
    ///
    /// Only functions and classes have a `name` to set, and the lowering emits
    /// this instruction only for the anonymous function/class forms, so anything
    /// else is left untouched. Redefinition is legal because the spec makes
    /// `name` configurable — that is what lets `SetFunctionName` work at all.
    fn set_function_name(&mut self, func: &Value, name: &str) {
        let obj_val = self.as_object_value(func);
        if let Value::Object(obj_ref) = obj_val {
            let mut borrowed = obj_ref.borrow_mut();
            if borrowed.kind() != ObjectKind::Function {
                return;
            }
            borrowed
                .define_property(
                    crate::vm::property::PropertyKey::from_str("name"),
                    crate::vm::property::PropertyDescriptor {
                        value: Value::string(name),
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        getter: None,
                        setter: None,
                    },
                )
                .ok();
        }
    }

    fn as_object_value(&mut self, val: &Value) -> Value {
        match val {
            Value::Function(id) => self.materialize_function(*id),
            other => other.clone(),
        }
    }

    /// Execute a single bytecode instruction. Returns `Ok(false)` when execution
    /// should stop (Halt, or the program counter is out of bounds), and `Ok(true)`
    /// to continue. This is factored out so that other methods can run a nested
    /// execution loop (e.g. for re-entrant function calls such as ToPrimitive).
    fn step(&mut self, module: &Module) -> Result<bool, RuntimeError> {
        // Borrowed, not cloned: `module` and `self` are distinct, so the
        // instruction can be matched in place — no 48-byte copy per step.
        let inst = match module.instructions.get(self.state.pc) {
            Some(i) => i,
            None => return Ok(false),
        };
        // A spent guard ends the run for good: JS may catch the `RangeError`,
        // but the next instruction fails with it again, so the test cannot
        // crawl back into the loop the guard just stopped.
        if let Some(fired) = self.guard_fired {
            return Err(RuntimeError::RangeError(fired.message(self.timeout)));
        }
        self.steps += 1;
        if let Some(limit) = self.step_limit {
            if self.steps > limit {
                return Err(RuntimeError::RangeError(
                    "execution step limit exceeded (possible infinite loop)".to_string(),
                ));
            }
        }
        if self.steps >= self.next_guard_check {
            self.next_guard_check = self.steps + GUARD_CHECK_INTERVAL;
            // The host's allocator crosses its budget *during* a step, so the
            // check has to live in the interpreter loop: there is nowhere else
            // to notice it before the process is killed.
            if memory_pressure() {
                self.guard_fired = Some(GuardFired::Memory);
            } else if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                self.guard_fired = Some(GuardFired::Timeout);
            }
            if let Some(fired) = self.guard_fired {
                return Err(RuntimeError::RangeError(fired.message(self.timeout)));
            }
        }
        match *inst {
            Instr::Halt {} => {
                return Ok(false);
            }
            Instr::Ret {} => {
                // Check if we need to execute any finally blocks before returning.
                //
                // The *innermost* pending one goes first, which is both what the
                // language requires and what the epilogue assumes: `ResumeExc`
                // reads `seh_stack.last()`, so it can only finish the record that
                // is on top. Scanning from the front made a nested
                // `try { try { return x } finally {…} } finally {…}` run the
                // outer `finally` while the inner record was still on top — the
                // epilogue then did nothing, fell into the implicit `Mov rv,
                // undefined`, and the return value was lost.
                let mut finally_to_execute = None;
                for (idx, record) in self.state.seh_stack.iter().enumerate().rev() {
                    if record.finally_pc != 0 && !record.in_finally {
                        finally_to_execute = Some(idx);
                        break;
                    }
                }

                if let Some(idx) = finally_to_execute {
                    // Mark the record as in_finally and set delayed return flag
                    if let Some(record) = self.state.seh_stack.get_mut(idx) {
                        record.in_finally = true;
                        record.delayed_return = true;
                    }
                    let finally_pc = self.state.seh_stack[idx].finally_pc;
                    self.state.jump(finally_pc);
                } else {
                    // No pending finally blocks, proceed with normal return
                    if self.state.ctrl_stack_reached_bottom() {
                        // The return value is already in Rv; stop the loop so the
                        // caller can read it.
                        return Ok(false);
                    }
                    // A derived constructor that returns without `super()` —
                    // implicitly or via `return this` — has no `this` to return.
                    // Raised through the SEH machinery: `assert.throws(
                    // ReferenceError, () => new C())` has to see it.
                    if self.state.this_state.last() == Some(&THIS_DERIVED_UNBOUND) {
                        let err = self.this_not_initialized();
                        if let Some(exc) = self.as_js_exception(&err) {
                            self.handle_throw(exc)?;
                            return Ok(true);
                        }
                        return Err(err);
                    }
                    // The frame's own constructor state, captured before the
                    // pops below take it off the stack.
                    let frame_ctor_state = self.state.this_state.last().copied();
                    let return_pc = self.state.popc()?;
                    let saved_seh_depth = self.state.popc()?;
                    let saved_closure_depth = self.state.popc()?;
                    // The frame's own `this`, needed for [[Construct]] below.
                    // It has to be captured *before* the caller's binding is
                    // restored, otherwise a constructor would return the
                    // caller's `this` instead of the object it just built.
                    let frame_this = self.state.this_val.clone();
                    // Captured before the [[Construct]] coercion below, which
                    // would replace a primitive return value with `this`.
                    let returned = self.state.get_register(Register::Rv)?;
                    // Restore the caller's `this` binding and frame arity.
                    if let Some(saved_this) = self.state.this_stack.pop() {
                        self.state.this_val = saved_this;
                    }
                    self.state.this_state.pop();
                    if let Some(saved_function) = self.state.function_stack.pop() {
                        self.state.function_val = saved_function;
                    }
                    self.state.frame_argc.pop();
                    // If this frame was invoked via `new`, apply [[Construct]] return
                    // semantics: a returned object becomes the result, otherwise the
                    // newly created `this` object is used.
                    if let Some(true) = self.state.construct_stack.pop() {
                        let rv = self.state.get_register(Register::Rv)?;
                        if !rv.is_object() {
                            self.state.set_register(Register::Rv, frame_this)?;
                        }
                    }
                    self.state.new_target_stack.pop();
                    self.state.closure_var_stack.truncate(saved_closure_depth);
                    self.state.seh_stack.truncate(saved_seh_depth);
                    // ES 9.2.2 step 13c: a derived constructor that returns a
                    // primitive other than `undefined` raises a TypeError. The
                    // frame is fully unwound by now — its own SEH records in
                    // particular are gone — so a `try` inside the constructor
                    // cannot swallow it, while `assert.throws(TypeError, () =>
                    // new C())` in the caller still can. That asymmetry is what
                    // `derived-class-return-override-catch.js` checks.
                    if frame_ctor_state == Some(THIS_DERIVED_BOUND)
                        && !returned.is_undefined()
                        && !returned.is_object()
                    {
                        let err = RuntimeError::TypeError(
                            "derived constructor can only return an Object or undefined"
                                .to_string(),
                        );
                        if let Some(exc) = self.as_js_exception(&err) {
                            self.handle_throw(exc)?;
                            return Ok(true);
                        }
                        return Err(err);
                    }
                    self.state.jump(return_pc);
                }
            }
            _ => {
                if let Err(err) = self.run_instruction(&inst, module) {
                    // Spec-level errors (`TypeError`, `RangeError`, …) and
                    // uncaught JS throws are ordinary exceptions as far as user
                    // code is concerned: `try { … } catch (e)` and
                    // `assert.throws(TypeError, …)` must see them. Route them
                    // through the structured-exception machinery instead of
                    // aborting the whole program.
                    if let Some(exc) = self.as_js_exception(&err) {
                        self.handle_throw(exc)?;
                        return Ok(true);
                    }
                    return Err(err);
                }
            }
        }

        Ok(true)
    }

    /// Turn a VM error into the JS value user code would catch.
    ///
    /// Returns `None` for errors that are *not* part of the language (internal

    /// errors, unimplemented features) — those must abort execution.
    fn as_js_exception(&self, err: &RuntimeError) -> Option<Value> {
        match err {
            RuntimeError::TypeError(_)
            | RuntimeError::RangeError(_)
            | RuntimeError::ReferenceError(_)
            | RuntimeError::SyntaxError(_)
            | RuntimeError::Thrown(_) => {
                Some(crate::builtins::runtime_error_to_js_error(err, &self.builtins))
            }
            // Not part of the language: let it abort execution.
            _ => None,
        }
    }

    /// ES `ToPrimitive` (https://tc39.es/ecma262/#sec-toprimitive).
    ///
    /// Returns the primitive value of `val`. An object with
    /// `Symbol.toPrimitive` converts through it; otherwise `valueOf()` and
    /// `toString()` are tried in hint order and the first primitive wins.
    pub fn to_primitive(
        &mut self,
        val: &Value,
        hint: &str,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !val.is_object() {
            return Ok(val.clone());
        }

        // ES 7.1.1 ToPrimitive step 2: an object with `Symbol.toPrimitive`
        // converts through that method instead of OrdinaryToPrimitive, and a
        // non-primitive result is a TypeError (no fallback to valueOf/toString).
        let exotic = self.get_member(
            val,
            &crate::builtins::to_primitive_symbol_key(),
            module,
        )?;
        if !exotic.is_undefined() {
            if !exotic.is_callable() {
                return Err(RuntimeError::TypeError(
                    "Symbol.toPrimitive is not a function".to_string(),
                ));
            }
            let result = self.invoke(&exotic, val.clone(), &[Value::string(hint)], module)?;
            if result.is_object() {
                return Err(RuntimeError::TypeError(
                    "Cannot convert object to primitive value".to_string(),
                ));
            }
            return Ok(result);
        }

        // OrdinaryToPrimitive: call `valueOf()` then `toString()` (or the reverse
        // when the hint is "string"), returning the first primitive result.
        // NOTE: `Symbol.toPrimitive` is not yet honoured because property lookup
        // here is keyed by string; it would be a no-op either way.
        let value_of_first = hint != "string";
        let first = if value_of_first {
            "valueOf"
        } else {
            "toString"
        };
        let second = if value_of_first {
            "toString"
        } else {
            "valueOf"
        };

        for name in [first, second] {
            // `[[Get]]`, not a descriptor lookup: the method may be an accessor
            // whose getter has to run (and can throw).
            let method = self.get_member(val, &PropertyKey::from_str(name), module)?;
            // `OrdinaryToPrimitive` skips a method that is missing or not
            // callable and falls through to the next name — a `{ toString: null,
            // get valueOf() { … } }` object must reach `valueOf`.
            if !method.is_callable() {
                continue;
            }
            let result = if method.is_user_function() {
                // User-defined valueOf/toString: invoke it re-entrantly.
                self.call_reentrant(&method, val.clone(), module)?
            } else {
                // Native prototype method (Array/Number/Error/...). call_builtin_method
                // dispatches on the object type; for ordinary objects it returns the
                // default `[object Class]` string.
                self.call_builtin_method(val, name, &[])?
            };
            if result.is_primitive() && !result.is_object() {
                return Ok(result);
            }
        }

        Err(RuntimeError::TypeError(
            "Cannot convert object to primitive value".to_string(),
        ))
    }

    /// Create the wrapper returned by `Function.prototype.bind`.
    fn make_bound(&mut self, target: Value, this_arg: Value, bound_args: Vec<Value>) -> Value {
        let id = self.next_bound_id;
        self.next_bound_id += 1;
        self.bound_functions.insert(
            id,
            BoundFunction {
                target,
                this_arg,
                bound_args,
            },
        );
        Value::Object(Rc::new(RefCell::new(
            crate::vm::object::NativeFunctionObject::new(&format!("{BOUND_PREFIX}{id}")),
        )))
    }

    /// Argument list for `f.call(...)` / `f.apply(...)`, minus the `thisArg`.
    fn call_or_apply_args(
        &self,
        method: &str,
        args: &[Value],
    ) -> Result<Vec<Value>, RuntimeError> {
        if method == "call" {
            return Ok(args.iter().skip(1).cloned().collect());
        }
        match args.get(1) {
            None | Some(Value::Undefined) | Some(Value::Null) => Ok(Vec::new()),
            Some(list) => self.array_like_elements(list).ok_or_else(|| {
                RuntimeError::TypeError("CreateListFromArrayLike called on non-object".to_string())
            }),
        }
    }

    /// Reject a class constructor that is being called without `new`.
    fn check_class_ctor(&self, callee: &Value) -> Result<(), RuntimeError> {
        if let Value::Object(obj_ref) = callee {
            let borrowed = obj_ref.borrow();
            if borrowed
                .as_any()
                .downcast_ref::<crate::vm::object::FunctionObject>()
                .is_some()
                && borrowed
                    .property_get(&PropertyKey::from_str(crate::builtins::CLASS_CTOR_FLAG))
                    .map(|d| d.value.to_boolean())
                    .unwrap_or(false)
            {
                return Err(RuntimeError::TypeError(
                    "Class constructor cannot be invoked without 'new'".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Re-entrantly invoke a callable `callee` with `this` bound and no arguments,
    /// returning its result. This drives a nested execution loop so that user
    /// defined `valueOf` / `toString` methods can be honoured during coercion.
    fn call_reentrant(
        &mut self,
        callee: &Value,
        this: Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        self.invoke(callee, this, &[], module)
    }

    /// Re-entrant invocation of **any** callable with an explicit `this` and
    /// argument list.
    ///
    /// This is the single entry point built-ins use to call back into JavaScript
    /// (`Function.prototype.call`, `Array.prototype.map`, …). It:
    ///
    /// 1. runs native built-ins directly (no bytecode frame), and
    /// 2. for bytecode functions pushes a fresh frame, drives a nested execution
    ///    loop until the callee returns, then restores the caller's context.
    /// The bytecode function behind a callee, in either spelling
    /// (`Value::Function(id)` or the boxed `FunctionObject`).
    ///
    /// Call paths that build the callee frame themselves need this to ask
    /// "is this a generator?" the same way `invoke` does.
    fn callable_func_id(&self, callee: &Value) -> Option<u32> {
        match callee {
            Value::Function(id) => Some(*id),
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .as_any()
                .downcast_ref::<FunctionObject>()
                .map(|func_obj| func_obj.func_id),
            _ => None,
        }
    }

    /// 生成器函数的函数号（`None` = 不是生成器）。
    ///
    /// **判定的唯一来源**：`callable_func_id` + `module.generators`。代理、原生内建、
    /// 绑定函数都没有字节码函数号，所以这里不必再判它们是不是 —— 而以前五个调用点
    /// 各写一遍这套组合（写法还各不相同：`CallMethod` 先 `callable_func_id` 再查集合，
    /// `New` 反过来用"生成器不是构造器"，`Call` 只有裸函数号直接查集合）。
    fn generator_of(&self, callee: &Value, module: &Module) -> Option<u32> {
        self.callable_func_id(callee)
            .filter(|id| module.generators.contains(id))
    }

    /// `async` 函数的函数号（`None` = 不是 async）。判定同样只有这一处。
    fn async_of(&self, callee: &Value, module: &Module) -> Option<u32> {
        self.callable_func_id(callee)
            .filter(|id| module.asyncs.contains(id))
    }

    /// 一个可调用体的种类（[`CalleeKind`]）。给需要"一眼看全"的地方用
    /// （`invoke_with_new_target` 与 `New` 的"生成器不是构造器"）。
    fn callee_kind(&self, callee: &Value, module: &Module) -> CalleeKind {
        if Self::is_proxy(callee) {
            return CalleeKind::Proxy;
        }
        if crate::builtins::is_native_function(callee) {
            return CalleeKind::Native;
        }
        match self.callable_func_id(callee) {
            Some(id) if module.generators.contains(&id) => CalleeKind::Generator,
            Some(id) if module.asyncs.contains(&id) => CalleeKind::Async,
            Some(_) => CalleeKind::Bytecode,
            None => CalleeKind::NotCallable,
        }
    }

    pub fn invoke(
        &mut self,
        callee: &Value,
        this: Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        self.invoke_with_new_target(callee, this, args, None, module)
    }

    /// [`Self::invoke`], with an explicit `new.target` for the callee frame.
    ///
    /// `super(...)` is the only caller that needs this: ES `SuperCall` performs
    /// `Construct(func, args, newTarget)` with the *current* frame's
    /// `new.target`, so a grandchild class sees `new.target === Child` in every
    /// constructor up the chain.
    pub fn invoke_with_new_target(
        &mut self,
        callee: &Value,
        this: Value,
        args: &[Value],
        new_target_override: Option<Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // A proxy's `apply` trap decides the *whole* call, including whether it
        // is legal: the target's callability is the no-trap answer, not a
        // precondition (ES 10.5.12).
        if Self::is_proxy(callee) {
            return match self.proxy_apply(callee, &this, args, module)? {
                Some(result) => Ok(result),
                // No `apply` trap: `[[Call]]` on the target itself, with the
                // same `this` and the same arguments.
                None => {
                    let (target, _handler) = self.proxy_parts(callee, "apply")?;
                    self.invoke_with_new_target(&target, this, args, new_target_override, module)
                }
            };
        }
        // Native built-ins never need a bytecode frame — except when a
        // `super(...)` supplies a `new.target`: then the built-in has to build
        // the instance with `newTarget.prototype` (ES 9.1.14
        // GetPrototypeFromConstructor), which is what makes
        // `class C extends Error {}` produce a real error object.
        if let Some(name) = crate::builtins::native_function_name(callee) {
            if let Some(new_target) = &new_target_override {
                if new_target.is_object() {
                    return self.native_construct(
                        &name,
                        callee,
                        args,
                        Some(new_target),
                        module,
                    );
                }
            }
            return self.call_native_by_name(&name, this, args, module);
        }

        let (func_id, effective_this, captured_this_new_target, captured_vars) = match callee {
            Value::Function(id) => (*id, this.clone(), None, Vec::new()),
            Value::Object(obj_ref) => {
                let borrowed = obj_ref.borrow();
                match borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::FunctionObject>()
                {
                    Some(func_obj) => (
                        func_obj.func_id,
                        func_obj.captured_this.clone().unwrap_or(this.clone()),
                        func_obj.captured_new_target.clone(),
                        func_obj.captured_vars.clone(),
                    ),
                    None => {
                        return Err(RuntimeError::TypeError("not a function".to_string()));
                    }
                }
            }
            Value::Undefined => {
                return Err(RuntimeError::TypeError(
                    "undefined is not a function".to_string(),
                ));
            }
            _ => return Err(RuntimeError::TypeError("not a function".to_string())),
        };

        // A generator function reached through `invoke` — the iterator protocol,
        // `Array.prototype.map`, any builtin that calls back — must build a
        // generator object exactly like the `Call` opcode does. Running the body
        // eagerly would execute its *parameter binding* right there, and a
        // parameter with a destructuring pattern re-enters `make_iterator`,
        // which reaches this call again: unbounded recursion for a source as
        // ordinary as `Array.prototype[Symbol.iterator] = function* () {…}`.
        if self.generator_of(callee, module).is_some() {
            return self.create_generator(
                func_id,
                effective_this,
                args.to_vec(),
                captured_vars,
                module,
            );
        }

        // An `async` function answers a promise, so the frame is driven first
        // and its outcome wrapped: a return fulfils the promise, a throw rejects
        // it (ES 27.7.5.1 `AsyncFunctionStart`).
        if self.async_of(callee, module).is_some() {
            let promise = self.new_promise();
            let outcome = self.drive_bytecode_frame(
                func_id,
                callee,
                args,
                &captured_vars,
                FrameMode::Call {
                    this_val: effective_this.clone(),
                    captured_new_target: captured_this_new_target.clone(),
                    new_target_override: new_target_override.clone(),
                },
                module,
            );
            match outcome {
                Ok(value) => self.promise_resolve(&promise, value, module),
                Err(err) => {
                    let value = self.error_value(err);
                    self.promise_settle(&promise, PromiseState::Rejected, value);
                }
            }
            return Ok(Self::promise_value(&promise));
        }

        self.drive_bytecode_frame(
            func_id,
            callee,
            args,
            &captured_vars,
            FrameMode::Call {
                this_val: effective_this,
                captured_new_target: captured_this_new_target,
                new_target_override,
            },
            module,
        )
    }

    /// 开一帧：压好参数之后的全部准备工作。
    ///
    /// `enter_frame` → `this` / `function_val` → 闭包捕获变量 → 控制栈三项 →
    /// `construct_stack` / `new_target` → `jump` 到函数入口。**顺序有讲究**：
    /// `enter_frame` 要先把**调用方**的 `this` / `function_val` 快照进 `this_stack` /
    /// `function_stack`，所以它必须在覆盖这两个字段之前跑。
    ///
    /// 这段以前出现在 7 个地方（`drive_bytecode_frame` 与 `CallEx` / `CallMethod` /
    /// `New` 的内联开帧），差别只有 `return_pc`（opcode 用 `pc + 1`，被 `invoke`
    /// 调用时用哨兵 `instructions.len()`）、`this` / `function_val` 的来源、
    /// `construct` 标志，以及各自要不要先清 `Rv`（那是记账，留在调用点）。
    #[allow(clippy::too_many_arguments)]
    fn open_frame(
        &mut self,
        func_id: u32,
        argc: usize,
        this: Value,
        function_val: Value,
        captured_vars: &[(String, Value)],
        construct: bool,
        new_target: Value,
        return_pc: usize,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        self.state.enter_frame(argc)?;
        self.state.this_val = this;
        self.state.function_val = function_val;
        for (name, value) in captured_vars {
            let mut map = std::collections::HashMap::new();
            map.insert(name.clone(), value.clone());
            self.state.closure_var_stack.push(map);
        }
        let location = match module.symtab.get(&FunctionId::new(func_id)) {
            Some(location) => *location,
            None => {
                return Err(RuntimeError::ReferenceError(format!(
                    "undefined function: {func_id}"
                )));
            }
        };
        self.state.pushc(self.state.closure_var_stack.len())?;
        self.state.pushc(self.state.seh_stack.len())?;
        self.state.pushc(return_pc)?;
        self.state.construct_stack.push(construct);
        self.state.new_target_stack.push(new_target);
        self.state.jump(location);
        Ok(())
    }

    /// Push the callee's frame, run it to completion, then restore the caller's
    /// execution context and answer the frame's `Rv`.
    ///
    /// The frame is driven by a nested `step` loop that stops at the sentinel
    /// return address one past the last instruction.
    ///
    /// **两种模式共用这一份实现**（P1）：`[[Call]]` 与 `[[Construct]]` 的差别只有四处 ——
    /// `this`（传入的 vs 新建的实例）、`function_val`/`new.target`、`construct_stack`
    /// 的标志位、以及构造帧先把 `Rv` 清成 `undefined`。以前这四处差异是靠**两份 100 行
    /// 的拷贝**表达的（`drive_bytecode_frame` 与 `invoke_construct`），改一处忘一处就是
    /// B45/B46 那种"为了一个特性到处打补丁"。
    #[allow(clippy::too_many_arguments)]
    fn drive_bytecode_frame(
        &mut self,
        func_id: u32,
        callee: &Value,
        args: &[Value],
        captured_vars: &[(String, Value)],
        mode: FrameMode,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let saved_pc = self.state.pc;
        let saved_rsp = self.state.rsp;
        let saved_rbp = self.state.rbp;
        let saved_closure = self.state.closure_var_stack.len();
        let saved_seh = self.state.seh_stack.len();
        let saved_this = self.state.this_val.clone();
        let saved_this_depth = self.state.this_stack.len();
        let saved_construct = self.state.construct_stack.len();
        let saved_new_target = self.state.new_target_stack.len();
        let saved_function = self.state.function_val.clone();
        // Control-stack depth of the *caller*: SEH records at or below it belong
        // to an outer frame and must see the exception propagated, not handled
        // from inside this nested loop.
        let saved_ctrl = self.state.ctrl_stack.len();
        self.invoke_boundaries.push(saved_ctrl);

        // Push the arguments and open the callee's frame at the new stack top,
        // using the same convention as the `Call`/`CallEx` opcodes (arg0 ends up
        // at `[rbp-1]`).
        for arg in args.iter().rev() {
            self.state.push(arg.clone())?;
        }
        self.state.rbp = self.state.rsp;

        // Sentinel return address one past the last instruction: when the callee
        // returns, `Ret` lands here and the nested loop stops.
        let return_pc = module.instructions.len();

        // 模式差异在一处算清；开帧那 15 行在 `open_frame` 里，与各 opcode 内联开帧共用。
        let (this_val, function_val, new_target_val, is_construct) = match mode {
            FrameMode::Call {
                this_val,
                captured_new_target,
                new_target_override,
            } => (
                this_val,
                match callee {
                    Value::Function(id) => self.materialize_function(*id),
                    other => other.clone(),
                },
                new_target_override
                    .or(captured_new_target)
                    .unwrap_or(Value::Undefined),
                false,
            ),
            // 构造帧的 `this` 是刚建好的实例，`function_val` 与 `new.target` 都是
            // `new_target`（`Reflect.construct` 会把它换成第三个参数）。
            FrameMode::Construct {
                new_obj,
                new_target,
            } => (new_obj, new_target.clone(), new_target, true),
        };
        if is_construct {
            // 构造器不显式返回对象时，结果就是 `this` —— 先把 `Rv` 清成
            // `undefined`，让"看 `Rv` 是不是对象"这一步能成立。
            self.state.set_register(Register::Rv, Value::Undefined)?;
        }
        self.open_frame(
            func_id,
            args.len(),
            this_val,
            function_val,
            captured_vars,
            is_construct,
            new_target_val,
            return_pc,
            module,
        )?;

        // Drive the nested frame. On error the caller's context is restored
        // first, so an enclosing `try` still sees the exception: unwinding has
        // to happen before the error is propagated upwards.
        let outcome = (|| -> Result<(), RuntimeError> {
            while self.step(module)? {}
            Ok(())
        })();

        self.invoke_boundaries.pop();

        // Restore the caller's execution context *before* propagating: the
        // callee frame is gone either way, whether it returned or was unwound
        // by an exception escaping to an outer handler.
        //
        // A normal return already popped this frame's three control-stack
        // entries in `Ret`; an exception that escapes the frame did not, so
        // rewind them here. Without this a native that *catches* the error and
        // keeps going in the same frame (the `Promise` executor) would pop the
        // stale `return_pc` as its own saved `rbp`.
        self.state.ctrl_stack.truncate(saved_ctrl);
        self.state.pc = saved_pc;
        self.state.rsp = saved_rsp;
        self.state.rbp = saved_rbp;
        self.state.closure_var_stack.truncate(saved_closure);
        self.state.seh_stack.truncate(saved_seh);
        self.state.this_stack.truncate(saved_this_depth);
        self.state.this_state.truncate(saved_this_depth);
        self.state.frame_argc.truncate(saved_this_depth);
        self.state.this_val = saved_this;
        self.state.function_val = saved_function;
        self.state.construct_stack.truncate(saved_construct);
        self.state.new_target_stack.truncate(saved_new_target);

        outcome?;
        let rv = self.state.get_register(Register::Rv)?;
        Ok(rv)
    }

    /// ES `ToPropertyDescriptor` (7.3.3).
    ///
    /// The six descriptor fields are read through `[[Get]]`, so an accessor
    /// field runs with the descriptor object as `this`; the result is a plain
    /// object holding only the fields that were present. `get`/`set` must be
    /// callable or undefined.
    fn to_property_descriptor(
        &mut self,
        value: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !matches!(value, Value::Object(_) | Value::Function(_)) {
            return Err(RuntimeError::TypeError(
                "Property description must be an object".to_string(),
            ));
        }
        let desc = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::OrdinaryObject::new(),
        )));
        // ES 6.2.4.6 step 10: a descriptor that carries both accessor fields
        // (`get`/`set`) and data fields (`value`/`writable`) is invalid — the
        // mixed objects caught here are exactly the ones test262 expects to
        // raise `TypeError` (`defineProperty/15.2.3.6-3-*`, `defineProperties/
        // 15.2.3.7-5-b-*`).
        let mut has_accessor_field = false;
        let mut has_data_field = false;
        for present_name in ["value", "writable", "get", "set"] {
            let key = PropertyKey::from_str(present_name);
            let present = match value {
                Value::Object(obj) => {
                    crate::vm::prototype::internal_has_property(Rc::clone(obj), &key)
                        .unwrap_or(false)
                }
                _ => false,
            };
            if !present {
                continue;
            }
            if present_name == "get" || present_name == "set" {
                has_accessor_field = true;
            } else {
                has_data_field = true;
            }
        }
        if has_accessor_field && has_data_field {
            return Err(RuntimeError::TypeError(
                "Invalid property descriptor: cannot both specify accessors and a value or writable attribute"
                    .to_string(),
            ));
        }
        for name in ["enumerable", "configurable", "value", "writable", "get", "set"] {
            let key = PropertyKey::from_str(name);
            let present = match value {
                Value::Object(obj) => {
                    crate::vm::prototype::internal_has_property(Rc::clone(obj), &key)
                        .unwrap_or(false)
                }
                _ => false,
            };
            if !present {
                continue;
            }
            let raw = self.get_member(value, &key, module)?;
            let converted = match name {
                "get" | "set" => {
                    if !raw.is_undefined() && !raw.is_callable() {
                        return Err(RuntimeError::TypeError(
                            "Getter or setter is not callable".to_string(),
                        ));
                    }
                    raw
                }
                "value" => raw,
                _ => Value::Bool(raw.to_boolean()),
            };
            if let Value::Object(desc_ref) = &desc {
                let _ = desc_ref.borrow_mut().property_set(key, converted);
            }
        }
        Ok(desc)
    }

    /// `ToPropertyDescriptor` for every own enumerable key of `props`
    /// (`Object.defineProperties` / the `Object.create` properties argument).
    fn to_property_descriptors(
        &mut self,
        props: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let Value::Object(props_ref) = props else {
            return Err(RuntimeError::TypeError(
                "Properties must be an object".to_string(),
            ));
        };
        // Own enumerable keys first, then a real [[Get]] for each value: the
        // properties object may itself hold accessors (ES 19.1.2.3.1 step 3).
        let keys: Vec<PropertyKey> = {
            let borrowed = props_ref.borrow();
            borrowed
                .own_keys()
                .into_iter()
                .filter(|k| {
                    borrowed
                        .property_get(k)
                        .is_some_and(|d| d.enumerable)
                })
                .collect()
        };
        let out = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::OrdinaryObject::new(),
        )));
        for key in keys {
            let value = self.get_member(props, &key, module)?;
            let converted = self.to_property_descriptor(&value, module)?;
            if let Value::Object(out_ref) = &out {
                let _ = out_ref.borrow_mut().property_set(key, converted);
            }
        }
        Ok(out)
    }

    /// ES 19.1.2.1 `Object.assign(target, ...sources)`.
    ///
    /// Values are read through real `[[Get]]` and written through real
    /// `[[Set]]`, so own or prototype accessors on either side run; targets
    /// and sources are boxed with ToObject.
    fn object_assign(&mut self, args: &[Value], module: &Module) -> Result<Value, RuntimeError> {
        let Some(target) = args.first() else {
            return Err(RuntimeError::TypeError(
                "Object.assign requires at least 1 argument".to_string(),
            ));
        };
        let target = crate::builtins::to_object(target)?;
        for source in args.iter().skip(1) {
            if matches!(source, Value::Undefined | Value::Null) {
                continue;
            }
            let source = crate::builtins::to_object(source)?;
            let keys: Vec<PropertyKey> = match &source {
                Value::Object(src_ref) => {
                    let borrowed = src_ref.borrow();
                    borrowed
                        .own_keys()
                        .into_iter()
                        .filter(|k| {
                            borrowed
                                .property_get(k)
                                .is_some_and(|d| d.enumerable)
                        })
                        .collect()
                }
                _ => continue,
            };
            for key in keys {
                let value = self.get_member(&source, &key, module)?;
                self.set_member(&target, key.clone(), value, module)?;
            }
        }
        Ok(target)
    }

    /// ES 25.5.2 `JSON.stringify(value[, replacer[, space]])`.
    ///
    /// The traversal has to run in the VM: `toJSON` and `replacer` are user
    /// functions, and every property read goes through `[[Get]]` (so accessors
    /// fire). The builtin layer only has a data-only fallback
    /// (`builtins::json_stringify`).
    fn json_stringify_full(
        &mut self,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let value = args.first().cloned().unwrap_or(Value::Undefined);
        let replacer = args.get(1).cloned().unwrap_or(Value::Undefined);

        // ── replacer: a function, or an array of property names ──
        let mut replacer_fn: Option<Value> = None;
        let mut property_list: Option<Vec<String>> = None;
        if replacer.is_callable() {
            replacer_fn = Some(replacer.clone());
        } else if matches!(&replacer, Value::Object(o) if o.borrow().kind() == ObjectKind::Array) {
            let mut list: Vec<String> = Vec::new();
            for item in self.array_like_elements(&replacer).unwrap_or_default() {
                let name = match &item {
                    Value::String(s) => Some(s.to_string()),
                    Value::Number(n) => Some(crate::builtins::number_to_string(*n)),
                    Value::Object(o) => match o.borrow().kind() {
                        // A boxed String/Number counts, converted with `ToString`
                        // (so a user `toString` wins over the internal slot);
                        // any other object does not.
                        ObjectKind::String | ObjectKind::Number => {
                            let primitive = self.to_primitive(&item, "string", module)?;
                            Some(primitive.to_js_string())
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(name) = name {
                    if !list.contains(&name) {
                        list.push(name);
                    }
                }
            }
            property_list = Some(list);
        }

        // ── space → gap (ES 25.5.2.1 steps 5-8) ──
        let space = args.get(2).cloned().unwrap_or(Value::Undefined);
        let space = match &space {
            Value::Object(o) => match o.borrow().kind() {
                ObjectKind::Number => {
                    let primitive = self.to_primitive(&space, "number", module)?;
                    Value::Number(primitive.to_number())
                }
                ObjectKind::String => {
                    let primitive = self.to_primitive(&space, "string", module)?;
                    Value::string(&primitive.to_js_string())
                }
                _ => space.clone(),
            },
            other => other.clone(),
        };
        let gap = match &space {
            Value::Number(_) => " "
                .repeat(crate::builtins::to_integer_or_infinity(Some(&space)).clamp(0, 10) as usize),
            Value::String(s) => s.chars().take(10).collect(),
            _ => String::new(),
        };

        // The value is reached through a synthetic wrapper object holding it
        // under the empty key, exactly as the spec's algorithm starts.
        let mut wrapper = crate::vm::object::OrdinaryObject::new();
        if let Some(proto) = crate::builtins::wrapper_prototype("Object") {
            wrapper.set_prototype(Some(proto));
        }
        let _ = wrapper.define_property(
            PropertyKey::from_str(""),
            crate::vm::property::PropertyDescriptor::data_descriptor(value),
        );
        let wrapper = Value::Object(Rc::new(RefCell::new(wrapper)));

        let mut indent = String::new();
        let mut stack: Vec<Value> = Vec::new();
        let serialized = self.json_serialize_property(
            "",
            &wrapper,
            &gap,
            &replacer_fn,
            &property_list,
            &mut indent,
            &mut stack,
            module,
        )?;
        Ok(match serialized {
            Some(text) => Value::string(&text),
            None => Value::Undefined,
        })
    }

    /// `SerializeJSONProperty(state, key, holder)` (ES 25.5.2.2).
    ///
    /// `None` means the value must be omitted (object member) or replaced by
    /// `null` (array element).
    #[allow(clippy::too_many_arguments)]
    fn json_serialize_property(
        &mut self,
        key: &str,
        holder: &Value,
        gap: &str,
        replacer_fn: &Option<Value>,
        property_list: &Option<Vec<String>>,
        indent: &mut String,
        stack: &mut Vec<Value>,
        module: &Module,
    ) -> Result<Option<String>, RuntimeError> {
        let mut value = self.get_member(holder, &PropertyKey::from_str(key), module)?;

        // `toJSON` first (a Date or a user object can define one).
        if value.as_object().is_some() || matches!(value, Value::Function(_)) {
            let to_json = self.get_member(&value, &PropertyKey::from_str("toJSON"), module)?;
            if to_json.is_callable() {
                value = self.invoke(&to_json, value.clone(), &[Value::string(key)], module)?;
            }
        }
        if let Some(replacer) = replacer_fn {
            value = self.invoke(
                replacer,
                holder.clone(),
                &[Value::string(key), value.clone()],
                module,
            )?;
        }
        // Boxed primitives serialize as the primitive they wrap.
        let wrapper_kind = match &value {
            Value::Object(obj_ref) => Some(obj_ref.borrow().kind()),
            _ => None,
        };
        // Boxed primitives: the spec asks for `ToNumber` / `ToString` (which run
        // user `valueOf` / `toString`), except for `[[BooleanData]]` where the
        // internal slot is read directly.
        match wrapper_kind {
            Some(ObjectKind::Number) => {
                let primitive = self.to_primitive(&value, "number", module)?;
                value = Value::Number(primitive.to_number());
            }
            Some(ObjectKind::String) => {
                let primitive = self.to_primitive(&value, "string", module)?;
                value = Value::string(&primitive.to_js_string());
            }
            Some(ObjectKind::Boolean) => value = Value::Bool(value.to_number() != 0.0),
            _ => {}
        }

        match &value {
            Value::Null => Ok(Some("null".to_string())),
            Value::Bool(b) => Ok(Some(if *b { "true" } else { "false" }.to_string())),
            Value::String(s) => {
                let mut out = String::new();
                crate::builtins::quote_json_string(s, &mut out);
                Ok(Some(out))
            }
            Value::Number(n) => Ok(Some(crate::builtins::json_number(*n))),
            // A function is not serializable, even when boxed as an object.
            Value::Object(obj_ref) if obj_ref.borrow().kind() == ObjectKind::Function => Ok(None),
            Value::Object(obj_ref) => {
                if obj_ref.borrow().kind() == ObjectKind::Array {
                    self.json_serialize_array(
                        &value,
                        gap,
                        replacer_fn,
                        property_list,
                        indent,
                        stack,
                        module,
                    )
                } else {
                    self.json_serialize_object(
                        &value,
                        gap,
                        replacer_fn,
                        property_list,
                        indent,
                        stack,
                        module,
                    )
                }
            }
            // `undefined`, functions and symbols.
            _ => Ok(None),
        }
    }

    /// `SerializeJSONObject` (ES 25.5.2.4).
    #[allow(clippy::too_many_arguments)]
    fn json_serialize_object(
        &mut self,
        value: &Value,
        gap: &str,
        replacer_fn: &Option<Value>,
        property_list: &Option<Vec<String>>,
        indent: &mut String,
        stack: &mut Vec<Value>,
        module: &Module,
    ) -> Result<Option<String>, RuntimeError> {
        if stack.iter().any(|v| v.strict_eq(value)) {
            return Err(RuntimeError::TypeError(
                "Converting circular structure to JSON".to_string(),
            ));
        }
        stack.push(value.clone());
        let stepback = indent.clone();
        indent.push_str(gap);
        let inner = indent.clone();

        let keys: Vec<String> = match property_list {
            Some(list) => list.clone(),
            None => crate::builtins::own_enumerable_string_keys(value),
        };
        let mut partial = Vec::new();
        for key in keys {
            if let Some(serialized) = self.json_serialize_property(
                &key,
                value,
                gap,
                replacer_fn,
                property_list,
                indent,
                stack,
                module,
            )? {
                let mut quoted = String::new();
                crate::builtins::quote_json_string(&key, &mut quoted);
                let colon = if gap.is_empty() { ":" } else { ": " };
                partial.push(format!("{quoted}{colon}{serialized}"));
            }
        }
        stack.pop();
        *indent = stepback.clone();
        Ok(Some(finalize_json(partial, gap, &inner, &stepback, '{', '}')))
    }

    /// `SerializeJSONArray` (ES 25.5.2.5).
    #[allow(clippy::too_many_arguments)]
    fn json_serialize_array(
        &mut self,
        value: &Value,
        gap: &str,
        replacer_fn: &Option<Value>,
        property_list: &Option<Vec<String>>,
        indent: &mut String,
        stack: &mut Vec<Value>,
        module: &Module,
    ) -> Result<Option<String>, RuntimeError> {
        if stack.iter().any(|v| v.strict_eq(value)) {
            return Err(RuntimeError::TypeError(
                "Converting circular structure to JSON".to_string(),
            ));
        }
        stack.push(value.clone());
        let stepback = indent.clone();
        indent.push_str(gap);
        let inner = indent.clone();

        let len = self.array_like_elements(value).map(|v| v.len()).unwrap_or(0);
        let mut partial = Vec::new();
        for index in 0..len {
            let serialized = self.json_serialize_property(
                &index.to_string(),
                value,
                gap,
                replacer_fn,
                property_list,
                indent,
                stack,
                module,
            )?;
            partial.push(serialized.unwrap_or_else(|| "null".to_string()));
        }
        stack.pop();
        *indent = stepback.clone();
        Ok(Some(finalize_json(partial, gap, &inner, &stepback, '[', ']')))
    }

    /// ES 25.5.1 `JSON.parse(text[, reviver])`.
    fn json_parse_full(
        &mut self,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let text_value = args.first().cloned().unwrap_or(Value::Undefined);
        // `ToString(text)`: a symbol has no string form (7.1.17 step 2), and
        // objects convert through `ToPrimitive("string")`, so a user `toString`
        // runs (and can throw).
        if matches!(text_value, Value::Symbol(_)) {
            return Err(RuntimeError::TypeError(
                "Cannot convert a Symbol value to a string".to_string(),
            ));
        }
        let primitive = self.to_primitive(&text_value, "string", module)?;
        let text = primitive.to_js_string();
        let parsed = crate::builtins::json_parse(&text)?;

        let reviver = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !reviver.is_callable() {
            return Ok(parsed);
        }

        let mut holder = crate::vm::object::OrdinaryObject::new();
        if let Some(proto) = crate::builtins::wrapper_prototype("Object") {
            holder.set_prototype(Some(proto));
        }
        let _ = holder.define_property(
            PropertyKey::from_str(""),
            crate::vm::property::PropertyDescriptor::data_descriptor(parsed),
        );
        let holder = Value::Object(Rc::new(RefCell::new(holder)));
        self.json_internalize(&holder, "", &reviver, module)
    }

    /// `InternalizeJSONProperty` (ES 25.5.1.1): walk bottom-up, letting the
    /// reviver replace or delete each property.
    fn json_internalize(
        &mut self,
        holder: &Value,
        name: &str,
        reviver: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let value = self.get_member(holder, &PropertyKey::from_str(name), module)?;
        if matches!(&value, Value::Object(_)) {
            let is_array = matches!(&value, Value::Object(o) if o.borrow().kind() == ObjectKind::Array);
            let keys: Vec<String> = if is_array {
                (0..self.array_like_elements(&value).map(|v| v.len()).unwrap_or(0))
                    .map(|i| i.to_string())
                    .collect()
            } else {
                crate::builtins::own_enumerable_string_keys(&value)
            };
            for key in keys {
                let new_element = self.json_internalize(&value, &key, reviver, module)?;
                let key = PropertyKey::from_str(&key);
                if matches!(new_element, Value::Undefined) {
                    self.delete_member(&value, &key, module)?;
                } else if let Value::Object(obj_ref) = &value {
                    // `CreateDataProperty` (not `[[Set]]`): a failure — e.g. a
                    // non-configurable property the reviver created — is
                    // ignored, and the existing value simply stays.
                    let _ = obj_ref.borrow_mut().define_property(
                        key,
                        crate::vm::property::PropertyDescriptor::data_descriptor(new_element),
                    );
                }
            }
        }
        self.invoke(
            reviver,
            holder.clone(),
            &[Value::string(name), value],
            module,
        )
    }

    /// ES 19.1.3.6 `Object.prototype.toString`.
    ///
    /// A string-valued `Symbol.toStringTag` takes precedence over the built-in
    /// class tag (`[object Array]`, `[object Function]`, …). Non-string tags are
    /// ignored, as the spec requires.
    fn object_prototype_to_string(
        &mut self,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !matches!(this, Value::Undefined | Value::Null) {
            let tag = self.get_member(
                this,
                &crate::builtins::to_string_tag_symbol_key(),
                module,
            )?;
            if let Value::String(tag) = tag {
                return Ok(Value::string(&format!("[object {tag}]")));
            }
        }
        crate::builtins::object_prototype_to_string(this, args)
    }

    /// Dispatch a built-in by name, honouring prototype methods.
    ///
    /// `set_prototype_method` registers prototype methods under the synthetic
    /// name `__proto_method__<name>`; those need the receiver as their first
    /// argument, whereas real natives (`Object.keys`, `isNaN`, …) do not.
    fn call_native_by_name(
        &mut self,
        name: &str,
        this: Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // Box bare `Value::Function(id)` arguments (and the receiver) into their
        // memoized function objects before handing them to the builtin layer.
        // The builtin layer matches on `Value::Object` — `Object.keys(fn)`,
        // `Object.getOwnPropertyDescriptor(fn, "length")` and friends all used
        // to report "first argument must be an object" for a plain function
        // reference, and `ToObject` passes the bare form through unchanged.
        let boxed_args: Vec<Value> = args.iter().map(|a| self.as_object_value(a)).collect();
        let args: &[Value] = &boxed_args;
        let this: Value = self.as_object_value(&this);

        // The `resolve` / `reject` handed to a `Promise` executor. They carry
        // their target's id in the name (see `promise_construct`), because a
        // native function here is identified by name alone.
        if let Some(id) = name.strip_prefix("__promise_resolve__") {
            if let Ok(id) = id.parse::<u64>() {
                if let Some(promise) = self.state.promises.get(&id).cloned() {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    // A resolved promise fulfils with the value (thenable
                    // adoption is not modelled: there is no way to run a
                    // foreign `then` and observe it synchronously).
                    self.promise_settle(&promise, PromiseState::Fulfilled, value);
                }
            }
            return Ok(Value::Undefined);
        }
        if let Some(id) = name.strip_prefix("__promise_reject__") {
            if let Ok(id) = id.parse::<u64>() {
                if let Some(promise) = self.state.promises.get(&id).cloned() {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    self.promise_settle(&promise, PromiseState::Rejected, value);
                }
            }
            return Ok(Value::Undefined);
        }
        // Per-element handlers of `Promise.all` / `race` / `allSettled` / `any`.
        if name.starts_with("__promise_agg_ok__") || name.starts_with("__promise_agg_err__") {
            if let Some(result) = self.dispatch_aggregation(name, args) {
                return result;
            }
        }
        // `Promise.prototype.finally` callbacks (name carries a registry id).
        if let Some(rest) = name.strip_prefix("__promise_finally__") {
            // `"<id>_f"` / `"<id>_r"`: which reaction (fulfilled or rejected)
            // decides what "pass through unchanged" means.
            if let Some((id, tag)) = rest.split_once('_') {
                if let Ok(id) = id.parse::<u64>() {
                    let callback = self.state.finally_handlers.get(&id).cloned();
                    let argument = args.first().cloned().unwrap_or(Value::Undefined);
                    if let Some(cb) = callback {
                        // A throwing callback rejects the derived promise.
                        self.invoke(&cb, Value::Undefined, &[], module)?;
                        if tag == "r" {
                            return Err(RuntimeError::Thrown(argument));
                        }
                        return Ok(argument);
                    }
                }
            }
            return Ok(Value::Undefined);
        }
        if name == crate::builtins::OBJECT_TO_STRING_NATIVE {
            return self.object_prototype_to_string(&this, args, module);
        }
        // `Symbol.prototype.description` getter: unwrap the receiver.
        if name == crate::builtins::SYMBOL_DESCRIPTION_NATIVE {
            return crate::builtins::symbol_description(&this);
        }
        // `Map.prototype.size` getter (ES 23.1.3.9).
        if name == crate::builtins::MAP_SIZE_NATIVE {
            return crate::builtins::map_size(&this);
        }
        // `Set.prototype.size` getter (ES 23.2.3.9).
        if name == crate::builtins::SET_SIZE_NATIVE {
            return crate::builtins::set_size(&this);
        }
        // TypedArray / ArrayBuffer getters (ES 23.2.3.1-4, 25.1.5.1). They are
        // accessors on the prototype, so the receiver arrives as `this`.
        if name == crate::builtins::typedarray::TA_LENGTH_NATIVE {
            return crate::builtins::typedarray::ta_length(&this);
        }
        if name == crate::builtins::typedarray::TA_BYTE_LENGTH_NATIVE {
            return crate::builtins::typedarray::ta_byte_length(&this);
        }
        if name == crate::builtins::typedarray::TA_BYTE_OFFSET_NATIVE {
            return crate::builtins::typedarray::ta_byte_offset(&this);
        }
        if name == crate::builtins::typedarray::TA_BUFFER_NATIVE {
            return crate::builtins::typedarray::ta_buffer(&this);
        }
        if name == crate::builtins::typedarray::AB_BYTE_LENGTH_NATIVE {
            return crate::builtins::typedarray::ab_byte_length(&this);
        }
        // `get Map[Symbol.species]` / `get Set[Symbol.species]` (23.1.2.2,
        // 23.2.2.2) and `get Promise[Symbol.species]` (27.2.2.3) all answer
        // their receiver.
        if name == crate::builtins::MAP_SPECIES_NATIVE
            || name == crate::builtins::SET_SPECIES_NATIVE
            || name == crate::builtins::PROMISE_SPECIES_NATIVE
        {
            return Ok(this);
        }
        // `Map`/`Set` prototype methods are dispatched here, not in the builtin
        // layer: they either call a user callback or mint an iterator object,
        // and their dispatch name carries which prototype they came from.
        if let Some(method) = name.strip_prefix(crate::builtins::MAP_METHOD_PREFIX) {
            return self.map_method(&this, method, args, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::SET_METHOD_PREFIX) {
            return self.set_method(&this, method, args, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::WEAKMAP_METHOD_PREFIX) {
            return self.weakmap_method(&this, method, args, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::WEAKSET_METHOD_PREFIX) {
            return self.weakset_method(&this, method, args);
        }
        // Generator methods: `gen.next(v)` resumes the body, `gen.return(v)` /
        // `gen.throw(e)` complete it, and `gen[Symbol.iterator]()` hands the
        // generator itself back.
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_NEXT_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let send = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_next(id, send, module);
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_RETURN_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_abrupt(id, Ok(value), module);
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_THROW_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let reason = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_abrupt(id, Err(RuntimeError::Thrown(reason)), module);
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_ITERATOR_PREFIX) {
            let _id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            return Ok(this);
        }
        // `get Array[Symbol.species]` returns its receiver (ES 22.1.2.5).
        if name == crate::builtins::ARRAY_SPECIES_NATIVE {
            return Ok(this);
        }
        // `Symbol.iterator` factory: returns an internal iterator over `this`.
        if name == crate::vm::iterator::ITERATOR_NATIVE_NAME {
            return self.make_iterator(this, module);
        }
        // `String(value)` is ToString(value): an object converts through
        // ToPrimitive (hint "string"), which can run user code, so it has to
        // happen here rather than in the builtin layer.
        if name == "String" && args.len() == 1 {
            let primitive = self.to_primitive(&args[0], "string", module)?;
            return Ok(Value::string(&primitive.to_js_string()));
        }
        // Per-iterator `next()` / `return()` methods.
        if let Some(id) = name.strip_prefix(crate::vm::iterator::ITERATOR_NEXT_PREFIX) {
            let id: u64 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed iterator id".to_string()))?;
            let iter_val = self
                .iterator_registry
                .get(&id)
                .cloned()
                .ok_or_else(|| RuntimeError::TypeError("iterator is no longer alive".to_string()))?;
            // `next(v)` forwards `v` to a JS iterator: that is how `yield*` gets
            // the value the outer `next(v)` supplied into the delegate.
            let send = args.first().cloned();
            let (item, done) = self.iterator_next(iter_val, send, module)?;
            return Ok(Self::iterator_result(item, done));
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::ITERATOR_RETURN_PREFIX) {
            let id: u64 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed iterator id".to_string()))?;
            if let Some(iter_val) = self.iterator_registry.get(&id).cloned() {
                self.iterator_close(iter_val, args.first(), false, module)?;
            }
            // `return()` is an iterator method, so it answers with an iterator
            // result — `yield*` checks that the value is an object
            // (ES 14.4.14 step 5.c.vi).
            return Ok(Self::iterator_result(Value::Undefined, true));
        }
        // `Function.prototype` registers `call` / `apply` / `bind` under their
        // plain names, so handle them before the static-method lookup.
        if matches!(name, "call" | "apply") && this.is_callable() {
            self.check_class_ctor(&this)?;
            let call_args = self.call_or_apply_args(name, args)?;
            let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
            return self.invoke(&this, this_arg, &call_args, module);
        }
        if name == "bind" && this.is_callable() {
            let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
            let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
            return Ok(self.make_bound(this, this_arg, bound_args));
        }
        if let Some(id) = name.strip_prefix(BOUND_PREFIX) {
            let id: u32 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed bound function".to_string()))?;
            let Some((target, this_arg, mut all)) = self
                .bound_functions
                .get(&id)
                .map(|b| (b.target.clone(), b.this_arg.clone(), b.bound_args.clone()))
            else {
                return Err(RuntimeError::InternalError(
                    "bound function is no longer available".to_string(),
                ));
            };
            all.extend(args.iter().cloned());
            return self.invoke(&target, this_arg, &all, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::PROTO_METHOD_PREFIX) {
            // `f.call` / `f.apply` / `f.bind` are the same operations as the
            // `CallMethod` opcode path handles; they must work identically when
            // the method value is fetched first (`Function.prototype.call.bind(…)`).
            if method == "bind" && this.is_callable() {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
                return Ok(self.make_bound(this, this_arg, bound_args));
            }
            if (method == "call" || method == "apply") && this.is_callable() {
                // `C.call(...)` must fail for a class constructor, same as `C()`.
                self.check_class_ctor(&this)?;
                let call_args = self.call_or_apply_args(method, args)?;
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                return self.invoke(&this, this_arg, &call_args, module);
            }
            if let Some(result) = self.try_array_callback_method(&this, method, args, module)? {
                return Ok(result);
            }
            // `Promise.prototype.then` has to *call* the handlers, so it is
            // dispatched here rather than in the builtin table.
            if let Some(result) = self.try_promise_method(&this, method, args, module)? {
                return Ok(result);
            }
            // `String.prototype.replace(re, fn)` — the replacer is user code.
            if let Some(result) = self.try_string_callback_method(&this, method, args, module)? {
                return Ok(result);
            }
            if let Some(result) = self.try_match_all_method(&this, method, args, module)? {
                return Ok(result);
            }
            // `entries` / `keys` / `values` return a *fresh* iterator over a
            // snapshot; only the VM can mint iterator objects (they live in the
            // iterator registry so `next()` can find its state).
            if matches!(method, "entries" | "keys" | "values") {
                // ES 23.1.3.28-30 start with `Let O be ? ToObject(this value)`:
                // `values.call(null)` is a TypeError, not an empty iterator.
                crate::builtins::require_object_coercible(&this)?;
                // *Then* `ToLength(Get(O, "length"))`. A receiver without a
                // usable `length` iterates as empty (`values.call({})`).
                {
                    let items = self.array_like_elements(&this).unwrap_or_default();
                    let snapshot: Vec<Value> = match method {
                        "entries" => items
                            .iter()
                            .enumerate()
                            .map(|(i, value)| {
                                Value::Object(Rc::new(RefCell::new(
                                    crate::vm::object::ArrayObject::from_vec(vec![
                                        Value::Number(i as f64),
                                        value.clone(),
                                    ]),
                                )))
                            })
                            .collect(),
                        "keys" => (0..items.len())
                            .map(|i| Value::Number(i as f64))
                            .collect(),
                        _ => items,
                    };
                    let snapshot_val = Value::Object(Rc::new(RefCell::new(
                        crate::vm::object::ArrayObject::from_vec(snapshot),
                    )));
                    return self.make_iterator(snapshot_val, module);
                }
            }
            return crate::builtins::call_prototype_method(&this, method, args);
        }
        // Static methods are stored under their qualified name ("Array.from").
        if name.contains('.') {
            // `Reflect.*` reads its key argument through `ToPropertyKey`, which
            // runs user code (`{ toString() { throw … } }`) — the builtin layer
            // cannot do that, so the key is converted here and handed on.
            let converted_args;
            let args: &[Value] = if matches!(
                name,
                "Reflect.get"
                    | "Reflect.set"
                    | "Reflect.has"
                    | "Reflect.deleteProperty"
                    | "Reflect.defineProperty"
                    | "Reflect.getOwnPropertyDescriptor"
            ) {
                converted_args = self.reflect_key_args(args, module)?;
                &converted_args
            } else {
                args
            };
            // A proxy receiver turns these `Object.*` / `Reflect.*` operations
            // into trap calls (ES 10.5.5 – 10.5.19); anything else falls
            // through to the ordinary implementation.
            if let Some(result) = self.proxy_static_trap(name, args, module)? {
                return Ok(result);
            }
            // `Object.assign` reads through real `[[Get]]` and writes through
            // real `[[Set]]` (own or prototype accessors may run), which only
            // the VM can do.
            if name == "Object.assign" {
                return self.object_assign(args, module);
            }
            // `Map.groupBy(items, callbackfn)`: iterates and calls back.
            if name == "Map.groupBy" {
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                return self.map_group_by(&items, &callback, module);
            }
            // The descriptor arguments are read through [[Get]] first, which
            // `Promise.resolve` / `Promise.reject` build promises, which only the
            // VM can register.
            if let Some(result) = self.promise_static(name, args, module) {
                return Ok(result);
            }
            // only the VM can do (their fields may be accessors).
            if name == "Object.defineProperty" && args.len() >= 3 {
                let descriptor = self.to_property_descriptor(&args[2], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), args[1].clone(), descriptor],
                );
            }
            if name == "Object.defineProperties" && args.len() >= 2 {
                let props = self.to_property_descriptors(&args[1], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), props],
                );
            }
            // `JSON.stringify` runs `toJSON`/`replacer` and reads through
            // `[[Get]]`; `JSON.parse`'s reviver is user code.
            if name == "JSON.stringify" {
                return self.json_stringify_full(args, module);
            }
            if name == "JSON.parse" {
                return self.json_parse_full(args, module);
            }
            if name == "Object.create" && args.len() >= 2 && !args[1].is_undefined() {
                let props = self.to_property_descriptors(&args[1], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), props],
                );
            }
            // `Reflect.get` / `set` / `apply` / `construct` / `defineProperty`:
            // the internal methods that can run user code.
            if let Some(result) = self.reflect_static(name, args, module)? {
                return Ok(result);
            }
            // `Proxy.revocable(t, h)` answers `{ proxy, revoke }`, and `revoke`
            // has to remember *which* proxy it revokes — a native's only state
            // is its name, so the VM mints it (`make_revoke`).
            if name == "Proxy.revocable" {
                // Same boxing as `new Proxy` above: `f` is an object.
                let target =
                    self.as_object_value(&args.first().cloned().unwrap_or(Value::Undefined));
                let handler =
                    self.as_object_value(&args.get(1).cloned().unwrap_or(Value::Undefined));
                let proxy = crate::builtins::proxy::proxy_construct(target, handler)?;
                let revoke = self.make_revoke(proxy.clone());
                let mut result = crate::vm::object::OrdinaryObject::new();
                result.set_prototype(Some(Rc::clone(&self.builtins.object_prototype)));
                for (field, value) in [("proxy", proxy), ("revoke", revoke)] {
                    let _ = result.define_property(
                        PropertyKey::from_str(field),
                        PropertyDescriptor::data_descriptor(value),
                    );
                }
                return Ok(Value::Object(Rc::new(RefCell::new(result))));
            }
            return crate::builtins::call_static_method(name, args);
        }
        // The `revoke` function of `Proxy.revocable`: its work is one flag on
        // the proxy it names. Everything after revocation throws.
        if let Some(id) = name.strip_prefix(REVOKE_PREFIX) {
            let id: u32 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed revoke function".to_string())
            })?;
            if let Some(Value::Object(obj_ref)) = self.revocable_proxies.get(&id) {
                if let Some(proxy) = obj_ref
                    .borrow_mut()
                    .as_any_mut()
                    .downcast_mut::<crate::vm::object::ProxyObject>()
                {
                    proxy.revoked = true;
                }
            }
            return Ok(Value::Undefined);
        }
        // `Proxy` has no `[[Call]]` at all (ES 28.2.2): it can only be
        // constructed.
        if name == "Proxy" {
            return Err(RuntimeError::TypeError(
                "Constructor Proxy requires 'new'".to_string(),
            ));
        }
        crate::builtins::call_native(name, args)
    }

    fn run_instruction(&mut self, inst: &Instr, module: &Module) -> Result<(), RuntimeError> {
        // 按**命名字段**解构，而不是"从 3 个槽位里按下标取"（旧编码的形状）。
        // `match *inst` 而非 `match inst`：`Instr` 是 Copy，按值匹配后每个绑定都是
        // `Operand` 而不是 `&Operand`，于是 arm 体里原来的表达式不用改类型。
        // 分组 arm（`A | B`）用位置别名 `a0/a1/a2`，因为 or-模式要求各分支绑定同名，
        // 而 `IndexGet { index }` 与 `PropGet { property }` 的字段名并不相同。
        match *inst {
            // `yield expr`: suspend the enclosing generator. The frame stays on
            // the value stack; `generator_next` lifts it out before rewinding
            // `rsp`. Jumping past the last instruction ends the nested `step`
            // loop the resumer is driving, exactly as a `Ret` to the sentinel
            // return address would.
            Instr::PrologueEnd {  } => {
                // Only reached while `create_generator` runs the parameter
                // prologue: on any later resume the instruction is already behind
                // the frame's pc. Parking mirrors `Yield` exactly — the frame is
                // what `run_generator_frame` snapshots — but no value is reported:
                // the generator is still `suspendedStart`, and its first `next()`
                // continues right after this instruction.
                if self.running_generator_prologue {
                    self.generator_yielded = Some(Value::Undefined);
                    self.generator_yield_pc = self.state.pc;
                    self.state.jump(module.instructions.len());
                }
            }
            Instr::Yield { value, .. } => {
                // `-1` is the "no operand" marker emitted for a bare `yield`
                // (codegen has nothing to point at, but the field is not
                // optional — P0b turns this into `Option<Operand>`).
                let value = match value {
                    Operand::Immd(-1) => Value::Undefined,
                    src => self.get_value(src)?,
                };
                // `yield expr` evaluates to the value the resumer supplied.
                self.generator_yielded = Some(value);
                self.generator_yield_pc = self.state.pc;
                self.state.jump(module.instructions.len());
            }
            Instr::Await { dst, src } => {
                // Same `-1` marker as `Yield`: a bare `await` has nothing to
                // await, so the operand carries the "absent" marker.
                let value = match src {
                    Operand::Immd(-1) => Value::Undefined,
                    src => self.get_value(src)?,
                };
                let awaited = self.await_value(value, module)?;
                self.set_value(dst, awaited)?;
            }
            // ===== Control Flow =====
            Instr::Call { func, argc } => {
                let func_id = func.as_immd();
                // The argument count is its own operand (see `CodeGen::gen_call`).
                let arg_count = argc.as_immd() as usize;
                // An `async` function answers a promise. Route it through
                // `invoke`, which drives the body and wraps its outcome, instead
                // of entering the frame directly.
                if self
                    .async_of(&Value::Function(func_id as u32), module)
                    .is_some()
                {
                    self.state.rbp = self.state.rsp;
                    let args = self.collect_call_args(arg_count)?;
                    let result = self.invoke(
                        &Value::Function(func_id as u32),
                        Value::Undefined,
                        &args,
                        module,
                    )?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }
                match module.symtab.get(&FunctionId::new(func_id as u32)) {
                    Some(location) => {
                        self.state.enter_frame(arg_count)?;
                        // Strict mode: this = undefined for regular function calls
                        self.state.this_val = Value::Undefined;
                        self.state.function_val = self.materialize_function(func_id as u32);
                        self.state.pushc(self.state.closure_var_stack.len())?;
                        self.state.pushc(self.state.seh_stack.len())?;
                        self.state.pushc(self.state.pc + 1)?;
                        self.state.construct_stack.push(false);
                        self.state.new_target_stack.push(Value::Undefined);
                        self.state.jump(*location);
                        return Ok(());
                    }
                    None => {
                        return Err(RuntimeError::ReferenceError(format!(
                            "undefined function: {func_id}"
                        )));
                    }
                }
            }
            Instr::CallEx { callee, argc } => {
                let arg_count = argc.as_immd() as usize;
                // Operands are read with the *caller's* frame pointer still in
                // place: a stack-slot operand would otherwise resolve inside the
                // callee's frame. The frame switch happens once they are read.
                let callee = self.get_value(callee)?;
                self.state.rbp = self.state.rsp;

                // A proxy is callable when its target is, and the call itself is
                // the `apply` trap — JavaScript either way, so `invoke` drives it.
                if Self::is_proxy(&callee) {
                    let args = self.collect_call_args(arg_count)?;
                    let result = self.invoke(&callee, Value::Undefined, &args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // Native (built-in) function: dispatch by name. This makes every
                // registered builtin callable without keeping a static whitelist
                // in sync with the lowering pass.
                if let Some(name) = crate::builtins::native_function_name(&callee) {
                    let args = self.collect_call_args(arg_count)?;
                    let result = self.call_native_by_name(&name, Value::Undefined, &args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                match callee {
                    Value::Function(id) => {
                        // `function*`: calling it only builds the generator;
                        // the body starts at the first `next()`.
                        if self.generator_of(&callee, module).is_some() {
                            let args = self.collect_call_args(arg_count)?;
                            let gobj = self.create_generator(
                                id,
                                Value::Undefined,
                                args,
                                Vec::new(),
                                module,
                            )?;
                            self.state.set_register(Register::Rv, gobj)?;
                            self.state.jump_offset(1);
                            return Ok(());
                        }
                        // `async` in its bare spelling: the call answers a
                        // promise (see `invoke_with_new_target`).
                        if self.async_of(&callee, module).is_some() {
                            let args = self.collect_call_args(arg_count)?;
                            let result = self.invoke(
                                &Value::Function(id),
                                Value::Undefined,
                                &args,
                                module,
                            )?;
                            self.state.set_register(Register::Rv, result)?;
                            self.state.jump_offset(1);
                            return Ok(());
                        }
                        // Strict mode: this = undefined for regular calls
                        // `return_pc` 与 `function_val` 先算出来：`open_frame` 要 `&mut self`，
                        // 调用参数里就不能再有别的 `self` 借用。
                        let return_pc = self.state.pc + 1;
                        let function_val = self.materialize_function(id);
                        self.open_frame(
                            id,
                            arg_count,
                            Value::Undefined,
                            function_val,
                            &[],
                            false,
                            Value::Undefined,
                            return_pc,
                            module,
                        )?;
                        return Ok(());
                    }
                    Value::Object(obj_ref) => {
                        // A class constructor cannot be called without `new`.
                        self.check_class_ctor(&Value::Object(Rc::clone(&obj_ref)))?;
                        // A FunctionObject coming from class/closure lowering.
                        let borrowed = obj_ref.borrow();
                        if borrowed.kind() == ObjectKind::Function {
                            let Some(func_obj) = borrowed
                                .as_any()
                                .downcast_ref::<crate::vm::object::FunctionObject>()
                            else {
                                return Err(RuntimeError::TypeError(
                                    "not a constructor".to_string(),
                                ));
                            };
                            let id = func_obj.func_id;
                            // Arrow functions capture `this`; others use undefined.
                            let captured_this = func_obj.captured_this.clone();
                            // Arrows also inherit the enclosing `new.target`.
                            let captured_new_target = func_obj.captured_new_target.clone();
                            let captured_vars = func_obj.captured_vars.clone();
                            drop(borrowed);

                            // `function*` in its *boxed* spelling: a generator
                            // method taken off its object (`var g = C.prototype.m; g()`),
                            // or a generator function expression materialized by
                            // `MakeFuncObj`. Like the bare arm above and
                            // `invoke`, this only builds the generator object —
                            // running the body here executed it eagerly and
                            // returned its (empty) `rv`, so
                            // `var it = (function* () {})()` was `undefined` and
                            // every parameter pattern that iterated it died with
                            // `GetMethod(undefined, @@iterator)`.
                            if module.generators.contains(&id) {
                                let args = self.collect_call_args(arg_count)?;
                                let gobj = self.create_generator(
                                    id,
                                    captured_this.unwrap_or(Value::Undefined),
                                    args,
                                    captured_vars,
                                    module,
                                )?;
                                self.state.set_register(Register::Rv, gobj)?;
                                self.state.jump_offset(1);
                                return Ok(());
                            }

                            // `async` in its *boxed* spelling (a method taken
                            // off its object, or a function expression): the call
                            // answers a promise.
                            if module.asyncs.contains(&id) {
                                let args = self.collect_call_args(arg_count)?;
                                let this = captured_this.unwrap_or(Value::Undefined);
                                let result = self.invoke(
                                    &Value::Object(Rc::clone(&obj_ref)),
                                    this,
                                    &args,
                                    module,
                                )?;
                                self.state.set_register(Register::Rv, result)?;
                                self.state.jump_offset(1);
                                return Ok(());
                            }

                            // `return_pc` 先算出来：`open_frame` 要 `&mut self`，不能再读 `self.state`。
                            let return_pc = self.state.pc + 1;
                            self.open_frame(
                                id,
                                arg_count,
                                captured_this.unwrap_or(Value::Undefined),
                                Value::Object(Rc::clone(&obj_ref)),
                                &captured_vars,
                                false,
                                captured_new_target.unwrap_or(Value::Undefined),
                                return_pc,
                                module,
                            )?;
                            return Ok(());
                        }
                        return Err(RuntimeError::TypeError("not a function".to_string()));
                    }
                    _ => return Err(RuntimeError::TypeError("not a function".to_string())),
                }
            }
            Instr::Jump { offset } => {
                let offset = offset.as_immd();
                self.state.jump_offset(offset);
                return Ok(());
            }
            Instr::DelayedJump { target, seh_depth } => {
                let offset = target.as_immd();
                let seh_depth = seh_depth.as_immd() as usize;

                // Check if we need to execute any finally blocks
                let mut finally_to_execute = None;
                let stack_len = self.state.seh_stack.len();
                let start_idx = stack_len.saturating_sub(seh_depth);

                // Innermost first, for the same reason as `Opcode::Ret`: the
                // epilogue can only finish the record on top of the stack.
                for idx in (start_idx..stack_len).rev() {
                    if let Some(record) = self.state.seh_stack.get(idx) {
                        if record.finally_pc != 0 && !record.in_finally {
                            finally_to_execute = Some((idx, record.finally_pc));
                            break;
                        }
                    }
                }

                if let Some((idx, finally_pc)) = finally_to_execute {
                    // Mark the record as in_finally and set delayed jump target
                    if let Some(record) = self.state.seh_stack.get_mut(idx) {
                        record.in_finally = true;
                        record.delayed_jump_target = Some(offset);
                    }
                    self.state.jump(finally_pc);
                    return Ok(());
                }

                // No `finally` to run anywhere on the way out: leave the frames
                // being unwound and land on the trampoline directly.
                //
                // The operand is an **absolute** PC — codegen patches it from the
                // block map precisely because `ResumeExc` (the finally path)
                // reads the same value as an address. Jumping *relative* here
                // landed past the last instruction and silently dropped
                // everything left in the program; that was
                // `for (x of y) { try { break; } catch (e) {} }`.
                for _ in 0..seh_depth {
                    self.state.seh_stack.pop();
                }
                self.state.jump(offset.max(0) as usize);
                return Ok(());
            }
            Instr::BrIf { condition, true_target, false_target } => {
                let cond = self.get_value(condition)?;
                let b = cond.to_boolean();
                let offset = if b {
                    true_target.as_immd()
                } else {
                    false_target.as_immd()
                };
                self.state.jump_offset(offset);
                return Ok(());
            }
            // Opcode::Ret is handled directly in run()

            // ===== Stack / Register Manipulation =====
            Instr::Mov { dst, src } => {
                let value = self.get_value(src)?;
                self.set_value(dst, value)?;
            }
            Instr::Push { src } => {
                let value = self.get_value(src)?;
                self.state.push(value)?;
            }
            Instr::Pop { dst } => {
                let value = self.state.pop()?;
                self.set_value(dst, value)?;
            }
            Instr::MovC { dst, src } => match (dst, src) {
                (Operand::Register(Register::Rsp), Operand::Register(Register::Rbp)) => {
                    self.state.rsp = self.state.rbp;
                }
                (Operand::Register(Register::Rbp), Operand::Register(Register::Rsp)) => {
                    self.state.rbp = self.state.rsp;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported MovC operands: {inst}"
                    )));
                }
            },
            Instr::PushC { src } => match src {
                Operand::Register(Register::Rsp) => {
                    self.state.pushc(self.state.rsp)?;
                }
                Operand::Register(Register::Rbp) => {
                    self.state.pushc(self.state.rbp)?;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported PushC operand: {inst}"
                    )));
                }
            },
            Instr::PopC { dst } => match dst {
                Operand::Register(Register::Rsp) => {
                    self.state.rsp = self.state.popc()?;
                }
                Operand::Register(Register::Rbp) => {
                    self.state.rbp = self.state.popc()?;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported PopC operand: {inst}"
                    )));
                }
            },
            Instr::AddC { dst, src, value } => match (dst, src) {
                (Operand::Register(Register::Rsp), Operand::Register(Register::Rsp)) => {
                    self.state.rsp += value.as_immd() as usize;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported AddC operands: {inst}"
                    )));
                }
            },
            Instr::SubC { dst, src, value } => match (dst, src) {
                (Operand::Register(Register::Rsp), Operand::Register(Register::Rsp)) => {
                    self.state.rsp -= value.as_immd() as usize;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported SubC operands: {inst}"
                    )));
                }
            },

            // ===== Load Instructions =====
            Instr::LoadConst { dst, index } => {
                let const_index = index.as_immd();
                let value = Self::from_constant(&module.constants[const_index as usize]);
                self.set_value(dst, value)?;
            }
            Instr::MakeRegExp { dst, source, flags } => {
                let source = match &module.constants[source.as_immd() as usize] {
                    Constant::String(v) => v.to_string(),
                };
                let flags = match &module.constants[flags.as_immd() as usize] {
                    Constant::String(v) => v.to_string(),
                };
                let proto = Some(Value::Object(Rc::clone(&self.builtins.regexp_prototype)));
                let value = crate::builtins::regexp_construct_value(
                    &[Value::string(&source), Value::string(&flags)],
                    proto,
                )?;
                self.set_value(dst, value)?;
            }
            Instr::DeclareLexical { name } => {
                let name_index = name.as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                // Idempotent, and never overwrites a value: the declaration that
                // follows is what initializes it.
                self.state.script_env.entry(name).or_insert(None);
            }
            Instr::InitLexical { name, value } => {
                let name_index = name.as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(value)?;
                // Ends the dead zone. This is the declaration's own write, so it
                // is allowed even though a plain `StoreEnv` to an uninitialized
                // binding would throw.
                self.state.script_env.insert(name, Some(value));
            }
            Instr::SetFunctionName { func, name } => {
                let func = self.get_value(func)?;
                let name = self.get_value(name)?;
                if let Value::String(text) = &name {
                    self.set_function_name(&func, text);
                }
            }
            Instr::LoadEnv { dst, name } => {
                let name_index = name.as_immd();
                let name = &module.constants[name_index as usize];
                match name {
                    Constant::String(name) => {
                        match self.state.resolve_env_name(name.as_str())? {
                            Some(value) => {
                                self.set_value(dst, value)?;
                            }
                            None => {
                                // Fall back to global environment
                                match self.state.get_global(name) {
                                    Some(value) => {
                                        self.set_value(dst, value)?;
                                    }
                                    None => {
                                        return Err(RuntimeError::ReferenceError(format!(
                                            "undefined variable: {name}"
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ===== Arithmetic =====
            Instr::Addx { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                // ES `Add`: ToPrimitive both operands, then string-concat if either
                // is a string, otherwise numeric addition.
                let lprim = self.to_primitive(&lhs, "default", module)?;
                let rprim = self.to_primitive(&rhs, "default", module)?;
                let value = if lprim.is_string() || rprim.is_string() {
                    Value::String(Rc::new(format!(
                        "{}{}",
                        lprim.to_js_string(),
                        rprim.to_js_string()
                    )))
                } else {
                    lprim + rprim
                };
                self.set_value(dst, value)?;
            }
            Instr::Subx { dst, lhs, rhs } => {
                // ToNumeric starts with ToPrimitive(hint number), as `Pow`
                // already did: without it an object operand (`date1 - date2`, a
                // boxed Number) collapses to NaN.
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let lprim = self.to_primitive(&lhs, "number", module)?;
                let rprim = self.to_primitive(&rhs, "number", module)?;
                let value = lprim - rprim;
                self.set_value(dst, value)?;
            }
            Instr::Mulx { dst, lhs, rhs } => {
                // ToNumeric starts with ToPrimitive(hint number), as `Pow`
                // already did: without it an object operand (`date1 - date2`, a
                // boxed Number) collapses to NaN.
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let lprim = self.to_primitive(&lhs, "number", module)?;
                let rprim = self.to_primitive(&rhs, "number", module)?;
                let value = lprim * rprim;
                self.set_value(dst, value)?;
            }
            Instr::Divx { dst, lhs, rhs } => {
                // ToNumeric starts with ToPrimitive(hint number), as `Pow`
                // already did: without it an object operand (`date1 - date2`, a
                // boxed Number) collapses to NaN.
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let lprim = self.to_primitive(&lhs, "number", module)?;
                let rprim = self.to_primitive(&rhs, "number", module)?;
                let value = lprim / rprim;
                self.set_value(dst, value)?;
            }
            Instr::Remx { dst, lhs, rhs } => {
                // ToNumeric starts with ToPrimitive(hint number), as `Pow`
                // already did: without it an object operand (`date1 - date2`, a
                // boxed Number) collapses to NaN.
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let lprim = self.to_primitive(&lhs, "number", module)?;
                let rprim = self.to_primitive(&rhs, "number", module)?;
                let value = lprim % rprim;
                self.set_value(dst, value)?;
            }
            Instr::Pow { dst, lhs, rhs } => {
                // ES `ExponentiationExpression`: ToNumeric on both operands,
                // then `**`. Like the other arithmetic opcodes this does not
                // model the BigInt case (the engine has no BigInt).
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let lprim = self.to_primitive(&lhs, "number", module)?;
                let rprim = self.to_primitive(&rhs, "number", module)?;
                let value = Value::Number(lprim.to_number().powf(rprim.to_number()));
                self.set_value(dst, value)?;
            }

            // ===== Unary =====
            Instr::Not { dst, src } => {
                let value = self.get_value(src)?;
                let result = Value::Bool(!value.to_boolean());
                self.set_value(dst, result)?;
            }
            Instr::BitNot { dst, src } => {
                // Bitwise NOT: convert to 32-bit signed integer, flip bits
                let value = self.get_value(src)?;
                let num = to_int32(value.to_number());
                let result = Value::Number((!num) as f64);
                self.set_value(dst, result)?;
            }
            Instr::BitAnd { dst: a0, lhs: a1, rhs: a2 }
              | Instr::BitOr { dst: a0, lhs: a1, rhs: a2 }
              | Instr::BitXor { dst: a0, lhs: a1, rhs: a2 } => {
                let opcode = inst.opcode();
                let lhs = self.get_value(a1)?;
                let rhs = self.get_value(a2)?;
                let a = to_int32(lhs.to_number());
                let b = to_int32(rhs.to_number());
                let result = match opcode {
                    Opcode::BitAnd => a & b,
                    Opcode::BitOr => a | b,
                    _ => a ^ b,
                };
                self.set_value(a0, Value::Number(result as f64))?;
            }
            Instr::Shl { dst: a0, lhs: a1, rhs: a2 }
              | Instr::Shr { dst: a0, lhs: a1, rhs: a2 }
              | Instr::UShr { dst: a0, lhs: a1, rhs: a2 } => {
                let opcode = inst.opcode();
                let lhs = self.get_value(a1)?;
                let rhs = self.get_value(a2)?;
                let shift_count = (to_uint32(rhs.to_number()) & 0x1F) as u32;
                let result = match opcode {
                    Opcode::Shl => to_int32(lhs.to_number()).wrapping_shl(shift_count),
                    Opcode::Shr => to_int32(lhs.to_number()).wrapping_shr(shift_count),
                    _ => (to_uint32(lhs.to_number()).wrapping_shr(shift_count)) as i32,
                };
                self.set_value(a0, Value::Number(result as f64))?;
            }
            Instr::Neg { dst, src } => {
                let value = self.get_value(src)?;
                let result = -value;
                self.set_value(dst, result)?;
            }

            // ===== Logical =====
            Instr::And { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                // JS logical AND: returns first falsy or last value
                let value = if !lhs.to_boolean() { lhs } else { rhs };
                self.set_value(dst, value)?;
            }
            Instr::Or { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                // JS logical OR: returns first truthy or last value
                let value = if lhs.to_boolean() { lhs } else { rhs };
                self.set_value(dst, value)?;
            }

            // ===== Comparison =====
            // js_abstract_relational(x, y) returns x < y
            Instr::Greater { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                // lhs > rhs  ⟺  rhs < lhs
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let result = Self::js_abstract_relational(&rhs, &lhs)?;
                self.set_value(dst, Value::Bool(result))?;
            }
            Instr::GreaterEqual { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                // lhs >= rhs  ⟺  !(lhs < rhs)
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let less = Self::js_abstract_relational(&lhs, &rhs)?;
                self.set_value(dst, Value::Bool(!less))?;
            }
            Instr::Less { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let result = Self::js_abstract_relational(&lhs, &rhs)?;
                self.set_value(dst, Value::Bool(result))?;
            }
            Instr::LessEqual { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                // lhs <= rhs  ⟺  !(rhs < lhs)
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let less = Self::js_abstract_relational(&rhs, &lhs)?;
                self.set_value(dst, Value::Bool(!less))?;
            }
            Instr::Equal { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let result = self.abstract_eq(&lhs, &rhs, module)?;
                self.set_value(dst, Value::Bool(result))?;
            }
            Instr::NotEqual { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let result = !self.abstract_eq(&lhs, &rhs, module)?;
                self.set_value(dst, Value::Bool(result))?;
            }
            Instr::StrictEqual { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let result = lhs.strict_eq(&rhs);
                self.set_value(dst, Value::Bool(result))?;
            }
            Instr::StrictNotEqual { dst, lhs, rhs } => {
                let lhs = self.get_value(lhs)?;
                let rhs = self.get_value(rhs)?;
                let result = !lhs.strict_eq(&rhs);
                self.set_value(dst, Value::Bool(result))?;
            }

            // ===== Type =====
            Instr::TypeOf { dst, src } => {
                let value = self.get_value(src)?;
                let type_str = value.type_of().to_string();
                self.set_value(dst, Value::String(Rc::new(type_str)))?;
            }
            Instr::TypeOfEnv { dst, name } => {
                // `typeof unresolvableName` is "undefined" — never a
                // ReferenceError. The name is a constant-pool index.
                let name_index = name.as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                // A dead-zone read *does* throw here — unlike an unresolvable
                // name, which is the one case `typeof` has to tolerate.
                let value = self.state.resolve_env_name(&name)?;
                let type_str = match value {
                    Some(v) => v.type_of().to_string(),
                    None => "undefined".to_string(),
                };
                self.set_value(dst, Value::String(Rc::new(type_str)))?;
            }
            Instr::InstanceOf { dst, lhs, rhs } => {
                let obj = self.get_value(lhs)?;
                let ctor = self.get_value(rhs)?;
                // A bare `Value::Function(id)` must be boxed first so its
                // (single, stable) `prototype` object is reachable.
                let ctor = self.as_object_value(&ctor);

                // ES 12.10.4: a callable `@@hasInstance` on the right-hand side
                // replaces the ordinary prototype-chain check. The arm must not
                // return early — `run_instruction` advances the PC at its end.
                let mut exotic_result: Option<bool> = None;
                if ctor.is_object() {
                    let has_instance = self.get_member(
                        &ctor,
                        &crate::builtins::has_instance_symbol_key(),
                        module,
                    )?;
                    if !has_instance.is_undefined() {
                        if !has_instance.is_callable() {
                            return Err(RuntimeError::TypeError(
                                "Symbol.hasInstance is not a function".to_string(),
                            ));
                        }
                        let result =
                            self.invoke(&has_instance, ctor.clone(), &[obj.clone()], module)?;
                        exotic_result = Some(result.to_boolean());
                    }
                }

                let result = match exotic_result {
                    Some(found) => found,
                    None => match ctor {
                        Value::Object(ctor_ref) => {
                            let ctor_proto = ctor_ref
                                .borrow()
                                .property_get(&PropertyKey::from("prototype"));
                            match ctor_proto {
                                Some(desc) => {
                                    let target_proto = desc.value;
                                    // Walk the object's prototype chain
                                    let mut current = match &obj {
                                        Value::Object(obj_ref) => obj_ref.borrow().get_prototype(),
                                        _ => None,
                                    };
                                    let mut found = false;
                                    while let Some(proto_ref) = current {
                                        if Value::Object(Rc::clone(&proto_ref)) == target_proto {
                                            found = true;
                                            break;
                                        }
                                        current = proto_ref.borrow().get_prototype();
                                    }
                                    found
                                }
                                None => false,
                            }
                        }
                        _ => {
                            return Err(RuntimeError::TypeError(
                                "Right-hand side of 'instanceof' is not callable".to_string(),
                            ));
                        }
                    },
                };

                self.set_value(dst, Value::Bool(result))?;
            }
            Instr::In { dst, lhs, rhs } => {
                let key = self.resolve_property_key(lhs, module)?;
                let obj = self.get_value(rhs)?;
                let obj = self.as_object_value(&obj);
                let result = match obj {
                    Value::Object(obj_ref) => {
                        // A `has` trap answers the `in` operator outright
                        // (ES 10.5.7); with no trap the lookup is the target's.
                        if obj_ref.borrow().kind() == ObjectKind::Proxy {
                            let proxy_val = Value::Object(Rc::clone(&obj_ref));
                            match self.proxy_has(&proxy_val, &key, module)? {
                                Some(has) => has,
                                None => crate::vm::prototype::internal_has_property(obj_ref, &key)
                                    .map_err(RuntimeError::TypeError)?,
                            }
                        } else {
                            crate::vm::prototype::internal_has_property(obj_ref, &key)
                                .map_err(RuntimeError::TypeError)?
                        }
                    }
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "Cannot use 'in' operator on non-object".to_string(),
                        ));
                    }
                };
                self.set_value(dst, Value::Bool(result))?;
            }

            // ===== Iteration =====
            Instr::DelegateOpen { iter } => {
                let iter = self.get_value(iter)?;
                self.delegate_stack.push(iter);
            }
            Instr::DelegateClose { .. } => {
                self.delegate_stack.pop();
            }
            Instr::MakeIter { dst, src } => {
                let src = self.get_value(src)?;
                let iter_val = self.make_iterator(src, module)?;
                self.set_value(dst, iter_val)?;
            }
            Instr::IterNext { dst, has_next, src } => {
                // Operand order comes from codegen: (item, has_next, src).
                let iter_val = self.get_value(src)?;
                let (item, done) = self.iterator_next(iter_val, None, module)?;
                self.set_value(dst, item)?;
                self.set_value(has_next, Value::Bool(!done))?;
            }
            Instr::IterClose { iter } => {
                let iter_val = self.get_value(iter)?;
                self.iterator_close(iter_val, None, false, module)?;
            }
            Instr::RequireObjectCoercible { src } => {
                let src = self.get_value(src)?;
                crate::builtins::require_object_coercible(&src)?;
            }
            Instr::MakeRest { dst, from } => {
                // Collect the tail of the incoming arguments into a fresh
                // array: args[from..] where arg i lives at [rbp - (i + 1)].
                let from = from.as_immd().max(0) as usize;
                let argc = self.state.frame_argc.last().copied().unwrap_or(0);
                let rbp = self.state.rbp as isize;
                let mut arr = ArrayObject::new();
                if argc > from {
                    for i in from..argc {
                        let index = rbp - (i as isize) - 1;
                        let val = self
                            .state
                            .data_stack
                            .get(index.max(0) as usize)
                            .cloned()
                            .unwrap_or(Value::Undefined);
                        arr.push(val);
                    }
                }
                arr.set_prototype(Some(Rc::clone(&self.builtins.array_prototype)));
                self.set_value(dst, Value::Object(Rc::new(RefCell::new(arr))))?;
            }
            Instr::CallSpread { callee, this, args } => {
                // operands: (callee, this, args-array); result goes to Rv.
                let callee = self.get_value(callee)?;
                let this = self.get_value(this)?;
                let args_val = self.get_value(args)?;
                let args = self
                    .array_like_elements(&args_val)
                    .ok_or_else(|| {
                        RuntimeError::TypeError(
                            "CallSpread: arguments must be array-like".to_string(),
                        )
                    })?;
                let result = self.invoke(&callee, this, &args, module)?;
                self.state.set_register(Register::Rv, result)?;
            }
            Instr::CallSuperSpread { callee, this, args } => {
                // `super(...)`: same shape as CallSpread, but the parent
                // constructor inherits this frame's `new.target`.
                let callee = self.get_value(callee)?;
                // The receiver is normally the *frame's* `this`, not an operand
                // (lowering a `load_this` would raise in a derived constructor,
                // whose `this` is still uninitialized here). An arrow body that
                // calls `super()` still carries it as an operand, though.
                let operand_this = self.get_value(this)?;
                let this = if operand_this.is_undefined() {
                    self.state.this_val.clone()
                } else {
                    operand_this
                };
                let args_val = self.get_value(args)?;
                let args = self
                    .array_like_elements(&args_val)
                    .ok_or_else(|| {
                        RuntimeError::TypeError(
                            "CallSuperSpread: arguments must be array-like".to_string(),
                        )
                    })?;
                let new_target = self
                    .state
                    .new_target_stack
                    .last()
                    .cloned()
                    .unwrap_or(Value::Undefined);
                // ES 12.3.5.1: `super()` binds the derived constructor's `this`.
                // Cleared *before* the call runs, because the parent's own frame
                // pushes an entry onto the same stack.
                // The innermost frame with an unbound `this` is the one being
                // bound — an arrow body can run the `super()` on its enclosing
                // constructor's behalf, and only derived constructors ever set
                // the flag at all.
                if let Some(slot) = self
                    .state
                    .this_state
                    .iter_mut()
                    .rev()
                    .find(|state| **state == THIS_DERIVED_UNBOUND)
                {
                    *slot = THIS_DERIVED_BOUND;
                }
                let result =
                    self.invoke_with_new_target(&callee, this, &args, Some(new_target), module)?;
                // ES 12.3.5.1 `BindThisValue`: whatever `super()` produced *is*
                // the derived `this`. A built-in parent builds its own instance
                // (`Error` sets `message`), so the pre-created object has to be
                // replaced by it; a bytecode parent hands the same one back.
                if result.is_object() {
                    self.state.this_val = result.clone();
                }
                self.state.set_register(Register::Rv, result)?;
            }
            Instr::NewSpread { dst, ctor, args } => {
                // operands: (dst, constructor, args-array)
                let ctor = self.get_value(ctor)?;
                let args_val = self.get_value(args)?;
                let args = self
                    .array_like_elements(&args_val)
                    .ok_or_else(|| {
                        RuntimeError::TypeError(
                            "NewSpread: arguments must be array-like".to_string(),
                        )
                    })?;
                let constructed = self.construct(&ctor, &args, module)?;
                self.set_value(dst, constructed)?;
            }
            Instr::ToString { dst, src } => {
                let src = self.get_value(src)?;
                let s = match self.to_primitive(&src, "string", module) {
                    Ok(v) => v.to_js_string(),
                    Err(_) => src.to_js_string(),
                };
                self.set_value(dst, Value::string(&s))?;
            }
            Instr::ToNumber { dst, src } => {
                // ES 7.1.4 ToNumber: objects convert through
                // ToPrimitive(hint "number"), which may run user code.
                let src = self.get_value(src)?;
                let prim = match self.to_primitive(&src, "number", module) {
                    Ok(v) => v,
                    Err(_) => src.clone(),
                };
                self.set_value(dst, Value::Number(prim.to_number()))?;
            }

            // ===== Object/Array Operations =====
            Instr::MakeArray { dst } => {
                let mut arr = crate::vm::object::ArrayObject::new();
                arr.set_prototype(Some(Rc::clone(&self.builtins.array_prototype)));
                let arr_val = Value::Object(Rc::new(RefCell::new(arr)));
                self.set_value(dst, arr_val)?;
            }
            Instr::ArrayPushSpread { array, src } => {
                // `[...src]` / `f(...src)`: append every element of `src`. The
                // loop runs here rather than in lowered bytecode so that no value
                // has to stay live across a basic-block boundary.
                //
                // ES 13.2.5.5 (`ArrayAccumulation`) and 13.3.8.1 both go through
                // `GetIterator`, so a replaced or deleted `@@iterator` has to be
                // honoured and accessors on the source have to run. This used to
                // call `array_like_elements`, a `length` + integer-key snapshot:
                // neither the protocol nor accessors were observable, and
                // `delete Array.prototype[Symbol.iterator]` left `[...[1]]`
                // working (see §2.3 of the conformance log).
                //
                // No `IteratorClose` is owed: the source is iterated to
                // exhaustion, and an abrupt `next()` propagates as-is — ES
                // 13.2.5.5 has no close step.
                let array_val = self.get_value(array)?;
                let src = self.get_value(src)?;
                let iter = self.make_iterator(src, module)?;
                let mut items: Vec<Value> = Vec::new();
                loop {
                    let (item, done) = self.iterator_next(iter.clone(), None, module)?;
                    if done {
                        break;
                    }
                    items.push(item);
                }
                match array_val {
                    Value::Object(obj_ref) => {
                        let mut obj = obj_ref.borrow_mut();
                        let Some(arr) = obj.as_any_mut().downcast_mut::<ArrayObject>() else {
                            return Err(RuntimeError::TypeError(
                                "ArrayPushSpread on non-array object".to_string(),
                            ));
                        };
                        for item in items {
                            arr.push(item);
                        }
                        Ok(())
                    }
                    _ => Err(RuntimeError::TypeError(
                        "ArrayPushSpread on non-object".to_string(),
                    )),
                }?;
            }
            Instr::MarkHole { array } => {
                let Value::Object(obj_ref) = self.get_value(array)? else {
                    return Ok(());
                };
                let mut borrowed = obj_ref.borrow_mut();
                if let Some(arr) = borrowed
                    .as_any_mut()
                    .downcast_mut::<crate::vm::object::ArrayObject>()
                {
                    let index = arr.len().saturating_sub(1);
                    arr.mark_hole(index);
                }
            }
            Instr::ArrayPush { array, value } => {
                let array_val = self.get_value(array)?;
                let elem = self.get_value(value)?;
                match array_val {
                    Value::Object(obj_ref) => {
                        let mut obj = obj_ref.borrow_mut();
                        if let Some(arr) = obj.as_any_mut().downcast_mut::<ArrayObject>() {
                            arr.push(elem);
                            Ok(())
                        } else {
                            Err(RuntimeError::TypeError(
                                "ArrayPush on non-array object".to_string(),
                            ))
                        }
                    }
                    _ => Err(RuntimeError::TypeError(
                        "ArrayPush on non-object".to_string(),
                    )),
                }?;
            }
            Instr::MakeObject { dst } => {
                let mut obj = crate::vm::object::OrdinaryObject::new();
                obj.set_prototype(Some(Rc::clone(&self.builtins.object_prototype)));
                let obj_val = Value::Object(Rc::new(RefCell::new(obj)));
                self.set_value(dst, obj_val)?;
            }
            Instr::IndexGet { dst: a0, object: a1, index: a2 }
              | Instr::PropGet { dst: a0, object: a1, property: a2 } => {
                let obj = self.get_value(a1)?;
                let key = self.resolve_property_key(a2, module)?;
                // Boxing a bare function reference has to be written back so that
                // later uses of the same slot observe the same prototype object.
                let obj = self.as_object_value(&obj);
                // Only writable locations can cache the boxed function object;
                // an inline `Value::Function` operand (e.g. `(function(){}).x`)
                // is not addressable.
                if is_addressable(a1) {
                    if matches!(self.get_value(a1)?, Value::Function(_)) {
                        self.set_value(a1, obj.clone())?;
                    }
                }
                let value = self.get_member(&obj, &key, module)?;
                self.set_value(a0, value)?;
            }
            Instr::IndexSet { object: a0, index: a1, value: a2 }
              | Instr::PropSet { object: a0, property: a1, value: a2 } => {
                let obj = self.get_value(a0)?;
                let key = self.resolve_property_key(a1, module)?;
                let val = self.get_value(a2)?;
                let obj = self.as_object_value(&obj);
                self.set_member(&obj, key, val, module)?;
                // Persist the boxed function object back into its slot.
                if is_addressable(a0) {
                    self.set_value(a0, obj)?;
                }
            }
            Instr::IndexDelete { dst: a0, object: a1, index: a2 }
              | Instr::PropDelete { dst: a0, object: a1, property: a2 } => {
                let obj = self.get_value(a1)?;
                let key = self.resolve_property_key(a2, module)?;
                let obj = self.as_object_value(&obj);
                let removed = self.delete_member(&obj, &key, module)?;
                self.set_value(a0, Value::Bool(removed))?;
            }
            Instr::Arguments { dst } => {
                // The callee's arguments live just below its frame pointer:
                // `arg i` sits at `rbp - argc + i` (see `enter_frame`).
                let argc = self.state.frame_argc.last().copied().unwrap_or(0);
                let rbp = self.state.rbp;
                let mut arr = ArrayObject::new();
                for i in 0..argc {
                    // `load_arg(i)` reads `[rbp - (i + 1)]`, so `arguments[i]`
                    // must use exactly the same slot.
                    let index = rbp as isize - i as isize - 1;
                    let val = if index >= 0 {
                        self.state
                            .data_stack
                            .get(index as usize)
                            .cloned()
                            .unwrap_or(Value::Undefined)
                    } else {
                        Value::Undefined
                    };
                    arr.push(val);
                }
                let obj = Value::Object(Rc::new(RefCell::new(arr)));
                self.set_value(dst, obj)?;
            }
            Instr::StoreEnv { name, value } => {
                let name_index = name.as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(value)?;
                match self.state.script_env.get_mut(&name) {
                    Some(slot @ Some(_)) => *slot = Some(value),
                    // Writing into a binding that has not been initialized is the
                    // dead zone too (a `let x;` declared after the loop that
                    // assigns to it).
                    Some(None) => {
                        return Err(RuntimeError::ReferenceError(format!(
                            "Cannot access '{name}' before initialization"
                        )))
                    }
                    None => {
                        self.state.globals.borrow_mut().insert(name, value);
                    }
                }
            }
            Instr::CallMethod { callee, property, argc } => {
                let mut obj_val = self.get_value(callee)?;
                // A bare function reference must be boxed before looking up the
                // method, otherwise `F.method()` silently resolves to undefined.
                obj_val = self.as_object_value(&obj_val);
                if is_addressable(callee)
                    && matches!(self.get_value(callee)?, Value::Function(_))
                {
                    self.set_value(callee, obj_val.clone())?;
                }
                let arg_count = argc.as_immd() as usize;

                let method_name = match property {
                    Operand::Immd(id) => match &module.constants[id as usize] {
                        Constant::String(s) => s.as_str().to_string(),
                    },
                    _ => {
                        let prop_val = self.get_value(property)?;
                        prop_val.to_js_string()
                    }
                };

                // Symbol-keyed method call (`obj[Symbol.iterator]()`): the key
                // must be resolved through the property map by symbol identity;
                // stringifying it (the default path below) would never match.
                // The key value is read with the caller's frame pointer; the
                // dispatch itself happens after the frame switch below.
                let symbol_key = if matches!(property, Operand::Immd(_)) {
                    None
                } else {
                    match self.get_value(property)? {
                        Value::Symbol(sym) => Some(PropertyKey::Symbol(sym.id)),
                        _ => None,
                    }
                };

                // Operands have been read with the caller's frame pointer; switch
                // to the callee's frame before collecting the outgoing arguments.
                // The switch must happen on EVERY path through here (including
                // the symbol-keyed early return below): the epilogue emitted by
                // codegen (`movc rsp, rbp; popc rbp`) expects rbp to be the new
                // frame base.
                self.state.rbp = self.state.rsp;

                // Read the outgoing arguments. These live below the *new* frame
                // pointer, so they must not go through the "missing argument"
                // rule (which is about the callee's own parameters).
                let mut raw_args = Vec::with_capacity(arg_count);
                for i in 0..arg_count {
                    let index = self.state.rbp - i - 1;
                    raw_args.push(self.state.raw_stack_value(index));
                }
                // Box bare function references: the builtin layer matches on
                // `Value::Object`, so `Object.keys(fn)` & friends would
                // otherwise be handed a `Value::Function(id)` it cannot read.
                let args: Vec<Value> = raw_args
                    .iter()
                    .map(|a| self.as_object_value(a))
                    .collect();

                if let Some(key) = symbol_key {
                    let method_val = match &obj_val {
                        Value::Object(obj_ref) => {
                            match crate::vm::prototype::find_descriptor(
                                Rc::clone(obj_ref),
                                &key,
                            )
                            .map_err(RuntimeError::TypeError)?
                            {
                                Some((_owner, desc)) => desc.value,
                                None => Value::Undefined,
                            }
                        }
                        _ => Value::Undefined,
                    };
                    if !method_val.is_callable() {
                        return Err(RuntimeError::TypeError(
                            "not a function".to_string(),
                        ));
                    }
                    let result =
                        self.invoke(&method_val, obj_val.clone(), &args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // `f.call(thisArg, …)` / `f.apply(thisArg, argsArray)` — the
                // only way to invoke a callable with an explicit `this`.
                // `f.bind(thisArg, ...args)` — returns a wrapper that remembers
                // the bound `this` and leading arguments.
                if method_name == "bind" && obj_val.is_callable() {
                    let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                    let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
                    let wrapper = self.make_bound(obj_val, this_arg, bound_args);
                    self.state.set_register(Register::Rv, wrapper)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                if (method_name == "call" || method_name == "apply") && obj_val.is_callable() {
                    let call_args = self.call_or_apply_args(&method_name, &args)?;
                    let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                    let result = self.invoke(&obj_val, this_arg, &call_args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // Array callback methods (`map`, `filter`, `reduce`, `sort`, …)
                // are executed here so they can call the user function.
                if let Some(result) =
                    self.try_array_callback_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // A function replacer for `String.prototype.replace` runs here
                // for the same reason (it calls user code).
                if let Some(result) =
                    self.try_string_callback_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }
                // `then` / `catch` / `finally` likewise (they call the handler).
                // This is the second method-call path — a chained call like
                // `new Promise(f).catch(cb)` comes through here, not through the
                // one above, and used to silently do nothing.
                if let Some(result) = self.try_promise_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // First check: is this a static method on a native function (constructor)?
                if let Some(name) = crate::builtins::native_function_name(&obj_val) {
                    let static_method = format!("{}.{}", name, method_name);
                    // `Object.assign` needs real `[[Get]]`/`[[Set]]` (accessors).
                    if static_method == "Object.assign" {
                        let result = self.object_assign(&args, module)?;
                        self.state.set_register(Register::Rv, result)?;
                        self.state.jump_offset(1);
                        return Ok(());
                    }
                    // Statics that read their inputs through `[[Get]]` (a
                    // descriptor object's fields may be accessors) can only be
                    // carried out by the VM, which is what the prototype-chain
                    // lookup below reaches through `call_native_by_name`. Trying
                    // them against the plain builtin layer first would either
                    // fail or lose the getter side effects.
                    let vm_handled_static = matches!(
                        static_method.as_str(),
                        // These have to reach `call_native_by_name` because a
                        // proxy receiver turns them into trap calls; the VM
                        // falls back to the same builtin for anything else.
                        "Object.defineProperty"
                            | "Object.defineProperties"
                            | "Object.create"
                            | "Proxy.revocable"
                            | "Object.getPrototypeOf"
                            | "Object.setPrototypeOf"
                            | "Object.isExtensible"
                            | "Object.preventExtensions"
                            | "Object.getOwnPropertyDescriptor"
                            | "Object.keys"
                            | "Object.getOwnPropertyNames"
                            | "Object.getOwnPropertySymbols"
                    );
                    if !vm_handled_static {
                        // A missing static falls through to the prototype chain,
                        // but a static that *exists* and fails (RangeError,
                        // TypeError, …) must propagate rather than be swallowed
                        // by the fallback.
                        match crate::builtins::call_static_method(&static_method, &args) {
                            Ok(result) => {
                                self.state.set_register(Register::Rv, result)?;
                                self.state.jump_offset(1);
                                return Ok(());
                            }
                            Err(err) if crate::builtins::is_unknown_builtin(&err) => {}
                            Err(err) => return Err(err),
                        }
                    }
                }

                // `slice` / `splice` / `concat` are pure enough for the builtin
                // layer, but their result array comes from `ArraySpeciesCreate`,
                // whose `Get`s and `Construct` can run user code.
                if let Some(result) =
                    self.try_array_species_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // `matchAll` answers an iterator, which the builtin layer cannot
                // build — and a *primitive* string receiver never reaches the
                // native dispatch further down (`lookup_property_on_object` only
                // reads real objects), so it has to be caught here.
                if let Some(result) =
                    self.try_match_all_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // Try built-in prototype method dispatch. As above: only
                // "no such method" may fall through.
                match crate::builtins::call_prototype_method(&obj_val, &method_name, &args) {
                    Ok(result) => {
                        self.state.set_register(Register::Rv, result)?;
                        self.state.jump_offset(1);
                        return Ok(());
                    }
                    Err(err) if crate::builtins::is_unknown_builtin(&err) => {}
                    Err(err) => return Err(err),
                }

                // Look up user-defined method on the object's prototype chain
                let method_val = self.lookup_property_on_object(&obj_val, &method_name, module)?;

                // A native prototype method (e.g. a method reached through the
                // prototype chain rather than the fast dispatch above).
                if let Some(name) = crate::builtins::native_function_name(&method_val) {
                    let result = self.call_native_by_name(&name, obj_val, &args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // `function*` reached as a *method* (`class C { *m() {} }`,
                // `obj.m` where `m` is a generator): build the generator object
                // instead of running the body, exactly like the `Call` opcode
                // and `invoke` do. Without this the body ran in the *caller's*
                // frame: its `Ret` returned to the top level, so the module
                // itself produced a generator object as its result and
                // `it.next()` was `undefined` — which is what made every
                // `class { *m() {} }` test fail.
                if let Some(id) = self.generator_of(&method_val, module) {
                    let gobj = self.create_generator(
                        id,
                        obj_val.clone(),
                        args.clone(),
                        Vec::new(),
                        module,
                    )?;
                    self.state.set_register(Register::Rv, gobj)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }
                // An `async` method (`obj.m()`): the call answers a promise.
                if self.async_of(&method_val, module).is_some() {
                    let result =
                        self.invoke(&method_val, obj_val.clone(), &args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                match method_val {
                    Value::Function(id) => {
                        // User-defined bytecode function - call it with this = obj_val
                        let return_pc = self.state.pc + 1;
                        let function_val = self.materialize_function(id);
                        self.open_frame(
                            id,
                            arg_count,
                            obj_val,
                            function_val,
                            &[],
                            false,
                            Value::Undefined,
                            return_pc,
                            module,
                        )?;
                        return Ok(());
                    }
                    Value::Object(obj_ref) => {
                        let borrowed = obj_ref.borrow();
                        if borrowed.kind() == ObjectKind::Function {
                            if let Some(func_obj) =
                                borrowed.as_any().downcast_ref::<FunctionObject>()
                            {
                                let id = func_obj.func_id;
                                // Check if this is an arrow function with captured this
                                let captured_this = func_obj.captured_this.clone();
                                let captured_new_target = func_obj.captured_new_target.clone();
                                let captured_vars = func_obj.captured_vars.clone();
                                drop(borrowed);
                                // For arrow functions, use captured this; for regular methods, use obj_val
                                let return_pc = self.state.pc + 1;
                                let this_val = captured_this.unwrap_or(obj_val);
                                self.open_frame(
                                    id,
                                    arg_count,
                                    this_val,
                                    Value::Object(Rc::clone(&obj_ref)),
                                    &captured_vars,
                                    false,
                                    captured_new_target.unwrap_or(Value::Undefined),
                                    return_pc,
                                    module,
                                )?;
                                return Ok(());
                            } else {
                                return Err(RuntimeError::TypeError("not a function".to_string()));
                            }
                        } else {
                            return Err(RuntimeError::TypeError("not a function".to_string()));
                        }
                    }
                    Value::Undefined => {
                        // Method not found
                        self.state.set_register(Register::Rv, Value::Undefined)?;
                    }
                    _ => {
                        return Err(RuntimeError::TypeError(format!(
                            "{} is not a function",
                            method_name
                        )));
                    }
                }
            }

            // ===== Exception Handling =====
            Instr::Try { catch_offset, finally_offset } => {
                let catch_offset = catch_offset.as_immd();
                let finally_offset = finally_offset.as_immd();
                let catch_pc = if catch_offset != 0 {
                    (self.state.pc as isize + catch_offset) as usize
                } else {
                    0
                };
                let finally_pc = if finally_offset != 0 {
                    (self.state.pc as isize + finally_offset) as usize
                } else {
                    0
                };
                let record = SehRecord {
                    handler_pc: catch_pc,
                    finally_pc,
                    saved_rsp: self.state.rsp,
                    saved_rbp: self.state.rbp,
                    in_finally: false,
                    pending_exception: None,
                    catch_executed: false,
                    delayed_jump_target: None,
                    delayed_return: false,
                    saved_closure_depth: self.state.closure_var_stack.len(),
                    saved_ctrl_depth: self.state.ctrl_stack.len(),
                    saved_this_depth: self.state.this_stack.len(),
                    saved_construct_depth: self.state.construct_stack.len(),
                    saved_new_target_depth: self.state.new_target_stack.len(),
                };
                self.state.seh_stack.push(record);
            }
            Instr::EndTry {  } => {
                self.state.seh_stack.pop();
            }
            Instr::ThrowExc { value } => {
                let exc_val = match value {
                    Operand::Immd(id) => {
                        let constant = &module.constants[id as usize];
                        VM::from_constant(constant)
                    }
                    _ => self.get_value(value)?,
                };
                return self.handle_throw(exc_val);
            }
            Instr::LoadException { dst } => {
                let exc_val = self.state.get_register(Register::Rv)?;
                self.set_value(dst, exc_val)?;
            }

            // ===== Function/Closure =====
            Instr::CreateClosure { dst, func } => {
                let func_id = func.as_immd();
                self.set_value(dst, Value::Function(func_id as u32))?;
            }
            Instr::New { callee, argc } => {
                let constructor_val = self.get_value(callee)?;
                let arg_count = argc.as_immd() as usize;
                // Operands are read with the caller's frame pointer; the frame
                // switch happens afterwards, before the arguments are collected.
                self.state.rbp = self.state.rsp;

                // `new.target` inside the constructor is this constructor. A bare
                // `Value::Function` is boxed first so the identity is the same
                // object the callee would see through `F`.
                let new_target_value = match &constructor_val {
                    Value::Function(id) => self.materialize_function(*id),
                    other => other.clone(),
                };

                // 1. Determine the function ID and prototype
                let (func_id, prototype) = match constructor_val {
                    // 生成器不是构造器 —— 判定与别处走同一条路径（`callee_kind`）。
                    Value::Function(id)
                        if self.callee_kind(&constructor_val, module)
                            == CalleeKind::Generator =>
                    {
                        return Err(RuntimeError::TypeError(format!(
                            "{} is not a constructor",
                            self.current_module_info
                                .as_ref()
                                .and_then(|info| info.get(&id))
                                .map(|(name, _)| name.as_str())
                                .unwrap_or("Generator")
                        )));
                    }
                    Value::Function(id) => {
                        // Box the bare function reference into its (memoized) object
                        // so `F.prototype` identity is stable across `new` calls.
                        let func_obj = self.materialize_function(id);
                        if let Value::Object(func_ref) = &func_obj {
                            let proto = func_ref
                                .borrow()
                                .property_get(&PropertyKey::from_str("prototype"));
                            // Update the constructor to point to the FunctionObject for future accesses
                            self.set_value(callee, func_obj.clone())?;
                            (id, proto.map(|d| d.value))
                        } else {
                            (id, None)
                        }
                    }

                    Value::Object(obj_ref) => {
                        let borrowed = obj_ref.borrow();
                        if borrowed.kind() == ObjectKind::Proxy {
                            // `new P(...)` on a proxy: the `construct` trap (or,
                            // without one, `[[Construct]]` on the target). Both
                            // can run JavaScript, so neither can happen here.
                            drop(borrowed);
                            let ctor = Value::Object(Rc::clone(&obj_ref));
                            let args = self.collect_call_args(arg_count)?;
                            let rv = self.construct(&ctor, &args, module)?;
                            self.state.set_register(Register::Rv, rv)?;
                            self.state.jump_offset(1);
                            return Ok(());
                        }
                        if borrowed.kind() == ObjectKind::Function {
                            if let Some(func_obj) =
                                borrowed.as_any().downcast_ref::<FunctionObject>()
                            {
                                let id = func_obj.func_id;
                                let proto =
                                    borrowed.property_get(&PropertyKey::from_str("prototype"));
                                drop(borrowed);
                                (id, proto.map(|d| d.value))
                            } else {
                                return Err(RuntimeError::TypeError(
                                    "not a constructor".to_string(),
                                ));
                            }
                        } else if borrowed.kind() == ObjectKind::NativeFunction {
                            // Built-in constructor (`new Array(…)`, `new Object()`,
                            // `new Error(…)`, `new (f.bind(…))(…)`, …).
                            let name = borrowed
                                .as_any()
                                .downcast_ref::<NativeFunctionObject>()
                                .map(|f| f.name.clone())
                                .unwrap_or_default();
                            drop(borrowed);

                            let ctor = Value::Object(Rc::clone(&obj_ref));
                            let args = self.collect_call_args(arg_count)?;
                            let rv = self.native_construct(&name, &ctor, &args, None, module)?;
                            self.state.set_register(Register::Rv, rv)?;
                            self.state.jump_offset(1);
                            return Ok(());
                        } else {
                            return Err(RuntimeError::TypeError("not a constructor".to_string()));
                        }
                    }
                    _ => {
                        return Err(RuntimeError::TypeError("not a constructor".to_string()));
                    }
                };

                // 2. Create new ordinary object
                let new_obj_val = Value::Object(Rc::new(RefCell::new(
                    crate::vm::object::OrdinaryObject::new(),
                )));

                // 3. Set prototype
                if let Some(Value::Object(proto_obj)) = prototype {
                    if let Value::Object(obj_ref) = &new_obj_val {
                        obj_ref
                            .borrow_mut()
                            .set_prototype(Some(Rc::clone(&proto_obj)));
                    }
                } else {
                    // Default prototype: Object.prototype
                    if let Value::Object(obj_ref) = &new_obj_val {
                        obj_ref
                            .borrow_mut()
                            .set_prototype(Some(Rc::clone(&self.builtins.object_prototype)));
                    }
                }

                // 4. Set this_val to the new object
                self.state.this_val = new_obj_val;
                // The running function inside the constructor is the constructor.
                self.state.function_val = new_target_value.clone();

                // 5. Save closure depth, SEH depth and return PC, then jump to constructor
                match module.symtab.get(&FunctionId::new(func_id)) {
                    Some(location) => {
                        self.state.enter_frame(arg_count)?;
                        if module.derived_ctors.contains(&func_id) {
                            self.mark_this_uninitialized();
                        }
                        // Reset Rv before invoking the constructor so that a constructor
                        // with no explicit `return` (Rv stays undefined) yields `this`
                        // via the [[Construct]] logic in `Ret`. This also avoids leaking a
                        // stale Rv value from a previous call.
                        self.state.set_register(Register::Rv, Value::Undefined)?;
                        self.state.pushc(self.state.closure_var_stack.len())?;
                        self.state.pushc(self.state.seh_stack.len())?;
                        self.state.pushc(self.state.pc + 1)?;
                        self.state.construct_stack.push(true);
                        self.state.new_target_stack.push(new_target_value);
                        self.state.jump(*location);
                        return Ok(());
                    }
                    None => {
                        return Err(RuntimeError::ReferenceError(format!(
                            "undefined function: {func_id}"
                        )));
                    }
                }
            }
            Instr::LoadThis { dst } => {
                if self.state.this_state.last() == Some(&THIS_DERIVED_UNBOUND) {
                    return Err(self.this_not_initialized());
                }
                let value = self.state.this_val.clone();
                self.set_value(dst, value)?;
            }
            Instr::LoadNewTarget { dst } => {
                // `new.target` is per frame: the constructor for a `[[Construct]]`
                // frame, otherwise undefined (an arrow frame carries the value
                // captured when the arrow object was created).
                let value = self
                    .state
                    .new_target_stack
                    .last()
                    .cloned()
                    .unwrap_or(Value::Undefined);
                self.set_value(dst, value)?;
            }
            Instr::LoadCurrentFunction { dst } => {
                let value = self.state.function_val.clone();
                self.set_value(dst, value)?;
            }
            Instr::MakeFuncObj { dst, func } => {
                let func_id = match func {
                    Operand::Symbol(sym) => sym,
                    Operand::Immd(val) => val as u32,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "invalid MakeFuncObj operand".to_string(),
                        ));
                    }
                };
                // A `function` expression captures its enclosing scope exactly
                // like an arrow does (the lowering pushes one `ClosureVar` per
                // free variable). Without this the inner function's reads fell
                // through to the environment and answered `undefined`, which is
                // what made every `counter()`-style closure test report `1,1,1`.
                let captured_vars = self.take_pending_captured_vars();
                let obj_val = if captured_vars.is_empty() {
                    self.materialize_function(func_id)
                } else {
                    // Its *own* object: `materialize_function` memoizes by
                    // `func_id`, so two evaluations of one expression inside a
                    // loop would otherwise share (and overwrite) one capture set.
                    self.make_capturing_function_object(func_id, captured_vars)
                };
                self.set_value(dst, obj_val)?;
            }
            Instr::MakeArrowFuncObj { dst, func, captured_this } => {
                let func_id = match func {
                    Operand::Symbol(sym) => sym,
                    Operand::Immd(val) => val as u32,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "invalid MakeArrowFuncObj func_id operand".to_string(),
                        ));
                    }
                };
                let captured_this = match self.get_value(captured_this)? {
                    // Lowering leaves the operand empty (see `lower_arrow_function`);
                    // the arrow captures whatever the frame's `this` is.
                    v if v.is_undefined() => self.state.this_val.clone(),
                    v => v,
                };
                let captured_vars = self.take_pending_captured_vars();
                // An arrow has no `[[Construct]]`: it inherits `new.target` from
                // the frame that created it, so capture that value here (the
                // same create-time snapshot rule used for `this`).
                let captured_new_target = self
                    .state
                    .new_target_stack
                    .last()
                    .cloned()
                    .unwrap_or(Value::Undefined);
                // `fn.length` / `fn.name` come from the compiler's per-function
                // metadata, exactly as for ordinary functions.
                let (arrow_name, arrow_arity) = self
                    .current_module_info
                    .as_ref()
                    .and_then(|info| info.get(&func_id))
                    .cloned()
                    .unwrap_or((String::new(), 0));
                let obj_val = crate::vm::object::new_arrow_function_object(
                    func_id,
                    &arrow_name,
                    arrow_arity,
                    captured_this,
                    captured_new_target,
                    captured_vars,
                );
                // Arrows inherit `super` (and the home object) from the enclosing
                // method: copy the `super`-related properties recorded on the
                // running function so `super()` / `super.x` work inside an arrow
                // nested in a constructor or method (ES 14.2.16).
                let inherited_super: Vec<(PropertyKey, crate::vm::PropertyDescriptor)> =
                    match &self.state.function_val {
                        Value::Object(func_ref) => {
                            let borrowed = func_ref.borrow();
                            ["__super__", "__superProto__"]
                                .iter()
                                .filter_map(|name| {
                                    let key = PropertyKey::from_str(name);
                                    borrowed
                                        .property_get(&key)
                                        .map(|desc| (key, desc))
                                })
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                if let Value::Object(arrow_ref) = &obj_val {
                    for (key, desc) in inherited_super {
                        let _ = arrow_ref.borrow_mut().define_property(key, desc);
                    }
                }
                self.set_value(dst, obj_val)?;
            }
            Instr::ClosureVar { name, value } => {
                let name_index = name.as_immd() as usize;
                let name = match &module.constants[name_index] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(value)?;
                // Push as a single-entry map onto closure_var_stack
                let mut map = HashMap::new();
                map.insert(name, value);
                self.state.closure_var_stack.push(map);
            }
            Instr::CallNative { callee, argc } => {
                let callable = self.get_value(callee)?;
                let arg_count = argc.as_immd() as usize;
                // Same ordering rule as `CallEx`: read operands first, then
                // switch to the frame that holds the outgoing arguments.
                self.state.rbp = self.state.rsp;

                if let Some(name) = crate::builtins::native_function_name(&callable) {
                    // Arguments are on the stack below the frame: arg0 at [rbp-1], arg1 at [rbp-2], etc.
                    let mut args = Vec::with_capacity(arg_count);
                    for i in 0..arg_count {
                        let index = self.state.rbp - i - 1;
                        args.push(self.state.raw_stack_value(index));
                    }

                    // `String(value)` is ToString(value): objects convert
                    // through ToPrimitive (hint "string"), which can call user
                    // code, so it must run in the VM rather than as a native.
                    let result = if name == "String" && args.len() == 1 {
                        let primitive = self.to_primitive(&args[0], "string", module)?;
                        Ok(Value::string(&primitive.to_js_string()))
                    } else if let Some(value) = self.reflect_static(&name, &args, module)? {
                        Ok(value)
                    } else {
                        crate::builtins::call_native(&name, &args)
                    };
                    match result {
                        Ok(result) => {
                            self.state.set_register(Register::Rv, result)?;
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                } else {
                    return Err(RuntimeError::TypeError(
                        "not a built-in function".to_string(),
                    ));
                }
            }

            Instr::Halt {  }
              | Instr::Ret {  } => {
                // These are handled in run(), not here
                unreachable!("Halt/Ret should be handled in run()")
            }
            Instr::ResumeExc {  } => {
                let action = if let Some(record) = self.state.seh_stack.last() {
                    if record.in_finally {
                        if record.pending_exception.is_some() {
                            Some((
                                record.handler_pc,
                                record.finally_pc,
                                record.pending_exception.clone(),
                                record.catch_executed,
                                record.delayed_jump_target,
                                record.delayed_return,
                            ))
                        } else if record.delayed_jump_target.is_some() {
                            Some((
                                record.handler_pc,
                                record.finally_pc,
                                None,
                                record.catch_executed,
                                record.delayed_jump_target,
                                record.delayed_return,
                            ))
                        } else if record.delayed_return {
                            Some((
                                record.handler_pc,
                                record.finally_pc,
                                None,
                                record.catch_executed,
                                record.delayed_jump_target,
                                record.delayed_return,
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some((
                    handler_pc,
                    finally_pc,
                    pending_exc,
                    catch_executed,
                    delayed_jump,
                    delayed_return,
                )) = action
                {
                    if let Some(exc) = pending_exc {
                        let has_real_catch = handler_pc != 0 && handler_pc != finally_pc;
                        if has_real_catch && !catch_executed {
                            if let Some(record) = self.state.seh_stack.last_mut() {
                                record.in_finally = false;
                                record.pending_exception = None;
                                record.catch_executed = true;
                            }
                            self.state.set_register(Register::Rv, exc)?;
                            self.state.jump(handler_pc);
                            return Ok(());
                        } else {
                            self.state.seh_stack.pop();
                            return self.handle_throw(exc);
                        }
                    } else if let Some(target_pc) = delayed_jump {
                        // Delayed jump after finally execution. `target_pc` is an
                        // absolute address (see Codegen for DelayedJump).
                        self.state.seh_stack.pop();
                        self.state.jump(target_pc.max(0) as usize);
                        return Ok(());
                    } else if delayed_return {
                        // Delayed return after finally execution: jump back to
                        // the function's trailing `Ret` (the return value is
                        // already in `Rv`).
                        //
                        // Falling through is wrong: the instruction after
                        // `ResumeExc` is the *normal* completion's continuation
                        // (`Br` to the code following the `try`), so a `return`
                        // inside `try`/`finally` would run the statements it was
                        // skipping and then hit the exit block's implicit
                        // `Mov rv, undefined`, losing the value — which is what
                        // broke `return_in_try_finally` and friends.
                        self.state.seh_stack.pop();
                        if let Some(pc) = self
                            .current_function_id()
                            .and_then(|id| module.exit_pc.get(&id).copied())
                        {
                            self.state.jump(pc);
                        }
                        return Ok(());
                    }
                } else if let Some(record) = self.state.seh_stack.last_mut() {
                    if record.in_finally {
                        record.in_finally = false;
                    }
                }
            }
        }

        self.state.jump_offset(1);
        Ok(())
    }

    /// Snapshot of an array-like receiver's elements (arrays and strings).
    fn array_like_elements(&self, val: &Value) -> Option<Vec<Value>> {
        match val {
            Value::Object(obj_ref) => {
                if let Some(arr) = obj_ref.borrow().as_any().downcast_ref::<ArrayObject>() {
                    return Some(
                        (0..arr.len())
                            .map(|i| arr.get(i).cloned().unwrap_or(Value::Undefined))
                            .collect(),
                    );
                }
                // A TypedArray view: its `length` is a *prototype getter*, so
                // reading it as an own property (what the array-like path below
                // does) answers `undefined`, and iterating a view yielded
                // nothing at all — `[...new Uint8Array([1,2,3])]` was `[]`.
                if let Some(view) = obj_ref
                    .borrow()
                    .as_any()
                    .downcast_ref::<crate::vm::object::TypedArrayObject>()
                {
                    return Some((0..view.length).map(|i| view.get_element(i)).collect());
                }
                // Generic array-like: `length` plus integer-keyed properties.
                // `Array.prototype.every.call({length:2, 0:'a', 1:'b'}, …)` and
                // friends rely on this; it is the spec's ListFromLength path.
                let borrowed = obj_ref.borrow();
                let len_desc = borrowed.property_get(&PropertyKey::from_str("length"))?;
                let len = len_desc.value.to_number();
                if !len.is_finite() || len < 0.0 {
                    return None;
                }
                let len = len.min((1u64 << 24) as f64) as usize;
                Some(
                    (0..len)
                        .map(|i| {
                            borrowed
                                .property_get(&PropertyKey::from_str(&i.to_string()))
                                .map(|d| d.value)
                                .unwrap_or(Value::Undefined)
                        })
                        .collect(),
                )
            }
            Value::String(s) => Some(s.chars().map(|c| Value::string(&c.to_string())).collect()),
            _ => None,
        }
    }

    /// Element list of an array-like receiver, hole-aware.
    ///
    /// `None` marks an index with no property: the ES callback methods skip
    /// holes rather than visiting `undefined`. Three details make the generic
    /// (`Array.prototype.filter.call(x, …)`) path work:
    ///
    /// * primitives are boxed first, so `filter.call(false, cb)` reads
    ///   `length`/`0` off `Boolean.prototype`;
    /// * `length` and the index keys are read through the prototype chain
    ///   (`[[Get]]`) — inherited array-like shapes are the norm in test262;
    /// * `length` goes through ToLength (clamped, non-negative).
    fn array_like_entries(
        &mut self,
        receiver: &Value,
        module: &Module,
    ) -> Result<Vec<Option<Value>>, RuntimeError> {
        // ToObject: a primitive receiver is boxed so its wrapper's prototype
        // supplies `length` and the index properties.
        let boxed = match receiver {
            Value::Object(_) | Value::Function(_) => None,
            _ => Some(crate::builtins::to_object(receiver)?),
        };
        let target = boxed.as_ref().unwrap_or(receiver);

        let Value::Object(obj_ref) = target else {
            return Ok(Vec::new());
        };
        let obj = Rc::clone(obj_ref);
        // The full `[[Get]]`: `length` may live on the prototype chain *and* be
        // an accessor (`Array.prototype.indexOf.call(instance)` where the
        // constructor's prototype defines `get length()`). Reading it with
        // `internal_get` skipped the accessor, so those receivers looked
        // zero-length and every element was skipped.
        let len_value = self.get_member(target, &PropertyKey::from_str("length"), module)?;
        // ToLength: clamp negatives to 0 and cap the allocation, which keeps a
        // bogus `length` from trying to materialize billions of slots.
        let len = len_value.to_number();
        if !len.is_finite() || len <= 0.0 {
            return Ok(Vec::new());
        }
        let len = (len.trunc() as u64).min(1u64 << 24) as usize;

        let mut entries: Vec<Option<Value>> = Vec::with_capacity(len);
        for i in 0..len {
            let key = PropertyKey::from_str(&i.to_string());
            let present = crate::vm::prototype::internal_has_property(Rc::clone(&obj), &key)
                .map_err(RuntimeError::TypeError)?;
            if present {
                // Through `[[Get]]`, so an accessor element is *invoked*.
                entries.push(Some(self.get_member(target, &key, module)?));
            } else {
                entries.push(None);
            }
        }
        Ok(entries)
    }

    /// The half of `Reflect` that runs user code: `get` / `set` run accessors,
    /// `apply` / `construct` call functions, and `defineProperty`'s attributes
    /// may be accessors. `Reflect.ownKeys` and the rest are pure data and live in
    /// the builtin layer.
    ///
    /// Two arguments are accepted but not honoured yet, both of them rare in
    /// daily code: `Reflect.get`'s `receiver` (the VM's `get_member` always
    /// passes the target) and `Reflect.construct`'s `newTarget`. `Reflect.set`
    /// answers `true` whenever the write is not refused outright — `set_member`
    /// does not report whether it landed.
    fn reflect_static(
        &mut self,
        name: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        use crate::builtins::reflect::{target_of, to_key};
        match name {
            "Reflect.get" => {
                target_of(args, "get")?;
                let target = args[0].clone();
                let key = to_key(args.get(1).unwrap_or(&Value::Undefined));
                // The third argument is the *receiver*: the `this` an accessor
                // is invoked with (ES 28.1.9 / 10.1.8). Absent, it is the target.
                let receiver = if args.len() > 2 {
                    args[2].clone()
                } else {
                    target.clone()
                };
                Ok(Some(
                    self.get_with_receiver(&target, &key, &receiver, module)?,
                ))
            }
            "Reflect.set" => {
                target_of(args, "set")?;
                let target = args[0].clone();
                let key = to_key(args.get(1).unwrap_or(&Value::Undefined));
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                // Same receiver rule, and `[[Set]]` *answers* instead of
                // throwing: a non-writable property, a setter-less accessor or
                // a non-object receiver are all `false` (ES 10.1.9).
                let receiver = if args.len() > 3 {
                    args[3].clone()
                } else {
                    target.clone()
                };
                let ok = self.set_with_receiver(&target, &key, &value, &receiver, module)?;
                Ok(Some(Value::Bool(ok)))
            }
            "Reflect.apply" => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !target.is_callable() {
                    return Err(RuntimeError::TypeError(
                        "Reflect.apply target is not a function".to_string(),
                    ));
                }
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                let list = self.reflect_argument_list(args.get(2), "apply", module)?;
                Ok(Some(self.invoke(&target, this_arg, &list, module)?))
            }
            "Reflect.construct" => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                if !target.is_callable() {
                    return Err(RuntimeError::TypeError(
                        "Reflect.construct target is not a constructor".to_string(),
                    ));
                }
                let list = self.reflect_argument_list(args.get(1), "construct", module)?;
                // `newTarget`, when given, has to be a constructor (ES 28.1.2
                // step 6) — and it decides both the instance's prototype and the
                // callee's `new.target`.
                match args.get(2) {
                    Some(new_target) if !self.is_constructor(new_target, module) => {
                        Err(RuntimeError::TypeError(format!(
                            "Reflect.construct: {} is not a constructor",
                            new_target.to_js_string()
                        )))
                    }
                    new_target => Ok(Some(self.construct_with_new_target(
                        &target,
                        &list,
                        new_target,
                        module,
                    )?)),
                }
            }
            "Reflect.defineProperty" => {
                target_of(args, "defineProperty")?;
                let attributes = args.get(2).cloned().unwrap_or(Value::Undefined);
                if !matches!(attributes, Value::Object(_)) {
                    return Err(RuntimeError::TypeError(
                        "Reflect.defineProperty: attributes must be an object".to_string(),
                    ));
                }
                // The attributes have to be *normalised* first (which fields are
                // present decides whether this is a data or accessor descriptor),
                // and that is the VM's `to_property_descriptor`.
                let descriptor = self.to_property_descriptor(&attributes, module)?;
                // A refused definition answers `false` instead of throwing, which
                // is the point of the `Reflect` form (ES 28.1.4) — so the throw
                // `Object.defineProperty` raises on refusal becomes `false` here.
                let defined = crate::builtins::call_static_method(
                    "Object.defineProperty",
                    &[args[0].clone(), args[1].clone(), descriptor],
                );
                Ok(Some(Value::Bool(defined.is_ok())))
            }
            _ => Ok(None),
        }
    }

    /// `args` with the key argument (index 1) put through `ToPropertyKey`.
    ///
    /// `Reflect.get(t, {toString(){throw x}})` has to throw `x`, and only the
    /// VM can run the conversion (ES 28.1.6 step 2).
    fn reflect_key_args(
        &mut self,
        args: &[Value],
        module: &Module,
    ) -> Result<Vec<Value>, RuntimeError> {
        let mut converted = args.to_vec();
        let key = match args.get(1) {
            Some(value) => self.to_property_key_value(value, module)?,
            None => Value::string("undefined"),
        };
        if converted.len() > 1 {
            converted[1] = key;
        } else {
            converted.push(key);
        }
        Ok(converted)
    }

    /// `ToPropertyKey` answering the key as a **value** (a string or a symbol),
    /// for the builtins that take a key argument and store it as one.
    fn to_property_key_value(
        &mut self,
        value: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        match value {
            Value::Symbol(_) | Value::String(_) => Ok(value.clone()),
            other => {
                let primitive = self.to_primitive(other, "string", module)?;
                match &primitive {
                    Value::Symbol(_) => Ok(primitive),
                    other => Ok(Value::string(&other.to_js_string())),
                }
            }
        }
    }

    /// `CreateListFromArrayLike` for `Reflect.apply` / `Reflect.construct`: the
    /// argument list has to be an object with a usable `length`.
    ///
    /// `length` and every index are read through real `[[Get]]`, so an accessor
    /// that throws is observed (`Reflect.apply(fn, null, { get length() { throw } })`).
    fn reflect_argument_list(
        &mut self,
        value: Option<&Value>,
        method: &str,
        module: &Module,
    ) -> Result<Vec<Value>, RuntimeError> {
        let Some(value) = value else {
            return Err(RuntimeError::TypeError(format!(
                "Reflect.{method}: arguments list is required"
            )));
        };
        match value {
            // `CreateListFromArrayLike` insists on an object — unlike
            // `Function.prototype.apply`, `Reflect.apply(f, null, null)` throws.
            Value::Object(_) => {
                let length = self.get_member(value, &PropertyKey::from_str("length"), module)?;
                let len = crate::builtins::to_length_throwing(&length)?;
                let mut items = Vec::with_capacity(len as usize);
                for index in 0..len {
                    items.push(
                        self.get_member(
                            value,
                            &PropertyKey::from_str(&index.to_string()),
                            module,
                        )?,
                    );
                }
                Ok(items)
            }
            _ => Err(RuntimeError::TypeError(format!(
                "Reflect.{method}: arguments list must be an array-like object"
            ))),
        }
    }

    // ─────────────────────────────────────────────────────
    // Proxy (ES 28.2 / 10.5) — trap dispatch
    // ─────────────────────────────────────────────────────
    //
    // The rule every method here leans on is ES 10.5's: an operation whose trap
    // the handler does not have is *not* an error, it is the same operation on
    // the target. So each `proxy_*` answers an `Option`: `None` means "forward",
    // and the caller then does exactly what it would have done for an ordinary
    // object — which works because `ProxyObject`'s object-layer methods already
    // forward to the target.
    //
    // The traps that are *not* here (`ownKeys`, `getOwnPropertyDescriptor`,
    // `defineProperty`, `getPrototypeOf`, `setPrototypeOf`, `isExtensible`,
    // `preventExtensions`) are therefore "forward" today: a handler that sets
    // them is not called. See the batch record in the plan for why.

    /// Whether `value` is a proxy exotic object.
    fn is_proxy(value: &Value) -> bool {
        match value {
            Value::Object(obj_ref) => obj_ref.borrow().kind() == ObjectKind::Proxy,
            _ => false,
        }
    }

    /// `[[ProxyTarget]]` and `[[ProxyHandler]]`.
    ///
    /// A revoked proxy throws on *every* operation, trap or not — that is the
    /// whole point of `revoke` (ES 28.2.2.2).
    fn proxy_parts(&self, proxy: &Value, what: &str) -> Result<(Value, Value), RuntimeError> {
        match proxy {
            Value::Object(obj_ref) => {
                let borrowed = obj_ref.borrow();
                match borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::ProxyObject>()
                {
                    Some(p) if !p.revoked => Ok((p.target.clone(), p.handler.clone())),
                    Some(_) => Err(RuntimeError::TypeError(format!(
                        "Cannot perform '{what}' on a proxy that has been revoked"
                    ))),
                    None => Err(RuntimeError::InternalError(
                        "not a proxy object".to_string(),
                    )),
                }
            }
            _ => Err(RuntimeError::InternalError(
                "not a proxy object".to_string(),
            )),
        }
    }

    /// The handler's trap `trap_name`, together with the target and the handler
    /// it must be called on. `None` = the handler has no such trap.
    fn proxy_trap(
        &mut self,
        proxy: &Value,
        trap_name: &str,
        module: &Module,
    ) -> Result<Option<(Value, Value, Value)>, RuntimeError> {
        let (target, handler) = self.proxy_parts(proxy, trap_name)?;
        // Reading the trap is an ordinary `[[Get]]` on the handler: a handler
        // with an accessor for `get` is legal, and only the VM can run it.
        let trap = match &handler {
            Value::Object(_) => {
                self.get_member(&handler, &PropertyKey::from_str(trap_name), module)?
            }
            _ => Value::Undefined,
        };
        // `GetMethod`: `null` and `undefined` both mean "no trap" — that is why
        // `{ get: null }` is a pass-through rather than an error.
        if trap.is_undefined() || matches!(trap, Value::Null) {
            return Ok(None);
        }
        if !trap.is_callable() {
            return Err(RuntimeError::TypeError(format!(
                "Proxy handler's '{trap_name}' trap is not a function"
            )));
        }
        Ok(Some((target, handler, trap)))
    }

    /// A `PropertyKey` as the value a trap is called with: the key is an
    /// ordinary argument, symbols included.
    fn key_to_value(key: &PropertyKey) -> Value {
        match key {
            PropertyKey::Str(text) => Value::string(text),
            PropertyKey::Symbol(id) => {
                Value::Symbol(Rc::new(crate::vm::value::SymbolData::new(None, *id)))
            }
        }
    }

    /// The target's *own* descriptor for `key`, for the invariant checks below.
    fn proxy_target_descriptor(target: &Value, key: &PropertyKey) -> Option<PropertyDescriptor> {
        match target {
            Value::Object(obj_ref) => obj_ref.borrow().property_get(key),
            _ => None,
        }
    }

    /// The target's own keys, for the `ownKeys` invariants.
    fn proxy_target_keys(target: &Value) -> Vec<PropertyKey> {
        match target {
            Value::Object(obj_ref) => obj_ref.borrow().own_keys(),
            _ => Vec::new(),
        }
    }

    /// `[[Get]]` through a proxy (ES 10.5.8).
    ///
    /// With no trap the operation is the *target's* `[[Get]]`, which is a
    /// recursive call rather than a fallthrough: the target may be a proxy too,
    /// and its own traps have to fire (`get/trap-is-null-target-is-proxy`).
    fn proxy_get(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        receiver: &Value,
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "get", module)? else {
            return Ok(Some(self.get_with_receiver(
                &self.proxy_parts(proxy, "get")?.0,
                key,
                receiver,
                module,
            )?));
        };
        let key_value = Self::key_to_value(key);
        let value = self.invoke(
            &trap,
            handler,
            &[target.clone(), key_value, receiver.clone()],
            module,
        )?;
        // Invariant (ES 10.5.8 step 17): a non-writable, non-configurable own
        // data property of the target cannot be reported as anything else —
        // otherwise `Object.freeze(t)` would be observable as unfrozen.
        if let Some(desc) = Self::proxy_target_descriptor(&target, key) {
            if desc.is_data_descriptor() && !desc.writable && !desc.configurable {
                if !value.strict_eq(&desc.value) {
                    return Err(RuntimeError::TypeError(format!(
                        "'get' on proxy: property '{}' is a read-only and non-configurable data property on the proxy target but the proxy did not return its actual value (expected '{}' but got '{}')",
                        key.display(),
                        desc.value.to_js_string(),
                        value.to_js_string()
                    )));
                }
            }
        }
        Ok(Some(value))
    }

    /// `[[Set]]` through a proxy (ES 10.5.9), answering whether the write
    /// succeeded.
    ///
    /// The two callers disagree about a falsish trap: an assignment turns it
    /// into a TypeError (the engine has no sloppy mode), while `Reflect.set`
    /// reports it as `false`. So this answers the boolean and lets them decide.
    fn proxy_set(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        value: &Value,
        receiver: &Value,
        module: &Module,
    ) -> Result<bool, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "set", module)? else {
            // No trap: `[[Set]]` on the target, receiver included — the target
            // may itself be a proxy, which is why this is a recursive dispatch.
            let target = self.proxy_parts(proxy, "set")?.0;
            return self.set_with_receiver(&target, key, value, receiver, module);
        };
        let answer = self.invoke(
            &trap,
            handler,
            &[
                target.clone(),
                Self::key_to_value(key),
                value.clone(),
                receiver.clone(),
            ],
            module,
        )?;
        // Invariant (step 21): a non-writable, non-configurable own data
        // property of the target cannot be reported as written.
        if let Some(desc) = Self::proxy_target_descriptor(&target, key) {
            if desc.is_data_descriptor() && !desc.writable && !desc.configurable {
                if !value.strict_eq(&desc.value) {
                    return Err(RuntimeError::TypeError(format!(
                        "'set' on proxy: trap returned truish for property '{}' which exists in the proxy target as a non-configurable and non-writable data property with a different value",
                        key.display()
                    )));
                }
            }
        }
        Ok(answer.to_boolean())
    }

    /// `[[HasProperty]]` through a proxy (ES 10.5.7).
    fn proxy_has(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Option<bool>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "has", module)? else {
            // No trap: `[[HasProperty]]` on the target, which may be a proxy.
            let target = self.proxy_parts(proxy, "has")?.0;
            if Self::is_proxy(&target) {
                return self.proxy_has(&target, key, module);
            }
            return match &target {
                Value::Object(obj_ref) => Ok(Some(
                    crate::vm::prototype::internal_has_property(Rc::clone(obj_ref), key)
                        .map_err(RuntimeError::TypeError)?,
                )),
                _ => Ok(Some(false)),
            };
        };
        let answer = self
            .invoke(
                &trap,
                handler,
                &[target.clone(), Self::key_to_value(key)],
                module,
            )?
            .to_boolean();
        // Invariants (ES 10.5.7): an existing non-configurable property, or any
        // own property of a non-extensible target, cannot be reported absent.
        if !answer {
            let extensible = match &target {
                Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                _ => true,
            };
            let desc = Self::proxy_target_descriptor(&target, key);
            let must_report = match &desc {
                Some(desc) => !desc.configurable || !extensible,
                None => false,
            };
            if must_report {
                return Err(RuntimeError::TypeError(format!(
                    "'has' on proxy: trap returned falsish for property '{}' which exists in the proxy target as non-configurable",
                    key.display()
                )));
            }
        }
        Ok(Some(answer))
    }

    /// `[[Delete]]` through a proxy (ES 10.5.10).
    fn proxy_delete(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Option<bool>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "deleteProperty", module)?
        else {
            // No trap: `[[Delete]]` on the target, which may be a proxy.
            let target = self.proxy_parts(proxy, "deleteProperty")?.0;
            if Self::is_proxy(&target) {
                return self.proxy_delete(&target, key, module);
            }
            return match &target {
                Value::Object(obj_ref) => Ok(Some(
                    crate::vm::prototype::internal_delete(Rc::clone(obj_ref), key)
                        .map_err(RuntimeError::TypeError)?,
                )),
                _ => Ok(Some(true)),
            };
        };
        let answer = self
            .invoke(
                &trap,
                handler,
                &[target.clone(), Self::key_to_value(key)],
                module,
            )?
            .to_boolean();
        // Invariant (ES 10.5.10): a non-configurable own property cannot be
        // reported as deleted. (A *frozen* target's property is covered too:
        // frozen implies non-configurable.)
        if answer {
            if let Some(desc) = Self::proxy_target_descriptor(&target, key) {
                if !desc.configurable {
                    return Err(RuntimeError::TypeError(format!(
                        "'deleteProperty' on proxy: trap returned truish for property '{}' which is non-configurable in the proxy target",
                        key.display()
                    )));
                }
            }
        }
        Ok(Some(answer))
    }

    /// `[[Call]]` through a proxy (ES 10.5.12). `None` = no `apply` trap, which
    /// forwards to calling the *target* with the same `this` and arguments.
    fn proxy_apply(
        &mut self,
        proxy: &Value,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "apply", module)? else {
            return Ok(None);
        };
        let list = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::ArrayObject::from_vec(args.to_vec()),
        )));
        Ok(Some(self.invoke(
            &trap,
            handler,
            &[target, this.clone(), list],
            module,
        )?))
    }

    /// `[[Construct]]` through a proxy (ES 10.5.13). `None` = no `construct`
    /// trap, which forwards to constructing the *target*.
    fn proxy_construct_trap(
        &mut self,
        proxy: &Value,
        args: &[Value],
        new_target: &Value,
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "construct", module)? else {
            return Ok(None);
        };
        let list = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::ArrayObject::from_vec(args.to_vec()),
        )));
        let result = self.invoke(&trap, handler, &[target, list, new_target.clone()], module)?;
        // `[[Construct]]` has to answer an object: a trap returning a primitive
        // is a TypeError, not a coercion.
        if !result.is_object() {
            return Err(RuntimeError::TypeError(
                "'construct' on proxy: trap returned a non-object".to_string(),
            ));
        }
        Ok(Some(result))
    }

    /// The traps that hang off `Object.*` / `Reflect.*` instead of an operator:
    /// `[[GetPrototypeOf]]` … `[[DefineOwnProperty]]`. Every one of them takes
    /// the target first, so "the receiver is a proxy" is all it takes to
    /// recognise the call.
    ///
    /// `None` = the ordinary path answers — no such trap, or not a proxy — and
    /// for a proxy that path is the same operation on the target.
    ///
    /// The invariant checks here are the ones a *user* could otherwise break:
    /// without them a proxy could report a frozen target as extensible, or a
    /// non-configurable property as absent.
    fn proxy_static_trap(
        &mut self,
        name: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let trap_name = match name {
            "Object.getPrototypeOf" | "Reflect.getPrototypeOf" => "getPrototypeOf",
            // `Reflect.has` / `Reflect.deleteProperty` are the same two
            // operations the `in` and `delete` operators perform.
            "Reflect.has" => "has",
            "Reflect.deleteProperty" => "deleteProperty",
            "Object.setPrototypeOf" | "Reflect.setPrototypeOf" => "setPrototypeOf",
            "Object.isExtensible" | "Reflect.isExtensible" => "isExtensible",
            "Object.preventExtensions" | "Reflect.preventExtensions" => "preventExtensions",
            "Object.getOwnPropertyDescriptor" | "Reflect.getOwnPropertyDescriptor" => {
                "getOwnPropertyDescriptor"
            }
            // The four "list the keys" operations are one trap.
            "Object.keys"
            | "Object.getOwnPropertyNames"
            | "Object.getOwnPropertySymbols"
            | "Reflect.ownKeys" => "ownKeys",
            "Object.defineProperty" | "Reflect.defineProperty" => "defineProperty",
            _ => return Ok(None),
        };
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        if !Self::is_proxy(&receiver) {
            return Ok(None);
        }
        let (target, handler, trap) = match self.proxy_trap(&receiver, trap_name, module)? {
            Some(parts) => parts,
            None => {
                // No trap: the same operation on the target. It is a *recursive*
                // dispatch rather than a fallthrough to the builtin layer, so a
                // target that is itself a proxy still gets its own trap called.
                if !Self::is_proxy(&self.proxy_parts(&receiver, trap_name)?.0) {
                    return Ok(None);
                }
                let mut forwarded = args.to_vec();
                forwarded[0] = self.proxy_parts(&receiver, trap_name)?.0;
                return self.proxy_static_trap(name, &forwarded, module);
            }
        };
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        // `Reflect.*` answers a boolean where `Object.*` throws; that is the
        // only difference between the two spellings of the same operation.
        let reflects = name.starts_with("Reflect");
        match trap_name {
            "getPrototypeOf" => {
                let result = self.invoke(&trap, handler, &[target.clone()], module)?;
                if !result.is_object() && !matches!(result, Value::Null) {
                    return Err(RuntimeError::TypeError(
                        "'getPrototypeOf' on proxy: trap returned neither an object nor null"
                            .to_string(),
                    ));
                }
                // ES 10.5.17: a non-extensible target's prototype is whatever it
                // is — the trap may not report a different one.
                if let Value::Object(obj_ref) = &target {
                    if !obj_ref.borrow().is_extensible()
                        && !Self::same_prototype(&obj_ref.borrow().get_prototype(), &result)
                    {
                        return Err(RuntimeError::TypeError(
                            "'getPrototypeOf' on proxy: trap returned a different prototype for a non-extensible target"
                                .to_string(),
                        ));
                    }
                }
                Ok(Some(result))
            }
            "setPrototypeOf" => {
                let proto = key.clone();
                if !proto.is_object() && !matches!(proto, Value::Null) {
                    return Err(RuntimeError::TypeError(
                        "Object prototype may only be an Object or null".to_string(),
                    ));
                }
                let answer = self
                    .invoke(&trap, handler, &[target.clone(), proto.clone()], module)?
                    .to_boolean();
                if answer {
                    // ES 10.5.18: a non-extensible target's prototype cannot be
                    // replaced.
                    if let Value::Object(obj_ref) = &target {
                        if !obj_ref.borrow().is_extensible()
                            && !Self::same_prototype(&obj_ref.borrow().get_prototype(), &proto)
                        {
                            return Err(RuntimeError::TypeError(
                                "'setPrototypeOf' on proxy: trap returned truish for a non-extensible target whose prototype differs"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Some(Self::proxy_boolean_answer(
                    reflects,
                    answer,
                    receiver,
                    "setPrototypeOf",
                )?))
            }
            "has" => Ok(Some(Value::Bool(
                self.proxy_has(&receiver, &crate::builtins::reflect::to_key(&key), module)?
                    .unwrap_or(false),
            ))),
            "deleteProperty" => Ok(Some(Value::Bool(
                self.proxy_delete(&receiver, &crate::builtins::reflect::to_key(&key), module)?
                    .unwrap_or(true),
            ))),
            "isExtensible" => {
                let answer = self
                    .invoke(&trap, handler, &[target.clone()], module)?
                    .to_boolean();
                // ES 10.5.16: the trap must agree with the target, or
                // `Object.isExtensible` and `preventExtensions` disagree.
                let actual = match &target {
                    Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                    _ => false,
                };
                if answer != actual {
                    return Err(RuntimeError::TypeError(format!(
                        "'isExtensible' on proxy: trap result does not reflect extensibility of proxy target (which is '{}')",
                        actual
                    )));
                }
                Ok(Some(Value::Bool(answer)))
            }
            "preventExtensions" => {
                let answer = self
                    .invoke(&trap, handler, &[target.clone()], module)?
                    .to_boolean();
                // ES 10.5.19: reporting success while the target is still
                // extensible would make the two answers contradict each other.
                if answer {
                    if let Value::Object(obj_ref) = &target {
                        if obj_ref.borrow().is_extensible() {
                            return Err(RuntimeError::TypeError(
                                "'preventExtensions' on proxy: trap returned truish but the proxy target is extensible"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Some(Self::proxy_boolean_answer(
                    reflects,
                    answer,
                    receiver,
                    "preventExtensions",
                )?))
            }
            "getOwnPropertyDescriptor" => {
                let result = self.invoke(&trap, handler, &[target.clone(), key.clone()], module)?;
                let target_desc =
                    Self::proxy_target_descriptor(&target, &crate::builtins::reflect::to_key(&key));
                let extensible = match &target {
                    Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                    _ => false,
                };
                if !result.is_object() {
                    if !result.is_undefined() {
                        return Err(RuntimeError::TypeError(
                            "'getOwnPropertyDescriptor' on proxy: trap returned neither an object nor undefined"
                                .to_string(),
                        ));
                    }
                    // ES 10.5.5: an existing non-configurable property — or any
                    // own property of a non-extensible target — cannot be
                    // reported as absent.
                    if let Some(desc) = &target_desc {
                        if !desc.configurable || !extensible {
                            return Err(RuntimeError::TypeError(
                                "'getOwnPropertyDescriptor' on proxy: trap returned undefined for a property that is non-configurable in the proxy target"
                                    .to_string(),
                            ));
                        }
                    }
                    return Ok(Some(Value::Undefined));
                }
                if let Some(desc) = &target_desc {
                    if !desc.configurable {
                        let reported = self.get_member(
                            &result,
                            &PropertyKey::from_str("configurable"),
                            module,
                        )?;
                        if reported.to_boolean() {
                            return Err(RuntimeError::TypeError(
                                "'getOwnPropertyDescriptor' on proxy: trap reported a non-configurable property as configurable"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Some(result))
            }
            "ownKeys" => {
                let result = self.invoke(&trap, handler, &[target.clone()], module)?;
                if !result.is_object() {
                    return Err(RuntimeError::TypeError(
                        "'ownKeys' on proxy: trap result must be an object".to_string(),
                    ));
                }
                // `CreateListFromArrayLike` restricted to strings and symbols.
                let items = self.array_like_elements(&result).ok_or_else(|| {
                    RuntimeError::TypeError(
                        "'ownKeys' on proxy: trap result is not array-like".to_string(),
                    )
                })?;
                let mut keys: Vec<PropertyKey> = Vec::new();
                for item in items {
                    match item {
                        Value::String(text) => keys.push(PropertyKey::from_str(&text)),
                        Value::Symbol(sym) => keys.push(PropertyKey::Symbol(sym.id)),
                        _ => {
                            return Err(RuntimeError::TypeError(
                                "'ownKeys' on proxy: trap result contains a key that is neither a string nor a symbol"
                                    .to_string(),
                            ))
                        }
                    }
                }
                let mut seen: Vec<PropertyKey> = Vec::new();
                for key in &keys {
                    if seen.contains(key) {
                        return Err(RuntimeError::TypeError(
                            "'ownKeys' on proxy: trap result contains duplicate entries"
                                .to_string(),
                        ));
                    }
                    seen.push(key.clone());
                }
                let target_keys = Self::proxy_target_keys(&target);
                let extensible = match &target {
                    Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                    _ => false,
                };
                for target_key in &target_keys {
                    // ES 10.5.11: every non-configurable own key has to be
                    // reported, and a non-extensible target's keys all are.
                    let required = match Self::proxy_target_descriptor(&target, target_key) {
                        Some(desc) => !desc.configurable,
                        None => false,
                    } || !extensible;
                    if required && !keys.contains(target_key) {
                        return Err(RuntimeError::TypeError(format!(
                            "'ownKeys' on proxy: trap result did not include '{}'",
                            target_key.display()
                        )));
                    }
                }
                if !extensible {
                    // … and a non-extensible target cannot appear to gain keys.
                    for key in &keys {
                        if !target_keys.contains(key) {
                            return Err(RuntimeError::TypeError(
                                "'ownKeys' on proxy: trap result includes a key the non-extensible target does not have"
                                    .to_string(),
                            ));
                        }
                    }
                }
                // Each caller reports a different slice of the same list:
                // `Object.getOwnPropertyNames` strings, `…Symbols` symbols,
                // `Object.keys` the enumerable strings, `Reflect.ownKeys` all.
                let values: Vec<Value> = keys
                    .iter()
                    .filter(|key| match name {
                        "Object.keys" => {
                            key.as_str().is_some()
                                && Self::proxy_target_descriptor(&target, key)
                                    .map(|desc| desc.enumerable)
                                    .unwrap_or(false)
                        }
                        "Object.getOwnPropertyNames" => key.as_str().is_some(),
                        "Object.getOwnPropertySymbols" => key.as_str().is_none(),
                        _ => true,
                    })
                    .map(Self::key_to_value)
                    .collect();
                Ok(Some(Value::Object(Rc::new(RefCell::new(
                    crate::vm::object::ArrayObject::from_vec(values),
                )))))
            }
            _ => {
                // `defineProperty`: the descriptor is normalised *before* the
                // trap sees it (ES 10.5.6 step 5) — the trap gets an object
                // built from the descriptor, not the one the caller passed.
                let descriptor =
                    self.to_property_descriptor(args.get(2).unwrap_or(&Value::Undefined), module)?;
                let answer = self
                    .invoke(
                        &trap,
                        handler,
                        &[target.clone(), key.clone(), descriptor],
                        module,
                    )?
                    .to_boolean();
                if answer {
                    let target_desc = Self::proxy_target_descriptor(
                        &target,
                        &crate::builtins::reflect::to_key(&key),
                    );
                    let extensible = match &target {
                        Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                        _ => false,
                    };
                    // ES 10.5.6: a non-configurable property cannot be
                    // redefined, and a non-extensible target gains no property.
                    let forbidden = match &target_desc {
                        Some(desc) => !desc.configurable,
                        None => !extensible,
                    };
                    if forbidden {
                        return Err(RuntimeError::TypeError(
                            "'defineProperty' on proxy: trap returned truish for a non-configurable property in the proxy target"
                                .to_string(),
                        ));
                    }
                }
                Ok(Some(Self::proxy_boolean_answer(
                    reflects,
                    answer,
                    receiver,
                    "defineProperty",
                )?))
            }
        }
    }

    /// `Object.*` and `Reflect.*` disagree only about *failure*: `Reflect`
    /// answers `false`, `Object` throws.
    fn proxy_boolean_answer(
        reflects: bool,
        answer: bool,
        receiver: Value,
        trap_name: &str,
    ) -> Result<Value, RuntimeError> {
        if reflects || answer {
            return Ok(if reflects {
                Value::Bool(answer)
            } else {
                receiver
            });
        }
        Err(RuntimeError::TypeError(format!(
            "'{trap_name}' on proxy: trap returned falsish"
        )))
    }

    /// Whether a reported prototype is the one the target actually has.
    fn same_prototype(actual: &Option<Rc<RefCell<dyn JSObject>>>, reported: &Value) -> bool {
        match (actual, reported) {
            (None, Value::Null) => true,
            (Some(a), Value::Object(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// The `revoke` function of `Proxy.revocable` (ES 28.2.2.2): a native whose
    /// synthetic name carries the id of the proxy it revokes.
    fn make_revoke(&mut self, proxy: Value) -> Value {
        let id = self.next_revoke_id;
        self.next_revoke_id += 1;
        self.revocable_proxies.insert(id, proxy);
        let mut revoke =
            crate::vm::object::NativeFunctionObject::new(&format!("{REVOKE_PREFIX}{id}"));
        // A revocation function is *anonymous* (ES 28.2.2.1.1): its `name` is
        // the empty string, not the synthetic name the VM dispatches on.
        let _ = revoke.define_property(
            PropertyKey::from_str("name"),
            PropertyDescriptor {
                value: Value::string(""),
                writable: false,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
        Value::Object(Rc::new(RefCell::new(revoke)))
    }

    /// `String.prototype.matchAll(re)` / `RegExp.prototype[Symbol.matchAll](s)`.
    ///
    /// Both answer an iterator of match arrays, and only the VM can mint
    /// iterator objects (they live in the iterator registry, which is how
    /// `next()` finds its state) — hence a hook rather than a builtin.
    fn try_match_all_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if method != "matchAll" && method != "Symbol.matchAll" {
            return Ok(None);
        }
        crate::builtins::require_object_coercible(receiver)?;
        let matches = crate::builtins::match_all_matches(
            receiver,
            args,
            Some(Value::Object(Rc::clone(&self.builtins.regexp_prototype))),
        )?;
        let snapshot_val = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::ArrayObject::from_vec(matches),
        )));
        Ok(Some(self.make_iterator(snapshot_val, module)?))
    }

    /// Species-aware wrapper for `slice` / `splice` / `concat`.
    ///
    /// Returns `Ok(None)` when the method is not one of them, when the receiver
    /// is not an Array, or when there is no species override — the ordinary
    /// builtin dispatch then runs unchanged. Otherwise the builtin computes the
    /// plain result (its side effects on the receiver are exactly as before)
    /// and the elements are copied into the species-constructed target, where a
    /// blocked write is abrupt (`CreateDataPropertyOrThrow`).
    fn try_array_species_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if !matches!(method, "slice" | "splice" | "concat") {
            return Ok(None);
        }
        // `ArraySpeciesCreate(O, 0)`: all three start from an empty result and
        // grow it index by index.
        let Some(target) = self.array_species_create(receiver, 0, module)? else {
            return Ok(None);
        };
        let plain = crate::builtins::call_prototype_method(receiver, method, args)?;
        // Snapshot the present indices first: writing into `target` can run
        // user code (accessors, proxies once they land), and `plain` is a
        // RefCell that must not stay borrowed across that call.
        let elements: Vec<(usize, Value)> = match &plain {
            Value::Object(result_ref) => {
                let borrowed = result_ref.borrow();
                let len = borrowed
                    .property_get(&PropertyKey::from_str("length"))
                    .map(|d| d.value.to_number() as usize)
                    .unwrap_or(0);
                (0..len)
                    .filter_map(|i| {
                        borrowed
                            .property_get(&PropertyKey::from_str(&i.to_string()))
                            .map(|d| (i, d.value))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        for (i, value) in elements {
            self.set_member(&target, PropertyKey::from_str(&i.to_string()), value, module)?;
        }
        Ok(Some(target))
    }

    /// Mark the frame that is about to run as having an unbound `this`.
    ///
    /// Must be called right after `enter_frame`, before the frame's body starts,
    /// and only for derived-class constructors.
    fn mark_this_uninitialized(&mut self) {
        let state = &mut self.state;
        while state.this_state.len() < state.this_stack.len() {
            state.this_state.push(THIS_NONE);
        }
        if let Some(slot) = state.this_state.last_mut() {
            *slot = THIS_DERIVED_UNBOUND;
        }
    }

    /// The ReferenceError ES raises when a derived constructor touches `this`
    /// before `super()` (ES 12.3.5.1 BindThisValue / 9.2.2).
    fn this_not_initialized(&self) -> RuntimeError {
        RuntimeError::ReferenceError(
            "Must call super constructor in derived class before accessing 'this' or returning from derived constructor".to_string(),
        )
    }

    /// ES 9.4.2.3 `ArraySpeciesCreate(O, len)`.
    ///
    /// Only the VM can run this one: both `Get`s go through `[[Get]]` (a
    /// poisoned `@@species` getter must be observable and abrupt — see
    /// `create-species-poisoned.js`) and `Construct` may re-enter user code.
    ///
    /// Returns `Ok(None)` for "build an ordinary array of `len`", which is what
    /// the spec does for a non-array receiver, an absent `constructor`, and a
    /// nullish `@@species`. Everything else is the object the species
    /// constructor produced (or a TypeError when it is not constructible).
    fn array_species_create(
        &mut self,
        receiver: &Value,
        len: usize,
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        // Step 2-3: a receiver that is not an Array never consults species.
        let is_array = match receiver {
            Value::Object(obj_ref) => {
                obj_ref.borrow().kind() == crate::vm::ObjectKind::Array
            }
            _ => false,
        };
        if !is_array {
            return Ok(None);
        }

        // Step 5: `? Get(O, "constructor")` — abrupt completions propagate.
        let mut species =
            self.get_member(receiver, &PropertyKey::from_str("constructor"), module)?;
        // Step 7: only an *object* `constructor` is asked for `@@species`, and
        // a nullish result falls back to an ordinary array.
        if species.is_object() {
            species = self.get_member(&species, &crate::builtins::species_symbol_key(), module)?;
            if species.is_null() {
                return Ok(None);
            }
        }
        if species.is_undefined() {
            return Ok(None);
        }
        // `Array[Symbol.species]` returns its receiver, so the common case
        // resolves back to `Array`: build it directly instead of routing every
        // `slice` through Construct.
        if crate::builtins::native_function_name(&species).as_deref() == Some("Array") {
            return Ok(None);
        }
        // Step 10: `? Construct(C, «len»)` — a non-constructible species is the
        // TypeError `create-ctor-non-object.js` expects.
        let target = self.construct(&species, &[Value::Number(len as f64)], module)?;
        Ok(Some(target))
    }

    /// ES `Array.prototype.indexOf` / `lastIndexOf` over the hole-aware element
    /// list, so generic array-likes (`indexOf.call({length: 2, 0: 'a'}, 'a')`)
    /// and primitives work the same way as real arrays.
    fn array_index_of(
        &self,
        entries: &[Option<Value>],
        method: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        /// SameValueZero: like `===`, but `NaN` matches itself.
        fn same_value_zero(a: &Value, b: &Value) -> bool {
            if let (Value::Number(x), Value::Number(y)) = (a, b) {
                if x.is_nan() && y.is_nan() {
                    return true;
                }
            }
            a.strict_eq(b)
        }

        let search = args.first().cloned().unwrap_or(Value::Undefined);
        let len = entries.len() as i64;
        // `? ToIntegerOrInfinity(fromIndex)`: ToNumber(Symbol) is abrupt, while
        // an absent argument reads as ToIntegerOf(undefined) = 0.
        let from = match args.get(1) {
            Some(v) if !v.is_undefined() => {
                crate::builtins::to_integer_or_infinity_throwing(Some(v))?
            }
            _ => {
                if method == "indexOf" {
                    0
                } else {
                    len - 1
                }
            }
        };

        if method == "indexOf" {
            let start = from.max(0);
            if start >= len {
                return Ok(Value::Number(-1.0));
            }
            for i in start..len {
                if let Some(element) = &entries[i as usize] {
                    if same_value_zero(element, &search) {
                        return Ok(Value::Number(i as f64));
                    }
                }
            }
        } else {
            let mut i = if from < 0 { -1 } else { from.min(len - 1) };
            while i >= 0 {
                if let Some(element) = &entries[i as usize] {
                    if same_value_zero(element, &search) {
                        return Ok(Value::Number(i as f64));
                    }
                }
                i -= 1;
            }
        }
        Ok(Value::Number(-1.0))
    }

    /// `Map.prototype` methods (ES 23.1.3). Dispatched under
    /// `MAP_METHOD_PREFIX`, so the receiver check is exact: an object that is
    /// not a Map raises, even when the method name also exists elsewhere.
    fn map_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let is_map = matches!(this, Value::Object(o) if o.borrow().kind() == ObjectKind::Map);
        if !is_map {
            return Err(RuntimeError::TypeError(format!(
                "Map.prototype.{method} called on an incompatible receiver"
            )));
        }
        match method {
            // The pure methods reuse the builtin layer's implementations; they
            // re-check the receiver, which is harmless.
            "get" => crate::builtins::map_get(this, args),
            "has" => crate::builtins::map_has(this, args),
            "set" => crate::builtins::map_set(this, args),
            "delete" => crate::builtins::map_delete(this, args),
            "clear" => crate::builtins::map_clear(this, args),
            "forEach" => self.map_for_each(this, args, module),
            // `upsert` (ES2026 proposal, already in the pinned test262):
            // "return the value for key, inserting one if it is missing".
            "getOrInsert" => self.map_get_or_insert(this, args),
            "getOrInsertComputed" => self.map_get_or_insert_computed(this, args, module),
            _ => self.map_iterator(this, method, module),
        }
    }

    /// `Set.prototype` methods (ES 23.2.3) — the Set-side mirror of
    /// [`Self::map_method`].
    fn set_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let is_set = matches!(this, Value::Object(o) if o.borrow().kind() == ObjectKind::Set);
        if !is_set {
            return Err(RuntimeError::TypeError(format!(
                "Set.prototype.{method} called on an incompatible receiver"
            )));
        }
        match method {
            "add" => crate::builtins::set_add(this, args),
            "has" => crate::builtins::set_has(this, args),
            "delete" => crate::builtins::set_delete(this, args),
            "clear" => crate::builtins::set_clear(this, args),
            "forEach" => self.set_for_each(this, args, module),
            // `set-methods` (ES2024 24.2.3.9–15): the seven operators over a
            // set-like argument (`{ size, has, keys }`).
            "union" | "intersection" | "difference" | "symmetricDifference" | "isSubsetOf"
            | "isSupersetOf" | "isDisjointFrom" => {
                self.set_like_method(this, method, args, module)
            }
            _ => self.set_iterator(this, method, module),
        }
    }

    /// `Set.prototype.forEach(cb, thisArg)` (ES 23.2.3.6): the callback gets
    /// `(value, value, set)`.
    fn set_for_each(
        &mut self,
        set_val: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let callback = args.first().cloned().unwrap_or(Value::Undefined);
        if !callback.is_callable() {
            return Err(RuntimeError::TypeError(
                "Set.prototype.forEach callback is not a function".to_string(),
            ));
        }
        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        let Value::Object(obj_ref) = set_val else {
            return Err(RuntimeError::TypeError(
                "Set.prototype.forEach called on a non-object".to_string(),
            ));
        };
        let mut idx = 0;
        loop {
            let step = {
                let borrowed = obj_ref.borrow();
                borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::SetObject>()
                    .and_then(|set| set.entry_from(idx))
            };
            let Some((next, value)) = step else {
                break;
            };
            idx = next + 1;
            self.invoke(
                &callback,
                this_arg.clone(),
                &[value.clone(), value, set_val.clone()],
                module,
            )?;
        }
        Ok(Value::Undefined)
    }

    /// `Set.prototype.values()` / `keys()` / `entries()` (ES 23.2.3.4–8). The
    /// result is live: it holds the set, not a snapshot.
    fn set_iterator(
        &mut self,
        set_val: &Value,
        method: &str,
        _module: &Module,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::iterator::{NativeIteratorObject, NativeIteratorState, SetIterKind};
        let kind = match method {
            "entries" => SetIterKind::Entry,
            _ => SetIterKind::Value,
        };
        let Value::Object(obj_ref) = set_val else {
            return Err(RuntimeError::TypeError(format!(
                "Set.prototype.{method} called on a non-object"
            )));
        };
        let id = self.next_iterator_id;
        self.next_iterator_id += 1;
        let iter_val = Value::Object(Rc::new(RefCell::new(NativeIteratorObject::new(
            id,
            NativeIteratorState::Set {
                set: Rc::clone(obj_ref),
                kind,
                idx: 0,
                done: false,
            },
        ))));
        self.iterator_registry.insert(id, iter_val.clone());
        Ok(iter_val)
    }

    /// `new Set([iterable])` (ES 23.2.1.1) — same shape as `map_construct`.
    fn set_construct(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        new_target: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let proto = self.prototype_from_constructor(constructor_val, new_target);
        let prototype = match &proto {
            Some(Value::Object(p)) => Some(Rc::clone(p)),
            _ => Some(Rc::clone(&self.builtins.set_prototype)),
        };
        let set_val = Value::Object(Rc::new(RefCell::new(crate::vm::object::SetObject::new(
            prototype,
        ))));

        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        if !matches!(iterable, Value::Undefined | Value::Null) {
            let iter = self.make_iterator(iterable, module)?;
            // Step 8.a: the adder is fetched once, through `[[Get]]`.
            let adder = match self.get_member(&set_val, &PropertyKey::from_str("add"), module) {
                Ok(adder) => adder,
                Err(err) => {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            };
            if !adder.is_callable() {
                self.iterator_close(iter, None, true, module)?;
                return Err(RuntimeError::TypeError(
                    "Set constructor: 'add' is not callable".to_string(),
                ));
            }
            loop {
                let (value, done) = match self.iterator_next(iter.clone(), None, module) {
                    Ok(step) => step,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                if done {
                    break;
                }
                if let Err(err) = self.invoke(&adder, set_val.clone(), &[value], module) {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            }
        }
        Ok(set_val)
    }

    /// `Map.prototype.getOrInsert(key, value)`.
    fn map_get_or_insert(&mut self, this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
        let key = args.first().cloned().unwrap_or(Value::Undefined);
        let value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let Value::Object(obj_ref) = this else {
            return Err(RuntimeError::TypeError(
                "Map.prototype.getOrInsert called on a non-object".to_string(),
            ));
        };
        let mut borrowed = obj_ref.borrow_mut();
        let map = borrowed
            .as_any_mut()
            .downcast_mut::<crate::vm::object::MapObject>()
            .expect("try_map_method checked the receiver");
        match map.get(&key) {
            Some(existing) => Ok(existing),
            None => {
                // The key is canonicalised (`-0` → `+0`) by `MapObject::set`.
                map.set(key, value.clone());
                Ok(value)
            }
        }
    }

    // ─────────────────────────────────────────────────────────
    // Set methods (`set-methods`, ES2024)
    // ─────────────────────────────────────────────────────────

    /// `Set.prototype.union` / `intersection` / … (ES2024 24.2.3).
    ///
    /// Seven methods share one shape: validate `this` (done by the caller),
    /// turn the argument into a *set record* through `GetSetRecord`, then walk
    /// one side's keys/values. `union` and `difference` were designed to be
    /// order-stable; check each one's doc before "optimising" the walk.
    fn set_like_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let other = args.first().cloned().unwrap_or(Value::Undefined);
        let record = self.get_set_record(&other, method, module)?;
        match method {
            // Copy `this`, then append everything from `other` that is new.
            "union" => {
                let result = self.set_copy_of(this);
                let iter = self.iter_from_method(&record.object, &record.keys, module)?;
                loop {
                    let (value, done) = self.iterator_next(iter.clone(), None, module)?;
                    if done {
                        break;
                    }
                    if !set_data_has(&result, &value) {
                        set_data_add(&result, value);
                    }
                }
                Ok(result)
            }
            // Spec: when `other` is strictly smaller it is the side that is
            // walked (the result then comes out in `other`'s order), otherwise
            // `this` is walked and filtered through `otherRec.[[Has]]`.
            "intersection" => {
                let this_size = set_data_size(this);
                if record.size >= this_size {
                    let result = set_data_new(Some(self.builtins.set_prototype.clone()));
                    for value in set_data_values(this) {
                        let keep = self.invoke(
                            &record.has,
                            record.object.clone(),
                            std::slice::from_ref(&value),
                            module,
                        )?;
                        if keep.to_boolean() {
                            set_data_add(&result, value);
                        }
                    }
                    Ok(result)
                } else {
                    let result = set_data_new(Some(self.builtins.set_prototype.clone()));
                    let iter = self.iter_from_method(&record.object, &record.keys, module)?;
                    loop {
                        let (value, done) = self.iterator_next(iter.clone(), None, module)?;
                        if done {
                            break;
                        }
                        if set_data_has(this, &value) {
                            set_data_add(&result, value);
                        }
                    }
                    Ok(result)
                }
            }
            // Same size rule as `intersection`, mirrored: when `this` is the
            // smaller side it is walked and each element asked of
            // `otherRec.[[Has]]`; `other`'s keys iterator is not even created
            // (`difference/allows-set-like-*.js` asserts exactly that).
            "difference" => {
                if set_data_size(this) <= record.size {
                    let result = set_data_new(Some(self.builtins.set_prototype.clone()));
                    for value in set_data_values(this) {
                        let found = self.invoke(
                            &record.has,
                            record.object.clone(),
                            std::slice::from_ref(&value),
                            module,
                        )?;
                        if !found.to_boolean() {
                            set_data_add(&result, value);
                        }
                    }
                    Ok(result)
                } else {
                    let result = self.set_copy_of(this);
                    let iter = self.iter_from_method(&record.object, &record.keys, module)?;
                    loop {
                        let (value, done) = self.iterator_next(iter.clone(), None, module)?;
                        if done {
                            break;
                        }
                        set_data_delete(&result, &value);
                    }
                    Ok(result)
                }
            }
            // Copy `this`; each of `other`'s keys toggles membership.
            "symmetricDifference" => {
                let result = self.set_copy_of(this);
                let iter = self.iter_from_method(&record.object, &record.keys, module)?;
                loop {
                    let (value, done) = self.iterator_next(iter.clone(), None, module)?;
                    if done {
                        break;
                    }
                    if !set_data_delete(&result, &value) {
                        set_data_add(&result, value);
                    }
                }
                Ok(result)
            }
            // `isSubsetOf` walks `this` and asks `otherRec.[[Has]]` for each
            // element; the size check is only a shortcut (ES 24.2.3.16).
            "isSubsetOf" => {
                if set_data_size(this) > record.size {
                    return Ok(Value::Bool(false));
                }
                for value in set_data_values(this) {
                    let found = self.invoke(
                        &record.has,
                        record.object.clone(),
                        std::slice::from_ref(&value),
                        module,
                    )?;
                    if !found.to_boolean() {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            // …and `isSupersetOf` the other way round: walk `other`'s keys and
            // look each one up in `this`.
            "isSupersetOf" => {
                if record.size > set_data_size(this) {
                    return Ok(Value::Bool(false));
                }
                let iter = self.iter_from_method(&record.object, &record.keys, module)?;
                loop {
                    let (value, done) = self.iterator_next(iter.clone(), None, module)?;
                    if done {
                        break;
                    }
                    if !set_data_has(this, &value) {
                        // The answer is known, so the iterator is abandoned —
                        // and abandoning one is `IteratorClose`
                        // (`isSupersetOf/set-like-iter-return.js` counts the
                        // `return()` calls).
                        self.iterator_close(iter.clone(), None, false, module)?;
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            // `isDisjointFrom` walks the smaller side: `this` (asking `has`) or
            // `other` (testing membership in `this`).
            "isDisjointFrom" => {
                if set_data_size(this) <= record.size {
                    for value in set_data_values(this) {
                        let found = self.invoke(
                            &record.has,
                            record.object.clone(),
                            std::slice::from_ref(&value),
                            module,
                        )?;
                        if found.to_boolean() {
                            return Ok(Value::Bool(false));
                        }
                    }
                    Ok(Value::Bool(true))
                } else {
                    let iter = self.iter_from_method(&record.object, &record.keys, module)?;
                    loop {
                        let (value, done) = self.iterator_next(iter.clone(), None, module)?;
                        if done {
                            break;
                        }
                        if set_data_has(this, &value) {
                            self.iterator_close(iter.clone(), None, false, module)?;
                            return Ok(Value::Bool(false));
                        }
                    }
                    Ok(Value::Bool(true))
                }
            }
            _ => Err(RuntimeError::InternalError(format!(
                "unhandled set method: {method}"
            ))),
        }
    }

    /// `WeakMap.prototype` methods (ES 23.3.3) — pure, but dispatched here so
    /// the receiver check is exact (`get`/`set`/`has`/`delete` are ordinary
    /// property names elsewhere).
    fn weakmap_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // The receiver check happens inside each builtin (they share `as_weak_map`).
        match method {
            "set" => crate::builtins::weakmap_set(this, args),
            "get" => crate::builtins::weakmap_get(this, args),
            "has" => crate::builtins::weakmap_has(this, args),
            "delete" => crate::builtins::weakmap_delete(this, args),
            // `upsert` (ES2026): same shape as `Map`'s, on a weak key.
            "getOrInsert" => {
                let key = args.first().cloned().unwrap_or(Value::Undefined);
                let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !crate::builtins::can_be_held_weakly(&key) {
                    return Err(RuntimeError::TypeError(
                        "Invalid value used as weak map key".to_string(),
                    ));
                }
                let Value::Object(obj_ref) = this else {
                    return Err(RuntimeError::TypeError(
                        "WeakMap.prototype.getOrInsert called on a non-object".to_string(),
                    ));
                };
                if obj_ref.borrow().kind() != ObjectKind::WeakMap {
                    return Err(RuntimeError::TypeError(
                        "WeakMap.prototype.getOrInsert called on an incompatible receiver"
                            .to_string(),
                    ));
                }
                let mut borrowed = obj_ref.borrow_mut();
                let map = borrowed
                    .as_any_mut()
                    .downcast_mut::<crate::vm::object::WeakMapObject>()
                    .expect("kind checked above");
                match map.get(&key) {
                    Some(existing) => Ok(existing),
                    None => {
                        map.set(key, value.clone());
                        Ok(value)
                    }
                }
            }
            "getOrInsertComputed" => {
                let key = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                if !crate::builtins::can_be_held_weakly(&key) {
                    return Err(RuntimeError::TypeError(
                        "Invalid value used as weak map key".to_string(),
                    ));
                }
                if !callback.is_callable() {
                    return Err(RuntimeError::TypeError(
                        "WeakMap.prototype.getOrInsertComputed callback is not a function"
                            .to_string(),
                    ));
                }
                let Value::Object(obj_ref) = this else {
                    return Err(RuntimeError::TypeError(
                        "WeakMap.prototype.getOrInsertComputed called on a non-object"
                            .to_string(),
                    ));
                };
                if obj_ref.borrow().kind() != ObjectKind::WeakMap {
                    return Err(RuntimeError::TypeError(
                        "WeakMap.prototype.getOrInsertComputed called on an incompatible receiver"
                            .to_string(),
                    ));
                }
                let existing = obj_ref
                    .borrow()
                    .as_any()
                    .downcast_ref::<crate::vm::object::WeakMapObject>()
                    .and_then(|map| map.get(&key));
                if let Some(existing) = existing {
                    return Ok(existing);
                }
                let value = self.invoke(
                    &callback,
                    Value::Undefined,
                    std::slice::from_ref(&key),
                    module,
                )?;
                let mut borrowed = obj_ref.borrow_mut();
                borrowed
                    .as_any_mut()
                    .downcast_mut::<crate::vm::object::WeakMapObject>()
                    .expect("kind checked above")
                    .set(key, value.clone());
                Ok(value)
            }
            _ => Err(RuntimeError::TypeError(format!(
                "WeakMap.prototype.{method} is not implemented"
            ))),
        }
    }

    /// `WeakSet.prototype` methods (ES 23.4.3).
    fn weakset_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match method {
            "add" => crate::builtins::weakset_add(this, args),
            "has" => crate::builtins::weakset_has(this, args),
            "delete" => crate::builtins::weakset_delete(this, args),
            _ => Err(RuntimeError::TypeError(format!(
                "WeakSet.prototype.{method} is not implemented"
            ))),
        }
    }

    /// `new WeakMap([iterable])` (ES 23.3.1.1): same shape as `map_construct`,
    /// with the object-key rule enforced by the adder.
    fn weakmap_construct(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        new_target: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let proto = self.prototype_from_constructor(constructor_val, new_target);
        let prototype = match &proto {
            Some(Value::Object(p)) => Some(Rc::clone(p)),
            _ => Some(Rc::clone(&self.builtins.weakmap_prototype)),
        };
        let map_val = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::WeakMapObject::new(prototype),
        )));
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        if !matches!(iterable, Value::Undefined | Value::Null) {
            let iter = self.make_iterator(iterable, module)?;
            // `adder = Get(map, "set")` — a patched `set` is honoured.
            let adder = match self.get_member(&map_val, &PropertyKey::from_str("set"), module) {
                Ok(adder) => adder,
                Err(err) => {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            };
            if !adder.is_callable() {
                self.iterator_close(iter, None, true, module)?;
                return Err(RuntimeError::TypeError(
                    "WeakMap constructor: 'set' is not callable".to_string(),
                ));
            }
            loop {
                let (item, done) = match self.iterator_next(iter.clone(), None, module) {
                    Ok(step) => step,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                if done {
                    break;
                }
                if !item.is_object() {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(RuntimeError::TypeError(
                        "WeakMap constructor: iterator value is not an entry object".to_string(),
                    ));
                }
                let key = match self.get_member(&item, &PropertyKey::from_str("0"), module) {
                    Ok(key) => key,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                let value = match self.get_member(&item, &PropertyKey::from_str("1"), module) {
                    Ok(value) => value,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                if let Err(err) = self.invoke(&adder, map_val.clone(), &[key, value], module) {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            }
        }
        Ok(map_val)
    }

    /// `new WeakSet([iterable])` (ES 23.4.1.1).
    fn weakset_construct(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        new_target: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let proto = self.prototype_from_constructor(constructor_val, new_target);
        let prototype = match &proto {
            Some(Value::Object(p)) => Some(Rc::clone(p)),
            _ => Some(Rc::clone(&self.builtins.weakset_prototype)),
        };
        let set_val = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::WeakSetObject::new(prototype),
        )));
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        if !matches!(iterable, Value::Undefined | Value::Null) {
            let iter = self.make_iterator(iterable, module)?;
            let adder = match self.get_member(&set_val, &PropertyKey::from_str("add"), module) {
                Ok(adder) => adder,
                Err(err) => {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            };
            if !adder.is_callable() {
                self.iterator_close(iter, None, true, module)?;
                return Err(RuntimeError::TypeError(
                    "WeakSet constructor: 'add' is not callable".to_string(),
                ));
            }
            loop {
                let (value, done) = match self.iterator_next(iter.clone(), None, module) {
                    Ok(step) => step,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                if done {
                    break;
                }
                if let Err(err) = self.invoke(&adder, set_val.clone(), &[value], module) {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            }
        }
        Ok(set_val)
    }

    /// ES 24.2.3.1 `GetSetRecord(obj)`: the `{ size, has, keys }` shape a
    /// set-like argument must expose, validated in the spec's order.
    fn get_set_record(
        &mut self,
        value: &Value,
        method: &str,
        module: &Module,
    ) -> Result<SetRecord, RuntimeError> {
        if !value.is_object() {
            return Err(RuntimeError::TypeError(format!(
                "Set.prototype.{method}: the argument must be an object"
            )));
        }
        let raw_size = self.get_member(value, &PropertyKey::from_str("size"), module)?;
        // `ToNumber(rawSize)` runs user code when `size` is an object
        // (`{ valueOf() { … } }`), which is observable — the tests count those
        // coercions — so it cannot be a plain `Value::to_number()`.
        let number = if raw_size.is_object() {
            self.to_primitive(&raw_size, "number", module)?.to_number()
        } else {
            raw_size.to_number()
        };
        if number.is_nan() {
            return Err(RuntimeError::TypeError(format!(
                "Set.prototype.{method}: size must be a number"
            )));
        }
        if number < 0.0 {
            return Err(RuntimeError::RangeError(
                "Set-like object's size must not be negative".to_string(),
            ));
        }
        let size = if number.is_finite() && number < usize::MAX as f64 {
            number.trunc() as usize
        } else {
            usize::MAX
        };
        let has = self.get_member(value, &PropertyKey::from_str("has"), module)?;
        if !has.is_callable() {
            return Err(RuntimeError::TypeError(format!(
                "Set.prototype.{method}: has must be callable"
            )));
        }
        let keys = self.get_member(value, &PropertyKey::from_str("keys"), module)?;
        if !keys.is_callable() {
            return Err(RuntimeError::TypeError(format!(
                "Set.prototype.{method}: keys must be callable"
            )));
        }
        Ok(SetRecord {
            object: value.clone(),
            size,
            has,
            keys,
        })
    }

    /// `GetIteratorFromMethod(obj, method)`: call `method` on `obj` and wrap the
    /// result as a JS iterator, without consulting `@@iterator` (a set-like
    /// object only has `keys`).
    fn iter_from_method(
        &mut self,
        target: &Value,
        method: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::iterator::{NativeIteratorObject, NativeIteratorState};
        let iterator = self.invoke(method, target.clone(), &[], module)?;
        if !iterator.is_object() {
            return Err(RuntimeError::TypeError(
                "keys() did not return an object".to_string(),
            ));
        }
        let id = self.next_iterator_id;
        self.next_iterator_id += 1;
        let iter_val = Value::Object(Rc::new(RefCell::new(NativeIteratorObject::new(
            id,
            NativeIteratorState::Js { iterator },
        ))));
        self.iterator_registry.insert(id, iter_val.clone());
        Ok(iter_val)
    }

    /// A fresh `Set` holding the same live entries as `this` (the starting
    /// point of `union` / `difference` / `symmetricDifference`).
    ///
    /// The result always has `%Set.prototype%`: the set methods are specified
    /// to return a plain `Set` even on a subclass receiver, and `@@species` is
    /// never consulted (`union/subclass-symbol-species.js` checks the counter).
    fn set_copy_of(&self, this: &Value) -> Value {
        let result = set_data_new(Some(self.builtins.set_prototype.clone()));
        for value in set_data_values(this) {
            set_data_add(&result, value);
        }
        result
    }

    /// `Map.prototype.getOrInsertComputed(key, callbackfn)`: the callback
    /// produces the value for a missing key (and is passed the canonical key
    /// only). A value it inserts itself is overwritten by the one it returns.
    fn map_get_or_insert_computed(
        &mut self,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let key = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !callback.is_callable() {
            return Err(RuntimeError::TypeError(
                "Map.prototype.getOrInsertComputed callback is not a function".to_string(),
            ));
        }
        let Value::Object(obj_ref) = this else {
            return Err(RuntimeError::TypeError(
                "Map.prototype.getOrInsertComputed called on a non-object".to_string(),
            ));
        };
        // Canonicalise before the lookup so the callback receives the key the
        // map would store (`-0` becomes `+0`).
        let canonical = match &key {
            Value::Number(n) if *n == 0.0 => Value::Number(0.0),
            other => other.clone(),
        };
        let existing = obj_ref
            .borrow()
            .as_any()
            .downcast_ref::<crate::vm::object::MapObject>()
            .and_then(|map| map.get(&canonical));
        if let Some(existing) = existing {
            return Ok(existing);
        }
        let value = self.invoke(
            &callback,
            Value::Undefined,
            std::slice::from_ref(&canonical),
            module,
        )?;
        let mut borrowed = obj_ref.borrow_mut();
        let map = borrowed
            .as_any_mut()
            .downcast_mut::<crate::vm::object::MapObject>()
            .expect("try_map_method checked the receiver");
        map.set(canonical, value.clone());
        Ok(value)
    }

    /// `Map.groupBy(items, callbackfn)` (ES2024 `array-grouping`).
    ///
    /// Not part of the ES6 target, but the pinned test262 already ships it and
    /// it is the same iteration + callback machinery the `Map` constructor
    /// needs. Keys are used as-is (`SameValueZero`), with `-0` normalised —
    /// there is no `ToPropertyKey` coercion.
    fn map_group_by(
        &mut self,
        items: &Value,
        callback: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !callback.is_callable() {
            return Err(RuntimeError::TypeError(
                "Map.groupBy callback is not a function".to_string(),
            ));
        }
        let map_val = Value::Object(Rc::new(RefCell::new(crate::vm::object::MapObject::new(
            Some(Rc::clone(&self.builtins.map_prototype)),
        ))));
        let iter = self.make_iterator(items.clone(), module)?;
        let mut index = 0usize;
        loop {
            let (value, done) = self.iterator_next(iter.clone(), None, module)?;
            if done {
                break;
            }
            let key = self.invoke(
                callback,
                Value::Undefined,
                &[value.clone(), Value::Number(index as f64)],
                module,
            )?;
            index += 1;
            let Value::Object(obj_ref) = &map_val else {
                unreachable!("Map.groupBy builds its own Map")
            };
            let mut borrowed = obj_ref.borrow_mut();
            let map = borrowed
                .as_any_mut()
                .downcast_mut::<crate::vm::object::MapObject>()
                .expect("Map.groupBy builds its own Map");
            // Each group is a plain array appended to in encounter order.
            let group = map.get(&key);
            match group {
                Some(Value::Object(list)) => {
                    if let Some(arr) = list.borrow_mut().as_any_mut().downcast_mut::<ArrayObject>()
                    {
                        arr.push(value);
                    }
                }
                _ => {
                    map.set(
                        key,
                        Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(vec![value])))),
                    );
                }
            }
        }
        Ok(map_val)
    }

    /// `Map.prototype.forEach(cb, thisArg)` (ES 23.1.3.5).
    ///
    /// The callback receives `(value, key, map)` — note the order — and the
    /// walk goes through `entry_from` so entries added while it runs are
    /// visited and deleted ones are skipped.
    fn map_for_each(
        &mut self,
        map_val: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let callback = args.first().cloned().unwrap_or(Value::Undefined);
        if !callback.is_callable() {
            return Err(RuntimeError::TypeError(
                "Map.prototype.forEach callback is not a function".to_string(),
            ));
        }
        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        let Value::Object(obj_ref) = map_val else {
            return Err(RuntimeError::TypeError(
                "Map.prototype.forEach called on a non-object".to_string(),
            ));
        };
        let mut idx = 0;
        loop {
            // The borrow must end before the callback runs: it may well mutate
            // the map (`m.forEach(function () { m.delete(1) })`).
            let step = {
                let borrowed = obj_ref.borrow();
                borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::MapObject>()
                    .and_then(|map| map.entry_from(idx))
            };
            let Some((next, key, value)) = step else {
                break;
            };
            idx = next + 1;
            self.invoke(
                &callback,
                this_arg.clone(),
                &[value, key, map_val.clone()],
                module,
            )?;
        }
        Ok(Value::Undefined)
    }

    /// `Map.prototype.entries()` / `keys()` / `values()` (ES 23.1.3.4–7).
    ///
    /// The result is *live*: it keeps the map, not a snapshot.
    fn map_iterator(
        &mut self,
        map_val: &Value,
        method: &str,
        _module: &Module,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::iterator::{MapIterKind, NativeIteratorObject, NativeIteratorState};
        let kind = match method {
            "keys" => MapIterKind::Key,
            "values" => MapIterKind::Value,
            _ => MapIterKind::Entry,
        };
        let Value::Object(obj_ref) = map_val else {
            return Err(RuntimeError::TypeError(format!(
                "Map.prototype.{method} called on a non-object"
            )));
        };
        let id = self.next_iterator_id;
        self.next_iterator_id += 1;
        let iter_val = Value::Object(Rc::new(RefCell::new(NativeIteratorObject::new(
            id,
            NativeIteratorState::Map {
                map: Rc::clone(obj_ref),
                kind,
                idx: 0,
                done: false,
            },
        ))));
        self.iterator_registry.insert(id, iter_val.clone());
        Ok(iter_val)
    }

    /// `new Map([iterable])` (ES 23.1.1.1).
    ///
    /// The iterable is consumed through the real iteration protocol, and every
    /// item is read with `[[Get]]` — `new Map([{ get 0() {…} }])` runs the
    /// getter.
    fn map_construct(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        new_target: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let proto = self.prototype_from_constructor(constructor_val, new_target);
        let prototype = match &proto {
            Some(Value::Object(p)) => Some(Rc::clone(p)),
            _ => Some(Rc::clone(&self.builtins.map_prototype)),
        };
        let map_val = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::MapObject::new(prototype),
        )));

        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        if !matches!(iterable, Value::Undefined | Value::Null) {
            let iter = self.make_iterator(iterable, module)?;
            // Step 9.a: the adder is looked up *once*, through `[[Get]]`, and
            // it is a subclass's `set` when there is one
            // (`iterable-calls-set.js`, `get-set-method-failure.js`).
            let adder = match self.get_member(&map_val, &PropertyKey::from_str("set"), module) {
                Ok(adder) => adder,
                Err(err) => {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            };
            if !adder.is_callable() {
                self.iterator_close(iter, None, true, module)?;
                return Err(RuntimeError::TypeError(
                    "Map constructor: 'set' is not callable".to_string(),
                ));
            }
            loop {
                // 9.b–d: `IteratorStep` + `IteratorValue`, and 9.l: any abrupt
                // completion closes the iterator before it is propagated.
                let (item, done) = match self.iterator_next(iter.clone(), None, module) {
                    Ok(step) => step,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                if done {
                    break;
                }
                // Step 9.f: a non-object item is a TypeError (unlike
                // `Object.fromEntries`, `Map` takes no primitives).
                if !item.is_object() {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(RuntimeError::TypeError(
                        "Map constructor: iterator value is not an entry object".to_string(),
                    ));
                }
                // 9.g–k: `Get(item, "0")` / `Get(item, "1")` run accessors, and
                // the adder is called with the new map as `this`.
                let key = match self.get_member(&item, &PropertyKey::from_str("0"), module) {
                    Ok(key) => key,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                let value = match self.get_member(&item, &PropertyKey::from_str("1"), module) {
                    Ok(value) => value,
                    Err(err) => {
                        self.iterator_close(iter, None, true, module)?;
                        return Err(err);
                    }
                };
                if let Err(err) = self.invoke(&adder, map_val.clone(), &[key, value], module) {
                    self.iterator_close(iter, None, true, module)?;
                    return Err(err);
                }
            }
        }
        Ok(map_val)
    }

    /// A promise as a `Value` (the coercion to `dyn JSObject` has to happen
    /// where the target type is known).
    fn promise_value(promise: &Rc<RefCell<PromiseObject>>) -> Value {
        let obj: Rc<RefCell<dyn crate::vm::object::JSObject>> = promise.clone();
        Value::Object(obj)
    }

    /// A fresh promise with the engine's `Promise.prototype`.
    fn new_promise(&mut self) -> Rc<RefCell<PromiseObject>> {
        let proto = Rc::clone(&self.builtins.promise_prototype);
        let id = self.state.next_promise_id;
        self.state.next_promise_id += 1;
        let promise = Rc::new(RefCell::new(PromiseObject::new(Some(proto), id)));
        self.state.promises.insert(id, Rc::clone(&promise));
        promise
    }

    /// `Promise.resolve(value)` / `Promise.reject(reason)` / `Promise.all` /
    /// `race` / `allSettled` / `any` — the statics that need the VM (they either
    /// register a promise or have to call user `then` methods).
    fn promise_static(&mut self, name: &str, args: &[Value], module: &Module) -> Option<Value> {
        match name {
            "Promise.resolve" => {
                let promise = self.new_promise();
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                // A promise argument is answered as-is (ES 27.2.4.6 step 2).
                if let Some(existing) = self.as_promise(&value) {
                    return Some(Self::promise_value(&existing));
                }
                self.promise_resolve(&promise, value, module);
                Some(Self::promise_value(&promise))
            }
            "Promise.reject" => {
                let promise = self.new_promise();
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                self.promise_settle(&promise, PromiseState::Rejected, value);
                Some(Self::promise_value(&promise))
            }
            "Promise.all" => Some(self.promise_aggregate(AggregateKind::All, args, module)),
            "Promise.race" => Some(self.promise_aggregate(AggregateKind::Race, args, module)),
            "Promise.allSettled" => {
                Some(self.promise_aggregate(AggregateKind::AllSettled, args, module))
            }
            "Promise.any" => Some(self.promise_aggregate(AggregateKind::Any, args, module)),
            _ => None,
        }
    }

    /// A host function boxed for use as a handler (the reaction runner checks
    /// `is_callable`, which is only answered by the boxed form).
    fn native_fn(name: &str) -> Value {
        Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(name))))
    }

    /// `await src` (ES 27.7.5.3): convert `src` to a promise, wait for it, then
    /// answer its fulfilment value or rethrow its reason.
    ///
    /// "Wait" here means draining the microtask queue until the promise settles:
    /// the engine has no event loop, so the only thing that can ever settle a
    /// promise is one of those jobs, which makes the loop convergent. A promise
    /// that can never settle (nothing left in the queue) leaves `undefined`.
    fn await_value(&mut self, src: Value, module: &Module) -> Result<Value, RuntimeError> {
        let promise = match self.as_promise(&src) {
            Some(p) => p,
            None => {
                let p = self.new_promise();
                self.promise_resolve(&p, src, module);
                p
            }
        };
        loop {
            let (state, value) = {
                let borrowed = promise.borrow();
                (borrowed.state, borrowed.value.clone())
            };
            match state {
                PromiseState::Fulfilled => return Ok(value),
                PromiseState::Rejected => return Err(RuntimeError::Thrown(value)),
                PromiseState::Pending => {
                    if self.state.promise_jobs.is_empty() {
                        return Ok(Value::Undefined);
                    }
                    self.run_promise_jobs(module)?;
                }
            }
        }
    }

    /// The promise resolution procedure (`ResolvePromise`, ES 27.2.1.3.2) as
    /// far as this engine can go: adopt a real promise, queue a job for a
    /// foreign thenable, otherwise fulfil.
    fn promise_resolve(
        &mut self,
        promise: &Rc<RefCell<PromiseObject>>,
        value: Value,
        module: &Module,
    ) {
        if let Some(existing) = self.as_promise(&value) {
            if Rc::ptr_eq(&existing, promise) {
                // `resolve(p)` with p itself: a chaining cycle (ES 27.2.1.3.2
                // step 6.1) is a TypeError.
                let reason = crate::builtins::runtime_error_to_js_error(
                    &RuntimeError::TypeError("Chaining cycle detected".to_string()),
                    &self.builtins,
                );
                self.promise_settle(promise, PromiseState::Rejected, reason);
                return;
            }
            // Adopt by forwarding: a reaction with no handlers passes the
            // settlement straight through to `promise` (ES 27.2.2.1 step 9).
            let forwarding = PromiseReaction {
                on_fulfilled: None,
                on_rejected: None,
                target: Rc::clone(promise),
            };
            let settled = {
                let borrowed = existing.borrow();
                if borrowed.is_pending() {
                    None
                } else {
                    Some((borrowed.state, borrowed.value.clone()))
                }
            };
            match settled {
                None => existing.borrow_mut().reactions.push(forwarding),
                Some((state, value)) => self.state.promise_jobs.push_back(PromiseJob::Reaction {
                    reaction: forwarding,
                    argument: value,
                    fulfilled: state == PromiseState::Fulfilled,
                }),
            }
            return;
        }
        // `ResolvePromise` step 8: an object (or function) with a callable
        // `then` is adopted through a job that calls `thenable.then(...)`.
        if matches!(value, Value::Object(_) | Value::Function(_)) {
            if let Ok(then) = self.get_member(&value, &PropertyKey::from_str("then"), module) {
                if then.is_callable() || matches!(then, Value::Function(_)) {
                    let then = self.as_object_value(&then);
                    let _ = then;
                    self.state.promise_jobs.push_back(PromiseJob::Thenable {
                        promise: Rc::clone(promise),
                        thenable: value,
                    });
                    return;
                }
            }
        }
        self.promise_settle(promise, PromiseState::Fulfilled, value);
    }

    /// `Promise.all` / `race` / `allSettled` / `any`. The iterable's elements are
    /// each resolved to a promise and get a per-element native handler whose name
    /// carries `(aggregation id, index)`.
    fn promise_aggregate(
        &mut self,
        kind: AggregateKind,
        args: &[Value],
        module: &Module,
    ) -> Value {
        let result = self.new_promise();
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        let id = self.state.next_aggregation_id;
        self.state.next_aggregation_id += 1;
        // Collect the element promises first: `remaining` is only known once the
        // iterable is exhausted, and the handlers must not fire before then.
        let mut elements: Vec<Value> = Vec::new();
        match self.make_iterator(iterable, module) {
            Ok(iter) => loop {
                let (value, done) = match self.iterator_next(iter.clone(), None, module) {
                    Ok(step) => step,
                    Err(err) => {
                        // A throwing iterator rejects the result promise
                        // (ES 27.2.4.1.1 step 6.f.i).
                        let reason = self.error_value(err);
                        self.promise_settle(&result, PromiseState::Rejected, reason);
                        return Self::promise_value(&result);
                    }
                };
                if done {
                    break;
                }
                elements.push(value);
            },
            Err(err) => {
                let reason = self.error_value(err);
                self.promise_settle(&result, PromiseState::Rejected, reason);
                return Self::promise_value(&result);
            }
        }
        let total = elements.len();
        self.state.aggregations.insert(
            id,
            PromiseAggregation {
                kind,
                result: Rc::clone(&result),
                remaining: total,
                values: vec![Value::Undefined; total],
                done: false,
                rejections: 0,
                total,
            },
        );
        // `Promise.all([])` / `race([])` / `allSettled([])` settle right away;
        // `Promise.any([])` rejects with an (empty) AggregateError.
        if total == 0 {
            return self.finish_empty_aggregate(id, kind);
        }
        for (index, element) in elements.into_iter().enumerate() {
            let element_promise = match self.as_promise(&element) {
                Some(p) => p,
                None => {
                    let p = self.new_promise();
                    self.promise_resolve(&p, element, module);
                    p
                }
            };
            // `promise_then` mints the child that the handler's return value
            // settles; the aggregation itself settles the result promise.
            self.promise_then(
                &element_promise,
                Some(Self::native_fn(&format!("__promise_agg_ok__{id}_{index}"))),
                Some(Self::native_fn(&format!("__promise_agg_err__{id}_{index}"))),
            );
        }
        Self::promise_value(&result)
    }

    /// Settle an aggregation whose element list is empty.
    fn finish_empty_aggregate(&mut self, id: u64, kind: AggregateKind) -> Value {
        let result = {
            let Some(agg) = self.state.aggregations.get(&id) else {
                return Value::Undefined;
            };
            Rc::clone(&agg.result)
        };
        self.state.aggregations.remove(&id);
        match kind {
            AggregateKind::All | AggregateKind::AllSettled => {
                let array = Value::Object(Rc::new(RefCell::new(
                    crate::vm::object::ArrayObject::from_vec(vec![]),
                )));
                self.promise_settle(&result, PromiseState::Fulfilled, array);
            }
            AggregateKind::Race => {
                // `race` over an empty iterable stays pending for ever.
                self.promise_settle(&result, PromiseState::Pending, Value::Undefined);
            }
            AggregateKind::Any => {
                let err = self.aggregate_error(vec![]);
                self.promise_settle(&result, PromiseState::Rejected, err);
            }
        }
        Self::promise_value(&result)
    }

    /// `AggregateError` with an `errors` array, as a JS error value.
    ///
    /// The full constructor is ES2021 and out of this engine's ES6 target, so
    /// no `AggregateError` global exists; the rejection reason still has the
    /// right shape (`name` `"AggregateError"`, own `errors`) for `Promise.any`.
    fn aggregate_error(&self, errors: Vec<Value>) -> Value {
        let prototype = Rc::clone(&self.builtins.error_prototype);
        let mut value = crate::builtins::create_error_object(
            Some("All promises were rejected".to_string()),
            prototype,
            "AggregateError",
        );
        let array = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::ArrayObject::from_vec(errors),
        )));
        if let Value::Object(obj) = &mut value {
            obj.borrow_mut()
                .define_property(
                    PropertyKey::from_str("name"),
                    crate::vm::property::PropertyDescriptor::data_descriptor(Value::string(
                        "AggregateError",
                    )),
                )
                .ok();
            obj.borrow_mut()
                .define_property(
                    PropertyKey::from_str("errors"),
                    crate::vm::property::PropertyDescriptor::data_descriptor(array),
                )
                .ok();
        }
        value
    }

    /// Run one per-element handler of an aggregation. Returns the value the
    /// reaction's child should settle with (ignored — the aggregation settles the
    /// result promise directly).
    fn dispatch_aggregation(
        &mut self,
        name: &str,
        args: &[Value],
    ) -> Option<Result<Value, RuntimeError>> {
        let (id, index) = parse_aggregation_handler(name)?;
        let argument = args.first().cloned().unwrap_or(Value::Undefined);
        let kind = self.state.aggregations.get(&id)?.kind;
        match kind {
            AggregateKind::All => {
                if name.starts_with("__promise_agg_err__") {
                    let result = {
                        let Some(agg) = self.state.aggregations.get(&id) else {
                            return Some(Ok(Value::Undefined));
                        };
                        if agg.done {
                            return Some(Ok(Value::Undefined));
                        }
                        Rc::clone(&agg.result)
                    };
                    self.state.aggregations.remove(&id);
                    self.promise_settle(&result, PromiseState::Rejected, argument);
                    return Some(Ok(Value::Undefined));
                }
                let (complete, result) = {
                    let Some(agg) = self.state.aggregations.get_mut(&id) else {
                        return Some(Ok(Value::Undefined));
                    };
                    agg.values[index] = argument;
                    agg.remaining -= 1;
                    (agg.remaining == 0, Rc::clone(&agg.result))
                };
                if complete {
                    let values = self
                        .state
                        .aggregations
                        .remove(&id)
                        .map(|a| a.values)
                        .unwrap_or_default();
                    let array = Value::Object(Rc::new(RefCell::new(
                        crate::vm::object::ArrayObject::from_vec(values),
                    )));
                    self.promise_settle(&result, PromiseState::Fulfilled, array);
                }
            }
            AggregateKind::Race => {
                let fulfilled = name.starts_with("__promise_agg_ok__");
                let result = {
                    let Some(agg) = self.state.aggregations.get(&id) else {
                        return Some(Ok(Value::Undefined));
                    };
                    if agg.done {
                        return Some(Ok(Value::Undefined));
                    }
                    Rc::clone(&agg.result)
                };
                self.state.aggregations.remove(&id);
                let state = if fulfilled {
                    PromiseState::Fulfilled
                } else {
                    PromiseState::Rejected
                };
                self.promise_settle(&result, state, argument);
            }
            AggregateKind::AllSettled => {
                let fulfilled = name.starts_with("__promise_agg_ok__");
                let (complete, result) = {
                    let Some(agg) = self.state.aggregations.get_mut(&id) else {
                        return Some(Ok(Value::Undefined));
                    };
                    let status = Value::string(if fulfilled { "fulfilled" } else { "rejected" });
                    let key = if fulfilled { "value" } else { "reason" };
                    let mut obj = crate::vm::object::OrdinaryObject::new();
                    obj.define_property(
                        PropertyKey::from_str("status"),
                        crate::vm::property::PropertyDescriptor::data_descriptor(status),
                    )
                    .ok();
                    obj.define_property(
                        PropertyKey::from_str(key),
                        crate::vm::property::PropertyDescriptor::data_descriptor(argument),
                    )
                    .ok();
                    agg.values[index] = Value::Object(Rc::new(RefCell::new(obj)));
                    agg.remaining -= 1;
                    (agg.remaining == 0, Rc::clone(&agg.result))
                };
                if complete {
                    let values = self
                        .state
                        .aggregations
                        .remove(&id)
                        .map(|a| a.values)
                        .unwrap_or_default();
                    let array = Value::Object(Rc::new(RefCell::new(
                        crate::vm::object::ArrayObject::from_vec(values),
                    )));
                    self.promise_settle(&result, PromiseState::Fulfilled, array);
                }
            }
            AggregateKind::Any => {
                if name.starts_with("__promise_agg_ok__") {
                    let result = {
                        let Some(agg) = self.state.aggregations.get(&id) else {
                            return Some(Ok(Value::Undefined));
                        };
                        if agg.done {
                            return Some(Ok(Value::Undefined));
                        }
                        Rc::clone(&agg.result)
                    };
                    self.state.aggregations.remove(&id);
                    self.promise_settle(&result, PromiseState::Fulfilled, argument);
                    return Some(Ok(Value::Undefined));
                }
                let (exhausted, errors, result) = {
                    let Some(agg) = self.state.aggregations.get_mut(&id) else {
                        return Some(Ok(Value::Undefined));
                    };
                    agg.values[index] = argument;
                    agg.rejections += 1;
                    (
                        agg.rejections == agg.total,
                        agg.values.clone(),
                        Rc::clone(&agg.result),
                    )
                };
                if exhausted {
                    self.state.aggregations.remove(&id);
                    let err = self.aggregate_error(errors);
                    self.promise_settle(&result, PromiseState::Rejected, err);
                }
            }
        }
        Some(Ok(Value::Undefined))
    }

    fn promise_settle(
        &mut self,
        promise: &Rc<RefCell<PromiseObject>>,
        state: PromiseState,
        value: Value,
    ) {
        let reactions = {
            let mut borrowed = promise.borrow_mut();
            if !borrowed.is_pending() {
                return;
            }
            borrowed.state = state;
            borrowed.value = value.clone();
            std::mem::take(&mut borrowed.reactions)
        };
        let fulfilled = state == PromiseState::Fulfilled;
        for reaction in reactions {
            self.state.promise_jobs.push_back(PromiseJob::Reaction {
                reaction,
                argument: value.clone(),
                fulfilled,
            });
        }
    }

    /// The receiver as a promise, or None when it is not one.
    fn as_promise(&self, value: &Value) -> Option<Rc<RefCell<PromiseObject>>> {
        let Value::Object(obj_ref) = value else {
            return None;
        };
        let id = {
            let borrowed = obj_ref.borrow();
            if borrowed.kind() != ObjectKind::Promise {
                return None;
            }
            borrowed.as_any().downcast_ref::<PromiseObject>()?.id
        };
        self.state.promises.get(&id).cloned()
    }

    /// `promise.then(onFulfilled, onRejected)` — always asynchronous, so a
    /// settled promise still queues its reaction.
    fn promise_then(
        &mut self,
        promise: &Rc<RefCell<PromiseObject>>,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
    ) -> Value {
        let child = self.new_promise();
        let reaction = PromiseReaction {
            on_fulfilled,
            on_rejected,
            target: Rc::clone(&child),
        };
        let settled = {
            let borrowed = promise.borrow();
            if borrowed.is_pending() {
                None
            } else {
                Some((borrowed.state, borrowed.value.clone()))
            }
        };
        match settled {
            None => promise.borrow_mut().reactions.push(reaction),
            Some((state, value)) => {
                self.state.promise_jobs.push_back(PromiseJob::Reaction {
                    reaction,
                    argument: value,
                    fulfilled: state == PromiseState::Fulfilled,
                });
            }
        }
        Self::promise_value(&child)
    }

    /// `String.prototype.replace(searchValue, replacer)` when the replacer is a
    /// **function** — the everyday `s.replace(/re/g, fn)` shape.
    ///
    /// The builtin layer cannot call user code, so the whole substitution runs
    /// here: find the matches, call the replacer with
    /// `(match, group…, offset, input)`, and splice its `ToString` in.
    fn try_string_callback_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if !matches!(method, "replace" | "replaceAll") {
            return Ok(None);
        }
        let Value::String(text) = this else {
            return Ok(None);
        };
        let replacer = match args.get(1) {
            // A bare `Value::Function(id)` is callable too; `is_callable()` only
            // answers for the boxed form, and this interception runs before the
            // boxing that `call_native_by_name` does.
            Some(v) if v.is_callable() || matches!(v, Value::Function(_)) => v.clone(),
            _ => return Ok(None),
        };
        let replacer = match replacer {
            Value::Function(_) => self.as_object_value(&replacer),
            other => other,
        };
        let search = args.first().cloned().unwrap_or(Value::Undefined);
        let mut global = method == "replaceAll";

        // Collect the matches: a RegExp contributes its captures (all of them
        // when `g`), anything else is a plain substring search.
        let mut matches: Vec<(usize, usize, Vec<Option<String>>)> = Vec::new();
        let mut regex_flags = String::new();
        let compiled = match &search {
            Value::Object(obj_ref) => {
                let kind_is_regexp =
                    obj_ref.borrow().kind() == crate::vm::property::ObjectKind::RegExp;
                if kind_is_regexp {
                    let borrowed = obj_ref.borrow();
                    let re = borrowed
                        .as_any()
                        .downcast_ref::<crate::vm::object::RegExpObject>()
                        .expect("ObjectKind::RegExp implies RegExpObject");
                    regex_flags = re.flags.clone();
                    global = global || regex_flags.contains('g');
                    re.compiled.clone()
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(matcher) = compiled {
            for captures in matcher.captures_iter(text.as_str()) {
                let whole = match captures.get(0) {
                    Some(m) => m,
                    None => continue,
                };
                let groups = (1..captures.len())
                    .map(|i| captures.get(i).map(|m| m.as_str().to_string()))
                    .collect();
                matches.push((whole.start(), whole.end(), groups));
                if !global {
                    break;
                }
            }
        } else {
            let needle = search.to_js_string();
            if !needle.is_empty() {
                if global {
                    let mut from = 0usize;
                    while let Some(at) = text.as_str()[from..].find(needle.as_str()) {
                        let start = from + at;
                        matches.push((start, start + needle.len(), Vec::new()));
                        from = start + needle.len().max(1);
                    }
                } else if let Some(at) = text.as_str().find(needle.as_str()) {
                    matches.push((at, at + needle.len(), Vec::new()));
                }
            } else {
                // An empty pattern matches at both ends (ES 22.1.3.17 step 8).
                matches.push((0, 0, Vec::new()));
            }
        }

        let mut out = String::new();
        let mut last = 0usize;
        for (start, end, groups) in matches {
            out.push_str(&text.as_str()[last..start]);
            let mut call_args: Vec<Value> = vec![Value::string(&text.as_str()[start..end])];
            for group in groups {
                call_args.push(match group {
                    Some(g) => Value::string(&g),
                    None => Value::Undefined,
                });
            }
            call_args.push(Value::Number(start as f64));
            call_args.push(Value::string(text.as_str()));
            let replacement = self
                .invoke(&replacer, Value::Undefined, &call_args, module)?
                .to_js_string();
            out.push_str(&replacement);
            last = end;
        }
        out.push_str(&text.as_str()[last..]);
        Ok(Some(Value::string(&out)))
    }

    /// `then` / `catch` / `finally`, when the receiver is a promise.
    fn try_promise_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        _module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if !matches!(method, "then" | "catch" | "finally") {
            return Ok(None);
        }
        let Some(promise) = self.as_promise(this) else {
            // Not a promise: let the ordinary prototype dispatch answer (it is
            // what reports "not a function" for a missing method).
            return Ok(None);
        };
        // A bare `Value::Function(id)` is callable but `is_callable()` only
        // answers for the boxed form, so box first — that is exactly what
        // `call_native_by_name` does before handing arguments to the builtin
        // layer, and this interception runs *before* that.
        let mut callable = |v: Option<&Value>| -> Option<Value> {
            v.filter(|v| v.is_callable() || matches!(v, Value::Function(_)))
                .map(|v| {
                    let mut boxed = v.clone();
                    if matches!(boxed, Value::Function(_)) {
                        boxed = self.as_object_value(&boxed);
                    }
                    boxed
                })
        };
        match method {
            "then" => {
                let on_fulfilled = callable(args.first());
                let on_rejected = callable(args.get(1));
                Ok(Some(self.promise_then(&promise, on_fulfilled, on_rejected)))
            }
            "catch" => {
                let on_rejected = callable(args.first());
                Ok(Some(self.promise_then(&promise, None, on_rejected)))
            }
            _ => {
                // `finally(cb)` (ES 27.2.5.3): returns a *fresh* promise that
                // settles like the receiver, except that a throwing `cb` rejects
                // it. The two handlers are natives carrying a registry id; the
                // callback itself runs when the reaction is drained.
                let callback = callable(args.first());
                match callback {
                    None => Ok(Some(self.promise_then(&promise, None, None))),
                    Some(cb) => {
                        let id = self.state.next_finally_id;
                        self.state.next_finally_id += 1;
                        self.state.finally_handlers.insert(id, cb);
                        let fulfilled = Self::native_fn(&format!("__promise_finally__{id}_f"));
                        let rejected = Self::native_fn(&format!("__promise_finally__{id}_r"));
                        Ok(Some(self.promise_then(
                            &promise,
                            Some(fulfilled),
                            Some(rejected),
                        )))
                    }
                }
            }
        }
    }

    /// Drain the microtask queue, running each reaction handler.
    fn run_promise_jobs(&mut self, module: &Module) -> Result<(), RuntimeError> {
        while let Some(job) = self.state.promise_jobs.pop_front() {
            let (reaction, argument, fulfilled) = match job {
                PromiseJob::Reaction {
                    reaction,
                    argument,
                    fulfilled,
                } => (reaction, argument, fulfilled),
                PromiseJob::Thenable { promise, thenable } => {
                    // ES 27.2.1.9: call `thenable.then(resolveFn, rejectFn)` with
                    // functions that resolve/reject the adopting promise.
                    let id = promise.borrow().id;
                    let resolve = Self::native_fn(&format!("__promise_resolve__{id}"));
                    let reject = Self::native_fn(&format!("__promise_reject__{id}"));
                    let then = match self.get_member(
                        &thenable,
                        &PropertyKey::from_str("then"),
                        module,
                    ) {
                        Ok(then) => then,
                        Err(err) => {
                            let value = self.error_value(err);
                            self.promise_settle(&promise, PromiseState::Rejected, value);
                            continue;
                        }
                    };
                    // A throwing `then` rejects the adopting promise.
                    if let Err(err) = self.invoke(&then, thenable, &[resolve, reject], module) {
                        let value = self.error_value(err);
                        self.promise_settle(&promise, PromiseState::Rejected, value);
                    }
                    continue;
                }
            };
            let handler = if fulfilled {
                reaction.on_fulfilled.clone()
            } else {
                reaction.on_rejected.clone()
            };
            match handler {
                Some(handler) if handler.is_callable() => {
                    match self.invoke(&handler, Value::Undefined, &[argument], module) {
                        Ok(value) => {
                            // ES 27.2.2.1 step 12: the handler's answer goes
                            // through the resolution procedure, so a thenable it
                            // returns is adopted rather than fulfilled directly.
                            self.promise_resolve(&reaction.target, value, module)
                        }
                        Err(RuntimeError::Thrown(reason)) => {
                            self.promise_settle(&reaction.target, PromiseState::Rejected, reason)
                        }
                        Err(other) => {
                            let value = self.error_value(other);
                            self.promise_settle(&reaction.target, PromiseState::Rejected, value)
                        }
                    }
                }
                // No handler of the matching kind: the settlement passes
                // through to the child (ES 27.2.2.1 step 9).
                _ => {
                    let state = if fulfilled {
                        PromiseState::Fulfilled
                    } else {
                        PromiseState::Rejected
                    };
                    self.promise_settle(&reaction.target, state, argument);
                }
            }
        }
        Ok(())
    }

    /// A `RuntimeError` as a JS error value (what a rejection reason should be).
    fn error_value(&self, err: RuntimeError) -> Value {
        match err {
            RuntimeError::Thrown(value) => value,
            other => crate::builtins::runtime_error_to_js_error(&other, &self.builtins),
        }
    }

    /// `new Promise(executor)`: build the promise, mint its `resolve` / `reject`
    /// natives (their target is carried in the function name), then run the
    /// executor. A throw from the executor rejects (ES 27.2.3.1 step 11).
    fn promise_construct(
        &mut self,
        executor: &Value,
        proto: Option<Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let promise = match proto {
            Some(Value::Object(p)) => {
                let id = self.state.next_promise_id;
                self.state.next_promise_id += 1;
                let promise = Rc::new(RefCell::new(PromiseObject::new(Some(p), id)));
                self.state.promises.insert(id, Rc::clone(&promise));
                promise
            }
            _ => self.new_promise(),
        };
        let id = promise.borrow().id;
        let native = |name: &str| {
            Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(name))))
        };
        let resolve = native(&format!("__promise_resolve__{id}"));
        let reject = native(&format!("__promise_reject__{id}"));
        match self.invoke(executor, Value::Undefined, &[resolve, reject], module) {
            Ok(_) => {}
            Err(err) => {
                let value = self.error_value(err);
                self.promise_settle(&promise, PromiseState::Rejected, value);
            }
        }
        Ok(Self::promise_value(&promise))
    }

    fn try_array_callback_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        const CALLBACK_METHODS: &[&str] = &[
            "map",
            "filter",
            "forEach",
            "some",
            "every",
            "reduce",
            "reduceRight",
            "find",
            "findIndex",
            "flatMap",
            "sort",
            // No callback, but they need the generic array-like element list:
            // `Array.prototype.indexOf.call({length: 2, 0: 'a'}, 'a')`.
            "indexOf",
            "lastIndexOf",
        ];
        if !CALLBACK_METHODS.contains(&method) {
            return Ok(None);
        }
        // `Array.prototype.map.call(null, …)` and friends: ToObject raises.
        if matches!(receiver, Value::Undefined | Value::Null) {
            return Err(RuntimeError::TypeError(format!(
                "Array.prototype.{method} called on null or undefined"
            )));
        }
        // `forEach` also lives on `Map.prototype` / `Set.prototype`, and this
        // interception runs before the native dispatch (it is what makes
        // `arr.forEach(cb)` call the callback at all). A collection has no
        // `length`, so it would be treated as a zero-length array-like and the
        // callback would never run — `m.forEach(cb)` / `s.forEach(cb)` has to
        // fall through to the collection's own method. Every *other* receiver
        // keeps the old behaviour, including primitives
        // (`Array.prototype.forEach.call(true, cb)` iterates nothing).
        if matches!(receiver, Value::Object(o)
            if matches!(o.borrow().kind(), ObjectKind::Map | ObjectKind::Set))
        {
            return Ok(None);
        }

        let entries = match self.array_like_entries(receiver, module) {
            Ok(entries) => entries,
            // Not array-like (e.g. `filter.call(undefined, …)`): let the caller
            // continue with the ordinary dispatch, which raises a TypeError.
            Err(RuntimeError::TypeError(_)) => return Ok(None),
            Err(err) => return Err(err),
        };

        // `indexOf` / `lastIndexOf` take a search element, not a callback, and
        // must not hit the callable check below. Strings keep their own
        // substring semantics (`"abcab".indexOf("ab")` is 0, not a per-char
        // scan), so a string receiver falls through to the string builtins.
        if method == "indexOf" || method == "lastIndexOf" {
            // `Array.prototype.indexOf.call(x, …)` and `x.indexOf(…)` reach the
            // same dispatch site. Anything that is not array-like — a string
            // (substring search) or a wrapper without `length`
            // (`String.prototype.indexOf.call(new Boolean, …)`) — belongs to
            // the string builtins.
            if !self.receiver_is_array_like(receiver) {
                return Ok(None);
            }
            return self.array_index_of(&entries, method, args).map(Some);
        }

        let callback = args.first().cloned().unwrap_or(Value::Undefined);
        // Second argument is `thisArg`: undefined means "call with undefined
        // `this`" (only relevant for non-strict callbacks, but observable).
        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        // The callback's third argument is `O` — `ToObject(this value)` — not the
        // primitive receiver itself: `Array.prototype.reduce.call(false, cb, 1)`
        // is asserted with `obj instanceof Boolean`, which is only true for the
        // wrapper.
        let callback_receiver = match receiver {
            Value::Object(_) | Value::Function(_) => receiver.clone(),
            other => crate::builtins::to_object(other).unwrap_or_else(|_| other.clone()),
        };
        let call = |vm: &mut Self,
                    cb: &Value,
                    elem: &Value,
                    index: usize|
         -> Result<Value, RuntimeError> {
            vm.invoke(
                cb,
                this_arg.clone(),
                &[
                    elem.clone(),
                    Value::Number(index as f64),
                    callback_receiver.clone(),
                ],
                module,
            )
        };

        match method {
            "sort" => {
                // Holes sort to the end; the dense store has no hole marker, so
                // they are treated as `undefined` (the previous behaviour).
                let mut items: Vec<Value> = entries
                    .iter()
                    .map(|e| e.clone().unwrap_or(Value::Undefined))
                    .collect();
                if callback.is_callable() {
                    // Insertion sort: stable and lets the comparator be a
                    // fallible JS call.
                    for i in 1..items.len() {
                        let mut j = i;
                        while j > 0 {
                            let cmp = self
                                .invoke(
                                    &callback,
                                    Value::Undefined,
                                    &[items[j - 1].clone(), items[j].clone()],
                                    module,
                                )?
                                .to_number();
                            if cmp > 0.0 {
                                items.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else if !callback.is_undefined() {
                    return Err(RuntimeError::TypeError(
                        "The comparison function must be either a function or undefined"
                            .to_string(),
                    ));
                } else {
                    items.sort_by(|a, b| a.to_js_string().cmp(&b.to_js_string()));
                }
                if let Value::Object(obj_ref) = receiver {
                    let mut borrowed = obj_ref.borrow_mut();
                    if let Some(arr) = borrowed.as_any_mut().downcast_mut::<ArrayObject>() {
                        arr.replace_elements(items);
                    }
                }
                Ok(Some(receiver.clone()))
            }
            _ if !callback.is_callable() => Err(RuntimeError::TypeError(format!(
                "{method} callback is not a function"
            ))),
            "forEach" => {
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        call(self, &callback, e, i)?;
                    }
                }
                Ok(Some(Value::Undefined))
            }
            "map" => {
                // Step 5: `A = ? ArraySpeciesCreate(O, len)`. When the default
                // array is used the result keeps the receiver's length *and*
                // its holes; with a species constructor only the present
                // indices are written, and a blocked write is abrupt
                // (CreateDataPropertyOrThrow).
                let species = self.array_species_create(receiver, entries.len(), module)?;
                let mut values: Vec<(usize, Value)> = Vec::with_capacity(entries.len());
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        values.push((i, call(self, &callback, e, i)?));
                    }
                }
                Ok(Some(match species {
                    None => {
                        let mut out: Vec<Value> = vec![Value::Undefined; entries.len()];
                        let mut holes: Vec<usize> = Vec::new();
                        let mut next = 0usize;
                        for (i, v) in values {
                            out[i] = v;
                            for h in next..i {
                                holes.push(h);
                            }
                            next = i + 1;
                        }
                        for h in next..entries.len() {
                            holes.push(h);
                        }
                        let result = crate::vm::object::new_array_object_from_vec(out);
                        if let Value::Object(result_ref) = &result {
                            if let Some(arr) = result_ref
                                .borrow_mut()
                                .as_any_mut()
                                .downcast_mut::<crate::vm::object::ArrayObject>()
                            {
                                for i in holes {
                                    arr.mark_hole(i);
                                }
                            }
                        }
                        result
                    }
                    Some(target) => {
                        for (i, v) in values {
                            self.set_member(
                                &target,
                                PropertyKey::from_str(&i.to_string()),
                                v,
                                module,
                            )?;
                        }
                        target
                    }
                }))
            }
            "flatMap" => {
                // Map, then flatten one level (ES 23.1.3.?): a mapped value that
                // is an array contributes its elements, anything else is
                // appended as-is. This is the same `call` plumbing `map` uses.
                let species = self.array_species_create(receiver, 0, module)?;
                let mut values: Vec<Value> = Vec::new();
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        let mapped = call(self, &callback, e, i)?;
                        let mut flattened = false;
                        if let Value::Object(obj_ref) = &mapped {
                            if obj_ref.borrow().kind() == ObjectKind::Array {
                                let borrowed = obj_ref.borrow();
                                if let Some(arr) = borrowed
                                    .as_any()
                                    .downcast_ref::<crate::vm::object::ArrayObject>()
                                {
                                    for k in 0..arr.len() {
                                        if let Some(v) = arr.get(k) {
                                            values.push(v.clone());
                                        }
                                    }
                                    flattened = true;
                                }
                            }
                        }
                        if !flattened {
                            values.push(mapped);
                        }
                    }
                }
                Ok(Some(match species {
                    None => crate::vm::object::new_array_object_from_vec(values),
                    Some(target) => {
                        for (i, v) in values.into_iter().enumerate() {
                            self.set_member(
                                &target,
                                PropertyKey::from_str(&i.to_string()),
                                v,
                                module,
                            )?;
                        }
                        target
                    }
                }))
            }
            "filter" => {
                let species = self.array_species_create(receiver, 0, module)?;
                let mut out: Vec<Value> = Vec::new();
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        if call(self, &callback, e, i)?.to_boolean() {
                            out.push(e.clone());
                        }
                    }
                }
                Ok(Some(match species {
                    None => crate::vm::object::new_array_object_from_vec(out),
                    Some(target) => {
                        for (i, v) in out.iter().enumerate() {
                            self.set_member(
                                &target,
                                PropertyKey::from_str(&i.to_string()),
                                v.clone(),
                                module,
                            )?;
                        }
                        target
                    }
                }))
            }
            "some" => {
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        if call(self, &callback, e, i)?.to_boolean() {
                            return Ok(Some(Value::Bool(true)));
                        }
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            "every" => {
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        if !call(self, &callback, e, i)?.to_boolean() {
                            return Ok(Some(Value::Bool(false)));
                        }
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            "find" => {
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        if call(self, &callback, e, i)?.to_boolean() {
                            return Ok(Some(e.clone()));
                        }
                    }
                }
                Ok(Some(Value::Undefined))
            }
            "findIndex" => {
                for (i, e) in entries.iter().enumerate() {
                    if let Some(e) = e {
                        if call(self, &callback, e, i)?.to_boolean() {
                            return Ok(Some(Value::Number(i as f64)));
                        }
                    }
                }
                Ok(Some(Value::Number(-1.0)))
            }
            "reduce" | "reduceRight" => {
                // Indices are visited in order (or reverse) and holes skipped;
                // without an initial value the accumulator is the first
                // *present* element.
                let mut indices: Vec<usize> = (0..entries.len()).collect();
                if method == "reduceRight" {
                    indices.reverse();
                }
                let (mut acc, start) = match args.get(1) {
                    Some(init) => (init.clone(), 0usize),
                    None => {
                        let Some(&first) = indices.iter().find(|&&i| entries[i].is_some()) else {
                            return Err(RuntimeError::TypeError(
                                "Reduce of empty array with no initial value".to_string(),
                            ));
                        };
                        let first_value = entries[first].clone().unwrap_or(Value::Undefined);
                        (first_value, indices.iter().position(|&i| i == first).unwrap_or(0) + 1)
                    }
                };
                for &i in indices.iter().skip(start) {
                    let Some(element) = entries[i].clone() else {
                        continue;
                    };
                    // `reduce` has no `thisArg` parameter: the callback runs
                    // with `undefined` as `this`.
                    acc = self.invoke(
                        &callback,
                        Value::Undefined,
                        &[acc, element, Value::Number(i as f64), callback_receiver.clone()],
                        module,
                    )?;
                }
                Ok(Some(acc))
            }
            _ => Ok(None),
        }
    }

    /// Read the arguments a caller pushed for a dynamic call.
    ///
    /// Arguments live just below the current frame: arg0 at `[rbp-1]`, arg1 at
    /// `[rbp-2]`, … (they were pushed in reverse order).
    fn collect_call_args(&self, arg_count: usize) -> Result<Vec<Value>, RuntimeError> {
        let mut args = Vec::with_capacity(arg_count);
        for i in 0..arg_count {
            let index = self.state.rbp - i - 1;
            args.push(self.state.raw_stack_value(index));
        }
        Ok(args)
    }

    /// Resolve a property key from an operand.
    ///
    /// If the operand is an immediate, it's a constant pool index (static property name).
    /// Otherwise, it's a runtime value (dynamic property name).
    fn resolve_property_key(
        &mut self,
        operand: Operand,
        module: &Module,
    ) -> Result<PropertyKey, RuntimeError> {
        match operand {
            Operand::Immd(id) => match &module.constants[id as usize] {
                Constant::String(s) => Ok(PropertyKey::from_str(s.as_str())),
            },
            _ => {
                let prop_val = self.get_value(operand)?;
                match &prop_val {
                    Value::Symbol(sym) => return Ok(PropertyKey::Symbol(sym.id)),
                    Value::String(s) => return Ok(PropertyKey::from_str(s.as_str())),
                    _ => {}
                }
                // ES `ToPropertyKey` = ToPrimitive(string) then ToString, so a
                // key object's `toString` runs (its side effects are observed
                // by test262) before the key is used.
                let primitive = self.to_primitive(&prop_val, "string", module)?;
                Ok(match &primitive {
                    Value::Symbol(sym) => PropertyKey::Symbol(sym.id),
                    other => PropertyKey::from_str(&other.to_js_string()),
                })
            }
        }
    }

    /// Discard the JS frames entered after the `try` that owns the record: an
    /// exception raised in a nested call must be handled in the frame that
    /// wrote the `try`, not in the frame that threw.
    fn unwind_frames_to(
        &mut self,
        ctrl_depth: usize,
        this_depth: usize,
        construct_depth: usize,
        new_target_depth: usize,
        closure_depth: usize,
    ) {
        self.state.ctrl_stack.truncate(ctrl_depth);
        // `this_stack` / `function_stack` / `frame_argc` are parallel: one
        // entry per active frame, holding the *caller's* binding. Popping them
        // restores the handler's frame binding one level at a time.
        while self.state.this_stack.len() > this_depth {
            if let Some(saved_this) = self.state.this_stack.pop() {
                self.state.this_val = saved_this;
            }
        }
        while self.state.function_stack.len() > this_depth {
            if let Some(saved_function) = self.state.function_stack.pop() {
                self.state.function_val = saved_function;
            }
        }
        self.state.frame_argc.truncate(this_depth);
        // Parallel to `this_stack`: a frame whose `this` was never bound must
        // not leave its state behind, or a later `Ret` would raise again.
        self.state.this_state.truncate(this_depth);
        self.state.construct_stack.truncate(construct_depth);
        self.state.new_target_stack.truncate(new_target_depth);
        self.state.closure_var_stack.truncate(closure_depth);
    }

    fn handle_throw(&mut self, exc_val: Value) -> Result<(), RuntimeError> {
        // An exception raised inside a Rust-driven call frame must not be
        // dispatched to a handler *outside* that frame from here: the native
        // that asked for the call (a callback loop, `new` from a builtin, …)
        // has to see the error so it can stop its own work. Turn it back into
        // a thrown value and let `invoke_*` propagate it to the outer step.
        if let Some(boundary) = self.invoke_boundary() {
            if let Some(top) = self.state.seh_stack.last() {
                if top.saved_ctrl_depth <= boundary {
                    return Err(RuntimeError::Thrown(exc_val));
                }
            }
        }
        match self.state.seh_stack.pop() {
            Some(mut record) => {
                self.state.rsp = record.saved_rsp;
                self.state.rbp = record.saved_rbp;
                self.unwind_frames_to(
                    record.saved_ctrl_depth,
                    record.saved_this_depth,
                    record.saved_construct_depth,
                    record.saved_new_target_depth,
                    record.saved_closure_depth,
                );

                let finally_pc = record.finally_pc;
                let handler_pc = record.handler_pc;

                if finally_pc != 0 && !record.in_finally {
                    record.in_finally = true;
                    record.pending_exception = Some(exc_val);
                    self.state.seh_stack.push(record);
                    self.state.jump(finally_pc);
                    Ok(())
                } else if handler_pc != 0 {
                    self.state.set_register(Register::Rv, exc_val)?;
                    self.state.jump(handler_pc);
                    Ok(())
                } else {
                    self.handle_throw(exc_val)
                }
            }
            None => Err(RuntimeError::Thrown(exc_val)),
        }
    }

    fn get_value(&self, operand: Operand) -> Result<Value, RuntimeError> {
        match operand {
            Operand::Primitive(p) => Ok(Self::from_primitive(p)),
            Operand::Register(reg) => self.state.get_register(reg),
            Operand::Stack(offset) => self.state.get_value_from_stack(offset),
            Operand::Immd(_) => {
                // Immd is typically used for block/function IDs, not values
                // In the codegen, Immd is never used as a value operand directly
                Err(RuntimeError::TypeError(format!(
                    "cannot load value from immediate operand: {operand}"
                )))
            }
            Operand::Symbol(sym) => Ok(Value::Function(sym)),
        }
    }

    fn set_value(&mut self, operand: Operand, value: Value) -> Result<(), RuntimeError> {
        match operand {
            Operand::Register(reg) => self.state.set_register(reg, value),
            Operand::Stack(offset) => self.state.set_value_to_stack(offset, value),
            _ => Err(RuntimeError::TypeError(format!(
                "cannot store to operand: {operand}"
            ))),
        }
    }

    fn from_primitive(p: Primitive) -> Value {
        match p {
            Primitive::Null => Value::Null,
            Primitive::Undefined => Value::Undefined,
            Primitive::Boolean(b) => Value::Bool(b),
            Primitive::Integer(i) => Value::Number(i as f64),
            Primitive::Float(f) => Value::Number(f),
        }
    }

    fn from_constant(c: &Constant) -> Value {
        match c {
            Constant::String(s) => Value::String(Rc::new(s.as_ref().clone())),
        }
    }

    /// JS Abstract Relational Comparison (ECMAScript 13.10.1)
    ///
    /// Returns true if x < y using ToPrimitive with number coercion.
    fn js_abstract_relational(x: &Value, y: &Value) -> Result<bool, RuntimeError> {
        // ToPrimitive: for basic types, use the value directly
        // String comparison has priority when both are strings
        match (x, y) {
            (Value::String(xs), Value::String(ys)) => Ok(xs < ys),
            _ => {
                // Convert to numbers and compare
                let nx = x.to_number();
                let ny = y.to_number();
                // NaN comparisons always return false
                if nx.is_nan() || ny.is_nan() {
                    return Ok(false);
                }
                Ok(nx < ny)
            }
        }
    }

    /// Coerce both operands with ToPrimitive (hint "number") for relational
    /// comparison, returning the coerced values.
    fn coerce_for_relational(
        &mut self,
        x: Value,
        y: Value,
        module: &Module,
    ) -> Result<(Value, Value), RuntimeError> {
        let px = self.to_primitive(&x, "number", module)?;
        let py = self.to_primitive(&y, "number", module)?;
        Ok((px, py))
    }

    /// JS Abstract Equality Comparison (`==`, ECMAScript 13.11.1).
    ///
    /// For object operands we first apply ToPrimitive (so `obj == "..."` and
    /// `obj == 42` behave per spec); two objects compare by reference.
    fn abstract_eq(&mut self, x: &Value, y: &Value, module: &Module) -> Result<bool, RuntimeError> {
        if x.is_object() || y.is_object() {
            let px = self.to_primitive(x, "default", module)?;
            let py = self.to_primitive(y, "default", module)?;
            return self.abstract_eq(&px, &py, module);
        }
        Ok(x.abstract_eq(y))
    }

    fn js_prop_get(obj: &Value, prop: &str) -> Result<Value, RuntimeError> {
        match obj {
            Value::String(s) => {
                // String prototype properties
                match prop {
                    "length" => Ok(Value::Number(s.len() as f64)),
                    _ => Ok(Value::Undefined),
                }
            }
            Value::Object(obj_ref) => {
                let key = PropertyKey::from_str(prop);
                crate::vm::prototype::internal_get(obj_ref.clone(), &key, None)
                    .map_err(RuntimeError::from_property_error)
            }
            _ => Ok(Value::Undefined),
        }
    }

    /// Resolve the prototype object that backs a primitive's wrapper type.
    fn primitive_prototype(&self, val: &Value) -> Option<Rc<RefCell<dyn JSObject>>> {
        match val {
            Value::String(_) => Some(Rc::clone(&self.builtins.string_prototype)),
            Value::Number(_) => Some(Rc::clone(&self.builtins.number_prototype)),
            Value::Bool(_) => Some(Rc::clone(&self.builtins.boolean_prototype)),
            Value::Symbol(_) => Some(Rc::clone(&self.builtins.symbol_prototype)),
            _ => None,
        }
    }

    /// ES `[[Get]]` over any JS value.
    ///
    /// Objects use the prototype chain; primitives get the automatic wrapper
    /// behaviour (`"abc".length`, `(1).toString`, …); `null`/`undefined` raise
    /// `TypeError` as the spec requires.
    fn get_member(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        match obj {
            Value::Object(obj_ref) => {
                // A proxy's `get` trap is JavaScript, so it has to run *before*
                // the descriptor search — and only the VM can run it. No trap
                // (or no `get` on the handler) falls through, and the search
                // below then answers the target's property: `ProxyObject`
                // forwards `property_get` to the target.
                if obj_ref.borrow().kind() == ObjectKind::Proxy {
                    let proxy_val = Value::Object(Rc::clone(obj_ref));
                    if let Some(value) = self.proxy_get(&proxy_val, key, &proxy_val, module)? {
                        return Ok(value);
                    }
                }
                // Accessor properties have to invoke their getter, which needs a
                // re-entrant call — hence the descriptor search here.
                if let Some((owner, desc)) =
                    crate::vm::prototype::find_descriptor(Rc::clone(obj_ref), key)
                        .map_err(RuntimeError::TypeError)?
                {
                    if desc.is_accessor_descriptor() {
                        // `this` is the *receiver* (the object the property was
                        // read from), not the object that happens to hold the
                        // descriptor — class accessors live on the prototype.
                        let receiver = Value::Object(Rc::clone(obj_ref));
                        let _ = owner;
                        return match desc.invoked_getter() {
                            Some(getter) => self.invoke(getter, receiver, &[], module),
                            None => Ok(Value::Undefined),
                        };
                    }
                    return Ok(desc.value);
                }
                Ok(Value::Undefined)
            }
            Value::Function(id) => {
                let boxed = self.materialize_function(*id);
                self.get_member(&boxed, key, module)
            }
            Value::String(s) => {
                let name = key.as_str().unwrap_or("");
                if name == "length" {
                    return Ok(Value::Number(s.chars().count() as f64));
                }
                if let Ok(idx) = name.parse::<usize>() {
                    return Ok(s
                        .chars()
                        .nth(idx)
                        .map(|c| Value::string(&c.to_string()))
                        .unwrap_or(Value::Undefined));
                }
                let proto = self.primitive_prototype(obj);
                self.get_from_prototype(obj, &proto, key, module)
            }
            Value::Number(_) | Value::Bool(_) | Value::Symbol(_) => {
                let proto = self.primitive_prototype(obj);
                self.get_from_prototype(obj, &proto, key, module)
            }
            Value::Undefined => Err(RuntimeError::TypeError(format!(
                "Cannot read properties of undefined (reading '{}')",
                key.display()
            ))),
            Value::Null => Err(RuntimeError::TypeError(format!(
                "Cannot read properties of null (reading '{}')",
                key.display()
            ))),
        }
    }

    /// [[Get]] on a primitive receiver: the property lives on the wrapper's
    /// prototype, and an accessor has to run with the *primitive* as `this`
    /// (`Symbol.prototype.description`, `"x".length` is handled by the caller).
    fn get_from_prototype(
        &mut self,
        receiver: &Value,
        proto: &Option<Rc<RefCell<dyn JSObject>>>,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let Some(proto_ref) = proto else {
            return Ok(Value::Undefined);
        };
        if let Some((_owner, desc)) =
            crate::vm::prototype::find_descriptor(Rc::clone(proto_ref), key)
                .map_err(RuntimeError::TypeError)?
        {
            if desc.is_accessor_descriptor() {
                return match desc.invoked_getter() {
                    Some(getter) => self.invoke(getter, receiver.clone(), &[], module),
                    None => Ok(Value::Undefined),
                };
            }
            return Ok(desc.value);
        }
        Ok(Value::Undefined)
    }

    /// ES `[[Get]]` with an explicit **receiver** (ES 10.1.8).
    ///
    /// The receiver is the `this` an accessor is invoked with, and it travels
    /// unchanged up the prototype chain. Only `Reflect.get`'s third argument can
    /// make it differ from the object being read — every ordinary property read
    /// has the object itself as receiver, which is what `get_member` does.
    fn get_with_receiver(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
        receiver: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let Value::Object(obj_ref) = obj else {
            return self.get_member(obj, key, module);
        };
        if obj_ref.borrow().kind() == ObjectKind::Proxy {
            let proxy_val = Value::Object(Rc::clone(obj_ref));
            if let Some(value) = self.proxy_get(&proxy_val, key, receiver, module)? {
                return Ok(value);
            }
        }
        if let Some((_owner, desc)) = crate::vm::prototype::find_descriptor(Rc::clone(obj_ref), key)
            .map_err(RuntimeError::TypeError)?
        {
            if desc.is_accessor_descriptor() {
                return match desc.invoked_getter() {
                    Some(getter) => self.invoke(getter, receiver.clone(), &[], module),
                    None => Ok(Value::Undefined),
                };
            }
            return Ok(desc.value);
        }
        Ok(Value::Undefined)
    }

    /// ES `[[Set]]` with an explicit **receiver** (ES 10.1.9), answering whether
    /// the write succeeded instead of throwing.
    ///
    /// `Reflect.set` needs the boolean: a non-writable property, an accessor
    /// without a setter, or a non-object receiver are all "returns false", not
    /// errors. An assignment expression is the same operation with the object as
    /// receiver and a TypeError on failure — that is `set_member`.
    ///
    /// The receiver is where the property *lands* when the target only has a
    /// writable data property to offer: `Reflect.set(t, k, v, r)` writes to `r`.
    fn set_with_receiver(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
        value: &Value,
        receiver: &Value,
        module: &Module,
    ) -> Result<bool, RuntimeError> {
        let Value::Object(obj_ref) = obj else {
            return Err(RuntimeError::TypeError(
                "Reflect.set: target is not an object".to_string(),
            ));
        };
        // A `set` trap answers for the whole operation, receiver included.
        if obj_ref.borrow().kind() == ObjectKind::Proxy {
            let proxy_val = Value::Object(Rc::clone(obj_ref));
            return self.proxy_set(&proxy_val, key, value, receiver, module);
        }
        let own = obj_ref.borrow().property_get(key);
        let own = match own {
            Some(desc) => desc,
            None => {
                // Not an own property: the prototype gets the same request, with
                // the *same* receiver (ES 10.1.9 step 4).
                let parent = obj_ref.borrow().get_prototype();
                return match parent {
                    Some(parent) => {
                        self.set_with_receiver(&Value::Object(parent), key, value, receiver, module)
                    }
                    // Step 5: no parent, so the property is created — on the
                    // receiver — with every attribute `true`.
                    None => Ok(Self::create_data_property(receiver, key, value)),
                };
            }
        };
        if own.is_data_descriptor() {
            if !own.writable {
                return Ok(false);
            }
            let Value::Object(recv_ref) = receiver else {
                return Ok(false);
            };
            // Step 11: an existing property of the receiver that cannot be
            // overwritten makes the whole operation fail.
            if let Some(existing) = recv_ref.borrow().property_get(key) {
                if existing.is_accessor_descriptor() || !existing.writable {
                    return Ok(false);
                }
            }
            return Ok(Self::create_data_property(receiver, key, value));
        }
        match own.invoked_setter() {
            Some(setter) => {
                self.invoke(setter, receiver.clone(), &[value.clone()], module)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// `CreateDataProperty` (ES 7.3.5): `[[DefineOwnProperty]]` of a fresh
    /// all-true data descriptor, answering whether it landed.
    fn create_data_property(obj: &Value, key: &PropertyKey, value: &Value) -> bool {
        match obj {
            Value::Object(obj_ref) => obj_ref
                .borrow_mut()
                .define_property(
                    key.clone(),
                    crate::vm::property::PropertyDescriptor::data_descriptor(value.clone()),
                )
                .unwrap_or(false),
            _ => false,
        }
    }

    fn lookup_on_prototype(
        &self,
        proto: &Option<Rc<RefCell<dyn JSObject>>>,
        key: &PropertyKey,
    ) -> Result<Value, RuntimeError> {
        match proto {
            Some(p) => crate::vm::prototype::internal_get(Rc::clone(p), key, None)
                .map_err(RuntimeError::TypeError),
            None => Ok(Value::Undefined),
        }
    }

    /// ES `[[Set]]` over any JS value.
    fn set_member(
        &mut self,
        obj: &Value,
        key: PropertyKey,
        value: Value,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        match obj {
            Value::Object(obj_ref) => {
                // A `set` trap replaces the whole assignment: it decides both
                // the value and whether the write succeeded (ES 10.5.9).
                if obj_ref.borrow().kind() == ObjectKind::Proxy {
                    let proxy_val = Value::Object(Rc::clone(obj_ref));
                    if self.proxy_set(&proxy_val, &key, &value, &proxy_val, module)? {
                        return Ok(());
                    }
                    // A trap that answered falsish is a failed assignment,
                    // which strict mode reports as a TypeError.
                    return Err(RuntimeError::TypeError(format!(
                        "'set' on proxy: trap returned falsish for property '{}'",
                        key.display()
                    )));
                }
                // An accessor anywhere on the chain intercepts the assignment
                // and routes it to the setter (which needs a re-entrant call).
                if let Some((owner, desc)) =
                    crate::vm::prototype::find_descriptor(Rc::clone(obj_ref), &key)
                        .map_err(RuntimeError::TypeError)?
                {
                    if desc.is_accessor_descriptor() {
                        // Same receiver rule as `get_member` above.
                        let receiver = Value::Object(Rc::clone(obj_ref));
                        let _ = owner;
                        return match desc.invoked_setter() {
                            Some(setter) => {
                                self.invoke(setter, receiver, &[value], module)?;
                                Ok(())
                            }
                            // G1 (README target): the engine has no sloppy
                            // mode, so an assignment that nothing can satisfy
                            // is a TypeError rather than a silent no-op.
                            None => Err(RuntimeError::TypeError(
                                "Cannot set property which has only a getter".to_string(),
                            )),
                        };
                    }
                    if !desc.writable {
                        return Err(RuntimeError::TypeError(format!(
                            "Cannot assign to read-only property '{}'",
                            key.display()
                        )));
                    }
                    let mut borrowed = obj_ref.borrow_mut();
                    borrowed
                        .property_set(key, value)
                        .map_err(RuntimeError::from_property_error)?;
                    return Ok(());
                }
                let mut borrowed = obj_ref.borrow_mut();
                borrowed
                    .property_set(key, value)
                    .map_err(RuntimeError::from_property_error)?;
                Ok(())
            }
            Value::Function(id) => {
                let boxed = self.materialize_function(*id);
                self.set_member(&boxed, key, value, module)
            }
            // Assigning to a property of a primitive has no receiver to hold
            // the property, which strict mode reports as a TypeError (the
            // sloppy-mode silence is not reachable: cf. G1 in the README).
            _ => Err(RuntimeError::TypeError(format!(
                "Cannot create property '{}' on {}",
                key.display(),
                match obj {
                    Value::String(_) => "string",
                    Value::Number(_) => "number",
                    Value::Bool(_) => "boolean",
                    Value::Symbol(_) => "symbol",
                    _ => "null or undefined",
                }
            ))),
        }
    }

    /// ES `[[Delete]]` on an object. Returns whether the property was removed.
    fn delete_member(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<bool, RuntimeError> {
        match obj {
            Value::Object(obj_ref) if obj_ref.borrow().kind() == ObjectKind::Proxy => {
                // A `deleteProperty` trap answers for the whole operation
                // (ES 10.5.10); with no trap the delete is the target's.
                let proxy_val = Value::Object(Rc::clone(obj_ref));
                match self.proxy_delete(&proxy_val, key, module)? {
                    Some(removed) => Ok(removed),
                    None => crate::vm::prototype::internal_delete(Rc::clone(obj_ref), key)
                        .map_err(RuntimeError::TypeError),
                }
            }
            Value::Object(obj_ref) => {
                crate::vm::prototype::internal_delete(Rc::clone(obj_ref), key)
                    .map_err(RuntimeError::TypeError)
            }
            Value::Function(id) => {
                let boxed = self.materialize_function(*id);
                self.delete_member(&boxed, key, module)
            }
            _ => Err(RuntimeError::TypeError(
                "Cannot delete property of a non-object".to_string(),
            )),
        }
    }

    /// Look up a property on an object via its prototype chain.
    /// `&mut self` because a proxy's `get` trap is JavaScript: `obj.m()` has to
    /// read `m` through the trap just like `obj.m` does.
    fn lookup_property_on_object(
        &mut self,
        obj: &Value,
        prop_name: &str,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        match obj {
            Value::Object(obj_ref) => {
                let key = PropertyKey::from_str(prop_name);
                if obj_ref.borrow().kind() == ObjectKind::Proxy {
                    let proxy_val = Value::Object(Rc::clone(obj_ref));
                    if let Some(value) = self.proxy_get(&proxy_val, &key, &proxy_val, module)? {
                        return Ok(value);
                    }
                }
                crate::vm::prototype::internal_get(obj_ref.clone(), &key, None)
                    .map_err(RuntimeError::TypeError)
            }
            Value::String(s) => {
                if prop_name == "length" {
                    Ok(Value::Number(s.len() as f64))
                } else {
                    Ok(Value::Undefined)
                }
            }
            _ => Ok(Value::Undefined),
        }
    }

    /// Call a built-in method dispatched via CallMethod opcode.
    ///
    /// Looks up the method through the prototype chain and executes
    /// known methods (toString, valueOf, etc.) directly.
    // ─────────────────────────────────────────────────────
    // Generators (ES 25.4) — M4-G1 subset
    // ─────────────────────────────────────────────────────
    //
    // A generator body runs on the ordinary value stack, so suspending it means
    // copying its frame slice out and resuming means copying it back: no second
    // frame representation, and no change to how the body is compiled. The
    // nested `while self.step(module)?` loop already used by `invoke` provides
    // the "run until this frame finishes" seam; `Opcode::Yield` stops it by
    // jumping past the last instruction, exactly like a `Ret` to the sentinel
    // return address does.

    /// Build the generator object a call to `function*` produces.
    ///
    /// The body does not run yet: it starts on the first `next()`.
    pub fn create_generator(
        &mut self,
        func_id: u32,
        this: Value,
        args: Vec<Value>,
        captured_vars: Vec<(String, Value)>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let id = self.next_generator_id;
        self.next_generator_id += 1;
        // The prologue below runs *after* the generator object has taken
        // ownership of the arguments, and the object's copies are the ones every
        // later resume rebuilds the frame from.
        let prologue_this = this.clone();
        let prologue_args = args.clone();
        let prologue_captured = captured_vars.clone();
        let mut gobj = crate::vm::object::GeneratorObject::new(id, func_id, args, this);
        gobj.captured_vars = captured_vars;
        // ES 14.4.11 / 9.1.13: the instance's prototype comes from
        // `GetPrototypeFromConstructor(functionObject, "%GeneratorPrototype%")`,
        // i.e. the constructor's own `prototype` property when it is an object.
        let func_obj = self.materialize_function(func_id);
        if let Value::Object(func_ref) = &func_obj {
            if let Some(desc) = func_ref.borrow().property_get(&PropertyKey::from_str("prototype")) {
                if let Value::Object(proto) = desc.value {
                    gobj.set_prototype(Some(proto));
                }
            }
        }
        let value = Value::Object(Rc::new(RefCell::new(gobj)));
        self.generator_registry.insert(id, value.clone());

        // ES 9.2.12 `FunctionDeclarationInstantiation` runs when the generator is
        // *called*: `function* f([[x]]) {}` must throw `TypeError` out of
        // `f([null])`, and a throwing default must reach the caller, not the first
        // `next()`. Only the parameter prologue runs here — the lowering puts an
        // `Opcode::PrologueEnd` barrier at its end and this parks the frame
        // there. Running the *body* eagerly would be both wrong and a hazard:
        // its first instruction may be a `yield` the caller should never see,
        // and a body that iterates re-enters this function recursively.
        self.run_generator_prologue(
            &value,
            func_id,
            &prologue_this,
            &prologue_args,
            &prologue_captured,
            module,
        )?;
        Ok(value)
    }

    /// Push a generator frame onto the stacks, positioned at its entry point.
    ///
    /// Shared by the eager parameter prologue (`create_generator`) and by the
    /// first `next()`: the two have to agree on the frame layout down to the
    /// control-stack trio, or a parked frame would resume into a different shape
    /// than the one it was built with.
    fn push_generator_frame(
        &mut self,
        func_id: u32,
        this: &Value,
        args: &[Value],
        captured_vars: &[(String, Value)],
        module: &Module,
    ) -> Result<(), RuntimeError> {
        let Some(location) = module.symtab.get(&FunctionId::new(func_id)).copied() else {
            return Err(RuntimeError::ReferenceError(format!(
                "undefined function: {func_id}"
            )));
        };
        for arg in args.iter().rev() {
            self.state.push(arg.clone())?;
        }
        self.state.rbp = self.state.rsp;
        self.state.enter_frame(args.len())?;
        self.state.this_val = this.clone();
        self.state.function_val = self.materialize_function(func_id);
        for (name, value) in captured_vars {
            let mut map = std::collections::HashMap::new();
            map.insert(name.clone(), value.clone());
            self.state.closure_var_stack.push(map);
        }
        self.state.pushc(self.state.closure_var_stack.len())?;
        self.state.pushc(self.state.seh_stack.len())?;
        self.state.pushc(self.resume_sentinel(module))?;
        self.state.construct_stack.push(false);
        self.state.new_target_stack.push(Value::Undefined);
        self.state.jump(location);
        Ok(())
    }

    /// Bind a freshly created generator's parameters, then park its frame.
    ///
    /// An error from the parameter prologue is returned to the *caller* — that
    /// is the whole point of running it here (`function* f([[x]]) {}` called as
    /// `f([null])` must throw, per ES 9.2.12).
    fn run_generator_prologue(
        &mut self,
        gen_value: &Value,
        func_id: u32,
        this: &Value,
        args: &[Value],
        captured_vars: &[(String, Value)],
        module: &Module,
    ) -> Result<(), RuntimeError> {
        let saved = self.save_execution_state();
        // Same depth bookkeeping as `generator_next`: the frame's own open
        // delegates and closure maps are what the snapshot has to carry.
        let delegate_depth = self.delegate_stack.len();
        self.push_generator_frame(func_id, this, args, captured_vars, module)?;
        let closure_depth = self.state.closure_var_stack.len();
        // Save and *restore*: a parameter default may create another generator
        // (`function* g() {…}` called as `[[,] = g()]`), and that nested prologue
        // used to clear this flag on its way out. The outer method's `PrologueEnd`
        // then found it `false` and did nothing — the whole body ran *during*
        // creation, and the first `next()` ran it a second time
        // (`class C { static *m([[,] = g()]) { cc += 1; } }` reported `cc=2`).
        let outer_prologue_flag = self.running_generator_prologue;
        self.running_generator_prologue = true;
        let run = self.run_generator_frame(&saved, closure_depth, delegate_depth, module);
        self.running_generator_prologue = outer_prologue_flag;
        // A parameter binding that threw leaves the generator finished: without
        // this, a later `next()` on it panicked (see
        // `run_generator_frame_or_complete`).
        let run = match run {
            Ok(run) => run,
            Err(err) => {
                self.mark_generator_completed(gen_value);
                return Err(err);
            }
        };
        // Parked at the barrier: the generator is still `suspendedStart` (its
        // body has not run) but owns a frame whose pc sits just past the
        // prologue, so `next()` continues there instead of rebuilding it.
        if let Some(frame) = run.frame {
            self.store_suspended_generator(
                gen_value,
                frame,
                crate::vm::object::GeneratorState::SuspendedStart,
            );
        }
        Ok(())
    }

    /// `gen.next([v])`: resume the body until the next `yield` or the return,
    /// and report `{ value, done }`.
    pub fn generator_next(
        &mut self,
        id: u64,
        send: Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // Saved before anything is pushed, exactly as `invoke` does: the
        // generator frame must not survive the resume.
        let saved = self.save_execution_state();
        // Captured before the frame is set up: the resume path puts the frame's
        // own open delegates back, and those are *this* frame's, not the
        // resumer's.
        let delegate_depth = self.delegate_stack.len();
        let gen_value = match self.generator_registry.get(&id).cloned() {
            Some(v) => v,
            None => {
                return Err(RuntimeError::TypeError(
                    "generator is no longer alive".to_string(),
                ))
            }
        };
        let state = {
            let Value::Object(obj_ref) = &gen_value else {
                return Err(RuntimeError::TypeError("not a generator".to_string()));
            };
            let borrowed = obj_ref.borrow();
            let Some(gobj) = borrowed
                .as_any()
                .downcast_ref::<crate::vm::object::GeneratorObject>()
            else {
                return Err(RuntimeError::TypeError("not a generator".to_string()));
            };
            gobj.state
        };
        if state == crate::vm::object::GeneratorState::Completed {
            return Ok(Self::iterator_result(Value::Undefined, true));
        }

        if state == crate::vm::object::GeneratorState::SuspendedStart {
            // A generator whose *parameter* prologue already ran (at call time,
            // see `create_generator`) parks with a frame in hand: the body still
            // has not started, so this is still the first resume — but the frame
            // exists and `next()` continues right after the barrier instead of
            // building it again.
            let parked = {
                let Value::Object(obj_ref) = &gen_value else {
                    return Err(RuntimeError::TypeError("not a generator".to_string()));
                };
                let mut gobj = obj_ref.borrow_mut();
                match gobj
                    .as_any_mut()
                    .downcast_mut::<crate::vm::object::GeneratorObject>()
                {
                    Some(gobj) if gobj.suspended.is_some() => {
                        gobj.state = crate::vm::object::GeneratorState::Executing;
                        gobj.suspended.take()
                    }
                    _ => None,
                }
            };
            if let Some(frame) = parked {
                let resume_pc = frame.pc;
                self.restore_generator_frame(&frame, module)?;
                // No value to deliver: nothing is waiting for `next(v)`'s
                // argument, because the barrier is not a `yield`.
                self.state.jump(resume_pc + 1);
                let closure_depth = self.state.closure_var_stack.len();
                let run = self.run_generator_frame_or_complete(
                    &saved,
                    closure_depth,
                    delegate_depth,
                    &gen_value,
                    module,
                )?;
                return Ok(self.finish_generator(&gen_value, run));
            }
            // First resume: open a frame exactly like `invoke` does.
            let (func_id, args, this, captured_vars) = {
                let Value::Object(obj_ref) = &gen_value else {
                    return Err(RuntimeError::TypeError("not a generator".to_string()));
                };
                let gobj = obj_ref.borrow();
                let gobj = gobj
                    .as_any()
                    .downcast_ref::<crate::vm::object::GeneratorObject>()
                    .unwrap();
                match module.symtab.get(&FunctionId::new(gobj.func_id)) {
                    Some(_) => (gobj.func_id, gobj.args.clone(), gobj.this.clone(), gobj.captured_vars.clone()),
                    None => {
                        return Err(RuntimeError::ReferenceError(format!(
                            "undefined function: {}",
                            gobj.func_id
                        )))
                    }
                }
            };
            self.push_generator_frame(func_id, &this, &args, &captured_vars, module)?;
        } else {
            // Resuming at a `yield`: put the frame slice back and hand the
            // sent value to the `Yield` that is waiting for it.
            let frame = {
                let Value::Object(obj_ref) = &gen_value else {
                    return Err(RuntimeError::TypeError("not a generator".to_string()));
                };
                let mut gobj = obj_ref.borrow_mut();
                let gobj = gobj
                    .as_any_mut()
                    .downcast_mut::<crate::vm::object::GeneratorObject>()
                    .unwrap();
                gobj.state = crate::vm::object::GeneratorState::Executing;
                match gobj.suspended.take() {
                    Some(frame) => frame,
                    None => {
                        // Unreachable through the spec's own state machine, but a
                        // panic here aborts the entire run — keep it local to the
                        // test instead. (`next()` on a *completed* generator is
                        // answered above; this one is running.)
                        gobj.state = crate::vm::object::GeneratorState::Completed;
                        drop(gobj);
                        return Err(RuntimeError::TypeError(
                            "generator is already running".to_string(),
                        ));
                    }
                }
            };
            let resume_pc = frame.pc;
            self.restore_generator_frame(&frame, module)?;
            // `yield expr` evaluates to the value the resumer supplied. The
            // instruction itself does not run again, so the destination is read
            // from the bytecode and written here, with the restored `rbp`.
            if let Some(inst) = module.instructions.get(resume_pc) {
                // 命名字段解构：目的寄存器是 `Yield` 声明的第一个操作数，
                // 不再依赖"槽位 0"这个位置约定。
                if let Instr::Yield { dst, .. } = inst {
                    self.set_value(*dst, send)?;
                }
            }
            self.state.jump(resume_pc + 1);
        }

        let closure_depth = self.state.closure_var_stack.len();
        let run = self.run_generator_frame_or_complete(
            &saved,
            closure_depth,
            delegate_depth,
            &gen_value,
            module,
        )?;
        Ok(self.finish_generator(&gen_value, run))
    }

    /// `gen.return(v)` / `gen.throw(e)`: complete the generator with the given
    /// completion instead of resuming it normally (ES 25.4.3.4 / 25.4.3.5).
    ///
    /// The body *is* resumed, which is the whole point:
    ///
    /// * a **return** completion re-enters the frame at the function's trailing
    ///   `Ret` (`Module::exit_pc`). That is what makes `Opcode::Ret`'s `finally`
    ///   dispatch fire, and it is why the body cannot simply be abandoned — a
    ///   `finally` that pushes to a log has to run;
    /// * a **throw** completion is handed to the resumed frame's SEH machinery,
    ///   so it lands on the `yield` exactly as if the `yield` had thrown and a
    ///   surrounding `try` can catch it.
    ///
    /// A parked `yield*` intercepts the completion first (ES 14.4.14 step 5):
    /// `return(v)` closes the delegate, `throw(e)` calls its `throw` method.
    fn generator_abrupt(
        &mut self,
        id: u64,
        completion: Result<Value, RuntimeError>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let saved = self.save_execution_state();
        let delegate_depth = self.delegate_stack.len();
        let gen_value = match self.generator_registry.get(&id).cloned() {
            Some(v) => v,
            None => {
                return Err(RuntimeError::TypeError(
                    "generator is no longer alive".to_string(),
                ))
            }
        };

        let (state, func_id, frame) = {
            let Value::Object(obj_ref) = &gen_value else {
                return Err(RuntimeError::TypeError("not a generator".to_string()));
            };
            let mut borrowed = obj_ref.borrow_mut();
            let Some(gobj) = borrowed
                .as_any_mut()
                .downcast_mut::<crate::vm::object::GeneratorObject>()
            else {
                return Err(RuntimeError::TypeError("not a generator".to_string()));
            };
            let state = gobj.state;
            let func_id = gobj.func_id;
            let frame = if state == crate::vm::object::GeneratorState::SuspendedYield {
                gobj.state = crate::vm::object::GeneratorState::Executing;
                gobj.suspended.take()
            } else {
                None
            };
            (state, func_id, frame)
        };

        // A generator that never started, or already finished, has no frame to
        // unwind: the completion is simply what the caller sees.
        let Some(mut frame) = frame else {
            self.mark_generator_completed(&gen_value);
            return match completion {
                Ok(value) => Ok(Self::iterator_result(value, true)),
                Err(err) => Err(err),
            };
        };

        match completion {
            Ok(value) => {
                // The delegates are taken out of the frame so the resume path
                // does not push them back onto `delegate_stack`; they are closed
                // by hand just below.
                let delegates = std::mem::take(&mut frame.delegates);
                self.restore_generator_frame(&frame, module)?;
                // ES 14.4.14 step 5.c `IteratorClose` runs *inside* the
                // generator's resumption, so an abrupt completion from the
                // delegate's `return` (or from the "result must be an object"
                // check) is delivered to the body: its own `try` catches it
                // (`star-rhs-iter-rtrn-rtrn-call-err.js`).
                let close = self.close_delegates(&delegates, &value, module);
                let start_pc = match close {
                    Ok(()) => {
                        self.state.set_register(Register::Rv, value.clone())?;
                        // The function's trailing `Ret` is what makes
                        // `Opcode::Ret`'s `finally` dispatch run on the way out.
                        // `None` only for a hand-built module without any `Ret`.
                        module.exit_pc.get(&func_id).copied()
                    }
                    Err(err) => match self.deliver_into_frame(err, saved.ctrl) {
                        Ok(()) => None,
                        Err(esc) => {
                            self.discard_generator_frame();
                            self.restore_execution_state(&saved);
                            self.mark_generator_completed(&gen_value);
                            return Err(esc);
                        }
                    },
                };
                if let Some(pc) = start_pc {
                    self.state.jump(pc);
                }
                let closure_depth = self.state.closure_var_stack.len();
                let run = self.run_generator_frame_or_complete(
                    &saved,
                    closure_depth,
                    delegate_depth,
                    &gen_value,
                    module,
                )?;
                match run.yielded {
                    // A `finally` that yields keeps the generator alive.
                    Some(_) => Ok(self.finish_generator(&gen_value, run)),
                    // Otherwise the completion's value is what the caller gets.
                    // `run.rv` cannot be used: the function's exit block opens
                    // with the implicit `Mov rv, undefined`, so a `finally` that
                    // merely runs would report `undefined` (see §2.1x).
                    None => {
                        self.mark_generator_completed(&gen_value);
                        Ok(Self::iterator_result(value, true))
                    }
                }
            }
            Err(RuntimeError::Thrown(reason)) => {
                // Step 5.b: a delegate with its own `throw` consumes the
                // completion and the generator keeps going from there.
                if let Some(iter) = frame.delegates.last().cloned() {
                    let iter = self.delegate_js_iterator(&iter).unwrap_or(iter);
                    let throw_method =
                        self.get_member(&iter, &PropertyKey::from_str("throw"), module)?;
                    if throw_method.is_callable() {
                        let inner = self.invoke(&throw_method, iter, &[reason], module)?;
                        if !inner.is_object() {
                            self.mark_generator_completed(&gen_value);
                            return Err(RuntimeError::TypeError(
                                "iterator.throw() returned a non-object value".to_string(),
                            ));
                        }
                        let done = self
                            .get_member(&inner, &PropertyKey::from_str("done"), module)?
                            .to_boolean();
                        let value =
                            self.get_member(&inner, &PropertyKey::from_str("value"), module)?;
                        if !done {
                            // The delegate produced another value: the generator
                            // stays parked on it, right where it was.
                            self.store_suspended_generator(
                                &gen_value,
                                frame,
                                crate::vm::object::GeneratorState::SuspendedYield,
                            );
                            return Ok(Self::iterator_result(value, false));
                        }
                        self.mark_generator_completed(&gen_value);
                        return Ok(Self::iterator_result(value, true));
                    }
                }
                // No delegate `throw`: the completion lands on the `yield`
                // itself, so the frame's own `try`/`finally` gets its chance.
                self.restore_generator_frame(&frame, module)?;
                match self.handle_throw(reason) {
                    Ok(()) => {
                        let closure_depth = self.state.closure_var_stack.len();
                        let run = self.run_generator_frame_or_complete(
                            &saved,
                            closure_depth,
                            delegate_depth,
                            &gen_value,
                            module,
                        )?;
                        Ok(self.finish_generator(&gen_value, run))
                    }
                    Err(err) => {
                        // Nothing in the body caught it: the generator is over
                        // and the exception belongs to the caller.
                        self.discard_generator_frame();
                        self.restore_execution_state(&saved);
                        self.mark_generator_completed(&gen_value);
                        Err(err)
                    }
                }
            }
            Err(other) => Err(other),
        }
    }

    /// The bytecode function this frame belongs to, when it is known.
    ///
    /// `function_val` is the callee object (`FunctionObject`, or a bare
    /// `Value::Function` reference before materialization).
    fn current_function_id(&self) -> Option<u32> {
        match &self.state.function_val {
            Value::Function(id) => Some(*id),
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .as_any()
                .downcast_ref::<crate::vm::object::FunctionObject>()
                .map(|f| f.func_id),
            _ => None,
        }
    }

    /// Hand an error to the *enclosing generator frame's* handlers.
    ///
    /// `boundary` is the resumer's control-stack depth: the frame's own handlers
    /// sit above it and win, anything below is left to the caller (returned as
    /// `Err`), which is what keeps the nested loop's bookkeeping sound.
    fn deliver_into_frame(&mut self, err: RuntimeError, boundary: usize) -> Result<(), RuntimeError> {
        let Some(exc) = self.as_js_exception(&err) else {
            return Err(err);
        };
        self.invoke_boundaries.push(boundary);
        let dispatched = self.handle_throw(exc);
        self.invoke_boundaries.pop();
        dispatched
    }

        /// Drive the generator frame that is currently installed until it yields or
    /// returns, then take the frame down and restore the resumer's context.
    ///
    /// On `Err` the caller's context has already been restored.
    fn run_generator_frame(
        &mut self,
        saved: &SavedExecutionState,
        closure_depth: usize,
        delegate_depth: usize,
        module: &Module,
    ) -> Result<GeneratorRunOutcome, RuntimeError> {
        // A generator body is a Rust-driven frame, exactly like the callee of
        // `invoke_with_new_target`: an exception that reaches the frame's
        // bottom must come back as `Err(Thrown(_))` so the *resumer* dispatches
        // it. Letting it jump straight to a handler outside the frame would
        // unwind control-stack entries the nested loop still has to pop.
        self.invoke_boundaries.push(saved.ctrl);
        self.generator_yielded = None;
        let outcome = (|| -> Result<(), RuntimeError> {
            while self.step(module)? {}
            Ok(())
        })();
        self.invoke_boundaries.pop();

        // A `yield` left the frame on the value stack: lift it out before the
        // resumer's context is restored (which would rewind `rsp` below it).
        let yielded = self.generator_yielded.take();
        let frame = if yielded.is_some() {
            Some(self.extract_generator_frame(closure_depth, delegate_depth, saved.seh))
        } else {
            None
        };
        let rv = self.state.get_register(Register::Rv)?;
        if frame.is_some() {
            self.discard_generator_frame();
        }
        self.restore_execution_state(saved);
        outcome?;
        Ok(GeneratorRunOutcome { yielded, frame, rv })
    }

    /// Unwind the bookkeeping of a frame that suspended: `Ret` never ran, so its
    /// control entries and `function_stack` entry are still there. The value
    /// stack and the other stacks are rewound by `restore_execution_state`.
    fn discard_generator_frame(&mut self) {
        let _ = self.state.popc();
        let _ = self.state.popc();
        let _ = self.state.popc();
        self.state.function_stack.pop();
    }

    /// Report what a finished run means for the generator object.
    /// Run a generator frame, marking the generator **completed** if the body
    /// lets an exception escape.
    ///
    /// The `?` alone is not enough: a generator whose body threw is finished for
    /// good, and leaving it in `Executing` made the *next* `next()` fall into the
    /// "resume at a yield" path with no suspended frame — which hit
    /// `expect("SuspendedYield without a frame")` and **aborted the whole run**
    /// instead of failing one test.
    fn run_generator_frame_or_complete(
        &mut self,
        saved: &SavedExecutionState,
        closure_depth: usize,
        delegate_depth: usize,
        gen_value: &Value,
        module: &Module,
    ) -> Result<GeneratorRunOutcome, RuntimeError> {
        match self.run_generator_frame(saved, closure_depth, delegate_depth, module) {
            Ok(run) => Ok(run),
            Err(err) => {
                self.mark_generator_completed(gen_value);
                Err(err)
            }
        }
    }

    fn finish_generator(
        &mut self,
        gen_value: &Value,
        run: GeneratorRunOutcome,
    ) -> Value {
        match (run.yielded, run.frame) {
            (Some(value), Some(frame)) => {
                self.store_suspended_generator(
                    gen_value,
                    frame,
                    crate::vm::object::GeneratorState::SuspendedYield,
                );
                Self::iterator_result(value, false)
            }
            _ => {
                self.mark_generator_completed(gen_value);
                Self::iterator_result(run.rv, true)
            }
        }
    }

    /// Park a generator frame.
    ///
    /// `state` is `SuspendedYield` after a `yield` and `SuspendedStart` when the
    /// frame stopped at the parameter-prologue barrier: both own a resumable
    /// frame (`next()` continues at `frame.pc + 1`), but only the former reports
    /// a value.
    fn store_suspended_generator(
        &mut self,
        gen_value: &Value,
        frame: crate::vm::object::SuspendedFrame,
        state: crate::vm::object::GeneratorState,
    ) {
        if let Value::Object(obj_ref) = gen_value {
            let mut gobj = obj_ref.borrow_mut();
            if let Some(gobj) = gobj
                .as_any_mut()
                .downcast_mut::<crate::vm::object::GeneratorObject>()
            {
                gobj.state = state;
                gobj.suspended = Some(frame);
            }
        }
    }

    fn mark_generator_completed(&mut self, gen_value: &Value) {
        if let Value::Object(obj_ref) = gen_value {
            let mut gobj = obj_ref.borrow_mut();
            if let Some(gobj) = gobj
                .as_any_mut()
                .downcast_mut::<crate::vm::object::GeneratorObject>()
            {
                gobj.state = crate::vm::object::GeneratorState::Completed;
                gobj.suspended = None;
            }
        }
    }

    /// The JS iterator behind a `MakeIterator` wrapper, when there is one.
    fn delegate_js_iterator(&self, iter: &Value) -> Option<Value> {
        let Value::Object(obj_ref) = iter else {
            return None;
        };
        let borrowed = obj_ref.borrow();
        let it = borrowed
            .as_any()
            .downcast_ref::<crate::vm::iterator::NativeIteratorObject>()?;
        match &*it.state().borrow() {
            crate::vm::iterator::NativeIteratorState::Js { iterator } => Some(iterator.clone()),
            _ => None,
        }
    }

    /// `IteratorClose` for every iterator a `yield*` left open, innermost first.
    ///
    /// `GetMethod(iterator, "return")` — absent means nothing to do — then the
    /// call, then the "result must be an object" check (ES 7.4.6).
    fn close_delegates(
        &mut self,
        delegates: &[Value],
        value: &Value,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        for iter in delegates.iter().rev() {
            // Unwrap the `MakeIterator` wrapper: its native `return` is
            // `IteratorClose`, which *swallows* whatever `return()` throws. A
            // `yield*` has to see it (`? Call(return, iterator, …)` in step
            // 5.c.iv — `star-rhs-iter-rtrn-rtrn-call-err.js`).
            let iter = self.delegate_js_iterator(iter).unwrap_or_else(|| iter.clone());
            let return_method = self.get_member(&iter, &PropertyKey::from_str("return"), module)?;
            if return_method.is_undefined() || return_method.is_null() {
                continue;
            }
            if !return_method.is_callable() {
                return Err(RuntimeError::TypeError(
                    "iterator.return is not a function".to_string(),
                ));
            }
            let result = self.invoke(&return_method, iter, &[value.clone()], module)?;
            if !result.is_object() {
                return Err(RuntimeError::TypeError(
                    "iterator.return() returned a non-object value".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Put a suspended generator frame back on the stacks.
    ///
    /// Everything except the program counter: `generator_next` continues after
    /// the `Yield`, an abrupt completion re-enters at the function's `Ret` (see
    /// `Module::exit_pc`).
    fn restore_generator_frame(
        &mut self,
        frame: &crate::vm::object::SuspendedFrame,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        let base = self.state.data_stack.len();
        self.delegate_stack.extend(frame.delegates.iter().cloned());
        self.state.data_stack.extend(frame.data.iter().cloned());
        self.state.rbp = base + frame.argc;
        self.state.rsp = self.state.rbp + frame.bp_offset;
        self.state.this_val = frame.this.clone();
        self.state.function_val = frame.function_val.clone();
        self.state.enter_frame(frame.argc)?;
        for map in &frame.closure_maps {
            self.state.closure_var_stack.push(map.clone());
        }
        // The body's live exception handlers come back with the frame; a
        // `finally` that had not run yet must still run.
        self.state.seh_stack.extend(frame.seh.iter().cloned());
        self.state.pushc(self.state.closure_var_stack.len())?;
        self.state.pushc(self.state.seh_stack.len())?;
        self.state.pushc(self.resume_sentinel(module))?;
        self.state.construct_stack.push(false);
        self.state.new_target_stack.push(Value::Undefined);
        // The register file is global, not per frame, so the body's in-flight
        // values have to come back before anything else runs.
        for (slot, value) in self.state.registers.iter_mut().zip(frame.registers.iter()) {
            *slot = value.clone();
        }
        Ok(())
    }

    /// One past the last instruction: `Ret` (and `Yield`) land here, which is
    /// what terminates the nested `step` loop.
    fn resume_sentinel(&self, module: &Module) -> usize {
        module.instructions.len()
    }

    /// `{ value, done }` — the shape every iterator result has.
    fn iterator_result(value: Value, done: bool) -> Value {
        let mut result = crate::vm::object::OrdinaryObject::new();
        let _ = result.property_set(PropertyKey::from_str("value"), value);
        let _ = result.property_set(PropertyKey::from_str("done"), Value::Bool(done));
        Value::Object(Rc::new(RefCell::new(result)))
    }

    /// Snapshot of everything a nested call must restore afterwards — the same
    /// inventory `invoke_with_new_target` saves.
    fn save_execution_state(&self) -> SavedExecutionState {
        SavedExecutionState {
            pc: self.state.pc,
            rsp: self.state.rsp,
            rbp: self.state.rbp,
            closure: self.state.closure_var_stack.len(),
            seh: self.state.seh_stack.len(),
            this: self.state.this_val.clone(),
            this_depth: self.state.this_stack.len(),
            construct: self.state.construct_stack.len(),
            new_target: self.state.new_target_stack.len(),
            function: self.state.function_val.clone(),
            ctrl: self.state.ctrl_stack.len(),
            delegate: self.delegate_stack.len(),
            registers: std::array::from_fn(|i| self.state.registers[i].clone()),
        }
    }

    fn restore_execution_state(&mut self, saved: &SavedExecutionState) {
        self.state.pc = saved.pc;
        self.state.rsp = saved.rsp;
        self.state.rbp = saved.rbp;
        self.state.closure_var_stack.truncate(saved.closure);
        self.state.seh_stack.truncate(saved.seh);
        self.state.this_stack.truncate(saved.this_depth);
        self.state.this_state.truncate(saved.this_depth);
        self.state.frame_argc.truncate(saved.this_depth);
        self.state.this_val = saved.this.clone();
        self.state.function_val = saved.function.clone();
        self.state.construct_stack.truncate(saved.construct);
        self.state.new_target_stack.truncate(saved.new_target);
        self.delegate_stack.truncate(saved.delegate);
        // The register file is *global*, not per frame: a nested run (a
        // generator body, an `invoke`) overwrites values the caller still has
        // in registers — an array literal built around `it.next()` is the
        // common victim.
        for (slot, value) in self.state.registers.iter_mut().zip(saved.registers.iter()) {
            *slot = value.clone();
        }
    }

    /// The prototype a `[[Construct]]` should give the new instance.
    fn prototype_from_constructor(
        &self,
        constructor_val: &Value,
        new_target: Option<&Value>,
    ) -> Option<Value> {
        if let Some(Value::Object(nt)) = new_target {
            if let Some(desc) = nt.borrow().property_get(&PropertyKey::from_str("prototype")) {
                if desc.value.is_object() {
                    return Some(desc.value);
                }
            }
        }
        match constructor_val {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .property_get(&PropertyKey::from_str("prototype"))
                .map(|d| d.value),
            _ => None,
        }
    }

    /// Lift the frame currently on top of the value stack out of it, so the
    /// stack can be rewound to the resumer's position.
    fn extract_generator_frame(
        &mut self,
        closure_depth: usize,
        delegate_depth: usize,
        seh_depth: usize,
    ) -> crate::vm::object::SuspendedFrame {
        let argc = self.state.frame_argc.last().copied().unwrap_or(0);
        let start = self.state.rbp.saturating_sub(argc);
        let end = self.state.rsp.min(self.state.data_stack.len());
        let data = if end > start {
            self.state.data_stack[start..end].to_vec()
        } else {
            Vec::new()
        };
        crate::vm::object::SuspendedFrame {
            data,
            argc,
            bp_offset: self.state.rsp.saturating_sub(self.state.rbp),
            pc: self.generator_yield_pc,
            this: self.state.this_val.clone(),
            function_val: self.state.function_val.clone(),
            closure_maps: self
                .state
                .closure_var_stack
                .get(closure_depth..)
                .unwrap_or_default()
                .to_vec(),
            registers: std::array::from_fn(|i| self.state.registers[i].clone()),
            delegates: self
                .delegate_stack
                .get(delegate_depth..)
                .unwrap_or_default()
                .to_vec(),
            seh: self
                .state
                .seh_stack
                .get(seh_depth..)
                .unwrap_or_default()
                .to_vec(),
        }
    }

    /// `GetIterator` (ES6 §7.4.1): produce the internal iterator for `src`.
    ///
    /// Fast path for arrays/strings/`arguments`; otherwise call
    /// `src[Symbol.iterator]()`.
    fn make_iterator(&mut self, src: Value, module: &Module) -> Result<Value, RuntimeError> {
        use crate::vm::iterator::NativeIteratorState;
        if std::env::var("BIUJS_TRACE_ITER").is_ok() {
            eprintln!("[make_iter] src_type={}", src.type_of());
        }

        // A native iterator is its own iterator, so `for (x of arr.keys())`
        // must not try to call `[Symbol.iterator]` on it (which would recurse).
        if let Value::Object(obj_ref) = &src {
            if obj_ref
                .borrow()
                .as_any()
                .downcast_ref::<crate::vm::iterator::NativeIteratorObject>()
                .is_some()
            {
                let id = self.next_iterator_id;
                self.next_iterator_id += 1;
                let iter_val = Value::Object(Rc::new(RefCell::new(
                    crate::vm::iterator::NativeIteratorObject::new(
                        id,
                        crate::vm::iterator::NativeIteratorState::Js {
                            iterator: src.clone(),
                        },
                    ),
                )));
                self.iterator_registry.insert(id, iter_val.clone());
                return Ok(iter_val);
            }
        }

        // ES 7.4.2 `GetIterator` starts with `GetMethod(obj, @@iterator)`, and
        // that step is *observable for every source*: a getter can throw, the
        // method can be replaced, and `delete Array.prototype[Symbol.iterator]`
        // has to turn `var [x] = [1]` into a TypeError.
        //
        // Skipping the lookup for real arrays and strings made all three cases
        // invisible. The fast path is still taken when the method resolves to
        // the built-in factory, so an ordinary `for (x of arr)` is unchanged.
        let factory = self.get_member(&src, &iterator_symbol_key(), module)?;
        if !factory.is_callable() {
            return Err(RuntimeError::TypeError(format!(
                "{} is not iterable",
                src.type_of()
            )));
        }
        // `Array.prototype[Symbol.iterator]` *is* `Array.prototype.values`
        // (ES 23.1.3.30), so both names mean "iterate the receiver as an
        // array-like" and both take the fast path below.
        let is_default_factory = match crate::builtins::native_function_name(&factory) {
            Some(name) => {
                name == crate::vm::iterator::ITERATOR_NATIVE_NAME
                    // `Array.prototype[Symbol.iterator]` is the same object as
                    // `Array.prototype.values`, whose native name carries the
                    // prototype-method prefix.
                    || name == format!("{}{}", crate::builtins::PROTO_METHOD_PREFIX, "values")
            }
            None => false,
        };

        let state = match &src {
            Value::Object(obj_ref) if is_default_factory => {
                match obj_ref.borrow().kind() {
                    ObjectKind::Array => {
                        let items = self.array_like_elements(&src).unwrap_or_default();
                        Some(NativeIteratorState::Array { items, idx: 0 })
                    }
                    // A String *wrapper* iterates its characters. Its built-in
                    // `@@iterator` is the same native factory as the array's, so
                    // without this arm the factory would route straight back
                    // into `make_iterator` forever — the wrapper matches neither
                    // the array arm nor the primitive-string one.
                    ObjectKind::String => Some(NativeIteratorState::String {
                        chars: self.array_like_elements(&src).unwrap_or_default(),
                        idx: 0,
                    }),
                    // The built-in factory is `Array.prototype.values`, which is
                    // *generic*: it iterates any receiver through
                    // `ToLength(Get(O, "length"))` plus the indexed properties.
                    // A receiver that reaches it without being an array —
                    // `o[Symbol.iterator] = Array.prototype[Symbol.iterator]` —
                    // has to iterate its array-like shape. Falling through to
                    // `invoke` here would call the factory back into
                    // `make_iterator` for ever (a Rust stack overflow, aborting
                    // the process). An object without a usable `length` iterates
                    // as empty, which is what `ToLength(undefined)` gives.
                    _ => Some(NativeIteratorState::Array {
                        items: self.array_like_elements(&src).unwrap_or_default(),
                        idx: 0,
                    }),
                }
            }
            Value::String(s) if is_default_factory => Some(NativeIteratorState::String {
                chars: s.chars().map(|c| Value::string(&c.to_string())).collect(),
                idx: 0,
            }),
            _ => None,
        };

        let state = match state {
            Some(state) => state,
            None => {
                // `src[Symbol.iterator]()`.
                let iterator = self.invoke(&factory, src.clone(), &[], module)?;
                crate::vm::iterator::NativeIteratorState::Js { iterator }
            }
        };

        let id = self.next_iterator_id;
        self.next_iterator_id += 1;
        let iter_val = Value::Object(Rc::new(RefCell::new(
            crate::vm::iterator::NativeIteratorObject::new(id, state),
        )));
        self.iterator_registry.insert(id, iter_val.clone());
        Ok(iter_val)
    }

    /// One step of `IterateNext`: returns `(value, done)`.
    fn iterator_next(
        &mut self,
        iter_val: Value,
        send: Option<Value>,
        module: &Module,
    ) -> Result<(Value, bool), RuntimeError> {
        use crate::vm::iterator::{native_next, NativeIteratorState};

        let iter_obj = match &iter_val {
            Value::Object(obj_ref) => Rc::clone(obj_ref),
            _ => {
                return Err(RuntimeError::TypeError(
                    "IteratorNext on non-iterator".to_string(),
                ));
            }
        };

        let is_js_iterator = {
            let borrowed = iter_obj.borrow();
            let Some(it) = borrowed.as_any().downcast_ref::<crate::vm::iterator::NativeIteratorObject>() else {
                return Err(RuntimeError::TypeError(
                    "IteratorNext on non-iterator".to_string(),
                ));
            };
            matches!(&*it.state().borrow(), NativeIteratorState::Js { .. })
        };

        if is_js_iterator {
            // Slow path: `it.next()`, then read { value, done }.
            let iterator = {
                let borrowed = iter_obj.borrow();
                let it = borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::iterator::NativeIteratorObject>()
                    .unwrap();
                match &*it.state().borrow() {
                    NativeIteratorState::Js { iterator } => iterator.clone(),
                    _ => unreachable!(),
                }
            };
            let next = self.get_member(&iterator, &PropertyKey::from_str("next"), module)?;
            let args = match &send {
                Some(v) => vec![v.clone()],
                None => Vec::new(),
            };
            let result = self.invoke(&next, iterator.clone(), &args, module)?;
            let done = match self.get_member(&result, &PropertyKey::from_str("done"), module)? {
                Value::Undefined => false,
                v => v.to_boolean(),
            };
            // The completion value is part of the result even when `done` is
            // true — `yield*` returns it (`function* i(){ return "R" }` gives
            // `yield* i()` the value `"R"`), and `IteratorResult` exposes it.
            let item = self.get_member(&result, &PropertyKey::from_str("value"), module)?;
            Ok((item, done))
        } else {
            let mut borrowed = iter_obj.borrow_mut();
            let it = borrowed
                .as_any_mut()
                .downcast_mut::<crate::vm::iterator::NativeIteratorObject>()
                .unwrap();
            match native_next(&mut *it.state().borrow_mut()) {
                Some(item) => Ok((item, false)),
                None => Ok((Value::Undefined, true)),
            }
        }
    }

    /// `IteratorClose` (ES6 §7.4.6): give the iterator a chance to clean up.
    ///
    /// Only meaningful on the slow path (calls `iterator.return()`); native
    /// iterators are stateless no-ops. Errors from `return()` are swallowed,
    /// as the spec requires for abrupt completions that already have a reason.
    /// `value` is the completion value `yield*` forwards to the iterator's
    /// `return` method (ES 14.4.14 step 5.c.iv); a plain `IteratorClose` passes
    /// none (ES 7.4.6).
    fn iterator_close(
        &mut self,
        iter_val: Value,
        value: Option<&Value>,
        over_abrupt: bool,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        use crate::vm::iterator::NativeIteratorState;

        let Value::Object(iter_obj) = &iter_val else {
            return Ok(());
        };
        let iterator = {
            let borrowed = iter_obj.borrow();
            let Some(it) = borrowed.as_any().downcast_ref::<crate::vm::iterator::NativeIteratorObject>() else {
                return Ok(());
            };
            match &*it.state().borrow() {
                NativeIteratorState::Js { iterator } => Some(iterator.clone()),
                _ => None,
            }
        };
        let Some(iterator) = iterator else {
            return Ok(());
        };
        let return_fn = self
            .get_member(&iterator, &PropertyKey::from_str("return"), module)
            .unwrap_or(Value::Undefined);
        if !return_fn.is_undefined() {
            // ES 7.4.6 step 4.b: a `return` that is neither `undefined` nor
            // callable is a TypeError.
            if !return_fn.is_callable() {
                return Err(RuntimeError::TypeError(
                    "iterator.return is not a function".to_string(),
                ));
            }
            let args = match value {
                Some(v) => vec![v.clone()],
                None => Vec::new(),
            };
            // ES 7.4.6 step 7: when the completion being closed over is a
            // *throw*, that completion wins — whatever `return()` raises or
            // returns (a number, `null`, …) is not observable. Checking the
            // result first turned a genuine `Test262Error` into a TypeError.
            if over_abrupt {
                let _ = self.invoke(&return_fn, iterator, &args, module);
                return Ok(());
            }
            // ES 7.4.6: only a *throw* completion swallows what `return()`
            // does (step 7, the `over_abrupt` branch above). For a normal
            // completion an exception out of `return()` **propagates** (step 8),
            // so it must not be discarded here.
            let result = self.invoke(&return_fn, iterator, &args, module)?;
            // Step 9: and a normal result that is not an object is a TypeError
            // of its own. Without this an iterator whose `return` answers `null`
            // closed silently (`*-close-null` families).
            if !result.is_object() {
                return Err(RuntimeError::TypeError(
                    "iterator.return() returned a non-object value".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// `[[Construct]]` variant of `invoke`: same frame mechanics, but the
    /// `this` binding is the freshly created object and the frame is flagged
    /// so `Ret` applies the construct return semantics.
    fn invoke_construct(
        &mut self,
        callee: &Value,
        new_obj: Value,
        args: &[Value],
        new_target_override: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if let Some(name) = crate::builtins::native_function_name(callee) {
            return self.call_native_by_name(&name, new_obj, args, module);
        }

        let (func_id, captured_vars) = match callee {
            Value::Function(id) => (*id, Vec::new()),
            Value::Object(obj_ref) => {
                let borrowed = obj_ref.borrow();
                match borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::FunctionObject>()
                {
                    Some(func_obj) => (func_obj.func_id, func_obj.captured_vars.clone()),
                    None => {
                        return Err(RuntimeError::TypeError("not a constructor".to_string()));
                    }
                }
            }
            _ => return Err(RuntimeError::TypeError("not a constructor".to_string())),
        };

        // `new.target` of a `[[Construct]]` frame is the constructor itself —
        // unless `Reflect.construct(F, args, newTarget)` said otherwise.
        let new_target_value = match new_target_override {
            Some(overridden) => overridden.clone(),
            None => match callee {
                Value::Function(id) => self.materialize_function(*id),
                other => other.clone(),
            },
        };
        // 帧驱动只有一份实现（P1）：这里只负责算出"这是个构造帧"。
        self.drive_bytecode_frame(
            func_id,
            callee,
            args,
            &captured_vars,
            FrameMode::Construct {
                new_obj,
                new_target: new_target_value,
            },
            module,
        )
    }

    /// ES 7.3.20 `IsConstructor`: whether `value` can be used with `new` — and,
    /// which is the same question, whether it is a legal `new.target`
    /// (`Reflect.construct(function(){}, [], f)` is how the harness asks it).
    fn is_constructor(&self, value: &Value, module: &Module) -> bool {
        match value {
            Value::Object(obj_ref) => {
                let kind = obj_ref.borrow().kind();
                match kind {
                    ObjectKind::Proxy => {
                        // A proxy is a constructor exactly when its target is.
                        let target = self
                            .proxy_parts(value, "construct")
                            .map(|(target, _)| target)
                            .ok();
                        matches!(target, Some(target) if self.is_constructor(&target, module))
                    }
                    ObjectKind::NativeFunction => {
                        let name = obj_ref
                            .borrow()
                            .as_any()
                            .downcast_ref::<crate::vm::object::NativeFunctionObject>()
                            .map(|f| f.name.clone())
                            .unwrap_or_default();
                        // A qualified name is a static or prototype method, and
                        // none of those is a constructor (ES 17); `Symbol` is
                        // callable but explicitly not new-able.
                        !name.contains('.') && name != "Symbol"
                    }
                    ObjectKind::Function => true,
                    _ => false,
                }
            }
            // Generators and `async` functions are callable but not new-able.
            Value::Function(id) => {
                !module.generators.contains(id) && !module.asyncs.contains(id)
            }
            _ => false,
        }
    }

    /// `[[Construct]]` (ES6 7.3.19): create the instance, run the constructor
    /// with `this` bound to it, and apply the construct return semantics.
    fn construct(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        self.construct_with_new_target(constructor_val, args, None, module)
    }

    /// [`Self::construct`] with an explicit `new.target`
    /// (`Reflect.construct(F, args, newTarget)`, ES 28.1.2): the instance's
    /// prototype and the callee's `new.target` both come from `new_target`
    /// instead of from the constructor being run.
    fn construct_with_new_target(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        new_target: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // A proxy's `construct` trap decides the whole construction; with no
        // trap the target is constructed (ES 10.5.13).
        if Self::is_proxy(constructor_val) {
            // `new P(...)` has the proxy itself as `new.target`.
            let nt = new_target.unwrap_or(constructor_val);
            return match self.proxy_construct_trap(constructor_val, args, nt, module)? {
                Some(result) => Ok(result),
                None => {
                    let (target, _handler) = self.proxy_parts(constructor_val, "construct")?;
                    self.construct_with_new_target(&target, args, new_target, module)
                }
            };
        }
        // Built-in constructors (Object, Array, Error, `f.bind(…)`, …) share
        // their `[[Construct]]` with the `New` opcode.
        if let Some(name) = crate::builtins::native_function_name(constructor_val) {
            return self.native_construct(&name, constructor_val, args, new_target, module);
        }

        // Bytecode constructor: resolve the prototype from the (boxed)
        // constructor's `prototype` property, create the instance, and run the
        // constructor body via the [[Construct]] frame flag.
        let boxed_ctor = match constructor_val {
            Value::Function(id) => self.materialize_function(*id),
            other => other.clone(),
        };
        // ES 9.1.14 `GetPrototypeFromConstructor`: with a `new.target`, its
        // `prototype` is what the instance inherits from.
        let from = new_target.unwrap_or(&boxed_ctor);
        let prototype = match from {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .property_get(&PropertyKey::from_str("prototype"))
                .map(|d| d.value),
            _ => {
                return Err(RuntimeError::TypeError("not a constructor".to_string()));
            }
        };

        let mut new_obj = crate::vm::object::OrdinaryObject::new();
        if let Some(Value::Object(proto_obj)) = &prototype {
            new_obj.set_prototype(Some(Rc::clone(proto_obj)));
        } else {
            new_obj.set_prototype(Some(Rc::clone(&self.builtins.object_prototype)));
        }
        let new_obj_val = Value::Object(Rc::new(RefCell::new(new_obj)));

        self.invoke_construct(&boxed_ctor, new_obj_val, args, new_target, module)
    }

    /// `[[Construct]]` for a built-in constructor (`new Array(…)`,
    /// `new Error(…)`, `new (f.bind(…))(…)`, …).
    ///
    /// Creates the `this` object from `C.prototype`, calls the native
    /// implementation, and maps the result through
    /// [`Self::finish_native_construct`].
    fn native_construct(
        &mut self,
        name: &str,
        constructor_val: &Value,
        args: &[Value],
        new_target: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // A bound function constructs its target with the bound arguments
        // prepended (ES 9.4.1.2).
        if let Some(id) = name.strip_prefix(BOUND_PREFIX) {
            let id: u32 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed bound function".to_string()))?;
            let Some((target, _this_arg, mut all)) = self
                .bound_functions
                .get(&id)
                .map(|b| (b.target.clone(), b.this_arg.clone(), b.bound_args.clone()))
            else {
                return Err(RuntimeError::InternalError(
                    "bound function is no longer available".to_string(),
                ));
            };
            all.extend(args.iter().cloned());
            return self.construct(&target, &all, module);
        }
        if name == "Symbol" {
            // `Symbol` is callable but not a constructor (ES 19.4.1.1).
            return Err(RuntimeError::TypeError(
                "Symbol is not a constructor".to_string(),
            ));
        }
        if name.contains('.') {
            // A static method is not a constructor.
            return Err(RuntimeError::TypeError("not a constructor".to_string()));
        }
        // `new Map(iterable)` / `new Set(iterable)`: the argument is consumed
        // through the iteration protocol, which only the VM can drive.
        if name == "Map" {
            return self.map_construct(constructor_val, args, new_target, module);
        }
        if name == "Set" {
            return self.set_construct(constructor_val, args, new_target, module);
        }
        if name == "WeakMap" {
            return self.weakmap_construct(constructor_val, args, new_target, module);
        }
        if name == "WeakSet" {
            return self.weakset_construct(constructor_val, args, new_target, module);
        }
        // `ArrayBuffer` and the TypedArray views: `new Uint8Array(buf)` is a
        // *view* over an existing buffer and `new Uint8Array(4)` one over a fresh
        // one, and the prototype comes from `newTarget` — so both need the VM.
        if name == "ArrayBuffer" {
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return crate::builtins::typedarray::arraybuffer_construct(args, proto);
        }
        if let Some(kind) = crate::builtins::typedarray::kind_from_name(name) {
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return crate::builtins::typedarray::typedarray_construct(kind, args, proto);
        }
        // `new Date(...)` builds the object itself: `Date()` without `new`
        // answers a *string*, so `call_native` cannot serve both
        // (ES 21.4.2.1 vs 21.4.3.1).
        if name == "Date" {
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return crate::builtins::date_construct_value(args, proto);
        }
        // `new Promise(executor)`: the executor has to be *called* with the two
        // settling functions, which only the VM can do.
        if name == "Promise" {
            let executor = args.first().cloned().unwrap_or(Value::Undefined);
            if !executor.is_callable() {
                return Err(RuntimeError::TypeError(
                    "Promise resolver undefined is not a function".to_string(),
                ));
            }
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return self.promise_construct(&executor, proto, module);
        }
        // `new Proxy(target, handler)`: the object is exotic (its operations are
        // the handler's traps), so it is built here rather than by the builtin
        // layer. There is no `prototype` to resolve: a proxy inherits from its
        // target (ES 28.2.2).
        if name == "Proxy" {
            // A bare `Value::Function` reference is a *function*, which is an
            // object — but `is_object` cannot see that, so it is boxed first.
            let target = self.as_object_value(&args.first().cloned().unwrap_or(Value::Undefined));
            let handler = self.as_object_value(&args.get(1).cloned().unwrap_or(Value::Undefined));
            return crate::builtins::proxy::proxy_construct(target, handler);
        }

        // ES 9.1.14 `GetPrototypeFromConstructor`: `newTarget.prototype` when
        // it is an object, otherwise the constructor's own `prototype` — which
        // for a plain `new Error(...)` is the same object anyway.
        let proto = self.prototype_from_constructor(constructor_val, new_target);
        // True when the prototype came from a *different* function than the
        // constructor being run: the instance the built-in just made carries
        // the intrinsic prototype and has to be re-parented.
        let reparent = new_target.is_some_and(|nt| !nt.strict_eq_obj(constructor_val));
        let mut this_obj = crate::vm::object::OrdinaryObject::new();
        match &proto {
            Some(Value::Object(p)) => this_obj.set_prototype(Some(Rc::clone(p))),
            _ => this_obj.set_prototype(Some(Rc::clone(&self.builtins.object_prototype))),
        }
        let this_val = Value::Object(Rc::new(RefCell::new(this_obj)));

        let result = crate::builtins::call_native(name, args)?;
        let result = self.finish_native_construct(result, &proto, this_val);
        if reparent {
            if let (Value::Object(obj_ref), Some(Value::Object(p))) = (&result, &proto) {
                obj_ref.borrow_mut().set_prototype(Some(Rc::clone(p)));
            }
        }
        Ok(result)
    }

    /// Result value of `new C(…)` for a built-in constructor `C`.
    ///
    /// Built-ins either return a fully formed object (Array, Object, Error, …),
    /// a primitive (String/Number/Boolean, which the constructor wraps in an
    /// object), or nothing at all — in which case the `this` object created for
    /// the call is the result.
    fn finish_native_construct(
        &self,
        result: Value,
        proto: &Option<Value>,
        this_obj: Value,
    ) -> Value {
        let proto_ref = match proto {
            Some(Value::Object(p)) => Some(Rc::clone(p)),
            _ => None,
        };
        match &result {
            Value::Object(obj_ref) => {
                // A result that already has a `[[Prototype]]` was built by the
                // constructor itself (`Error(...)`, `Object(x)` returning `x`);
                // do not rewire it.
                if obj_ref.borrow().get_prototype().is_none() {
                    if let Some(proto) = proto_ref {
                        obj_ref.borrow_mut().set_prototype(Some(proto));
                    }
                }
                result
            }
            // `new String("x")` / `new Number(1)` / `new Boolean(true)`.
            Value::String(_) | Value::Number(_) | Value::Bool(_) => Value::Object(Rc::new(
                RefCell::new(crate::vm::object::PrimitiveWrapperObject::new(
                    result, proto_ref,
                )),
            )),
            _ => this_obj,
        }
    }

    fn call_builtin_method(
        &self,
        obj: &Value,
        method_name: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        // First check: is this a static method on a native function (constructor)?
        if let Some(name) = crate::builtins::native_function_name(obj) {
            let static_method = format!("{}.{}", name, method_name);
            if let Ok(result) = crate::builtins::call_static_method(&static_method, args) {
                return Ok(result);
            }
        }

        // Delegate to the builtins module for prototype method dispatching
        match crate::builtins::call_prototype_method(obj, method_name, args) {
            Ok(result) => Ok(result),
            Err(_) => {
                // Fallback: unknown method, try property lookup via prototype chain
                if let Value::Object(obj_ref) = obj {
                    let key = crate::vm::property::PropertyKey::from_str(method_name);
                    match crate::vm::prototype::internal_get(obj_ref.clone(), &key, None) {
                        Ok(v) => Ok(v),
                        Err(e) => Err(RuntimeError::from_property_error(e)),
                    }
                } else {
                    Ok(Value::Undefined)
                }
            }
        }
    }
}

/// `finalize` for the JSON serializer (ES 25.5.2.4 step 9 / 25.5.2.5 step 10).
///
/// `inner` is the indent of the members, `stepback` the indent of the opening
/// line — with a gap the closing bracket lines up with the opening one.
fn finalize_json(
    partial: Vec<String>,
    gap: &str,
    inner: &str,
    stepback: &str,
    open: char,
    close: char,
) -> String {
    if partial.is_empty() {
        return format!("{open}{close}");
    }
    if gap.is_empty() {
        return format!("{open}{}{close}", partial.join(","));
    }
    format!(
        "{open}\n{inner}{}\n{stepback}{close}",
        partial.join(&format!(",\n{inner}"))
    )
}

impl Default for VM {
    fn default() -> Self {
        Self::new()
    }
}

/// VM execution state
struct State {
    data_stack: Vec<Value>,
    ctrl_stack: Vec<usize>,
    registers: [Value; 19],
    seh_stack: Vec<SehRecord>,
    /// The script's globals — shared with the `globalThis` object (see
    /// `vm::object::GlobalObject`), so both views stay in step.
    globals: Rc<RefCell<HashMap<String, Value>>>,
    /// The `globalThis` object itself, kept here so it survives re-registration
    /// and can be published as a global of the same name.
    global_object: Value,
    /// The script's **declarative record**: `let` / `const` / `class` declared at
    /// script scope. Distinct from `globals` (the global *object*) because those
    /// bindings are not properties: `globalThis.x` stays `undefined` for
    /// `let x`, while a nested function still resolves the name. `None` is an
    /// uninitialized binding — the temporal dead zone.
    ///
    /// Fresh per `run` (`run` replaces the whole `State`), so one test's
    /// declarations cannot leak into the next.
    script_env: HashMap<String, Option<Value>>,
    /// The microtask queue. The engine has no event loop, so this is drained at
    /// the end of the top-level `run` and inside `await`.
    promise_jobs: std::collections::VecDeque<PromiseJob>,
    /// Promises reachable from the native `resolve` / `reject` functions, keyed
    /// by the id embedded in their names — the engine dispatches natives by
    /// name, so that is how those two closures carry their target.
    promises: HashMap<u64, Rc<RefCell<PromiseObject>>>,
    next_promise_id: u64,
    /// In-flight `Promise.all` / `race` / `allSettled` / `any` aggregations,
    /// keyed by the id embedded in the per-element native handler names — the
    /// engine dispatches natives by name, so that is how those handlers carry
    /// their aggregation state.
    aggregations: HashMap<u64, PromiseAggregation>,
    next_aggregation_id: u64,
    /// Pending `finally` callbacks, keyed by the id embedded in the handler
    /// names minted by `try_promise_method`.
    finally_handlers: HashMap<u64, Value>,
    next_finally_id: u64,
    this_val: Value,
    /// Stack of captured variable maps for active closure scopes
    closure_var_stack: Vec<HashMap<String, Value>>,
    /// Per-frame flag: true if the current frame was invoked via `new` ([[Construct]]).
    /// Used to decide whether a returned value should be replaced by `this`.
    construct_stack: Vec<bool>,
    /// Per-frame `new.target`: the constructor when the frame was entered
    /// through `new`, `undefined` otherwise. A frame entered for an arrow
    /// function inherits the value captured when the arrow was created.
    new_target_stack: Vec<Value>,
    /// The function object whose body is currently executing. `super` reads the
    /// home object recorded on it at class-definition time.
    function_val: Value,
    /// Saved `function_val` of the caller frames, parallel to `this_stack`.
    function_stack: Vec<Value>,
    /// Saved `this` binding of the caller frame, restored on `Ret`.
    /// Keeping `this` per-frame prevents a nested call from clobbering it.
    this_stack: Vec<Value>,
    /// Per-frame constructor state — see [`THIS_NONE`]. Only derived-class
    /// constructors (`class C extends P`) are tracked: their `this` starts
    /// *unbound* and only `super()` binds it (ES 9.2.2 / 12.3.5.1), and on
    /// return they may only produce an object or `undefined` (ES 9.2.2 step
    /// 13c). Two facts, one stack, so there is no second parallel stack to
    /// keep in sync.
    this_state: Vec<u8>,
    /// Number of arguments passed to each active frame, parallel to
    /// `this_stack`. This is what `Opcode::Arguments` reads to build the
    /// `arguments` object: the callee has no other way to learn its arity.
    frame_argc: Vec<usize>,
    rsp: usize,
    rbp: usize,
    pc: usize,
}

impl State {
    fn new() -> Self {
        let globals = Rc::new(RefCell::new(HashMap::new()));
        let global_object = Value::Object(Rc::new(RefCell::new(
            crate::vm::object::GlobalObject::new(Rc::clone(&globals)),
        )));
        Self {
            data_stack: Vec::with_capacity(1024),
            ctrl_stack: Vec::with_capacity(256),
            registers: std::array::from_fn(|_| Value::Undefined),
            seh_stack: Vec::new(),
            globals,
            global_object,
            script_env: HashMap::new(),
            promise_jobs: std::collections::VecDeque::new(),
            promises: HashMap::new(),
            next_promise_id: 0,
            aggregations: HashMap::new(),
            next_aggregation_id: 0,
            finally_handlers: HashMap::new(),
            next_finally_id: 0,
            this_val: Value::Undefined,
            closure_var_stack: Vec::new(),
            construct_stack: Vec::new(),
            new_target_stack: Vec::new(),
            function_val: Value::Undefined,
            function_stack: Vec::new(),
            this_stack: Vec::new(),
            this_state: Vec::new(),
            frame_argc: Vec::new(),
            rsp: 0,
            rbp: 0,
            pc: 0,
        }
    }

    fn ctrl_stack_reached_bottom(&self) -> bool {
        self.ctrl_stack.is_empty()
    }

    fn jump(&mut self, target: usize) {
        self.pc = target;
    }

    fn jump_offset(&mut self, offset: isize) {
        self.pc = (self.pc as isize + offset) as usize;
    }

    fn get_register(&self, reg: Register) -> Result<Value, RuntimeError> {
        match reg {
            Register::Rsp | Register::Rbp => {
                // These are not accessible as value registers
                Err(RuntimeError::TypeError(format!(
                    "cannot read register {reg} as value"
                )))
            }
            _ => Ok(self.registers[reg.as_usize()].clone()),
        }
    }

    fn set_register(&mut self, reg: Register, value: Value) -> Result<(), RuntimeError> {
        match reg {
            Register::Rsp | Register::Rbp => Err(RuntimeError::TypeError(format!(
                "cannot write register {reg} as value"
            ))),
            _ => {
                self.registers[reg.as_usize()] = value;
                Ok(())
            }
        }
    }

    fn push(&mut self, value: Value) -> Result<(), RuntimeError> {
        if self.rsp >= STACK_MAX {
            return Err(RuntimeError::RangeError("stack overflow".to_string()));
        }
        if self.rsp >= self.data_stack.len() {
            // Doubling alone is not enough when a frame's prologue (`AddC rsp,
            // stack_size`) jumps `rsp` past the current capacity in one step:
            // cover the requested index as well.
            let grow_to = (self.data_stack.len() * 2)
                .max(self.rsp + 1)
                .max(1024)
                .min(STACK_MAX);
            self.data_stack.resize(grow_to, Value::Undefined);
        }
        self.data_stack[self.rsp] = value;
        self.rsp += 1;
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, RuntimeError> {
        if self.rsp == 0 {
            return Err(RuntimeError::RangeError("stack underflow".to_string()));
        }
        self.rsp -= 1;
        Ok(self.data_stack[self.rsp].clone())
    }

    fn pushc(&mut self, value: usize) -> Result<(), RuntimeError> {
        self.ctrl_stack.push(value);
        Ok(())
    }

    fn popc(&mut self) -> Result<usize, RuntimeError> {
        self.ctrl_stack
            .pop()
            .ok_or_else(|| RuntimeError::RangeError("control stack underflow".to_string()))
    }

    /// Make sure slot `index` is addressable, growing the (lazy) value stack.
    fn ensure(&mut self, index: usize) -> Result<(), RuntimeError> {
        if index >= STACK_MAX {
            return Err(RuntimeError::RangeError(
                "stack access out of bounds".to_string(),
            ));
        }
        if index >= self.data_stack.len() {
            let grow_to = (self.data_stack.len() * 2).max(index + 1).min(STACK_MAX);
            self.data_stack.resize(grow_to, Value::Undefined);
        }
        Ok(())
    }

    fn get_value_from_stack(&self, offset: isize) -> Result<Value, RuntimeError> {
        let index = self.rbp as isize + offset;
        if index < 0 || index as usize >= STACK_MAX {
            return Err(RuntimeError::RangeError(
                "stack access out of bounds".to_string(),
            ));
        }
        // Negative offsets address the incoming arguments (`arg i` at
        // `[rbp - (i + 1)]`). Slots below that region belong to parameters that
        // were *not* passed; they must read as `undefined` instead of stale
        // values left over from an earlier frame.
        //
        // Only the callee reads those slots, so `frame_argc` is the current
        // frame's arity here. Call sites read their outgoing arguments through
        // `raw_stack_value`, which bypasses this check.
        if offset < 0 {
            let argc = self.frame_argc.last().copied().unwrap_or(0);
            if (-offset) as usize > argc {
                return Ok(Value::Undefined);
            }
        }
        Ok(self.raw_stack_value(index as usize))
    }

    /// Read a data-stack slot without the "missing argument" rule above.
    fn raw_stack_value(&self, index: usize) -> Value {
        self.data_stack
            .get(index)
            .cloned()
            .unwrap_or(Value::Undefined)
    }

    fn set_value_to_stack(&mut self, offset: isize, value: Value) -> Result<(), RuntimeError> {
        let index = self.rbp as isize + offset;
        if index < 0 || index as usize >= STACK_MAX {
            return Err(RuntimeError::RangeError(
                "stack access out of bounds".to_string(),
            ));
        }
        // Never write below the argument region: those slots belong to the
        // caller's frame and writing them would corrupt it.
        if offset < 0 {
            let argc = self.frame_argc.last().copied().unwrap_or(0);
            if (-offset) as usize > argc {
                return Ok(());
            }
        }
        self.ensure(index as usize)?;
        self.data_stack[index as usize] = value;
        Ok(())
    }

    /// Enter a JS call frame: save the caller's `this`, record the argument
    /// count (used by `Opcode::Arguments`) and guard recursion depth.
    fn enter_frame(&mut self, argc: usize) -> Result<(), RuntimeError> {
        if self.ctrl_stack.len() / 3 >= MAX_CALL_DEPTH {
            return Err(RuntimeError::RangeError(
                "Maximum call stack size exceeded".to_string(),
            ));
        }
        self.this_stack.push(self.this_val.clone());
        self.this_state.push(THIS_NONE);
        self.function_stack.push(self.function_val.clone());
        self.frame_argc.push(argc);
        Ok(())
    }

    /// Resolve `name` in the environment.
    ///
    /// Order: the script's **declarative record** first (the truth for
    /// script-scope `let`/`const`/`class`, and the carrier of their temporal dead
    /// zone), then the closure snapshots, then the global object. Splitting this
    /// out matters because `typeof` has to consult exactly the same chain — a
    /// `typeof` that skipped the record answered `"undefined"` for a name the
    /// ordinary read resolves to a function.
    fn resolve_env_name(&self, name: &str) -> Result<Option<Value>, RuntimeError> {
        // `globalThis` is answered from the realm's global object rather than
        // from the globals map: publishing it *in* the map would make
        // map → object → map, a cycle that (with no GC) pinned every run's
        // whole `State` — measured as a shard that outgrew its `ulimit`.
        if name == "globalThis" {
            return Ok(Some(self.global_object.clone()));
        }
        match self.script_env.get(name) {
            Some(Some(value)) => return Ok(Some(value.clone())),
            Some(None) => {
                return Err(RuntimeError::ReferenceError(format!(
                    "Cannot access '{name}' before initialization"
                )))
            }
            None => {}
        }
        if let Some(value) = self
            .closure_var_stack
            .iter()
            .rev()
            .find_map(|map| map.get(name).cloned())
        {
            return Ok(Some(value));
        }
        Ok(self.get_global(name))
    }

    fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.borrow().get(name).cloned()
    }
}

/// ES `ToInt32` (ECMAScript 7.1.6): truncate toward zero, then modulo 2**32.
pub fn to_int32(n: f64) -> i32 {
    if n.is_nan() || n.is_infinite() || n == 0.0 {
        return 0;
    }
    let truncated = n.trunc();
    let rem = truncated % 4_294_967_296.0; // 2**32
    let rem = if rem < 0.0 { rem + 4_294_967_296.0 } else { rem };
    if rem >= 2_147_483_648.0 {
        (rem - 4_294_967_296.0) as i32
    } else {
        rem as i32
    }
}

/// ES `ToUint32` (ECMAScript 7.1.7).
pub fn to_uint32(n: f64) -> u32 {
    to_int32(n) as u32
}

/// The shape `GetSetRecord` validates for the `set-methods` operators
/// (ES2024 24.2.3.1): a set-like argument is any object with a numeric `size`,
/// a callable `has` and a callable `keys`.
struct SetRecord {
    object: Value,
    size: usize,
    has: Value,
    keys: Value,
}

/// A fresh `Set` with the given prototype and no entries.
fn set_data_new(prototype: Option<Rc<RefCell<dyn crate::vm::object::JSObject>>>) -> Value {
    Value::Object(Rc::new(RefCell::new(crate::vm::object::SetObject::new(
        prototype,
    ))))
}

/// Run `f` with the `SetObject` behind `value` (no-op when it is not a Set).
fn with_set_data<R>(value: &Value, f: impl FnOnce(&mut crate::vm::object::SetObject) -> R) -> Option<R> {
    let Value::Object(obj_ref) = value else {
        return None;
    };
    let mut borrowed = obj_ref.borrow_mut();
    borrowed
        .as_any_mut()
        .downcast_mut::<crate::vm::object::SetObject>()
        .map(f)
}

fn set_data_size(value: &Value) -> usize {
    with_set_data(value, |set| set.size()).unwrap_or(0)
}

fn set_data_has(value: &Value, needle: &Value) -> bool {
    with_set_data(value, |set| set.has(needle)).unwrap_or(false)
}

fn set_data_add(value: &Value, entry: Value) {
    with_set_data(value, |set| set.add(entry));
}

fn set_data_delete(value: &Value, needle: &Value) -> bool {
    with_set_data(value, |set| set.delete(needle)).unwrap_or(false)
}

/// Live entries in insertion order.
fn set_data_values(value: &Value) -> Vec<Value> {
    with_set_data(value, |set| {
        let mut out = Vec::new();
        let mut idx = 0;
        while let Some((next, entry)) = set.entry_from(idx) {
            idx = next + 1;
            out.push(entry);
        }
        out
    })
    .unwrap_or_default()
}

/// SEH (Structured Exception Handling) record for try/catch/finally
///
/// `Clone` because a generator's suspended frame carries the records that were
/// live at the `yield`: without them the body's `try`/`finally` would be
/// forgotten the moment it suspends.
#[derive(Clone, Debug)]
struct SehRecord {
    /// catch handler PC (0 if no catch)
    handler_pc: usize,
    /// finally handler PC (0 if no finally)
    finally_pc: usize,
    saved_rsp: usize,
    saved_rbp: usize,
    /// whether we're currently executing finally
    in_finally: bool,
    /// pending exception value when entering finally from throw
    pending_exception: Option<Value>,
    /// whether catch handler has already been executed for this SEH scope
    catch_executed: bool,
    /// delayed jump target (for break/continue inside try-finally)
    delayed_jump_target: Option<isize>,
    /// delayed return flag (for return inside try-finally)
    delayed_return: bool,
    /// saved closure var stack depth when return was deferred by finally
    saved_closure_depth: usize,
    /// Call-frame depths captured when the `try` was entered.
    ///
    /// An exception raised inside a nested call has to unwind those frames
    /// before the handler runs: the control stack still holds their return
    /// addresses, and leaving them there would make the handler resume the
    /// throwing function instead of continuing in the handler's frame.
    saved_ctrl_depth: usize,
    /// Depth of `this_stack` (and its parallel `function_stack` /
    /// `frame_argc`) — one entry per active JS frame.
    saved_this_depth: usize,
    saved_construct_depth: usize,
    saved_new_target_depth: usize,
}

/// Per-frame constructor states for [`State::this_state`].
///
/// * `THIS_NONE` — an ordinary frame.
/// * `THIS_DERIVED_UNBOUND` — a derived constructor before `super()`.
/// * `THIS_DERIVED_BOUND` — a derived constructor after `super()`.
const THIS_NONE: u8 = 0;
const THIS_DERIVED_UNBOUND: u8 = 1;
const THIS_DERIVED_BOUND: u8 = 2;

/// What a run of a generator body ended with.
///
/// `frame` is `Some` when the body suspended again (at a `yield`, possibly one
/// inside a `finally` that a return completion is running).
struct GeneratorRunOutcome {
    yielded: Option<Value>,
    frame: Option<crate::vm::object::SuspendedFrame>,
    rv: Value,
}

/// Per-frame execution context saved across a nested (`invoke`-style or
/// generator-resume) execution loop.
#[derive(Debug, Clone)]
struct SavedExecutionState {
    pc: usize,
    rsp: usize,
    rbp: usize,
    closure: usize,
    seh: usize,
    this: Value,
    this_depth: usize,
    construct: usize,
    new_target: usize,
    function: Value,
    /// Control-stack depth *before* the callee's own entries: the boundary a
    /// generator body (or an `invoke`) may not dispatch an exception across.
    ctrl: usize,
    delegate: usize,
    registers: [Value; 19],
}

/// One entry of the microtask queue: a reaction to run and the settlement that
/// triggered it.
#[derive(Debug, Clone)]
pub enum PromiseJob {
    Reaction {
        reaction: crate::vm::object::PromiseReaction,
        argument: Value,
        fulfilled: bool,
    },
    /// `PromiseResolveThenableJob` (ES 27.2.1.9): a foreign thenable was passed
    /// to a resolving function, so its `then` has to be called with functions
    /// that settle the promise this job is adopting (`promise`).
    Thenable {
        promise: Rc<RefCell<crate::vm::object::PromiseObject>>,
        thenable: Value,
    },
}

/// Which of the four `Promise` combinators an aggregation belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    All,
    Race,
    AllSettled,
    Any,
}

/// Recognise the two per-element handler names minted by `promise_aggregate`,
/// answering `(aggregation id, element index)`.
fn parse_aggregation_handler(name: &str) -> Option<(u64, usize)> {
    let rest = name
        .strip_prefix("__promise_agg_ok__")
        .or_else(|| name.strip_prefix("__promise_agg_err__"))?;
    let (id, index) = rest.split_once('_')?;
    Some((id.parse().ok()?, index.parse().ok()?))
}

/// State for one in-flight `Promise.all` / `race` / `allSettled` / `any`.
///
/// The per-element handlers are natives whose names carry `id`; when the queue
/// runs them they look the aggregation up here and mutate it. That is the
/// engine's usual way of giving a host function captured state (see
/// `__promise_resolve__`).
#[derive(Debug)]
pub struct PromiseAggregation {
    kind: AggregateKind,
    result: Rc<RefCell<crate::vm::object::PromiseObject>>,
    /// Elements still to settle (for `all` / `allSettled` / `any`).
    remaining: usize,
    /// Element results, by index (`all` / `allSettled` values, `any` reasons).
    values: Vec<Value>,
    /// `race` / `any` settle on the first match; once true later ones are noise.
    done: bool,
    /// `any` rejection countdown.
    rejections: usize,
    /// `any` total element count (known only after the iterable is exhausted).
    total: usize,
}

impl Drop for VM {
    /// Break this realm's cycles so dropping the VM actually frees it.
    ///
    /// A host that builds a VM per program (the test262 runner does, because a
    /// test may mutate built-ins and expects a clean realm) otherwise leaks the
    /// whole builtins graph of every program: measured as a shard that could not
    /// finish under a 1 GiB `ulimit`.
    fn drop(&mut self) {
        self.state.globals.borrow_mut().remove("globalThis");
        self.builtins.teardown();
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    /// A feature that has not been implemented yet
    NotImplemented(String),
    /// TypeError from the JS specification
    TypeError(String),
    /// ReferenceError from the JS specification
    ReferenceError(String),
    /// RangeError from the JS specification
    RangeError(String),
    /// SyntaxError from the JS specification
    SyntaxError(String),
    /// An internal VM error (should not happen in correct code)
    InternalError(String),
    /// A JS exception thrown by user code (via `throw`)
    Thrown(Value),
}

/// `[[DefineOwnProperty]]` reports failure as a plain `String`, which cannot
/// carry the error *kind* — every failure out of that channel used to become a
/// TypeError. One case genuinely needs a `RangeError` to survive: writing an
/// array's `length` (ES 10.4.2.4 `ArraySetLength` throws RangeError, and
/// `Object.defineProperty([], "length", {value: -1})` must not report a
/// TypeError). The array's own `define_property` prefixes such messages with
/// `RANGE_ERROR_PREFIX` and this is where they are turned back.
pub const PROPERTY_ERROR_RANGE_PREFIX: &str = "RangeError: ";

impl RuntimeError {
    /// Turn a `[[DefineOwnProperty]]` failure message back into an error,
    /// preserving the one kind that the `String` channel has to carry.
    pub fn from_property_error(msg: String) -> Self {
        match msg.strip_prefix(PROPERTY_ERROR_RANGE_PREFIX) {
            Some(rest) => RuntimeError::RangeError(rest.to_string()),
            None => RuntimeError::TypeError(msg),
        }
    }

    /// Render an error for the `[[DefineOwnProperty]]` `String` channel,
    /// preserving `RangeError` through [`PROPERTY_ERROR_RANGE_PREFIX`].
    pub fn into_property_error(self) -> String {
        match self {
            RuntimeError::RangeError(msg) => format!("{PROPERTY_ERROR_RANGE_PREFIX}{msg}"),
            other => other.to_string(),
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::NotImplemented(msg) => write!(f, "NotImplemented: {msg}"),
            RuntimeError::TypeError(msg) => write!(f, "TypeError: {msg}"),
            RuntimeError::ReferenceError(msg) => write!(f, "ReferenceError: {msg}"),
            RuntimeError::RangeError(msg) => write!(f, "RangeError: {msg}"),
            RuntimeError::SyntaxError(msg) => write!(f, "SyntaxError: {msg}"),
            RuntimeError::InternalError(msg) => write!(f, "InternalError: {msg}"),
            RuntimeError::Thrown(val) => {
                // Error-like objects should render as `Name: message`, not
                // `[object Object]`, so failures are diagnosable. `name` /
                // `message` are prototype properties for the standard error
                // types, so the whole chain has to be consulted.
                if let Some(obj_ref) = val.as_object() {
                    let get = |prop: &str| -> Option<String> {
                        crate::vm::prototype::find_descriptor(
                            Rc::clone(&obj_ref),
                            &PropertyKey::from_str(prop),
                        )
                        .ok()
                        .flatten()
                        .and_then(|(_, d)| match d.value {
                            Value::String(s) => Some(s.to_string()),
                            _ => None,
                        })
                    };
                    let name = get("name").unwrap_or_else(|| "Error".to_string());
                    match get("message") {
                        Some(msg) if !msg.is_empty() => return write!(f, "{name}: {msg}"),
                        _ => return write!(f, "{name}"),
                    }
                }
                write!(f, "{val}")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Constant, Instr, Operand, Primitive, Register, RelPc};

    // ──────────────────────── State ────────────────────────

    #[test]
    fn test_state_new() {
        let state = State::new();
        assert_eq!(state.pc, 0);
        assert_eq!(state.rsp, 0);
        assert_eq!(state.rbp, 0);
        assert!(state.ctrl_stack.is_empty());
        assert!(state.seh_stack.is_empty());
        assert!(state.globals.borrow().is_empty());
    }

    #[test]
    fn test_state_push_pop() {
        let mut state = State::new();
        state.push(Value::Number(42.0)).unwrap();
        assert_eq!(state.rsp, 1);
        let val = state.pop().unwrap();
        assert_eq!(val, Value::Number(42.0));
        assert_eq!(state.rsp, 0);
    }

    #[test]
    fn test_state_push_pop_multiple() {
        let mut state = State::new();
        state.push(Value::Number(1.0)).unwrap();
        state.push(Value::Number(2.0)).unwrap();
        state.push(Value::Number(3.0)).unwrap();
        assert_eq!(state.rsp, 3);
        assert_eq!(state.pop().unwrap(), Value::Number(3.0));
        assert_eq!(state.pop().unwrap(), Value::Number(2.0));
        assert_eq!(state.pop().unwrap(), Value::Number(1.0));
        assert_eq!(state.rsp, 0);
    }

    #[test]
    fn test_state_pop_underflow() {
        let mut state = State::new();
        let result = state.pop();
        assert!(matches!(result, Err(RuntimeError::RangeError(_))));
    }

    #[test]
    fn test_state_pushc_popc() {
        let mut state = State::new();
        state.pushc(100).unwrap();
        state.pushc(200).unwrap();
        assert_eq!(state.popc().unwrap(), 200);
        assert_eq!(state.popc().unwrap(), 100);
    }

    #[test]
    fn test_state_popc_underflow() {
        let mut state = State::new();
        let result = state.popc();
        assert!(matches!(result, Err(RuntimeError::RangeError(_))));
    }

    #[test]
    fn test_state_ctrl_stack_reached_bottom() {
        let mut state = State::new();
        assert!(state.ctrl_stack_reached_bottom());
        state.pushc(0).unwrap();
        assert!(!state.ctrl_stack_reached_bottom());
    }

    #[test]
    fn test_state_jump() {
        let mut state = State::new();
        state.jump(100);
        assert_eq!(state.pc, 100);
    }

    #[test]
    fn test_state_jump_offset() {
        let mut state = State::new();
        state.pc = 10;
        state.jump_offset(5);
        assert_eq!(state.pc, 15);
        state.jump_offset(-3);
        assert_eq!(state.pc, 12);
    }

    #[test]
    fn test_state_register() {
        let mut state = State::new();
        state
            .set_register(Register::R0, Value::Number(42.0))
            .unwrap();
        let val = state.get_register(Register::R0).unwrap();
        assert_eq!(val, Value::Number(42.0));
    }

    #[test]
    fn test_state_register_default_undefined() {
        let state = State::new();
        let val = state.get_register(Register::R0).unwrap();
        assert_eq!(val, Value::Undefined);
    }

    #[test]
    fn test_state_register_rsp_rbp_not_readable() {
        let state = State::new();
        let result = state.get_register(Register::Rsp);
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn test_state_register_rsp_rbp_not_writable() {
        let mut state = State::new();
        let result = state.set_register(Register::Rsp, Value::Number(0.0));
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn test_state_get_value_from_stack() {
        let mut state = State::new();
        state.rbp = 0;
        // The value stack grows lazily, so an untouched slot reads as undefined.
        assert_eq!(state.get_value_from_stack(0).unwrap(), Value::Undefined);
        state.set_value_to_stack(0, Value::Number(42.0)).unwrap();
        let val = state.get_value_from_stack(0).unwrap();
        assert_eq!(val, Value::Number(42.0));
    }

    #[test]
    fn test_state_set_value_to_stack() {
        let mut state = State::new();
        state.rbp = 0;
        state.set_value_to_stack(0, Value::Number(42.0)).unwrap();
        assert_eq!(state.data_stack[0], Value::Number(42.0));
    }

    #[test]
    fn test_state_stack_out_of_bounds() {
        let state = State::new();
        let result = state.get_value_from_stack(-1);
        assert!(matches!(result, Err(RuntimeError::RangeError(_))));
    }

    #[test]
    fn test_state_global() {
        let mut state = State::new();
        assert!(state.get_global("x").is_none());
        state
            .globals
            .borrow_mut()
            .insert("x".to_string(), Value::Number(42.0));
        assert_eq!(state.get_global("x"), Some(Value::Number(42.0)));
    }

    // ──────────────────────── VM utility methods ────────────────────────

    #[test]
    fn test_vm_from_primitive() {
        assert_eq!(VM::from_primitive(Primitive::Null), Value::Null);
        assert_eq!(VM::from_primitive(Primitive::Undefined), Value::Undefined);
        assert_eq!(
            VM::from_primitive(Primitive::Boolean(true)),
            Value::Bool(true)
        );
        assert_eq!(
            VM::from_primitive(Primitive::Integer(42)),
            Value::Number(42.0)
        );
        assert_eq!(
            VM::from_primitive(Primitive::Float(3.14)),
            Value::Number(3.14)
        );
    }

    #[test]
    fn test_vm_from_constant() {
        let c = Constant::from("hello");
        assert_eq!(VM::from_constant(&c), Value::string("hello"));
    }

    // ──────────────────────── js_abstract_relational ────────────────────────

    #[test]
    fn test_js_abstract_relational_numbers() {
        assert!(VM::js_abstract_relational(&Value::Number(1.0), &Value::Number(2.0)).unwrap());
        assert!(!VM::js_abstract_relational(&Value::Number(2.0), &Value::Number(1.0)).unwrap());
        assert!(!VM::js_abstract_relational(&Value::Number(1.0), &Value::Number(1.0)).unwrap());
    }

    #[test]
    fn test_js_abstract_relational_strings() {
        assert!(VM::js_abstract_relational(&Value::string("a"), &Value::string("b")).unwrap());
        assert!(!VM::js_abstract_relational(&Value::string("b"), &Value::string("a")).unwrap());
        assert!(!VM::js_abstract_relational(&Value::string("a"), &Value::string("a")).unwrap());
    }

    #[test]
    fn test_js_abstract_relational_nan() {
        assert!(
            !VM::js_abstract_relational(&Value::Number(f64::NAN), &Value::Number(1.0)).unwrap()
        );
        assert!(
            !VM::js_abstract_relational(&Value::Number(1.0), &Value::Number(f64::NAN)).unwrap()
        );
    }

    #[test]
    fn test_js_abstract_relational_mixed_types() {
        // "2" < 3 → true (string coerced to number)
        assert!(VM::js_abstract_relational(&Value::string("2"), &Value::Number(3.0)).unwrap());
        // "hello" < 3 → false ("hello" → NaN)
        assert!(!VM::js_abstract_relational(&Value::string("hello"), &Value::Number(3.0)).unwrap());
    }

    // ──────────────────────── js_prop_get ────────────────────────

    #[test]
    fn test_js_prop_get_string_length() {
        let s = Value::string("hello");
        assert_eq!(VM::js_prop_get(&s, "length").unwrap(), Value::Number(5.0));
    }

    #[test]
    fn test_js_prop_get_string_unknown() {
        let s = Value::string("hello");
        assert_eq!(VM::js_prop_get(&s, "foo").unwrap(), Value::Undefined);
    }

    #[test]
    fn test_js_prop_get_array_length() {
        use crate::vm::object::new_array_object;
        let arr_val = {
            let arr = new_array_object();
            if let Value::Object(ref arr_ref) = arr {
                let mut obj = arr_ref.borrow_mut();
                if let Some(a) = obj.as_any_mut().downcast_mut::<ArrayObject>() {
                    a.push(Value::Number(1.0));
                    a.push(Value::Number(2.0));
                }
            }
            arr
        };
        assert_eq!(
            VM::js_prop_get(&arr_val, "length").unwrap(),
            Value::Number(2.0)
        );
    }

    #[test]
    fn test_js_prop_get_object() {
        use crate::vm::object::new_ordinary_object;
        use crate::vm::property::PropertyKey;
        let obj_val = {
            let obj = new_ordinary_object();
            if let Value::Object(ref obj_ref) = obj {
                let mut ob = obj_ref.borrow_mut();
                ob.property_set(PropertyKey::from_str("x"), Value::Number(42.0))
                    .unwrap();
            }
            obj
        };
        assert_eq!(VM::js_prop_get(&obj_val, "x").unwrap(), Value::Number(42.0));
        assert_eq!(VM::js_prop_get(&obj_val, "y").unwrap(), Value::Undefined);
    }

    #[test]
    fn test_js_prop_get_number() {
        let n = Value::Number(42.0);
        assert_eq!(VM::js_prop_get(&n, "toString").unwrap(), Value::Undefined);
    }

    // ──────────────────────── VM::run with simple modules ────────────────────────

    /// Helper to build a minimal module with given instructions
    fn make_module(
        instructions: Vec<Instr>,
        constants: Vec<Constant>,
    ) -> crate::bytecode::Module {
        crate::bytecode::Module::new(
            Some("test".to_string()),
            constants,
            HashMap::new(),
            HashMap::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            HashMap::new(),
            instructions,
        )
    }

    #[test]
    fn test_vm_run_halt() {
        let module = make_module(vec![Instr::Halt {}], vec![]);
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Undefined);
    }

    #[test]
    fn test_vm_run_mov_halt() {
        let module = make_module(
            vec![
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(true)) },
                Instr::Halt {},
            ],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_run_load_const() {
        // Load constant into Rv, halt
        let module = make_module(
            vec![
                Instr::LoadConst { dst: Operand::Register(Register::Rv), index: Operand::Immd(0) },
                Instr::Halt {},
            ],
            vec![Constant::from("hello")],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::string("hello"));
    }

    #[test]
    fn test_vm_run_arithmetic() {
        // R0 = 10, R1 = 3, R2 = R0 + R1, Rv = R2, halt
        let module = make_module(
            vec![
                Instr::Mov { dst: Operand::Register(Register::R0), src: Operand::Primitive(Primitive::Float(10.0)) },
                Instr::Mov { dst: Operand::Register(Register::R1), src: Operand::Primitive(Primitive::Float(3.0)) },
                Instr::Addx { dst: Operand::Register(Register::R2), lhs: Operand::Register(Register::R0), rhs: Operand::Register(Register::R1) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Register(Register::R2) },
                Instr::Halt {},
            ],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Number(13.0));
    }

    #[test]
    fn test_vm_run_jump() {
        // 0: jump +2 (skip instruction 1)
        // 1: mov rv, false (should be skipped)
        // 2: mov rv, true
        // 3: halt
        let module = make_module(
            vec![
                Instr::Jump { offset: RelPc::immediate(2) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(false)) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(true)) },
                Instr::Halt {},
            ],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_run_br_if_true() {
        // 0: br_if true, +2, +1
        // 1: mov rv, false (false branch)
        // 2: mov rv, true  (true branch)
        // 3: halt
        let module = make_module(
            vec![
                Instr::BrIf { condition: Operand::Primitive(Primitive::Boolean(true)), true_target: RelPc::immediate(2), false_target: RelPc::immediate(1) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(false)) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(true)) },
                Instr::Halt {},
            ],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_run_br_if_false() {
        // 0: br_if false, +1, +2
        // 1: mov rv, true (true branch)
        // 2: mov rv, false (false branch)
        // 3: halt
        let module = make_module(
            vec![
                Instr::BrIf { condition: Operand::Primitive(Primitive::Boolean(false)), true_target: RelPc::immediate(1), false_target: RelPc::immediate(2) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(true)) },
                Instr::Mov { dst: Operand::Register(Register::Rv), src: Operand::Primitive(Primitive::Boolean(false)) },
                Instr::Halt {},
            ],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    // ──────────────────────── RuntimeError ────────────────────────

    #[test]
    fn test_runtime_error_display() {
        assert_eq!(
            format!("{}", RuntimeError::TypeError("test".to_string())),
            "TypeError: test"
        );
        assert_eq!(
            format!("{}", RuntimeError::ReferenceError("x".to_string())),
            "ReferenceError: x"
        );
        assert_eq!(
            format!("{}", RuntimeError::RangeError("overflow".to_string())),
            "RangeError: overflow"
        );
        assert_eq!(
            format!("{}", RuntimeError::InternalError("oops".to_string())),
            "InternalError: oops"
        );
        assert_eq!(
            format!("{}", RuntimeError::NotImplemented("feature".to_string())),
            "NotImplemented: feature"
        );
        assert_eq!(
            format!("{}", RuntimeError::Thrown(Value::Number(42.0))),
            "42"
        );
    }
}
