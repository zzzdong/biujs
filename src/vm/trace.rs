//! 精确根集：VM 侧根（`docs/GC-DESIGN.md` §6.1）。
//!
//! 本引擎的运行时状态**全部是自己管理的数据结构**（值栈、寄存器数组、调用帧、
//! 闭包变量栈、SEH 栈、全局表……），没有任何东西藏在机器栈里，所以根集可以
//! **100% 精确枚举**，不需要保守栈扫描 —— 这是"给脚本引擎做 GC"比"给任意 Rust
//! 程序做 GC"容易一个数量级的根本原因，也是 NG1 的立足点。
//!
//! ⚠️ **新增任何含 `Gc` / `Value` 的 VM / `State` 字段时，必须同步补进
//! [`impl Trace for VM`] / [`impl Trace for State`]**。漏一个不会编译报错，
//! 只会在某次收集之后读到一个已被回收的槽 —— 即静默 UAF（INV-3）。

use crate::vm::gc::{Trace, Worklist};
use crate::vm::object::PromiseReaction;

use super::{BoundFunction, Frame, PromiseAggregation, PromiseJob, SehRecord, State, VM};

impl Trace for SehRecord {
    fn trace(&self, out: &mut Worklist) {
        // 只有 `pending_exception` 与 `delayed_return_value` 持有值；
        // 其余字段都是 pc / 水位 / 布尔 / 区域身份。
        self.pending_exception.trace(out);
        self.delayed_return_value.trace(out);
    }
}

impl Trace for Frame {
    fn trace(&self, out: &mut Worklist) {
        // `pc` / `base` / `rsp` / `argc` / `this_state` / 两个水位都不是值。
        self.this.trace(out);
        self.func.trace(out);
        self.new_target.trace(out);
    }
}

impl Trace for PromiseReaction {
    fn trace(&self, out: &mut Worklist) {
        self.on_fulfilled.trace(out);
        self.on_rejected.trace(out);
        self.target.trace(out);
    }
}

impl Trace for PromiseJob {
    fn trace(&self, out: &mut Worklist) {
        match self {
            PromiseJob::Reaction {
                reaction,
                argument,
                ..
            } => {
                reaction.trace(out);
                argument.trace(out);
            }
            PromiseJob::Thenable { promise, thenable } => {
                promise.trace(out);
                thenable.trace(out);
            }
        }
    }
}

impl Trace for PromiseAggregation {
    fn trace(&self, out: &mut Worklist) {
        self.result.trace(out);
        self.values.trace(out);
    }
}

impl Trace for BoundFunction {
    fn trace(&self, out: &mut Worklist) {
        self.target.trace(out);
        self.this_arg.trace(out);
        self.bound_args.trace(out);
    }
}

impl Trace for State {
    fn trace(&self, out: &mut Worklist) {
        // 只 trace **活跃**的那一段：`rsp` 以上是已经弹出的槽，把它们当根会白白
        // 留住垃圾（`data_stack.len()` 是历史最高水位，不是当前栈顶）。
        let live = self.rsp.min(self.data_stack.len());
        self.data_stack[..live].trace(out);
        self.registers.trace(out);
        self.seh_stack.trace(out);
        // `globals` 刻意仍是 `Rc<RefCell<..>>`：它不是堆对象，是全局对象的另一面视图。
        for v in self.globals.borrow().values() {
            v.trace(out);
        }
        self.global_object.trace(out);
        self.script_env.trace(out);
        self.promise_jobs.trace(out);
        self.promises.trace(out);
        self.aggregations.trace(out);
        self.finally_handlers.trace(out);
        self.closure_var_stack.trace(out);
        // `ClosureVar` pushes cells here for the `Make*FuncObj` that follows.
        // The window is short, but an instruction boundary is a GC safe point,
        // so these are roots too.
        for (_, cell) in &self.pending_captured {
            cell.trace(out);
        }
        self.frames.trace(out);
    }
}

impl Trace for VM {
    fn trace(&self, out: &mut Worklist) {
        self.state.trace(out);
        self.builtins.trace(out);
        self.iterator_registry.trace(out);
        // `GetTemplateObject` 的站点缓存：模板对象（及其 `raw`）只能从这里到达，
        // 漏掉这一行，一次收集就会把缓存扫成悬垂句柄（INV-3）。
        self.template_objects.trace(out);
        self.generator_registry.trace(out);
        self.generator_send.trace(out);
        self.generator_yielded.trace(out);
        self.delegate_stack.trace(out);
        self.func_objs.trace(out);
        self.bound_functions.trace(out);
        self.revocable_proxies.trace(out);
        // 宿主侧入根的 `Value`：原生内建握在 Rust 局部变量里、跨一次"回调进 JS"
        // 仍然要用的那些。**漏了这一行，入根就等于没做** —— 收集时它们照样会被
        // 回收（实测：GC stress 下 `Builtins.map_prototype` 被扫成占位对象）。
        self.scope.roots().trace(out);
        // `invoke_boundaries` / `current_code` / 三个护栏字段 / 三个计数器都不是值。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::gc::{Gc, Object};
    use crate::vm::object::OrdinaryObject;
    use crate::vm::value::Value;

    fn traced_contains(vm: &VM, g: &Gc<Object>) -> bool {
        let mut wl = Worklist::new();
        vm.trace(&mut wl);
        wl.iter().any(|r| Gc::ptr_eq(r, g))
    }

    /// 值栈里的对象必须在根集里。
    #[test]
    fn vm_roots_include_the_value_stack() {
        let mut vm = VM::new();
        let g = vm.heap.alloc(Object::Ordinary(OrdinaryObject::new()));
        vm.state.data_stack.push(Value::Object(g));
        // `rsp` 是活跃栈顶，push 必然伴随它前进（`State::trace` 只 trace `..rsp`）。
        vm.state.rsp = vm.state.data_stack.len();
        assert!(traced_contains(&vm, &g));
    }

    /// 寄存器堆同样要被枚举 —— 它是**定长数组**，最容易漏。
    #[test]
    fn vm_roots_include_the_register_file() {
        let mut vm = VM::new();
        let g = vm.heap.alloc(Object::Ordinary(OrdinaryObject::new()));
        vm.state.registers[3] = Value::Object(g);
        assert!(traced_contains(&vm, &g));
    }

    /// 全局表里的对象（走 `globals` 那条 `Rc<RefCell<..>>` 路径）。
    #[test]
    fn vm_roots_include_the_globals_map() {
        let mut vm = VM::new();
        let g = vm.heap.alloc(Object::Ordinary(OrdinaryObject::new()));
        vm.state
            .globals
            .borrow_mut()
            .insert("x".to_string(), Value::Object(g));
        assert!(traced_contains(&vm, &g));
    }

    /// 带标签模板的站点缓存：模板对象只从这里可达，必须是根。
    #[test]
    fn vm_roots_include_the_template_object_cache() {
        let mut vm = VM::new();
        let g = vm.heap.alloc(Object::Ordinary(OrdinaryObject::new()));
        vm.template_objects.push(Some(Value::Object(g)));
        assert!(traced_contains(&vm, &g));
    }

    /// 帧上的 `this` / `func` / `new_target`。
    #[test]
    fn vm_roots_include_frame_fields() {
        let mut vm = VM::new();
        let g = vm.heap.alloc(Object::Ordinary(OrdinaryObject::new()));
        vm.state.frames.push(Frame {
            pc: 0,
            base: 0,
            rsp: 0,
            this: Value::Object(g),
            this_state: super::super::THIS_NONE,
            func: Value::Undefined,
            argc: 0,
            construct: false,
            new_target: Value::Undefined,
            closure_depth: 0,
            seh_depth: 0,
        });
        assert!(traced_contains(&vm, &g));
    }

    /// 内建的 23 个 prototype 必须是根（`Builtins::trace`）。
    #[test]
    fn vm_roots_include_the_builtins_prototypes() {
        let vm = VM::new();
        let mut wl = Worklist::new();
        vm.trace(&mut wl);
        assert!(
            wl.iter().any(|g| Gc::ptr_eq(g, &vm.builtins.map_prototype)),
            "Builtins::map_prototype 不在根集里"
        );
        assert!(wl.iter().any(|g| Gc::ptr_eq(g, &vm.builtins.object_prototype)));
    }

    /// `Map.prototype.set` 必须**经属性表**可达 —— 这是 GC stress 曾经炸出
    /// "Map constructor: 'set' is not callable" 的那个对象。
    #[test]
    fn map_prototype_methods_are_reachable() {
        let vm = VM::new();
        let mut wl = Worklist::new();
        vm.trace(&mut wl);
        // 第一轮：Builtins 的 prototype
        assert!(wl.iter().any(|g| Gc::ptr_eq(g, &vm.builtins.map_prototype)));
        // 第二轮：从 map_prototype 传播一层，必须能摸到它的方法
        let mut children = Worklist::new();
        let proto = wl
            .iter()
            .find(|g| Gc::ptr_eq(g, &vm.builtins.map_prototype))
            .expect("上面已断言过");
        proto.try_borrow().expect("未被借用").trace(&mut children);
        assert!(
            !children.is_empty(),
            "Map.prototype 没有任何子引用 —— 属性表没被 trace"
        );
    }

    /// 反例：只活在 Rust 局部变量里的对象**不在**根集里 —— 这正是 `Root` 存在的理由。
    #[test]
    fn a_bare_local_value_is_not_a_root() {
        let mut vm = VM::new();
        let g = vm.heap.alloc(Object::Ordinary(OrdinaryObject::new()));
        let _local = Value::Object(g);
        assert!(!traced_contains(&vm, &g));
    }
}
