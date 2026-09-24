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

use crate::builtins::Builtins;
use crate::vm::iterator::iterator_symbol_key;
use crate::bytecode::{
    Bytecode, Constant, FunctionId, Module, Opcode, Operand, Primitive, Register,
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

/// Prefix of the synthetic name given to bound-function wrappers.
const BOUND_PREFIX: &str = "__bound__";

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
    /// Bound functions created by `Function.prototype.bind`, keyed by a
    /// synthetic id that is embedded in the wrapper's name.
    bound_functions: HashMap<u32, BoundFunction>,
    next_bound_id: u32,
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
        match val {
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

impl VM {
    pub fn new() -> Self {
        let mut state = State::new();
        let builtins = Builtins::new();
        builtins.register(&mut state.globals);
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
            current_module_info: None,
            func_objs: HashMap::new(),
            step_limit: Some(DEFAULT_STEP_LIMIT),
            steps: 0,
            bound_functions: HashMap::new(),
            next_bound_id: 0,
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

    /// Execute a bytecode module and return the result
    pub fn run(&mut self, module: &Module) -> Result<Value, RuntimeError> {
        self.state = State::new();
        self.func_objs.clear();
        self.current_module_info = Some(module.func_info.clone());
        self.bound_functions.clear();
        self.next_bound_id = 0;
        self.invoke_boundaries.clear();
        self.steps = 0;
        // Re-register builtins into the fresh state
        self.builtins.register(&mut self.state.globals);

        while self.step(module)? {}

        // If we run out of instructions, return Rv
        Ok(self
            .state
            .get_register(Register::Rv)
            .unwrap_or(Value::Undefined))
    }

    /// Box a bare `Value::Function(id)` into a stable `FunctionObject`.
    ///
    /// The mapping is memoized for the lifetime of a `run` so that `F.prototype`
    /// and any properties hung off the function object remain observable.
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
        let inst = match module.instructions.get(self.state.pc) {
            Some(i) => i.clone(),
            None => return Ok(false),
        };
        self.steps += 1;
        if let Some(limit) = self.step_limit {
            if self.steps > limit {
                return Err(RuntimeError::RangeError(
                    "execution step limit exceeded (possible infinite loop)".to_string(),
                ));
            }
        }
        let Bytecode {
            opcode,
            operands: _,
        } = inst;

        match opcode {
            Opcode::Halt => {
                return Ok(false);
            }
            Opcode::Ret => {
                // Check if we need to execute any finally blocks before returning
                let mut finally_to_execute = None;
                for (idx, record) in self.state.seh_stack.iter().enumerate() {
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
                    if self.state.this_uninitialized.last() == Some(&true) {
                        let err = self.this_not_initialized();
                        if let Some(exc) = self.as_js_exception(&err) {
                            self.handle_throw(exc)?;
                            return Ok(true);
                        }
                        return Err(err);
                    }
                    let return_pc = self.state.popc()?;
                    let saved_seh_depth = self.state.popc()?;
                    let saved_closure_depth = self.state.popc()?;
                    // The frame's own `this`, needed for [[Construct]] below.
                    // It has to be captured *before* the caller's binding is
                    // restored, otherwise a constructor would return the
                    // caller's `this` instead of the object it just built.
                    let frame_this = self.state.this_val.clone();
                    // Restore the caller's `this` binding and frame arity.
                    if let Some(saved_this) = self.state.this_stack.pop() {
                        self.state.this_val = saved_this;
                    }
                    self.state.this_uninitialized.pop();
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
        // Native built-ins never need a bytecode frame.
        if let Some(name) = crate::builtins::native_function_name(callee) {
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
        self.invoke_boundaries.push(self.state.ctrl_stack.len());

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

        self.state.enter_frame(args.len())?;
        self.state.this_val = effective_this;
        self.state.function_val = match callee {
            Value::Function(id) => self.materialize_function(*id),
            other => other.clone(),
        };
        for (name, value) in &captured_vars {
            let mut map = std::collections::HashMap::new();
            map.insert(name.clone(), value.clone());
            self.state.closure_var_stack.push(map);
        }

        match module.symtab.get(&FunctionId::new(func_id)) {
            Some(location) => {
                self.state.pushc(self.state.closure_var_stack.len())?;
                self.state.pushc(self.state.seh_stack.len())?;
                self.state.pushc(return_pc)?;
                self.state.construct_stack.push(false);
                self.state.new_target_stack.push(
                    new_target_override
                        .or(captured_this_new_target)
                        .unwrap_or(Value::Undefined),
                );
                self.state.jump(*location);
            }
            None => {
                return Err(RuntimeError::ReferenceError(format!(
                    "undefined function: {func_id}"
                )));
            }
        }

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
        self.state.pc = saved_pc;
        self.state.rsp = saved_rsp;
        self.state.rbp = saved_rbp;
        self.state.closure_var_stack.truncate(saved_closure);
        self.state.seh_stack.truncate(saved_seh);
        self.state.this_stack.truncate(saved_this_depth);
        self.state.this_uninitialized.truncate(saved_this_depth);
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
                    self.delete_member(&value, &key)?;
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

        if name == crate::builtins::OBJECT_TO_STRING_NATIVE {
            return self.object_prototype_to_string(&this, args, module);
        }
        // `Symbol.prototype.description` getter: unwrap the receiver.
        if name == crate::builtins::SYMBOL_DESCRIPTION_NATIVE {
            return crate::builtins::symbol_description(&this);
        }
        // Generator methods: `gen.next(v)` resumes the body, and
        // `gen[Symbol.iterator]()` hands the generator itself back.
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_NEXT_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let send = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_next(id, send, module);
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
                self.iterator_close(iter_val, module)?;
            }
            return Ok(Value::Undefined);
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
            // `entries` / `keys` / `values` return a *fresh* iterator over a
            // snapshot; only the VM can mint iterator objects (they live in the
            // iterator registry so `next()` can find its state).
            if matches!(method, "entries" | "keys" | "values") {
                if let Some(items) = self.array_like_elements(&this) {
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
            // `Object.assign` reads through real `[[Get]]` and writes through
            // real `[[Set]]` (own or prototype accessors may run), which only
            // the VM can do.
            if name == "Object.assign" {
                return self.object_assign(args, module);
            }
            // The descriptor arguments are read through [[Get]] first, which
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
            return crate::builtins::call_static_method(name, args);
        }
        crate::builtins::call_native(name, args)
    }

    fn run_instruction(&mut self, inst: &Bytecode, module: &Module) -> Result<(), RuntimeError> {
        let Bytecode { opcode, operands } = inst;

        match opcode {
            // `yield expr`: suspend the enclosing generator. The frame stays on
            // the value stack; `generator_next` lifts it out before rewinding
            // `rsp`. Jumping past the last instruction ends the nested `step`
            // loop the resumer is driving, exactly as a `Ret` to the sentinel
            // return address would.
            Opcode::Yield => {
                // `-1` is the "no operand" marker emitted for a bare `yield`
                // (codegen cannot leave an operand slot empty).
                let value = match operands.get(1) {
                    Some(Operand::Immd(-1)) => Value::Undefined,
                    Some(src) => self.get_value(*src)?,
                    None => Value::Undefined,
                };
                // `yield expr` evaluates to the value the resumer supplied.
                self.generator_yielded = Some(value);
                self.generator_yield_pc = self.state.pc;
                self.state.jump(module.instructions.len());
            }
            // ===== Control Flow =====
            Opcode::Call => {
                let func_id = operands[0].as_immd();
                // Second operand is the argument count (see `CodeGen::gen_call`).
                let arg_count = operands.get(1).map(|o| o.as_immd() as usize).unwrap_or(0);
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
            Opcode::CallEx => {
                let arg_count = operands[1].as_immd() as usize;
                // Operands are read with the *caller's* frame pointer still in
                // place: a stack-slot operand would otherwise resolve inside the
                // callee's frame. The frame switch happens once they are read.
                let callee = self.get_value(operands[0])?;
                self.state.rbp = self.state.rsp;

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
                        if module.generators.contains(&id) {
                            let args = self.collect_call_args(arg_count)?;
                            let gobj = self.create_generator(
                                id,
                                Value::Undefined,
                                args,
                                Vec::new(),
                            );
                            self.state.set_register(Register::Rv, gobj)?;
                            self.state.jump_offset(1);
                            return Ok(());
                        }
                        self.state.enter_frame(arg_count)?;
                        // Strict mode: this = undefined for regular calls
                        self.state.this_val = Value::Undefined;
                        self.state.function_val = self.materialize_function(id);
                        match module.symtab.get(&FunctionId::new(id)) {
                            Some(location) => {
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
                                    "undefined function: {id}"
                                )));
                            }
                        }
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

                            self.state.enter_frame(arg_count)?;
                            self.state.this_val = captured_this.unwrap_or(Value::Undefined);
                            self.state.function_val = Value::Object(Rc::clone(&obj_ref));
                            for (name, value) in &captured_vars {
                                let mut map = std::collections::HashMap::new();
                                map.insert(name.clone(), value.clone());
                                self.state.closure_var_stack.push(map);
                            }
                            match module.symtab.get(&FunctionId::new(id)) {
                                Some(location) => {
                                    self.state.pushc(self.state.closure_var_stack.len())?;
                                    self.state.pushc(self.state.seh_stack.len())?;
                                    self.state.pushc(self.state.pc + 1)?;
                                    self.state.construct_stack.push(false);
                                    self.state
                                        .new_target_stack
                                        .push(captured_new_target.unwrap_or(Value::Undefined));
                                    self.state.jump(*location);
                                    return Ok(());
                                }
                                None => {
                                    return Err(RuntimeError::ReferenceError(format!(
                                        "undefined function: {id}"
                                    )));
                                }
                            }
                        }
                        return Err(RuntimeError::TypeError("not a function".to_string()));
                    }
                    _ => return Err(RuntimeError::TypeError("not a function".to_string())),
                }
            }
            Opcode::Jump => {
                let offset = operands[0].as_immd();
                self.state.jump_offset(offset);
                return Ok(());
            }
            Opcode::DelayedJump => {
                let offset = operands[0].as_immd();
                let seh_depth = operands[1].as_immd() as usize;

                // Check if we need to execute any finally blocks
                let mut finally_to_execute = None;
                let stack_len = self.state.seh_stack.len();
                let start_idx = stack_len.saturating_sub(seh_depth);

                for idx in start_idx..stack_len {
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

                // All finally blocks executed, jump to target
                self.state.jump_offset(offset);
                return Ok(());
            }
            Opcode::BrIf => {
                let cond = self.get_value(operands[0])?;
                let b = cond.to_boolean();
                let offset = if b {
                    operands[1].as_immd()
                } else {
                    operands[2].as_immd()
                };
                self.state.jump_offset(offset);
                return Ok(());
            }
            // Opcode::Ret is handled directly in run()

            // ===== Stack / Register Manipulation =====
            Opcode::Mov => {
                let value = self.get_value(operands[1])?;
                self.set_value(operands[0], value)?;
            }
            Opcode::Push => {
                let value = self.get_value(operands[0])?;
                self.state.push(value)?;
            }
            Opcode::Pop => {
                let value = self.state.pop()?;
                self.set_value(operands[0], value)?;
            }
            Opcode::MovC => match (operands[0], operands[1]) {
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
            Opcode::PushC => match operands[0] {
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
            Opcode::PopC => match operands[0] {
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
            Opcode::AddC => match (operands[0], operands[1]) {
                (Operand::Register(Register::Rsp), Operand::Register(Register::Rsp)) => {
                    self.state.rsp += operands[2].as_immd() as usize;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported AddC operands: {inst}"
                    )));
                }
            },
            Opcode::SubC => match (operands[0], operands[1]) {
                (Operand::Register(Register::Rsp), Operand::Register(Register::Rsp)) => {
                    self.state.rsp -= operands[2].as_immd() as usize;
                }
                _ => {
                    return Err(RuntimeError::TypeError(format!(
                        "unsupported SubC operands: {inst}"
                    )));
                }
            },

            // ===== Load Instructions =====
            Opcode::LoadConst => {
                let const_index = operands[1].as_immd();
                let value = Self::from_constant(&module.constants[const_index as usize]);
                self.set_value(operands[0], value)?;
            }
            Opcode::LoadEnv => {
                let name_index = operands[1].as_immd();
                let name = &module.constants[name_index as usize];
                match name {
                    Constant::String(name) => {
                        // Search closure_var_stack first (newest entries first)
                        let mut found = None;
                        for map in self.state.closure_var_stack.iter().rev() {
                            if let Some(value) = map.get(name.as_str()) {
                                found = Some(value.clone());
                                break;
                            }
                        }
                        match found {
                            Some(value) => {
                                self.set_value(operands[0], value)?;
                            }
                            None => {
                                // Fall back to global environment
                                match self.state.get_global(name) {
                                    Some(value) => {
                                        self.set_value(operands[0], value)?;
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
            Opcode::Addx => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
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
                self.set_value(operands[0], value)?;
            }
            Opcode::Subx => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let value = lhs - rhs;
                self.set_value(operands[0], value)?;
            }
            Opcode::Mulx => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let value = lhs * rhs;
                self.set_value(operands[0], value)?;
            }
            Opcode::Divx => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let value = lhs / rhs;
                self.set_value(operands[0], value)?;
            }
            Opcode::Remx => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let value = lhs % rhs;
                self.set_value(operands[0], value)?;
            }
            Opcode::Pow => {
                // ES `ExponentiationExpression`: ToNumeric on both operands,
                // then `**`. Like the other arithmetic opcodes this does not
                // model the BigInt case (the engine has no BigInt).
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let lprim = self.to_primitive(&lhs, "number", module)?;
                let rprim = self.to_primitive(&rhs, "number", module)?;
                let value = Value::Number(lprim.to_number().powf(rprim.to_number()));
                self.set_value(operands[0], value)?;
            }

            // ===== Unary =====
            Opcode::Not => {
                let value = self.get_value(operands[1])?;
                let result = Value::Bool(!value.to_boolean());
                self.set_value(operands[0], result)?;
            }
            Opcode::BitNot => {
                // Bitwise NOT: convert to 32-bit signed integer, flip bits
                let value = self.get_value(operands[1])?;
                let num = to_int32(value.to_number());
                let result = Value::Number((!num) as f64);
                self.set_value(operands[0], result)?;
            }
            Opcode::BitAnd | Opcode::BitOr | Opcode::BitXor => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let a = to_int32(lhs.to_number());
                let b = to_int32(rhs.to_number());
                let result = match opcode {
                    Opcode::BitAnd => a & b,
                    Opcode::BitOr => a | b,
                    _ => a ^ b,
                };
                self.set_value(operands[0], Value::Number(result as f64))?;
            }
            Opcode::Shl | Opcode::Shr | Opcode::UShr => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let shift_count = (to_uint32(rhs.to_number()) & 0x1F) as u32;
                let result = match opcode {
                    Opcode::Shl => to_int32(lhs.to_number()).wrapping_shl(shift_count),
                    Opcode::Shr => to_int32(lhs.to_number()).wrapping_shr(shift_count),
                    _ => (to_uint32(lhs.to_number()).wrapping_shr(shift_count)) as i32,
                };
                self.set_value(operands[0], Value::Number(result as f64))?;
            }
            Opcode::Neg => {
                let value = self.get_value(operands[1])?;
                let result = -value;
                self.set_value(operands[0], result)?;
            }

            // ===== Logical =====
            Opcode::And => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // JS logical AND: returns first falsy or last value
                let value = if !lhs.to_boolean() { lhs } else { rhs };
                self.set_value(operands[0], value)?;
            }
            Opcode::Or => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // JS logical OR: returns first truthy or last value
                let value = if lhs.to_boolean() { lhs } else { rhs };
                self.set_value(operands[0], value)?;
            }

            // ===== Comparison =====
            // js_abstract_relational(x, y) returns x < y
            Opcode::Greater => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // lhs > rhs  ⟺  rhs < lhs
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let result = Self::js_abstract_relational(&rhs, &lhs)?;
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::GreaterEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // lhs >= rhs  ⟺  !(lhs < rhs)
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let less = Self::js_abstract_relational(&lhs, &rhs)?;
                self.set_value(operands[0], Value::Bool(!less))?;
            }
            Opcode::Less => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let result = Self::js_abstract_relational(&lhs, &rhs)?;
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::LessEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // lhs <= rhs  ⟺  !(rhs < lhs)
                let (lhs, rhs) = self.coerce_for_relational(lhs, rhs, module)?;
                let less = Self::js_abstract_relational(&rhs, &lhs)?;
                self.set_value(operands[0], Value::Bool(!less))?;
            }
            Opcode::Equal => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = self.abstract_eq(&lhs, &rhs, module)?;
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::NotEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = !self.abstract_eq(&lhs, &rhs, module)?;
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::StrictEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = lhs.strict_eq(&rhs);
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::StrictNotEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = !lhs.strict_eq(&rhs);
                self.set_value(operands[0], Value::Bool(result))?;
            }

            // ===== Type =====
            Opcode::TypeOf => {
                let value = self.get_value(operands[1])?;
                let type_str = value.type_of().to_string();
                self.set_value(operands[0], Value::String(Rc::new(type_str)))?;
            }
            Opcode::TypeOfEnv => {
                // `typeof unresolvableName` is "undefined" — never a
                // ReferenceError. The name is a constant-pool index.
                let name_index = operands[1].as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self
                    .state
                    .closure_var_stack
                    .iter()
                    .rev()
                    .find_map(|map| map.get(&name).cloned())
                    .or_else(|| self.state.get_global(&name));
                let type_str = match value {
                    Some(v) => v.type_of().to_string(),
                    None => "undefined".to_string(),
                };
                self.set_value(operands[0], Value::String(Rc::new(type_str)))?;
            }
            Opcode::InstanceOf => {
                let obj = self.get_value(operands[1])?;
                let ctor = self.get_value(operands[2])?;
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

                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::In => {
                let key = self.resolve_property_key(operands[1], module)?;
                let obj = self.get_value(operands[2])?;
                let obj = self.as_object_value(&obj);
                let result = match obj {
                    Value::Object(obj_ref) => crate::vm::prototype::internal_has_property(
                        obj_ref, &key,
                    )
                    .map_err(RuntimeError::TypeError)?,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "Cannot use 'in' operator on non-object".to_string(),
                        ));
                    }
                };
                self.set_value(operands[0], Value::Bool(result))?;
            }

            // ===== Iteration =====
            Opcode::MakeIter => {
                let src = self.get_value(operands[1])?;
                let iter_val = self.make_iterator(src, module)?;
                self.set_value(operands[0], iter_val)?;
            }
            Opcode::IterNext => {
                // Operand order comes from codegen: (item, has_next, src).
                let iter_val = self.get_value(operands[2])?;
                let (item, done) = self.iterator_next(iter_val, None, module)?;
                self.set_value(operands[0], item)?;
                self.set_value(operands[1], Value::Bool(!done))?;
            }
            Opcode::IterClose => {
                let iter_val = self.get_value(operands[0])?;
                self.iterator_close(iter_val, module)?;
            }
            Opcode::MakeRest => {
                // Collect the tail of the incoming arguments into a fresh
                // array: args[from..] where arg i lives at [rbp - (i + 1)].
                let from = operands[1].as_immd().max(0) as usize;
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
                self.set_value(operands[0], Value::Object(Rc::new(RefCell::new(arr))))?;
            }
            Opcode::CallSpread => {
                // operands: (callee, this, args-array); result goes to Rv.
                let callee = self.get_value(operands[0])?;
                let this = self.get_value(operands[1])?;
                let args_val = self.get_value(operands[2])?;
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
            Opcode::CallSuperSpread => {
                // `super(...)`: same shape as CallSpread, but the parent
                // constructor inherits this frame's `new.target`.
                let callee = self.get_value(operands[0])?;
                // The receiver is normally the *frame's* `this`, not an operand
                // (lowering a `load_this` would raise in a derived constructor,
                // whose `this` is still uninitialized here). An arrow body that
                // calls `super()` still carries it as an operand, though.
                let operand_this = self.get_value(operands[1])?;
                let this = if operand_this.is_undefined() {
                    self.state.this_val.clone()
                } else {
                    operand_this
                };
                let args_val = self.get_value(operands[2])?;
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
                    .this_uninitialized
                    .iter_mut()
                    .rev()
                    .find(|flag| **flag)
                {
                    *slot = false;
                }
                let result =
                    self.invoke_with_new_target(&callee, this, &args, Some(new_target), module)?;
                self.state.set_register(Register::Rv, result)?;
            }
            Opcode::NewSpread => {
                // operands: (dst, constructor, args-array)
                let ctor = self.get_value(operands[1])?;
                let args_val = self.get_value(operands[2])?;
                let args = self
                    .array_like_elements(&args_val)
                    .ok_or_else(|| {
                        RuntimeError::TypeError(
                            "NewSpread: arguments must be array-like".to_string(),
                        )
                    })?;
                let constructed = self.construct(&ctor, &args, module)?;
                self.set_value(operands[0], constructed)?;
            }
            Opcode::ToString => {
                let src = self.get_value(operands[1])?;
                let s = match self.to_primitive(&src, "string", module) {
                    Ok(v) => v.to_js_string(),
                    Err(_) => src.to_js_string(),
                };
                self.set_value(operands[0], Value::string(&s))?;
            }
            Opcode::ToNumber => {
                // ES 7.1.4 ToNumber: objects convert through
                // ToPrimitive(hint "number"), which may run user code.
                let src = self.get_value(operands[1])?;
                let prim = match self.to_primitive(&src, "number", module) {
                    Ok(v) => v,
                    Err(_) => src.clone(),
                };
                self.set_value(operands[0], Value::Number(prim.to_number()))?;
            }

            // ===== Object/Array Operations =====
            Opcode::MakeArray => {
                let mut arr = crate::vm::object::ArrayObject::new();
                arr.set_prototype(Some(Rc::clone(&self.builtins.array_prototype)));
                let arr_val = Value::Object(Rc::new(RefCell::new(arr)));
                self.set_value(operands[0], arr_val)?;
            }
            Opcode::ArrayPushSpread => {
                // `[...src]` / `f(...src)`: append every element of `src`.
                // The loop runs here rather than in lowered bytecode so that no
                // value has to stay live across a basic-block boundary.
                let array_val = self.get_value(operands[0])?;
                let src = self.get_value(operands[1])?;
                let items = self.array_like_elements(&src).ok_or_else(|| {
                    RuntimeError::TypeError(format!("{} is not iterable", src.type_of()))
                })?;
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
            Opcode::ArrayPush => {
                let array_val = self.get_value(operands[0])?;
                let elem = self.get_value(operands[1])?;
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
            Opcode::MakeObject => {
                let mut obj = crate::vm::object::OrdinaryObject::new();
                obj.set_prototype(Some(Rc::clone(&self.builtins.object_prototype)));
                let obj_val = Value::Object(Rc::new(RefCell::new(obj)));
                self.set_value(operands[0], obj_val)?;
            }
            Opcode::IndexGet | Opcode::PropGet => {
                let obj = self.get_value(operands[1])?;
                let key = self.resolve_property_key(operands[2], module)?;
                // Boxing a bare function reference has to be written back so that
                // later uses of the same slot observe the same prototype object.
                let obj = self.as_object_value(&obj);
                // Only writable locations can cache the boxed function object;
                // an inline `Value::Function` operand (e.g. `(function(){}).x`)
                // is not addressable.
                if is_addressable(operands[1]) {
                    if matches!(self.get_value(operands[1])?, Value::Function(_)) {
                        self.set_value(operands[1], obj.clone())?;
                    }
                }
                let value = self.get_member(&obj, &key, module)?;
                self.set_value(operands[0], value)?;
            }
            Opcode::IndexSet | Opcode::PropSet => {
                let obj = self.get_value(operands[0])?;
                let key = self.resolve_property_key(operands[1], module)?;
                let val = self.get_value(operands[2])?;
                let obj = self.as_object_value(&obj);
                self.set_member(&obj, key, val, module)?;
                // Persist the boxed function object back into its slot.
                if is_addressable(operands[0]) {
                    self.set_value(operands[0], obj)?;
                }
            }
            Opcode::IndexDelete | Opcode::PropDelete => {
                let obj = self.get_value(operands[1])?;
                let key = self.resolve_property_key(operands[2], module)?;
                let obj = self.as_object_value(&obj);
                let removed = self.delete_member(&obj, &key)?;
                self.set_value(operands[0], Value::Bool(removed))?;
            }
            Opcode::Arguments => {
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
                self.set_value(operands[0], obj)?;
            }
            Opcode::StoreEnv => {
                let name_index = operands[0].as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(operands[1])?;
                self.state.globals.insert(name, value);
            }
            Opcode::CallMethod => {
                let mut obj_val = self.get_value(operands[0])?;
                // A bare function reference must be boxed before looking up the
                // method, otherwise `F.method()` silently resolves to undefined.
                obj_val = self.as_object_value(&obj_val);
                if is_addressable(operands[0])
                    && matches!(self.get_value(operands[0])?, Value::Function(_))
                {
                    self.set_value(operands[0], obj_val.clone())?;
                }
                let arg_count = operands[2].as_immd() as usize;

                let method_name = match operands[1] {
                    Operand::Immd(id) => match &module.constants[id as usize] {
                        Constant::String(s) => s.as_str().to_string(),
                    },
                    _ => {
                        let prop_val = self.get_value(operands[1])?;
                        prop_val.to_js_string()
                    }
                };

                // Symbol-keyed method call (`obj[Symbol.iterator]()`): the key
                // must be resolved through the property map by symbol identity;
                // stringifying it (the default path below) would never match.
                // The key value is read with the caller's frame pointer; the
                // dispatch itself happens after the frame switch below.
                let symbol_key = if matches!(operands[1], Operand::Immd(_)) {
                    None
                } else {
                    match self.get_value(operands[1])? {
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
                        "Object.defineProperty" | "Object.defineProperties" | "Object.create"
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
                let method_val = self.lookup_property_on_object(&obj_val, &method_name);

                // A native prototype method (e.g. a method reached through the
                // prototype chain rather than the fast dispatch above).
                if let Some(name) = crate::builtins::native_function_name(&method_val) {
                    let result = self.call_native_by_name(&name, obj_val, &args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                match method_val {
                    Value::Function(id) => {
                        self.state.enter_frame(arg_count)?;
                        // User-defined bytecode function - call it with this = obj_val
                        self.state.this_val = obj_val;
                        self.state.function_val = self.materialize_function(id);
                        match module.symtab.get(&FunctionId::new(id)) {
                            Some(location) => {
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
                                    "undefined function: {id}"
                                )));
                            }
                        }
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
                                self.state.enter_frame(arg_count)?;
                                // For arrow functions, use captured this; for regular methods, use obj_val
                                self.state.this_val = captured_this.unwrap_or(obj_val);
                                self.state.function_val = Value::Object(Rc::clone(&obj_ref));
                                // Push captured variables onto closure_var_stack
                                for (name, value) in &captured_vars {
                                    let mut map = std::collections::HashMap::new();
                                    map.insert(name.clone(), value.clone());
                                    self.state.closure_var_stack.push(map);
                                }
                                match module.symtab.get(&FunctionId::new(id)) {
                                    Some(location) => {
                                        self.state.pushc(self.state.closure_var_stack.len())?;
                                        self.state.pushc(self.state.seh_stack.len())?;
                                        self.state.pushc(self.state.pc + 1)?;
                                        self.state.construct_stack.push(false);
                                        self.state
                                            .new_target_stack
                                            .push(captured_new_target.unwrap_or(Value::Undefined));
                                        self.state.jump(*location);
                                        return Ok(());
                                    }
                                    None => {
                                        return Err(RuntimeError::ReferenceError(format!(
                                            "undefined function: {id}"
                                        )));
                                    }
                                }
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
            Opcode::Try => {
                let catch_offset = operands[0].as_immd();
                let finally_offset = operands[1].as_immd();
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
            Opcode::EndTry => {
                self.state.seh_stack.pop();
            }
            Opcode::ThrowExc => {
                let exc_val = match operands[0] {
                    Operand::Immd(id) => {
                        let constant = &module.constants[id as usize];
                        VM::from_constant(constant)
                    }
                    _ => self.get_value(operands[0])?,
                };
                return self.handle_throw(exc_val);
            }
            Opcode::LoadException => {
                let exc_val = self.state.get_register(Register::Rv)?;
                self.set_value(operands[0], exc_val)?;
            }

            // ===== Function/Closure =====
            Opcode::CreateClosure => {
                let func_id = operands[1].as_immd();
                self.set_value(operands[0], Value::Function(func_id as u32))?;
            }
            Opcode::New => {
                let constructor_val = self.get_value(operands[0])?;
                let arg_count = operands[1].as_immd() as usize;
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
                    Value::Function(id) if module.generators.contains(&id) => {
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
                            self.set_value(operands[0], func_obj.clone())?;
                            (id, proto.map(|d| d.value))
                        } else {
                            (id, None)
                        }
                    }

                    Value::Object(obj_ref) => {
                        let borrowed = obj_ref.borrow();
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
                            let rv = self.native_construct(&name, &ctor, &args, module)?;
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
            Opcode::LoadThis => {
                if self.state.this_uninitialized.last() == Some(&true) {
                    return Err(self.this_not_initialized());
                }
                let value = self.state.this_val.clone();
                self.set_value(operands[0], value)?;
            }
            Opcode::LoadNewTarget => {
                // `new.target` is per frame: the constructor for a `[[Construct]]`
                // frame, otherwise undefined (an arrow frame carries the value
                // captured when the arrow object was created).
                let value = self
                    .state
                    .new_target_stack
                    .last()
                    .cloned()
                    .unwrap_or(Value::Undefined);
                self.set_value(operands[0], value)?;
            }
            Opcode::LoadCurrentFunction => {
                let value = self.state.function_val.clone();
                self.set_value(operands[0], value)?;
            }
            Opcode::MakeFuncObj => {
                let func_id = match operands[1] {
                    Operand::Symbol(sym) => sym,
                    Operand::Immd(val) => val as u32,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "invalid MakeFuncObj operand".to_string(),
                        ));
                    }
                };
                let obj_val = self.materialize_function(func_id);
                self.set_value(operands[0], obj_val)?;
            }
            Opcode::MakeArrowFuncObj => {
                let func_id = match operands[1] {
                    Operand::Symbol(sym) => sym,
                    Operand::Immd(val) => val as u32,
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "invalid MakeArrowFuncObj func_id operand".to_string(),
                        ));
                    }
                };
                let captured_this = match self.get_value(operands[2])? {
                    // Lowering leaves the operand empty (see `lower_arrow_function`);
                    // the arrow captures whatever the frame's `this` is.
                    v if v.is_undefined() => self.state.this_val.clone(),
                    v => v,
                };
                // Collect all pending captured variables from closure_var_stack
                // The variables were pushed by ClosureVar instructions in order
                let mut captured_vars: Vec<(String, Value)> = Vec::new();
                while let Some(vars) = self.state.closure_var_stack.last() {
                    if vars.is_empty() {
                        break;
                    }
                    // Pop the outermost entry (most recently pushed group)
                    let entry = self.state.closure_var_stack.pop().unwrap();
                    // Insert at front to maintain original order
                    for (k, v) in entry {
                        captured_vars.push((k, v));
                    }
                }
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
                self.set_value(operands[0], obj_val)?;
            }
            Opcode::ClosureVar => {
                let name_index = operands[0].as_immd() as usize;
                let name = match &module.constants[name_index] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(operands[1])?;
                // Push as a single-entry map onto closure_var_stack
                let mut map = HashMap::new();
                map.insert(name, value);
                self.state.closure_var_stack.push(map);
            }
            Opcode::CallNative => {
                let callable = self.get_value(operands[0])?;
                let arg_count = operands[1].as_immd() as usize;
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

            Opcode::Halt | Opcode::Ret => {
                // These are handled in run(), not here
                unreachable!("Halt/Ret should be handled in run()")
            }
            Opcode::ResumeExc => {
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
                        // Delayed return after finally execution
                        self.state.seh_stack.pop();
                        // Continue to normal return flow - re-execute Ret opcode
                        // The return value is already in Rv
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
        let len_value = crate::vm::prototype::internal_get(
            Rc::clone(&obj),
            &PropertyKey::from_str("length"),
            None,
        )
        .map_err(RuntimeError::TypeError)?;
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
        while state.this_uninitialized.len() < state.this_stack.len() {
            state.this_uninitialized.push(false);
        }
        if let Some(slot) = state.this_uninitialized.last_mut() {
            *slot = true;
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

    /// Run an `Array.prototype` method that has to call back into user code.
    ///
    /// Returns `Ok(None)` when `method` is not one of those methods, so callers
    /// can fall back to the ordinary dispatch chain.
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
                    receiver.clone(),
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
                        &[acc, element, Value::Number(i as f64), receiver.clone()],
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
        // not leave its flag behind, or a later `Ret` would raise again.
        self.state.this_uninitialized.truncate(this_depth);
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
                    .map_err(|e| RuntimeError::TypeError(e))
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
                        return match desc.getter {
                            Some(getter) => self.invoke(&getter, receiver, &[], module),
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
                return match desc.getter {
                    Some(getter) => self.invoke(&getter, receiver.clone(), &[], module),
                    None => Ok(Value::Undefined),
                };
            }
            return Ok(desc.value);
        }
        Ok(Value::Undefined)
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
                        return match desc.setter {
                            Some(setter) => {
                                self.invoke(&setter, receiver, &[value], module)?;
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
                        .map_err(RuntimeError::TypeError)?;
                    return Ok(());
                }
                let mut borrowed = obj_ref.borrow_mut();
                borrowed
                    .property_set(key, value)
                    .map_err(RuntimeError::TypeError)?;
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
    fn delete_member(&mut self, obj: &Value, key: &PropertyKey) -> Result<bool, RuntimeError> {
        match obj {
            Value::Object(obj_ref) => crate::vm::prototype::internal_delete(Rc::clone(obj_ref), key)
                .map_err(RuntimeError::TypeError),
            Value::Function(id) => {
                let boxed = self.materialize_function(*id);
                self.delete_member(&boxed, key)
            }
            _ => Err(RuntimeError::TypeError(
                "Cannot delete property of a non-object".to_string(),
            )),
        }
    }

    /// Look up a property on an object via its prototype chain.
    fn lookup_property_on_object(&self, obj: &Value, prop_name: &str) -> Value {
        match obj {
            Value::Object(obj_ref) => {
                let key = PropertyKey::from_str(prop_name);
                crate::vm::prototype::internal_get(obj_ref.clone(), &key, None)
                    .unwrap_or(Value::Undefined)
            }
            Value::String(s) => {
                if prop_name == "length" {
                    Value::Number(s.len() as f64)
                } else {
                    Value::Undefined
                }
            }
            _ => Value::Undefined,
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
    ) -> Value {
        let id = self.next_generator_id;
        self.next_generator_id += 1;
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
        value
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
            let location = *module.symtab.get(&FunctionId::new(func_id)).unwrap();
            let argc = args.len();
            for arg in args.iter().rev() {
                self.state.push(arg.clone())?;
            }
            self.state.rbp = self.state.rsp;
            self.state.enter_frame(argc)?;
            self.state.this_val = this;
            self.state.function_val = self.materialize_function(func_id);
            for (name, value) in &captured_vars {
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
                gobj.suspended
                    .take()
                    .expect("SuspendedYield without a frame")
            };
            let base = self.state.data_stack.len();
            self.state.data_stack.extend(frame.data.iter().cloned());
            self.state.rbp = base + frame.argc;
            self.state.rsp = self.state.rbp + frame.bp_offset;
            self.state.this_val = frame.this.clone();
            self.state.function_val = frame.function_val.clone();
            self.state.enter_frame(frame.argc)?;
            for map in &frame.closure_maps {
                self.state.closure_var_stack.push(map.clone());
            }
            self.state.pushc(self.state.closure_var_stack.len())?;
            self.state.pushc(self.state.seh_stack.len())?;
            self.state.pushc(self.resume_sentinel(module))?;
            self.state.construct_stack.push(false);
            self.state.new_target_stack.push(Value::Undefined);
            // `yield expr` evaluates to the value the resumer supplied. The
            // instruction itself does not run again, so the destination is read
            // from the bytecode and written here, with the restored `rbp`.
            // The register file is global, so the body's in-flight values have
            // to come back before anything else runs.
            for (slot, value) in self
                .state
                .registers
                .iter_mut()
                .zip(frame.registers.iter())
            {
                *slot = value.clone();
            }
            if let Some(inst) = module.instructions.get(frame.pc) {
                if matches!(inst.opcode, Opcode::Yield) {
                    let dst = inst.operands[0];
                    self.set_value(dst, send)?;
                }
            }
            self.state.jump(frame.pc + 1);
        }

        let closure_depth = self.state.closure_var_stack.len();
        self.generator_yielded = None;
        let outcome = (|| -> Result<(), RuntimeError> {
            while self.step(module)? {}
            Ok(())
        })();

        // A `yield` left the frame on the value stack: lift it out before the
        // caller's context is restored (which would rewind `rsp` below it).
        let yielded = self.generator_yielded.take();
        let suspended = if yielded.is_some() {
            Some(self.extract_generator_frame(closure_depth))
        } else {
            None
        };
        let rv = self.state.get_register(Register::Rv)?;
        if suspended.is_some() {
            // The body suspended rather than returned, so `Ret` never ran and
            // the frame's bookkeeping is still on the stacks. Unwind it before
            // the resumer's context is restored (which only truncates the
            // stacks it saved — the control stack is not among them).
            let _ = self.state.popc();
            let _ = self.state.popc();
            let _ = self.state.popc();
            self.state.function_stack.pop();
        }
        self.restore_execution_state(&saved);
        outcome?;

        let Value::Object(obj_ref) = &gen_value else {
            return Err(RuntimeError::TypeError("not a generator".to_string()));
        };
        let mut gobj = obj_ref.borrow_mut();
        let Some(gobj) = gobj
            .as_any_mut()
            .downcast_mut::<crate::vm::object::GeneratorObject>()
        else {
            return Err(RuntimeError::TypeError("not a generator".to_string()));
        };
        match (yielded, suspended) {
            (Some(value), Some(frame)) => {
                gobj.state = crate::vm::object::GeneratorState::SuspendedYield;
                gobj.suspended = Some(frame);
                Ok(Self::iterator_result(value, false))
            }
            _ => {
                gobj.state = crate::vm::object::GeneratorState::Completed;
                gobj.suspended = None;
                Ok(Self::iterator_result(rv, true))
            }
        }
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
        self.state.this_uninitialized.truncate(saved.this_depth);
        self.state.frame_argc.truncate(saved.this_depth);
        self.state.this_val = saved.this.clone();
        self.state.function_val = saved.function.clone();
        self.state.construct_stack.truncate(saved.construct);
        self.state.new_target_stack.truncate(saved.new_target);
        // The register file is *global*, not per frame: a nested run (a
        // generator body, an `invoke`) overwrites values the caller still has
        // in registers — an array literal built around `it.next()` is the
        // common victim.
        for (slot, value) in self.state.registers.iter_mut().zip(saved.registers.iter()) {
            *slot = value.clone();
        }
    }

    /// Lift the frame currently on top of the value stack out of it, so the
    /// stack can be rewound to the resumer's position.
    fn extract_generator_frame(
        &mut self,
        closure_depth: usize,
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
            closure_maps: self.state.closure_var_stack[closure_depth..].to_vec(),
            registers: std::array::from_fn(|i| self.state.registers[i].clone()),
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

        let state = match &src {
            Value::Object(obj_ref) => {
                let is_array = obj_ref.borrow().kind() == ObjectKind::Array;
                if is_array {
                    let items = self.array_like_elements(&src).unwrap_or_default();
                    Some(NativeIteratorState::Array { items, idx: 0 })
                } else {
                    None
                }
            }
            Value::String(s) => Some(NativeIteratorState::String {
                chars: s.chars().map(|c| Value::string(&c.to_string())).collect(),
                idx: 0,
            }),
            _ => None,
        };

        let state = match state {
            Some(state) => state,
            None => {
                // Slow path: `src[Symbol.iterator]()`.
                let factory = self.get_member(&src, &iterator_symbol_key(), module)?;
                if !factory.is_callable() {
                    return Err(RuntimeError::TypeError(format!(
                        "{} is not iterable",
                        src.type_of()
                    )));
                }
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
    fn iterator_close(&mut self, iter_val: Value, module: &Module) -> Result<(), RuntimeError> {
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
        if return_fn.is_callable() {
            // Swallow errors from `return()` — the original completion wins.
            let _ = self.invoke(&return_fn, iterator, &[], module);
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
        // See `invoke_with_new_target`: exceptions escaping this frame must be
        // propagated to the caller rather than handled from inside this loop.
        self.invoke_boundaries.push(self.state.ctrl_stack.len());

        for arg in args.iter().rev() {
            self.state.push(arg.clone())?;
        }
        self.state.rbp = self.state.rsp;
        let return_pc = module.instructions.len();

        self.state.enter_frame(args.len())?;
        self.state.this_val = new_obj;
        for (name, value) in &captured_vars {
            let mut map = std::collections::HashMap::new();
            map.insert(name.clone(), value.clone());
            self.state.closure_var_stack.push(map);
        }

        // `new.target` of a `[[Construct]]` frame is the constructor itself.
        let new_target_value = match callee {
            Value::Function(id) => self.materialize_function(*id),
            other => other.clone(),
        };
        self.state.function_val = new_target_value.clone();

        match module.symtab.get(&FunctionId::new(func_id)) {
            Some(location) => {
                self.state.set_register(Register::Rv, Value::Undefined)?;
                self.state.pushc(self.state.closure_var_stack.len())?;
                self.state.pushc(self.state.seh_stack.len())?;
                self.state.pushc(return_pc)?;
                self.state.construct_stack.push(true);
                self.state.new_target_stack.push(new_target_value);
                self.state.jump(*location);
            }
            None => {
                return Err(RuntimeError::ReferenceError(format!(
                    "undefined function: {func_id}"
                )));
            }
        }

        let outcome = (|| -> Result<(), RuntimeError> {
            while self.step(module)? {}
            Ok(())
        })();

        self.invoke_boundaries.pop();

        self.state.pc = saved_pc;
        self.state.rsp = saved_rsp;
        self.state.rbp = saved_rbp;
        self.state.closure_var_stack.truncate(saved_closure);
        self.state.seh_stack.truncate(saved_seh);
        self.state.this_stack.truncate(saved_this_depth);
        self.state.this_uninitialized.truncate(saved_this_depth);
        self.state.frame_argc.truncate(saved_this_depth);
        self.state.this_val = saved_this;
        self.state.function_val = saved_function;
        self.state.construct_stack.truncate(saved_construct);
        self.state.new_target_stack.truncate(saved_new_target);

        outcome?;
        let rv = self.state.get_register(Register::Rv)?;
        Ok(rv)
    }

    /// `[[Construct]]` (ES6 7.3.19): create the instance, run the constructor
    /// with `this` bound to it, and apply the construct return semantics.
    fn construct(
        &mut self,
        constructor_val: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // Built-in constructors (Object, Array, Error, `f.bind(…)`, …) share
        // their `[[Construct]]` with the `New` opcode.
        if let Some(name) = crate::builtins::native_function_name(constructor_val) {
            return self.native_construct(&name, constructor_val, args, module);
        }

        // Bytecode constructor: resolve the prototype from the (boxed)
        // constructor's `prototype` property, create the instance, and run the
        // constructor body via the [[Construct]] frame flag.
        let boxed_ctor = match constructor_val {
            Value::Function(id) => self.materialize_function(*id),
            other => other.clone(),
        };
        let prototype = match &boxed_ctor {
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

        self.invoke_construct(&boxed_ctor, new_obj_val, args, module)
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

        // The instance prototype is `C.prototype`, *not* the constructor's own
        // `[[Prototype]]` (which is `Function.prototype`).
        let proto = match constructor_val {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .property_get(&PropertyKey::from_str("prototype"))
                .map(|d| d.value),
            _ => None,
        };
        let mut this_obj = crate::vm::object::OrdinaryObject::new();
        match &proto {
            Some(Value::Object(p)) => this_obj.set_prototype(Some(Rc::clone(p))),
            _ => this_obj.set_prototype(Some(Rc::clone(&self.builtins.object_prototype))),
        }
        let this_val = Value::Object(Rc::new(RefCell::new(this_obj)));

        let result = crate::builtins::call_native(name, args)?;
        Ok(self.finish_native_construct(result, &proto, this_val))
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
                        Err(e) => Err(RuntimeError::TypeError(e)),
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
    globals: HashMap<String, Value>,
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
    /// Per frame: `this` has not been bound yet. Only ever true for a
    /// derived-class constructor (`class C extends P`), whose `this` exists as
    /// a binding but whose value is the *uninitialized* TDZ-like state until
    /// `super()` supplies it (ES 9.2.2 / 12.3.5.1).
    this_uninitialized: Vec<bool>,
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
        Self {
            data_stack: Vec::with_capacity(1024),
            ctrl_stack: Vec::with_capacity(256),
            registers: std::array::from_fn(|_| Value::Undefined),
            seh_stack: Vec::new(),
            globals: HashMap::new(),
            this_val: Value::Undefined,
            closure_var_stack: Vec::new(),
            construct_stack: Vec::new(),
            new_target_stack: Vec::new(),
            function_val: Value::Undefined,
            function_stack: Vec::new(),
            this_stack: Vec::new(),
            this_uninitialized: Vec::new(),
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
        self.this_uninitialized.push(false);
        self.function_stack.push(self.function_val.clone());
        self.frame_argc.push(argc);
        Ok(())
    }

    fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.get(name).cloned()
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

/// SEH (Structured Exception Handling) record for try/catch/finally
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
    registers: [Value; 19],
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
    use crate::bytecode::{Bytecode, Constant, Opcode, Operand, Primitive, Register};

    // ──────────────────────── State ────────────────────────

    #[test]
    fn test_state_new() {
        let state = State::new();
        assert_eq!(state.pc, 0);
        assert_eq!(state.rsp, 0);
        assert_eq!(state.rbp, 0);
        assert!(state.ctrl_stack.is_empty());
        assert!(state.seh_stack.is_empty());
        assert!(state.globals.is_empty());
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
        state.globals.insert("x".to_string(), Value::Number(42.0));
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
        instructions: Vec<Bytecode>,
        constants: Vec<Constant>,
    ) -> crate::bytecode::Module {
        crate::bytecode::Module::new(
            Some("test".to_string()),
            constants,
            HashMap::new(),
            HashMap::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            instructions,
        )
    }

    #[test]
    fn test_vm_run_halt() {
        let module = make_module(vec![Bytecode::empty(Opcode::Halt)], vec![]);
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Undefined);
    }

    #[test]
    fn test_vm_run_mov_halt() {
        let module = make_module(
            vec![
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(true)),
                ),
                Bytecode::empty(Opcode::Halt),
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
                Bytecode::double(
                    Opcode::LoadConst,
                    Operand::Register(Register::Rv),
                    Operand::Immd(0),
                ),
                Bytecode::empty(Opcode::Halt),
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
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::R0),
                    Operand::Primitive(Primitive::Float(10.0)),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::R1),
                    Operand::Primitive(Primitive::Float(3.0)),
                ),
                Bytecode::triple(
                    Opcode::Addx,
                    Operand::Register(Register::R2),
                    Operand::Register(Register::R0),
                    Operand::Register(Register::R1),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Register(Register::R2),
                ),
                Bytecode::empty(Opcode::Halt),
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
                Bytecode::single(Opcode::Jump, Operand::Immd(2)),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(false)),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(true)),
                ),
                Bytecode::empty(Opcode::Halt),
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
                Bytecode::triple(
                    Opcode::BrIf,
                    Operand::Primitive(Primitive::Boolean(true)),
                    Operand::Immd(2),
                    Operand::Immd(1),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(false)),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(true)),
                ),
                Bytecode::empty(Opcode::Halt),
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
                Bytecode::triple(
                    Opcode::BrIf,
                    Operand::Primitive(Primitive::Boolean(false)),
                    Operand::Immd(1),
                    Operand::Immd(2),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(true)),
                ),
                Bytecode::double(
                    Opcode::Mov,
                    Operand::Register(Register::Rv),
                    Operand::Primitive(Primitive::Boolean(false)),
                ),
                Bytecode::empty(Opcode::Halt),
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
