use crate::bytecode::Constant;
use crate::bytecode::FunctionId;
use crate::bytecode::Opcode;
use crate::compiler::ir::IrFunction;
use crate::compiler::ir::IrUnit;

use super::cfg::ControlFlowGraph;
use super::instruction::*;

pub trait InstBuilder {
    fn module(&self) -> &IrUnit;

    fn module_mut(&mut self) -> &mut IrUnit;

    fn control_flow_graph(&self) -> &ControlFlowGraph;

    fn control_flow_graph_mut(&mut self) -> &mut ControlFlowGraph;

    fn set_entry(&mut self, entry: BlockId) {
        self.control_flow_graph_mut().set_entry(entry);
    }

    fn emit(&mut self, inst: Instruction) {
        self.control_flow_graph_mut().emit(inst);
    }

    fn make_halt(&mut self) {
        self.emit(Instruction::Halt { value: None });
        self.seal_block(self.current_block());
    }

    fn make_halt_with_value(&mut self, value: Value) {
        self.emit(Instruction::Halt { value: Some(value) });
        self.seal_block(self.current_block());
    }

    fn alloc(&mut self) -> Value {
        self.control_flow_graph_mut().create_variable()
    }

    fn make_constant(&mut self, value: Constant) -> Value {
        let const_id = self.module_mut().make_constant(value);
        Value::Constant(const_id)
    }

    fn load_constant(&mut self, value: Constant) -> Value {
        let const_id = self.module_mut().make_constant(value);
        let dst = self.alloc();
        self.emit(Instruction::LoadConst {
            dst,
            const_id: Value::Constant(const_id),
        });
        dst
    }

    fn load_arg(&mut self, index: usize) -> Value {
        let addr = self.alloc();
        self.emit(Instruction::LoadArg { dst: addr, index });
        addr
    }

    fn create_block(&mut self, label: Name) -> BlockId {
        self.control_flow_graph_mut().create_block(label)
    }

    fn current_block(&self) -> BlockId {
        self.control_flow_graph()
            .current_block()
            .expect("no current block")
    }

    fn try_current_block(&self) -> Option<BlockId> {
        self.control_flow_graph().current_block()
    }

    fn switch_to_block(&mut self, block: BlockId) {
        self.control_flow_graph_mut().switch_to_block(block);
    }

    fn seal_block(&mut self, block: BlockId) {
        self.control_flow_graph_mut().seal_block(block);
    }

    fn unaryop(&mut self, op: Opcode, src: Value) -> Value {
        let result = self.alloc();

        self.emit(Instruction::UnaryOp {
            op,
            dst: result,
            src,
        });

        result
    }

    fn binop(&mut self, op: Opcode, lhs: Value, rhs: Value) -> Value {
        let result = self.alloc();

        self.emit(Instruction::BinaryOp {
            op,
            dst: result,
            lhs,
            rhs,
        });

        result
    }

    fn assign(&mut self, dst: Value, src: Value) {
        self.emit(Instruction::Move { dst, src })
    }

    fn get_property(&mut self, object: Value, property: &str) -> Value {
        let result = self.alloc();

        let property = self.make_constant(property.into());

        self.emit(Instruction::PropertyGet {
            dst: result,
            object,
            property,
        });

        result
    }

    fn set_property(&mut self, object: Value, property: &str, value: Value) {
        let property = self.make_constant(property.into());

        self.emit(Instruction::PropertySet {
            object,
            property,
            value,
        });
    }

    fn call_property(&mut self, object: Value, property: &str, args: Vec<Value>) -> Value {
        let dst = self.alloc();

        let property = self.make_constant(property.into());

        self.emit(Instruction::PropertyCall {
            object,
            property,
            args,
            result: dst,
        });

        dst
    }

    fn call_property_dynamic(&mut self, object: Value, property: Value, args: Vec<Value>) -> Value {
        let dst = self.alloc();

        self.emit(Instruction::PropertyCall {
            object,
            property,
            args,
            result: dst,
        });

        dst
    }

    fn load_external_variable(&mut self, name: String) -> Value {
        let result = self.alloc();

        let name = self.make_constant(name.into());

        self.emit(Instruction::LoadEnv { dst: result, name });

        result
    }

    fn call_function(&mut self, func: FunctionId, args: Vec<Value>) -> Value {
        let result = self.alloc();

        if args.len() > 8 {
            panic!("too many arguments");
        }

        self.emit(Instruction::Call {
            func: Value::Function(func),
            args,
            result,
        });

        result
    }

    fn make_call(&mut self, func: Value, args: Vec<Value>) -> Value {
        let result = self.alloc();

        if args.len() > 8 {
            panic!("too many arguments");
        }

        self.emit(Instruction::CallEx {
            callable: func,
            args,
            result,
        });

        result
    }

    fn make_call_native(&mut self, func: Value, args: Vec<Value>) -> Value {
        let result = self.alloc();

        if args.len() > 8 {
            panic!("too many arguments");
        }

        self.emit(Instruction::CallNative { func, args, result });

        result
    }

    fn jump(&mut self, dst_blk: BlockId) {
        self.emit(Instruction::Jump {
            dst: Value::Block(dst_blk),
            args: Vec::new(),
        });
    }

    fn br_if(&mut self, condition: Value, true_blk: BlockId, false_blk: BlockId) {
        self.emit(Instruction::BrIf {
            condition,
            true_blk: Value::Block(true_blk),
            false_blk: Value::Block(false_blk),
            true_args: Vec::new(),
            false_args: Vec::new(),
        });
    }

    fn return_(&mut self, value: Option<Value>) {
        self.emit(Instruction::Return { value });
    }

    fn make_iterator(&mut self, iter: Value) -> Value {
        let result = self.alloc();

        self.emit(Instruction::MakeIterator {
            src: iter,
            dst: result,
        });

        result
    }

    fn iterate_next(&mut self, iter: Value) -> (Value, Value) {
        let next = self.alloc();
        let has_next = self.alloc();
        self.emit(Instruction::IterateNext {
            iter,
            item: next,
            has_next,
        });
        (next, has_next)
    }

    fn make_array(&mut self) -> Value {
        let array = self.alloc();
        self.emit(Instruction::MakeArray { dst: array });
        array
    }

    fn array_push(&mut self, array: Value, value: Value) -> Value {
        self.emit(Instruction::ArrayPush { array, value });
        array
    }

    fn make_object(&mut self) -> Value {
        let object = self.alloc();
        self.emit(Instruction::MakeObject { dst: object });
        object
    }

    fn index_get(&mut self, object: Value, index: Value) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::IndexGet { dst, object, index });
        dst
    }

    fn index_set(&mut self, object: Value, index: Value, value: Value) {
        self.emit(Instruction::IndexSet {
            object,
            index,
            value,
        });
    }

    fn typeof_(&mut self, src: Value) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::TypeOf { dst, src });
        dst
    }

    fn new_(&mut self, constructor: Value, args: Vec<Value>) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::New {
            dst,
            constructor,
            args,
        });
        dst
    }

    fn load_this(&mut self) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::LoadThis { dst });
        dst
    }

    fn make_func_obj(&mut self, func_id: Value) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::MakeFuncObj { dst, func_id });
        dst
    }

    fn make_arrow_func_obj(&mut self, func_id: Value, captured_this: Value) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::MakeArrowFuncObj { dst, func_id, captured_this });
        dst
    }

    fn push_seh(&mut self, handler: BlockId, finally: Option<BlockId>) {
        self.emit(Instruction::PushSeh { handler, finally });
    }

    fn pop_seh(&mut self) {
        self.emit(Instruction::PopSeh);
    }

    fn add_exception_edge(&mut self, try_body: BlockId, catch: BlockId) {
        self.control_flow_graph_mut().add_edge(try_body, catch);
    }

    fn throw_value(&mut self, value: Value) {
        self.emit(Instruction::Throw {
            value,
            args: Vec::new(),
        });
    }

    fn load_exception(&mut self) -> Value {
        let dst = self.alloc();
        self.emit(Instruction::LoadException { dst });
        dst
    }

    fn resume_exception(&mut self) {
        self.emit(Instruction::ResumeException { args: vec![] });
    }

    fn delayed_jump(&mut self, target: BlockId, seh_depth: usize) {
        self.emit(Instruction::DelayedJump { target, seh_depth });
    }

    /// 封存当前块并切换到目标块
    fn seal_and_switch_to(&mut self, target: BlockId) {
        let current = self.current_block();
        self.seal_block(current);
        self.switch_to_block(target);
    }
}

#[derive(Debug)]
pub struct IrBuilder<'a> {
    module: &'a mut IrUnit,
}

impl<'a> IrBuilder<'a> {
    pub fn new(module: &'a mut IrUnit) -> Self {
        Self { module }
    }
}

impl InstBuilder for IrBuilder<'_> {
    fn module(&self) -> &IrUnit {
        self.module
    }

    fn module_mut(&mut self) -> &mut IrUnit {
        self.module
    }

    fn control_flow_graph(&self) -> &ControlFlowGraph {
        &self.module.control_flow_graph
    }

    fn control_flow_graph_mut(&mut self) -> &mut ControlFlowGraph {
        &mut self.module.control_flow_graph
    }
}

pub struct FunctionBuilder<'long, 'short: 'long> {
    module: &'long mut IrUnit,
    function: &'short mut IrFunction,
}

impl<'long, 'short: 'long> FunctionBuilder<'long, 'short> {
    pub fn new(module: &'long mut IrUnit, function: &'short mut IrFunction) -> Self {
        Self { module, function }
    }
}

impl<'long, 'short: 'long> InstBuilder for FunctionBuilder<'long, 'short> {
    fn module(&self) -> &IrUnit {
        self.module
    }

    fn module_mut(&mut self) -> &mut IrUnit {
        self.module
    }

    fn control_flow_graph(&self) -> &ControlFlowGraph {
        &self.function.control_flow_graph
    }

    fn control_flow_graph_mut(&mut self) -> &mut ControlFlowGraph {
        &mut self.function.control_flow_graph
    }
}
