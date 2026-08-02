use std::{collections::BTreeMap, fmt};

use petgraph::{
    algo::dominators::Dominators,
    graph::{DiGraph, NodeIndex},
};

use super::instruction::*;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct NaturalLoop {
    pub(crate) header: BlockId,
    pub(crate) blocks: Vec<BlockId>,
}

#[derive(Debug, Clone)]
pub struct Block {
    id: BlockId,
    #[allow(dead_code)]
    label: Name,
    instructions: Vec<Instruction>,
    params: Vec<Variable>,
    seal: bool,
}

impl Block {
    pub fn new(id: BlockId, label: impl Into<Name>) -> Self {
        Self {
            id,
            label: label.into(),
            instructions: Vec::new(),
            params: Vec::new(),
            seal: false,
        }
    }

    pub fn id(&self) -> BlockId {
        self.id
    }

    pub fn params(&self) -> &[Variable] {
        &self.params
    }

    pub fn params_mut(&mut self) -> &mut Vec<Variable> {
        &mut self.params
    }

    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    pub fn instructions_mut(&mut self) -> &mut Vec<Instruction> {
        &mut self.instructions
    }

    pub fn emit(&mut self, instruction: Instruction) {
        if !self.seal {
            self.instructions.push(instruction);
        }
    }

    pub fn seal(&mut self) {
        self.seal = true;
    }

    pub fn is_sealed(&self) -> bool {
        self.seal
    }

    pub fn set_block_params(&mut self, params: Vec<Variable>) {
        self.params = params;
    }

    pub fn append_block_param(&mut self, param: Variable) {
        self.params.push(param);
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block{}", self.id.as_usize())?;
        if !self.params.is_empty() {
            write!(f, "(")?;
            for (i, param) in self.params.iter().enumerate() {
                write!(f, "{param}")?;
                if i < self.params.len() - 1 {
                    write!(f, ",")?;
                }
            }
            write!(f, ")")?;
        }
        writeln!(f)?;
        for (i, inst) in self.instructions.iter().enumerate() {
            writeln!(f, "{i}\t{inst}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    blocks: Vec<Block>,
    entry: Option<BlockId>,
    current_block: Option<BlockId>,
    variables: Vec<Variable>,
    graph: DiGraph<BlockId, ()>,
    block_node_map: BTreeMap<BlockId, NodeIndex>,
    precedences: BTreeMap<BlockId, Vec<BlockId>>,
    successors: BTreeMap<BlockId, Vec<BlockId>>,
}

impl ControlFlowGraph {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            entry: None,
            current_block: None,
            variables: Vec::new(),
            graph: DiGraph::new(),
            block_node_map: BTreeMap::new(),
            precedences: BTreeMap::new(),
            successors: BTreeMap::new(),
        }
    }

    pub fn switch_to_block(&mut self, block: BlockId) {
        self.current_block = Some(block);
    }

    pub fn create_block(&mut self, label: impl Into<Name>) -> BlockId {
        let id = BlockId::new(self.blocks.len());
        self.blocks.push(Block::new(id, label));
        let node_index = self.graph.add_node(id);
        self.block_node_map.insert(id, node_index);
        self.precedences.insert(id, Vec::new());
        self.successors.insert(id, Vec::new());
        id
    }

    pub fn seal_block(&mut self, block: BlockId) {
        self.blocks[block.as_usize()].seal();
    }

    pub fn entry(&self) -> Option<BlockId> {
        self.entry
    }

    pub fn set_entry(&mut self, entry: BlockId) {
        self.entry = Some(entry);
    }

    pub fn current_block(&self) -> Option<BlockId> {
        self.current_block
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn set_block_params(&mut self, block: BlockId, params: Vec<Variable>) {
        self.blocks
            .get_mut(block.as_usize())
            .expect("no current block")
            .set_block_params(params);
    }

    pub fn append_block_param(&mut self, block: BlockId, param: Variable) {
        self.blocks
            .get_mut(block.as_usize())
            .expect("no current block")
            .append_block_param(param);
    }

    pub fn emit(&mut self, inst: Instruction) {
        let curr = self.current_block.expect("no current block");

        if self.blocks[curr.as_usize()].is_sealed() {
            return;
        }

        match &inst {
            Instruction::Jump { dst, .. } => {
                let dst = dst.to_block();
                self.successors.entry(curr).or_default().push(dst);
                self.precedences.entry(dst).or_default().push(curr);

                let curr_node = self.block_node_map[&curr];
                let dst_node = self.block_node_map[&dst];
                self.graph.add_edge(curr_node, dst_node, ());
            }
            Instruction::BrIf {
                true_blk,
                false_blk,
                ..
            } => {
                let true_blk = true_blk.to_block();
                let false_blk = false_blk.to_block();
                self.successors.entry(curr).or_default().push(true_blk);
                self.successors.entry(curr).or_default().push(false_blk);
                self.precedences.entry(true_blk).or_default().push(curr);
                self.precedences.entry(false_blk).or_default().push(curr);

                let curr_node = self.block_node_map[&curr];
                let true_node = self.block_node_map[&true_blk];
                let false_node = self.block_node_map[&false_blk];
                self.graph.add_edge(curr_node, true_node, ());
                self.graph.add_edge(curr_node, false_node, ());
            }
            _ => {}
        }

        self.current_block
            .and_then(|curr| self.blocks.get_mut(curr.as_usize()))
            .expect("no current block")
            .emit(inst);
    }

    pub fn create_variable(&mut self) -> Value {
        let id = Variable::new(self.variables.len());
        self.variables.push(id);
        Value::new(id)
    }

    pub(crate) fn get_block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.as_usize())
    }

    pub(crate) fn get_block_mut(&mut self, id: BlockId) -> Option<&mut Block> {
        self.blocks.get_mut(id.as_usize())
    }

    pub(crate) fn get_block_params(&self, id: BlockId) -> Vec<Variable> {
        self.blocks
            .get(id.as_usize())
            .map(|block| block.params.clone())
            .unwrap_or_default()
    }

    pub fn get_precedences(&self, block_id: BlockId) -> &Vec<BlockId> {
        &self.precedences[&block_id]
    }

    pub fn get_successors(&self, block_id: BlockId) -> &Vec<BlockId> {
        &self.successors[&block_id]
    }

    pub fn add_edge(&mut self, from: BlockId, to: BlockId) {
        self.successors.entry(from).or_default().push(to);
        self.precedences.entry(to).or_default().push(from);

        let from_node = self.block_node_map[&from];
        let to_node = self.block_node_map[&to];
        self.graph.add_edge(from_node, to_node, ());
    }

    pub(crate) fn dominators(&self) -> Dominators<NodeIndex> {
        let entry = self.block_node_map[&self.entry.unwrap()];
        petgraph::algo::dominators::simple_fast(&self.graph, entry)
    }

    pub(crate) fn dominance_frontier(
        &self,
        dominators: &Dominators<NodeIndex>,
    ) -> BTreeMap<BlockId, Vec<BlockId>> {
        let mut df: BTreeMap<BlockId, Vec<BlockId>> = BTreeMap::new();

        for block_id in self.block_node_map.keys().cloned() {
            let block_node = self.block_node_map[&block_id];
            let idom_opt = dominators.immediate_dominator(block_node);

            for pred_id in self.get_precedences(block_id) {
                let mut runner_node = self.block_node_map[pred_id];

                while Some(runner_node) != idom_opt {
                    let runner_id = *self.graph.node_weight(runner_node).unwrap();
                    df.entry(runner_id).or_default().push(block_id);

                    if let Some(next) = dominators.immediate_dominator(runner_node) {
                        runner_node = next;
                    } else {
                        break;
                    }
                }
            }
        }

        for (_, list) in df.iter_mut() {
            list.sort_unstable();
            list.dedup();
        }
        df
    }

    pub fn loop_root_reverse_postorder_layout2(&self) -> BlockLayout {
        let entry = self.block_node_map[&self.entry.unwrap()];
        let dominators = self.dominators();

        let mut dom_tree: BTreeMap<NodeIndex, Vec<NodeIndex>> = BTreeMap::new();
        for node in self.graph.node_indices() {
            if let Some(idom) = dominators.immediate_dominator(node) {
                dom_tree.entry(idom).or_default().push(node);
            } else if node != entry {
                dom_tree.entry(entry).or_default().push(node);
            }
        }

        let mut postorder = Vec::new();

        fn dfs_dom_tree(
            node: NodeIndex,
            dom_tree: &BTreeMap<NodeIndex, Vec<NodeIndex>>,
            graph: &DiGraph<BlockId, ()>,
            postorder: &mut Vec<BlockId>,
        ) {
            if let Some(children) = dom_tree.get(&node) {
                for &child in children {
                    dfs_dom_tree(child, dom_tree, graph, postorder);
                }
            }
            postorder.push(graph[node]);
        }

        dfs_dom_tree(entry, &dom_tree, &self.graph, &mut postorder);
        postorder.reverse();

        let mut pos = 0;
        let mut block_pos_map = BTreeMap::new();
        for block_id in &postorder {
            block_pos_map.insert(*block_id, pos);
            pos += self
                .get_block(*block_id)
                .expect("block not found")
                .instructions()
                .len();
        }

        BlockLayout::new(postorder, block_pos_map)
    }

    pub fn graph_node(&self, block: BlockId) -> NodeIndex {
        self.block_node_map[&block]
    }

    pub fn graph_node_indices(&self) -> Vec<NodeIndex> {
        self.block_node_map.values().cloned().collect()
    }

    pub fn graph_node_weight(&self, node: NodeIndex) -> BlockId {
        self.graph[node]
    }
}

impl Default for ControlFlowGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ControlFlowGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for block in self.blocks.iter() {
            write!(f, "{}", block)?;
        }
        Ok(())
    }
}

pub struct BlockLayout {
    blocks: Vec<BlockId>,
    block_pos_map: BTreeMap<BlockId, usize>,
}

impl BlockLayout {
    pub fn new(blocks: Vec<BlockId>, block_pos_map: BTreeMap<BlockId, usize>) -> Self {
        Self {
            blocks,
            block_pos_map,
        }
    }

    pub fn blocks(&self) -> &[BlockId] {
        &self.blocks
    }

    pub fn get_block_pos(&self, block_id: BlockId) -> usize {
        *self.block_pos_map.get(&block_id).expect("block not found")
    }

    pub fn iter<'a>(&self, cfg: &'a ControlFlowGraph) -> impl Iterator<Item = &'a Block> {
        self.blocks
            .iter()
            .map(|block| cfg.get_block(*block).expect("block not found"))
    }

    pub fn iter_rev<'a>(&self, cfg: &'a ControlFlowGraph) -> impl Iterator<Item = &'a Block> {
        self.blocks
            .iter()
            .rev()
            .map(|block| cfg.get_block(*block).expect("block not found"))
    }

    pub fn for_each_mut<F>(&self, cfg: &mut ControlFlowGraph, mut f: F)
    where
        F: FnMut(&mut Block),
    {
        for &id in &self.blocks {
            f(cfg.get_block_mut(id).expect("block not found"));
        }
    }
}

pub trait DomExt {
    fn is_dominated(&self, descendant: NodeIndex, ancestor: NodeIndex) -> bool;
}

impl DomExt for Dominators<NodeIndex> {
    fn is_dominated(&self, descendant: NodeIndex, ancestor: NodeIndex) -> bool {
        if descendant == ancestor {
            return false;
        }
        let mut cur = descendant;
        while let Some(idom) = self.immediate_dominator(cur) {
            if idom == ancestor {
                return true;
            }
            cur = idom;
        }
        false
    }
}
