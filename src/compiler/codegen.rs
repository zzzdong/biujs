use std::collections::{BTreeMap, HashMap};

use log::{debug, trace};

use super::ir::{BlockId, ControlFlowGraph, Instruction, Value, Variable};
use crate::bytecode::{AbsPc, EhRegion, Instr, Operand, Register, RelPc};

use super::regalloc::{Action, RegAlloc};

type PatchFn = Box<dyn Fn(&mut Codegen)>;

/// 一条受保护区域的**声明**：块 id 形式，来自 IR 的 `PushSeh` / `PopSeh`。
///
/// 布局完成后用 `block_map` 解析成 pc（见 `Codegen::resolve_eh_regions`）。
/// 之所以先留声明再解析：`PushSeh` 发在**跳进 `try` 体之前的那条指令流位置**上，
/// 而 `try` 体的入口块要等布局才知道落在哪个 pc；`EndTry` 更是在体降级完之后才发射。
#[derive(Debug, Clone, Copy)]
struct EhDecl {
    /// 区域身份 = `try` 体入口块（`PushSeh.body`）。
    body: isize,
    /// `Try` 指令自己的 pc（运行期登记处理点的位置，也是 `start`）。
    try_pc: isize,
    /// 处理器块：有 `catch` 时是 catch 块，否则是 finally 块（与 ChakraCore 同构）。
    handler: isize,
    /// `finally` 块（没有则为 `None`）。
    finally: Option<isize>,
}

pub struct Codegen {
    reg_alloc: RegAlloc,
    codes: Vec<Instr>,
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
    /// EH 区域声明（顺序即 `Try` 的发射顺序）。
    eh_decls: Vec<EhDecl>,
    /// `(区域身份块 id, 该 `EndTry` 的 pc)`。**一条区域可以有 0/1/2 个**：
    /// `try` 体的正常退出与 `catch` 的正常退出各发射一次 `PopSeh`。
    eh_exits: Vec<(isize, isize)>,
    /// 解析好的区域表（`generate_code` 末尾填好，供 `eh_regions()` 取）。
    eh_regions: Vec<EhRegion>,
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
            eh_decls: Vec::new(),
            eh_exits: Vec::new(),
            eh_regions: Vec::new(),
        }
    }

    pub fn generate_code(&mut self, cfg: ControlFlowGraph) -> &[Instr] {
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
            // 帧大小要等寄存器分配跑完才知道，所以先发射占位再回填。
            if let Instr::AddC { value, .. } = &mut this.codes[pos] {
                *value = Operand::new_immd(this.reg_alloc.stack_size() as isize);
            }
        }));
        // placeholder
        self.codes.push(Instr::AddC { dst: Operand::Register(Register::Rsp), src: Operand::Register(Register::Rsp), value: Operand::new_immd(0) });

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
                        self.codes.push(Instr::Mov { dst: Operand::Stack(stack as isize), src: register.into() });
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
                        self.codes.push(Instr::Mov { dst: dst, src: Operand::new_stack(stack) });
                    }
                    Instruction::LoadConst { dst, const_id } => {
                        let dst = self.gen_operand(dst);

                        self.codes.push(Instr::LoadConst { dst: dst, index: const_id.to_operand() });
                    }
                    Instruction::MakeRegExp { dst, source, flags } => {
                        let dst = self.gen_operand(dst);
                        let source = self.gen_operand(source);
                        let flags = self.gen_operand(flags);
                        self.codes.push(Instr::MakeRegExp { dst: dst, source: source, flags: flags });
                    }
                    Instruction::DeclareLexical { name } => {
                        let name = name.to_operand();
                        self.codes
                            .push(Instr::DeclareLexical { name: name });
                    }
                    Instruction::InitLexical { name, value } => {
                        let name = name.to_operand();
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Instr::InitLexical { name: name, value: value });
                    }
                    Instruction::SetFunctionName { func, name } => {
                        let func = self.gen_operand(func);
                        let name = self.gen_operand(name);
                        self.codes
                            .push(Instr::SetFunctionName { func: func, name: name });
                    }
                    Instruction::LoadEnv { dst, name } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Instr::LoadEnv { dst: dst, name: name.to_operand() });
                    }
                    Instruction::Move { dst, src } => {
                        let src = self.gen_operand(src);
                        let dst = self.gen_operand(dst);
                        self.codes.push(Instr::Mov { dst: dst, src: src });
                    }

                    // Unary and Binary Operators
                    Instruction::UnaryOp { op, dst, src } => {
                        let src = self.gen_operand(src);
                        let dst = self.gen_operand(dst);

                        self.codes.push(Instr::unary(op, dst, src));
                    }
                    Instruction::BinaryOp { op, dst, lhs, rhs } => {
                        let src1 = self.gen_operand(lhs);
                        let src2 = self.gen_operand(rhs);
                        let dst = self.gen_operand(dst);
                        self.codes.push(Instr::binary(op, dst, src1, src2));
                    }

                    // Collection / Structural Operations
                    Instruction::MakeArray { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Instr::MakeArray { dst: dst });
                    }
                    Instruction::ArrayPushSpread { array, src } => {
                        let array = self.gen_operand(array);
                        let src = self.gen_operand(src);
                        self.codes.push(Instr::ArrayPushSpread { array: array, src: src });
                    }
                    Instruction::MarkHole { array } => {
                        let array = self.gen_operand(array);
                        self.codes.push(Instr::MarkHole { array: array });
                    }
                    Instruction::ArrayPush { array, value } => {
                        let array = self.gen_operand(array);
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Instr::ArrayPush { array: array, value: value });
                    }
                    Instruction::MakeObject { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Instr::MakeObject { dst: dst });
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
                            .push(Instr::IndexSet { object: object, index: idx, value: value });
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
                            .push(Instr::IndexGet { dst: dst, object: object, index: idx });
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
                            .push(Instr::PropGet { dst: dst, object: object, property: property });
                    }
                    Instruction::PropertyDelete {
                        dst,
                        object,
                        property,
                    } => {
                        let dst = self.gen_operand(dst);
                        let object = self.gen_operand(object);
                        let property = self.gen_operand(property);
                        self.codes.push(Instr::PropDelete { dst: dst, object: object, property: property });
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
                            .push(Instr::IndexDelete { dst: dst, object: object, index: index });
                    }
                    Instruction::StoreEnv { name, value } => {
                        let name = name.to_operand();
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Instr::StoreEnv { name: name, value: value });
                    }
                    Instruction::Arguments { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Instr::Arguments { dst: dst });
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
                            .push(Instr::PropSet { object: object, property: property, value: value });
                    }

                    // Iteration Instructions
                    Instruction::DelegateOpen { iter } => {
                        let iter = self.gen_operand(iter);
                        self.codes
                            .push(Instr::DelegateOpen { iter: iter });
                    }
                    Instruction::DelegateClose { iter } => {
                        let iter = self.gen_operand(iter);
                        self.codes
                            .push(Instr::DelegateClose { iter: iter });
                    }
                    Instruction::MakeIterator {
                        src: iter,
                        dst: result,
                    } => {
                        let src = self.gen_operand(iter);
                        let dst = self.gen_operand(result);
                        self.codes
                            .push(Instr::MakeIter { dst: dst, src: src });
                    }
                    Instruction::IteratorClose { iter } => {
                        let iter = self.gen_operand(iter);
                        self.codes.push(Instr::IterClose { iter: iter });
                    }
                    Instruction::RequireObjectCoercible { src } => {
                        let src = self.gen_operand(src);
                        self.codes
                            .push(Instr::RequireObjectCoercible { src: src });
                    }
                    Instruction::Yield { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = match src {
                            Some(src) => self.gen_operand(src),
                            None => Operand::new_immd(-1),
                        };
                        self.codes.push(Instr::Yield { dst: dst, value: src });
                    }
                    Instruction::PrologueEnd => {
                        self.codes.push(Instr::PrologueEnd {});
                    }
                    Instruction::Await { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = match src {
                            Some(src) => self.gen_operand(src),
                            None => Operand::new_immd(-1),
                        };
                        self.codes.push(Instr::Await { dst: dst, src: src });
                    }
                    Instruction::ToString { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = self.gen_operand(src);
                        self.codes.push(Instr::ToString { dst: dst, src: src });
                    }
                    Instruction::MakeRest { dst, from } => {
                        let dst = self.gen_operand(dst);
                        self.codes.push(Instr::MakeRest { dst: dst, from: Operand::new_immd(from as isize) });
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
                            self.codes.push(Instr::Push { src: reg.into() });
                        }
                        self.codes
                            .push(Instr::CallSpread { callee: callee, this: this, args: args });
                        for reg in in_use_registers.iter().rev().copied() {
                            self.codes.push(Instr::Pop { dst: reg.into() });
                        }
                        // The result travels in Rv (same as CallMethod).
                        let result = self.gen_operand(result);
                        self.codes.push(Instr::Mov { dst: result, src: Operand::new_register(Register::Rv) });
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
                            self.codes.push(Instr::Push { src: reg.into() });
                        }
                        self.codes.push(Instr::CallSuperSpread { callee: callee, this: this, args: args });
                        for reg in in_use_registers.iter().rev().copied() {
                            self.codes.push(Instr::Pop { dst: reg.into() });
                        }
                        let result = self.gen_operand(result);
                        self.codes.push(Instr::Mov { dst: result, src: Operand::new_register(Register::Rv) });
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
                            self.codes.push(Instr::Push { src: reg.into() });
                        }
                        self.codes
                            .push(Instr::NewSpread { dst: dst, ctor: ctor, args: args });
                        for reg in in_use_registers.iter().rev().copied() {
                            self.codes.push(Instr::Pop { dst: reg.into() });
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
                            .push(Instr::IterNext { dst: dst, has_next: has_next, src: src });
                    }

                    // JS-specific Instructions
                    Instruction::TypeOf { dst, src } => {
                        let dst = self.gen_operand(dst);
                        let src = self.gen_operand(src);
                        self.codes.push(Instr::TypeOf { dst: dst, src: src });
                    }
                    Instruction::TypeOfEnv { dst, name } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Instr::TypeOfEnv { dst: dst, name: name.to_operand() });
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
                        self.codes.push(Instr::LoadThis { dst: dst });
                    }
                    Instruction::LoadNewTarget { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Instr::LoadNewTarget { dst: dst });
                    }
                    Instruction::LoadCurrentFunction { dst } => {
                        let dst = self.gen_operand(dst);
                        self.codes
                            .push(Instr::LoadCurrentFunction { dst: dst });
                    }
                    Instruction::MakeFuncObj { dst, func_id } => {
                        let dst = self.gen_operand(dst);
                        let func_id = self.gen_operand(func_id);
                        self.codes
                            .push(Instr::MakeFuncObj { dst: dst, func: func_id });
                    }
                    Instruction::MakeArrowFuncObj {
                        dst,
                        func_id,
                        captured_this,
                    } => {
                        let dst = self.gen_operand(dst);
                        let func_id = self.gen_operand(func_id);
                        let captured_this = self.gen_operand(captured_this);
                        self.codes.push(Instr::MakeArrowFuncObj { dst: dst, func: func_id, captured_this: captured_this });
                    }
                    Instruction::ClosureVar { name, value } => {
                        let name = name.to_operand();
                        let value = self.gen_operand(value);
                        self.codes
                            .push(Instr::ClosureVar { name: name, value: value });
                    }

                    // Control Flow Instructions
                    Instruction::Return { value } => {
                        if let Some(v) = value {
                            let ret = self.gen_operand(v);
                            self.codes.push(Instr::Mov { dst: Operand::new_register(Register::Rv), src: ret });
                        } else {
                            // A value-less `return` (or falling off the end of a
                            // function) yields `undefined`. Rv still holds the
                            // result of the last call otherwise, which would make
                            // a constructor "return" that leftover object.
                            self.codes.push(Instr::Mov { dst: Operand::new_register(Register::Rv), src: Operand::new_primitive(crate::bytecode::Primitive::Undefined) });
                        }

                        self.codes.push(Instr::Ret {});
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
                            // 先读目标块，再写偏移：两处都借用 `this`，不能同时持有。
                            let target = match &this.codes[pos] {
                                Instr::Jump { offset } => offset.as_immd(),
                                other => unreachable!("jump patch site holds {other}"),
                            };
                            let absolute = this.block_map[&target] - pos as isize;
                            if let Instr::Jump { offset } = &mut this.codes[pos] {
                                offset.set(absolute);
                            }
                        }));

                        self.codes.push(Instr::Jump { offset: RelPc::new(dst) });
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
                            let (true_op, false_op) = match &this.codes[pos] {
                                Instr::BrIf {
                                    true_target,
                                    false_target,
                                    ..
                                } => (true_target.as_immd(), false_target.as_immd()),
                                other => unreachable!("br_if patch site holds {other}"),
                            };
                            let true_off = this.block_map[&true_op] - pos as isize;
                            let false_off = this.block_map[&false_op] - pos as isize;
                            if let Instr::BrIf {
                                true_target,
                                false_target,
                                ..
                            } = &mut this.codes[pos]
                            {
                                true_target.set(true_off);
                                false_target.set(false_off);
                            }
                        }));

                        self.codes.push(Instr::BrIf {
                            condition: condition,
                            true_target: RelPc::new(true_blk),
                            false_target: RelPc::new(false_blk),
                        });
                    }
                    Instruction::Halt { value } => {
                        if let Some(v) = value {
                            let src = self.gen_operand(v);
                            self.codes.push(Instr::Mov { dst: Operand::new_register(Register::Rv), src: src });
                        }
                        self.codes.push(Instr::Halt {});
                    }
                    Instruction::PushSeh {
                        handler,
                        finally,
                        body,
                    } => {
                        let handler_id = handler.as_usize() as isize;
                        let pos = self.codes.len();
                        // 记下这条区域的声明；`end` 与处理器地址都用 `block_map` 解析，
                        // 所以这里不需要等回填（回填仍照旧写进 `Try` 的操作数）。
                        self.eh_decls.push(EhDecl {
                            body: body.as_usize() as isize,
                            try_pc: pos as isize,
                            handler: handler_id,
                            finally: finally.map(|blk| blk.as_usize() as isize),
                        });
                        patchs.push(Box::new(move |this: &mut Self| {
                            // 偏移先算出来，再一次性写回：`Try` 现在只有两个字段，
                            // 旧编码里那个第 3 槽是纯填充。
                            let catch_off = this.block_map[&handler_id] - pos as isize;
                            let finally_off = finally
                                .map(|blk| this.block_map[&(blk.as_usize() as isize)] - pos as isize);
                            if let Instr::Try {
                                catch_offset,
                                finally_offset,
                            } = &mut this.codes[pos]
                            {
                                catch_offset.set(catch_off);
                                if let Some(finally_off) = finally_off {
                                    finally_offset.set(finally_off);
                                }
                            }
                        }));
                        self.codes.push(Instr::Try {
                            catch_offset: RelPc::immediate(0),
                            finally_offset: RelPc::immediate(0),
                        });
                    }
                    Instruction::PopSeh { region } => {
                        // 结构化配对：记下"这条 `EndTry` 关的是哪条区域"。
                        // **不再**靠扫指令流 + 深度计数推断（那条规则被实测证伪）。
                        self.eh_exits
                            .push((region.as_usize() as isize, self.codes.len() as isize));
                        self.codes.push(Instr::EndTry {});
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
                        self.codes.push(Instr::ThrowExc { value: val });
                    }
                    Instruction::LoadException { dst } => {
                        let reg = self.gen_operand(dst);
                        self.codes
                            .push(Instr::LoadException { dst: reg });
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
                        self.codes.push(Instr::ResumeExc {});
                    }
                    Instruction::DelayedJump { target, seh_depth } => {
                        let target_op = Operand::new_immd(target.as_usize() as isize);
                        let seh_depth_op = Operand::new_immd(seh_depth as isize);

                        let pos = self.codes.len();
                        patchs.push(Box::new(move |this: &mut Self| {
                            let target = match &this.codes[pos] {
                                Instr::DelayedJump { target, .. } => target.as_immd() as usize,
                                other => unreachable!("delayed-jump patch site holds {other}"),
                            };
                            // The target is consumed later by `ResumeExc`, at the
                            // end of the finally block, so it must be an absolute
                            // PC: a pc-relative offset would be applied from the
                            // wrong instruction.
                            let absolute = this.block_map[&(target as isize)];
                            if let Instr::DelayedJump { target, .. } = &mut this.codes[pos] {
                                target.set(absolute);
                            }
                        }));

                        self.codes.push(Instr::DelayedJump {
                            target: AbsPc::new(target_op),
                            seh_depth: seh_depth_op,
                        });
                    }
                }

                self.inst_index += 1;
            }
        }

        for patch in patchs {
            patch(self);
        }

        self.eh_regions = self.resolve_eh_regions();

        &self.codes
    }

    /// EH 区域表：把声明里的块 id 解析成 pc。**这是"哪条 `EndTry` 收哪条 `Try`"的唯一出口。**
    fn resolve_eh_regions(&self) -> Vec<EhRegion> {
        let pc_of = |blk: isize| -> usize {
            *self
                .block_map
                .get(&blk)
                .unwrap_or_else(|| panic!("EH 声明引用了没有布局的块 {blk}")) as usize
        };
        self.eh_decls
            .iter()
            .map(|decl| {
                let mut exits: Vec<usize> = self
                    .eh_exits
                    .iter()
                    .filter(|(body, _)| *body == decl.body)
                    .map(|(_, pc)| *pc as usize)
                    .collect();
                exits.sort_unstable();
                EhRegion {
                    start: decl.try_pc as usize,
                    exits,
                    catch: Some(pc_of(decl.handler)),
                    finally: decl.finally.map(pc_of),
                }
            })
            .collect()
    }

    /// 本次代码生成产出的 EH 区域表（`generate_code` 之后才有内容）。
    pub fn eh_regions(&self) -> &[EhRegion] {
        &self.eh_regions
    }

    fn gen_call(&mut self, func: Value, args: &[Value], result: Value) {
        // 1. Backup used registers
        let in_use_registers = self.call_saved_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Instr::Push { src: reg.into() });
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 3. Set up new stack frame
        self.codes.push(Instr::PushC { src: Operand::new_register(Register::Rbp) });

        // 4. Call the function (the argument count travels along so the callee
        //    can materialise `arguments`).
        self.codes.push(Instr::Call { func: func.to_operand(), argc: Operand::new_immd(args.len() as isize) });

        // 5. Restore stack pointer (reset to current base pointer)
        self.codes.push(Instr::MovC { dst: Operand::new_register(Register::Rsp), src: Operand::new_register(Register::Rbp) });

        // 6. Pop the saved base pointer
        self.codes
            .push(Instr::PopC { dst: Register::Rbp.into() });

        // 7. Clean up arguments from the stack
        self.codes.push(Instr::SubC { dst: Operand::Register(Register::Rsp), src: Operand::Register(Register::Rsp), value: Operand::new_immd(args.len() as isize) });

        // 8. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Instr::Pop { dst: reg.into() });
        }

        // 9. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Instr::Mov { dst: result_reg, src: Operand::new_register(Register::Rv) });
    }

    fn gen_call_ex(&mut self, func: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(func);

        // 1. Backup used registers
        let in_use_registers = self.call_saved_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Instr::Push { src: reg.into() });
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 3. Set up new stack frame
        self.codes.push(Instr::PushC { src: Operand::new_register(Register::Rbp) });

        // 4. Call the function (CallEx). The argument count travels with the
        //    instruction so the VM can read the pushed arguments when the
        //    callee turns out to be a native built-in.
        self.codes.push(Instr::CallEx { callee: callable, argc: Operand::new_immd(args.len() as isize) });

        // 5. Restore stack pointer to current base pointer
        self.codes.push(Instr::MovC { dst: Operand::new_register(Register::Rsp), src: Operand::new_register(Register::Rbp) });

        // 6. Pop saved base pointer
        self.codes
            .push(Instr::PopC { dst: Register::Rbp.into() });

        // 7. Clean up arguments from the stack
        self.codes.push(Instr::SubC { dst: Operand::Register(Register::Rsp), src: Operand::Register(Register::Rsp), value: Operand::new_immd(args.len() as isize) });

        // 8. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Instr::Pop { dst: reg.into() });
        }

        // 9. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Instr::Mov { dst: result_reg, src: Operand::new_register(Register::Rv) });
    }

    fn gen_call_native(&mut self, func: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(func);

        // 1. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 2. Set up new stack frame
        self.codes.push(Instr::PushC { src: Operand::new_register(Register::Rbp) });

        // 3. Call the native function
        self.codes.push(Instr::CallNative { callee: callable, argc: Operand::new_immd(args.len() as isize) });

        // 4. Restore stack pointer to current base pointer
        self.codes.push(Instr::MovC { dst: Operand::new_register(Register::Rsp), src: Operand::new_register(Register::Rbp) });

        // 5. Pop saved base pointer
        self.codes
            .push(Instr::PopC { dst: Register::Rbp.into() });

        // 6. Clean up arguments from the stack
        self.codes.push(Instr::SubC { dst: Operand::Register(Register::Rsp), src: Operand::Register(Register::Rsp), value: Operand::new_immd(args.len() as isize) });

        // 7. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Instr::Mov { dst: result_reg, src: Operand::new_register(Register::Rv) });
    }

    fn gen_prop_call(&mut self, object: Value, property: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(object);

        // 1. Backup used registers
        let in_use_registers = self.call_saved_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Instr::Push { src: reg.into() });
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 2. Set up new stack frame
        self.codes.push(Instr::PushC { src: Operand::new_register(Register::Rbp) });

        let prop = self.gen_operand(property);
        // 3. Call method on the object
        self.codes.push(Instr::CallMethod { callee: callable, property: prop, argc: Operand::new_immd(args.len() as isize) });

        // 4. Restore stack pointer to current base pointer
        self.codes.push(Instr::MovC { dst: Operand::new_register(Register::Rsp), src: Operand::new_register(Register::Rbp) });

        // 5. Pop saved base pointer
        self.codes
            .push(Instr::PopC { dst: Register::Rbp.into() });

        // 6. Clean up arguments from the stack
        self.codes.push(Instr::SubC { dst: Operand::Register(Register::Rsp), src: Operand::Register(Register::Rsp), value: Operand::new_immd(args.len() as isize) });

        // 7. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Instr::Pop { dst: reg.into() });
        }

        // 8. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Instr::Mov { dst: result_reg, src: Operand::new_register(Register::Rv) });
    }

    fn gen_new(&mut self, constructor: Value, args: &[Value], result: Value) {
        let callable = self.gen_operand(constructor);

        // 1. Backup used registers
        let in_use_registers = self.call_saved_registers();
        for reg in in_use_registers.iter().copied() {
            self.codes.push(Instr::Push { src: reg.into() });
        }

        // 2. Push arguments onto the stack
        self.store_args(args, self.inst_index);

        // 3. Set up new stack frame
        self.codes.push(Instr::PushC { src: Operand::new_register(Register::Rbp) });

        // 4. Call the constructor with New opcode (passing arg count)
        self.codes.push(Instr::New { callee: callable, argc: Operand::new_immd(args.len() as isize) });

        // 5. Restore stack pointer to current base pointer
        self.codes.push(Instr::MovC { dst: Operand::new_register(Register::Rsp), src: Operand::new_register(Register::Rbp) });

        // 6. Pop saved base pointer
        self.codes
            .push(Instr::PopC { dst: Register::Rbp.into() });

        // 7. Clean up arguments from the stack
        self.codes.push(Instr::SubC { dst: Operand::Register(Register::Rsp), src: Operand::Register(Register::Rsp), value: Operand::new_immd(args.len() as isize) });

        // 8. Restore backed-up registers
        for reg in in_use_registers.iter().rev().copied() {
            self.codes.push(Instr::Pop { dst: reg.into() });
        }

        // 9. Move return value to destination register
        let result_reg = self.gen_operand(result);
        self.codes.push(Instr::Mov { dst: result_reg, src: Operand::new_register(Register::Rv) });
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
            self.codes.push(Instr::Mov { dst: Operand::Stack(stack as isize), src: arg_op });
            // The slot now holds the incoming value; any register copy is stale,
            // so the block entry must reload from memory.
            self.reg_alloc.mark_stack_written(*param);
            self.reg_alloc.forget_register(param);
        }
    }

    fn store_args(&mut self, args: &[Value], index: usize) {
        for arg in args.iter().rev() {
            let op = self.gen_operand(*arg);
            self.codes.push(Instr::Push { src: op });
            if let Value::Variable(arg) = arg
                && let Some(action) = self.reg_alloc.release(*arg, index)
            {
                match action {
                    Action::Spill { stack, register } => {
                        trace!("spilling({arg}) {register} -> [rbp+{stack}]");
                        self.codes.push(Instr::Mov { dst: Operand::Stack(stack as isize), src: register.into() });
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
                    self.codes.push(Instr::Mov { dst: Operand::Stack(stack as isize), src: register.into() });
                }
                Action::Restore { register, stack } => {
                    trace!("unspilling [rbp+{stack}] -> {register}");
                    self.codes.push(Instr::Mov { dst: register.into(), src: Operand::Stack(stack as isize) });
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
    fn emit_code(&mut self, code: Instr) {
        self.codes.push(code);
    }

    pub fn debug_insts(&self) -> &BTreeMap<usize, Instruction> {
        &self.insts
    }
}
