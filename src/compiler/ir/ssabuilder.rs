use petgraph::{algo::dominators::Dominators, graph::NodeIndex};

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::compiler::ir::instruction::Variable;
use crate::compiler::ir::{BlockId, ControlFlowGraph, Instruction, Value};

/// 变量版本栈，用于SSA重命名
/// 键：原始变量ID，值：版本栈（栈顶是当前活跃版本）
type VersionStack = HashMap<Variable, Vec<Variable>>;

/// 记录每个块中每个原始变量的最终版本
/// 用于在重命名后查找正确的变量版本
type BlockVarVersions = HashMap<BlockId, HashMap<Variable, Variable>>;

#[derive(Debug)]
struct VariableInfo {
    defs: Vec<(BlockId, usize)>, // 写点
    uses: Vec<(BlockId, usize)>, // 读点
}

impl VariableInfo {
    pub fn new() -> Self {
        Self {
            defs: Vec::new(),
            uses: Vec::new(),
        }
    }
}

pub struct SSABuilder<'a> {
    cfg: &'a mut ControlFlowGraph,
    dominators: Dominators<NodeIndex>,
    /// 记录每个throw块对应的异常handler块列表
    throw_to_handlers: HashMap<BlockId, Vec<BlockId>>,
}

impl<'a> SSABuilder<'a> {
    pub fn new(cfg: &'a mut ControlFlowGraph) -> Self {
        let dominators = cfg.dominators();
        Self {
            cfg,
            dominators,
            throw_to_handlers: HashMap::new(),
        }
    }

    /// 完整的SSA转换流程
    pub fn convert_to_ssa(&mut self) {
        // 0. 预处理：为所有Throw指令添加异常边到对应的SEH handler
        self.add_exception_edges_for_throws();

        // 重新计算支配关系（添加异常边后，支配关系可能改变）
        self.dominators = self.cfg.dominators();

        // 1. 收集变量的定义和使用信息
        let var_info = self.collect_var_def_and_use();

        // 2. 计算支配边界
        let dominance_frontier = self.cfg.dominance_frontier(&self.dominators);

        // 3. 确定Phi节点位置
        let phi_placements = self.determine_phi_nodes(&var_info, &dominance_frontier);

        // 4. 添加Phi块参数
        self.add_phi_parameters(&phi_placements);

        // 5. 变量重命名，并记录每个块中每个原始变量的最终版本
        let block_var_versions = self.rename_variables(&var_info);

        // 6. 处理跳转参数（在重命名之后，使用重命名后的变量）
        self.handle_jump_args(&phi_placements, &block_var_versions);
    }

    /// 预处理：扫描所有块，为每个Throw指令添加从throw块到对应SEH handler的异常边
    fn add_exception_edges_for_throws(&mut self) {
        let mut seh_scope: Vec<(BlockId, Option<BlockId>)> = Vec::new();
        let mut throw_edges: Vec<(BlockId, BlockId)> = Vec::new();

        // Track the outer SEH scope for each finally block (the scope BEFORE its PushSeh)
        // This is used for ResumeException: when finally propagates an exception,
        // it goes to these outer handlers
        let mut finally_outer_scope: HashMap<BlockId, Vec<(BlockId, Option<BlockId>)>> = HashMap::new();

        for block in self.cfg.blocks() {
            for inst in block.instructions() {
                match inst {
                    Instruction::PushSeh { handler, finally } => {
                        if let Some(finally_blk) = finally {
                            finally_outer_scope.insert(*finally_blk, seh_scope.clone());
                        }
                        seh_scope.push((*handler, *finally));
                    }
                    Instruction::PopSeh => {
                        seh_scope.pop();
                    }
                    Instruction::Throw { .. } => {
                        for (catch_handler, finally_handler) in &seh_scope {
                            throw_edges.push((block.id(), *catch_handler));
                            if let Some(finally_blk) = finally_handler {
                                throw_edges.push((block.id(), *finally_blk));
                            }
                        }
                        break;
                    }
                    Instruction::ResumeException { .. } => {
                        if let Some(outer_scope) = finally_outer_scope.get(&block.id()) {
                            for (catch_handler, _) in outer_scope {
                                throw_edges.push((block.id(), *catch_handler));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // 添加异常边（如果尚未添加），同时记录throw→handler映射
        for (throw_block, handler) in throw_edges {
            let successors = self.cfg.get_successors(throw_block);
            if !successors.contains(&handler) {
                self.cfg.add_edge(throw_block, handler);
            }
            self.throw_to_handlers
                .entry(throw_block)
                .or_default()
                .push(handler);
        }
    }

    /// 收集所有变量的定义和使用点
    fn collect_var_def_and_use(&self) -> HashMap<Variable, VariableInfo> {
        let mut map = HashMap::new();

        for block in self.cfg.blocks() {
            for (idx, inst) in block.instructions().iter().enumerate() {
                let (defined, used) = inst.defined_and_used_vars();

                for var in defined {
                    map.entry(var)
                        .or_insert_with(VariableInfo::new)
                        .defs
                        .push((block.id(), idx));
                }

                for var in used {
                    map.entry(var)
                        .or_insert_with(VariableInfo::new)
                        .uses
                        .push((block.id(), idx));
                }
            }
        }

        map
    }

    /// 计算需要插入Phi节点的位置
    fn determine_phi_nodes(
        &self,
        var_info: &HashMap<Variable, VariableInfo>,
        dominance_frontier: &BTreeMap<BlockId, Vec<BlockId>>,
    ) -> HashMap<Variable, Vec<BlockId>> {
        let mut phi_placements: HashMap<Variable, Vec<BlockId>> = HashMap::new();

        for (var, info) in var_info {
            if info.defs.len() <= 1 {
                continue;
            }

            // 获取所有定义点所在的块
            let def_blocks: HashSet<BlockId> = info.defs.iter().map(|(block, _)| *block).collect();

            // 计算支配边界闭包
            let mut worklist: Vec<BlockId> = def_blocks.iter().cloned().collect();
            let mut visited = HashSet::new();

            while let Some(block) = worklist.pop() {
                if let Some(df_blocks) = dominance_frontier.get(&block) {
                    for &df_block in df_blocks {
                        if !visited.contains(&df_block) {
                            visited.insert(df_block);
                            worklist.push(df_block);
                            phi_placements.entry(*var).or_default().push(df_block);
                        }
                    }
                }
            }

            // **新增：特别处理汇合块**
            // 获取变量的所有使用点
            let use_blocks: HashSet<BlockId> = info.uses.iter().map(|(block, _)| *block).collect();

            // 检查是否有多个定义流向同一个使用块
            for &use_block in &use_blocks {
                // 找到流向该使用块的所有前驱
                let predecessors = self.get_block_predecessors(use_block);

                // 检查哪些前驱块中有该变量的定义
                let preds_with_def: Vec<_> = predecessors
                    .iter()
                    .filter(|&&pred| {
                        def_blocks.contains(&pred)
                            || phi_placements
                                .get(var)
                                .is_some_and(|blocks| blocks.contains(&pred)) // 或者前驱块中有该变量的phi
                    })
                    .collect();

                // 如果有多个不同的定义流向这个使用块，就需要phi
                if preds_with_def.len() > 1 && !visited.contains(&use_block) {
                    visited.insert(use_block);
                    phi_placements.entry(*var).or_default().push(use_block);

                    // 将该使用块加入工作列表，因为它可能影响下游
                    worklist.push(use_block);
                }
            }
        }

        phi_placements
    }

    /// 为需要Phi节点的块添加块参数
    fn add_phi_parameters(&mut self, phi_placements: &HashMap<Variable, Vec<BlockId>>) {
        for (var, blocks) in phi_placements {
            for block_id in blocks {
                self.cfg.append_block_param(*block_id, *var);
            }
        }
    }

    /// 处理跳转参数：为跳转到需要Phi参数的块的前驱块添加参数
    fn handle_jump_args(
        &mut self,
        phi_placements: &HashMap<Variable, Vec<BlockId>>,
        block_var_versions: &BlockVarVersions,
    ) {
        // 建立块到其需要的Phi参数的映射
        let block_phi_vars: HashMap<BlockId, Vec<Variable>> = phi_placements
            .iter()
            .map(|(var, blocks)| blocks.iter().map(move |&block_id| (block_id, *var)))
            .fold(HashMap::new(), |mut acc, iter| {
                for (block_id, var) in iter {
                    acc.entry(block_id).or_default().push(var);
                }
                acc
            });

        // 为每个需要Phi的块处理其前驱块的跳转指令
        for (block_id, phi_vars) in &block_phi_vars {
            let predecessors = self.cfg.get_precedences(*block_id).clone();
            let sorted_vars = phi_vars.to_vec();

            for pred_block_id in predecessors {
                // 获取前驱块中原始变量的最终版本
                let arg_values: Vec<Variable> = sorted_vars
                    .iter()
                    .filter_map(|phi_var| {
                        block_var_versions
                            .get(&pred_block_id)
                            .and_then(|pred_vars| pred_vars.get(phi_var).copied())
                    })
                    .collect();

                // 在前驱块的跳转指令中添加参数
                self.add_jump_arguments(pred_block_id, *block_id, &arg_values);
            }
        }
    }

    /// 为指定前驱块的跳转到目标块的指令添加参数
    fn add_jump_arguments(
        &mut self,
        pred_block_id: BlockId,
        target_block_id: BlockId,
        arg_values: &[Variable],
    ) {
        // 预先获取后继信息，避免与后续可变借用冲突
        let _successors: Vec<BlockId> = self.cfg.get_successors(pred_block_id).clone();
        // 检查pred_block_id是否是throw块且target_block_id是否是它的异常handler
        let is_exception_handler = self
            .throw_to_handlers
            .get(&pred_block_id)
            .map(|handlers| handlers.contains(&target_block_id))
            .unwrap_or(false);

        if let Some(pred_block) = self.cfg.get_block_mut(pred_block_id) {
            for inst in pred_block.instructions_mut().iter_mut() {
                match inst {
                    Instruction::Jump { dst, args } if dst.to_block() == target_block_id => {
                        args.extend(arg_values.iter().map(|&arg| Value::Variable(arg)));
                    }
                    Instruction::BrIf {
                        true_blk,
                        false_blk,
                        true_args,
                        false_args,
                        ..
                    } if true_blk.to_block() == target_block_id
                        || false_blk.to_block() == target_block_id =>
                    {
                        if true_blk.to_block() == target_block_id {
                            true_args.extend(arg_values.iter().map(|&arg| Value::Variable(arg)));
                        } else {
                            false_args.extend(arg_values.iter().map(|&arg| Value::Variable(arg)));
                        }
                    }
                    Instruction::Throw { args, .. } => {
                        if is_exception_handler {
                            args.extend(arg_values.iter().map(|&arg| Value::Variable(arg)));
                        }
                    }
                    Instruction::ResumeException { args } => {
                        if is_exception_handler {
                            args.extend(arg_values.iter().map(|&arg| Value::Variable(arg)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// 获取块的前驱
    fn get_block_predecessors(&self, block_id: BlockId) -> &Vec<BlockId> {
        self.cfg.get_precedences(block_id)
    }

    /// 执行SSA重命名算法
    fn rename_variables(&mut self, var_info: &HashMap<Variable, VariableInfo>) -> BlockVarVersions {
        let mut stacks: VersionStack = HashMap::new();
        let mut block_var_versions: BlockVarVersions = HashMap::new();

        // 从入口块开始遍历支配树
        let entry = self.cfg.entry().unwrap();
        self.rename_variables_in_block(entry, &mut stacks, var_info, &mut block_var_versions);

        block_var_versions
    }

    /// 重命名指定块及其子树中的变量
    fn rename_variables_in_block(
        &mut self,
        block_id: BlockId,
        stacks: &mut VersionStack,
        var_info: &HashMap<Variable, VariableInfo>,
        block_var_versions: &mut BlockVarVersions,
    ) {
        // 保存进入此块前的栈快照（用于在处理完所有子节点后恢复）
        let initial_snapshot: HashMap<Variable, Vec<Variable>> = stacks
            .iter()
            .map(|(var_id, stack)| (*var_id, stack.clone()))
            .collect();

        // 处理块参数（Phi参数）
        self.process_block_parameters(block_id, stacks);

        // 处理块内指令
        self.process_block_instructions(block_id, stacks);

        // 记录此块中每个原始变量的最终版本
        self.record_block_final_versions(block_id, stacks, block_var_versions);

        // 递归处理支配树中的子节点（确保子节点间状态隔离）
        self.process_child_blocks(block_id, stacks, var_info, block_var_versions);

        // 恢复进入此块前的栈状态
        *stacks = initial_snapshot;
    }

    /// 处理块参数（Phi参数）：创建新版本并更新块参数引用
    fn process_block_parameters(&mut self, block_id: BlockId, stacks: &mut VersionStack) {
        let param_var_ids: Vec<Variable> = self.cfg.get_block_params(block_id);

        // 为每个phi参数创建新版本
        let mut new_versions: HashMap<Variable, Variable> = HashMap::new();
        for var_id in &param_var_ids {
            let new_version = self.cfg.create_variable();
            let new_var_id = new_version.to_variable();
            new_versions.insert(*var_id, new_var_id);
        }

        // 更新块参数引用（将参数中的原始变量ID替换为新版本）
        {
            let block = self.cfg.get_block_mut(block_id).unwrap();
            for param in block.params_mut().iter_mut() {
                if let Some(new_version) = new_versions.get(param) {
                    *param = *new_version;
                }
            }
        }

        // 将新的phi参数版本压入栈中
        for (var_id, new_var_id) in new_versions {
            stacks.entry(var_id).or_default().push(new_var_id);
        }
    }

    /// 处理块内指令：重命名并更新变量栈
    fn process_block_instructions(&mut self, block_id: BlockId, stacks: &mut VersionStack) {
        let block = self.cfg.get_block_mut(block_id).unwrap();
        let mut instructions: Vec<Instruction> = block.instructions_mut().drain(..).collect();

        for inst in instructions.iter_mut() {
            let (defined, _) = inst.defined_and_used_vars();

            // 为定义创建新版本（先不压栈）
            let mut new_versions = HashMap::new();
            for var in &defined {
                let new_ver = self.cfg.create_variable().to_variable();
                new_versions.insert(*var, Value::Variable(new_ver));
            }

            // 重命名指令（使用点使用当前栈，定义点使用新版本）
            SSABuilder::rename_instruction(inst, &new_versions, stacks);

            // 将新定义的变量版本压栈
            for var in &defined {
                if let Some(new_ver) = new_versions.get(var)
                    && let Value::Variable(var_id) = new_ver
                {
                    stacks.entry(*var).or_default().push(*var_id);
                }
            }

            // 如果本条是 Jump/BrIf，立即回退它的 def （如果它定义了变量）
            if matches!(inst, Instruction::Jump { .. } | Instruction::BrIf { .. }) {
                for var in defined {
                    if let Some(stack) = stacks.get_mut(&var) {
                        stack.pop(); // 弹出jump/brif定义的变量
                    }
                }
            }
        }

        // 将处理后的指令放回块中
        let block = self.cfg.get_block_mut(block_id).unwrap();
        *block.instructions_mut() = instructions;
    }

    /// 记录此块中每个原始变量的最终版本
    fn record_block_final_versions(
        &self,
        block_id: BlockId,
        stacks: &VersionStack,
        block_var_versions: &mut BlockVarVersions,
    ) {
        for (var_id, stack) in stacks.iter() {
            if let Some(&current_version) = stack.last() {
                block_var_versions
                    .entry(block_id)
                    .or_default()
                    .insert(*var_id, current_version);
            }
        }
    }

    /// 递归处理支配树中的子节点，确保子节点间状态隔离
    fn process_child_blocks(
        &mut self,
        block_id: BlockId,
        stacks: &mut VersionStack,
        var_info: &HashMap<Variable, VariableInfo>,
        block_var_versions: &mut BlockVarVersions,
    ) {
        let block_node = self.cfg.graph_node(block_id);

        // 收集所有子节点
        let child_nodes: Vec<_> = self
            .cfg
            .graph_node_indices()
            .iter()
            .cloned()
            .filter(|node| self.dominators.immediate_dominator(*node) == Some(block_node))
            .collect();

        // 为每个子节点处理，确保状态隔离
        for child_node in child_nodes {
            let child_block_id = self.cfg.graph_node_weight(child_node);

            // 保存当前状态
            let child_snapshot: HashMap<Variable, Vec<Variable>> = stacks
                .iter()
                .map(|(var_id, stack)| (*var_id, stack.clone()))
                .collect();

            // 处理子节点
            self.rename_variables_in_block(child_block_id, stacks, var_info, block_var_versions);

            // 恢复状态以处理下一个子节点
            *stacks = child_snapshot;
        }
    }

    /// 重命名指令中的变量引用
    fn rename_instruction(
        inst: &mut Instruction,
        new_versions: &HashMap<Variable, Value>,
        stacks: &VersionStack,
    ) {
        match inst {
            Instruction::LoadArg { dst, .. } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::LoadConst { dst, .. } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::LoadEnv { dst, .. } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::Move { dst, src } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(src, stacks);
            }
            Instruction::UnaryOp { dst, src, .. } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(src, stacks);
            }
            Instruction::BinaryOp { dst, lhs, rhs, .. } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(lhs, stacks);
                SSABuilder::rename_use(rhs, stacks);
            }
            Instruction::Call { func, args, result } => {
                SSABuilder::rename_definition(result, new_versions);
                SSABuilder::rename_use(func, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::CallEx {
                callable,
                args,
                result,
            } => {
                SSABuilder::rename_definition(result, new_versions);
                SSABuilder::rename_use(callable, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::CallNative { func, args, result } => {
                SSABuilder::rename_definition(result, new_versions);
                SSABuilder::rename_use(func, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::PropertyGet {
                dst,
                object,
                property,
            } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(object, stacks);
                SSABuilder::rename_use(property, stacks);
            }
            Instruction::PropertySet {
                object,
                property,
                value,
            } => {
                SSABuilder::rename_use(object, stacks);
                SSABuilder::rename_use(property, stacks);
                SSABuilder::rename_use(value, stacks);
            }
            Instruction::PropertyCall {
                object,
                property,
                args,
                result,
            } => {
                SSABuilder::rename_definition(result, new_versions);
                SSABuilder::rename_use(object, stacks);
                SSABuilder::rename_use(property, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::Return { value } => {
                if let Some(val) = value {
                    SSABuilder::rename_use(val, stacks);
                }
            }
            Instruction::Jump { dst, args } => {
                SSABuilder::rename_use(dst, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::BrIf {
                condition,
                true_blk,
                false_blk,
                true_args,
                false_args,
            } => {
                SSABuilder::rename_use(condition, stacks);
                SSABuilder::rename_use(true_blk, stacks);
                SSABuilder::rename_use(false_blk, stacks);
                for arg in true_args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
                for arg in false_args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::MakeIterator { src, dst } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(src, stacks);
            }
            Instruction::IterateNext {
                iter,
                item,
                has_next,
            } => {
                SSABuilder::rename_definition(item, new_versions);
                SSABuilder::rename_definition(has_next, new_versions);
                SSABuilder::rename_use(iter, stacks);
            }
            Instruction::MakeArray { dst } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::ArrayPush { array, value } => {
                SSABuilder::rename_use(array, stacks);
                SSABuilder::rename_use(value, stacks);
            }
            Instruction::MakeObject { dst } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::IndexGet { dst, object, index } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(object, stacks);
                SSABuilder::rename_use(index, stacks);
            }
            Instruction::IndexSet {
                object,
                index,
                value,
            } => {
                SSABuilder::rename_use(object, stacks);
                SSABuilder::rename_use(index, stacks);
                SSABuilder::rename_use(value, stacks);
            }
            Instruction::Halt { value } => {
                if let Some(v) = value {
                    SSABuilder::rename_use(v, stacks);
                }
            }
            Instruction::PushSeh { .. } => {}
            Instruction::PopSeh => {}
            Instruction::Throw { value, args } => {
                SSABuilder::rename_use(value, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::LoadException { dst } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::ResumeException { .. } => {
                // No variables to rename
            }
            Instruction::DelayedJump { .. } => {
                // No variables to rename
            }
            Instruction::TypeOf { dst, src } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(src, stacks);
            }
            Instruction::New {
                dst,
                constructor,
                args,
            } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(constructor, stacks);
                for arg in args.iter_mut() {
                    SSABuilder::rename_use(arg, stacks);
                }
            }
            Instruction::LoadThis { dst } => {
                SSABuilder::rename_definition(dst, new_versions);
            }
            Instruction::MakeFuncObj { dst, func_id } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(func_id, stacks);
            }
            Instruction::MakeArrowFuncObj { dst, func_id, captured_this } => {
                SSABuilder::rename_definition(dst, new_versions);
                SSABuilder::rename_use(func_id, stacks);
                SSABuilder::rename_use(captured_this, stacks);
            }
        }
    }

    /// 重命名定义点：使用预创建的新版本
    fn rename_definition(var: &mut Value, new_versions: &HashMap<Variable, Value>) {
        if let Value::Variable(var_id) = var
            && let Some(new_version) = new_versions.get(var_id)
        {
            *var = *new_version;
        }
    }

    /// 重命名使用点：从栈顶获取当前版本
    fn rename_use(var: &mut Value, stacks: &VersionStack) {
        if let Value::Variable(var_id) = var
            && let Some(stack) = stacks.get(var_id)
            && let Some(current_version) = stack.last()
        {
            *var = Value::Variable(*current_version);
        }
        // 如果栈为空，说明变量未定义，保持原样（可能会在后端报错）
    }

    /// 提取throw_to_handlers映射并消费SSABuilder
    pub fn into_throw_to_handlers(self) -> HashMap<BlockId, Vec<BlockId>> {
        self.throw_to_handlers
    }
}
