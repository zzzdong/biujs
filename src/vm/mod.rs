pub mod iterator;
pub(crate) mod builtins_dispatch;
pub(crate) mod exec;
pub(crate) mod gc;
pub mod object;
pub mod property;
pub mod prototype;
pub(crate) mod proxy;
pub(crate) mod trace;
pub(crate) mod utf16;
pub mod value;

pub use object::{
    ArrayObject, FunctionObject, JSObject, NativeFunctionObject, OrdinaryObject, new_array_object,
    new_array_object_from_vec, new_function_object, new_ordinary_object,
};
pub use property::{ObjectKind, PropertyDescriptor, PropertyKey};
pub use value::Value;

use crate::vm::gc::{Gc, Heap, Object};

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::builtins::Builtins;
use crate::vm::object::{PromiseObject, PromiseReaction, PromiseState};
use crate::vm::iterator::iterator_symbol_key;
use crate::bytecode::{
    CodeBlock, Constant, Instr, Module, Operand, Primitive, Register,
};

/// The shared, mutable cell a **captured** local binding lives in.
///
/// A closure captures the cell, never the value: both the defining frame and
/// every closure that captured it read and write the same `Rc`, which is what
/// makes `function counter() { let c = 0; return () => ++c; }` count instead of
/// answering the frozen creation-time snapshot. `None` is the uninitialised
/// (dead-zone) state; reads through the environment then raise, writes other
/// than the declaration's are refused by the compiler.
///
/// It is deliberately **not** a heap object: cells are VM bookkeeping (frames,
/// function objects, suspended generator frames), never reachable from JS, and
/// the collector reaches them through the `Trace` impls on exactly those
/// containers — `vm/trace.rs` and `vm/object.rs`.
pub type EnvCell = Rc<RefCell<Option<Value>>>;

/// A fresh cell holding `value` (`None` for the uninitialised state).
pub(crate) fn new_env_cell(value: Option<Value>) -> EnvCell {
    Rc::new(RefCell::new(value))
}

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

/// 取数工具：把 trace 写进**文件**（`File` 的写是无缓冲的系统调用），
/// 所以进程被 SIGABRT（栈溢出）杀掉时输出**不会丢行**。
///
/// 这不是可选的洁癖：`eprintln!` + abort 会**截断也会丢行**，本项目已经栽过三次
/// （`docs/archive/deep-arch-log.md` §24.1 / §38.1 / §41.3，其中 §38 那次据此得出了
/// 错误结论并排查了两轮）。另外 `cargo test` 会捕获通过的测试的 stderr
/// （§24.1），所以 `eprintln!` 还必须配 `--nocapture`。
///
/// 用 `BIUJS_TRACE=<path>` 打开；不开时只有一次 `OnceLock` 的原子读。
fn vm_trace<F: FnOnce(&mut std::fs::File)>(f: F) {
    vm_trace_with(false, f)
}

/// 同上，但只在 `BIUJS_TRACE_STEPS` 也打开时写。
///
/// 分成两个开关是实测需要：逐条指令那一行在 526 条 feature 上就是 63629 行、
/// 在一个 test262 分片上是**千万行**量级 —— 与探针混在同一个开关里，
/// 想在全量上量一把尺子就得先写几个 GB 的步骤日志。探针本身只在命中时才写，
/// 所以"全量 + 探针"是廉价的，"全量 + 逐条"才是贵的。
fn vm_trace_steps<F: FnOnce(&mut std::fs::File)>(f: F) {
    vm_trace_with(true, f)
}

fn vm_trace_with<F: FnOnce(&mut std::fs::File)>(steps: bool, f: F) {
    static TRACE: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    // 逐条那个开关**必须**也只读一次：它在 `step` 里被问了几百万次，
    // 而 `env::var_os` 每次都要走一遍环境表 + 加锁 —— 实测让 `language/statements`
    // 整个慢 29%（70s → 90s），连带把两条"又分配又死循环"的用例从"撞内存上限"
    // 推到"撞墙钟"（护栏 7/0/22 → 11/0/18，见 §44.3）。一次 `OnceLock` 读是纳秒级。
    static STEPS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let slot = TRACE.get_or_init(|| {
        std::env::var("BIUJS_TRACE")
            .ok()
            .map(|p| std::sync::Mutex::new(std::fs::File::create(p).expect("open trace file")))
    });
    if steps
        && !*STEPS.get_or_init(|| std::env::var_os("BIUJS_TRACE_STEPS").is_some())
    {
        return;
    }
    if let Some(m) = slot {
        if let Ok(mut file) = m.lock() {
            f(&mut file);
        }
    }
}

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
    /// `GetTemplateObject` 的缓存：下标是**调用点**的 site id。
    ///
    /// 规范要求同一个站点每次求值都得到**同一个**（已冻结的）模板对象，
    /// 所以缓存活在 VM 上、按 `run` 重置（一个 module 一套 site id）。
    template_objects: Vec<Option<Value>>,
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
    /// 本次运行所用模块的**每函数描述**（`CodeBlock` 的副本）。
    ///
    /// 存在的理由：`materialize_function` / `make_capturing_function_object` 需要
    /// "这个函数叫什么、几个形参"，而它们的调用点会经 `get_member` / `set_member` /
    /// `delete_member` 级联出去 —— 把 `module: &Module` 穿进那条链是一次几十处的扇出。
    /// 所以先留一份**运行期快照**：它的类型与 `Module::code_blocks` 完全一致，
    /// 于是"按函数属性"只有一个类型、一种形状。
    ///
    /// **迁移中的中间态**：M2 让帧自己持有 `code: Rc<CodeBlock>` 之后，
    /// 这份快照就该删掉（那时 `self.code()` 读当前帧，不再需要 VM 级副本）。
    /// 见 `architecture-redesign.md` §3.4 / §4.10。
    current_code: Option<Vec<CodeBlock>>,
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
    /// callbacks, `call`/`apply`, `new` from a native).    ///
    /// An exception raised inside such a frame must *not* be dispatched to a
    /// handler in the outer frame from within the nested loop: the native that
    /// requested the call (e.g. `Array.prototype.forEach`) has to see the error
    /// so it can abandon its own work. Records at or below the boundary are
    /// therefore turned back into `RuntimeError::Thrown` and propagated.
    invoke_boundaries: Vec<usize>,
    /// 宿主侧根栈（`docs/GC-DESIGN.md` §6.2）。
    ///
    /// 收集器**看不见机器栈**，所以原生内建握在 Rust 局部变量里的 `Value`
    /// 必须显式登记到这里。持有 `Rc`（而不是内嵌值）是为了让 `Root` 守卫
    /// 不借任何东西 —— 否则一个存活的守卫会把整个 VM 锁住，native 就再也
    /// 调不了 `self.invoke(..)`。
    scope: std::rc::Rc<crate::vm::gc::Scope>,
    /// 对象堆。**声明在最后**，因此按字段声明顺序析构时 heap 晚于 `state` /
    /// `builtins` 释放 —— 这是 `Gc` 句柄不悬垂的前提（`Drop for VM` 体先于
    /// 所有字段析构运行，故 `clear_wrapper_prototypes()` 也早于堆释放）。
    heap: Heap,
}

impl VM {
    /// Control-stack depth outside the innermost Rust-driven call frame, if any.
    fn invoke_boundary(&self) -> Option<usize> {
        self.invoke_boundaries.last().copied()
    }

    /// 把一个 `Value` 登记为**宿主侧根**，返回 RAII 守卫。
    ///
    /// 收集器看不见机器栈，所以原生内建握在 Rust 局部变量里的 `Value`
    /// （跨一次"回调进 JS"仍然要用的那些）必须显式登记。守卫 `Drop` 时自动注销，
    /// 且不借任何东西（持 `Rc`），所以登记后照样能继续调 `self.invoke(..)`。
    fn root(&self, v: &Value) -> crate::vm::gc::Root {
        std::rc::Rc::clone(&self.scope).root_value(v)
    }

    /// 一次登记多个 `Value`（例如原生留在本地的快照）。
    fn root_all(&self, vs: &[Value]) -> crate::vm::gc::Roots {
        std::rc::Rc::clone(&self.scope).root_all(vs)
    }

    /// 一个**可增长**的根组：原生边算边累积时，每产出一个值就 `push` 一次。
    fn roots(&self) -> crate::vm::gc::Roots {
        std::rc::Rc::clone(&self.scope).root_all(&[])
    }

    /// Whether `val` carries the `length` + integer-key shape the
    /// `Array.prototype` methods operate on (real arrays, array-likes, string
    /// wrappers). Strings are excluded: they use substring search.
    fn receiver_is_array_like(&mut self, val: &Value) -> bool {
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
                boxed = crate::builtins::to_object(&mut self.heap, other).ok();
                match &boxed {
                    Some(value) => value,
                    None => return false,
                }
            }
        };
        match target {
            Value::Object(obj_ref) => crate::vm::prototype::internal_has_property(
                *obj_ref,
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
pub(crate) enum FrameMode {
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
        /// 帧的函数对象与 `new.target`（`Reflect.construct` 的第三个参数）。
        new_target: Value,
    },
}

/// 一次 `[[Call]]` 的结果（P1-2c，见 `docs/archive/interpreter-refactor.md` §3.3.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    /// 帧已压好（`open_frame` 做过）：**回到分派循环**去跑被调方。
    ///
    /// 普通调用走这一条 —— 所以它不能变成"起一层嵌套 `step` 循环"：这些 opcode
    /// 的 arm 里 `drive_bytecode_frame` 出现 0 次就是为了这个。
    Entered,
    /// 这条指令已经算完（代理陷阱 / 原生 / 生成器 / async：这些都必须重入）。
    /// 结果已由 `enter_call` 写进 `dst`。
    Done,
}

/// 参数从哪来。**这是统一调用路径时最容易忽略、也最要命的一半**（§3.3.4）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum CallArgs<'a> {
    /// 已在数据栈上：opcode 的内联路径。参数是 lowering 事先 `Push` 上去的，
    /// 帧按 `arg0 = [rbp-1]` 原地读，**开帧时一个都不能碰**。
    OnStack(usize),
    /// 同样留在栈上（`argc` 个），但调用方**已经备好一份收集好的副本**给重入分支用。
    ///
    /// `CallMethod` 需要这个：它把每个参数 `as_object_value` 装箱了（内建层按
    /// `Value::Object` 匹配），而装箱不能发生在"帧原地读"的那条路上。
    OnStackWithCopy { argc: usize, args: &'a [Value] },
    /// 已经收集成切片（`invoke` / 原生回调 / 代理陷阱转发）。`invoke` 侧改道到这里
    /// 时才会有调用点。
    #[allow(dead_code)]
    Collected(&'a [Value]),
}

/// 一个可调用体的种类（P1-2a）。判定的**唯一**出口是 [`VM::callee_kind`]。
///
/// 以前"是不是原生 / 代理 / 生成器 / async"这套判断散在 `invoke_with_new_target`、
/// `CallEx`、`CallMethod`、`New`、`Call` 五处，**每处写法还不一样**。B45（代理可调用）
/// 与 B46（`Reflect.construct` 的 `new.target`）之所以要在好几处分别接线，就是从这些
/// 分叉漏出去的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalleeKind {
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
        // 堆先建：`state` / `builtins` 的构造都要往里分配对象。
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        let builtins = Builtins::new(&mut heap);
        builtins.register(&mut state.globals.borrow_mut(), &mut heap);
        // `globalThis.globalThis === globalThis`, as in every host.
        state
            .globals
            .borrow_mut()
            .insert("globalThis".to_string(), state.global_object.clone());
        Self {
            state,
            builtins,
            iterator_registry: HashMap::new(),
            template_objects: Vec::new(),
            next_iterator_id: 0,
            generator_registry: HashMap::new(),
            next_generator_id: 0,
            generator_send: Value::Undefined,
            generator_yielded: None,
            generator_yield_pc: 0,
            delegate_stack: Vec::new(),
            current_code: None,
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
            scope: std::rc::Rc::new(crate::vm::gc::Scope::new()),
            heap,
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
        // 换 realm：整个 `State` 换成新的（堆不换，它归 VM）。
        self.state = State::new(&mut self.heap);
        self.func_objs.clear();
        self.current_code = Some(module.code_blocks.clone());
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
        // Site ids belong to the module, so the cache is per `run` as well.
        self.template_objects.clear();
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
        self.builtins
            .register(&mut self.state.globals.borrow_mut(), &mut self.heap);
        self.state
            .globals
            .borrow_mut()
            .insert("globalThis".to_string(), self.state.global_object.clone());

        // **顶层脚本也是一个帧**（`architecture-redesign.md` §4.3 陷阱 5）。
        // 这条不是锦上添花：`pc` 现在住在 `Frame` 里，如果脚本没有帧，`pc` 就没有
        // 唯一的家 —— 于是"读 pc / 写 pc"会分叉成两条路径，正是本次深改要消灭的东西。
        // 顺带把"最外层"这个概念从"控制栈空不空"变成 `frames.len() == 1`，
        // 与 `PushC`/`PopC` 在控制栈上的簿记彻底分开。
        self.state.open_script_frame()?;

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
    /// Drain the cells pushed by `ClosureVar` for the function object being
    /// created, in the order the lowering emitted them.
    ///
    /// The pending stack is **separate** from the environment: draining it never
    /// touches the frame's own cells or the ones it inherited (see
    /// [`State::pending_captured`]).
    fn take_pending_captured_vars(&mut self) -> Vec<(String, EnvCell)> {
        std::mem::take(&mut self.state.pending_captured)
    }

    /// A fresh (never memoized) function object carrying `captured_vars`.
    ///
    /// Only used when a `function` expression actually captures something: the
    /// memoized object (`materialize_function`) is keyed by `func_id` and stands
    /// for "the" object of a declaration, which cannot carry per-evaluation
    /// captures.
    /// 本次运行的每函数描述（`CodeBlock` 的运行期快照，见 `current_code`）。
    fn code(&self, func_id: u32) -> Option<&CodeBlock> {
        self.current_code.as_ref()?.get(func_id as usize)
    }

    fn make_capturing_function_object(
        &mut self,
        func_id: u32,
        captured_vars: Vec<(String, EnvCell)>,
    ) -> Value {
        let (name, arity) = self
            .code(func_id)
            .map(|block| (block.name.clone(), block.arity))
            .unwrap_or_default();
        let obj = crate::vm::object::new_function_object(&mut self.heap, func_id, &name, arity);
        if let Value::Object(ref obj_ref) = obj {
            let mut borrowed = obj_ref.borrow_mut();
            borrowed.set_prototype(Some(self.builtins.function_prototype));
            if let Some(func_obj) = borrowed.as_function_mut() {
                func_obj.captured_vars = captured_vars;
            }
        }
        obj
    }

    fn materialize_function(&mut self, id: u32) -> Value {
        if let Some(v) = self.func_objs.get(&id) {
            return v.clone();
        }
        let (name, arity) = self
            .code(id)
            .map(|block| (block.name.clone(), block.arity))
            .unwrap_or_default();
        // `fn.length` / `fn.name` / `fn.prototype` are installed by the
        // constructor (non-writable, non-enumerable, configurable).
        let obj = crate::vm::object::new_function_object(&mut self.heap, id, &name, arity);
        // Attach Function.prototype so `f.call`, `f.bind`, … resolve.
        if let Value::Object(ref obj_ref) = obj {
            obj_ref
                .borrow_mut()
                .set_prototype(Some(self.builtins.function_prototype));
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

    /// Snapshot of an array-like receiver's elements (arrays and strings).
    fn array_like_elements(&self, val: &Value) -> Option<Vec<Value>> {
        match val {
            Value::Object(obj_ref) => {
                if let Some(arr) = obj_ref.borrow().as_array() {
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
                    .borrow().as_typed_array()
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
            _ => Some(crate::builtins::to_object(&mut self.heap, receiver)?),
        };
        let target = boxed.as_ref().unwrap_or(receiver);

        let Value::Object(obj_ref) = target else {
            return Ok(Vec::new());
        };
        let obj = *obj_ref;
        // Fast path: a plain dense array (no holes, no accessor elements) — the
        // dense slots *are* the `[[Get]]` results, so the whole walk below can be
        // skipped. Holes/accessors fall through to the generic path.
        {
            let borrowed = obj_ref.borrow();
            if let Some(dense) = borrowed.as_array().and_then(|a| a.dense_elements()) {
                return Ok(dense.iter().cloned().map(Some).collect());
            }
        }
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
            let present = crate::vm::prototype::internal_has_property(obj, &key)
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
                    &mut self.heap,
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
            return self.array_species_ordinary(len);
        }

        // Step 5: `? Get(O, "constructor")` — abrupt completions propagate.
        let mut species =
            self.get_member(receiver, &PropertyKey::from_str("constructor"), module)?;
        // Step 7: only an *object* `constructor` is asked for `@@species`, and
        // a nullish result falls back to an ordinary array.
        if species.is_object() {
            species = self.get_member(&species, &crate::builtins::species_symbol_key(), module)?;
            if species.is_null() {
                return self.array_species_ordinary(len);
            }
        }
        if species.is_undefined() {
            return self.array_species_ordinary(len);
        }
        // `Array[Symbol.species]` returns its receiver, so the common case
        // resolves back to `Array`: build it directly instead of routing every
        // `slice` through Construct.
        if crate::builtins::native_function_name(&species).as_deref() == Some("Array") {
            return self.array_species_ordinary(len);
        }
        // Step 10: `? Construct(C, «len»)` — a non-constructible species is the
        // TypeError `create-ctor-non-object.js` expects. A custom `@@species`
        // constructor decides the length policy for itself, so it is *not* subject
        // to the ordinary-array cap `array_species_ordinary` enforces below.
        let target = self.construct(&species, &[Value::Number(len as f64)], module)?;
        Ok(Some(target))
    }

    /// `ArraySpeciesCreate`'s ordinary-array result: `ArrayCreate(len)`, which ES
    /// 9.4.2.2 throws `RangeError` for `len > 2**32 - 1`. This engine's arrays are
    /// `Vec`-backed and top out at `MAX_MATERIALIZED_LEN`, so the cap fires far
    /// earlier — but it must fire *here*, before the caller materializes
    /// `vec![Value::Undefined; len]`, otherwise `Array.prototype.map.call({length: 2**32}, ...)`
    /// (and every other species-backed method) attempts a ~16 GiB allocation and
    /// crashes the whole process. That is the same "crash beats failure" hazard
    /// the TypedArray slice fixed; both stem from an unbounded `len` reaching a
    /// `Vec` materialization.
    fn array_species_ordinary(&self, len: usize) -> Result<Option<Value>, RuntimeError> {
        if len > crate::builtins::MAX_MATERIALIZED_LEN {
            return Err(RuntimeError::RangeError(
                "Invalid array length".to_string(),
            ));
        }
        Ok(None)
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
                borrowed.as_set()
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
        let iter_obj = NativeIteratorObject::new(
            &mut self.heap,
            id,
            NativeIteratorState::Set {
                set: *obj_ref,
                kind,
                idx: 0,
                done: false,
            },
        );
        let iter_val = Value::Object(self.heap.alloc(Object::NativeIterator(iter_obj)));
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
            Some(Value::Object(p)) => Some(*p),
            _ => Some(self.builtins.set_prototype),
        };
        let set_obj = crate::vm::object::SetObject::new(prototype);
        let set_val = Value::Object(self.heap.alloc(Object::Set(set_obj)));
        let _root_set = self.root(&set_val);

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
            // `adder` 同为主机侧局部量，整个循环里每次迭代都可能跑用户代码。
            let _root_adder = self.root(&adder);
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
        let map = borrowed.as_map_mut()
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
        // 宿主侧入根：下面各分支要么新建 `result` 跨 `iterator_next`/`invoke`
        // 循环存活，要么跨 `invoke` 持有 `this`（isSubsetOf 等）。收集器看不见
        // 机器栈，不登记会被回收（GC stress 下 `set_methods_operate_on_set_like_`
        // `arguments` 输出错位）。
        let _root_this = self.root(this);
        let mut roots = self.roots();
        match method {
            // Copy `this`, then append everything from `other` that is new.
            "union" => {
                let result = self.set_copy_of(this);
                roots.push(&result);
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
                    let result = set_data_new(&mut self.heap, Some(self.builtins.set_prototype));
                    roots.push(&result);
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
                    let result = set_data_new(&mut self.heap, Some(self.builtins.set_prototype));
                    roots.push(&result);
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
                    let result = set_data_new(&mut self.heap, Some(self.builtins.set_prototype));
                    roots.push(&result);
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
                roots.push(&result);
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
                roots.push(&result);
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
                let map = borrowed.as_weak_map_mut()
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
                    .borrow().as_weak_map()
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
                borrowed.as_weak_map_mut()
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
            Some(Value::Object(p)) => Some(*p),
            _ => Some(self.builtins.weakmap_prototype),
        };
        let map_obj = crate::vm::object::WeakMapObject::new(prototype);
        let map_val = Value::Object(self.heap.alloc(Object::WeakMap(map_obj)));
        let _root_wm = self.root(&map_val);
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
            let _root_adder = self.root(&adder);
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
            Some(Value::Object(p)) => Some(*p),
            _ => Some(self.builtins.weakset_prototype),
        };
        let set_obj = crate::vm::object::WeakSetObject::new(prototype);
        let set_val = Value::Object(self.heap.alloc(Object::WeakSet(set_obj)));
        let _root_ws = self.root(&set_val);
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
            let _root_adder = self.root(&adder);
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
        let iter_obj = NativeIteratorObject::new(
            &mut self.heap,
            id,
            NativeIteratorState::Js { iterator },
        );
        let iter_val = Value::Object(self.heap.alloc(Object::NativeIterator(iter_obj)));
        self.iterator_registry.insert(id, iter_val.clone());
        Ok(iter_val)
    }

    /// A fresh `Set` holding the same live entries as `this` (the starting
    /// point of `union` / `difference` / `symmetricDifference`).
    ///
    /// The result always has `%Set.prototype%`: the set methods are specified
    /// to return a plain `Set` even on a subclass receiver, and `@@species` is
    /// never consulted (`union/subclass-symbol-species.js` checks the counter).
    fn set_copy_of(&mut self, this: &Value) -> Value {
        let result = set_data_new(&mut self.heap, Some(self.builtins.set_prototype));
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
            .borrow().as_map()
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
        let map = borrowed.as_map_mut()
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
    /// `%TypedArray%.from(source, mapfn, thisArg)`（ES 22.2.2.1）。
    ///
    /// 三步：迭代出元素 → 可选回调（会跑用户代码）→ `new C(len)` 再写入。
    /// 回调期间 `items` 只活在 Rust 局部，故必须入根（收集器看不见机器栈）。
    fn typed_array_from(
        &mut self,
        ctor: &Value,
        source: &Value,
        mapfn: &Value,
        this_arg: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let mut items: Vec<Value> = Vec::new();
        let iter = self.make_iterator(source.clone(), module)?;
        loop {
            let (value, done) = self.iterator_next(iter.clone(), None, module)?;
            if done {
                break;
            }
            items.push(value);
        }
        if mapfn.is_callable() {
            let mut acc = self.roots();
            for v in items.iter() {
                acc.push(v);
            }
            for (i, v) in items.iter_mut().enumerate() {
                let mapped = self.invoke(
                    mapfn,
                    this_arg.clone(),
                    &[v.clone(), Value::Number(i as f64)],
                    module,
                )?;
                *v = mapped;
                acc.push(v);
            }
        }
        let new_ta = self.construct(ctor, &[Value::Number(items.len() as f64)], module)?;
        let _root = self.root(&new_ta);
        if let Value::Object(o) = &new_ta {
            let mut b = o.borrow_mut();
            if let Some(ta) = b.as_typed_array_mut() {
                for (i, v) in items.iter().enumerate() {
                    ta.set_element(i, v.clone());
                }
            }
        }
        Ok(new_ta)
    }

    /// `%TypedArray%.of(...items)`（ES 22.2.2.2）：`new C(items.length)` 后写入。
    fn typed_array_of(
        &mut self,
        ctor: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let new_ta = self.construct(ctor, &[Value::Number(args.len() as f64)], module)?;
        let _root = self.root(&new_ta);
        if let Value::Object(o) = &new_ta {
            let mut b = o.borrow_mut();
            if let Some(ta) = b.as_typed_array_mut() {
                for (i, v) in args.iter().enumerate() {
                    ta.set_element(i, v.clone());
                }
            }
        }
        Ok(new_ta)
    }

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
        let map_obj = crate::vm::object::MapObject::new(Some(self.builtins.map_prototype));
        let map_val = Value::Object(self.heap.alloc(Object::Map(map_obj)));
        // 宿主侧入根：`map_val` 跨下面的 `invoke` 循环存活，而每次 `invoke` 都会
        // 跑用户回调到达 GC 安全点。收集器看不见机器栈，不登记会被回收。
        let _root_map = self.root(&map_val);
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
            let map = borrowed.as_map_mut()
                .expect("Map.groupBy builds its own Map");
            // Each group is a plain array appended to in encounter order.
            let group = map.get(&key);
            match group {
                Some(Value::Object(list)) => {
                    if let Some(arr) = list.borrow_mut().as_array_mut()
                    {
                        arr.push(value);
                    }
                }
                _ => {
                    let arr = ArrayObject::from_vec(vec![value]);
                    map.set(key, Value::Object(self.heap.alloc(Object::Array(arr))));
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
                borrowed.as_map()
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
        let iter_obj = NativeIteratorObject::new(
            &mut self.heap,
            id,
            NativeIteratorState::Map {
                map: *obj_ref,
                kind,
                idx: 0,
                done: false,
            },
        );
        let iter_val = Value::Object(self.heap.alloc(Object::NativeIterator(iter_obj)));
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
            Some(Value::Object(p)) => Some(*p),
            _ => Some(self.builtins.map_prototype),
        };
        let map_obj = crate::vm::object::MapObject::new(prototype);
        let map_val = Value::Object(self.heap.alloc(Object::Map(map_obj)));
        // 宿主侧入根：下面 `make_iterator` / 回调会一路跑字节码到达 GC 安全点，
        // 而这个 map 只活在 Rust 局部变量里（收集器看不见机器栈）。
        let _root_map = self.root(&map_val);

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
            // `adder` 同样是宿主侧局部量，而整个循环里每次 `iterator_next`
            // 都可能跑用户代码 ⇒ 必须入根。
            let _root_adder = self.root(&adder);
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
    fn promise_value(promise: &Gc<Object>) -> Value {
        Value::Object(*promise)
    }

    /// A fresh promise with the engine's `Promise.prototype`.
    fn new_promise(&mut self) -> Gc<Object> {
        let proto = self.builtins.promise_prototype;
        let id = self.state.next_promise_id;
        self.state.next_promise_id += 1;
        let promise = self
            .heap
            .alloc(Object::Promise(PromiseObject::new(Some(proto), id)));
        self.state.promises.insert(id, promise);
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
    fn native_fn(heap: &mut Heap, name: &str) -> Value {
        let obj = NativeFunctionObject::new(name);
        Value::Object(heap.alloc(Object::NativeFunction(obj)))
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
                let p = borrowed.as_promise().expect("promise");
                (p.state, p.value.clone())
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
        promise: &Gc<Object>,
        value: Value,
        module: &Module,
    ) {
        if let Some(existing) = self.as_promise(&value) {
            if Gc::ptr_eq(&existing, promise) {
                // `resolve(p)` with p itself: a chaining cycle (ES 27.2.1.3.2
                // step 6.1) is a TypeError.
                let reason = crate::builtins::runtime_error_to_js_error(
                    &mut self.heap,
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
                target: *promise,
            };
            let settled = {
                let borrowed = existing.borrow();
                let p = borrowed.as_promise().expect("promise");
                if p.is_pending() {
                    None
                } else {
                    Some((p.state, p.value.clone()))
                }
            };
            match settled {
                None => existing
                    .borrow_mut()
                    .as_promise_mut()
                    .expect("promise")
                    .reactions
                    .push(forwarding),
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
                        promise: *promise,
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
                result,
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
            let ok = Self::native_fn(&mut self.heap, &format!("__promise_agg_ok__{id}_{index}"));
            let err = Self::native_fn(&mut self.heap, &format!("__promise_agg_err__{id}_{index}"));
            self.promise_then(&element_promise, Some(ok), Some(err));
        }
        Self::promise_value(&result)
    }

    /// Settle an aggregation whose element list is empty.
    fn finish_empty_aggregate(&mut self, id: u64, kind: AggregateKind) -> Value {
        let result = {
            let Some(agg) = self.state.aggregations.get(&id) else {
                return Value::Undefined;
            };
            agg.result
        };
        self.state.aggregations.remove(&id);
        match kind {
            AggregateKind::All | AggregateKind::AllSettled => {
                let array = Value::Object(self.heap.alloc(Object::Array(
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
    fn aggregate_error(&mut self, errors: Vec<Value>) -> Value {
        let prototype = self.builtins.error_prototype;
        let mut value = crate::builtins::create_error_object(
            &mut self.heap,
            Some("All promises were rejected".to_string()),
            prototype,
            "AggregateError",
        );
        let array = Value::Object(self.heap.alloc(Object::Array(
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
                        agg.result
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
                    (agg.remaining == 0, agg.result)
                };
                if complete {
                    let values = self
                        .state
                        .aggregations
                        .remove(&id)
                        .map(|a| a.values)
                        .unwrap_or_default();
                    let array = Value::Object(self.heap.alloc(Object::Array(
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
                    agg.result
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
                    agg.values[index] = Value::Object(self.heap.alloc(Object::Ordinary(obj)));
                    agg.remaining -= 1;
                    (agg.remaining == 0, agg.result)
                };
                if complete {
                    let values = self
                        .state
                        .aggregations
                        .remove(&id)
                        .map(|a| a.values)
                        .unwrap_or_default();
                    let array = Value::Object(self.heap.alloc(Object::Array(
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
                        agg.result
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
                        agg.result,
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
        promise: &Gc<Object>,
        state: PromiseState,
        value: Value,
    ) {
        let reactions = {
            let mut borrowed = promise.borrow_mut();
            let p = borrowed.as_promise_mut().expect("promise");
            if !p.is_pending() {
                return;
            }
            p.state = state;
            p.value = value.clone();
            std::mem::take(&mut p.reactions)
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
    fn as_promise(&self, value: &Value) -> Option<Gc<Object>> {
        let Value::Object(obj_ref) = value else {
            return None;
        };
        if obj_ref.borrow().as_promise().is_none() {
            return None;
        }
        Some(*obj_ref)
    }

    /// `promise.then(onFulfilled, onRejected)` — always asynchronous, so a
    /// settled promise still queues its reaction.
    fn promise_then(
        &mut self,
        promise: &Gc<Object>,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
    ) -> Value {
        let child = self.new_promise();
        let reaction = PromiseReaction {
            on_fulfilled,
            on_rejected,
            target: child,
        };
        let settled = {
            let borrowed = promise.borrow();
            let p = borrowed.as_promise().expect("promise");
            if p.is_pending() {
                None
            } else {
                Some((p.state, p.value.clone()))
            }
        };
        match settled {
            None => promise
                .borrow_mut()
                .as_promise_mut()
                .expect("promise")
                .reactions
                .push(reaction),
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
                    let id = promise.borrow().as_promise().expect("promise").id;
                    let resolve =
                        Self::native_fn(&mut self.heap, &format!("__promise_resolve__{id}"));
                    let reject =
                        Self::native_fn(&mut self.heap, &format!("__promise_reject__{id}"));
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
    fn error_value(&mut self, err: RuntimeError) -> Value {
        match err {
            RuntimeError::Thrown(value) => value,
            other => crate::builtins::runtime_error_to_js_error(
                &mut self.heap,
                &other,
                &self.builtins,
            ),
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
                let promise = self
                    .heap
                    .alloc(Object::Promise(PromiseObject::new(Some(p), id)));
                self.state.promises.insert(id, promise);
                promise
            }
            _ => self.new_promise(),
        };
        let id = promise.borrow().as_promise().expect("promise").id;
        let resolve = Self::native_fn(&mut self.heap, &format!("__promise_resolve__{id}"));
        let reject = Self::native_fn(&mut self.heap, &format!("__promise_reject__{id}"));
        match self.invoke(executor, Value::Undefined, &[resolve, reject], module) {
            Ok(_) => {}
            Err(err) => {
                let value = self.error_value(err);
                self.promise_settle(&promise, PromiseState::Rejected, value);
            }
        }
        Ok(Self::promise_value(&promise))
    }


    /// Read the arguments a caller pushed for a dynamic call.
    ///
    /// 实参仍在栈上、就在 `rsp` 之下：arg0 在 `[rsp-1]`、arg1 在 `[rsp-2]`……
    /// （M2-2a 第 1 步·扩张：以前读 `rbp`，靠"调用点已把 `rbp` 切到实参窗口"。
    ///  此刻 `rsp` 与那个 `rbp` 相等，所以改成 `rsp` 相对是**纯等价** ——
    ///  切换还没搬走，程序行为不变。**同样的读法有三份副本**，三份都要改。） (they were pushed in reverse order).
    fn collect_call_args(&self, arg_count: usize) -> Result<Vec<Value>, RuntimeError> {
        let mut args = Vec::with_capacity(arg_count);
        for i in 0..arg_count {
            let index = self.state.rsp - i - 1;
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
    /// **退帧的唯一出口**：退到 `frame_depth`（留下 `frames[..frame_depth]`，于是
    /// `frames[frame_depth - 1]` 成为当前帧），并把两条"按帧存集合"的栈截到水位。
    ///
    /// 走这里的四条路径：`Ret`（正常返回）、异常回退（`handle_throw`）、
    /// Rust 驱动调用的恢复（`drive_bytecode_frame`）、生成器恢复
    /// （`restore_execution_state` / `discard_generator_frame`）。
    ///
    /// **为什么要合并**：以前"退一帧要做哪些 truncate"这个知识被复制了四份，
    /// 而切片 1b 已经吃过一次这个亏 —— 同挤一条 `ctrl_stack` 时一次 truncate 顺手管两样，
    /// 拆开之后那个"顺手"在别的路径上**沉默丢失**（`rbp` 被破坏，§14.3）。
    /// 合并之后这类附带清理只有一处可写。
    ///
    /// **水位由调用点给，不在这里从帧里推**：三路的来源本就不同 ——
    /// `Ret` 用本帧记录的水位，异常回退用 `SehRecord` 在 `try` 处记的水位，
    /// Rust 驱动调用与生成器用 `SavedExecutionState` 记的。统一成"从帧里推"会改语义
    /// （例如 `try` 之后又压了闭包映射，两条水位就不相等了）。
    ///
    /// **`rbp` 反过来**：它是活跃帧的派生值，所以退栈之后由这里一步定到位 ——
    /// 以前这件事靠每个调用点的一对 `PushC rbp` / `PopC rbp` 去搬（M2-2b 之前还
    /// 要靠三位快照），而"退帧"和"搬 `rbp`"是两件事写在两处，谁漏一处就沉默损坏。
    /// 两处**生成器路径**（`handle_throw` / `restore_execution_state`）在调用之后
    /// 用它们的快照覆盖这个值 —— 那些快照属于挂起前那一次几何（§44.2 实测）。
    fn unwind_frames_to(
        &mut self,
        frame_depth: usize,
        closure_depth: usize,
        seh_depth: usize,
    ) {
        // 帧自己带着 `this` / `this_state` / `argc` / `func` / `pc` /
        // `construct` / `new.target`，所以"回退到第几帧"一步到位 ——
        // 切片 1b/1c/1d/1e 之前这里要分别 truncate 七条平行栈。
        // `rsp` 要取**被弹出的那一帧**的出生值（不是留下来的那一帧的）：
        // `frames[d]` 是从 `frames[d-1]` 里被调起来的，它的出生 `rsp` 就是"退回
        // 到 `d-1` 时该有的 `rsp"`。取留下来的那一帧的 `rsp` 是错的 —— 那是它
        // 自己开帧时的值，而它此后早已继续压栈。
        let restore_rsp = if frame_depth < self.state.frames.len() {
            Some(self.state.frames[frame_depth].rsp)
        } else {
            None
        };
        self.state.frames.truncate(frame_depth);
        self.state.closure_var_stack.truncate(closure_depth);
        // `pending_captured` only ever holds entries between a `ClosureVar` and
        // the `Make*FuncObj` that drains it — no user code runs in between, so a
        // frame can only be closing with its *own* (always empty) set here.
        // Clearing is hygiene, not semantics.
        self.state.pending_captured.clear();
        self.state.seh_stack.truncate(seh_depth);
        self.state.rbp = self.state.frame_base();
        if let Some(rsp) = restore_rsp {
            self.state.rsp = rsp;
        }
    }

    fn handle_throw(&mut self, exc_val: Value) -> Result<(), RuntimeError> {
        // An exception raised inside a Rust-driven call frame must not be
        // dispatched to a handler *outside* that frame from here: the native
        // that asked for the call (a callback loop, `new` from a builtin, …)
        // has to see the error so it can stop its own work. Turn it back into
        // a thrown value and let `invoke_*` propagate it to the outer step.
        if let Some(boundary) = self.invoke_boundary() {
            if let Some(top) = self.state.seh_stack.last() {
                if top.saved_frame_depth <= boundary {
                    return Err(RuntimeError::Thrown(exc_val));
                }
            }
        }
        match self.state.seh_stack.pop() {
            Some(mut record) => {
                // 下面这一次 `unwind_frames_to` 就把帧、`closure_var_stack`、
                // `seh_stack` 全截到位 —— 不需要再额外清什么。
                //
                // （控制栈还在的年代这里必须多做一步：被回退的帧会留下没来得及
                // `PopC` 的保存项，外层之后的 `PopC` 会弹到过期值，实测症状是
                // "数组字面量里夹一次会抛异常的调用" 直接报 `ArrayPush on non-object`。
                // 控制栈与 `PushC`/`PopC` 删掉之后，这类残留不再可能存在。）
                self.unwind_frames_to(
                    record.saved_frame_depth,
                    record.saved_closure_depth,
                    // 处理器这条记录刚刚被 `seh_stack.pop()` 掉了，所以"退到哪"就是
                    // 现在的长度（不再额外截）。
                    self.state.seh_stack.len(),
                );
                // 快照**压过** `unwind_frames_to` 刚定好的派生值（§44.2）：`SehRecord`
                // 跟着生成器一起挂起（`restore_generator_frame` 把 `frame.seh` 整份带回来），
                // 而恢复时帧的几何已经挪到新的栈底 —— 这里的值属于**挂起前**那一次。
                // 全量 test262 上 8 处与派生值不等（全部 `language/expressions/yield/*`）。
                self.state.rbp = record.saved_rbp;
                // `rsp` 同理：这份快照也是在 `try` 处记的，属于**挂起前**那一次几何。
                self.state.rsp = record.saved_rsp;
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
                    // UTF-16 码元数：`s.len()` 是字节数（"é" 会报 2）。
                    "length" => Ok(Value::Number(crate::vm::utf16::length(s) as f64)),
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
    fn primitive_prototype(&self, val: &Value) -> Option<Gc<Object>> {
        match val {
            Value::String(_) => Some(self.builtins.string_prototype),
            Value::Number(_) => Some(self.builtins.number_prototype),
            Value::Bool(_) => Some(self.builtins.boolean_prototype),
            Value::Symbol(_) => Some(self.builtins.symbol_prototype),
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
                    let proxy_val = Value::Object(*obj_ref);
                    if let Some(value) = self.proxy_get(&proxy_val, key, &proxy_val, module)? {
                        return Ok(value);
                    }
                }
                // Accessor properties have to invoke their getter, which needs a
                // re-entrant call — hence the descriptor search here.
                if let Some((owner, desc)) =
                    crate::vm::prototype::find_descriptor(*obj_ref, key)
                        .map_err(RuntimeError::TypeError)?
                {
                    if desc.is_accessor_descriptor() {
                        // `this` is the *receiver* (the object the property was
                        // read from), not the object that happens to hold the
                        // descriptor — class accessors live on the prototype.
                        let receiver = Value::Object(*obj_ref);
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
                    return Ok(Value::Number(crate::vm::utf16::length(s) as f64));
                }
                if let Ok(idx) = name.parse::<usize>() {
                    // `s[i]` 取的是**码元**：非 BMP 字符会得到半个代理对。
                    return Ok(s
                        .encode_utf16()
                        .nth(idx)
                        .map(|unit| Value::string(&crate::vm::utf16::from_units(&[unit])))
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
        proto: &Option<Gc<Object>>,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let Some(proto_ref) = proto else {
            return Ok(Value::Undefined);
        };
        if let Some((_owner, desc)) =
            crate::vm::prototype::find_descriptor(*proto_ref, key)
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
            let proxy_val = Value::Object(*obj_ref);
            if let Some(value) = self.proxy_get(&proxy_val, key, receiver, module)? {
                return Ok(value);
            }
        }
        if let Some((_owner, desc)) = crate::vm::prototype::find_descriptor(*obj_ref, key)
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
            let proxy_val = Value::Object(*obj_ref);
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
        proto: &Option<Gc<Object>>,
        key: &PropertyKey,
    ) -> Result<Value, RuntimeError> {
        match proto {
            Some(p) => crate::vm::prototype::internal_get(*p, key, None)
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
                    let proxy_val = Value::Object(*obj_ref);
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
                    crate::vm::prototype::find_descriptor(*obj_ref, &key)
                        .map_err(RuntimeError::TypeError)?
                {
                    if desc.is_accessor_descriptor() {
                        // Same receiver rule as `get_member` above.
                        let receiver = Value::Object(*obj_ref);
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
                let proxy_val = Value::Object(*obj_ref);
                match self.proxy_delete(&proxy_val, key, module)? {
                    Some(removed) => Ok(removed),
                    None => crate::vm::prototype::internal_delete(*obj_ref, key)
                        .map_err(RuntimeError::TypeError),
                }
            }
            Value::Object(obj_ref) => {
                crate::vm::prototype::internal_delete(*obj_ref, key)
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
                    let proxy_val = Value::Object(*obj_ref);
                    if let Some(value) = self.proxy_get(&proxy_val, &key, &proxy_val, module)? {
                        return Ok(value);
                    }
                }
                crate::vm::prototype::internal_get(obj_ref.clone(), &key, None)
                    .map_err(RuntimeError::TypeError)
            }
            Value::String(s) => {
                if prop_name == "length" {
                    Ok(Value::Number(crate::vm::utf16::length(s) as f64))
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

    /// The **cached** template object of a tagged-template call site.
    ///
    /// ES `GetTemplateObject`: one array per *site*, shared by every evaluation,
    /// carrying a `raw` array of the un-escaped strings; both are frozen so the
    /// elements and `length` are non-writable / non-configurable. A fresh object
    /// per evaluation would fail `cache-same-site` (identity) and every
    /// `verifyProperty` on the elements.
    fn make_template_object(
        &mut self,
        site: usize,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if let Some(Some(cached)) = self.template_objects.get(site) {
            return Ok(cached.clone());
        }
        let Some(desc) = module.templates.get(site) else {
            // Only a hand-built module can get here: `codegen` and this table
            // are written by the same lowering pass.
            return Err(RuntimeError::InternalError(format!(
                "unknown template site {site}"
            )));
        };
        let cooked: Vec<Value> = desc
            .cooked
            .iter()
            .map(|cooked| match cooked {
                Some(text) => Value::string(text),
                // Invalid escape: legal in a *tagged* template since ES 2018,
                // and the cooked entry is `undefined`.
                None => Value::Undefined,
            })
            .collect();
        let cooked_val = crate::vm::object::new_array_object_from_vec(&mut self.heap, cooked);
        let raw: Vec<Value> = desc.raw.iter().map(|text| Value::string(text)).collect();
        let raw_val = crate::vm::object::new_array_object_from_vec(&mut self.heap, raw);

        if let Value::Object(raw_ref) = &raw_val {
            raw_ref.borrow_mut().freeze();
        }
        if let Value::Object(strings_ref) = &cooked_val {
            let raw_desc = crate::vm::property::PropertyDescriptor {
                value: raw_val,
                writable: false,
                enumerable: false,
                configurable: false,
                getter: None,
                setter: None,
            };
            strings_ref
                .borrow_mut()
                .define_property(PropertyKey::from_str("raw"), raw_desc)
                .map_err(RuntimeError::TypeError)?;
            strings_ref.borrow_mut().freeze();
        }

        if self.template_objects.len() <= site {
            self.template_objects.resize(site + 1, None);
        }
        self.template_objects[site] = Some(cooked_val.clone());
        Ok(cooked_val)
    }

    /// Build the generator object a call to `function*` produces.
    ///
    /// The body does not run yet: it starts on the first `next()`.
    pub fn create_generator(
        &mut self,
        func_id: u32,
        this: Value,
        args: Vec<Value>,
        captured_vars: Vec<(String, EnvCell)>,
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
        let mut gobj =
            crate::vm::object::GeneratorObject::new(&mut self.heap, id, func_id, args, this);
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
        let value = Value::Object(self.heap.alloc(Object::Generator(gobj)));
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

    /// The environment base of the frame currently on top.
    ///
    /// `run_generator_frame*` needs the *base* (what the frame owns), not the
    /// current top: the body may have pushed more cells since it opened.
    fn current_frame_closure_depth(&self) -> usize {
        self.state
            .frames
            .last()
            .map_or(self.state.closure_var_stack.len(), |f| f.closure_depth)
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
        captured_vars: &[(String, EnvCell)],
        module: &Module,
    ) -> Result<(), RuntimeError> {
        let Some(location) = module.entry_pc(func_id) else {
            return Err(RuntimeError::ReferenceError(format!(
                "undefined function: {func_id}"
            )));
        };
        for arg in args.iter().rev() {
            self.state.push(arg.clone())?;
        }
        self.state.rbp = self.state.rsp;
        self.state.enter_frame()?;
        let callee_func = self.materialize_function(func_id);
        // Same boundary rule as `open_frame`: the frame's environment starts
        // before the inherited cells, so `env_cell` can scan from here.
        let closure_depth = self.state.closure_var_stack.len();
        for (name, cell) in captured_vars {
            self.state.push_env_cell(name.clone(), cell.clone());
        }
        let seh_depth = self.state.seh_stack.len();
                        self.state.note_frame(FrameBirth {
            pc: location,
            func: callee_func,
            this: this.clone(),
            this_state: THIS_NONE,
            argc: args.len(),
            construct: false,
            new_target: Value::Undefined,
            closure_depth,
            seh_depth,
        });
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
        captured_vars: &[(String, EnvCell)],
        module: &Module,
    ) -> Result<(), RuntimeError> {
        let saved = self.save_execution_state();
        // Same depth bookkeeping as `generator_next`: the frame's own open
        // delegates and closure maps are what the snapshot has to carry.
        let delegate_depth = self.delegate_stack.len();
        self.push_generator_frame(func_id, this, args, captured_vars, module)?;
        // The prologue's snapshot boundary is the frame's environment *base*,
        // the same value `push_generator_frame` recorded — everything the frame
        // owns (inherited cells and its own `MakeCell`s alike) has to be in it.
        let closure_depth = self
            .state
            .frames
            .last()
            .map_or(self.state.closure_var_stack.len(), |f| f.closure_depth);
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
            let Some(gobj) = borrowed.as_generator()
            else {
                return Err(RuntimeError::TypeError("not a generator".to_string()));
            };
            gobj.state
        };
        if state == crate::vm::object::GeneratorState::Completed {
            return Ok(Self::iterator_result(&mut self.heap, Value::Undefined, true));
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
                match gobj.as_generator_mut()
                {
                    Some(gobj) if gobj.suspended.is_some() => {
                        gobj.state = crate::vm::object::GeneratorState::Executing;
                        gobj.suspended.take().map(|f| *f)
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
                let closure_depth = self.current_frame_closure_depth();
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
                let gobj = gobj.as_generator()
                    .unwrap();
                match module.entry_pc(gobj.func_id) {
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
                let gobj = gobj.as_generator_mut()
                    .unwrap();
                gobj.state = crate::vm::object::GeneratorState::Executing;
                match gobj.suspended.take() {
                    Some(frame) => *frame,
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
    ///   `Ret` (`CodeBlock::exit_pc`). That is what makes `Opcode::Ret`'s `finally`
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
            let Some(gobj) = borrowed.as_generator_mut()
            else {
                return Err(RuntimeError::TypeError("not a generator".to_string()));
            };
            let state = gobj.state;
            let func_id = gobj.func_id;
            let frame = if state == crate::vm::object::GeneratorState::SuspendedYield {
                gobj.state = crate::vm::object::GeneratorState::Executing;
                gobj.suspended.take().map(|f| *f)
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
                Ok(value) => Ok(Self::iterator_result(&mut self.heap, value, true)),
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
                        //
                        // **没有尾 `Ret` 时也要把重入点显式定下来**：置哨兵 = "这次恢复
                        // 没有代码要跑"。以前这里是 `module.func_exit_pc(func_id)` 直接返回
                        // `None` + 下面"不 jump"，于是 `run_generator_frame` 会从**当前 pc**
                        // 开始跑 —— 而那是**调用方**停住的地方。实测（`for-of` + `try{break}`
                        // 里 `IteratorClose` 触发的那条路）：pc = 21 = 调用方的 `iter_close`，
                        // 也就是把调用方的那条指令当成生成器的代码又执行了一遍。
                        // 今天它靠"再进一次 `generator_abrupt` 会因已无挂起帧而立刻返回"
                        // 自终止 —— 纯属巧合。显式置哨兵把这个巧合变成契约，
                        // 也是 `pc` 进帧（`deep-arch-log.md` §11.8）的前置。
                        Some(
                            module
                                .func_exit_pc(func_id)
                                .unwrap_or(module.instructions.len()),
                        )
                    }
                    Err(err) => match self.deliver_into_frame(err, saved.frame_depth) {
                        // 异常被交付**进帧里**：`handle_throw` 已经把 pc 设到处理器上，
                        // 所以这里**不能**覆盖它 —— 这是 `None` 唯一合法的用法。
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
                        Ok(Self::iterator_result(&mut self.heap, value, true))
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
                            return Ok(Self::iterator_result(&mut self.heap, value, false));
                        }
                        self.mark_generator_completed(&gen_value);
                        return Ok(Self::iterator_result(&mut self.heap, value, true));
                    }
                }
                // No delegate `throw`: the completion lands on the `yield`
                // itself, so the frame's own `try`/`finally` gets its chance.
                self.restore_generator_frame(&frame, module)?;
                match self.handle_throw(reason) {
                    Ok(()) => {
                        let closure_depth = self.current_frame_closure_depth();
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
    /// The current frame's function object (`FunctionObject`, or a bare
    /// `Value::Function` reference before materialization).
    /// 当前帧的函数对象。
    ///
    /// **判定的唯一来源**。以前是 `State::function_val` 外加一条平行的
    /// `function_stack`（存调用方的值）—— 那两套记账与"帧是谁"是分离的，
    /// 一次嵌套运行之后可以对不上（§9.3 实测：`frames.last().func = objFn#5`
    /// 而 `function_val = objFn#1`）。现在它由帧回答，那种分叉按构造消失。
    ///
    /// 没有帧时返回 `undefined`：`run` 在任何指令之前总会压顶层脚本帧，
    /// 所以运行期不该出现空 `frames`（与 `State::pc()` 同一条约定）。
    fn current_func(&self) -> Value {
        self.state
            .frames
            .last()
            .map(|frame| frame.func.clone())
            .unwrap_or(Value::Undefined)
    }

    /// 当前帧的 `this` 绑定。
    ///
    /// **判定的唯一来源**。取代了 `State::this_val` + 平行的 `this_stack` 那两套记账。
    /// 没有帧时返回 `undefined`，与 `State::pc()` / `VM::current_func()` 同一条约定。
    fn current_this(&self) -> Value {
        self.state
            .frames
            .last()
            .map(|frame| frame.this.clone())
            .unwrap_or(Value::Undefined)
    }

    /// 当前帧的实参个数（`Opcode::Arguments` / `MakeRest` 与"没传的形参读 undefined"
    /// 那条规则都靠它）。
    fn current_argc(&self) -> usize {
        self.state.frames.last().map_or(0, |frame| frame.argc)
    }

    /// 当前帧的 `new.target`。箭头函数在创建时快照这个值，所以它也是每帧的。
    fn current_new_target(&self) -> Value {
        self.state
            .frames
            .last()
            .map(|frame| frame.new_target.clone())
            .unwrap_or(Value::Undefined)
    }

    fn current_function_id(&self) -> Option<u32> {
        match &self.current_func() {
            Value::Function(id) => Some(*id),
            Value::Object(obj_ref) => obj_ref
                .borrow().as_function()
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
        self.invoke_boundaries.push(saved.frame_depth);
        self.generator_yielded = None;
        // 出口改成"回到 resumer 那一帧"：生成器**返回**时 `Ret` 把它收掉；
        // 而 `yield` / `PrologueEnd` 泊车时帧**还在**，靠 `step` 返回 `false`
        // （它们把**自己那帧**的 pc 停在哨兵上）退出循环 —— 帧留着是刻意的，
        // 挂起态就是那个没被收掉的帧。
        //
        // 边界取 `saved.frame_depth`（`save_execution_state` 在**装帧之前**取的），
        // 而不是 `frames.len() - 1`：后者等于"假设生成器帧一定装好了"。
        let boundary = saved.frame_depth;
        let outcome = (|| -> Result<(), RuntimeError> {
            loop {
                if self.state.frames.len() <= boundary {
                    break;
                }
                if !self.step(module)? {
                    break;
                }
            }
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
    /// control entries are gone with `frames` itself. The value
    /// stack and the other stacks are rewound by `restore_execution_state`.
    fn discard_generator_frame(&mut self) {
        // `Ret` 没跑，所以帧的**附带状态**（它自己的闭包映射 / try 记录）也没被收 ——
        // 按本帧记录的水位、走同一个"退帧"助手收掉。
        let (closure_depth, seh_depth) = self
            .state
            .frames
            .last()
            .map(|frame| (frame.closure_depth, frame.seh_depth))
            .unwrap_or((0, 0));
        self.unwind_frames_to(
            self.state.frames.len().saturating_sub(1),
            closure_depth,
            seh_depth,
        );
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
                Self::iterator_result(&mut self.heap, value, false)
            }
            _ => {
                self.mark_generator_completed(gen_value);
                Self::iterator_result(&mut self.heap, run.rv, true)
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
            if let Some(gobj) = gobj.as_generator_mut()
            {
                gobj.state = state;
                gobj.suspended = Some(Box::new(frame));
            }
        }
    }

    fn mark_generator_completed(&mut self, gen_value: &Value) {
        if let Value::Object(obj_ref) = gen_value {
            let mut gobj = obj_ref.borrow_mut();
            if let Some(gobj) = gobj.as_generator_mut()
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
        let it = borrowed.as_native_iterator()?;
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
    /// `CodeBlock::exit_pc`).
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
        let callee_func = frame.function_val.clone();
        self.state.enter_frame()?;
        // Same boundary rule as `open_frame`: the restored maps belong to this
        // frame, so its environment starts *before* them.
        let closure_depth = self.state.closure_var_stack.len();
        for map in &frame.closure_maps {
            self.state.closure_var_stack.push(map.clone());
        }
        // The body's live exception handlers come back with the frame; a
        // `finally` that had not run yet must still run.
        self.state.seh_stack.extend(frame.seh.iter().cloned());
        let seh_depth = self.state.seh_stack.len();
                        self.state.note_frame(FrameBirth {
            pc: module.instructions.len(),
            func: callee_func,
            this: frame.this.clone(),
            this_state: THIS_NONE,
            argc: frame.argc,
            construct: false,
            new_target: Value::Undefined,
            closure_depth,
            seh_depth,
        });
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
    fn iterator_result(heap: &mut Heap, value: Value, done: bool) -> Value {
        let mut result = crate::vm::object::OrdinaryObject::new();
        let _ = result.property_set(PropertyKey::from_str("value"), value);
        let _ = result.property_set(PropertyKey::from_str("done"), Value::Bool(done));
        Value::Object(heap.alloc(Object::Ordinary(result)))
    }

    /// Snapshot of everything a nested call must restore afterwards — the same
    /// inventory `invoke_with_new_target` saves.
    fn save_execution_state(&self) -> SavedExecutionState {
        // 常驻的**不变量监视器**（不是一次性探针）：`rbp` 必须恒等于当前帧的基址。
        // 违反它意味着又有地方把 `rbp` 当成了"调用准备 / 原生实参参照系"用 ——
        // 那正是 §25/§42 那两轮连续回退的根源，所以监视器留在源码里而不是测完就撤。
        //
        // 取样点选在保存现场：这是**唯一**会把这整套状态交给"嵌套执行"的地方
        // （生成器体 / 原生回调进来的解释器循环），也就是最容易丢掉"当前帧是谁"的地方。
        vm_trace(|w| {
            let base = self.state.frames.last().map(|frame| frame.base);
            if base != Some(self.state.rbp) {
                let _ = std::io::Write::write_fmt(
                    w,
                    format_args!(
                        "M2 探针(save): rbp={} 所在帧 base={:?} frames={} rsp={}\n",
                        self.state.rbp,
                        base,
                        self.state.frames.len(),
                        self.state.rsp,
                    ),
                );
            }
        });
        SavedExecutionState {
            rsp: self.state.rsp,
            // **不能**省：生成器体的帧在挂起前记下这里，恢复时几何已经挪了
            // （`restore_generator_frame` 按新的栈底重建），实测 5 处不匹配，
            // 全部在 `language/expressions/yield/*` 与 class 计算属性名那几条（§44.2）。
            rbp: self.state.rbp,
            closure: self.state.closure_var_stack.len(),
            seh: self.state.seh_stack.len(),
            frame_depth: self.state.frames.len(),
            delegate: self.delegate_stack.len(),
            registers: std::array::from_fn(|i| self.state.registers[i].clone()),
        }
    }

    fn restore_execution_state(&mut self, saved: &SavedExecutionState) {
        self.unwind_frames_to(saved.frame_depth, saved.closure, saved.seh);
        // 快照**压过** `unwind_frames_to` 刚定好的派生值 —— 与 `save_execution_state`
        // 里那条注释成对：这两份都属于挂起前的几何（§44.2 实测 5 处不等）。
        self.state.rbp = saved.rbp;
        self.state.rsp = saved.rsp;
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
        let argc = self.current_argc();
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
            this: self.current_this(),
            function_val: self.current_func(),
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
    /// `%TypedArray%.prototype` 的 `keys()` / `values()` / `entries()`。
    ///
    /// 三者都返回**迭代器**，只有 VM 能构造。做法是复用 `Array` 那条快照迭代器
    /// （`NativeIteratorState::Array`），只是预先把 items 算成各自该有的形态：
    ///   `values`  → 元素值
    ///   `keys`    → 下标（数字）
    ///   `entries` → `[下标, 值]` 的二元数组
    fn make_typed_array_iterator(
        &mut self,
        receiver: &Value,
        method: &str,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::iterator::{NativeIteratorObject, NativeIteratorState};
        let (len, elements) = {
            let Value::Object(o) = receiver else {
                return Err(RuntimeError::TypeError(
                    "TypedArray iterator called on a non-object".to_string(),
                ));
            };
            let mut b = o.borrow_mut();
            let Some(ta) = b.as_typed_array_mut() else {
                return Err(RuntimeError::TypeError(
                    "TypedArray iterator called on a non-TypedArray".to_string(),
                ));
            };
            let len = ta.length;
            let mut els = Vec::with_capacity(len);
            for i in 0..len {
                els.push(ta.get_element(i));
            }
            (len, els)
        };
        // 入根：`entries` 要为每个下标新建数组，期间会分配。
        let _roots = self.root_all(&elements);
        let items: Vec<Value> = match method {
            "keys" => (0..len).map(|i| Value::Number(i as f64)).collect(),
            "values" => elements,
            // `entries`
            _ => (0..len)
                .map(|i| {
                    crate::vm::object::new_array_object_from_vec(
                        &mut self.heap,
                        vec![Value::Number(i as f64), elements[i].clone()],
                    )
                })
                .collect(),
        };
        let id = self.next_iterator_id;
        self.next_iterator_id += 1;
        let iter_obj =
            NativeIteratorObject::new(&mut self.heap, id, NativeIteratorState::Array { items, idx: 0 });
        let iter_val = Value::Object(self.heap.alloc(Object::NativeIterator(iter_obj)));
        self.iterator_registry.insert(id, iter_val.clone());
        Ok(iter_val)
    }

    fn make_iterator(&mut self, src: Value, module: &Module) -> Result<Value, RuntimeError> {
        use crate::vm::iterator::NativeIteratorState;
        if std::env::var("BIUJS_TRACE_ITER").is_ok() {
            eprintln!("[make_iter] src_type={}", src.type_of());
        }

        // A native iterator is its own iterator, so `for (x of arr.keys())`
        // must not try to call `[Symbol.iterator]` on it (which would recurse).
        if let Value::Object(obj_ref) = &src {
            if obj_ref
                .borrow().as_native_iterator()
                .is_some()
            {
                let id = self.next_iterator_id;
                self.next_iterator_id += 1;
                let iter_obj = crate::vm::iterator::NativeIteratorObject::new(
                    &mut self.heap,
                    id,
                    crate::vm::iterator::NativeIteratorState::Js {
                        iterator: src.clone(),
                    },
                );
                let iter_val = Value::Object(self.heap.alloc(Object::NativeIterator(iter_obj)));
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
            Value::Object(_) if is_default_factory => {
                // Read the kind and release the borrow: the element list below is
                // read through `[[Get]]`, which needs `&mut self`.
                let kind = match &src {
                    Value::Object(obj_ref) => obj_ref.borrow().kind(),
                    _ => unreachable!(),
                };
                // The elements are read through `[[Get]]` (`array_like_entries`),
                // not out of the dense store: `Object.defineProperty(a, 0, {get})`
                // must be *invoked* by `[...a]` / `for (x of a)` / `f(...a)`, and
                // a hole yields `undefined` (ES 23.1.5.2.1).
                let read_entries = |vm: &mut Self| -> Result<Vec<Value>, RuntimeError> {
                    Ok(vm
                        .array_like_entries(&src, module)?
                        .into_iter()
                        .map(|e| e.unwrap_or(Value::Undefined))
                        .collect())
                };
                match kind {
                    ObjectKind::Array => Some(NativeIteratorState::Array {
                        items: read_entries(self)?,
                        idx: 0,
                    }),
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
                        items: read_entries(self)?,
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
        let iter_obj = crate::vm::iterator::NativeIteratorObject::new(&mut self.heap, id, state);
        let iter_val = Value::Object(self.heap.alloc(Object::NativeIterator(iter_obj)));
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
            Value::Object(obj_ref) => *obj_ref,
            _ => {
                return Err(RuntimeError::TypeError(
                    "IteratorNext on non-iterator".to_string(),
                ));
            }
        };

        let is_js_iterator = {
            let borrowed = iter_obj.borrow();
            let Some(it) = borrowed.as_native_iterator() else {
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
                let it = borrowed.as_native_iterator()
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
            let it = borrowed.as_native_iterator_mut()
                .unwrap();
            match native_next(&mut self.heap, &mut *it.state().borrow_mut()) {
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
            let Some(it) = borrowed.as_native_iterator() else {
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
                match borrowed.as_function()
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
                            .borrow().as_native_function()
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
                !module.is_generator(*id) && !module.is_async(*id)
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
            new_obj.set_prototype(Some(*proto_obj));
        } else {
            new_obj.set_prototype(Some(self.builtins.object_prototype));
        }
        let new_obj_val = Value::Object(self.heap.alloc(Object::Ordinary(new_obj)));
        // 宿主侧入根：`new_obj_val` 跨下面 `invoke_construct` 跑构造函数体（JS）
        // 存活，而构造函数体（尤其用户定义的 @@species 构造器）会到达 GC 安全点。
        // 收集器看不见机器栈，不登记 ⇒ 新实例被回收再分配 ⇒ 返回的是一个占位对象
        // （实测：`array_species::species_target_receives_the_elements` 在 GC stress
        // 下 map/filter 结果变成空对象）。
        let _root_new = self.root(&new_obj_val);

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
            return crate::builtins::typedarray::arraybuffer_construct(&mut self.heap, args, proto);
        }
        if name == "DataView" {
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return crate::builtins::typedarray::dataview_construct(&mut self.heap, args, proto);
        }
        if let Some(kind) = crate::builtins::typedarray::kind_from_name(name) {
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return crate::builtins::typedarray::typedarray_construct(&mut self.heap, kind, args, proto);
        }
        // `new Date(...)` builds the object itself: `Date()` without `new`
        // answers a *string*, so `call_native` cannot serve both
        // (ES 21.4.2.1 vs 21.4.3.1).
        if name == "Date" {
            let proto = self.prototype_from_constructor(constructor_val, new_target);
            return crate::builtins::date_construct_value(&mut self.heap, args, proto);
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
            return crate::builtins::proxy::proxy_construct(&mut self.heap, target, handler);
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
            Some(Value::Object(p)) => this_obj.set_prototype(Some(*p)),
            _ => this_obj.set_prototype(Some(self.builtins.object_prototype)),
        }
        let this_val = Value::Object(self.heap.alloc(Object::Ordinary(this_obj)));

        let result = crate::builtins::call_native(name, args, &mut self.heap)?;
        let result = self.finish_native_construct(result, &proto, this_val);
        if reparent {
            if let (Value::Object(obj_ref), Some(Value::Object(p))) = (&result, &proto) {
                obj_ref.borrow_mut().set_prototype(Some(*p));
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
        &mut self,
        result: Value,
        proto: &Option<Value>,
        this_obj: Value,
    ) -> Value {
        let proto_ref = match proto {
            Some(Value::Object(p)) => Some(*p),
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
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                Value::Object(self.heap.alloc(Object::PrimitiveWrapper(
                    crate::vm::object::PrimitiveWrapperObject::new(result, proto_ref),
                )))
            }
            _ => this_obj,
        }
    }

    fn call_builtin_method(
        &mut self,
        obj: &Value,
        method_name: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        // First check: is this a static method on a native function (constructor)?
        if let Some(name) = crate::builtins::native_function_name(obj) {
            let static_method = format!("{}.{}", name, method_name);
            if let Ok(result) =
                crate::builtins::call_static_method(&static_method, args, &mut self.heap)
            {
                return Ok(result);
            }
        }

        // Delegate to the builtins module for prototype method dispatching
        match crate::builtins::call_prototype_method(&mut self.heap, obj, method_name, args) {
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

/// 一个**新帧的出生值**：全部由开帧点显式给出。
///
/// 为什么是具名字段而不是九个位置参数：镜面互校（`assert_frame_mirror`）在切片 1e
/// 就要退役了 —— 它以前正是靠"帧 vs 平行栈"的交叉校验兜住"搬错行"这类错误。
/// 守卫退场的同时把参数改成**具名**的，是为了让这类错误在代码上就直接看得出来。
///
/// 字段与 `Frame` 一一对应（少 `closure_depth` / `seh_depth` 之外的东西？不 —— 就是
/// `Frame` 的全部字段），`note_frame` 把它整体搬进 `Frame`。
struct FrameBirth {
    pc: usize,
    func: Value,
    this: Value,
    this_state: u8,
    argc: usize,
    construct: bool,
    new_target: Value,
    closure_depth: usize,
    seh_depth: usize,
}

/// 一个活跃的调用帧。
///
/// **迁移进度**（`deep-arch-log.md` §7.7 / §13 / §14）：`pc` 与
/// `closure_depth` / `seh_depth` 已经是**权威记录**（不再是别处的镜像）；
/// 切片 1e 之后 `argc` / `construct` / `new_target` 也进了帧 ——
/// 那三条平行栈与 `assert_frame_mirror` 随之整套退役。
///
/// **为什么有些字段不能先进镜面**：镜面阶段要求"开帧时写一次、之后只读"，
/// 而这个前提是**实测**出来的，不是设计时假设的：
/// - `this` 被 `super()` 中帧绑定（ES `BindThisValue`，`CallSuperSpread` 里那处写）；
/// - `func` 被 `SavedExecutionState` 独立恢复（§9.3 的潜伏分叉）；
/// - `pc` 每条指令都改，`this_state` 被 `CallSuperSpread` 按帧回填。
/// 最初把 `this` / `func` 放进镜面时，互校在 `class C extends Error` 与
/// `proxy::apply_and_construct_traps` 上立刻报"非帧边界的写入"。
///
/// 所以这些字段走的是**直接搬**（以字节码逐字节不变为证），而不是靠镜面证明等价。
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    /// 指令指针。**`pc` 住在帧里**（不再是 `State` 的字段）。
    ///
    /// **初始值由开帧点显式传入**，只有两种合法来源（见 `note_frame`）：
    /// 被调函数的**入口 pc**，或**哨兵**（"等调用方 `jump` 到真正的重入点"）。
    /// 绝不允许"顺着上一条指令落下来"—— 那样会在新帧里执行调用方的指令
    /// （实测踩过：`deep-arch-log.md` §12）。
    pc: usize,
    /// 本帧的**窗口基址**，也就是本帧出生那一刻的 `rbp`。
    ///
    /// 为什么它是**出生参数**而不是派生值：`rbp` 现在只有一个写入时机（开帧），
    /// 所以帧一压上来它就定了（§43.2 实测 63629 步里"当前 `rbp` == 所在帧基址"
    /// 零例外）。于是"`rbp` 是多少"这件事可以**由帧回答**，不必再靠快照往回搬
    /// —— `drive_bytecode_frame` 那处就是这么省掉的。
    ///
    /// **生成器例外**：帧被挂起再恢复时（`restore_generator_frame`）几何会挪到
    /// 新的栈底，所以**跨挂起边界的快照不能换成派生值**（实测 13 条用例，见 §44.2）。
    base: usize,
    /// 本帧**收帧时要回退到的 `rsp`** —— 也就是本帧出生那一刻的 `rsp`（调用方
    /// 压完实参之后的值）。
    ///
    /// 与 `base` 同理，是**出生参数**：`rsp` 在帧内会随压栈/出栈一路走，所以
    /// "该退回到多少"只有开帧这一刻知道。现在它由帧回答，不必再靠 `PushC`/`PopC`
    ///
    /// **注意与 `base` 的一处不同**：`base` 是"本帧自己的窗口基址"，所以退栈后取
    /// **留下来的那一帧**的 `base`；而这里是"退回到多少"，要取**被弹出的那一帧**
    /// 的 `rsp`（见 `unwind_frames_to`）。
    rsp: usize,
    /// 本帧的 `this` 绑定。
    ///
    /// **会在中帧被写**：`CallSuperSpread` 按 ES 12.3.5.1 `BindThisValue` 把派生构造器的
    /// `this` 换成父构造器产出的对象（实测就是它让 `this` 进不了镜面）。
    this: Value,
    /// 本帧的构造器状态（`THIS_NONE` / `THIS_DERIVED_UNBOUND` / `THIS_DERIVED_BOUND`）。
    ///
    /// 与"这一帧的 `this` 绑定了没有"是同一件事，所以它跟 `this` 一起住在帧里 ——
    /// 以前它与 `this_stack` 平行、还要 `mark_this_uninitialized` 去补齐，谁短谁长要人盯；
    /// 现在它是帧的**出生参数**（`THIS_DERIVED_UNBOUND` 直接给派生构造器）。
    this_state: u8,
    /// 本帧的函数对象。
    ///
    /// **取代了 `State::function_val` + 平行的 `function_stack`**：那两套记账
    /// 与帧身份是分离的，嵌套运行之后可以对不上（§9.3 实测）。现在由帧回答。
    func: Value,
    /// 实参个数（`Opcode::Arguments` / `MakeRest` 与"没传的形参读 undefined"靠它）。
    argc: usize,
    /// 是否 `[[Construct]]` 帧：`Ret` 用它决定"返回值不是对象时改用 `this`"。
    construct: bool,
    /// `new.target`：构造帧是构造器，否则 `undefined`
    /// （箭头帧带着创建时快照下来的那个值）。
    new_target: Value,
    /// 收帧时要回退到的水位（**绝对值**）—— 与 `state.closure_var_stack` /
    /// `state.seh_stack` 的长度对应。切片 1b 之后它们是这两个水位的**唯一**记录
    /// （以前还要在控制栈上再存一份三连）。
    closure_depth: usize,
    seh_depth: usize,
}

/// VM execution state
struct State {
    data_stack: Vec<Value>,
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
    promises: HashMap<u64, Gc<Object>>,
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
    /// Stack of captured-variable **cells** for active closure scopes.
    ///
    /// Every entry is a shared cell (`Rc<RefCell<..>>`): a closure captures the
    /// *cell*, never a copy of the value, so writes through either side are
    /// visible to the other. A frame's own captured locals get their cells from
    /// `MakeCell` (emitted in the function prologue); the cells it inherited from
    /// enclosing frames are pushed by `open_frame` when the frame opens.
    closure_var_stack: Vec<HashMap<String, EnvCell>>,
    /// Cells pushed by `ClosureVar` for the `Make*FuncObj` that follows.
    ///
    /// A separate stack from `closure_var_stack` on purpose: capturing must not
    /// *consume* the frame's environment (the old single-stack protocol popped
    /// the frame's own maps when a closure was created, which is how a nested
    /// function declaration lost its enclosing scope).
    pending_captured: Vec<(String, EnvCell)>,
    /// 每个活跃调用帧一条。切片 1e 之后它是**唯一的**按帧状态 ——
    /// 以前并存的那些平行栈（`ctrl_stack` / `function_stack` / `this_stack` /
    /// `this_state` / `frame_argc` / `construct_stack` / `new_target_stack`）全部退役。
    /// 它暂时只是平行栈的第二种存放；等读点翻完之后，平行栈会被删掉，
    /// 它就变成唯一来源。
    frames: Vec<Frame>,
    rsp: usize,
    rbp: usize,
}

impl State {
    fn new(heap: &mut Heap) -> Self {
        let globals = Rc::new(RefCell::new(HashMap::new()));
        let global_object = Value::Object(heap.alloc(Object::Global(
            crate::vm::object::GlobalObject::new(Rc::clone(&globals)),
        )));
        Self {
            data_stack: Vec::with_capacity(1024),
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
            closure_var_stack: Vec::new(),
            pending_captured: Vec::new(),
            frames: Vec::new(),
            rsp: 0,
            rbp: 0,
        }
    }

    /// 压**顶层脚本帧**：与 5 个开帧点**完全同构**（`enter_frame` + 控制栈三项 +
    /// `note_frame`），`pc` 从 0 起。
    ///
    /// 单独抽一个方法而不是让 `run` 内联：单元测试也需要"一个已经有帧的 `State`"，
    /// 而它必须与运行期压的是**同一种**帧 —— 否则测试验的就是别的东西了。
    /// 少了控制栈那三项，`note_frame` 的 `ctrl_base` 会下溢（实测踩到过）；
    /// （切片 1b 之前这里还要在控制栈上凑齐"每帧三项"，那套簿记已退役。）
    /// 现在脚本帧就是普通的一员。
    fn open_script_frame(&mut self) -> Result<(), RuntimeError> {
        self.enter_frame()?;
        let closure_depth = self.closure_var_stack.len();
        let seh_depth = self.seh_stack.len();
        self.note_frame(FrameBirth {
            pc: 0,
            func: Value::Undefined,
            this: Value::Undefined,
            this_state: THIS_NONE,
            argc: 0,
            construct: false,
            new_target: Value::Undefined,
            closure_depth,
            seh_depth,
        });
        self.jump(0);
        Ok(())
    }

    /// 当前帧的指令指针。
    ///
    /// `pc` 住在 `Frame` 里。这条搬动必须与三件事**一起**做（实测过两次，
    /// 见 `deep-arch-log.md` §10 / §11 / §12）：
    /// 1. 嵌套循环改成**边界驱动**（`frames.len() > boundary`）；
    /// 2. 调用点在开帧前自己 `jump_offset(1)`（取代 `Ret` 的 `jump(return_pc)`）；
    /// 3. 新帧的初始 pc **显式传入**（入口 pc 或哨兵），不许默认。
    ///
    /// 没有帧时返回 0：`run` 在跑任何指令之前总会压一个顶层脚本帧，
    /// 所以运行期不该出现空 `frames`；返回 0 只是给"裸 `State`"的单元测试兜底。
    fn pc(&self) -> usize {
        self.frames.last().map_or(0, |frame| frame.pc)
    }

    /// 设置当前帧的 `pc`。没有帧时**只断言**（debug）、不静默丢弃 ——
    /// 静默丢弃会把"控制流走丢了"变成"什么都没发生"。
    fn set_pc(&mut self, value: usize) {
        match self.frames.last_mut() {
            Some(frame) => frame.pc = value,
            None => debug_assert!(false, "set_pc 时没有帧：pc 住在 Frame 里，先压帧"),
        }
    }

    fn jump(&mut self, target: usize) {
        self.set_pc(target);
    }

    fn jump_offset(&mut self, offset: isize) {
        self.set_pc((self.pc() as isize + offset) as usize);
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
        // Only the callee reads those slots, so the current frame's `argc` is the
        // one to consult here. Call sites read their outgoing arguments through
        // `raw_stack_value`, which bypasses this check.
        if offset < 0 {
            let argc = self.frames.last().map(|f| f.argc).unwrap_or(0);
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
            let argc = self.frames.last().map(|f| f.argc).unwrap_or(0);
            if (-offset) as usize > argc {
                return Ok(());
            }
        }
        self.ensure(index as usize)?;
        self.data_stack[index as usize] = value;
        Ok(())
    }

    /// 开始一帧：只做递归深度的护栏（`this` / `argc` / `new.target` 都是出生参数）。
    fn enter_frame(&mut self) -> Result<(), RuntimeError> {
        // 深度按**调用帧**数算：`frames[0]` 是顶层脚本帧，不算一次调用。
        // 以前读控制栈长度 / 3（每帧三项）—— 那还混着 `PushC`/`PopC` 的条目，
        // 只有"它们最多两项、整数除法恰好不出错"才成立，是个侥幸。
        if self.frames.len().saturating_sub(1) >= MAX_CALL_DEPTH {
            return Err(RuntimeError::RangeError(
                "Maximum call stack size exceeded".to_string(),
            ));
        }
        Ok(())
    }

    /// 把一个新帧记进 `frames`。**它的全部字段都由调用点作为"出生值"给出**
    /// （见 `FrameBirth`）—— 所以调用点不需要先摆好任何平行栈：
    /// 切片 1e 之后那些平行栈（`frame_argc` / `construct_stack` / `new_target_stack`）
    /// 已经不存在了，`frames` 是唯一的按帧状态。
    ///
    /// 唯一的例外是 `base`：它由 `note_frame` 自己从 `self.rbp` 取，而**不是**由
    /// 调用点作为 `FrameBirth` 的字段传进来 —— 因为"帧的窗口基址 = 开帧那一刻的
    /// `rbp`"这条等式正是要成立的东西，让调用点**另外**再写一遍就等于给它一个
    /// 可以互相矛盾的机会（§25 的教训：两份知识必然分叉）。开帧是 `rbp` 唯一的写入
    /// 时机，所以在这里读它就是权威来源。
    fn note_frame(&mut self, birth: FrameBirth) {
        let base = self.rbp;
        let rsp = self.rsp;
        self.frames.push(Frame {
            pc: birth.pc,
            base,
            rsp,
            func: birth.func,
            this: birth.this,
            this_state: birth.this_state,
            argc: birth.argc,
            construct: birth.construct,
            new_target: birth.new_target,
            closure_depth: birth.closure_depth,
            seh_depth: birth.seh_depth,
        });
    }

    /// 当前帧的窗口基址 —— `rbp` 的**派生值**。
    ///
    /// 没有帧时返回 0：与 `pc()` 同一个兜底口径（运行期总有脚本帧，只有裸 `State`
    /// 的单元测试才会撞上）。
    fn frame_base(&self) -> usize {
        self.frames.last().map_or(0, |frame| frame.base)
    }

    /// **M2-1 的互校**：镜面必须与各平行栈此刻的顶部一致。
    ///
    /// 只在 debug 下跑（`step` 每次调用）。它抓的是"开/收帧的两套记账分叉" ——
    /// 而那正是本阶段唯一新引入的风险。
    ///
    /// **一律用 `debug_assert!(a == b, "…")` 而不是 `debug_assert_eq!(a, b)`**：
    /// 后者在失败时会格式化两个 `Value`，而 `Value::Debug` 会递归进对象图
    /// （实测：在 `class C extends Error` 的用例上，真正失配的那次断言被
    /// **栈溢出**掩盖了 —— 失败信息本身把栈打爆，看不到原始原因）。


    /// Resolve `name` in the environment.
    ///
    /// Order: the **closure cells** of the active frames first (innermost
    /// wins — a function-local binding shadows a script-level one of the same
    /// name), then the script's declarative record (the truth for script-scope
    /// `let`/`const`/`class`, and the carrier of their temporal dead zone),
    /// then the global object. Splitting this out matters because `typeof` has
    /// to consult exactly the same chain — a `typeof` that skipped the record
    /// answered `"undefined"` for a name the ordinary read resolves to a
    /// function.
    fn resolve_env_name(&self, name: &str) -> Result<Option<Value>, RuntimeError> {
        // `globalThis` is answered from the realm's global object rather than
        // from the globals map: publishing it *in* the map would make
        // map → object → map, a cycle that (with no GC) pinned every run's
        // whole `State` — measured as a shard that outgrew its `ulimit`.
        if name == "globalThis" {
            return Ok(Some(self.global_object.clone()));
        }
        if let Some(cell) = self.env_cell(name) {
            // A cell is only ever created for a *declared* binding, so it is
            // always initialised by the time anything can read it through the
            // environment. `None` would be the dead zone of a `let` that a
            // closure captured before its declaration ran.
            let value = cell.borrow().clone().ok_or_else(|| {
                RuntimeError::ReferenceError(format!(
                    "Cannot access '{name}' before initialization"
                ))
            })?;
            return Ok(Some(value));
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
        Ok(self.get_global(name))
    }

    /// The cell `name` resolves to in the **current frame's** environment: its
    /// own captured locals plus everything it captured from enclosing frames.
    ///
    /// The scan stops at the frame's base (`frames.last().closure_depth`): the
    /// entries below it belong to enclosing frames and are visible here only
    /// through the copies `open_frame` pushed into this frame's range.
    pub(crate) fn env_cell(&self, name: &str) -> Option<EnvCell> {
        let base = self.frames.last().map_or(0, |f| f.closure_depth);
        self.closure_var_stack
            .get(base..)?
            .iter()
            .rev()
            .find_map(|map| map.get(name).cloned())
    }

    /// Bind `cell` under `name` in the current frame's environment.
    pub(crate) fn push_env_cell(&mut self, name: String, cell: EnvCell) {
        let mut map = HashMap::new();
        map.insert(name, cell);
        self.closure_var_stack.push(map);
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
fn set_data_new(heap: &mut Heap, prototype: Option<Gc<Object>>) -> Value {
    let set_obj = crate::vm::object::SetObject::new(prototype);
    Value::Object(heap.alloc(Object::Set(set_obj)))
}

/// Run `f` with the `SetObject` behind `value` (no-op when it is not a Set).
fn with_set_data<R>(value: &Value, f: impl FnOnce(&mut crate::vm::object::SetObject) -> R) -> Option<R> {
    let Value::Object(obj_ref) = value else {
        return None;
    };
    let mut borrowed = obj_ref.borrow_mut();
    borrowed.as_set_mut()
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
    ///
    /// **为什么必须单独记**：以前帧的簿记与 `PushC`/`PopC` 的簿记挤在同一条
    /// `ctrl_stack` 上，所以 `unwind_frames_to` 里一次 `truncate(saved_ctrl_depth)`
    /// 顺手把**被回退帧遗留的 `PushC` 项**也清掉了。切片 1b 把两者分开之后，
    /// 这个清理动作就没有人做了 —— 实测后果：外层之后的 `PopC` 会弹到过期值，
    /// `rbp` 被破坏（`[a, f(), b]` 这种"数组字面量里夹一次会抛异常的调用"
    /// 直接报 `ArrayPush on non-object`）。
    ///
    /// ⇒ **一条记录要记两个水位，因为它们现在是两条独立的栈。**
    /// **帧深度**（`frames.len()`）在进入这条 `try` 时的值。
    ///
    /// 嵌套调用里抛出的异常必须先把那些帧收掉再跑处理器：否则处理器的帧身份
    /// 还是抛出者那一帧（`pc` 就在抛出点上），会接着跑错的地方。
    /// 切片 1b 之前这里记的是控制栈深度 —— 那是同一个事实的另一种编码。
    saved_frame_depth: usize,
}

/// Per-frame constructor states, stored in [`Frame::this_state`].
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
    rsp: usize,
    rbp: usize,
    closure: usize,
    seh: usize,
    frame_depth: usize,
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
        promise: Gc<Object>,
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
    result: Gc<Object>,
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
    /// 清空 wrapper 原型的线程局部缓存。
    ///
    /// 该缓存按名字存放 `Gc<Object>` 句柄，而句柄**没有引用计数、也没有 Drop** ——
    /// 不清的话，下一个 VM 会查到本 VM 已经释放的槽并读它（悬垂，release 下是静默 UB）。
    /// `Rc` 时代"忘了清 = 泄漏"，现在升级为"忘了清 = 悬垂"，所以这一步是**必须**的。
    ///
    /// 注意 drop 体**先于**字段析构运行，故它必然早于 `heap` 的释放。
    ///
    /// 至于对象之间的环（`prototype` ⇄ `constructor` 等）：不再需要手工断 ——
    /// 它们都活在同一个 `Heap` 里，随 `heap` 字段析构整片释放。原先那条写死的
    /// 23 项清单（`Builtins::teardown`）已删除。
    fn drop(&mut self) {
        crate::builtins::clear_wrapper_prototypes();
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
                            obj_ref,
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
        let mut heap = Heap::new();
        let state = State::new(&mut heap);
        assert_eq!(state.rsp, 0);
        assert_eq!(state.rbp, 0);
        assert!(state.seh_stack.is_empty());
        assert!(state.globals.borrow().is_empty());
    }

    #[test]
    fn test_state_push_pop() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        state.push(Value::Number(42.0)).unwrap();
        assert_eq!(state.rsp, 1);
        let val = state.pop().unwrap();
        assert_eq!(val, Value::Number(42.0));
        assert_eq!(state.rsp, 0);
    }

    #[test]
    fn test_state_push_pop_multiple() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
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
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        let result = state.pop();
        assert!(matches!(result, Err(RuntimeError::RangeError(_))));
    }

    /// 单元测试用：压一个"顶层脚本帧"。
    ///
    /// `pc` 现在住在 `Frame` 里，所以任何要动 `pc` 的测试都必须先有帧 ——
    /// 这正是本步的意图：**没有帧就没有 pc**，不再有一条"帧外"的 pc 通道。
    fn state_with_script_frame() -> State {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        // 用**运行期同一个**压帧入口，测试才不会验到另一种帧。
        state.open_script_frame().expect("open_script_frame");
        state
    }

    #[test]
    fn test_state_jump() {
        let mut state = state_with_script_frame();
        state.jump(100);
        assert_eq!(state.pc(), 100);
    }

    #[test]
    fn test_state_jump_offset() {
        let mut state = state_with_script_frame();
        state.jump(10);
        state.jump_offset(5);
        assert_eq!(state.pc(), 15);
        state.jump_offset(-3);
        assert_eq!(state.pc(), 12);
    }

    /// **`pc` 是每帧一个**：给上层帧跳转不会动下层帧的 `pc`。
    ///
    /// 这条钉住的正是"`pc` 进帧"换来的东西 —— 调用方不再需要一个"返回地址寄存器"：
    /// 它自己的下一条本来就在自己那一帧里（`Call` 等调用点在开帧前自己
    /// `jump_offset(1)`，取代了 `Ret` 的 `jump(return_pc)`）。
    #[test]
    fn pc_is_per_frame() {
        let mut state = state_with_script_frame();
        state.jump(7);
        // 压一个"被调帧"：它有自己的 pc，**由开帧点显式给**。
        state.enter_frame().expect("enter_frame");
        state.note_frame(FrameBirth {
            pc: 42,
            func: Value::Undefined,
            this: Value::Undefined,
            this_state: THIS_NONE,
            argc: 1,
            construct: false,
            new_target: Value::Undefined,
            closure_depth: 0,
            seh_depth: 0,
        });
        assert_eq!(state.pc(), 42, "被调帧的出生 pc 就是入口");
        assert_eq!(state.frames[0].pc, 7, "调用方那帧的 pc 必须没被动过");
        state.frames.pop();
        assert_eq!(state.pc(), 7, "收帧之后回到调用方的 pc");
    }

    /// 顶层脚本帧是 `frames[0]`：它由 `run` 在任何指令之前压好，
    /// 于是"最外层"是 `frames.len() == 1`（不再是"控制栈空不空"）。
    ///
    /// 这一条钉住的就是 1a 的全部内容：**脚本也有帧**。
    #[test]
    fn the_script_runs_in_a_frame() {
        let state = state_with_script_frame();
        assert_eq!(state.frames.len(), 1, "脚本帧");
        assert_eq!(
            state.frames.len(),
            1,
            "它自己就把 argc 带着（不再有平行的 frame_argc 栈）"
        );
        // 脚本帧不是"一次调用"：深度按 `frames.len() - 1` 算。
        assert_eq!(state.frames.len().saturating_sub(1), 0);
    }

    #[test]
    fn test_state_register() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        state
            .set_register(Register::R0, Value::Number(42.0))
            .unwrap();
        let val = state.get_register(Register::R0).unwrap();
        assert_eq!(val, Value::Number(42.0));
    }

    #[test]
    fn test_state_register_default_undefined() {
        let mut heap = Heap::new();
        let state = State::new(&mut heap);
        let val = state.get_register(Register::R0).unwrap();
        assert_eq!(val, Value::Undefined);
    }

    #[test]
    fn test_state_register_rsp_rbp_not_readable() {
        let mut heap = Heap::new();
        let state = State::new(&mut heap);
        let result = state.get_register(Register::Rsp);
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn test_state_register_rsp_rbp_not_writable() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        let result = state.set_register(Register::Rsp, Value::Number(0.0));
        assert!(matches!(result, Err(RuntimeError::TypeError(_))));
    }

    #[test]
    fn test_state_get_value_from_stack() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        state.rbp = 0;
        // The value stack grows lazily, so an untouched slot reads as undefined.
        assert_eq!(state.get_value_from_stack(0).unwrap(), Value::Undefined);
        state.set_value_to_stack(0, Value::Number(42.0)).unwrap();
        let val = state.get_value_from_stack(0).unwrap();
        assert_eq!(val, Value::Number(42.0));
    }

    #[test]
    fn test_state_set_value_to_stack() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
        state.rbp = 0;
        state.set_value_to_stack(0, Value::Number(42.0)).unwrap();
        assert_eq!(state.data_stack[0], Value::Number(42.0));
    }

    #[test]
    fn test_state_stack_out_of_bounds() {
        let mut heap = Heap::new();
        let state = State::new(&mut heap);
        let result = state.get_value_from_stack(-1);
        assert!(matches!(result, Err(RuntimeError::RangeError(_))));
    }

    #[test]
    fn test_state_global() {
        let mut heap = Heap::new();
        let mut state = State::new(&mut heap);
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
        let mut heap = Heap::new();
        let arr_val = {
            let arr = new_array_object(&mut heap);
            if let Value::Object(ref arr_ref) = arr {
                let mut obj = arr_ref.borrow_mut();
                if let Some(a) = obj.as_array_mut() {
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
        let mut heap = Heap::new();
        let obj_val = {
            let obj = new_ordinary_object(&mut heap);
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
            // 手搭的模块没有"按函数的属性"；调用方要走 `entry_pc` / `is_generator`
            // 之类的地方各自兜底（与空 Hash…… 的旧行为一致）。
            Vec::new(),
            instructions,
            HashMap::new(),
            Vec::new(),
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
