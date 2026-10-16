//! 执行循环：`step` 与指令分派。
//!
//! `run_instruction` 在 P0-4 第一步之后只剩一张薄分派表（原来 1740 行的巨型
//! `match`），各语义组的分支体住在下面的 `run_<group>` 里。
//!
//! `step` 自己处理 `Halt` / `Ret`（它们决定还要不要继续跑），其余指令
//! 交给 `run_instruction`。

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;
use std::time::Instant;

use crate::bytecode::{Constant, Instr, Module, Opcode, Operand, Register};
use crate::vm::gc::Object;
use crate::vm::object::{JSObject, PromiseState};
use crate::vm::{
    ArrayObject, BOUND_PREFIX, BoundFunction, CallArgs, CalleeKind, Control, FrameBirth,
    FrameMode, GUARD_CHECK_INTERVAL, GuardFired,
    ObjectKind, PropertyKey, RuntimeError, SehRecord, THIS_DERIVED_BOUND,
    THIS_DERIVED_UNBOUND, THIS_NONE, Value, VM, finalize_json, is_addressable,
    memory_pressure, to_int32, to_uint32, vm_trace_steps,
};

impl VM {
    /// Execute a single bytecode instruction. Returns `Ok(false)` when execution
    /// should stop (Halt, or the program counter is out of bounds), and `Ok(true)`
    /// to continue. This is factored out so that other methods can run a nested
    /// execution loop (e.g. for re-entrant function calls such as ToPrimitive).
    pub(crate) fn step(&mut self, module: &Module) -> Result<bool, RuntimeError> {
        // ── GC 安全点 ──
        //
        // `alloc()` 内部**绝不**触发收集（INV-4）：那会在调用方栈上的临时 `Gc`
        // 还没入根时就把对象收走。收集只在这里 —— **指令边界** —— 发生，此刻
        // 所有活着的 JS 值都在 VM 的状态里（`data_stack` / `registers` / `frames`…），
        // 由 `Trace for VM` 精确枚举。
        //
        // 两段式借用：先 `&self` 收根，再 `&mut self.heap` 收集。
        //
        // **`invoke_boundaries` 非空 ⇒ 跳过收集**：此刻正有一段由原生内建驱动的
        // 调用在飞行中（`Array.prototype.map` 的回调、Map/Set 构造器的迭代协议、
        // `JSON.parse` 的 reviver…），那些原生把 `Value` 握在 **Rust 局部变量**里，
        // 而收集器看不见机器栈 —— 收一次就会把它们收走（GC stress 实测 6 条失败：
        // "Map constructor: 'set' is not callable"）。宿主侧入根（`Scope`）是正解，
        // 但在那之前，这是不让收集器踩空的闸门。
        if self.heap.should_collect() {
            let mut roots = crate::vm::gc::Worklist::new();
            crate::vm::gc::Trace::trace(self, &mut roots);
            self.heap.collect(roots);
        }
        // Borrowed, not cloned: `module` and `self` are distinct, so the
        // instruction can be matched in place — no 48-byte copy per step.
        let inst = match module.instructions.get(self.state.pc()) {
            Some(i) => i,
            None => return Ok(false),
        };
        vm_trace_steps(|w| {
            let op = format!("{inst:?}");
            let op = op.split([' ', '{']).next().unwrap_or("?");
            let _ = writeln!(
                w,
                "step={} pc={} op={} rsp={} rbp={} frames={}",
                self.steps,
                self.state.pc(),
                op,
                self.state.rsp,
                self.state.rbp,
                self.state.frames.len(),
            );
        });
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
        // M2-1 互校：镜面与各平行栈必须一致。放在每条指令之前跑（debug 下），
        // 所以任何一处开/收帧的分叉都会在下一条指令上被抓到，而不是等到很久以后。
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
                    // "已经回到最外层"：只剩顶层脚本那一帧。
                    // 以前问的是"控制栈空不空"，
                    // 而控制栈上还混着 `PushC`/`PopC` 的条目（两条知识混在一条栈上）。
                    if self.state.frames.len() == 1 {
                        // The return value is already in Rv; stop the loop so the
                        // caller can read it.
                        return Ok(false);
                    }
                    // A derived constructor that returns without `super()` —
                    // implicitly or via `return this` — has no `this` to return.
                    // Raised through the SEH machinery: `assert.throws(
                    // ReferenceError, () => new C())` has to see it.
                    if self.state.frames.last().map(|f| f.this_state) == Some(THIS_DERIVED_UNBOUND) {
                        let err = self.this_not_initialized();
                        if let Some(exc) = self.as_js_exception(&err) {
                            self.handle_throw(exc)?;
                            return Ok(true);
                        }
                        return Err(err);
                    }
                    // The frame's own constructor state, captured before the
                    // pops below take it off the stack.
                    let frame_ctor_state = self.state.frames.last().map(|f| f.this_state);
                    // 三个水位现在住在帧里（`Frame{closure_depth, seh_depth}`），
                    // （切片 1b 之前这里要从控制栈弹三项；那套簿记已退役。）
                    let (saved_seh_depth, saved_closure_depth) = {
                        let frame = self.state.frames.last().expect("Ret 时必有帧");
                        (frame.seh_depth, frame.closure_depth)
                    };
                    // The frame's own `this`, needed for [[Construct]] below.
                    // It has to be captured *before* the caller's binding is
                    // restored, otherwise a constructor would return the
                    // caller's `this` instead of the object it just built.
                    let frame_this = self.current_this();
                    // Captured before the [[Construct]] coercion below, which
                    // would replace a primitive return value with `this`.
                    let returned = self.state.get_register(Register::Rv)?;
                    // 调用方的 `this` 与构造器状态随 `frames.pop()` 一起回来 —— 它们在帧里。
                    // If this frame was invoked via `new`, apply [[Construct]] return
                    // semantics: a returned object becomes the result, otherwise the
                    // newly created `this` object is used.
                    // `[[Construct]]` 语义用的 flag 也在帧里（以前要 pop 一条平行栈）。
                    let frame_construct = self.state.frames.last().is_some_and(|f| f.construct);
                    if frame_construct {
                        let rv = self.state.get_register(Register::Rv)?;
                        if !rv.is_object() {
                            self.state.set_register(Register::Rv, frame_this)?;
                        }
                    }
                    // 本帧收掉了；它自己带着所有按帧的值，退帧只有上面这一处。
                    self.unwind_frames_to(
                        self.state.frames.len() - 1,
                        saved_closure_depth,
                        saved_seh_depth,
                    );
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
                    // 不再 jump(return_pc) —— 见 State::pc 的说明。
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
    pub(crate) fn as_js_exception(&mut self, err: &RuntimeError) -> Option<Value> {
        match err {
            RuntimeError::TypeError(_)
            | RuntimeError::RangeError(_)
            | RuntimeError::ReferenceError(_)
            | RuntimeError::SyntaxError(_)
            | RuntimeError::Thrown(_) => {
                Some(crate::builtins::runtime_error_to_js_error(&mut self.heap, err, &self.builtins))
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
    pub(crate) fn make_bound(&mut self, target: Value, this_arg: Value, bound_args: Vec<Value>) -> Value {
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
        Value::Object(self.heap.alloc(Object::NativeFunction(
            crate::vm::object::NativeFunctionObject::new(&format!("{BOUND_PREFIX}{id}")),
        )))
    }

    /// Argument list for `f.call(...)` / `f.apply(...)`, minus the `thisArg`.
    pub(crate) fn call_or_apply_args(
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
    pub(crate) fn check_class_ctor(&self, callee: &Value) -> Result<(), RuntimeError> {
        if let Value::Object(obj_ref) = callee {
            let borrowed = obj_ref.borrow();
            if borrowed.as_function()
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
    pub(crate) fn call_reentrant(
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
    pub(crate) fn callable_func_id(&self, callee: &Value) -> Option<u32> {
        match callee {
            Value::Function(id) => Some(*id),
            Value::Object(obj_ref) => obj_ref
                .borrow().as_function()
                .map(|func_obj| func_obj.func_id),
            _ => None,
        }
    }

    /// 生成器函数的函数号（`None` = 不是生成器）。
    ///
    /// **判定的唯一来源**：`callable_func_id` + 那个函数自己的 `FnFlags::generator`。
    /// 代理、原生内建、绑定函数都没有字节码函数号，所以这里不必再判它们是不是 ——
    /// 而以前五个调用点各写一遍这套组合（写法还各不相同：`CallMethod` 先
    /// `callable_func_id` 再查集合，`New` 反过来用"生成器不是构造器"，`Call` 只有裸函数号）。
    pub(crate) fn generator_of(&self, callee: &Value, module: &Module) -> Option<u32> {
        self.callable_func_id(callee)
            .filter(|id| module.is_generator(*id))
    }

    /// `async` 函数的函数号（`None` = 不是 async）。判定同样只有这一处。
    pub(crate) fn async_of(&self, callee: &Value, module: &Module) -> Option<u32> {
        self.callable_func_id(callee)
            .filter(|id| module.is_async(*id))
    }

    /// 一个可调用体的种类（[`CalleeKind`]）。给需要"一眼看全"的地方用
    /// （`invoke_with_new_target` 与 `New` 的"生成器不是构造器"）。
    pub(crate) fn callee_kind(&self, callee: &Value, module: &Module) -> CalleeKind {
        if Self::is_proxy(callee) {
            return CalleeKind::Proxy;
        }
        if crate::builtins::is_native_function(callee) {
            return CalleeKind::Native;
        }
        match self.callable_func_id(callee) {
            Some(id) if module.is_generator(id) => CalleeKind::Generator,
            Some(id) if module.is_async(id) => CalleeKind::Async,
            Some(_) => CalleeKind::Bytecode,
            None => CalleeKind::NotCallable,
        }
    }

    /// `[[Call]]` 的统一入口（P1-2c）：`CallEx`（以及接下来的 `CallMethod` /
    /// `CallNative` / `CallSpread`）都走这里，可调用体种类的分支只有**这一份**。
    ///
    /// 分支顺序与原来 `CallEx` 那条 arm 一字不差：代理 → 原生内建 → 字节码函数
    /// （生成器 / async / 开帧）。差别只在"结果写哪儿"（`dst`）与"参数从哪来"
    /// （`CallArgs`），而 `this` 由调用方给：普通调用给 `Undefined`，方法调用给对象 ——
    /// 这正是 `CallEx` 与 `CallMethod` 原本各写一遍的那份分支结构的全部差异。
    ///
    /// `Construct` 形态**不在这里**：`New` 的开帧顺序（`this_val` 先被覆盖、
    /// `enter_frame` 后跑）至今是个待查项（§3.3.3），在弄清楚之前不动它。
    pub(crate) fn enter_call(
        &mut self,
        callee: &Value,
        this: Value,
        args: CallArgs<'_>,
        dst: Register,
        module: &Module,
    ) -> Result<Control, RuntimeError> {
        let target = Operand::Register(dst);
        // 1. 代理的 `apply` 陷阱决定整次调用（ES 10.5.12）。
        if Self::is_proxy(callee) {
            let args = self.call_args_vec(args)?;
            let result = self.invoke(callee, this, &args, module)?;
            self.set_value(target, result)?;
            return Ok(Control::Done);
        }
        // 2. 原生内建：按名字分发（`Function.prototype.bind` 的产物也在其中）。
        if let Some(name) = crate::builtins::native_function_name(callee) {
            let args = self.call_args_vec(args)?;
            let result = self.call_native_by_name(&name, this, &args, module)?;
            self.set_value(target, result)?;
            return Ok(Control::Done);
        }
        // 3. 字节码函数。两种拼写（`Value::Function` / 装箱后的 `FunctionObject`）
        //    的差别只有 `this` 怎么定 —— 装箱的那份可能捕获了 `this`。
        match callee {
            Value::Function(id) => {
                if self.generator_of(callee, module).is_some() {
                    let args = self.call_args_vec(args)?;
                    let gobj =
                        self.create_generator(*id, this, args, Vec::new(), module)?;
                    self.set_value(target, gobj)?;
                    return Ok(Control::Done);
                }
                if self.async_of(callee, module).is_some() {
                    let args = self.call_args_vec(args)?;
                    let result = self.invoke(callee, this, &args, module)?;
                    self.set_value(target, result)?;
                    return Ok(Control::Done);
                }
                // 内联开帧：参数**留在栈上**，只记个数。
                let Some(argc) = Self::call_args_argc(args) else {
                    return Err(RuntimeError::InternalError(
                        "enter_call: 内联开帧要求参数仍在栈上".to_string(),
                    ));
                };
                let function_val = self.materialize_function(*id);
                self.open_frame(
                    *id,
                    argc,
                    this,
                    function_val,
                    &[],
                    false,
                    Value::Undefined,
                    module,
                )?;
                Ok(Control::Entered)
            }
            Value::Object(obj_ref) => {
                // 类构造器不能被直接调用。
                self.check_class_ctor(&Value::Object(*obj_ref))?;
                let borrowed = obj_ref.borrow();
                if borrowed.kind() != ObjectKind::Function {
                    return Err(RuntimeError::TypeError("not a function".to_string()));
                }
                let Some(func_obj) = borrowed.as_function()
                else {
                    return Err(RuntimeError::TypeError("not a constructor".to_string()));
                };
                let id = func_obj.func_id;
                let captured_this = func_obj.captured_this.clone();
                let captured_new_target = func_obj.captured_new_target.clone();
                let captured_vars = func_obj.captured_vars.clone();
                drop(borrowed);
                // 装箱的那份可能捕获了 `this`（箭头函数 / 绑定函数）；没有就用调用方
                // 传进来的 —— 普通调用是 `undefined`（`CallEx`），方法调用是接收者
                // （`CallMethod`）。**这里必须用调用方给的那个**：早先写成
                // `captured_this.unwrap_or(Value::Undefined)` 就把方法调用的 `this`
                // 弄丢了（`f.getX()` 里 `this.x` 变成在 `undefined` 上取属性）。
                let boxed = Value::Object(*obj_ref);
                let frame_this = captured_this.clone().unwrap_or(this);
                if self.generator_of(&boxed, module).is_some() {
                    let args = self.call_args_vec(args)?;
                    let gobj = self.create_generator(id, frame_this, args, captured_vars, module)?;
                    self.set_value(target, gobj)?;
                    return Ok(Control::Done);
                }
                if self.async_of(&boxed, module).is_some() {
                    let args = self.call_args_vec(args)?;
                    let result = self.invoke(&boxed, frame_this, &args, module)?;
                    self.set_value(target, result)?;
                    return Ok(Control::Done);
                }
                let Some(argc) = Self::call_args_argc(args) else {
                    return Err(RuntimeError::InternalError(
                        "enter_call: 内联开帧要求参数仍在栈上".to_string(),
                    ));
                };
                self.open_frame(
                    id,
                    argc,
                    frame_this,
                    boxed,
                    &captured_vars,
                    false,
                    captured_new_target.unwrap_or(Value::Undefined),
                    module,
                )?;
                Ok(Control::Entered)
            }
            _ => Err(RuntimeError::TypeError("not a function".to_string())),
        }
    }

    /// `CallArgs` → `Vec<Value>`。**只有需要 `&[Value]` 的分支才调它**；内联开帧
    /// 那条分支一个参数都不许碰（它们还留在数据栈上，帧要原地读）。
    pub(crate) fn call_args_vec(&mut self, args: CallArgs<'_>) -> Result<Vec<Value>, RuntimeError> {
        match args {
            CallArgs::OnStack(n) => self.collect_call_args(n),
            CallArgs::OnStackWithCopy { args, .. } => Ok(args.to_vec()),
            CallArgs::Collected(values) => Ok(values.to_vec()),
        }
    }

    /// 内联开帧要的"参数个数"——只有参数**仍在栈上**时才有。
    pub(crate) fn call_args_argc(args: CallArgs<'_>) -> Option<usize> {
        match args {
            CallArgs::OnStack(n) => Some(n),
            CallArgs::OnStackWithCopy { argc, .. } => Some(argc),
            // `Collected` 的参数已经被弹走（或根本不在栈上），开帧会读到别人的东西。
            CallArgs::Collected(_) => None,
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
                match borrowed.as_function()
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
    /// `enter_frame` → `this` / 函数对象 → 闭包捕获变量 →
    /// 组装出生值 → `note_frame` → `jump` 到函数入口。**顺序有讲究**：
    /// `this` 与函数对象都是**出生参数**（直接进新帧），所以这里不再有"先快照调用方的值"
    /// 这一步 —— 调用方那一帧里本来就有它自己的 `this` 与函数对象。
    ///
    /// 这段以前出现在 7 个地方（`drive_bytecode_frame` 与 `CallEx` / `CallMethod` /
    /// `New` 的内联开帧），差别只有调用方是谁（opcode 走自己的帧，被 `invoke`
    /// 调用时用哨兵 `instructions.len()`）、`this` 与函数对象的来源、
    /// `construct` 标志，以及各自要不要先清 `Rv`（那是记账，留在调用点）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_frame(
        &mut self,
        func_id: u32,
        argc: usize,
        this: Value,
        function_val: Value,
        captured_vars: &[(String, crate::vm::EnvCell)],
        construct: bool,
        new_target: Value,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        // **帧的窗口基址只有一个写入时机**：开帧。
        //
        // 以前它分散在 8 处调用点（每次调用前 `rbp = rsp`），而那些调用点里有一半
        // 最终**不开帧**（原生被调方）—— 于是 `rbp` 同时承担"当前帧的窗口基址"与
        // "调用准备 / 原生实参参照系"两个角色，这也是本次 Migrating 连续回退的根源
        // （见 `docs/archive/deep-arch-log.md` §25/§42）。
        // 收尾的 `rsp` 复位已经改成 `popc rsp`（不依赖 `rbp`），实参也改成 `rsp` 相对
        // 读取 —— 两件事做完之后，"只在开帧时设定一次"才第一次成为可能。
        self.state.rbp = self.state.rsp;
        self.state.enter_frame()?;
        // The callee inherits its captured **cells** — the very objects the
        // The frame's environment starts **here**: everything pushed from this
        // point on belongs to this frame — the cells it inherits first, its own
        // captured locals after. `env_cell` scans from this boundary, and the
        // frame releases this whole range when it returns.
        let closure_depth = self.state.closure_var_stack.len();
        // The callee inherits its captured **cells** — the very objects the
        // defining frame holds, not copies of their current contents.
        for (name, cell) in captured_vars {
            self.state.push_env_cell(name.clone(), cell.clone());
        }
        let location = match module.entry_pc(func_id) {
            Some(location) => location,
            None => {
                return Err(RuntimeError::ReferenceError(format!(
                    "undefined function: {func_id}"
                )));
            }
        };
        let seh_depth = self.state.seh_stack.len();
        self.state.note_frame(FrameBirth {
            pc: location,
            func: function_val.clone(),
            this,
            this_state: THIS_NONE,
            argc,
            construct,
            new_target,
            closure_depth,
            seh_depth,
        });
        Ok(())
    }

    /// Push the callee's frame, run it to completion, then restore the caller's
    /// execution context and answer the frame's `Rv`.
    ///
    /// The frame is driven by a nested `step` loop that stops at the sentinel
    /// return address one past the last instruction.
    ///
    /// **两种模式共用这一份实现**（P1）：`[[Call]]` 与 `[[Construct]]` 的差别只有四处 ——
    /// `this`（传入的 vs 新建的实例）、函数对象/`new.target`、`construct` 标志
    /// 的标志位、以及构造帧先把 `Rv` 清成 `undefined`。以前这四处差异是靠**两份 100 行
    /// 的拷贝**表达的（`drive_bytecode_frame` 与 `invoke_construct`），改一处忘一处就是
    /// B45/B46 那种"为了一个特性到处打补丁"。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn drive_bytecode_frame(
        &mut self,
        func_id: u32,
        callee: &Value,
        args: &[Value],
        captured_vars: &[(String, crate::vm::EnvCell)],
        mode: FrameMode,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let saved_rsp = self.state.rsp;
        let saved_closure = self.state.closure_var_stack.len();
        let saved_pending = self.state.pending_captured.len();
        let saved_seh = self.state.seh_stack.len();
        // **边界 = 调用方的帧深度**，一个值同时回答两个问题：
        // 1. 嵌套循环什么时候停（`frames.len() <= boundary`）；
        // 2. 哪些 SEH 记录属于外层帧、必须让异常穿出去（`handle_throw`）。
        // 切片 1b 之前这是**两个数**（控制栈深度 / 帧深度），
        // 而它们必须保持一致 —— 那就是"两条知识"的来源。
        let boundary = self.state.frames.len();
        self.invoke_boundaries.push(boundary);
        // `PushC`/`PopC` 的保存项也需要一个水位：异常逃出被调帧时，它自己那几条
        // 没来得及 `PopC` 的项必须丢掉 —— 否则外层之后会 `PopC` 到过期的 `rsp`/`rbp`。

        // Push the arguments and open the callee's frame at the new stack top,
        // using the same convention as the `Call`/`CallEx` opcodes (arg0 ends up
        // at `[rbp-1]`).
        for arg in args.iter().rev() {
            self.state.push(arg.clone())?;
        }
        // 不再在这里切 `rbp`：下面 `open_frame` 会在开帧时设定（唯一的写入时机）。

        // Sentinel return address one past the last instruction: when the callee
        // returns, `Ret` lands here and the nested loop stops.
        // 哨兵返回地址 = **整个模块的下一位**，而不是被调函数自己的出口。
        //
        // 嵌套 `step` 循环的终止条件是 `module.instructions.get(pc) == None`，即
        // **pc 越界**；它不是"pc == 某个返回值"。所以哨兵必须是一个**不存在的 pc**。
        // （试过用 `module.body_of(func_id).end` 当哨兵 —— 那是个合法的 pc，于是
        // `Ret` 之后循环接着跑下一段函数的代码，array / async / promise 一片红。
        // 要改成 per-function 哨兵，得先让循环在"pc == 本帧哨兵"时停止 —— 那是
        // 深改第 4 步（帧模型重写）的事，见 `docs/archive/architecture-redesign.md`。）

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
            module,
        )?;

        // Drive the nested frame. On error the caller's context is restored
        // first, so an enclosing `try` still sees the exception: unwinding has
        // to happen before the error is propagated upwards.
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

        // Restore the caller's execution context *before* propagating: the
        // callee frame is gone either way, whether it returned or was unwound
        // by an exception escaping to an outer handler.
        //
        // 正常返回时被调帧自己已经 `PopC` 干净了；异常逃出去的那条路上没有 ——
        // 所以在这里把它的保存项都丢掉。没有这一步，一个 *接住* 异常并继续
        // 在同一帧里跑的原生（`Promise` 的执行器）会 `PopC` 到过期的 `rbp`。
        // 调用方的 `pc` / `this` / `func` / `argc` / `new.target` / **窗口基址** 都由
        // 下面这条 `frames.truncate(boundary)` 一起恢复 —— 它们全在帧里。
        //
        // `rbp` 因此**不需要快照**：它是 `frames.last().base` 的派生值（M2-2b）。
        // 实测（526 条 feature + 全量 test262 4 片）这处"快照值 == 退栈后当前帧的
        // base"**零例外**，而 `handle_throw` / `restore_execution_state` 那两处
        // 不成立 —— 它们跨生成器的挂起边界，快照属于**挂起前**那一次几何。
        self.unwind_frames_to(boundary, saved_closure, saved_seh);
        self.state.pending_captured.truncate(saved_pending);
        // 快照**压过** `unwind_frames_to` 刚定好的派生值：这里是 Rust 驱动调用，
        // `saved_rsp` 是驱动**之前**记录的（实参压栈之前），而派生值是"被弹出那一帧
        // 的出生 `rsp"`（实参压栈之后）—— 两者不同，所以这份快照必须赢。
        self.state.rsp = saved_rsp;
        self.state.rbp = self.state.frame_base();

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
    pub(crate) fn to_property_descriptor(
        &mut self,
        value: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !matches!(value, Value::Object(_) | Value::Function(_)) {
            return Err(RuntimeError::TypeError(
                "Property description must be an object".to_string(),
            ));
        }
        let desc = Value::Object(self.heap.alloc(Object::Ordinary(
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
                    crate::vm::prototype::internal_has_property(*obj, &key)
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
                    crate::vm::prototype::internal_has_property(*obj, &key)
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
    pub(crate) fn to_property_descriptors(
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
        let out = Value::Object(self.heap.alloc(Object::Ordinary(
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
    pub(crate) fn object_assign(&mut self, args: &[Value], module: &Module) -> Result<Value, RuntimeError> {
        let Some(target) = args.first() else {
            return Err(RuntimeError::TypeError(
                "Object.assign requires at least 1 argument".to_string(),
            ));
        };
        let target = crate::builtins::to_object(&mut self.heap, target)?;
        for source in args.iter().skip(1) {
            if matches!(source, Value::Undefined | Value::Null) {
                continue;
            }
            let source = crate::builtins::to_object(&mut self.heap, source)?;
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
    pub(crate) fn json_stringify_full(
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
        let wrapper = Value::Object(self.heap.alloc(Object::Ordinary(wrapper)));

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
    pub(crate) fn json_serialize_property(
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
    pub(crate) fn json_serialize_object(
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
    pub(crate) fn json_serialize_array(
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
    pub(crate) fn json_parse_full(
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
        let parsed = crate::builtins::json_parse(&mut self.heap, &text)?;

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
        let holder = Value::Object(self.heap.alloc(Object::Ordinary(holder)));
        self.json_internalize(&holder, "", &reviver, module)
    }

    /// `InternalizeJSONProperty` (ES 25.5.1.1): walk bottom-up, letting the
    /// reviver replace or delete each property.
    pub(crate) fn json_internalize(
        &mut self,
        holder: &Value,
        name: &str,
        reviver: &Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // 宿主侧入根：`holder` 与 `value` 都跨下面的 `self.invoke(reviver, …)`
        // 存活，而 reviver 是用户函数 ⇒ 必须登记（否则被回收 ⇒ 测试
        // `json::parse_reviver_walks_bottom_up` 在 GC stress 下输出错位）。
        let _root_holder = self.root(holder);
        let value = self.get_member(holder, &PropertyKey::from_str(name), module)?;
        let _root_value = self.root(&value);
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
    pub(crate) fn object_prototype_to_string(
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


    pub(crate) fn run_instruction(&mut self, inst: &Instr, module: &Module) -> Result<(), RuntimeError> {
        // 按**命名字段**解构，而不是"从 3 个槽位里按下标取"（旧编码的形状）。
        // `match *inst` 而非 `match inst`：`Instr` 是 Copy，按值匹配后每个绑定都是
        // `Operand` 而不是 `&Operand`，于是 arm 体里原来的表达式不用改类型。
        // 分组 arm（`A | B`）用位置别名 `a0/a1/a2`，因为 or-模式要求各分支绑定同名，
        // 而 `IndexGet { index }` 与 `PropGet { property }` 的字段名并不相同。
        // 分派：`run_instruction` 只剩这一张表 —— 每条指令在这里**一次性**定位到
        // 自己的 `run_<group>`（一次跳转表命中 + 一次调用），而不是挨个问 17 个组
        // "这条归你吗"。后者写起来更省事，但实测解释器吞吐 -17%。
        //
        // 表是穷尽的（漏一条就编译不过），而它与各组 arm 的对应关系是从 arm 头
        // 反推出来的（90 条不重不漏），不是手抄的。
        //
        // 组函数返回 `true` 表示"这条指令归我"：组内的 `return Ok(true)` 就是原来
        // 的 `return Ok(())`；而不 return 的分支落到组末尾的 `jump_offset(1)`，
        // 与搬移前落到 `match` 之后的行为一致。
        let handled = match *inst {
            Instr::PrologueEnd { .. } | Instr::Yield { .. } |
              Instr::Await { .. } => self.run_suspend(inst, module)?,
            Instr::Call { .. } | Instr::CallEx { .. } => self.run_call(inst, module)?,
            Instr::Jump { .. } | Instr::DelayedJump { .. } | Instr::BrIf { .. } => self.run_jump(inst, module)?,
            Instr::Mov { .. } | Instr::Push { .. } | Instr::Pop { .. } | Instr::AddC { .. } | Instr::SubC { .. } => self.run_stack(inst, module)?,
            Instr::LoadConst { .. } | Instr::MakeRegExp { .. } | Instr::DeclareLexical { .. } |
              Instr::InitLexical { .. } | Instr::SetFunctionName { .. } | Instr::LoadEnv { .. } => self.run_load(inst, module)?,
            Instr::Addx { .. } | Instr::Subx { .. } | Instr::Mulx { .. } | Instr::Divx { .. } |
              Instr::Remx { .. } | Instr::Pow { .. } | Instr::Not { .. } |
              Instr::BitNot { .. } | Instr::BitAnd { .. } | Instr::BitOr { .. } |
              Instr::BitXor { .. } | Instr::Shl { .. } | Instr::Shr { .. } |
              Instr::UShr { .. } | Instr::Neg { .. } => self.run_arith(inst, module)?,
            Instr::And { .. } | Instr::Or { .. } | Instr::Greater { .. } |
              Instr::GreaterEqual { .. } | Instr::Less { .. } | Instr::LessEqual { .. } |
              Instr::Equal { .. } | Instr::NotEqual { .. } | Instr::StrictEqual { .. } |
              Instr::StrictNotEqual { .. } => self.run_compare(inst, module)?,
            Instr::TypeOf { .. } | Instr::TypeOfEnv { .. } | Instr::InstanceOf { .. } | Instr::In { .. } => self.run_type(inst, module)?,
            Instr::DelegateOpen { .. } | Instr::DelegateClose { .. } | Instr::MakeIter { .. } |
              Instr::IterNext { .. } | Instr::IterClose { .. } |
              Instr::RequireObjectCoercible { .. } | Instr::MakeRest { .. } => self.run_iteration(inst, module)?,
            Instr::CallSpread { .. } | Instr::CallSuperSpread { .. } | Instr::NewSpread { .. } => self.run_call_spread(inst, module)?,
            Instr::ToString { .. } | Instr::ToNumber { .. } => self.run_convert(inst, module)?,
            Instr::MakeArray { .. } | Instr::ArrayPushSpread { .. } | Instr::MarkHole { .. } |
              Instr::ArrayPush { .. } => self.run_array(inst, module)?,
            Instr::MakeObject { .. } | Instr::IndexGet { .. } | Instr::PropGet { .. } |
              Instr::IndexSet { .. } | Instr::PropSet { .. } | Instr::IndexDelete { .. } |
              Instr::PropDelete { .. } | Instr::Arguments { .. } | Instr::StoreEnv { .. } |
              Instr::CallMethod { .. } => self.run_object(inst, module)?,
            Instr::Try { .. } | Instr::EndTry { .. } | Instr::ThrowExc { .. } | Instr::LoadException { .. } => self.run_exception(inst, module)?,
            Instr::CreateClosure { .. } | Instr::New { .. } => self.run_construct(inst, module)?,
            Instr::LoadThis { .. } | Instr::LoadNewTarget { .. } |
              Instr::LoadCurrentFunction { .. } | Instr::MakeFuncObj { .. } |
              Instr::MakeArrowFuncObj { .. } | Instr::MakeCell { .. } | Instr::ClosureVar { .. } | Instr::CallNative { .. } => self.run_function(inst, module)?,
            Instr::Halt { .. } | Instr::Ret { .. } | Instr::ResumeExc { .. } => self.run_halt(inst, module)?,
        };
        if !handled {
            // 两处写法漂了才会到这里：兜底成"空操作 + 前进一步"，不静默改语义。
            self.state.jump_offset(1);
        }
        Ok(())
    }

    /// `PrologueEnd`/`Yield`/`Await`
    pub(crate) fn run_suspend(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
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
                    self.generator_yield_pc = self.state.pc();
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
                self.generator_yield_pc = self.state.pc();
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `Call`/`CallEx`
    pub(crate) fn run_call(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
                    // 不再切 `rbp`：`collect_call_args` 已经改成 `rsp` 相对（第 1 步），
                    // 而 `invoke` 内部开帧由 `open_frame` 负责 —— 这条路径根本不开帧。
                    let args = self.collect_call_args(arg_count)?;
                    let result = self.invoke(
                        &Value::Function(func_id as u32),
                        Value::Undefined,
                        &args,
                        module,
                    )?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(true);
                }
                match module.entry_pc(func_id as u32) {
                    Some(location) => {
                        self.state.enter_frame()?;
                        let callee_func = self.materialize_function(func_id as u32);
                        let closure_depth = self.state.closure_var_stack.len();
                        let seh_depth = self.state.seh_stack.len();
                        // 调用点自己把**调用方**的 pc 推进一步（以前由 `Ret` 的 jump(return_pc) 做）。
                        self.state.jump_offset(1);
                                                                        self.state.note_frame(FrameBirth {
                            pc: location,
                            func: callee_func,
                            this: Value::Undefined,
                            this_state: THIS_NONE,
                            argc: arg_count,
                            construct: false,
                            new_target: Value::Undefined,
                            closure_depth,
                            seh_depth,
                        });
                        return Ok(true);
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
                // 不再"调用前切换 rbp" —— 切换是 `open_frame` 的事（只有真正开帧的路径才需要）。
                // 统一入口（P1-2c）：代理 / 原生 / 字节码（生成器 / async / 开帧）
                // 的分支只有一份；`this` 由这里给 —— 普通调用是 `undefined`，
                // 方法调用要给对象（`CallMethod` 走同一个入口，只是 `this` 不同）。
                // 调用点自己把**调用方**的 pc 推进一步（以前由 `Ret` 的 jump(return_pc) 做）。
                self.state.jump_offset(1);
                match self.enter_call(
                    &callee,
                    Value::Undefined,
                    CallArgs::OnStack(arg_count),
                    Register::Rv,
                    module,
                )? {
                    // 帧已压好：接着跑被调方（不递归 —— 普通调用没有额外开销）。
                    Control::Entered => return Ok(true),
                    // 结果已写进 `Rv`；收尾照跑（见 `CallMethod` 那条同名注释）。
                    Control::Done => return Ok(true),
                }
            }
            // 本组每一条分支都自己决定 pc（调用点自己推进），所以没有"落到组末尾"
            // 的分支 —— match 各臂全部分歧，它就是函数的尾表达式。
            _ => return Ok(false),
        }
    }

    /// `Jump`/`DelayedJump`/`BrIf`
    pub(crate) fn run_jump(&mut self, inst: &Instr, _module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
            Instr::Jump { offset } => {
                let offset = offset.as_immd();
                self.state.jump_offset(offset);
                return Ok(true);
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
                    return Ok(true);
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
                return Ok(true);
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
                return Ok(true);
            }
            // Opcode::Ret is handled directly in run()
            // 跳转组的每条分支都自己设置 pc，同样没有落到组末尾的分支。
            _ => return Ok(false),
        }
    }

    /// `Mov`/`Push`/`Pop`/`AddC`/`SubC`
    pub(crate) fn run_stack(&mut self, inst: &Instr, _module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `LoadConst`/`MakeRegExp`/`DeclareLexical`/`InitLexical`/`SetFunctionName`/`LoadEnv`
    pub(crate) fn run_load(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
                let proto = Some(Value::Object(self.builtins.regexp_prototype));
                let value = crate::builtins::regexp_construct_value(
                    &mut self.heap,
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
                //
                // A captured lexical has the same two states — its cell starts
                // uninitialized (`MakeCell`), and this is the one write that
                // fills it in.
                if let Some(cell) = self.state.env_cell(&name) {
                    *cell.borrow_mut() = Some(value);
                } else {
                    self.state.script_env.insert(name, Some(value));
                }
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `Addx`/`Subx`/`Mulx`/`Divx`/`Remx`/`Pow`/`Not`/`BitNot`/`BitAnd`/`Shl`/`Neg`
    pub(crate) fn run_arith(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `And`/`Or`/`Greater`/`GreaterEqual`/`Less`/`LessEqual`/`Equal`/`NotEqual`/`StrictEqual`/`StrictNotEqual`
    pub(crate) fn run_compare(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `TypeOf`/`TypeOfEnv`/`InstanceOf`/`In`
    pub(crate) fn run_type(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
                                        if Value::Object(proto_ref) == target_proto {
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
                            let proxy_val = Value::Object(obj_ref);
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `DelegateOpen`/`DelegateClose`/`MakeIter`/`IterNext`/`IterClose`/`RequireObjectCoercible`/`MakeRest`
    pub(crate) fn run_iteration(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
                let argc = self.current_argc();
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
                arr.set_prototype(Some(self.builtins.array_prototype));
                let value = Value::Object(self.heap.alloc(Object::Array(arr)));
                self.set_value(dst, value)?;
            }
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `CallSpread`/`CallSuperSpread`/`NewSpread`
    pub(crate) fn run_call_spread(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
                    self.current_this()
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
                let new_target = self.current_new_target();
                // ES 12.3.5.1: `super()` binds the derived constructor's `this`.
                // Cleared *before* the call runs, because the parent's own frame
                // pushes an entry onto the same stack.
                // The innermost frame with an unbound `this` is the one being
                // bound — an arrow body can run the `super()` on its enclosing
                // constructor's behalf, and only derived constructors ever set
                // the flag at all.
                if let Some(frame) = self
                    .state
                    .frames
                    .iter_mut()
                    .rev()
                    .find(|frame| frame.this_state == THIS_DERIVED_UNBOUND)
                {
                    frame.this_state = THIS_DERIVED_BOUND;
                }
                let result =
                    self.invoke_with_new_target(&callee, this, &args, Some(new_target), module)?;
                // ES 12.3.5.1 `BindThisValue`: whatever `super()` produced *is*
                // the derived `this`. A built-in parent builds its own instance
                // (`Error` sets `message`), so the pre-created object has to be
                // replaced by it; a bytecode parent hands the same one back.
                if result.is_object() {
                    // 中帧写：把本帧的 `this` 换成 `super()` 产出的对象。
                    // （上面那条 BOUND 标记打在**最内层未绑定**的那一帧上，与本行写的
                    //  "当前帧"在"箭头函数代跑 `super()`"时可能不是同一帧 —— 那是既有行为，
                    //  切片 1d 保持原样，只换数据位置。）
                    if let Some(frame) = self.state.frames.last_mut() {
                        frame.this = result.clone();
                    }
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `ToString`/`ToNumber`
    pub(crate) fn run_convert(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `MakeArray`/`ArrayPushSpread`/`MarkHole`/`ArrayPush`
    pub(crate) fn run_array(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
            // ===== Object/Array Operations =====
            Instr::MakeArray { dst } => {
                let mut arr = crate::vm::object::ArrayObject::new();
                arr.set_prototype(Some(self.builtins.array_prototype));
                let arr_val = Value::Object(self.heap.alloc(Object::Array(arr)));
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
                        let Some(arr) = obj.as_array_mut() else {
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
                    return Ok(true);
                };
                let mut borrowed = obj_ref.borrow_mut();
                if let Some(arr) = borrowed.as_array_mut()
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
                        if let Some(arr) = obj.as_array_mut() {
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `MakeObject`/`IndexGet`/`IndexSet`/`IndexDelete`/`Arguments`/`StoreEnv`/`CallMethod`
    pub(crate) fn run_object(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
            Instr::MakeObject { dst } => {
                let mut obj = crate::vm::object::OrdinaryObject::new();
                obj.set_prototype(Some(self.builtins.object_prototype));
                let obj_val = Value::Object(self.heap.alloc(Object::Ordinary(obj)));
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
                let argc = self.current_argc();
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
                let obj = Value::Object(self.heap.alloc(Object::Array(arr)));
                self.set_value(dst, obj)?;
            }
            Instr::StoreEnv { name, value } => {
                let name_index = name.as_immd();
                let name = match &module.constants[name_index as usize] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(value)?;
                // A captured local lives in its **cell**: write through it, or
                // every closure that captured the binding would keep seeing the
                // creation-time value (the `counter()` answering `1,1,1` bug).
                // The cell check comes first: a function-local cell shadows a
                // script-level binding of the same name.
                if let Some(cell) = self.state.env_cell(&name) {
                    // Still in its dead zone: only the declaration's
                    // `InitLexical` may fill an uninitialized cell in.
                    if cell.borrow().is_none() {
                        return Err(RuntimeError::ReferenceError(format!(
                            "Cannot access '{name}' before initialization"
                        )));
                    }
                    *cell.borrow_mut() = Some(value);
                } else {
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

                // 操作数已用**调用方**的帧指针读完。这里不再切换 `rbp`：
                // 切换属于 `open_frame`（只有真正开帧的路径才需要），而收尾的
                // `popc rsp` / `popc rbp` 也不再依赖 `rbp` —— 旧注释里那条
                // "the switch must happen on EVERY path" 的前提已被移除。

                // Read the outgoing arguments. These live below the *new* frame
                // pointer, so they must not go through the "missing argument"
                // rule (which is about the callee's own parameters).
                let mut raw_args = Vec::with_capacity(arg_count);
                for i in 0..arg_count {
                    let index = self.state.rsp - i - 1;
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
                                *obj_ref,
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
                    return Ok(true);
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
                    return Ok(true);
                }

                if (method_name == "call" || method_name == "apply") && obj_val.is_callable() {
                    let call_args = self.call_or_apply_args(&method_name, &args)?;
                    let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                    let result = self.invoke(&obj_val, this_arg, &call_args, module)?;
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(true);
                }

                // Array callback methods (`map`, `filter`, `reduce`, `sort`, …)
                // are executed here so they can call the user function.
                if let Some(result) =
                    self.try_array_callback_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(true);
                }

                // A function replacer for `String.prototype.replace` runs here
                // for the same reason (it calls user code).
                if let Some(result) =
                    self.try_string_callback_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(true);
                }
                // `then` / `catch` / `finally` likewise (they call the handler).
                // This is the second method-call path — a chained call like
                // `new Promise(f).catch(cb)` comes through here, not through the
                // one above, and used to silently do nothing.
                if let Some(result) = self.try_promise_method(&obj_val, &method_name, &args, module)?
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(true);
                }

                // First check: is this a static method on a native function (constructor)?
                if let Some(name) = crate::builtins::native_function_name(&obj_val) {
                    let static_method = format!("{}.{}", name, method_name);
                    // `Object.assign` needs real `[[Get]]`/`[[Set]]` (accessors).
                    if static_method == "Object.assign" {
                        let result = self.object_assign(&args, module)?;
                        self.state.set_register(Register::Rv, result)?;
                        self.state.jump_offset(1);
                        return Ok(true);
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
                        match crate::builtins::call_static_method(&static_method, &args, &mut self.heap) {
                            Ok(result) => {
                                self.state.set_register(Register::Rv, result)?;
                                self.state.jump_offset(1);
                                return Ok(true);
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
                    return Ok(true);
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
                    return Ok(true);
                }

                // Try built-in prototype method dispatch. As above: only
                // "no such method" may fall through.
                match crate::builtins::call_prototype_method(
                    &mut self.heap,
                    &obj_val,
                    &method_name,
                    &args,
                ) {
                    Ok(result) => {
                        self.state.set_register(Register::Rv, result)?;
                        self.state.jump_offset(1);
                        return Ok(true);
                    }
                    Err(err) if crate::builtins::is_unknown_builtin(&err) => {}
                    Err(err) => return Err(err),
                }

                // Look up user-defined method on the object's prototype chain
                let method_val = self.lookup_property_on_object(&obj_val, &method_name, module)?;

                // 方法没找到：`obj.missing()` 不抛，`Rv = undefined`（保持原样；
                // 原来这条是靠落到分派循环尾部的 `jump_offset(1)` 推进 pc）。
                if matches!(method_val, Value::Undefined) {
                    self.state.set_register(Register::Rv, Value::Undefined)?;
                    self.state.jump_offset(1);
                    return Ok(true);
                }
                // 其余全部交给统一入口（P1-2c）：原生原型方法 / 生成器方法 /
                // async 方法 / 两种 callee 形状的开帧。`this` 是接收者；参数是
                // "栈上原地读 + 已备好一份装箱副本" —— 装箱必须发生在进入口之前
                // （内建层按 `Value::Object` 匹配），见 §3.3.5 的方案 A。
                //
                // 顺带**修掉一个既有 bug**：原来这条 arm 的生成器分支传 `Vec::new()`，
                // 把闭包变量丢了 ——
                // `function outer() { var x = 42; return { *g() { yield x; } }; }`
                // 的 `outer().g().next().value` 会返回 `[object Function]`（node 是 42）。
                // 统一入口按 `CallEx` 的原样传真 `captured_vars`。
                //
                // 唯一的错误信息变化：不是可调用物时，原文说
                // `"{method_name} is not a function"`，现在统一入口说 `"not a function"`
                // —— 错误类型不变，只是少了方法名。
                // 调用点自己把**调用方**的 pc 推进一步（以前由 `Ret` 的 jump(return_pc) 做）。
                self.state.jump_offset(1);
                match self.enter_call(
                    &method_val,
                    obj_val.clone(),
                    CallArgs::OnStackWithCopy {
                        argc: arg_count,
                        args: &args,
                    },
                    Register::Rv,
                    module,
                )? {
                    // 帧已压好：接着跑被调方（不递归）。
                    Control::Entered => return Ok(true),
                    // 结果已写进 `Rv`，**收尾照跑**，不再"跳过下一条指令"。
                    //
                    // 以前这里要多推进一步，因为被跳过的那条是 `movc rsp, rbp`：
                    // 在 `Done` 路径上 `invoke` 已经把 `rbp` 恢复成**调用方**的基址，
                    // 拿它复位 `rsp` 会把调用方的栈打回基址 —— 所以只能跳过。
                    // 收尾改成 `popc rsp`（复位到 `pushc rsp` 显式保存的"实参之后"）
                    // 之后，`rsp` 的复位不再依赖 `rbp` ⇒ `Entered` 与 `Done` 两条
                    // 路径的收尾**第一次完全一致**，这条隐式约定就此消失。
                    Control::Done => return Ok(true),
                }
            }
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `Try`/`EndTry`/`ThrowExc`/`LoadException`
    pub(crate) fn run_exception(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
            // ===== Exception Handling =====
            Instr::Try { .. } => {
                // **以 EH 表为准**（M1.1 之后它才可信：区域由编译期声明、配对是结构化的）。
                // 之前退回用指令偏移的两条理由都已消失：
                // 1. "表不可靠" —— `EhRegion` 不再从指令流派生，§10.3 的根因已修；
                // 2. 查表开销 —— `eh_index` 是一次 O(1) 哈希，而这一步同时省掉了两次偏移加法。
                //
                // 处理器地址是**运行时真正要用的那部分**，所以它必须来自唯一的权威来源；
                // 指令操作数保留下来只作互校（见下），不再参与决定控制流。
                let region = module.eh_region_starting_at(self.state.pc()).ok_or_else(|| {
                    RuntimeError::InternalError(format!(
                        "Try(pc={}) 没有对应的 EH 区域声明 —— 编译期漏声明",
                        self.state.pc()
                    ))
                })?;
                let (catch_pc, finally_pc) = (region.catch.unwrap_or(0), region.finally.unwrap_or(0));
                // 互校：指令操作数由 `codegen` 的 `PatchFn` 回填，表由 `resolve_eh_regions`
                // 用 `block_map` 解析 —— **两条独立的计算路径**，分叉了就说明其中一条错了。
                // 只在 debug 下读操作数，所以这里重新取一次指令（避免 release 下绑定未使用）。
                #[cfg(debug_assertions)]
                if let Instr::Try {
                    catch_offset,
                    finally_offset,
                } = module.instructions[self.state.pc()]
                {
                    let abs = |off: isize| -> usize {
                        if off == 0 {
                            0
                        } else {
                            (self.state.pc() as isize + off) as usize
                        }
                    };
                    debug_assert_eq!(
                        (abs(catch_offset.as_immd()), abs(finally_offset.as_immd())),
                        (catch_pc, finally_pc),
                        "Try(pc={})：指令操作数与 EH 表不一致 —— 两条回填路径分叉了",
                        self.state.pc()
                    );
                }
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
                    saved_frame_depth: self.state.frames.len(),
                };
                self.state.seh_stack.push(record);
            }
            Instr::EndTry {  } => {
                // 这条断言**现在成立**了，而且它曾经是 §10.3 的证物：
                // 当时它在这里响过 —— `EndTry(pc=73)` 关掉的不是派生表里的任何区域，
                // 因为那时"配对"靠扫指令流 + 深度计数，而 `EndTry` 与 `Try` 根本不一配对。
                //
                // M1.1 之后配对是结构化的（`PopSeh { region }` → 区域表的 `exits`），
                // 所以每条 `EndTry` 的 pc 必然登记在某个区域上。留着它当守卫：
                // 它一响就说明发射端与表之间又出现了新的分叉。
                debug_assert!(
                    module.eh_region_ending_at(self.state.pc()).is_some(),
                    "EndTry(pc={}) 不是任何区域的退出点 —— 发射端与 EH 表分叉了",
                    self.state.pc()
                );
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
                return self.handle_throw(exc_val).map(|_| true);
            }
            Instr::LoadException { dst } => {
                let exc_val = self.state.get_register(Register::Rv)?;
                self.set_value(dst, exc_val)?;
            }
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `CreateClosure`/`New`
    pub(crate) fn run_construct(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
            // ===== Function/Closure =====
            Instr::CreateClosure { dst, func } => {
                let func_id = func.as_immd();
                self.set_value(dst, Value::Function(func_id as u32))?;
            }
            Instr::New { callee, argc } => {
                let constructor_val = self.get_value(callee)?;
                let arg_count = argc.as_immd() as usize;
                // **这里不再切 `rbp`**（§22 记录的那处"中途换参照系"就此消失）：
                //
                // 以前切换放在最前面，于是下面 `set_value(callee, func_obj)` 那次写
                // 落在**被调方**的参照系里 —— 它写的是调用方自己的操作数槽（`[rbp+3]`），
                // 却写到了被调方窗口的同一个偏移上（一个尚未分配、被 `ensure` 硬撑出来的槽）。
                // 那是"读在切换前、写在切换后"的时序巧合，从来不是契约。
                //
                // 现在切换只在真正开帧之前做（见下方 `enter_frame`），`New` 走原生 /
                // 代理分支时**完全不切** —— 那些路径不开帧，切换出来的基址不属于任何帧。

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
                            self.code(id)
                                .map(|block| block.name.as_str())
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
                            let ctor = Value::Object(obj_ref);
                            let args = self.collect_call_args(arg_count)?;
                            let rv = self.construct(&ctor, &args, module)?;
                            self.state.set_register(Register::Rv, rv)?;
                            self.state.jump_offset(1);
                            return Ok(true);
                        }
                        if borrowed.kind() == ObjectKind::Function {
                            if let Some(func_obj) =
                                borrowed.as_function()
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
                            let name = borrowed.as_native_function()
                                .map(|f| f.name.clone())
                                .unwrap_or_default();
                            drop(borrowed);

                            let ctor = Value::Object(obj_ref);
                            let args = self.collect_call_args(arg_count)?;
                            let rv = self.native_construct(&name, &ctor, &args, None, module)?;
                            self.state.set_register(Register::Rv, rv)?;
                            self.state.jump_offset(1);
                            return Ok(true);
                        } else {
                            return Err(RuntimeError::TypeError("not a constructor".to_string()));
                        }
                    }
                    _ => {
                        return Err(RuntimeError::TypeError("not a constructor".to_string()));
                    }
                };

                // 2. Create new ordinary object
                let new_obj_val = Value::Object(self.heap.alloc(Object::Ordinary(
                    crate::vm::object::OrdinaryObject::new(),
                )));

                // 3. Set prototype
                if let Some(Value::Object(proto_obj)) = prototype {
                    if let Value::Object(obj_ref) = &new_obj_val {
                        obj_ref
                            .borrow_mut()
                            .set_prototype(Some(proto_obj));
                    }
                } else {
                    // Default prototype: Object.prototype
                    if let Value::Object(obj_ref) = &new_obj_val {
                        obj_ref
                            .borrow_mut()
                            .set_prototype(Some(self.builtins.object_prototype));
                    }
                }

                // The running function inside the constructor is the constructor.
                let callee_func = new_target_value.clone();

                // 5. Save closure depth, SEH depth and return PC, then jump to constructor
                match module.entry_pc(func_id) {
                    Some(location) => {
                        // 帧的窗口基址：**开帧**是唯一的写入时机（与 `open_frame` 一致）。
                        // 只有走到这里才真的要开帧 —— 原生构造器 / 代理分支在上面就返回了。
                        self.state.rbp = self.state.rsp;
                        self.state.enter_frame()?;
                        // 派生构造器的 `this` 出生即"未绑定"（ES 9.2.2 / 12.3.5.1）。
                        // 以前这是 `enter_frame` 之后一次单独的 `mark_this_uninitialized`；
                        // 现在它是帧的**出生参数** —— 不必先建一个半成品再去改它。
                        let initial_this_state = if module.is_derived_ctor(func_id) {
                            THIS_DERIVED_UNBOUND
                        } else {
                            THIS_NONE
                        };
                        // 下面 `jump(location)` 用的是 `CodeBlock::start`（= 原来的 `symtab` 入口 pc）
                        // Reset Rv before invoking the constructor so that a constructor
                        // with no explicit `return` (Rv stays undefined) yields `this`
                        // via the [[Construct]] logic in `Ret`. This also avoids leaking a
                        // stale Rv value from a previous call.
                        self.state.set_register(Register::Rv, Value::Undefined)?;
                        let closure_depth = self.state.closure_var_stack.len();
                        let seh_depth = self.state.seh_stack.len();
                        // 调用点自己把**调用方**的 pc 推进一步（以前由 `Ret` 的 jump(return_pc) 做）。
                        self.state.jump_offset(1);
                                                                        self.state.note_frame(FrameBirth {
                            pc: location,
                            func: callee_func,
                            this: new_obj_val,
                            this_state: initial_this_state,
                            argc: arg_count,
                            construct: true,
                            new_target: new_target_value.clone(),
                            closure_depth,
                            seh_depth,
                        });
                        return Ok(true);
                    }
                    None => {
                        return Err(RuntimeError::ReferenceError(format!(
                            "undefined function: {func_id}"
                        )));
                    }
                }
            }
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `LoadThis`/`LoadNewTarget`/`LoadCurrentFunction`/`MakeFuncObj`/`MakeArrowFuncObj`/`ClosureVar`/`CallNative`
    pub(crate) fn run_function(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
            Instr::LoadThis { dst } => {
                if self.state.frames.last().map(|f| f.this_state) == Some(THIS_DERIVED_UNBOUND) {
                    return Err(self.this_not_initialized());
                }
                let value = self.current_this();
                self.set_value(dst, value)?;
            }
            Instr::LoadNewTarget { dst } => {
                // `new.target` is per frame: the constructor for a `[[Construct]]`
                // frame, otherwise undefined (an arrow frame carries the value
                // captured when the arrow object was created).
                let value = self.current_new_target();
                self.set_value(dst, value)?;
            }
            Instr::LoadCurrentFunction { dst } => {
                let value = self.current_func();
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
                    v if v.is_undefined() => self.current_this(),
                    v => v,
                };
                let captured_vars = self.take_pending_captured_vars();
                // An arrow has no `[[Construct]]`: it inherits `new.target` from
                // the frame that created it, so capture that value here (the
                // same create-time snapshot rule used for `this`).
                let captured_new_target = self.current_new_target();
                // `fn.length` / `fn.name` come from the compiler's per-function
                // metadata, exactly as for ordinary functions.
                let (arrow_name, arrow_arity) = self
                    .code(func_id)
                    .map(|block| (block.name.clone(), block.arity))
                    .unwrap_or_default();
                let obj_val = crate::vm::object::new_arrow_function_object(
                    &mut self.heap,
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
                    match &self.current_func() {
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
            Instr::MakeCell { name } => {
                let name_index = name.as_immd() as usize;
                let name = match &module.constants[name_index] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                // A captured function-local gets its cell **at frame entry**:
                // the prologue emits one `MakeCell` per name a nested function
                // captures, so the cell exists before any closure can look it
                // up — including a closure created by a hoisted declaration,
                // which runs before the rest of the body.
                //
                // `None` = not yet initialized: a captured `let` has a *dynamic*
                // dead zone (a closure can read it before its declaration has
                // run), so the cell has to carry that state instead of starting
                // out `undefined`. `var`s and parameters are initialized to
                // `undefined` by the hoisting / parameter prologue.
                let cell = crate::vm::new_env_cell(None);
                self.state.push_env_cell(name, cell);
            }
            Instr::ClosureVar {
                name,
                value,
                fallback,
            } => {
                let name_index = name.as_immd() as usize;
                let name = match &module.constants[name_index] {
                    Constant::String(s) => s.as_str().to_string(),
                };
                let value = self.get_value(value)?;
                let copy_fallback = fallback.as_immd() != 0;
                // Capture the **cell**, not the value. The cell is looked up by
                // name in the current frame's environment, so a name captured
                // from an enclosing frame resolves to that very cell (the chain
                // works at any depth) and a name that is this frame's own
                // captured local resolves to its prologue cell.
                if let Some(cell) = self.state.env_cell(&name) {
                    self.state.pending_captured.push((name, cell));
                } else if copy_fallback {
                    // No cell: a block-scoped local (a `for (let …)` head, a
                    // block's `let`). Those keep the copy semantics on purpose —
                    // a per-iteration binding is *supposed* to hand each
                    // iteration's closure its own value.
                    let cell = crate::vm::new_env_cell(Some(value));
                    self.state.pending_captured.push((name, cell));
                }
                // Otherwise the name is neither a local here nor a cell in
                // scope: it resolves through the global environment at read
                // time, and capturing a snapshot would shadow the global with a
                // frozen `undefined`.
            }
            Instr::CallNative { callee, argc } => {
                let callable = self.get_value(callee)?;
                let arg_count = argc.as_immd() as usize;
                // 不再切 `rbp`：实参是 `rsp` 相对读的，而这条路径**永远不开帧**
                // （`CallNative` 只分派内建）—— 切换只会造出一个不属于任何帧的基址。

                if let Some(name) = crate::builtins::native_function_name(&callable) {
                    // Arguments are on the stack below the frame: arg0 at [rbp-1], arg1 at [rbp-2], etc.
                    let mut args = Vec::with_capacity(arg_count);
                    for i in 0..arg_count {
                        let index = self.state.rsp - i - 1;
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
                        crate::builtins::call_native(&name, &args, &mut self.heap)
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
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }

    /// `Halt`/`ResumeExc`
    pub(crate) fn run_halt(&mut self, inst: &Instr, module: &Module) -> Result<bool, RuntimeError> {
        match *inst {
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
                            return Ok(true);
                        } else {
                            self.state.seh_stack.pop();
                            return self.handle_throw(exc).map(|_| true);
                        }
                    } else if let Some(target_pc) = delayed_jump {
                        // Delayed jump after finally execution. `target_pc` is an
                        // absolute address (see Codegen for DelayedJump).
                        self.state.seh_stack.pop();
                        self.state.jump(target_pc.max(0) as usize);
                        return Ok(true);
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
                            .and_then(|id| module.func_exit_pc(id))
                        {
                            self.state.jump(pc);
                        }
                        return Ok(true);
                    }
                } else if let Some(record) = self.state.seh_stack.last_mut() {
                    if record.in_finally {
                        record.in_finally = false;
                    }
                }
            }
            _ => return Ok(false),
        }
        self.state.jump_offset(1);
        Ok(true)
    }
}
