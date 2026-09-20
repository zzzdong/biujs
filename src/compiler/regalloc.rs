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
    /// `live_out` set of every block: values a successor still needs after the
    /// block's terminator. Used to spill exactly those values to memory at a
    /// block boundary.
    live_out: HashMap<BlockId, HashSet<Variable>>,
}

impl Liveness {
    fn new() -> Self {
        Liveness {
            intervals: HashMap::new(),
            live_out: HashMap::new(),
        }
    }

    /// Values a successor still needs once `block` has finished.
    fn live_out_of(&self, block: BlockId) -> Option<&HashSet<Variable>> {
        self.live_out.get(&block)
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

    /// Mark `var` as live at `index`, creating its interval on first sight.
    fn record(&mut self, var: Variable, index: usize) {
        self.intervals
            .entry(var)
            .or_insert_with(|| LiveInterval::new(var))
            .active(index);
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
        // Compute per-block live_in/live_out over the whole CFG (including the
        // exception edges), then turn them into per-variable live intervals.
        let (_live_in_sets, live_out_sets) = Self::compute_liveness_sets(cfg, block_layout);
        Self::build_intervals(cfg, block_layout, &live_out_sets)
    }

    /// Build one live interval per variable.
    ///
    /// An interval must cover **every** instruction index at which the value is
    /// needed, no matter which basic block that instruction lives in. The
    /// previous implementation approximated this with a forward scan plus a
    /// partial "extend for live-out" pass, which left holes across block
    /// boundaries: a value still needed by a successor could look dead, so the
    /// allocator was free to hand its register to another value and silently
    /// overwrite it.
    ///
    /// Instead we run a proper backward liveness scan over each block, seeded
    /// with its `live_out` set, and record the value at exactly the indices
    /// where it is live. The resulting ranges are precise inside a block and
    /// complete across blocks.
    fn build_intervals(
        cfg: &ControlFlowGraph,
        block_layout: &BlockLayout,
        live_out_sets: &HashMap<BlockId, HashSet<Variable>>,
    ) -> Liveness {
        let mut liveness = Liveness::new();
        liveness.live_out = live_out_sets.clone();
        let mut index = 0usize;

        for block in block_layout.iter(cfg) {
            let block_start = index;
            let len = block.instructions().len();
            let block_out: HashSet<Variable> = live_out_sets
                .get(&block.id())
                .cloned()
                .unwrap_or_default();

            // Values live on exit seed the backward walk: they must stay live up
            // to the terminator, whichever successor runs next.
            let mut live = block_out.clone();

            // Block parameters are defined at the block entry by the
            // predecessors' jump arguments, so they are live from the start.
            for param in block.params() {
                live.insert(*param);
            }

            // `live_at[i]` = variables live *before* instruction `i` runs, plus
            // the variables `i` defines (a definition needs a register too).
            let mut live_at: Vec<HashSet<Variable>> = vec![HashSet::new(); len];

            for (local, inst) in block.instructions().iter().enumerate().rev() {
                let (defined, used) = inst.defined_and_used_vars();

                // Destination parameters of a jump are live at the jump itself:
                // codegen emits `mov param_reg, arg` there, so `param_reg` must
                // not be shared with a value still needed at that point.
                for param in Self::consumed_params(cfg, block.id(), inst) {
                    live.insert(param);
                }

                for var in &defined {
                    live.remove(var);
                }
                for var in &used {
                    live.insert(*var);
                }

                let mut at = live.clone();
                at.extend(defined.iter().copied());
                live_at[local] = at;
            }

            for (local, vars) in live_at.iter().enumerate() {
                let at = block_start + local;
                for var in vars {
                    liveness.record(*var, at);
                }
            }

            for param in block.params() {
                liveness.record(*param, block_start);
            }

            // Live-out values have to survive the whole block even if no
            // instruction mentions them (e.g. an empty block).
            let block_last = block_start + len.saturating_sub(1);
            for var in &block_out {
                liveness.record(*var, block_last);
            }

            index += len;
        }

        liveness
    }

    /// Block parameters that the given jump instruction writes.
    ///
    /// These are the values handed over across the edge; they must be live at
    /// the jump so their destination register is reserved at that point.
    fn consumed_params(
        cfg: &ControlFlowGraph,
        block_id: BlockId,
        inst: &Instruction,
    ) -> Vec<Variable> {
        fn params_of(cfg: &ControlFlowGraph, block_id: BlockId) -> Vec<Variable> {
            cfg.get_block(block_id)
                .map(|block| block.params().to_vec())
                .unwrap_or_default()
        }

        let mut params = Vec::new();
        match inst {
            Instruction::Jump { dst, .. } => {
                if let Some(target) = dst.as_block() {
                    params.extend(params_of(cfg, target));
                }
            }
            Instruction::BrIf {
                true_blk,
                false_blk,
                ..
            } => {
                if let Some(target) = true_blk.as_block() {
                    params.extend(params_of(cfg, target));
                }
                if let Some(target) = false_blk.as_block() {
                    params.extend(params_of(cfg, target));
                }
            }
            Instruction::DelayedJump { target, .. } => {
                params.extend(params_of(cfg, *target));
            }
            // Exception edges carry the handler's parameters.
            Instruction::Throw { .. } | Instruction::ResumeException { .. } => {
                for &successor in cfg.get_successors(block_id) {
                    params.extend(params_of(cfg, successor));
                }
            }
            _ => {}
        }

        params
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
    /// Register contents cannot be trusted across a block boundary. Code
    /// generation walks the blocks in layout order, but at run time any
    /// predecessor may run, and a value that this pass believes sits in a
    /// register may never have been written on the path that was actually taken
    /// (or may have been written by a sibling block that never executed).
    /// Dropping every register assignment makes each block re-establish the
    /// values it needs from memory — `spill_live_out` keeps that memory current.
    pub fn begin_block(&mut self) {
        for reg in self.reg_set.registers.iter_mut() {
            reg.variable = None;
            reg.is_fixed = false;
        }
    }

    /// Write the values a successor still needs (`live_out`) to their stack
    /// slots, so the next block can reload them.
    ///
    /// Called before a block terminator. Only values that are actually live
    /// across the boundary are written; values that die inside the block are
    /// left alone. This is the memory side of the block-boundary hand-off that
    /// `begin_block` reloads from.
    pub fn spill_live_out(&mut self, block: BlockId) -> Vec<(Register, usize)> {
        let Some(live_out) = self.liveness.live_out_of(block).cloned() else {
            return Vec::new();
        };

        let holders: Vec<(Register, Variable)> = self
            .reg_set
            .registers
            .iter()
            .filter_map(|reg| reg.variable.map(|var| (reg.register, var)))
            .filter(|(_, var)| live_out.contains(var))
            .collect();

        let mut spills = Vec::new();
        for (register, var) in holders {
            let stack = match self.liveness.intervals.get(&var).and_then(|iv| iv.stack) {
                Some(stack) => stack,
                None => {
                    let stack = self.liveness.new_stack_slot();
                    self.liveness.set_stack(var, stack);
                    stack
                }
            };
            self.written_stacks.insert(var);
            spills.push((register, stack));
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

    /// Forget that `value` lives in a register.
    ///
    /// Used when a value is handed over through memory (a block parameter at a
    /// jump): the register no longer holds the current value, so the reading
    /// block must reload it from its stack slot instead of trusting whatever the
    /// register happens to contain.
    pub fn forget_register(&mut self, value: &Variable) {
        self.reg_set.release(*value);
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

    /// 获取变量被分配的栈偏移量，如果变量不在栈上则返回None
    pub fn get_stack_offset(&self, value: &Variable) -> Option<usize> {
        self.liveness
            .intervals
            .get(value)
            .and_then(|interval| interval.stack)
    }

    /// Make `value` available in a register at `index`.
    ///
    /// Returns the register plus the memory bookkeeping the caller has to emit,
    /// **in order**. Up to two actions can be required: first the spill of the
    /// value evicted from the register, then the reload of `value` itself from
    /// its stack slot. The original code returned a single action and dropped
    /// the evicted value's spill whenever a reload was also needed — the victim
    /// was marked as "spilled" without the store ever being emitted, so a value
    /// that was still live got reloaded as whatever stale content the slot held.
    pub fn alloc(&mut self, value: Variable, index: usize) -> (Register, Vec<Action>) {
        // Extract the fields we need so the borrow of `self.liveness` is released
        // before we potentially mutate it via `must_alloc`.
        let (preset_reg, stack) = {
            let interval = self.liveness.intervals.get(&value).unwrap();
            (interval.reg, interval.stack)
        };

        if let Some(register) = self.reg_set.find(value) {
            return (register, Vec::new());
        }

        let mut actions = Vec::new();

        let register = match preset_reg {
            Some(register) => {
                // Re-taking a pre-assigned register evicts whoever holds it
                // now; that value has to be preserved on the stack first.
                if let Some((occupant, spill)) = self.evict_occupant(register, value, index) {
                    self.written_stacks.insert(occupant);
                    actions.push(spill);
                }
                self.reg_set.use_register(register, value, true);
                register
            }
            None => {
                let (register, spill, victim) =
                    self.reg_set.must_alloc(value, index, &mut self.liveness);
                if let Some(spill) = spill {
                    if let Some(victim) = victim {
                        // The victim's value is written to its stack slot by the
                        // spill below, so it can be reloaded later.
                        self.written_stacks.insert(victim);
                    }
                    actions.push(spill);
                }
                register
            }
        };

        // The value owns a stack slot, so it was spilled at some point (as an
        // allocation victim) and the register does not necessarily hold it.
        // Reload it from memory. The spill above (if any) is emitted first, so
        // the evicted value is safe before this register is overwritten.
        if let Some(stack) = stack
            && self.stack_holds_value(&value)
        {
            actions.push(Action::Restore { stack, register });
        }

        (register, actions)
    }

    /// Preserve the value currently in `register` before `value` takes it over.
    ///
    /// `use_register` overwrites the occupant without saving it, so any value
    /// that is still needed later would silently turn into whatever the new
    /// owner writes. Spilling it (allocating a slot first if the linear scan
    /// never gave it one) keeps it recoverable.
    ///
    /// The caller is responsible for emitting the returned spill and only then
    /// recording the occupant's slot as valid.
    fn evict_occupant(
        &mut self,
        register: Register,
        value: Variable,
        _index: usize,
    ) -> Option<(Variable, Action)> {
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
        Some((occupant, Action::Spill { register, stack }))
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
        } else {
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
        }

        // 3. Block parameters cross a block boundary instead of being produced
        //    by an instruction inside the block: every predecessor hands the
        //    value over, and the block entry consumes it. Give each parameter a
        //    dedicated stack slot and treat the slot as written — the hand-off
        //    always goes through memory (`Codegen::store_jump_args`), so the
        //    entry can always reload it, no matter which predecessor ran or in
        //    which order the blocks are generated.
        for block in block_layout.iter(cfg) {
            for param in block.params() {
                self.ensure_stack_slot(*param);
                self.mark_stack_written(*param);
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

        // A spilled value must keep *one* stack slot for its whole life: block
        // parameters are written through that slot by every predecessor and
        // reloaded by the block entry, so re-slotting a victim here would make
        // one edge write a different location than the one that is read.
        let slot = match liveness.intervals.get(&victim_var).and_then(|iv| iv.stack) {
            Some(slot) => slot,
            None => {
                let slot = liveness.new_stack_slot();
                liveness.set_stack(victim_var, slot);
                slot
            }
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::ir::Value;

    fn var(v: Variable) -> Value {
        Value::Variable(v)
    }

    /// A value produced in one block and consumed in a later block must have
    /// every index in between covered by its live interval. The old analysis
    /// built intervals from def/use points only and left the live-through
    /// indices uncovered, so the allocator could treat such a value as dead and
    /// reuse its register.
    #[test]
    fn cross_block_interval_covers_every_live_index() {
        let mut cfg = ControlFlowGraph::new();
        let b0 = cfg.create_block("b0");
        let b1 = cfg.create_block("b1");
        let b2 = cfg.create_block("b2");
        cfg.set_entry(b0);

        let v0 = cfg.create_variable().to_variable();
        let v1 = cfg.create_variable().to_variable();

        // b0: v0 is produced, then control flows on (indices 0, 1).
        cfg.switch_to_block(b0);
        cfg.emit(Instruction::MakeArray { dst: var(v0) });
        cfg.emit(Instruction::Jump {
            dst: Value::Block(b1),
            args: vec![],
        });

        // b1: v0 is live through this block but never mentioned (index 2).
        cfg.switch_to_block(b1);
        cfg.emit(Instruction::Jump {
            dst: Value::Block(b2),
            args: vec![],
        });

        // b2: v0 is finally consumed (indices 3, 4).
        cfg.switch_to_block(b2);
        cfg.emit(Instruction::Move {
            dst: var(v1),
            src: var(v0),
        });
        cfg.emit(Instruction::Return {
            value: Some(var(v1)),
        });

        let layout = cfg.loop_root_reverse_postorder_layout2();
        let liveness = LiveIntervalAnalyzer::scan(&cfg, &layout);
        let interval = liveness
            .intervals
            .get(&v0)
            .expect("v0 should have a live interval");

        // v0 is defined at 0 and used at 3, so it is live at 0, 1, 2 and 3.
        for index in 0..4 {
            assert!(
                interval.covers(index),
                "index {index} is live but not covered: {interval:?}"
            );
        }
    }

    /// `alloc` must report *both* pieces of memory bookkeeping when it evicts an
    /// occupant and reloads the requested value: emitting only the reload would
    /// drop the evicted value (it was still marked as spilled, so the next
    /// reload would read stale memory).
    #[test]
    fn alloc_reports_spill_and_reload_together() {
        let registers = [Register::R0, Register::R1, Register::R2, Register::R3];
        let mut alloc = RegAlloc::new(&registers);

        let evicted = Variable::new(0);
        let value = Variable::new(1);

        // `evicted` currently sits in R0 and its slot holds its value.
        alloc.liveness.record(evicted, 0);
        alloc.liveness.set_register(evicted, Register::R0);
        alloc.liveness.set_stack(evicted, 0);
        alloc.mark_stack_written(evicted);
        alloc.reg_set.use_register(Register::R0, evicted, false);

        // `value` shares R0 (their ranges do not overlap) and is spilled to 1.
        alloc.liveness.record(value, 5);
        alloc.liveness.set_register(value, Register::R0);
        alloc.liveness.set_stack(value, 1);
        alloc.mark_stack_written(value);

        let (register, actions) = alloc.alloc(value, 5);
        assert_eq!(register, Register::R0);
        assert_eq!(
            actions.len(),
            2,
            "the eviction and the reload are both required: {actions:?}"
        );

        match &actions[0] {
            Action::Spill { register, stack } => {
                assert_eq!((*register, *stack), (Register::R0, 0));
            }
            other => panic!("expected the evicted value to be spilled first: {other:?}"),
        }
        match &actions[1] {
            Action::Restore { register, stack } => {
                assert_eq!((*register, *stack), (Register::R0, 1));
            }
            other => panic!("expected the value to be reloaded second: {other:?}"),
        }
    }
}
