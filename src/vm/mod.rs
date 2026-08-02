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
use crate::bytecode::{
    Bytecode, Constant, FunctionId, Module, Opcode, Operand, Primitive, Register,
};

const STACK_MAX: usize = 0x0FFF;

/// JavaScript Virtual Machine
///
/// Executes bytecode modules produced by the Compiler.
/// Ported from evalit's register-based VM with JS-specific Value semantics.
pub struct VM {
    state: State,
    builtins: Builtins,
}

impl VM {
    pub fn new() -> Self {
        let mut state = State::new();
        let builtins = Builtins::new();
        builtins.register(&mut state.globals);
        Self { state, builtins }
    }

    /// Execute a bytecode module and return the result
    pub fn run(&mut self, module: &Module) -> Result<Value, RuntimeError> {
        self.state = State::new();
        // Re-register builtins into the fresh state
        self.builtins.register(&mut self.state.globals);

        while self.step(module)? {}

        // If we run out of instructions, return Rv
        Ok(self
            .state
            .get_register(Register::Rv)
            .unwrap_or(Value::Undefined))
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
                    let return_pc = self.state.popc()?;
                    let saved_seh_depth = self.state.popc()?;
                    let saved_closure_depth = self.state.popc()?;
                    // If this frame was invoked via `new`, apply [[Construct]] return
                    // semantics: a returned object becomes the result, otherwise the
                    // newly created `this` object is used.
                    if let Some(true) = self.state.construct_stack.pop() {
                        let rv = self.state.get_register(Register::Rv)?;
                        if !rv.is_object() {
                            self.state
                                .set_register(Register::Rv, self.state.this_val.clone())?;
                        }
                    }
                    self.state.closure_var_stack.truncate(saved_closure_depth);
                    self.state.seh_stack.truncate(saved_seh_depth);
                    self.state.jump(return_pc);
                }
            }
            _ => {
                self.run_instruction(&inst, module)?;
            }
        }

        Ok(true)
    }

    /// ES `OrdinaryToPrimitive` (https://tc39.es/ecma262/#sec-ordinarytoprimitive).
    ///
    /// Returns the primitive value of `val`. Objects are converted by calling
    /// `valueOf()` then `toString()` (or `Symbol.toPrimitive` when present) and
    /// returning the first result that is a primitive. This replaces the previous
    /// fallback that simply stringified objects.
    pub fn to_primitive(
        &mut self,
        val: &Value,
        hint: &str,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !val.is_object() {
            return Ok(val.clone());
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
            let method = self.lookup_property_on_object(val, name);
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

    /// Re-entrantly invoke a callable `callee` with `this` bound and no arguments,
    /// returning its result. This drives a nested execution loop so that user
    /// defined `valueOf` / `toString` methods can be honoured during coercion.
    fn call_reentrant(
        &mut self,
        callee: &Value,
        this: Value,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let saved_pc = self.state.pc;
        let saved_rsp = self.state.rsp;
        let saved_rbp = self.state.rbp;
        let saved_closure = self.state.closure_var_stack.len();
        let saved_seh = self.state.seh_stack.len();

        // Sentinel return address that is one past the last instruction; when the
        // callee returns, `Ret` sets `pc` to it and the nested loop stops.
        let return_pc = module.instructions.len();

        // Set up the call frame the same way the `Call`/`CallEx` opcodes do.
        self.state.this_val = this.clone();
        match callee {
            Value::Function(id) => match module.symtab.get(&FunctionId::new(*id)) {
                Some(location) => {
                    self.state.pushc(self.state.closure_var_stack.len())?;
                    self.state.pushc(self.state.seh_stack.len())?;
                    self.state.pushc(return_pc)?;
                    self.state.construct_stack.push(false);
                    self.state.jump(*location);
                }
                None => {
                    return Err(RuntimeError::ReferenceError(format!(
                        "undefined function: {id}"
                    )));
                }
            },
            Value::Object(obj_ref) => {
                let (id, captured_this, captured_vars) = {
                    let borrowed = obj_ref.borrow();
                    if borrowed.kind() == crate::vm::property::ObjectKind::Function {
                        if let Some(func_obj) = borrowed
                            .as_any()
                            .downcast_ref::<crate::vm::object::FunctionObject>()
                        {
                            (
                                func_obj.func_id,
                                func_obj.captured_this.clone(),
                                func_obj.captured_vars.clone(),
                            )
                        } else {
                            return Err(RuntimeError::TypeError("not a function".to_string()));
                        }
                    } else {
                        return Err(RuntimeError::TypeError("not a function".to_string()));
                    }
                };
                self.state.this_val = captured_this.unwrap_or(this);
                for (name, value) in &captured_vars {
                    let mut map = std::collections::HashMap::new();
                    map.insert(name.clone(), value.clone());
                    self.state.closure_var_stack.push(map);
                }
                match module.symtab.get(&FunctionId::new(id)) {
                    Some(location) => {
                        self.state.pushc(self.state.closure_var_stack.len())?;
                        self.state.pushc(self.state.seh_stack.len())?;
                        self.state.pushc(return_pc)?;
                        self.state.construct_stack.push(false);
                        self.state.jump(*location);
                    }
                    None => {
                        return Err(RuntimeError::ReferenceError(format!(
                            "undefined function: {id}"
                        )));
                    }
                }
            }
            _ => return Err(RuntimeError::TypeError("not a function".to_string())),
        }

        while self.step(module)? {}

        let rv = self.state.get_register(Register::Rv)?;
        // Restore the caller's execution context.
        self.state.pc = saved_pc;
        self.state.rsp = saved_rsp;
        self.state.rbp = saved_rbp;
        self.state.closure_var_stack.truncate(saved_closure);
        self.state.seh_stack.truncate(saved_seh);
        Ok(rv)
    }

    fn run_instruction(&mut self, inst: &Bytecode, module: &Module) -> Result<(), RuntimeError> {
        let Bytecode { opcode, operands } = inst;

        match opcode {
            // ===== Control Flow =====
            Opcode::Call => {
                let func_id = operands[0].as_immd();
                match module.symtab.get(&FunctionId::new(func_id as u32)) {
                    Some(location) => {
                        // Strict mode: this = undefined for regular function calls
                        self.state.this_val = Value::Undefined;
                        self.state.pushc(self.state.closure_var_stack.len())?;
                        self.state.pushc(self.state.seh_stack.len())?;
                        self.state.pushc(self.state.pc + 1)?;
                        self.state.construct_stack.push(false);
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
                // Dynamic call: operand may be Symbol or Register/Stack holding a function value
                match operands[0] {
                    Operand::Symbol(sym) => match module.symtab.get(&FunctionId::new(sym)) {
                        Some(location) => {
                            // Strict mode: this = undefined
                            self.state.this_val = Value::Undefined;
                            self.state.pushc(self.state.closure_var_stack.len())?;
                            self.state.pushc(self.state.seh_stack.len())?;
                            self.state.pushc(self.state.pc + 1)?;
                            self.state.jump(*location);
                            return Ok(());
                        }
                        None => {
                            return Err(RuntimeError::ReferenceError(format!(
                                "undefined function: sym_{sym}"
                            )));
                        }
                    },
                    Operand::Register(_) | Operand::Stack(_) => {
                        let value = self.get_value(operands[0])?;
                        match value {
                            Value::Function(id) => {
                                // Strict mode: this = undefined for regular calls
                                self.state.this_val = Value::Undefined;
                                match module.symtab.get(&FunctionId::new(id)) {
                                    Some(location) => {
                                        self.state.pushc(self.state.closure_var_stack.len())?;
                                        self.state.pushc(self.state.seh_stack.len())?;
                                        self.state.pushc(self.state.pc + 1)?;
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
                                // Check if this is a FunctionObject (from class lowering)
                                let borrowed = obj_ref.borrow();
                                if borrowed.kind() == ObjectKind::Function {
                                    if let Some(func_obj) = borrowed
                                        .as_any()
                                        .downcast_ref::<crate::vm::object::FunctionObject>(
                                    ) {
                                        let id = func_obj.func_id;
                                        // Check if this is an arrow function with captured this
                                        let captured_this = func_obj.captured_this.clone();
                                        let captured_vars = func_obj.captured_vars.clone();
                                        drop(borrowed);
                                        // For arrow functions, use captured this; otherwise undefined
                                        self.state.this_val =
                                            captured_this.unwrap_or(Value::Undefined);
                                        // Push captured variables onto closure_var_stack
                                        for (name, value) in &captured_vars {
                                            let mut map = std::collections::HashMap::new();
                                            map.insert(name.clone(), value.clone());
                                            self.state.closure_var_stack.push(map);
                                        }
                                        match module.symtab.get(&FunctionId::new(id)) {
                                            Some(location) => {
                                                self.state
                                                    .pushc(self.state.closure_var_stack.len())?;
                                                self.state.pushc(self.state.seh_stack.len())?;
                                                self.state.pushc(self.state.pc + 1)?;
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
                                        return Err(RuntimeError::TypeError(
                                            "not a constructor".to_string(),
                                        ));
                                    }
                                } else {
                                    return Err(RuntimeError::TypeError(
                                        "not a function".to_string(),
                                    ));
                                }
                            }
                            _ => {
                                return Err(RuntimeError::TypeError("not a function".to_string()));
                            }
                        }
                    }
                    _ => {
                        return Err(RuntimeError::TypeError("invalid call target".to_string()));
                    }
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

            // ===== Unary =====
            Opcode::Not => {
                let value = self.get_value(operands[1])?;
                let result = Value::Bool(!value.to_boolean());
                self.set_value(operands[0], result)?;
            }
            Opcode::BitNot => {
                // Bitwise NOT: convert to 32-bit signed integer, flip bits
                let value = self.get_value(operands[1])?;
                let num = value.to_number() as i32;
                let result = Value::Number((!num) as f64);
                self.set_value(operands[0], result)?;
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
            Opcode::InstanceOf => {
                let obj = self.get_value(operands[1])?;
                let ctor = self.get_value(operands[2])?;

                let result = match (&obj, &ctor) {
                    // Get the constructor's prototype property
                    (_, Value::Object(ctor_ref)) => {
                        let ctor_proto = ctor_ref
                            .borrow()
                            .property_get(&PropertyKey::from("prototype"));
                        if let Some(desc) = ctor_proto {
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
                        } else {
                            false
                        }
                    }
                    _ => false,
                };

                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::In => {
                // TODO: implement when objects exist
                self.set_value(operands[0], Value::Bool(false))?;
            }

            // ===== Iteration =====
            Opcode::MakeIter => {
                // TODO: implement proper iteration
                let src = self.get_value(operands[1])?;
                self.set_value(operands[0], src)?;
            }
            Opcode::IterNext => {
                // TODO: implement proper iteration
                // For now, just signal end
                self.set_value(operands[0], Value::Undefined)?;
                self.set_value(operands[1], Value::Bool(false))?;
            }

            // ===== Object/Array Operations =====
            Opcode::MakeArray => {
                let mut arr = crate::vm::object::ArrayObject::new();
                arr.set_prototype(Some(Rc::clone(&self.builtins.array_prototype)));
                let arr_val = Value::Object(Rc::new(RefCell::new(arr)));
                self.set_value(operands[0], arr_val)?;
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
            Opcode::IndexGet => {
                let obj = self.get_value(operands[1])?;
                let key = self.resolve_property_key(operands[2], module)?;
                let value = match obj {
                    Value::Object(obj_ref) => {
                        crate::vm::prototype::internal_get(obj_ref, &key, None)
                            .map_err(|e| RuntimeError::TypeError(e))?
                    }
                    _ => Value::Undefined,
                };
                self.set_value(operands[0], value)?;
            }
            Opcode::IndexSet => {
                let obj = self.get_value(operands[0])?;
                let key = self.resolve_property_key(operands[1], module)?;
                let val = self.get_value(operands[2])?;
                match obj {
                    Value::Object(obj_ref) => {
                        crate::vm::prototype::internal_set(obj_ref, key, val)
                            .map_err(|e| RuntimeError::TypeError(e))?;
                    }
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "IndexSet on non-object".to_string(),
                        ));
                    }
                }
            }
            Opcode::PropGet => {
                let obj = self.get_value(operands[1])?;
                let key = self.resolve_property_key(operands[2], module)?;
                let value = match obj {
                    Value::Object(obj_ref) => {
                        crate::vm::prototype::internal_get(obj_ref, &key, None)
                            .map_err(|e| RuntimeError::TypeError(e))?
                    }
                    Value::Function(func_id) => {
                        // Convert Value::Function to FunctionObject and update storage
                        let func_obj = new_function_object(func_id, "<function>");
                        self.set_value(operands[1], func_obj.clone())?;

                        // Now get the property from the FunctionObject
                        if let Value::Object(func_ref) = &func_obj {
                            crate::vm::prototype::internal_get(func_ref.clone(), &key, None)
                                .map_err(|e| RuntimeError::TypeError(e))?
                        } else {
                            Value::Undefined
                        }
                    }
                    _ => Value::Undefined,
                };
                self.set_value(operands[0], value)?;
            }
            Opcode::PropSet => {
                let obj = self.get_value(operands[0])?;
                let key = self.resolve_property_key(operands[1], module)?;
                let val = self.get_value(operands[2])?;
                match obj {
                    Value::Object(obj_ref) => {
                        crate::vm::prototype::internal_set(obj_ref, key, val)
                            .map_err(|e| RuntimeError::TypeError(e))?;
                    }
                    Value::Function(func_id) => {
                        // Convert Value::Function to FunctionObject so properties can be set
                        let func_obj = new_function_object(func_id, "<function>");
                        if let Value::Object(obj_ref) = &func_obj {
                            crate::vm::prototype::internal_set(Rc::clone(obj_ref), key, val)
                                .map_err(|e| RuntimeError::TypeError(e))?;
                            // Update the original location to point to the new FunctionObject
                            self.set_value(operands[0], func_obj)?;
                        }
                    }
                    _ => {
                        return Err(RuntimeError::TypeError("PropSet on non-object".to_string()));
                    }
                }
            }
            Opcode::CallMethod => {
                let obj_val = self.get_value(operands[0])?;
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

                // Read arguments from the stack
                let mut args = Vec::with_capacity(arg_count);
                for i in 0..arg_count {
                    let offset = -(i as isize + 1);
                    if let Ok(arg) = self.state.get_value_from_stack(offset) {
                        args.push(arg);
                    }
                }

                // First check: is this a static method on a native function (constructor)?
                if let Some(name) = crate::builtins::native_function_name(&obj_val) {
                    let static_method = format!("{}.{}", name, method_name);
                    if let Ok(result) = crate::builtins::call_static_method(&static_method, &args) {
                        self.state.set_register(Register::Rv, result)?;
                        self.state.jump_offset(1);
                        return Ok(());
                    }
                }

                // Try built-in prototype method dispatch
                if let Ok(result) =
                    crate::builtins::call_prototype_method(&obj_val, &method_name, &args)
                {
                    self.state.set_register(Register::Rv, result)?;
                    self.state.jump_offset(1);
                    return Ok(());
                }

                // Look up user-defined method on the object's prototype chain
                let method_val = self.lookup_property_on_object(&obj_val, &method_name);

                match method_val {
                    Value::Function(id) => {
                        // User-defined bytecode function - call it with this = obj_val
                        self.state.this_val = obj_val;
                        match module.symtab.get(&FunctionId::new(id)) {
                            Some(location) => {
                                self.state.pushc(self.state.closure_var_stack.len())?;
                                self.state.pushc(self.state.seh_stack.len())?;
                                self.state.pushc(self.state.pc + 1)?;
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
                                let captured_vars = func_obj.captured_vars.clone();
                                drop(borrowed);
                                // For arrow functions, use captured this; for regular methods, use obj_val
                                self.state.this_val = captured_this.unwrap_or(obj_val);
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

                // 1. Determine the function ID and prototype
                let (func_id, prototype) = match constructor_val {
                    Value::Function(id) => {
                        // Value::Function should have been converted to FunctionObject by PropGet
                        // If we get here, it means the constructor was called directly without accessing its properties
                        // Create a FunctionObject and use its prototype
                        let func_obj = new_function_object(id, "<constructor>");
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
                            // Handle native constructors (like Object, Array, Error, etc.)
                            let native_fn = borrowed
                                .as_any()
                                .downcast_ref::<NativeFunctionObject>()
                                .map(|f| (f.name.clone(), f.get_prototype()))
                                .unwrap_or_default();
                            let name = native_fn.0;
                            let proto = native_fn.1;
                            drop(borrowed);

                            // Get arguments from the stack (args were pushed in reverse order)
                            let mut args = Vec::with_capacity(arg_count);
                            for i in 0..arg_count {
                                let offset = -(i as isize + 1);
                                let arg = self.state.get_value_from_stack(offset)?;
                                args.push(arg);
                            }
                            // Reverse to get original order
                            args.reverse();

                            // Create a new object with the correct prototype
                            let mut new_obj = crate::vm::object::OrdinaryObject::new();
                            if let Some(ref p) = proto {
                                new_obj.set_prototype(Some(Rc::clone(p)));
                            } else {
                                new_obj.set_prototype(Some(Rc::clone(
                                    &self.builtins.object_prototype,
                                )));
                            }
                            let new_obj_val = Value::Object(Rc::new(RefCell::new(new_obj)));
                            self.state.this_val = new_obj_val.clone();

                            // Call native constructor
                            match crate::builtins::call_native(&name, &args) {
                                Ok(result) => {
                                    // For Error constructors, merge the result properties into our object
                                    if name.ends_with("Error") {
                                        if let Value::Object(result_ref) = &result {
                                            let result_borrowed = result_ref.borrow();
                                            // Copy name and message from the result
                                            if let Some(name_prop) = result_borrowed
                                                .property_get(&PropertyKey::from_str("name"))
                                            {
                                                if let Value::Object(target_ref) = &new_obj_val {
                                                    target_ref
                                                        .borrow_mut()
                                                        .property_set(
                                                            PropertyKey::from_str("name"),
                                                            name_prop.value,
                                                        )
                                                        .ok();
                                                }
                                            }
                                            if let Some(msg_prop) = result_borrowed
                                                .property_get(&PropertyKey::from_str("message"))
                                            {
                                                if let Value::Object(target_ref) = &new_obj_val {
                                                    target_ref
                                                        .borrow_mut()
                                                        .property_set(
                                                            PropertyKey::from_str("message"),
                                                            msg_prop.value,
                                                        )
                                                        .ok();
                                                }
                                            }
                                        }
                                        self.state.set_register(Register::Rv, new_obj_val)?;
                                    } else {
                                        self.state.set_register(Register::Rv, result)?;
                                    }
                                }
                                Err(e) => return Err(e),
                            }
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

                // 5. Save closure depth, SEH depth and return PC, then jump to constructor
                match module.symtab.get(&FunctionId::new(func_id)) {
                    Some(location) => {
                        // Reset Rv before invoking the constructor so that a constructor
                        // with no explicit `return` (Rv stays undefined) yields `this`
                        // via the [[Construct]] logic in `Ret`. This also avoids leaking a
                        // stale Rv value from a previous call.
                        self.state.set_register(Register::Rv, Value::Undefined)?;
                        self.state.pushc(self.state.closure_var_stack.len())?;
                        self.state.pushc(self.state.seh_stack.len())?;
                        self.state.pushc(self.state.pc + 1)?;
                        self.state.construct_stack.push(true);
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
                let value = self.state.this_val.clone();
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
                let obj_val = crate::vm::object::new_function_object(func_id, "<class>");
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
                let captured_this = self.get_value(operands[2])?;
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
                let obj_val = crate::vm::object::new_arrow_function_object(
                    func_id,
                    "<arrow>",
                    captured_this,
                    captured_vars,
                );
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

                if let Some(name) = crate::builtins::native_function_name(&callable) {
                    // Arguments are on the stack below the frame: arg0 at [rbp-1], arg1 at [rbp-2], etc.
                    let mut args = Vec::with_capacity(arg_count);
                    for i in 0..arg_count {
                        let offset = -(i as isize + 1);
                        let arg = self.state.get_value_from_stack(offset)?;
                        args.push(arg);
                    }

                    match crate::builtins::call_native(&name, &args) {
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
                    } else if let Some(target_offset) = delayed_jump {
                        // Delayed jump after finally execution
                        self.state.seh_stack.pop();
                        self.state.jump_offset(target_offset);
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

    /// Resolve a property key from an operand.
    ///
    /// If the operand is an immediate, it's a constant pool index (static property name).
    /// Otherwise, it's a runtime value (dynamic property name).
    fn resolve_property_key(
        &self,
        operand: Operand,
        module: &Module,
    ) -> Result<PropertyKey, RuntimeError> {
        match operand {
            Operand::Immd(id) => match &module.constants[id as usize] {
                Constant::String(s) => Ok(PropertyKey::from_str(s.as_str())),
            },
            _ => {
                let prop_val = self.get_value(operand)?;
                Ok(crate::vm::prototype::value_to_property_key(&prop_val))
            }
        }
    }

    fn handle_throw(&mut self, exc_val: Value) -> Result<(), RuntimeError> {
        match self.state.seh_stack.pop() {
            Some(mut record) => {
                self.state.rsp = record.saved_rsp;
                self.state.rbp = record.saved_rbp;

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
    rsp: usize,
    rbp: usize,
    pc: usize,
}

impl State {
    fn new() -> Self {
        Self {
            data_stack: vec![Value::Undefined; STACK_MAX],
            ctrl_stack: Vec::with_capacity(256),
            registers: std::array::from_fn(|_| Value::Undefined),
            seh_stack: Vec::new(),
            globals: HashMap::new(),
            this_val: Value::Undefined,
            closure_var_stack: Vec::new(),
            construct_stack: Vec::new(),
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

    fn get_value_from_stack(&self, offset: isize) -> Result<Value, RuntimeError> {
        let index = self.rbp as isize + offset;
        if index < 0 || index as usize >= STACK_MAX {
            return Err(RuntimeError::RangeError(
                "stack access out of bounds".to_string(),
            ));
        }
        Ok(self.data_stack[index as usize].clone())
    }

    fn set_value_to_stack(&mut self, offset: isize, value: Value) -> Result<(), RuntimeError> {
        let index = self.rbp as isize + offset;
        if index < 0 || index as usize >= STACK_MAX {
            return Err(RuntimeError::RangeError(
                "stack access out of bounds".to_string(),
            ));
        }
        self.data_stack[index as usize] = value;
        Ok(())
    }

    fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.get(name).cloned()
    }
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
            RuntimeError::InternalError(msg) => write!(f, "InternalError: {msg}"),
            RuntimeError::Thrown(val) => write!(f, "{val}"),
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
        state.data_stack[0] = Value::Number(42.0);
        state.rbp = 0;
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
