use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::JSObject;
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

const MAX_PROTO_DEPTH: usize = 1000;

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

/// ES6 [[Get]](P) — traverse the prototype chain to find a property.
///
/// Returns the raw property value. If the property is an accessor,
/// returns `Value::Undefined` and does NOT invoke the getter.
pub fn internal_get(
    obj: Rc<RefCell<dyn JSObject>>,
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

// ─────────────────────────────────────────────────────────
// [[Set]] — prototype chain property assignment
// ─────────────────────────────────────────────────────────

/// ES6 [[Set]](P, V) — set a property value, traversing the prototype chain.
///
/// Returns Ok(true) if the set succeeded, Ok(false) if strict mode should throw.
pub fn internal_set(
    obj: Rc<RefCell<dyn JSObject>>,
    key: PropertyKey,
    value: Value,
) -> Result<bool, String> {
    // Step 1: Find own property
    let own_desc = {
        let borrowed = obj.borrow();
        borrowed.property_get(&key)
    };

    if let Some(desc) = own_desc {
        if desc.is_data_descriptor() {
            if !desc.writable {
                return Ok(false);
            }
            let mut borrowed = obj.borrow_mut();
            return borrowed.property_set(key, value);
        } else {
            // Accessor property — call setter
            if let Some(setter) = desc.setter {
                // TODO: call setter function with value
                // For now, just store the value directly
                let mut borrowed = obj.borrow_mut();
                return borrowed.property_set(key, value);
            }
            return Ok(false);
        }
    }

    // Step 2: No own property — walk prototype chain
    let mut current = {
        let borrowed = obj.borrow();
        borrowed.get_prototype()
    };

    while let Some(proto) = current {
        let proto_desc = {
            let borrowed = proto.borrow();
            borrowed.property_get(&key)
        };

        if let Some(desc) = proto_desc {
            if desc.is_data_descriptor() {
                if !desc.writable {
                    return Ok(false);
                }
                let mut borrowed = obj.borrow_mut();
                return borrowed.property_set(key, value);
            }
            // Accessor on prototype — call setter
            if let Some(_setter) = desc.setter {
                // TODO: call setter on obj with value
                let mut borrowed = obj.borrow_mut();
                return borrowed.property_set(key, value);
            }
            return Ok(false);
        }

        let borrowed = proto.borrow();
        current = borrowed.get_prototype();
    }

    // Step 3: No property found anywhere — create on obj
    let mut borrowed = obj.borrow_mut();
    borrowed.property_set(key, value)
}

// ─────────────────────────────────────────────────────────
// [[HasProperty]] — prototype chain existence check
// ─────────────────────────────────────────────────────────

/// ES6 [[HasProperty]](P) — check if a property exists in the prototype chain.
pub fn internal_has_property(
    obj: Rc<RefCell<dyn JSObject>>,
    key: &PropertyKey,
) -> Result<bool, String> {
    let mut current = Some(obj);
    let mut depth = 0;

    while let Some(obj_ref) = current {
        if depth > MAX_PROTO_DEPTH {
            return Err("Maximum prototype chain depth exceeded".to_string());
        }

        let borrowed = obj_ref.borrow();
        if borrowed.has_property(key) {
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
    obj: Rc<RefCell<dyn JSObject>>,
    key: &PropertyKey,
) -> Result<bool, String> {
    let mut borrowed = obj.borrow_mut();
    Ok(borrowed.property_delete(key))
}

// ─────────────────────────────────────────────────────────
// Property name-to-key conversion helpers
// ─────────────────────────────────────────────────────────

/// Convert a Value to a PropertyKey for property access
pub fn value_to_property_key(val: &Value) -> PropertyKey {
    match val {
        Value::String(s) => PropertyKey::from_str(s.as_str()),
        Value::Symbol(sym_data) => PropertyKey::Symbol(sym_data.id),
        // All other types are coerced to string per JS spec
        other => PropertyKey::from_str(&other.to_string()),
    }
}

/// Convert a string to PropertyKey
pub fn str_to_property_key(s: &str) -> PropertyKey {
    PropertyKey::from_str(s)
}