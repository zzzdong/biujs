
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// 原型链遍历的深度上限。
///
/// VM 侧的 `[[Get]]` / `[[HasProperty]]` / `[[Set]]` 也要用它 —— 那几处的 walk
/// 必须逐跳检查 Proxy（纯描述符 walk 做不到），所以深度检查跟着一起搬到了
/// 调用方手里。
pub(crate) const MAX_PROTO_DEPTH: usize = 1000;

/// `start` 的原型链上是否出现 `needle`（含 `start` 自己）。
///
/// ES 10.1.2.1 step 6：`[[SetPrototypeOf]]` 必须拒绝"把对象的原型设成它自己的
/// 后代"，否则之后每一次属性查找都会绕成死循环。`Object.setPrototypeOf` 与
/// `Reflect.setPrototypeOf` 都要做这条判定，所以放在这里共用一份 —— 以前是
/// 两份手写的 while 循环。
pub fn prototype_chain_contains(start: Gc<Object>, needle: Gc<Object>) -> bool {
    let mut current = Some(start);
    let mut depth = 0usize;
    while let Some(obj) = current {
        if Gc::ptr_eq(&obj, &needle) {
            return true;
        }
        if depth > MAX_PROTO_DEPTH {
            // 过深的链按"有环"处理：继续走既无意义也可能不终止。
            return true;
        }
        current = obj.borrow().get_prototype();
        depth += 1;
    }
    false
}

/// Options for prototype chain traversal
#[derive(Debug, Clone)]
pub struct GetPrototypeChainOptions {
    /// Only check own properties, don't traverse the prototype chain
    pub own_only: bool,
    /// Whether to throw on accessor properties instead of calling them
    pub throw_on_accessor: bool,
}

impl Default for GetPrototypeChainOptions {
    fn default() -> Self {
        Self {
            own_only: false,
            throw_on_accessor: false,
        }
    }
}

// ─────────────────────────────────────────────────────────
// [[Get]] — prototype chain property access
// ─────────────────────────────────────────────────────────

/// 纯描述符 walk（**不执行用户代码**）：沿原型链找第一个描述符并返回 data 值；
/// accessor 一律返回 `Undefined`，不会调用 getter。
///
/// 只给不需要重入调用的内部读取使用。JS 语义的 [[Get]] 是 `VM::get_member`
/// （`find_descriptor` 之后真正 `invoke` getter）；需要 getter 的调用方不要走这里。
pub fn internal_get(
    obj: Gc<Object>,
    key: &PropertyKey,
    options: Option<GetPrototypeChainOptions>,
) -> Result<Value, String> {
    let opts = options.unwrap_or_default();
    let mut current = Some(obj);
    let mut depth = 0;

    while let Some(obj_ref) = current {
        if depth > MAX_PROTO_DEPTH {
            return Err("Maximum prototype chain depth exceeded".to_string());
        }

        let borrowed = obj_ref.borrow();
        if let Some(desc) = borrowed.property_get(key) {
            return get_property_value(desc, opts.throw_on_accessor);
        }

        if opts.own_only {
            return Ok(Value::Undefined);
        }

        current = borrowed.get_prototype();
        depth += 1;
    }

    Ok(Value::Undefined)
}

/// Extract value from a PropertyDescriptor
fn get_property_value(desc: PropertyDescriptor, throw_on_accessor: bool) -> Result<Value, String> {
    if desc.is_data_descriptor() {
        Ok(desc.value)
    } else if throw_on_accessor {
        Err("Cannot get value of accessor property".to_string())
    } else {
        // Accessor property without invoking getter — return undefined
        Ok(Value::Undefined)
    }
}

/// The first descriptor found for `key` along the prototype chain, together
/// with the object that holds it.
///
/// The VM uses this to invoke accessors (which need re-entrant function calls)
/// instead of the pure-data walk in `internal_get`.
pub fn find_descriptor(
    obj: Gc<Object>,
    key: &PropertyKey,
) -> Result<Option<(Gc<Object>, PropertyDescriptor)>, String> {
    let mut current = Some(obj);
    let mut depth = 0;
    while let Some(owner) = current {
        if depth > MAX_PROTO_DEPTH {
            return Err("Maximum prototype chain depth exceeded".to_string());
        }
        let desc = owner.borrow().property_get(key);
        if let Some(desc) = desc {
            return Ok(Some((owner, desc)));
        }
        current = owner.borrow().get_prototype();
        depth += 1;
    }
    Ok(None)
}

// ─────────────────────────────────────────────────────────
// [[Set]] 的唯一入口是 `VM::set_member`（`vm/mod.rs`）
// ─────────────────────────────────────────────────────────
//
// 这里曾经有一份 `internal_set`：遇到 accessor 时是 `TODO: call setter`，
// 实际用 `property_set` 把 accessor 覆盖成数据描述符 —— 与 `set_member`
// 真正 `invoke(setter)` 的行为相反。它全仓无调用点（死代码），已删除。
// 保留两套相反语义只会让后续重构的回归不可信：需要重入执行用户代码的
// [[Get]]/[[Set]] 一律走 `VM::get_member` / `VM::set_member`；本文件只提供
// 不执行用户代码的描述符 walk（`internal_get` / `find_descriptor`）。

// ─────────────────────────────────────────────────────────
// [[HasProperty]] — prototype chain existence check
// ─────────────────────────────────────────────────────────

/// ES6 [[HasProperty]](P) — check if a property exists in the prototype chain.
pub fn internal_has_property(
    obj: Gc<Object>,
    key: &PropertyKey,
) -> Result<bool, String> {
    let mut current = Some(obj);
    let mut depth = 0;

    while let Some(obj_ref) = current {
        if depth > MAX_PROTO_DEPTH {
            return Err("Maximum prototype chain depth exceeded".to_string());
        }

        let borrowed = obj_ref.borrow();
        if borrowed.has_own_property(key) {
            return Ok(true);
        }

        current = borrowed.get_prototype();
        depth += 1;
    }

    Ok(false)
}

// ─────────────────────────────────────────────────────────
// [[Delete]] — property deletion
// ─────────────────────────────────────────────────────────

/// ES6 [[Delete]](P) — delete an own property.
pub fn internal_delete(
    heap: &mut Heap,
    obj: Gc<Object>,
    key: &PropertyKey,
) -> Result<bool, String> {
    // ES 10.1.9.1: `[[Delete]]` answers `true` when the own property is *already*
    // absent — `false` means "it is there and refuses to go" (a non-configurable
    // one). Without the first branch `delete o.missing` answered `false`, which
    // Node answers `true` to (and `Reflect.deleteProperty` must too).
    if obj.borrow().property_get(key).is_none() {
        return Ok(true);
    }
    let mut borrowed = obj.borrow_mut();
    Ok(borrowed.property_delete(heap, key))
}

// ─────────────────────────────────────────────────────────
// Property name-to-key conversion helpers
// ─────────────────────────────────────────────────────────

