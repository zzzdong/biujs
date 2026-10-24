//! 帧槽分配：内存驻留语义下，每个变量一个固定栈槽。
//!
//! 这里**只**分配栈槽，不做寄存器分配。
//!
//! 曾经的线性扫描（`LiveInterval` / `RegisterSet` / spill-reload 动作，共约 900 行）
//! 只服务 `BIUJS_REG_VARS` 那条实验路径；该路径已随这个环境变量一起删除。理由是
//! 单趟、按布局顺序生成字节码的 codegen 无法知道**真正执行的是哪条前驱路径**——
//! "这个值在寄存器里"可能从未被写过。走内存寻址可以整类消掉这种 bug，实践中还更快
//! （调用点不必保存/恢复寄存器）。
//!
//! 历史：`src/compiler/regalloc.rs`（1063 行，`git log --follow` 可追）。

use crate::compiler::ir::Variable;

/// 变量 → 稳定栈槽；`stack_size()` 即帧大小。
///
/// 槽位在**首次使用**时分配，之后不变：同一条指令流里，读和写必须落在同一个槽上，
/// 否则前驱路径写进去的值就看不见了。
#[derive(Debug, Default)]
pub struct FrameSlots {
    /// 下标 = `Variable` 的 id（`ir` 层保证 id 连续且不大）。
    slots: Vec<Option<usize>>,
    next_slot: usize,
}

impl FrameSlots {
    pub fn new() -> Self {
        Self::default()
    }

    /// 帧大小（以栈槽计）。生成结束后才稳定 —— 所有变量都拿到槽位了。
    pub fn stack_size(&self) -> usize {
        self.next_slot
    }

    /// 变量 `var` 的槽位；第一次使用时分配一个。
    pub fn ensure_slot(&mut self, var: Variable) -> usize {
        let id = var.as_usize();
        if self.slots.len() <= id {
            self.slots.resize(id + 1, None);
        }
        if let Some(slot) = self.slots[id] {
            return slot;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.slots[id] = Some(slot);
        slot
    }

    /// 第 `arg` 个调用参数的偏移。
    ///
    /// 参数区在 `rbp` **之下**（负偏移）：调用方先 `Push`，被调方按 `[rbp-1]`、
    /// `[rbp-2]` … 读，所以帧自己的局部变量全部落在正偏移的槽里。
    pub fn load_arg(&self, arg: usize) -> isize {
        0 - ((arg as isize) + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_stable_and_the_frame_grows_by_one() {
        let mut slots = FrameSlots::new();
        assert_eq!(slots.stack_size(), 0);

        let a = Variable::new(0);
        let b = Variable::new(1);
        assert_eq!(slots.ensure_slot(a), 0);
        assert_eq!(slots.ensure_slot(b), 1);
        assert_eq!(slots.stack_size(), 2);

        // 同一个变量重复拿到的必须是同一个槽，帧大小不变。
        assert_eq!(slots.ensure_slot(a), 0);
        assert_eq!(slots.ensure_slot(b), 1);
        assert_eq!(slots.stack_size(), 2);
    }

    #[test]
    fn load_arg_addresses_below_rbp() {
        let slots = FrameSlots::new();
        assert_eq!(slots.load_arg(0), -1);
        assert_eq!(slots.load_arg(2), -3);
    }
}
