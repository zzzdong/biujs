pub mod value;

pub use value::Value;

use std::collections::HashMap;

use crate::bytecode::{Bytecode, Constant, FunctionId, Module, Opcode, Operand, Primitive, Register};

const STACK_MAX: usize = 0x0FFF;

/// JavaScript Virtual Machine
///
/// Executes bytecode modules produced by the Compiler.
/// Ported from evalit's register-based VM with JS-specific Value semantics.
pub struct VM {
    state: State,
}

impl VM {
    pub fn new() -> Self {
        Self {
            state: State::new(),
        }
    }

    /// Execute a bytecode module and return the result
    pub fn run(&mut self, module: &Module) -> Result<Value, RuntimeError> {
        self.state = State::new();

        while let Some(inst) = module.instructions.get(self.state.pc).cloned() {
            let Bytecode { opcode, operands: _ } = inst;

            match opcode {
                Opcode::Halt => {
                    let ret = self.state.get_register(Register::Rv)?;
                    return Ok(ret);
                }
                Opcode::Ret => {
                    if self.state.ctrl_stack_reached_bottom() {
                        let ret = self.state.get_register(Register::Rv)?;
                        return Ok(ret);
                    }
                    let return_pc = self.state.popc()?;
                    let saved_seh_depth = self.state.popc()?;
                    self.state.seh_stack.truncate(saved_seh_depth);
                    self.state.jump(return_pc);
                }
                _ => {
                    self.run_instruction(&inst, module)?;
                }
            }
        }

        // If we run out of instructions, return Rv
        Ok(self.state.get_register(Register::Rv).unwrap_or(Value::Undefined))
    }

    fn run_instruction(&mut self, inst: &Bytecode, module: &Module) -> Result<(), RuntimeError> {
        let Bytecode { opcode, operands } = inst;

        match opcode {
            // ===== Control Flow =====
            Opcode::Call => {
                let func_id = operands[0].as_immd();
                match module.symtab.get(&FunctionId::new(func_id as u32)) {
                    Some(location) => {
                        self.state.pushc(self.state.seh_stack.len())?;
                        self.state.pushc(self.state.pc + 1)?;
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
                            Value::Function(id) => match module.symtab.get(&FunctionId::new(id)) {
                                Some(location) => {
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
                            },
                            _ => {
                                return Err(RuntimeError::TypeError(
                                    "not a function".to_string(),
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "invalid call target".to_string(),
                        ));
                    }
                }
            }
            Opcode::Jump => {
                let offset = operands[0].as_immd();
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
                        // Look up in global environment
                        // For now, we store globals in a simple HashMap
                        // TODO: integrate with proper Environment
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

            // ===== Arithmetic =====
            Opcode::Addx => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let value = lhs + rhs;
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
                let result = Self::js_abstract_relational(&rhs, &lhs)?;
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::GreaterEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // lhs >= rhs  ⟺  !(lhs < rhs)
                let less = Self::js_abstract_relational(&lhs, &rhs)?;
                self.set_value(operands[0], Value::Bool(!less))?;
            }
            Opcode::Less => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = Self::js_abstract_relational(&lhs, &rhs)?;
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::LessEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                // lhs <= rhs  ⟺  !(rhs < lhs)
                let less = Self::js_abstract_relational(&rhs, &lhs)?;
                self.set_value(operands[0], Value::Bool(!less))?;
            }
            Opcode::Equal => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = lhs.abstract_eq(&rhs);
                self.set_value(operands[0], Value::Bool(result))?;
            }
            Opcode::NotEqual => {
                let lhs = self.get_value(operands[1])?;
                let rhs = self.get_value(operands[2])?;
                let result = !lhs.abstract_eq(&rhs);
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
                self.set_value(operands[0], Value::String(type_str))?;
            }
            Opcode::InstanceOf => {
                // TODO: implement when objects/prototypes exist
                self.set_value(operands[0], Value::Bool(false))?;
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
                self.set_value(operands[0], Value::Array(Vec::new()))?;
            }
            Opcode::ArrayPush => {
                let array_val = self.get_value(operands[0])?;
                let elem = self.get_value(operands[1])?;
                match array_val {
                    Value::Array(mut arr) => {
                        arr.push(elem);
                        self.set_value(operands[0], Value::Array(arr))?;
                    }
                    _ => {
                        return Err(RuntimeError::TypeError(
                            "ArrayPush on non-array".to_string(),
                        ));
                    }
                }
            }
            Opcode::MakeObject => {
                self.set_value(operands[0], Value::Object(HashMap::new()))?;
            }
            Opcode::IndexGet => {
                let obj = self.get_value(operands[1])?;
                let idx = self.get_value(operands[2])?;
                let value = Self::js_index_get(&obj, &idx)?;
                self.set_value(operands[0], value)?;
            }
            Opcode::IndexSet => {
                let obj = self.get_value(operands[0])?;
                let idx = self.get_value(operands[1])?;
                let val = self.get_value(operands[2])?;
                Self::js_index_set(&obj, &idx, val)?;
                // Note: IndexSet modifies in-place via interior mutability
                // For now, we need to handle this differently since Value is clone-based
                // TODO: use Rc<RefCell<>> for mutable objects
                return Err(RuntimeError::NotImplemented);
            }
            Opcode::PropGet => {
                let obj = self.get_value(operands[1])?;
                let prop_val = self.get_value(operands[2])?;
                let prop_name = match prop_val {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                let value = Self::js_prop_get(&obj, &prop_name)?;
                self.set_value(operands[0], value)?;
            }
            Opcode::PropSet => {
                let _obj = self.get_value(operands[0])?;
                let _prop = self.get_value(operands[1])?;
                let _val = self.get_value(operands[2])?;
                // TODO: implement property mutation (needs Rc<RefCell<>> for objects)
                return Err(RuntimeError::NotImplemented);
            }
            Opcode::CallMethod => {
                let _obj = self.get_value(operands[0])?;
                let _method = self.get_value(operands[1])?;
                let _arg_count = operands[2].as_immd() as usize;
                // TODO: implement method calls
                return Err(RuntimeError::NotImplemented);
            }

            // ===== Exception Handling =====
            Opcode::Try => {
                let handler_offset = operands[0].as_immd();
                let handler_pc = (self.state.pc as isize + handler_offset) as usize;
                let record = SehRecord {
                    handler_pc,
                    saved_rsp: self.state.rsp,
                    saved_rbp: self.state.rbp,
                };
                self.state.seh_stack.push(record);
            }
            Opcode::EndTry => {
                self.state.seh_stack.pop();
            }
            Opcode::ThrowExc => {
                let exc_val = self.get_value(operands[0])?;
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
                // TODO: implement new operator
                return Err(RuntimeError::NotImplemented);
            }
            Opcode::CallNative => {
                // TODO: implement native function calls
                return Err(RuntimeError::NotImplemented);
            }

            Opcode::Halt | Opcode::Ret => {
                // These are handled in run(), not here
                unreachable!("Halt/Ret should be handled in run()")
            }
        }

        self.state.jump_offset(1);
        Ok(())
    }

    fn handle_throw(&mut self, exc_val: Value) -> Result<(), RuntimeError> {
        match self.state.seh_stack.pop() {
            Some(record) => {
                // Restore stack pointers to try entry point
                self.state.rsp = record.saved_rsp;
                self.state.rbp = record.saved_rbp;

                // Put exception value in Rv for LoadException
                self.state.set_register(Register::Rv, exc_val)?;
                self.state.jump(record.handler_pc);
                Ok(())
            }
            None => {
                Err(RuntimeError::Custom(exc_val))
            }
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
            Constant::String(s) => Value::String(s.as_ref().clone()),
        }
    }

    /// JS Abstract Relational Comparison (ECMAScript 13.10.1)
    ///
    /// Returns true if x < y using ToPrimitive with number coercion.
    fn js_abstract_relational(x: &Value, y: &Value) -> Result<bool, RuntimeError> {
        // ToPrimitive: for basic types, use the value directly
        // String comparison has priority when both are strings
        match (x, y) {
            (Value::String(xs), Value::String(ys)) => {
                Ok(xs < ys)
            }
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

    fn js_index_get(obj: &Value, index: &Value) -> Result<Value, RuntimeError> {
        match obj {
            Value::Array(arr) => {
                if let Value::Number(n) = index {
                    let i = *n as i64;
                    if i >= 0 && (i as usize) < arr.len() {
                        Ok(arr[i as usize].clone())
                    } else {
                        Ok(Value::Undefined)
                    }
                } else {
                    Ok(Value::Undefined)
                }
            }
            Value::Object(map) => {
                let key = match index {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                Ok(map.get(&key).cloned().unwrap_or(Value::Undefined))
            }
            _ => Ok(Value::Undefined),
        }
    }

    fn js_index_set(_obj: &Value, _index: &Value, _value: Value) -> Result<(), RuntimeError> {
        // TODO: need interior mutability for objects
        Err(RuntimeError::NotImplemented)
    }

    fn js_prop_get(obj: &Value, prop: &str) -> Result<Value, RuntimeError> {
        match obj {
            Value::Object(map) => {
                Ok(map.get(prop).cloned().unwrap_or(Value::Undefined))
            }
            Value::String(s) => {
                // String prototype properties
                match prop {
                    "length" => Ok(Value::Number(s.len() as f64)),
                    _ => Ok(Value::Undefined),
                }
            }
            Value::Array(arr) => {
                match prop {
                    "length" => Ok(Value::Number(arr.len() as f64)),
                    _ => Ok(Value::Undefined),
                }
            }
            _ => Ok(Value::Undefined),
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
    registers: [Value; 19], // R0..R15 + Rsp_idx + Rbp_idx + Rv_idx
    seh_stack: Vec<SehRecord>,
    globals: HashMap<String, Value>,
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
            Register::Rsp | Register::Rbp => {
                Err(RuntimeError::TypeError(format!(
                    "cannot write register {reg} as value"
                )))
            }
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
        self.ctrl_stack.pop().ok_or_else(|| {
            RuntimeError::RangeError("control stack underflow".to_string())
        })
    }

    fn get_value_from_stack(&self, offset: isize) -> Result<Value, RuntimeError> {
        let index = self.rbp as isize + offset;
        if index < 0 || index as usize >= STACK_MAX {
            return Err(RuntimeError::RangeError("stack access out of bounds".to_string()));
        }
        Ok(self.data_stack[index as usize].clone())
    }

    fn set_value_to_stack(&mut self, offset: isize, value: Value) -> Result<(), RuntimeError> {
        let index = self.rbp as isize + offset;
        if index < 0 || index as usize >= STACK_MAX {
            return Err(RuntimeError::RangeError("stack access out of bounds".to_string()));
        }
        self.data_stack[index as usize] = value;
        Ok(())
    }

    fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.get(name).cloned()
    }
}

/// SEH (Structured Exception Handling) record for try/catch
struct SehRecord {
    handler_pc: usize,
    saved_rsp: usize,
    saved_rbp: usize,
}

#[derive(Debug)]
pub enum RuntimeError {
    NotImplemented,
    TypeError(String),
    ReferenceError(String),
    RangeError(String),
    SyntaxError(String),
    Custom(Value),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::NotImplemented => write!(f, "feature not yet implemented"),
            RuntimeError::TypeError(msg) => write!(f, "TypeError: {msg}"),
            RuntimeError::ReferenceError(msg) => write!(f, "ReferenceError: {msg}"),
            RuntimeError::RangeError(msg) => write!(f, "RangeError: {msg}"),
            RuntimeError::SyntaxError(msg) => write!(f, "SyntaxError: {msg}"),
            RuntimeError::Custom(val) => write!(f, "{val}"),
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
        state.set_register(Register::R0, Value::Number(42.0)).unwrap();
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
        assert_eq!(VM::from_primitive(Primitive::Boolean(true)), Value::Bool(true));
        assert_eq!(VM::from_primitive(Primitive::Integer(42)), Value::Number(42.0));
        assert_eq!(VM::from_primitive(Primitive::Float(3.14)), Value::Number(3.14));
    }

    #[test]
    fn test_vm_from_constant() {
        let c = Constant::from("hello");
        assert_eq!(VM::from_constant(&c), Value::String("hello".to_string()));
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
        assert!(VM::js_abstract_relational(
            &Value::String("a".to_string()),
            &Value::String("b".to_string())
        ).unwrap());
        assert!(!VM::js_abstract_relational(
            &Value::String("b".to_string()),
            &Value::String("a".to_string())
        ).unwrap());
        assert!(!VM::js_abstract_relational(
            &Value::String("a".to_string()),
            &Value::String("a".to_string())
        ).unwrap());
    }

    #[test]
    fn test_js_abstract_relational_nan() {
        assert!(!VM::js_abstract_relational(
            &Value::Number(f64::NAN),
            &Value::Number(1.0)
        ).unwrap());
        assert!(!VM::js_abstract_relational(
            &Value::Number(1.0),
            &Value::Number(f64::NAN)
        ).unwrap());
    }

    #[test]
    fn test_js_abstract_relational_mixed_types() {
        // "2" < 3 → true (string coerced to number)
        assert!(VM::js_abstract_relational(
            &Value::String("2".to_string()),
            &Value::Number(3.0)
        ).unwrap());
        // "hello" < 3 → false ("hello" → NaN)
        assert!(!VM::js_abstract_relational(
            &Value::String("hello".to_string()),
            &Value::Number(3.0)
        ).unwrap());
    }

    // ──────────────────────── js_prop_get ────────────────────────

    #[test]
    fn test_js_prop_get_string_length() {
        let s = Value::String("hello".to_string());
        assert_eq!(VM::js_prop_get(&s, "length").unwrap(), Value::Number(5.0));
    }

    #[test]
    fn test_js_prop_get_string_unknown() {
        let s = Value::String("hello".to_string());
        assert_eq!(VM::js_prop_get(&s, "foo").unwrap(), Value::Undefined);
    }

    #[test]
    fn test_js_prop_get_array_length() {
        let arr = Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]);
        assert_eq!(VM::js_prop_get(&arr, "length").unwrap(), Value::Number(2.0));
    }

    #[test]
    fn test_js_prop_get_object() {
        let mut map = HashMap::new();
        map.insert("x".to_string(), Value::Number(42.0));
        let obj = Value::Object(map);
        assert_eq!(VM::js_prop_get(&obj, "x").unwrap(), Value::Number(42.0));
        assert_eq!(VM::js_prop_get(&obj, "y").unwrap(), Value::Undefined);
    }

    #[test]
    fn test_js_prop_get_number() {
        let n = Value::Number(42.0);
        assert_eq!(VM::js_prop_get(&n, "toString").unwrap(), Value::Undefined);
    }

    // ──────────────────────── js_index_get ────────────────────────

    #[test]
    fn test_js_index_get_array() {
        let arr = Value::Array(vec![
            Value::Number(10.0),
            Value::Number(20.0),
            Value::Number(30.0),
        ]);
        assert_eq!(VM::js_index_get(&arr, &Value::Number(0.0)).unwrap(), Value::Number(10.0));
        assert_eq!(VM::js_index_get(&arr, &Value::Number(1.0)).unwrap(), Value::Number(20.0));
        assert_eq!(VM::js_index_get(&arr, &Value::Number(2.0)).unwrap(), Value::Number(30.0));
        // Out of bounds → undefined
        assert_eq!(VM::js_index_get(&arr, &Value::Number(5.0)).unwrap(), Value::Undefined);
        // Negative index → undefined
        assert_eq!(VM::js_index_get(&arr, &Value::Number(-1.0)).unwrap(), Value::Undefined);
    }

    #[test]
    fn test_js_index_get_object() {
        let mut map = HashMap::new();
        map.insert("key".to_string(), Value::Number(42.0));
        let obj = Value::Object(map);
        assert_eq!(
            VM::js_index_get(&obj, &Value::String("key".to_string())).unwrap(),
            Value::Number(42.0)
        );
        assert_eq!(
            VM::js_index_get(&obj, &Value::String("missing".to_string())).unwrap(),
            Value::Undefined
        );
    }

    #[test]
    fn test_js_index_get_non_indexable() {
        let n = Value::Number(42.0);
        assert_eq!(VM::js_index_get(&n, &Value::Number(0.0)).unwrap(), Value::Undefined);
    }

    // ──────────────────────── VM::run with simple modules ────────────────────────

    /// Helper to build a minimal module with given instructions
    fn make_module(instructions: Vec<Bytecode>, constants: Vec<Constant>) -> crate::bytecode::Module {
        crate::bytecode::Module::new(
            Some("test".to_string()),
            constants,
            HashMap::new(),
            instructions,
        )
    }

    #[test]
    fn test_vm_run_halt() {
        let module = make_module(
            vec![Bytecode::empty(Opcode::Halt)],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Undefined);
    }

    #[test]
    fn test_vm_run_mov_halt() {
        let module = make_module(
            vec![
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(true))),
                Bytecode::empty(Opcode::Halt),
            ],
            vec![],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_run_load_const_halt() {
        let module = make_module(
            vec![
                Bytecode::double(Opcode::LoadConst, Operand::Register(Register::R0), Operand::Immd(0)),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Register(Register::R0)),
                Bytecode::empty(Opcode::Halt),
            ],
            vec![Constant::from("hello")],
        );
        let mut vm = VM::new();
        let result = vm.run(&module).unwrap();
        assert_eq!(result, Value::String("hello".to_string()));
    }

    #[test]
    fn test_vm_run_arithmetic() {
        // R0 = 10, R1 = 3, R2 = R0 + R1, Rv = R2, halt
        let module = make_module(
            vec![
                Bytecode::double(Opcode::Mov, Operand::Register(Register::R0), Operand::Primitive(Primitive::Float(10.0))),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::R1), Operand::Primitive(Primitive::Float(3.0))),
                Bytecode::triple(Opcode::Addx, Operand::Register(Register::R2), Operand::Register(Register::R0), Operand::Register(Register::R1)),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Register(Register::R2)),
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
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(false))),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(true))),
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
                Bytecode::triple(Opcode::BrIf, Operand::Primitive(Primitive::Boolean(true)), Operand::Immd(2), Operand::Immd(1)),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(false))),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(true))),
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
                Bytecode::triple(Opcode::BrIf, Operand::Primitive(Primitive::Boolean(false)), Operand::Immd(1), Operand::Immd(2)),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(true))),
                Bytecode::double(Opcode::Mov, Operand::Register(Register::Rv), Operand::Primitive(Primitive::Boolean(false))),
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
            format!("{}", RuntimeError::SyntaxError("bad syntax".to_string())),
            "SyntaxError: bad syntax"
        );
        assert_eq!(
            format!("{}", RuntimeError::NotImplemented),
            "feature not yet implemented"
        );
        assert_eq!(
            format!("{}", RuntimeError::Custom(Value::Number(42.0))),
            "42"
        );
    }
}
