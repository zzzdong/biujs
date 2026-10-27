//! 对象的**形状**（隐藏类）：属性名 → 槽位，以及"再加一个属性"的状态迁移。
//!
//! # 为什么需要它
//!
//! 属性现在存在 `HashMap<PropertyKey, PropertyDescriptor>` 里（`PropertyTable`）：
//! 每次 `obj.x` 都要一次哈希 + 一次描述符克隆，而且每个属性都背着一个完整的
//! `PropertyDescriptor`（值 + 三个布尔 + getter/setter）。
//!
//! 形状把"布局"从"数据"里分出来：同形状的对象共享一个 [`Shape`]，属性读变成
//! **沿形状查到槽位下标 → `slots[i]`**；属性值以裸 `Value` 存在对象自己的
//! `slots` 里；只有属性"不是默认属性"（accessor / 只读 / 不可枚举 / 不可配置）
//! 时才需要一个完整描述符。
//!
//! # 为什么形状必须按 VM 共享
//!
//! [`PropertyKey::Str`](crate::vm::property::PropertyKey::Str) 装的是驻留后的
//! [`Atom`](crate::vm::atom::Atom)，而 atom 是 **per-heap** 的：同一个 id 在不同
//! 堆里是不同的字符串。跨 VM 复用形状会把 `length` 和 `x` 串味。所以：
//!
//! - 空形状（root）挂在 [`crate::vm::gc::Heap`] 上，随堆生灭；
//! - 迁移出来的子形状从 root 往下**强引用**，于是只要还有一个对象用着这条链上的
//!   任意形状，整条链就活着；链上最后一个对象没了，链也就回收了；
//! - 反过来，子形状指回父亲用 `Weak` —— 否则父子互相强引用就是个环，谁也死不了。
//!
//! # 与 GC 的关系
//!
//! 形状里只有 [`PropertyKey`]（整数 id），没有 `Value`，所以**不需要** GC 追踪；
//! 对象的属性值在 `slots: Vec<Value>` 里，照常由 `Trace` 覆盖。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::vm::property::PropertyKey;

/// 一个对象的属性布局。
///
/// `table` 装的是**全部**属性（继承下来的 + 本形状新增的那一个），所以读是 O(1)
/// 的一次哈希，不必沿链往上走；代价是每次迁移要复制这张表。我们的对象普遍很小
/// （少数几十个属性），形状的数量也有限，复制是可接受的 —— ChakraCore 的类型也
/// 是这么做的。
#[derive(Debug)]
pub struct Shape {
    /// 父形状（本形状是"加了某个属性之后"的那个）。root 为 `None`。
    ///
    /// 用 `Weak`：父子之间只能有一个方向是强引用。
    parent: Option<Weak<Shape>>,
    /// 属性名 → 槽位下标（覆盖本形状**可见的全部**属性）。
    table: HashMap<PropertyKey, u32>,
    /// 属性名，**按创建顺序**。形状的迁移顺序就是属性的创建顺序，所以这份列表
    /// 直接就是 `OrdinaryOwnPropertyKeys` 要的字符串顺序（索引键另外排序）。
    keys: Vec<PropertyKey>,
    /// 本形状新增的那个属性名（root 为 `None`）。
    added: Option<PropertyKey>,
    /// 槽位个数（= `keys.len()`）。
    slot_count: u32,
    /// 迁移表："本形状再加一个属性"→ 已经建好的子形状。建过一次之后，下一个
    /// 同样布局的对象直接复用，不必再复制一张表。
    transitions: RefCell<HashMap<PropertyKey, Rc<Shape>>>,
}

impl Shape {
    /// 空形状：没有任何属性。整个 VM 只有一个（挂在堆上）。
    pub fn root() -> Rc<Self> {
        Rc::new(Self {
            parent: None,
            table: HashMap::new(),
            keys: Vec::new(),
            added: None,
            slot_count: 0,
            transitions: RefCell::new(HashMap::new()),
        })
    }

    /// 本形状可见的属性个数（= 槽位数）。
    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }

    /// 属性名，**按创建顺序**（`OrdinaryOwnPropertyKeys` 的字符串部分）。
    pub fn keys(&self) -> &[PropertyKey] {
        &self.keys
    }

    /// `key` 的槽位下标。
    pub fn slot_of(&self, key: &PropertyKey) -> Option<u32> {
        self.table.get(key).copied()
    }

    /// "再加一个属性"后的形状，以及新属性占的槽位下标。
    ///
    /// 同一个 `(形状, 属性名)` 只会建一次子形状，之后都从迁移表里拿 —— 这正是
    /// "同布局的对象共享形状"这件事落地的那一行。
    pub fn transition(self: &Rc<Self>, key: PropertyKey) -> (Rc<Self>, u32) {
        if let Some(next) = self.transitions.borrow().get(&key) {
            let slot = next.slot_of(&key).expect("子形状一定含有刚加的键");
            return (Rc::clone(next), slot);
        }

        let mut table = self.table.clone();
        let mut keys = self.keys.clone();
        let slot = self.slot_count;
        table.insert(key, slot);
        keys.push(key);

        let next = Rc::new(Self {
            parent: Some(Rc::downgrade(self)),
            table,
            keys,
            added: Some(key),
            slot_count: slot + 1,
            transitions: RefCell::new(HashMap::new()),
        });
        self.transitions.borrow_mut().insert(key, Rc::clone(&next));
        (next, slot)
    }

    /// 本形状新增的那个属性名（调试/测试用）。
    pub fn added(&self) -> Option<PropertyKey> {
        self.added
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// root 之后每加一个属性就多一个槽位，`keys` 是创建顺序。
    #[test]
    fn transitions_append_slots_in_creation_order() {
        let root = Shape::root();
        assert_eq!(root.slot_count(), 0);
        assert!(root.keys().is_empty());

        let (with_a, slot_a) = root.transition(PropertyKey::from_index(0));
        assert_eq!(slot_a, 0);
        assert_eq!(with_a.slot_count(), 1);
        assert_eq!(with_a.keys(), &[PropertyKey::from_index(0)]);

        let (with_b, slot_b) = with_a.transition(PropertyKey::from_index(1));
        assert_eq!(slot_b, 1);
        assert_eq!(
            with_b.keys(),
            &[PropertyKey::from_index(0), PropertyKey::from_index(1)]
        );
        // 子形状能看见父形状的槽位（`table` 是全量拷贝）。
        assert_eq!(with_b.slot_of(&PropertyKey::from_index(0)), Some(0));
    }

    /// 迁移表：同一个 (形状, 键) 只建一次，第二个对象拿到的是同一个 `Rc`。
    #[test]
    fn transitions_are_shared() {
        let root = Shape::root();
        let key = PropertyKey::from_index(7);

        let (first, slot_first) = root.transition(key);
        let (second, slot_second) = root.transition(key);

        assert_eq!(slot_first, slot_second);
        assert!(
            Rc::ptr_eq(&first, &second),
            "同布局的对象必须共享同一个 Shape"
        );
    }

    /// 父边是 `Weak`：父子之间只能有一个方向是强引用，否则就是个环。
    ///
    /// 方向是"父 → 子"强：所以 root 的迁移表会让子形状一直活着（这正是复用形状
    /// 要的），而子形状**不会**反过来拖住 root。
    #[test]
    fn parent_edge_is_weak() {
        let root = Shape::root();
        let (child, _) = root.transition(PropertyKey::from_index(0));

        // 子形状活着时父形状能升回来（`parent` 是 `Weak` ⇒ 必须 `upgrade`）。
        {
            let parent = child
                .parent
                .as_ref()
                .and_then(Weak::upgrade)
                .expect("父形状还在");
            assert!(Rc::ptr_eq(&parent, &root));
        }

        // 唯一对 root 的强引用是局部变量本身 —— 升上来的那个已经随作用域释放了。
        assert_eq!(
            Rc::strong_count(&root),
            1,
            "子形状不得反向强引用父形状（父子之间只能一个方向是强引用）"
        );

        // 反过来 root 的迁移表确实持有 child（否则"共享形状"就无从谈起）。
        drop(child);
        assert!(
            root.transitions.borrow().contains_key(&PropertyKey::from_index(0)),
            "root 的迁移表必须留住子形状"
        );
    }

    /// `added` 记录"这一层加了谁"，从 root 往下读回整条迁移路径。
    #[test]
    fn added_records_the_transition_key() {
        let root = Shape::root();
        assert_eq!(root.added(), None);
        let (with_a, _) = root.transition(PropertyKey::from_index(3));
        assert_eq!(with_a.added(), Some(PropertyKey::from_index(3)));
    }
}
