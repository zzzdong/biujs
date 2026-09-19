use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::Hash,
};

use log::trace;

use super::ir::{Block, BlockId, ControlFlowGraph, Instruction};
use crate::{
    bytecode::{MIN_REQUIRED_REGISTER, Register},
    compiler::ir::{cfg::BlockLayout, instruction::Variable},
};

#[derive(Debug, Clone)]
pub struct LiveRange {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone)]
pub struct LiveInterval {
    var: Variable,
    start: usize,
    end: usize,
    ranges: Vec<LiveRange>,
    reg: Option<Register>,
    stack: Option<usize>,
}

impl LiveInterval {
    pub fn new(var: Variable) -> Self {
        LiveInterval {
            var,
            start: usize::MAX,
            end: 0,
            ranges: Vec::new(),
            reg: None,
            stack: None,
        }
    }

    #[track_caller]
    pub fn active(&mut self, index: usize) {
        log::trace!(
            "---> active {:?}: {index}, from {}",
            self.var,
            std::panic::Location::caller()
        );

        self.start = self.start.min(index);
        self.end = self.end.max(index);

        if self.ranges.is_empty() {
            self.ranges.push(LiveRange {
                start: index,
                end: index,
            });

            return;
        }

        let mut done = false;
        for range in self.ranges.iter_mut() {
            if range.end == index - 1 {
                range.end = range.end.max(index);
                done = true;
                break;
            }
        }

        if done {
            return;
        }

        self.ranges.push(LiveRange {
            start: index,
            end: index,
        });
    }

    pub fn update_end(&mut self, index: usize) {
        self.end = self.end.max(index);
        // 更新最后一个范围的结束位置
        if let Some(last) = self.ranges.last_mut()
            && index > last.end
        {
            last.end = index;
        }
    }

    pub fn merge(&mut self, other: &LiveInterval) {
        self.start = self.start.min(other.start);
        self.end = self.end.max(other.end);

        self.ranges.extend(other.ranges.clone());
    }

    /// Whether `index` falls within one of this interval's live ranges.
    pub fn covers(&self, index: usize) -> bool {
        self.ranges
            .iter()
            .any(|r| index >= r.start && index <= r.end)
    }
}

#[derive(Debug, Clone)]
pub struct Liveness {
    intervals: HashMap<Variable, LiveInterval>,
}

impl Liveness {
    fn new() -> Self {
        Liveness {
            intervals: HashMap::new(),
        }
    }

    /// All live intervals in a **deterministic** order.
    ///
    /// `intervals` is a `HashMap`, so `values()` yields a different order on
    /// every process run. Register allocation depends on that order, which made
    /// code generation (and therefore program behaviour) non-deterministic.
    /// Sorting by variable id pins it down.
    fn intervals(&self) -> Vec<LiveInterval> {
        let mut intervals: Vec<LiveInterval> = self.intervals.values().cloned().collect();
        intervals.sort_by_key(|interval| interval.var.as_usize());
        intervals
    }

    fn set_register(&mut self, var: Variable, reg: Register) {
        self.intervals.get_mut(&var).unwrap().reg.replace(reg);
    }

    fn set_stack(&mut self, var: Variable, stack: usize) {
        self.intervals.get_mut(&var).unwrap().stack.replace(stack);
    }

    fn stack_size(&self) -> usize {
        let stacks = self
            .intervals
            .values()
            .filter(|interval| interval.stack.is_some());

        if stacks.clone().count() == 0 {
            return 0;
        }

        stacks
            .map(|interval| interval.stack.unwrap())
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Allocate a fresh stack slot index for a spilled variable.
    fn new_stack_slot(&self) -> usize {
        self.stack_size()
    }
}

pub struct LiveIntervalAnalyzer {}

impl LiveIntervalAnalyzer {
    pub fn scan(cfg: &ControlFlowGraph, block_layout: &BlockLayout) -> Liveness {
        // 1. 遍历一次控制流图，生成基本存活区间
        let mut liveness = Self::build_basic_intervals(cfg, block_layout);

        // 第二遍：计算live_in和live_out集合
        let (_live_in_sets, live_out_sets) = Self::compute_liveness_sets(cfg, block_layout);

        // 第三遍：更新变量存活周期
        Self::update_intervals(cfg, block_layout, &live_out_sets, &mut liveness);

        liveness
    }

    /// 第一遍：扫描所有指令，建立基本的LiveInterval
    fn build_basic_intervals(cfg: &ControlFlowGraph, block_layout: &BlockLayout) -> Liveness {
        let mut liveness = Liveness::new();
        let mut index = 0;

        for block in block_layout.iter(cfg) {
            let block_start = index;
            // 处理块参数（Phi参数）：它们在块开始时就是活跃的
            for param in block.params() {
                liveness
                    .intervals
                    .entry(*param)
                    .or_insert(LiveInterval::new(*param))
                    .active(block_start);
            }

            for inst in block.instructions().iter() {
                match inst {
                    Instruction::Jump { dst, args } => {
                        let params = cfg
                            .get_block(dst.to_block())
                            .expect("no such block")
                            .params();
                        for (_arg, param) in args.iter().zip(params.iter()) {
                            liveness
                                .intervals
                                .entry(*param)
                                .or_insert(LiveInterval::new(*param))
                                .active(index);
                        }
                    }
                    Instruction::BrIf {
                        condition: _,
                        true_blk,
                        false_blk,
                        true_args,
                        false_args,
                    } => {
                        let true_params = cfg
                            .get_block(true_blk.to_block())
                            .expect("no such block")
                            .params();
                        for (_arg, param) in true_args.iter().zip(true_params.iter()) {
                            liveness
                                .intervals
                                .entry(*param)
                                .or_insert(LiveInterval::new(*param))
                                .active(index);
                        }

                        let false_params = cfg
                            .get_block(false_blk.to_block())
                            .expect("no such block")
                            .params();
                        for (_arg, param) in false_args.iter().zip(false_params.iter()) {
                            liveness
                                .intervals
                                .entry(*param)
                                .or_insert(LiveInterval::new(*param))
                                .active(index);
                        }
                    }
                    _ => {}
                }

                Self::process_instruction(&mut liveness, index, inst);

                index += 1;
            }
        }

        liveness
    }

    /// 处理指令中的变量，更新LiveInterval
    fn process_instruction(liveness: &mut Liveness, index: usize, inst: &Instruction) {
        let (defined, used) = inst.defined_and_used_vars();

        // 处理使用的变量（在定义之前处理使用）
        for var in used {
            liveness
                .intervals
                .entry(var)
                .or_insert(LiveInterval::new(var))
                .active(index);
        }

        // 处理定义的变量
        for var in defined {
            liveness
                .intervals
                .entry(var)
                .or_insert(LiveInterval::new(var))
                .active(index);
        }
    }

    /// 第二遍：计算每个块的live_in和live_out集合
    fn compute_liveness_sets(
        cfg: &ControlFlowGraph,
        block_layout: &BlockLayout,
    ) -> (
        HashMap<BlockId, HashSet<Variable>>,
        HashMap<BlockId, HashSet<Variable>>,
    ) {
        let mut live_in_sets = HashMap::new();
        let mut live_out_sets = HashMap::new();
        let mut changed = true;

        // 初始化每个块的live_in和live_out为空集合
        for block in block_layout.iter(cfg) {
            live_in_sets.insert(block.id(), HashSet::new());
            live_out_sets.insert(block.id(), HashSet::new());
        }

        while changed {
            changed = false;

            // 从后向前遍历基本块
            for block in block_layout.iter_rev(cfg) {
                // live_out(B) = ∪ live_in(successors)。这里直接使用后继块的
                // live_in；后继块的块参数在计算其 live_in 时已被移除，因此不会
                // 泄漏到本块的 live_out 中。
                let new_live_out = Self::compute_block_liveness(cfg, block, &live_in_sets);

                // live_in(B) = (live_out - defs) ∪ uses —— 从后向前扫描指令，
                // 在 live_out 的副本上就地修改。之前 live_in 和 live_out 存的是
                // 同一个集合，导致"在本块定义、在后继块使用"的变量区间在定义点
                // 就被截断，寄存器分组据此把实际同时活跃的变量放进同一个寄存器，
                // 造成跨块的值被静默覆盖。
                let mut new_live_in = new_live_out.clone();
                for inst in block.instructions().iter().rev() {
                    let (defined, used) = inst.defined_and_used_vars();

                    // 定义杀掉活跃性
                    for var in defined {
                        new_live_in.remove(&var);
                    }

                    // 使用产生活跃性
                    for var in used {
                        new_live_in.insert(var);
                    }
                }

                // 块参数在块入口处定义（值来自前驱的跳转实参），不属于 live_in。
                for param in block.params() {
                    new_live_in.remove(param);
                }

                // 检查是否有变化
                let old_live_in = live_in_sets.get(&block.id()).unwrap();
                let old_live_out = live_out_sets.get(&block.id()).unwrap();

                if &new_live_in != old_live_in || &new_live_out != old_live_out {
                    changed = true;
                    live_in_sets.insert(block.id(), new_live_in);
                    live_out_sets.insert(block.id(), new_live_out);
                }
            }
        }

        (live_in_sets, live_out_sets)
    }

    /// 计算单个基本块的live_out（从后继块的live_in获取）
    fn compute_block_liveness(
        cfg: &ControlFlowGraph,
        block: &Block,
        live_in_sets: &HashMap<BlockId, HashSet<Variable>>,
    ) -> HashSet<Variable> {
        let mut live_out = HashSet::new();

        // 获取所有后继块
        let successors = cfg.get_successors(block.id());

        // 遍历所有后继块
        for &succ_block_id in successors {
            // 将后继块的live_in中的所有变量添加到当前块的live_out中
            if let Some(succ_live_in) = live_in_sets.get(&succ_block_id) {
                live_out.extend(succ_live_in.iter().cloned());
            }
        }

        live_out
    }

    /// 第三遍：更新变量的存活周期
    fn update_intervals(
        cfg: &ControlFlowGraph,
        block_layout: &BlockLayout,
        live_out_sets: &HashMap<BlockId, HashSet<Variable>>,
        liveness: &mut Liveness,
    ) {
        // 计算每个块的起始和结束索引
        let mut block_starts = Vec::new();
        let mut current_index = 0;

        for block in block_layout.iter(cfg) {
            block_starts.push(current_index);
            current_index += block.instructions().len();
        }

        // 遍历所有块，更新变量的存活周期
        for (block_id, block) in block_layout.iter(cfg).enumerate() {
            let block_start = block_starts[block_id];
            let block_end = block_start + block.instructions().len();

            // 本块内被使用的变量集合（含跳转实参），提前算好避免平方复杂度
            let mut used_here: HashSet<Variable> = HashSet::new();
            for inst in block.instructions() {
                let (_, used) = inst.defined_and_used_vars();
                used_here.extend(used);
            }

            // live_out 中的变量在块尾仍然存活：把它们的区间延伸到块的末尾。
            // 对于"本块定义、后继块使用"的变量，这是其寄存器不被复用的关键。
            if let Some(live_out) = live_out_sets.get(&block.id()) {
                for &var in live_out {
                    if let Some(interval) = liveness.intervals.get_mut(&var) {
                        interval.update_end(block_end);
                    }
                }
            }

            // live-through 变量（live-out 但本块没有出现）同样必须被认为覆盖
            // 本块 —— 它的值在块内一直保存在寄存器里，区间上不能留下"空洞"，
            // 否则分组会认为别的变量可以复用同一个寄存器。
            if let Some(live_out) = live_out_sets.get(&block.id()) {
                let mut to_extend: Vec<Variable> = Vec::new();
                for &var in live_out {
                    if used_here.contains(&var) {
                        continue;
                    }
                    let covers_block = liveness
                        .intervals
                        .get(&var)
                        .map(|iv| {
                            iv.ranges
                                .iter()
                                .any(|r| r.start <= block_start && r.end >= block_end.saturating_sub(1))
                        })
                        .unwrap_or(false);
                    if !covers_block {
                        to_extend.push(var);
                    }
                }
                for var in to_extend {
                    if let Some(interval) = liveness.intervals.get_mut(&var) {
                        interval.ranges.push(LiveRange {
                            start: block_start,
                            end: block_end.saturating_sub(1),
                        });
                        // 保持区间按起点有序：`update_end` 依赖最后一个区间
                        interval.ranges.sort_by_key(|r| r.start);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegAlloc {
    liveness: Liveness,
    pub(super) reg_set: RegisterSet,
    /// Variables whose stack slot is known to hold their current value.
    ///
    /// A spill slot only becomes valid once something writes it. Reloading a
    /// value from a slot that was never written (which happens for variables
    /// the linear scan decided to keep on the stack from the start) yields
    /// `undefined` instead of the real value.
    written_stacks: HashSet<Variable>,
}

impl RegAlloc {
    pub fn new(registers: &[Register]) -> Self {
        if registers.len() <= MIN_REQUIRED_REGISTER {
            panic!("Not enough registers");
        }
        Self {
            liveness: Liveness::new(),
            reg_set: RegisterSet::new(registers),
            written_stacks: HashSet::new(),
        }
    }

    /// Record that `value`'s stack slot now holds its current value.
    pub fn mark_stack_written(&mut self, value: Variable) {
        self.written_stacks.insert(value);
    }

    /// Start a new basic block.
    ///
    /// Register contents cannot be trusted across a block boundary: a block that
    /// is *generated* later may run earlier at run time (loops, shared merge
    /// blocks), and it may reuse the same register. Dropping every register
    /// assignment forces each value to be reloaded from its stack slot, which
    /// `flush_block` keeps up to date.
    pub fn begin_block(&mut self) {
        for reg in self.reg_set.registers.iter_mut() {
            reg.variable = None;
            reg.is_fixed = false;
        }
    }

    /// Write every value that currently lives in a register to its stack slot,
    /// then forget the register assignments.
    ///
    /// Called before a block terminator so that any successor block can recover
    /// the values it needs from memory, whichever path was taken to reach it.
    pub fn flush_block(&mut self) -> Vec<(Register, usize)> {
        let mut spills = Vec::new();
        for reg in self.reg_set.registers.iter() {
            let Some(var) = reg.variable else { continue };
            let stack = match self.liveness.intervals.get(&var).and_then(|iv| iv.stack) {
                Some(stack) => stack,
                None => {
                    let stack = self.liveness.new_stack_slot();
                    self.liveness.set_stack(var, stack);
                    stack
                }
            };
            self.written_stacks.insert(var);
            spills.push((reg.register, stack));
        }
        for reg in self.reg_set.registers.iter_mut() {
            reg.variable = None;
            reg.is_fixed = false;
        }
        spills
    }

    /// Whether `value` can be reloaded from its stack slot.
    fn stack_holds_value(&self, value: &Variable) -> bool {
        self.written_stacks.contains(value)
    }

    pub fn load_arg(&mut self, arg: usize) -> isize {
        0 - ((arg as isize) + 1)
    }

    pub fn in_use_registers(&self) -> Vec<Register> {
        self.reg_set
            .registers
            .iter()
            .filter(|reg| reg.variable.is_some())
            .map(|reg| reg.register)
            .collect()
    }

    /// Registers whose contents must survive a call.
    ///
    /// `in_use_registers` only reports what the allocator *currently* believes
    /// is live. Liveness is computed on the CFG, so a value that is used again
    /// from a different block can be released early; the callee then clobbers
    /// the register and the caller silently reads `undefined`. Saving every
    /// register this function ever assigns cannot lose a value, which matters
    /// far more than the extra pushes around a call.
    pub fn call_saved_registers(&self) -> Vec<Register> {
        let mut regs = self.in_use_registers();
        for interval in self.liveness.intervals.values() {
            if let Some(reg) = interval.reg {
                if !regs.contains(&reg) {
                    regs.push(reg);
                }
            }
        }
        regs.sort_by_key(|reg| reg.as_usize());
        regs
    }

    /// 获取变量被分配的寄存器
    pub fn get_register(&self, value: &Variable) -> Option<Register> {
        for reg_entry in &self.reg_set.registers {
            if reg_entry.variable.as_ref() == Some(value) {
                return Some(reg_entry.register);
            }
        }
        None
    }

    pub fn stack_size(&self) -> usize {
        self.liveness.stack_size()
    }

    /// Stable stack slot for `var`, allocating one on first use.
    ///
    /// Used by the memory-resident code path, where variables are never kept in
    /// registers across instructions (see `Codegen::memory_resident_vars`).
    pub fn ensure_stack_slot(&mut self, var: Variable) -> usize {
        if let Some(stack) = self.liveness.intervals.get(&var).and_then(|iv| iv.stack) {
            return stack;
        }
        let stack = self.liveness.new_stack_slot();
        self.liveness.set_stack(var, stack);
        stack
    }

    pub fn spill_all(&mut self) -> Vec<(Register, usize)> {
        let mut spills = Vec::new();
        for reg_entry in &self.reg_set.registers {
            if let Some(var) = &reg_entry.variable {
                if let Some(stack) = self.get_stack_offset(var) {
                    spills.push((reg_entry.register, stack));
                }
            }
        }
        spills
    }

    /// 获取变量被分配的栈偏移量，如果变量不在栈上则返回None
    pub fn get_stack_offset(&self, value: &Variable) -> Option<usize> {
        self.liveness
            .intervals
            .get(value)
            .and_then(|interval| interval.stack)
    }

    pub fn alloc(&mut self, value: Variable, index: usize) -> (Register, Option<Action>) {
        // Extract the fields we need so the borrow of `self.liveness` is released
        // before we potentially mutate it via `must_alloc`.
        let (preset_reg, stack) = {
            let interval = self.liveness.intervals.get(&value).unwrap();
            (interval.reg, interval.stack)
        };

        match self.reg_set.find(value) {
            Some(register) => (register, None),
            None => match preset_reg {
                Some(register) => {
                    // Re-taking a pre-assigned register evicts whoever holds it
                    // now; that value has to be preserved on the stack first.
                    let spill = self.evict_occupant(register, value, index);
                    self.reg_set.use_register(register, value, true);
                    // The variable owns a stack slot, so it was spilled at some
                    // point (as an allocation victim). Whatever the register
                    // holds now is not necessarily its value, so reload it.
                    // Without this the caller silently reads a stale value.
                    if let Some(stack) = stack
                        && self.stack_holds_value(&value)
                    {
                        return (
                            register,
                            Some(Action::Restore {
                                stack,
                                register,
                            }),
                        );
                    }
                    (register, spill)
                }
                None => {
                    let (reg, spill, victim) =
                        self.reg_set.must_alloc(value, index, &mut self.liveness);

                    // 如果变量在栈上，并且在当前索引处开始一个新的活跃范围，需要从栈恢复
                    if let Some(stack) = stack
                        && self.stack_holds_value(&value)
                        && self
                            .liveness
                            .intervals
                            .get(&value)
                            .map(|iv| iv.ranges.iter().any(|r| r.start == index))
                            .unwrap_or(false)
                    {
                        let restore = Action::Restore {
                            stack,
                            register: reg,
                        };
                        return (reg, Some(restore));
                    }

                    if spill.is_some()
                        && let Some(victim) = victim
                    {
                        // The victim's value is being written to its stack slot
                        // by the emitted spill, so it can be reloaded later.
                        self.written_stacks.insert(victim);
                    }

                    (reg, spill)
                }
            },
        }
    }

    /// Preserve the value currently in `register` before `value` takes it over.
    ///
    /// `use_register` overwrites the occupant without saving it, so any value
    /// that is still needed later would silently turn into whatever the new
    /// owner writes. Spilling it (allocating a slot first if the linear scan
    /// never gave it one) keeps it recoverable.
    fn evict_occupant(
        &mut self,
        register: Register,
        value: Variable,
        _index: usize,
    ) -> Option<Action> {
        let occupant = self.reg_set.occupant(register)?;
        if occupant == value {
            return None;
        }

        let stack = match self.liveness.intervals.get(&occupant).and_then(|iv| iv.stack) {
            Some(stack) => stack,
            None => {
                let stack = self.liveness.new_stack_slot();
                self.liveness.set_stack(occupant, stack);
                stack
            }
        };

        self.reg_set.release(occupant);
        self.written_stacks.insert(occupant);
        Some(Action::Spill { register, stack })
    }

    pub fn release(&mut self, value: Variable, index: usize) -> Option<Action> {
        trace!("releasing {value}");

        let interval = self.liveness.intervals.get(&value).unwrap();
        // 只有当变量在索引处完全结束其活跃周期时才释放
        if interval.end == index
            && let Some(stack) = interval.stack
            && let Some(register) = self.reg_set.release(value)
        {
            let spill = Action::Spill { register, stack };
            // The caller emits `Mov [rbp+stack], register`, so the slot is valid.
            self.written_stacks.insert(value);
            return Some(spill);
        }

        None
    }

    pub fn arrange(&mut self, cfg: &ControlFlowGraph, block_layout: &BlockLayout) {
        self.liveness = LiveIntervalAnalyzer::scan(cfg, block_layout);

        let registers: Vec<Register> = self
            .reg_set
            .registers
            .iter()
            .map(|reg| reg.register)
            .collect();

        let mut intervals = self.liveness.intervals();

        // 按开始时间排序，优先处理长区间
        intervals.sort_by(|a, b| {
            let a_len = a.end - a.start;
            let b_len = b.end - b.start;
            a.start.cmp(&b.start).then(b_len.cmp(&a_len))
        });

        // 1. 分组
        let mut groups: Vec<Vec<LiveInterval>> = Vec::new();
        for interval in intervals {
            let mut placed = false;
            for group in groups.iter_mut() {
                if Self::can_join_group(&interval, group) {
                    group.push(interval.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                groups.push(vec![interval.clone()]);
            }
        }

        // 2. 分配寄存器
        // 2.1 如果组数量不多于可用寄存器数量，则直接分配
        if groups.len() <= registers.len() {
            for (group, reg) in groups.into_iter().zip(registers.iter()) {
                for interval in group {
                    self.liveness.set_register(interval.var, *reg);
                }
            }
            return;
        }

        // 2.2 保留3个临时寄存器，其他的进行优先级分配
        let (_temp_regs, fixed_regs) = registers.split_at(3);

        // 排序
        groups.sort_by(|a, b| {
            let a_len: usize = a.iter().map(|interval| interval.ranges.len()).sum();
            let b_len: usize = b.iter().map(|interval| interval.ranges.len()).sum();
            b_len.cmp(&a_len)
        });

        for (i, group) in groups.iter().enumerate() {
            trace!("Group[{i}]: {group:?}");
        }

        // 2.2.1 分配固定寄存器
        let (fixed_group, temp_group) = groups.split_at(fixed_regs.len());
        for (group, reg) in fixed_group.iter().zip(fixed_regs) {
            for interval in group {
                self.liveness.set_register(interval.var, *reg);
            }
        }

        // 2.2.2 分配临时寄存器，只分配栈上空间，不分配寄存器
        for (i, group) in temp_group.iter().enumerate() {
            for interval in group {
                self.liveness.set_stack(interval.var, i);
            }
        }

        // 验证所有块参数都有分配（寄存器或栈）
        for block in block_layout.iter(cfg) {
            for param in block.params() {
                let interval = self.liveness.intervals.get(param);
                match interval {
                    Some(interval) => {
                        if interval.reg.is_none() && interval.stack.is_none() {
                            trace!(
                                "Warning: phi parameter {:?} has no register or stack allocation",
                                param
                            );
                        }
                    }
                    None => {
                        trace!("Warning: phi parameter {:?} has no live interval", param);
                    }
                }
            }
        }
    }

    fn can_join_group(interval: &LiveInterval, group: &[LiveInterval]) -> bool {
        group
            .iter()
            .all(|existing| !Self::has_overlap(interval, existing))
    }

    fn has_overlap(interval_a: &LiveInterval, interval_b: &LiveInterval) -> bool {
        interval_a.start <= interval_b.end && interval_b.start <= interval_a.end
    }
}

#[derive(Debug, Clone)]
pub(super) struct RegisterSet {
    registers: Vec<RegisterHold>,
}

impl RegisterSet {
    fn new(registers: &[Register]) -> Self {
        let registers = registers
            .iter()
            .map(|addr| RegisterHold::new(*addr))
            .collect();
        Self { registers }
    }

    /// Allocate a register for `value`, spilling an existing live variable to the
    /// stack if no register is free (instead of panicking). Returns the chosen
    /// register, the `Action::Spill` for the victim (if any) and the victim
    /// itself, so callers can track which stack slots hold a live value.
    fn must_alloc(
        &mut self,
        value: Variable,
        index: usize,
        liveness: &mut Liveness,
    ) -> (Register, Option<Action>, Option<Variable>) {
        if let Some(reg) = self.registers.iter_mut().find(|reg| reg.variable.is_none()) {
            reg.variable = Some(value);
            return (reg.register, None, None);
        }

        // No free register: evict a victim. Prefer a register whose current
        // variable is dead at `index` (so spilling it is cheap); otherwise evict
        // any in-use register. The victim is assigned a fresh stack slot and a
        // Spill action is emitted so its value is preserved.
        let victim_var = self
            .registers
            .iter()
            .filter_map(|reg| reg.variable)
            .find(|var| {
                liveness
                    .intervals
                    .get(var)
                    .map(|iv| !iv.covers(index))
                    .unwrap_or(false)
            })
            .or_else(|| self.registers.iter().filter_map(|reg| reg.variable).next());

        let victim_var = match victim_var {
            Some(v) => v,
            None => {
                // Should be unreachable: a register is always occupied here.
                let reg = self.registers.iter_mut().next().unwrap();
                reg.variable = Some(value);
                return (reg.register, None, None);
            }
        };

        let slot = liveness.new_stack_slot();
        liveness.set_stack(victim_var, slot);
        let victim_reg = self
            .registers
            .iter()
            .find(|reg| reg.variable == Some(victim_var))
            .unwrap()
            .register;
        // The victim register is now reused by the new value.
        self.registers
            .iter_mut()
            .find(|reg| reg.register == victim_reg)
            .unwrap()
            .variable = Some(value);

        (
            victim_reg,
            Some(Action::Spill {
                register: victim_reg,
                stack: slot,
            }),
            Some(victim_var),
        )
    }

    fn release(&mut self, value: Variable) -> Option<Register> {
        match self
            .registers
            .iter_mut()
            .find(|reg| reg.variable == Some(value))
        {
            Some(reg) => {
                reg.variable = None;
                Some(reg.register)
            }
            None => None,
        }
    }

    fn use_register(&mut self, register: Register, variable: Variable, is_fixed: bool) {
        for reg in self.registers.iter_mut() {
            if reg.register == register {
                reg.variable = Some(variable);
                reg.is_fixed = is_fixed;
                return;
            }
        }
    }

    /// The variable currently held by `register`, if any.
    fn occupant(&self, register: Register) -> Option<Variable> {
        self.registers
            .iter()
            .find(|reg| reg.register == register)
            .and_then(|reg| reg.variable)
    }

    fn find(&self, variable: Variable) -> Option<Register> {
        self.registers
            .iter()
            .find(|reg| reg.variable == Some(variable))
            .map(|reg| reg.register)
    }
}

impl fmt::Display for RegisterSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for register in self.registers.iter() {
            match register.variable {
                Some(var) => write!(f, "{var}"),
                None => write!(f, "-"),
            }?;
            write!(f, "\t|")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RegisterHold {
    register: Register,
    variable: Option<Variable>,
    is_fixed: bool,
}

impl RegisterHold {
    fn new(register: Register) -> Self {
        Self {
            register,
            variable: None,
            is_fixed: false,
        }
    }
}

#[derive(Debug)]
pub enum Action {
    Restore { stack: usize, register: Register },
    Spill { register: Register, stack: usize },
}
