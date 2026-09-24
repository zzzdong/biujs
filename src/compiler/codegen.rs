use std::collections::{BTreeMap, HashMap};

use log::{debug, trace};

use super::ir::{BlockId, ControlFlowGraph, Instruction, Value, Variable};
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
    /// Keep every variable in a fixed stack slot, using registers only as
    /// per-instruction temporaries.
    ///
    /// Default. A single-pass, layout-ordered code generator cannot know which
    /// predecessor ran, so a value that this pass believes is in a register may
    /// never have been written on the path that is actually taken. Addressing
    /// variables through memory removes that entire class of bug. It is also
    /// faster in practice: no registers have to be saved around a call.
    /// `BIUJS_REG_VARS` opts into the register path (see `begin_block` /
    /// `spill_live_out`), which keeps values in registers inside a block and
    /// only routes them through memory at block boundaries.
    memory_resident_vars: bool,
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
            // Memory-resident by default; opt into the register path explicitly.
            memory_resident_vars: std::env::var("BIUJS_REG_VARS").is_err(),
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

        // A value can escape to an exception handler from the middle of a block,
        // which the per-terminator hand-off of the register path cannot cover.
        // Functions with a handler therefore always keep variables in memory,
        // so any handler can reload them whichever instruction threw.
        let uses_exception_handlers = cfg.blocks().iter().any(|block| {
            block
                .instructions()
                .iter()
                .any(|inst| matches!(inst, Instruction::PushSeh { .. }))
        });
        if uses_exception_handlers {
            self.memory_resident_vars = true;
        }

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

            // No register state survives a block boundary; each block reloads
            // what it needs from memory (see `RegAlloc::begin_block`).
            self.reg_alloc.begin_block();

            for inst in block.instructions() {
                // Hand the values a successor still needs over through memory
                // before control leaves the block.
                if inst.is_terminator() {
                    for (register, stack) in self.reg_alloc.spill_live_out(block.id()) {
                        trace!("block-end spill [rbp+{stack}] <- {register}");
                        self.codes.push(Bytecode::double(
                            Opcode::Mov,
                            Operand::Stack(stack as isize),
                            register.into(),
                        ));
                    }
                }

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
                    Instruction::ArrayPushSpread { array, src } => {
                        let array = self.gen_operand(array);
                        let src = self.gen_operand(src);
                        self.codes.push(Bytecode::double(
                            Opcode::ArrayPushSpread,
                            array,
                            src,
                        ));
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
                    Instruction::PropertyDelete {
                        dst,
                        object,
                        property,
                    } => {
                        let dst = self.gen_operand(dst);
                        let object = self.gen_operand(object);
                        let property = self.gen_operand(property);
                        self.codes.push(Bytecode::triple(
                            Opcode::PropDelete,
                            dst,
                            object,
                            property,
                        ));
                    }
                    Instruction::IndexDelete {
                        dst,
                        object,
                        index,
                    } => {
                        let dst = self.gen_operand(dst);
                        let object = self.gen_operand(object);
                        let index = self.gen_operand(index);
                        self.codes
                            .push(Bytecode::triple(Opcode::IndexDelete, dst, object, index));
                    }
                    Instruction::StoreEnv { name, value } => {
                        let name = name.to_operand();
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Bytecode::double(Opcode::StoreEnv, name, value));
                    }
                    Instruction::Arguments { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::single(Opcode::Arguments, dst));
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
                    Instruction::DelegateOpen { iter } => {
                        let iter = self.gen_operand(iter);
                        self.codes
                            .push(Bytecode::single(Opcode::DelegateOpen, iter));
                    }
                    Instruction::DelegateClose { iter } => {
                        let iter = self.gen_operand(iter);
                        self.codes
                            .push(Bytecode::single(Opcode::DelegateClose, iter));
                    }
                    Instruction::MakeIterator {
                        src: iter,
                        dst: result,
                    } => {
                        let src = self.gen_operand(iter);
                        let dst = self.gen_operand(result);
                        self.codes
                            .push(Bytecode::double(Opcode::MakeIter, dst, src));
                    }
                    Instruction::IteratorClose { iter } => {
                        let iter = self.gen_operand(iter);
                        self.codes.push(Bytecode::single(Opcode::IterClose, iter));
                    }
                    Instruction::Yield { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = match src {
                            Some(src) => self.gen_operand(src),
                            None => Operand::new_immd(-1),
                        };
                        self.codes.push(Bytecode::double(Opcode::Yield, dst, src));
                    }
                    Instruction::ToString { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = self.gen_operand(src);
                        self.codes.push(Bytecode::double(Opcode::ToString, dst, src));
                    }
                    Instruction::MakeRest { dst, from } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Bytecode::double(
                            Opcode::MakeRest,
                            dst,
                            Operand::new_immd(from as isize),
                        ));
                    }
                    Instruction::CallSpread {
                        result,
                        callee,
                        this,
                        args,
                    } => {
                        let callee = self.gen_operand(callee);
                        let this = self.gen_operand(this);
                        let args = self.gen_operand(args);
                        // The callee runs in a nested frame and freely reuses
                        // registers, so live values must be saved across it —
                        // same contract as `gen_call`.
                        let in_use_registers = self.call_saved_registers();
                        for reg in in_use_registers.iter().copied() {
                            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
                        }
                        self.codes
                            .push(Bytecode::triple(Opcode::CallSpread, callee, this, args));
                        for reg in in_use_registers.iter().rev().copied() {
                            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
                        }
                        // The result travels in Rv (same as CallMethod).
                        let result = self.gen_operand(result);
                        self.codes.push(Bytecode::double(
                            Opcode::Mov,
                            result,
                            Operand::new_register(Register::Rv),
                        ));
                    }
                    Instruction::CallSuper {
                        result,
                        callee,
                        this,
                        args,
                    } => {
                        let callee = self.gen_operand(callee);
                        let this = self.gen_operand(this);
                        let args = self.gen_operand(args);
                        // Same register contract as `CallSpread`: the callee runs
                        // in a nested frame and reuses registers.
                        let in_use_registers = self.call_saved_registers();
                        for reg in in_use_registers.iter().copied() {
                            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
                        }
                        self.codes.push(Bytecode::triple(
                            Opcode::CallSuperSpread,
                            callee,
                            this,
                            args,
                        ));
                        for reg in in_use_registers.iter().rev().copied() {
                            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
                        }
                        let result = self.gen_operand(result);
                        self.codes.push(Bytecode::double(
                            Opcode::Mov,
                            result,
                            Operand::new_register(Register::Rv),
                        ));
                    }
                    Instruction::NewSpread {
                        dst,
                        constructor,
                        args,
                    } => {                        let dst = self.gen_operand(dst);
                        let ctor = self.gen_operand(constructor);
                        let args = self.gen_operand(args);
                        let in_use_registers = self.call_saved_registers();
                        for reg in in_use_registers.iter().copied() {
                            self.codes.push(Bytecode::single(Opcode::Push, reg.into()));
                        }
                        self.codes
                            .push(Bytecode::triple(Opcode::NewSpread, dst, ctor, args));
                        for reg in in_use_registers.iter().rev().copied() {
                            self.codes.push(Bytecode::single(Opcode::Pop, reg.into()));
                        }
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
                    Instruction::TypeOfEnv { dst, name } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Bytecode::double(Opcode::TypeOfEnv, dst, name.to_operand()));
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
                    Instruction::LoadNewTarget { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Bytecode::single(Opcode::LoadNewTarget, dst));
                    }
                    Instruction::LoadCurrentFunction { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Bytecode::single(Opcode::LoadCurrentFunction, dst));
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
                        } else {
                            // A value-less `return` (or falling off the end of a
                            // function) yields `undefined`. Rv still holds the
                            // result of the last call otherwise, which would make
                            // a constructor "return" that leftover object.
                            self.codes.push(Bytecode::double(
                                Opcode::Mov,
                                Operand::new_register(Register::Rv),
                                Operand::new_primitive(crate::bytecode::Primitive::Undefined),
                            ));
                        }

                        self.codes.push(Bytecode::empty(Opcode::Ret));
                    }
                    Instruction::Jump { dst, args } => {
                        // Hand each block parameter over through its stack slot
                        // (see `store_jump_args` for why it must not travel in a
                        // register only).
                        let params = cfg
                            .get_block(dst.to_block())
                            .map(|b| b.params())
                            .unwrap_or_default();
                        self.store_jump_args(params, &args);

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
                        // Hand both branches' parameters over through memory.
                        let true_params = cfg
                            .get_block(true_blk.to_block())
                            .map(|b| b.params())
                            .unwrap_or_default();
                        self.store_jump_args(true_params, &true_args);

                        let false_params = cfg
                            .get_block(false_blk.to_block())
                            .map(|b| b.params())
                            .unwrap_or_default();
                        self.store_jump_args(false_params, &false_args);

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
                                self.store_jump_args(params, &args);
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
                                self.store_jump_args(params, &args);
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
                            // The target is consumed later by `ResumeExc`, at the
                            // end of the finally block, so it must be an absolute
                            // PC: a pc-relative offset would be applied from the
                            // wrong instruction.
                            let absolute = this.block_map[&(target as isize)];
                            this.codes[pos].operands[0] = Operand::new_immd(absolute);
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
        let in_use_registers = self.call_saved_registers();
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

        // 4. Call the function (the argument count travels along so the callee
        //    can materialise `arguments`).
        self.codes.push(Bytecode::double(
            Opcode::Call,
            func.to_operand(),
            Operand::new_immd(args.len() as isize),
        ));

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
        let in_use_registers = self.call_saved_registers();
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

        // 4. Call the function (CallEx). The argument count travels with the
        //    instruction so the VM can read the pushed arguments when the
        //    callee turns out to be a native built-in.
        self.codes.push(Bytecode::double(
            Opcode::CallEx,
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

    fn gen_call_native(&mut self, func: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(func);

        // 1. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 2. Set up new stack frame
        self.codes.push(Bytecode::single(
            Opcode::PushC,
            Operand::new_register(Register::Rbp),
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
        let in_use_registers = self.call_saved_registers();
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
        let in_use_registers = self.call_saved_registers();
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

    /// Registers that have to survive a nested call.
    ///
    /// With variables resident in memory nothing is held in a register across a
    /// call, so the callee cannot clobber a value the caller still needs.
    fn call_saved_registers(&self) -> Vec<Register> {
        if self.memory_resident_vars {
            Vec::new()
        } else {
            self.reg_alloc.call_saved_registers()
        }
    }

    /// Hand a block's parameters over at a jump.
    ///
    /// A block parameter is not produced by an instruction inside its own block:
    /// it is written by every predecessor's terminator. The value therefore
    /// always travels through the parameter's dedicated stack slot, and the
    /// block entry reloads it from there. Delivering only through a register is
    /// unsound here: the predecessor and the target can be generated in either
    /// order, so a register hand-off written by one edge is invisible (or stale)
    /// when the block itself is generated — which is how a loop header ended up
    /// reading a stale stack slot while the back edge updated only the register,
    /// leaving the loop counter frozen.
    fn store_jump_args(&mut self, params: &[Variable], args: &[Value]) {
        for (param, arg) in params.iter().zip(args.iter()) {
            let arg_op = self.gen_operand(*arg);
            let stack = self.reg_alloc.ensure_stack_slot(*param);
            self.codes.push(Bytecode::double(
                Opcode::Mov,
                Operand::Stack(stack as isize),
                arg_op,
            ));
            // The slot now holds the incoming value; any register copy is stale,
            // so the block entry must reload from memory.
            self.reg_alloc.mark_stack_written(*param);
            self.reg_alloc.forget_register(param);
        }
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

    /// Emit the bookkeeping that comes with a register allocation.
    ///
    /// `alloc` may report that a victim was spilled (`Action::Spill`) and/or
    /// that the value itself must be reloaded from its stack slot
    /// (`Action::Restore`). The actions are emitted in the order given: a spill
    /// must reach memory before the register is overwritten by a restore.
    /// Callers that only take the register silently drop that work and lose
    /// values.
    fn apply_alloc_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Spill { register, stack } => {
                    trace!("spilling [rbp+{stack}] <- {register}");
                    self.codes.push(Bytecode::double(
                        Opcode::Mov,
                        Operand::Stack(stack as isize),
                        register.into(),
                    ));
                }
                Action::Restore { register, stack } => {
                    trace!("unspilling [rbp+{stack}] -> {register}");
                    self.codes.push(Bytecode::double(
                        Opcode::Mov,
                        register.into(),
                        Operand::Stack(stack as isize),
                    ));
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
            Value::Variable(var) if self.memory_resident_vars => {
                // Memory-resident mode: every variable lives in a fixed stack
                // slot, so no value can be lost by register reuse.
                let stack = self.reg_alloc.ensure_stack_slot(var);
                Operand::Stack(stack as isize)
            }
            Value::Variable(var) => {
                let (register, actions) = self.reg_alloc.alloc(var, self.inst_index);
                trace!("allocating {value} -> {register}, actions = {actions:?}");
                self.apply_alloc_actions(actions);
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
