//! 属性名驻留（Atom / Interner）。
//!
//! # 为什么需要它
//!
//! 属性键以前是 `Rc<String>`：每次 `PropertyKey::from_str("length")` 都要分配一个
//! `String` + 一个 `Rc`，每次属性查找都要做字符串哈希与字符串比较。这是属性访问
//! 路径上最大的单项开销（ChakraCore 用 `PropertyRecord` 做同一件事，见
//! `docs/ARCH-REVIEW-2026-10-24.md` §3.8）。
//!
//! # 设计
//!
//! - `Atom(u32)` 是驻留后的**整数 id**。`PropertyKey::Str(Atom)` 因此仍是 4 字节
//!   载荷，`PropertyKey` 的总尺寸不变（`Symbol(u64)` 仍是最大变体）。
//! - 驻留表挂在 [`crate::vm::gc::Heap`] 上，**不引入 `thread_local`**：atom 的
//!   生命周期与堆一致（INV-8：`Gc` 不得活过它所属的堆，atom 同理）。
//! - `WELL_KNOWN` 里的常用属性名在 `Heap::new` 时**按顺序**驻留，所以它们的 id 是
//!   确定的 `0..N`。于是 `well_known("length")` 查一张静态表就能拿到 `Atom`，
//!   **不需要堆** —— 这一点是关键：`impl JSObject` 的 `own_keys()` 之类拿不到
//!   `&mut Heap` 的地方也能构造常用键。
//! - 反过来，把 `Atom` 变回字符串需要堆（`Heap::resolve`）。所以只有**真要输出
//!   字符串**的地方（错误消息、`Object.keys`、Proxy trap 参数）才去解析；
//!   `key == well_known("length")` 这类比较是整数比较，不解析。

use std::collections::HashMap;
use std::rc::Rc;

/// 驻留后的属性名。`Copy` + 整数相等，比较与哈希都是 `u32` 的开销。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Atom(u32);

impl Atom {
    /// 序号（调试/测试用；不代表任何语义）。
    pub fn index(self) -> u32 {
        self.0
    }
}

/// 预驻留的常用属性名。
///
/// **必须保持字典序排序** —— `well_known` 用二分查找，`Interner::new` 按这个顺序
/// 驻留，于是 id 就是下标。新增名字时插到正确位置，不要追加到末尾。
pub const WELL_KNOWN: &[&str] = &[
    "BYTES_PER_ELEMENT",
    // 编译器给 class 构造器打的内部标记（`builtins::CLASS_CTOR_FLAG`）。
    "__isClassCtor__",
    "__value__",
    "add",
    "arguments",
    "asyncIterator",
    "buffer",
    "byteLength",
    "byteOffset",
    "caller",
    "cause",
    "configurable",
    "constructor",
    "description",
    "done",
    "entries",
    "enumerable",
    "errors",
    "flags",
    "for",
    "get",
    "global",
    "has",
    "ignoreCase",
    "index",
    "input",
    "keyFor",
    "keys",
    "lastIndex",
    "length",
    "message",
    "multiline",
    "name",
    "next",
    "parse",
    "prototype",
    "raw",
    "return",
    "set",
    "size",
    "source",
    "stack",
    "status",
    "sticky",
    "stringify",
    "then",
    "throw",
    "toJSON",
    "toString",
    "unicode",
    "unicodeSets",
    "value",
    "values",
    "writable",
];

/// 查一个常用属性名，**不需要堆**。
///
/// 只对 [`WELL_KNOWN`] 里的名字有效；返回 `None` 表示这个名字没有预驻留，
/// 调用方应该走 `PropertyKey::intern`（动态名字）。
pub fn well_known(name: &str) -> Option<Atom> {
    WELL_KNOWN
        .binary_search(&name)
        .ok()
        .map(|position| Atom(position as u32))
}

/// 属性名驻留表：`Rc<str>` → `Atom` 与反查表。
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<Rc<str>, Atom>,
    names: Vec<Rc<str>>,
}

impl Interner {
    /// 建表并预驻留 [`WELL_KNOWN`]（id = 在表里的位置）。
    pub fn new() -> Self {
        let mut interner = Self::default();
        for name in WELL_KNOWN {
            interner.intern(name);
        }
        interner
    }

    /// 驻留一个名字；已驻留过就返回原有 id（同一名字永远同一个 `Atom`）。
    pub fn intern(&mut self, name: &str) -> Atom {
        if let Some(atom) = self.map.get(name) {
            return *atom;
        }
        let name: Rc<str> = Rc::from(name);
        let atom = Atom(self.names.len() as u32);
        self.names.push(Rc::clone(&name));
        self.map.insert(name, atom);
        atom
    }

    /// 查一个**已驻留**的名字，不插入（读路径用：名字没驻留过就不可能在地图里，
    /// 这样未知变量名不会把驻留表撑大）。
    pub fn lookup(&self, name: &str) -> Option<Atom> {
        self.map.get(name).copied()
    }

    /// `Atom` → 名字。句柄一定来自本表，所以下标必然在界内。
    pub fn resolve(&self, atom: Atom) -> &str {
        &self.names[atom.0 as usize]
    }

    /// 已驻留的名字数（测试/调试用）。
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_table_is_sorted() {
        // `well_known` 用二分查找，所以表必须保持字典序。新增名字请插到正确位置
        // （这个测试会抓住插错的情况）。
        assert!(
            WELL_KNOWN.windows(2).all(|pair| pair[0] < pair[1]),
            "WELL_KNOWN 必须严格递增排序"
        );
    }

    #[test]
    fn well_known_ids_are_the_table_position() {
        for (position, name) in WELL_KNOWN.iter().enumerate() {
            assert_eq!(
                well_known(name),
                Some(Atom(position as u32)),
                "{name} 的 id 应等于它在 WELL_KNOWN 里的位置"
            );
        }
        // 表外名字没有预驻留 id。
        assert_eq!(well_known("not-a-well-known-name"), None);
    }

    #[test]
    fn interning_is_idempotent_and_well_known_comes_first() {
        let mut interner = Interner::new();
        // 预驻留的在前：常用名字不占后面的 id，也不会重复分配。
        assert_eq!(interner.len(), WELL_KNOWN.len());
        let a = interner.intern("length");
        assert_eq!(a, well_known("length").expect("length 是常用名"));
        assert_eq!(interner.len(), WELL_KNOWN.len());

        // 动态名字从表尾开始拿 id，且重复驻留拿到同一个。
        let first = interner.intern("a-dynamic-name");
        assert_eq!(first.index() as usize, WELL_KNOWN.len());
        assert_eq!(interner.intern("a-dynamic-name"), first);
        assert_eq!(interner.len(), WELL_KNOWN.len() + 1);
        assert_eq!(interner.resolve(first), "a-dynamic-name");
        assert_eq!(interner.resolve(a), "length");
    }

    #[test]
    fn every_well_known_name_resolves_to_itself() {
        let interner = Interner::new();
        for name in WELL_KNOWN {
            let atom = well_known(name).expect("常用名");
            assert_eq!(interner.resolve(atom), *name);
        }
    }
}
