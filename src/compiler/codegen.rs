use std::collections::{BTreeMap, HashMap};

use log::{debug, trace};

use super::ir::{BlockId, ControlFlowGraph, Instruction, Value};
use crate::bytecode::{Bytecode, Opcode, Operand, Register};

use super::regalloc::{Action, RegAlloc};

type PatchFn = Box<dyn Fn(&mut Codegen)>;

pub struct Codegen {
    reg_alloc: RegAlloc,
    codes: Vec<Bytecode>,
    block_map: HashMap<isize, isize>,
    inst_index: usize,
    insts: BTreeMap<usize, Instruction>,
    throw_to_handlers: HashMap<BlockId, Vec<BlockId>>,
}

impl Codegen {
    pub fn new(registers: &[Register], throw_to_handlers: HashMap<BlockId, Vec<BlockId>>) -> Self {
        Self {
            reg_alloc: RegAlloc::new(registers),
            codes: Vec::new(),
            block_map: HashMap::new(),
            inst_index: 0,
            insts: BTreeMap::new(),
            throw_to_handlers,
        }
    }

    pub fn generate_code(&mut self, cfg: ControlFlowGraph) -> &[Bytecode] {
        // debug ir
        log::debug!("=== IR CFG ===");
        for block in cfg.blocks() {
            log::debug!("B{} (params: {:?}):", block.id(), block.params());
            for inst in block.instructions() {
                log::debug!("  {}", inst);
            }
        }
        log::debug!("=== end IR ===");
        let block_layout = cfg.loop_root_reverse_postorder_layout2();

        self.reg_alloc.arrange(&cfg, &block_layout);

        let mut patchs: Vec<PatchFn> = Vec::new();

        // alloc stack frame, need rewrite with actual stack size
        // rsp = rsp + stack_size
        let pos = self.codes.len();
        patchs.push(Box::new(move |this: &mut Self| {
            this.codes[pos].operands[2] = Operand::new_immd(this.reg_alloc.stack_size() as isize)
        }));
        // placeholder
        self.codes.push(Bytecode::triple(
            Opcode::AddC,
            Operand::Register(Register::Rsp),
            Operand::Register(Register::Rsp),
            Operand::new_immd(0),
        ));

        for block in block_layout.iter(&cfg) {
            self.block_map
                .insert(block.id().as_usize() as isize, self.codes.len() as isize);

            for inst in block.instructions() {
                debug!("inst[{}]: {inst:?}", self.inst_index);
                debug!("register: {}", self.reg_alloc.reg_set);

                self.insts.insert(self.codes.len(), inst.clone());

                match inst.clone() {
                    // Function Call Instructions
                    Instruction::Call { func, args, result } => {
                        self.gen_call(func, &args, result);
                    }
                    Instruction::CallEx {
                        callable,
                        args,
                        result,
                    } => {
                        self.gen_call_ex(callable, &args, result);
                    }
                    Instruction::CallNative { func, args, result } => {
                        self.gen_call_native(func, &args, result);
                    }
                    Instruction::PropertyCall {
                        object,
                        property,
                        args,
                        result,
                    } => {
                        self.gen_prop_call(object, property, &args, result);
                    }

                    // Load and Move Instructions
                    Instruction::LoadArg { dst, index } => {
                        let dst = self.gen_operand(dst);
                        let stack = self.reg_alloc.load_arg(index);
                        self.codes.push(Bytecode::double(
                            Opcode::Mov,
                            dst,
                            Operand::new_stack(stack),
                        ));
                    }
                    Instruction::LoadConst { dst, const_id } => {
                        let dst = self.gen_operand(dst);

                        self.codes.push(Bytecode::double(
                            Opcode::LoadConst,
                            dst,
                            const_id.to_operand(),
                        ));
                    }
                    Instruction::LoadEnv { dst, name } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Bytecode::double(Opcode::LoadEnv, dst, name.to_operand()));
                    }
                    Instruction::Move { dst, src } => {
                        let src = self.gen_operand(src);
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::double(Opcode::Mov, dst, src));
                    }

                    // Unary and Binary Operators
                    Instruction::UnaryOp { op, dst, src } => {
                        let src = self.gen_operand(src);
                        let dst = self.gen_operand(dst);

                        self.codes.push(Bytecode::double(op, dst, src));
                    }
                    Instruction::BinaryOp { op, dst, lhs, rhs } => {
                        let src1 = self.gen_operand(lhs);
                        let src2 = self.gen_operand(rhs);
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::triple(op, dst, src1, src2));
                    }

                    // Collection / Structural Operations
                    Instruction::MakeArray { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::single(Opcode::MakeArray, dst));
                    }
                    Instruction::ArrayPush { array, value } => {
                        let array = self.gen_operand(array);
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Bytecode::double(Opcode::ArrayPush, array, value));
                    }
                    Instruction::MakeObject { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::single(Opcode::MakeObject, dst));
                    }
                    Instruction::IndexSet {
                        object,
                        index: idx,
                        value,
                    } => {
                        let object = self.gen_operand(object);
                        let idx = self.gen_operand(idx);
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Bytecode::triple(Opcode::IndexSet, object, idx, value));
                    }
                    Instruction::IndexGet {
                        dst,
                        object,
                        index: idx,
                    } => {
                        let dst = self.gen_operand(dst);
                        let object = self.gen_operand(object);
                        let idx = self.gen_operand(idx);
                        self.codes
                            .push(Bytecode::triple(Opcode::IndexGet, dst, object, idx));
                    }
                    Instruction::PropertyGet {
                        dst,
                        object,
                        property,
                    } => {
                        let dst = self.gen_operand(dst);
                        let object = self.gen_operand(object);
                        let property = self.gen_operand(property);
                        self.codes
                            .push(Bytecode::triple(Opcode::PropGet, dst, object, property));
                    }
                    Instruction::PropertySet {
                        object,
                        property,
                        value,
                    } => {
                        let object = self.gen_operand(object);
                        let property = self.gen_operand(property);
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Bytecode::triple(Opcode::PropSet, object, property, value));
                    }

                    // Iteration Instructions
                    Instruction::MakeIterator {
                        src: iter,
                        dst: result,
                    } => {
                        let src = self.gen_operand(iter);
                        let dst = self.gen_operand(result);
                        self.codes
                            .push(Bytecode::double(Opcode::MakeIter, dst, src));
                    }
                    Instruction::IterateNext {
                        iter,
                        item,
                        has_next,
                    } => {
                        let src = self.gen_operand(iter);
                        let dst = self.gen_operand(item);
                        let has_next = self.gen_operand(has_next);
                        self.codes
                            .push(Bytecode::triple(Opcode::IterNext, dst, has_next, src));
                    }

                    // JS-specific Instructions
                    Instruction::TypeOf { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = self.gen_operand(src);
                        self.codes.push(Bytecode::double(Opcode::TypeOf, dst, src));
                    }
                    Instruction::New {
                        dst,
                        constructor,
                        args,
                    } => {
                        self.gen_new(constructor, &args, dst);
                    }
                    Instruction::LoadThis { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::single(Opcode::LoadThis, dst));
                    }
                    Instruction::MakeFuncObj { dst, func_id } => {
                        let dst = self.gen_operand(dst);
                        let func_id = self.gen_operand(func_id);
                        self.codes
                            .push(Bytecode::double(Opcode::MakeFuncObj, dst, func_id));
                    }
                    Instruction::MakeArrowFuncObj {
                        dst,
                        func_id,
                        captured_this,
                    } => {
                        let dst = self.gen_operand(dst);
                        let func_id = self.gen_operand(func_id);
                        let captured_this = self.gen_operand(captured_this);
                        self.codes.push(Bytecode::triple(
                            Opcode::MakeArrowFuncObj,
                            dst,
                            func_id,
                            captured_this,
                        ));
                    }
                    Instruction::ClosureVar { name, value } => {
                        let name = name.to_operand();
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Bytecode::double(Opcode::ClosureVar, name, value));
                    }

                    // Control Flow Instructions
                    Instruction::Return { value } => {
                        if let Some(v) = value {
                            let ret = self.gen_operand(v);
                            self.codes.push(Bytecode::double(
                                Opcode::Mov,
                                Operand::new_register(Register::Rv),
                                ret,
                            ));
                        }

                        self.codes.push(Bytecode::empty(Opcode::Ret));
                    }
                    Instruction::Jump { dst, args } => {
                        // 为每个跳转参数生成mov指令
                        for (param, arg) in cfg
                            .get_block(dst.to_block())
                            .map(|b| b.params())
                            .unwrap_or_default()
                            .iter()
                            .zip(args.iter())
                        {
                            let arg_op = self.gen_operand(*arg);
                            // 检查形参是否已分配寄存器
                            if let Some(param_reg) = self.reg_alloc.get_register(param) {
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    param_reg.into(),
                                    arg_op,
                                ));
                            } else if let Some(stack_offset) =
                                self.reg_alloc.get_stack_offset(param)
                            {
                                // 形参在栈上，存储到栈位置
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    Operand::Stack(stack_offset as isize),
                                    arg_op,
                                ));
                            } else {
                                // 既不在寄存器也不在栈上，分配一个新寄存器（后备方案）
                                let param_reg = self.reg_alloc.alloc(*param, self.inst_index).0;
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    param_reg.into(),
                                    arg_op,
                                ));
                            }
                        }

                        let dst = self.gen_operand(dst);

                        let pos = self.codes.len();
                        patchs.push(Box::new(move |this: &mut Self| {
                            let dst = this.codes[pos].operands[0].as_immd();
                            this.codes[pos].operands[0] =
                                Operand::new_immd(this.block_map[&dst] - pos as isize);
                        }));

                        self.codes.push(Bytecode::single(Opcode::Jump, dst));
                    }
                    Instruction::BrIf {
                        condition,
                        true_blk,
                        false_blk,
                        true_args,
                        false_args,
                    } => {
                        // 为true分支参数生成mov指令
                        let true_block_id = true_blk.to_block();
                        let true_params = cfg
                            .get_block(true_block_id)
                            .map(|b| b.params())
                            .unwrap_or_default();
                        for (param, arg) in true_params.iter().zip(true_args.iter()) {
                            let arg_op = self.gen_operand(*arg);
                            if let Some(param_reg) = self.reg_alloc.get_register(param) {
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    param_reg.into(),
                                    arg_op,
                                ));
                            } else if let Some(stack_offset) =
                                self.reg_alloc.get_stack_offset(param)
                            {
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    Operand::Stack(stack_offset as isize),
                                    arg_op,
                                ));
                            } else {
                                let param_reg = self.reg_alloc.alloc(*param, self.inst_index).0;
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    param_reg.into(),
                                    arg_op,
                                ));
                            }
                        }

                        // 为false分支参数生成mov指令
                        let false_block_id = false_blk.to_block();
                        let false_params = cfg
                            .get_block(false_block_id)
                            .map(|b| b.params())
                            .unwrap_or_default();
                        for (param, arg) in false_params.iter().zip(false_args.iter()) {
                            let arg_op = self.gen_operand(*arg);
                            if let Some(param_reg) = self.reg_alloc.get_register(param) {
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    param_reg.into(),
                                    arg_op,
                                ));
                            } else if let Some(stack_offset) =
                                self.reg_alloc.get_stack_offset(param)
                            {
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    Operand::Stack(stack_offset as isize),
                                    arg_op,
                                ));
                            } else {
                                let param_reg = self.reg_alloc.alloc(*param, self.inst_index).0;
                                self.codes.push(Bytecode::double(
                                    Opcode::Mov,
                                    param_reg.into(),
                                    arg_op,
                                ));
                            }
                        }

                        let condition = self.gen_operand(condition);
                        let true_blk = self.gen_operand(true_blk);
                        let false_blk = self.gen_operand(false_blk);

                        let pos = self.codes.len();
                        patchs.push(Box::new(move |this: &mut Self| {
                            let true_blk = this.codes[pos].operands[1].as_immd();
                            this.codes[pos].operands[1] =
                                Operand::new_immd(this.block_map[&true_blk] - pos as isize);
                            let false_blk = this.codes[pos].operands[2].as_immd();
                            this.codes[pos].operands[2] =
                                Operand::new_immd(this.block_map[&false_blk] - pos as isize);
                        }));

                        self.codes.push(Bytecode::triple(
                            Opcode::BrIf,
                            condition,
                            true_blk,
                            false_blk,
                        ));
                    }
                    Instruction::Halt { value } => {
                        if let Some(v) = value {
                            let src = self.gen_operand(v);
                            self.codes.push(Bytecode::double(
                                Opcode::Mov,
                                Operand::new_register(Register::Rv),
                                src,
                            ));
                        }
                        self.codes.push(Bytecode::empty(Opcode::Halt));
                    }
                    Instruction::PushSeh { handler, finally } => {
                        let handler_id = handler.as_usize() as isize;
                        let pos = self.codes.len();
                        patchs.push(Box::new(move |this: &mut Self| {
                            // Patch catch handler offset
                            this.codes[pos].operands[0] =
                                Operand::new_immd(this.block_map[&handler_id] - pos as isize);
                            // Patch finally handler offset if present
                            if let Some(finally_blk) = finally {
                                let finally_id = finally_blk.as_usize() as isize;
                                this.codes[pos].operands[1] =
                                    Operand::new_immd(this.block_map[&finally_id] - pos as isize);
                            }
                        }));
                        // Use triple to hold both catch and finally offsets
                        self.codes.push(Bytecode::triple(
                            Opcode::Try,
                            Operand::new_immd(0),
                            Operand::new_immd(0),
                            Operand::new_immd(0),
                        ));
                    }
                    Instruction::PopSeh => {
                        self.codes.push(Bytecode::empty(Opcode::EndTry));
                    }
                    Instruction::Throw { value, args } => {
                        // 为异常handler的phi参数生成mov指令
                        // 仅处理异常handler后继块，忽略正常跳转目标（死代码）
                        let handlers: Vec<BlockId> = self
                            .throw_to_handlers
                            .get(&block.id())
                            .map(|h| h.clone())
                            .unwrap_or_default();
                        for &succ_id in cfg.get_successors(block.id()) {
                            // 跳过非异常handler的后继块
                            if !handlers.contains(&succ_id) {
                                continue;
                            }
                            if let Some(succ_block) = cfg.get_block(succ_id) {
                                let params = succ_block.params();
                                if params.is_empty() {
                                    continue;
                                }
                                // 这个后继块是catch handler，传递phi参数
                                for (param, arg) in params.iter().zip(args.iter()) {
                                    let arg_op = self.gen_operand(*arg);
                                    if let Some(param_reg) = self.reg_alloc.get_register(param) {
                                        self.codes.push(Bytecode::double(
                                            Opcode::Mov,
                                            param_reg.into(),
                                            arg_op,
                                        ));
                                    } else if let Some(stack_offset) =
                                        self.reg_alloc.get_stack_offset(param)
                                    {
                                        self.codes.push(Bytecode::double(
                                            Opcode::Mov,
                                            Operand::Stack(stack_offset as isize),
                                            arg_op,
                                        ));
                                    } else {
                                        let param_reg =
                                            self.reg_alloc.alloc(*param, self.inst_index).0;
                                        self.codes.push(Bytecode::double(
                                            Opcode::Mov,
                                            param_reg.into(),
                                            arg_op,
                                        ));
                                    }
                                }
                            }
                        }
                        let val = self.gen_operand(value);
                        self.codes.push(Bytecode::single(Opcode::ThrowExc, val));
                    }
                    Instruction::LoadException { dst } => {
                        let reg = self.gen_operand(dst);
                        self.codes
                            .push(Bytecode::single(Opcode::LoadException, reg));
                    }
                    Instruction::ResumeException { args } => {
                        let handlers: Vec<BlockId> = self
                            .throw_to_handlers
                            .get(&block.id())
                            .map(|h| h.clone())
                            .unwrap_or_default();
                        for &succ_id in cfg.get_successors(block.id()) {
                            if !handlers.contains(&succ_id) {
                                continue;
                            }
                            if let Some(succ_block) = cfg.get_block(succ_id) {
                                let params = succ_block.params();
                                if params.is_empty() {
                                    continue;
                                }
                                for (param, arg) in params.iter().zip(args.iter()) {
                                    let arg_op = self.gen_operand(*arg);
                                    if let Some(param_reg) = self.reg_alloc.get_register(param) {
                                        self.codes.push(Bytecode::double(
                                            Opcode::Mov,
                                            param_reg.into(),
                                            arg_op,
                                        ));
                                    } else if let Some(stack_offset) =
                                        self.reg_alloc.get_stack_offset(param)
                                    {
                                        self.codes.push(Bytecode::double(
                                            Opcode::Mov,
                                            Operand::Stack(stack_offset as isize),
                                            arg_op,
                                        ));
                                    } else {
                                        let param_reg =
                                            self.reg_alloc.alloc(*param, self.inst_index).0;
                                        self.codes.push(Bytecode::double(
                                            Opcode::Mov,
                                            param_reg.into(),
                                            arg_op,
                                        ));
                                    }
                                }
                            }
                        }
                        self.codes.push(Bytecode::empty(Opcode::ResumeExc));
                    }
                    Instruction::DelayedJump { target, seh_depth } => {
                        let target_op = Operand::new_immd(target.as_usize() as isize);
                        let seh_depth_op = Operand::new_immd(seh_depth as isize);

                        let pos = self.codes.len();
                        patchs.push(Box::new(move |this: &mut Self| {
                            let target = this.codes[pos].operands[0].as_immd() as usize;
                            let offset = this.block_map[&(target as isize)] - pos as isize;
                            this.codes[pos].operands[0] = Operand::new_immd(offset);
                        }));

                        self.codes.push(Bytecode::double(
                            Opcode::DelayedJump,
                            target_op,
                            seh_depth_op,
                        ));
                    }
                }

                self.inst_index += 1;
            }
        }

        for patch in patchs {
            patch(self);
        }

        &self.codes
    }

    fn gen_call(&mut self, func: Value, args: &[Value], result: Value) {
        // 1. Backup used registers
        let in_use_registers = self.reg_alloc.in_use_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 3. Set up new stack frame
        self.codes.push(Bytecode::single(
            Opcode::PushC,
            Operand::new_register(Register::Rbp),
        ));
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rbp),
            Operand::new_register(Register::Rsp),
        ));

        // 4. Call the function
        self.codes
            .push(Bytecode::single(Opcode::Call, func.to_operand()));

        // 5. Restore stack pointer (reset to current base pointer)
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rsp),
            Operand::new_register(Register::Rbp),
        ));

        // 6. Pop the saved base pointer
        self.codes
            .push(Bytecode::single(Opcode::PopC, Register::Rbp.into()));

        // 7. Clean up arguments from the stack
        self.codes.push(Bytecode::triple(
            Opcode::SubC,
            Operand::Register(Register::Rsp),
            Operand::Register(Register::Rsp),
            Operand::new_immd(args.len() as isize),
        ));

        // 8. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
        }

        // 9. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Bytecode::double(
            Opcode::Mov,
            result_reg,
            Operand::new_register(Register::Rv),
        ));
    }

    fn gen_call_ex(&mut self, func: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(func);

        // 1. Backup used registers
        let in_use_registers = self.reg_alloc.in_use_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 3. Set up new stack frame
        self.codes.push(Bytecode::single(
            Opcode::PushC,
            Operand::new_register(Register::Rbp),
        ));
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rbp),
            Operand::new_register(Register::Rsp),
        ));

        // 4. Call the function (CallEx)
        self.codes.push(Bytecode::single(Opcode::CallEx, callable));

        // 5. Restore stack pointer to current base pointer
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rsp),
            Operand::new_register(Register::Rbp),
        ));

        // 6. Pop saved base pointer
        self.codes
            .push(Bytecode::single(Opcode::PopC, Register::Rbp.into()));

        // 7. Clean up arguments from the stack
        self.codes.push(Bytecode::triple(
            Opcode::SubC,
            Operand::Register(Register::Rsp),
            Operand::Register(Register::Rsp),
            Operand::new_immd(args.len() as isize),
        ));

        // 8. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
        }

        // 9. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Bytecode::double(
            Opcode::Mov,
            result_reg,
            Operand::new_register(Register::Rv),
        ));
    }

    fn gen_call_native(&mut self, func: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(func);

        // 1. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 2. Set up new stack frame
        self.codes.push(Bytecode::single(
            Opcode::PushC,
            Operand::new_register(Register::Rbp),
        ));
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rbp),
            Operand::new_register(Register::Rsp),
        ));

        // 3. Call the native function
        self.codes.push(Bytecode::double(
            Opcode::CallNative,
            callable,
            Operand::new_immd(args.len() as isize),
        ));

        // 4. Restore stack pointer to current base pointer
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rsp),
            Operand::new_register(Register::Rbp),
        ));

        // 5. Pop saved base pointer
        self.codes
            .push(Bytecode::single(Opcode::PopC, Register::Rbp.into()));

        // 6. Clean up arguments from the stack
        self.codes.push(Bytecode::triple(
            Opcode::SubC,
            Operand::Register(Register::Rsp),
            Operand::Register(Register::Rsp),
            Operand::new_immd(args.len() as isize),
        ));

        // 7. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Bytecode::double(
            Opcode::Mov,
            result_reg,
            Operand::new_register(Register::Rv),
        ));
    }

    fn gen_prop_call(&mut self, object: Value, property: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(object);

        // 1. Backup used registers
        let in_use_registers = self.reg_alloc.in_use_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 2. Set up new stack frame
        self.codes.push(Bytecode::single(
            Opcode::PushC,
            Operand::new_register(Register::Rbp),
        ));
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rbp),
            Operand::new_register(Register::Rsp),
        ));

        let prop = self.gen_operand(property);
        // 3. Call method on the object
        self.codes.push(Bytecode::triple(
            Opcode::CallMethod,
            callable,
            prop,
            Operand::new_immd(args.len() as isize),
        ));

        // 4. Restore stack pointer to current base pointer
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rsp),
            Operand::new_register(Register::Rbp),
        ));

        // 5. Pop saved base pointer
        self.codes
            .push(Bytecode::single(Opcode::PopC, Register::Rbp.into()));

        // 6. Clean up arguments from the stack
        self.codes.push(Bytecode::triple(
            Opcode::SubC,
            Operand::Register(Register::Rsp),
            Operand::Register(Register::Rsp),
            Operand::new_immd(args.len() as isize),
        ));

        // 7. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
        }

        // 8. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Bytecode::double(
            Opcode::Mov,
            result_reg,
            Operand::new_register(Register::Rv),
        ));
    }

    fn gen_new(&mut self, constructor: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(constructor);

        // 1. Backup used registers
        let in_use_registers = self.reg_alloc.in_use_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 3. Set up new stack frame
        self.codes.push(Bytecode::single(
            Opcode::PushC,
            Operand::new_register(Register::Rbp),
        ));
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rbp),
            Operand::new_register(Register::Rsp),
        ));

        // 4. Call the constructor with New opcode (passing arg count)
        self.codes.push(Bytecode::double(
            Opcode::New,
            callable,
            Operand::new_immd(args.len() as isize),
        ));

        // 5. Restore stack pointer to current base pointer
        self.codes.push(Bytecode::double(
            Opcode::MovC,
            Operand::new_register(Register::Rsp),
            Operand::new_register(Register::Rbp),
        ));

        // 6. Pop saved base pointer
        self.codes
            .push(Bytecode::single(Opcode::PopC, Register::Rbp.into()));

        // 7. Clean up arguments from the stack
        self.codes.push(Bytecode::triple(
            Opcode::SubC,
            Operand::Register(Register::Rsp),
            Operand::Register(Register::Rsp),
            Operand::new_immd(args.len() as isize),
        ));

        // 8. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
        }

        // 9. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Bytecode::double(
            Opcode::Mov,
            result_reg,
            Operand::new_register(Register::Rv),
        ));
    }

    fn store_args(&mut self, args: &[Value], index: usize) {
        for arg in args.iter().rev() {
            let op = self.gen_operand(*arg);
            self.codes.push(Bytecode::single(Opcode::Push, op));
            if let Value::Variable(arg) = arg
                && let Some(action) = self.reg_alloc.release(*arg, index)
            {
                match action {
                    Action::Spill { stack, register } => {
                        trace!("spilling({arg}) {register} -> [rbp+{stack}]");
                        self.codes.push(Bytecode::double(
                            Opcode::Mov,
                            Operand::Stack(stack as isize),
                            register.into(),
                        ));
                    }
                    _ => unreachable!("action must be spill"),
                }
            }
        }
    }

    fn gen_operand(&mut self, value: Value) -> Operand {
        match value {
            Value::Primitive(v) => Operand::new_primitive(v),
            Value::Constant(id) => Operand::new_immd(id.as_usize() as isize),
            Value::Function(id) => Operand::new_symbol(id.as_usize() as u32),
            Value::Block(id) => Operand::new_immd(id.as_usize() as isize),
            Value::Variable(var) => {
                let (register, action) = self.reg_alloc.alloc(var, self.inst_index);
                trace!("allocating {value} -> {register}, action = {action:?}");

                if let Some(action) = action {
                    match action {
                        Action::Restore { stack, register } => {
                            trace!("unspilling({value}) [rbp+{stack}] -> {register}");
                            self.codes.push(Bytecode::double(
                                Opcode::Mov,
                                register.into(),
                                Operand::Stack(stack as isize),
                            ));
                        }
                        Action::Spill { register, stack } => {
                            // A victim variable was evicted from `register`; preserve
                            // its value on the stack before the register is reused.
                            trace!("spilling [rbp+{stack}] <- {register}");
                            self.codes.push(Bytecode::double(
                                Opcode::Mov,
                                Operand::Stack(stack as isize),
                                register.into(),
                            ));
                        }
                    }
                }

                Operand::new_register(register)
            }
        }
    }

    #[allow(dead_code)]
    fn emit_code(&mut self, code: Bytecode) {
        self.codes.push(code);
    }

    pub fn debug_insts(&self) -> &BTreeMap<usize, Instruction> {
        &self.insts
    }
}
