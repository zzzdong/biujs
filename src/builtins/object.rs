use crate::RuntimeError;
use crate::vm::property::PropertyDescriptor;
use crate::vm::object::{JSObject, new_array_object_from_vec};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

use super::set_static_method;

pub fn object_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        let obj = crate::vm::object::OrdinaryObject::new();
        Ok(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
            obj,
        ))))
    } else {
        Ok(args[0].clone())
    }
}

pub fn object_keys(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.keys requires at least 1 argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            let keys = borrowed.own_keys();
            let str_keys: Vec<Value> = keys.iter().map(|k| Value::string(&k.display())).collect();
            Ok(new_array_object_from_vec(str_keys))
        }
        _ => Err(RuntimeError::TypeError(
            "Object.keys: argument is not an object".to_string(),
        )),
    }
}

pub fn object_values(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.values requires at least 1 argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            let keys = borrowed.own_keys();
            let mut values = Vec::new();
            for key in &keys {
                if let Some(desc) = borrowed.property_get(key) {
                    values.push(desc.value);
                }
            }
            Ok(new_array_object_from_vec(values))
        }
        _ => Err(RuntimeError::TypeError(
            "Object.values: argument is not an object".to_string(),
        )),
    }
}

pub fn object_entries(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.entries requires at least 1 argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            let keys = borrowed.own_keys();
            let entries: Vec<Value> = keys
                .iter()
                .filter_map(|k| {
                    borrowed.property_get(k).map(|desc| {
                        let key_str = k.display();
                        new_array_object_from_vec(vec![Value::string(&key_str), desc.value])
                    })
                })
                .collect();
            Ok(new_array_object_from_vec(entries))
        }
        _ => Ok(new_array_object_from_vec(vec![])),
    }
}

pub fn object_define_property(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 3 {
        return Err(RuntimeError::TypeError(
            "Object.defineProperty requires at least 3 arguments".to_string(),
        ));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.defineProperty: first argument must be an object".to_string(),
            ));
        }
    };
    let key = match &args[1] {
        Value::String(s) => PropertyKey::from_str(s),
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.defineProperty: property key must be a string".to_string(),
            ));
        }
    };
    let desc = &args[2];

    // Honour the full descriptor (value/writable/enumerable/configurable and
    // get/set) instead of only copying the value.
    if let Value::Object(desc_obj) = desc {
        let keys = desc_obj.borrow().own_keys();
        if keys.is_empty() {
            return Err(RuntimeError::TypeError(
                "Property description must be an object".to_string(),
            ));
        }
        let desc_val = desc.clone();
        let mut obj_mut = obj.borrow_mut();
        let ordinary = obj_mut
            .as_any_mut()
            .downcast_mut::<crate::vm::object::OrdinaryObject>();
        if let Some(ordinary) = ordinary {
            apply_property_descriptor(ordinary, &key, &desc_val)?;
        } else {
            // Non-ordinary object (array/function/…): fall back to the value.
            let value = desc_obj
                .borrow()
                .property_get(&PropertyKey::from_str("value"))
                .map(|d| d.value)
                .unwrap_or(Value::Undefined);
            obj_mut
                .property_set(key, value)
                .map_err(RuntimeError::TypeError)?;
        }
    }

    Ok(Value::Object(Rc::clone(obj)))
}

pub fn object_get_own_property_descriptor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.getOwnPropertyDescriptor requires at least 2 arguments".to_string(),
        ));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => {
            return Err(RuntimeError::TypeError(
                format!("Object.getOwnPropertyDescriptor: first argument must be an object (got {:?}, argc={})", args, args.len()),
            ));
        }
    };
    let key = match &args[1] {
        Value::String(s) => PropertyKey::from_str(s),
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.getOwnPropertyDescriptor: property key must be a string".to_string(),
            ));
        }
    };

    let borrowed = obj.borrow();
    if let Some(desc) = borrowed.property_get(&key) {
        // Create a descriptor object
        let mut desc_obj = crate::vm::object::OrdinaryObject::new();
        if desc.is_accessor_descriptor() {
            // Accessor descriptor: report get/set, never value/writable.
            let undefined = Value::Undefined;
            desc_obj
                .property_set(
                    PropertyKey::from_str("get"),
                    desc.getter.clone().unwrap_or(undefined.clone()),
                )
                .map_err(|e| RuntimeError::TypeError(e))?;
            desc_obj
                .property_set(
                    PropertyKey::from_str("set"),
                    desc.setter.clone().unwrap_or(undefined),
                )
                .map_err(|e| RuntimeError::TypeError(e))?;
        } else {
            desc_obj
                .property_set(PropertyKey::from_str("value"), desc.value.clone())
                .map_err(|e| RuntimeError::TypeError(e))?;
            desc_obj
                .property_set(
                    PropertyKey::from_str("writable"),
                    Value::Bool(desc.writable),
                )
                .map_err(|e| RuntimeError::TypeError(e))?;
        }
        desc_obj
            .property_set(
                PropertyKey::from_str("enumerable"),
                Value::Bool(desc.enumerable),
            )
            .map_err(|e| RuntimeError::TypeError(e))?;
        desc_obj
            .property_set(
                PropertyKey::from_str("configurable"),
                Value::Bool(desc.configurable),
            )
            .map_err(|e| RuntimeError::TypeError(e))?;
        Ok(Value::Object(Rc::new(RefCell::new(desc_obj))))
    } else {
        Ok(Value::Undefined)
    }
}

pub fn object_get_own_property_names(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.getOwnPropertyNames requires at least 1 argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            let keys = borrowed.own_keys();
            let str_keys: Vec<Value> = keys.iter().map(|k| Value::string(&k.display())).collect();
            Ok(new_array_object_from_vec(str_keys))
        }
        _ => Ok(new_array_object_from_vec(vec![])),
    }
}

pub fn object_get_prototype_of(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.getPrototypeOf requires at least 1 argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            if let Some(proto) = borrowed.get_prototype() {
                Ok(Value::Object(Rc::clone(&proto)))
            } else {
                Ok(Value::Null)
            }
        }
        _ => Ok(Value::Null),
    }
}

pub fn object_set_prototype_of(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.setPrototypeOf requires at least 2 arguments".to_string(),
        ));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.setPrototypeOf: first argument must be an object".to_string(),
            ));
        }
    };
    let proto = match &args[1] {
        Value::Object(proto_ref) => Some(Rc::clone(proto_ref)),
        Value::Null => None,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.setPrototypeOf: prototype must be an object or null".to_string(),
            ));
        }
    };
    obj.borrow_mut().set_prototype(proto);
    Ok(Value::Object(Rc::clone(obj)))
}

pub fn object_create(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.create requires at least 1 argument".to_string(),
        ));
    }
    let proto = match &args[0] {
        Value::Object(proto_ref) => Some(Rc::clone(proto_ref)),
        Value::Null => None,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.create: prototype must be an object or null".to_string(),
            ));
        }
    };
    let mut new_obj = crate::vm::object::OrdinaryObject::new();
    new_obj.set_prototype(proto);

    // Second argument: property descriptors, each applied in order.
    if let Some(props) = args.get(1) {
        if !props.is_undefined() && !props.is_null() {
            let props_ref = match props {
                Value::Object(o) => Rc::clone(o),
                _ => {
                    return Err(RuntimeError::TypeError(
                        "Object.create: properties must be an object".to_string(),
                    ));
                }
            };
            let keys = props_ref.borrow().own_keys();
            for key in keys {
                let desc = props_ref
                    .borrow()
                    .property_get(&key)
                    .map(|d| d.value)
                    .unwrap_or(Value::Undefined);
                apply_property_descriptor(&mut new_obj, &key, &desc)?;
            }
        }
    }

    Ok(Value::Object(Rc::new(RefCell::new(new_obj))))
}

/// Apply a property descriptor object (as consumed by `Object.defineProperty`
/// and the `properties` argument of `Object.create`).
///
/// Only the attributes the descriptor actually mentions are changed; the rest
/// default to `false` for `configurable`/`enumerable`/`writable`, exactly as
/// the spec's `ToPropertyDescriptor` requires.
pub fn apply_property_descriptor(
    obj: &mut crate::vm::object::OrdinaryObject,
    key: &PropertyKey,
    desc: &Value,
) -> Result<bool, RuntimeError> {
    let desc_ref = match desc {
        Value::Object(o) => o,
        _ => {
            return Err(RuntimeError::TypeError(
                "Property description must be an object".to_string(),
            ));
        }
    };
    let read = |name: &str| -> Option<Value> {
        desc_ref
            .borrow()
            .property_get(&PropertyKey::from_str(name))
            .map(|d| d.value)
    };

    let getter = read("get");
    let setter = read("set");
    if getter.is_some() || setter.is_some() {
        let mut descriptor = PropertyDescriptor::accessor_descriptor(getter, setter);
        if let Some(v) = read("enumerable") {
            descriptor.enumerable = v.to_boolean();
        }
        if let Some(v) = read("configurable") {
            descriptor.configurable = v.to_boolean();
        }
        return obj
            .define_property(key.clone(), descriptor)
            .map_err(RuntimeError::TypeError);
    }

    let value = read("value").unwrap_or(Value::Undefined);
    let mut descriptor = PropertyDescriptor::data_descriptor(value);
    if let Some(v) = read("writable") {
        descriptor.writable = v.to_boolean();
    }
    if let Some(v) = read("enumerable") {
        descriptor.enumerable = v.to_boolean();
    }
    if let Some(v) = read("configurable") {
        descriptor.configurable = v.to_boolean();
    }
    obj.define_property(key.clone(), descriptor)
        .map_err(RuntimeError::TypeError)
}

pub fn object_has_own(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.hasOwn requires at least 2 arguments".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let key = match &args[1] {
                Value::String(s) => PropertyKey::from_str(s),
                _ => PropertyKey::from_str(&args[1].to_js_string()),
            };
            let borrowed = obj_ref.borrow();
            // Check if property exists directly on this object (not in prototype chain)
            // by checking if the property is in own_keys
            let own_keys = borrowed.own_keys();
            Ok(Value::Bool(own_keys.iter().any(|k| k == &key)))
        }
        _ => Ok(Value::Bool(false)),
    }
}

pub fn object_is(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.is requires at least 2 arguments".to_string(),
        ));
    }
    // SameValue algorithm: NaN === NaN, +0 !== -0
    let result = match (&args[0], &args[1]) {
        (Value::Number(a), Value::Number(b)) => {
            if a.is_nan() && b.is_nan() {
                true
            } else if *a == 0.0 && *b == 0.0 {
                // Check signs: +0 !== -0
                a.is_sign_positive() == b.is_sign_positive()
            } else {
                a == b
            }
        }
        (a, b) => a == b,
    };
    Ok(Value::Bool(result))
}

/// `Object.assign(target, ...sources)` — copies own enumerable properties.
pub fn object_assign(args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(target) = args.first() else {
        return Err(RuntimeError::TypeError(
            "Object.assign requires at least 1 argument".to_string(),
        ));
    };
    let Value::Object(target_ref) = target else {
        return Err(RuntimeError::TypeError(
            "Object.assign: target is not an object".to_string(),
        ));
    };
    for source in args.iter().skip(1) {
        if let Value::Object(source_ref) = source {
            let entries: Vec<(PropertyKey, Value)> = {
                let borrowed = source_ref.borrow();
                borrowed
                    .own_keys()
                    .into_iter()
                    .filter_map(|k| {
                        borrowed
                            .property_get(&k)
                            .filter(|d| d.enumerable)
                            .map(|d| (k, d.value))
                    })
                    .collect()
            };
            for (key, value) in entries {
                let _ = target_ref.borrow_mut().property_set(key, value);
            }
        }
    }
    Ok(target.clone())
}

/// `Object.defineProperties(obj, props)`
pub fn object_define_properties(args: &[Value]) -> Result<Value, RuntimeError> {
    let (Some(target), Some(props)) = (args.first(), args.get(1)) else {
        return Err(RuntimeError::TypeError(
            "Object.defineProperties requires 2 arguments".to_string(),
        ));
    };
    let Value::Object(target_ref) = target else {
        return Err(RuntimeError::TypeError(
            "Object.defineProperties: target is not an object".to_string(),
        ));
    };
    if let Value::Object(props_ref) = props {
        let descriptors: Vec<(PropertyKey, Value)> = {
            let borrowed = props_ref.borrow();
            borrowed
                .own_keys()
                .into_iter()
                .filter_map(|k| borrowed.property_get(&k).map(|d| (k, d.value)))
                .collect()
        };
        for (key, descriptor) in descriptors {
            object_define_property(&[target.clone(), Value::string(&key.display()), descriptor])?;
        }
    }
    Ok(target.clone())
}

/// `Object.prototype.toString.call(value)` — the spec's [[Class]] based form.
pub fn object_prototype_to_string(obj: &Value, _args: &[Value]) -> Result<Value, RuntimeError> {
    let class = match obj {
        Value::Object(o) => o.borrow().class_name().to_string(),
        Value::Undefined => "Undefined".to_string(),
        Value::Null => "Null".to_string(),
        Value::Bool(_) => "Boolean".to_string(),
        Value::Number(_) => "Number".to_string(),
        Value::String(_) => "String".to_string(),
        Value::Symbol(_) => "Symbol".to_string(),
        Value::Function(_) => "Function".to_string(),
    };
    Ok(Value::string(&format!("[object {class}]")))
}

/// `Object.prototype.hasOwnProperty(V)`
pub fn object_has_own_property(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = match args.first() {
        Some(Value::String(s)) => PropertyKey::from_str(s),
        Some(other) => PropertyKey::from_str(&other.to_js_string()),
        None => return Ok(Value::Bool(false)),
    };
    match obj {
        Value::Object(obj_ref) => Ok(Value::Bool(obj_ref.borrow().has_property(&key))),
        Value::String(s) => Ok(Value::Bool(key.as_str() == Some("length"))),
        _ => Ok(Value::Bool(false)),
    }
}

/// Name of the `Object.prototype.toString` native.
///
/// It needs a dedicated name because `Object.prototype.toString.call(x)` must
/// report the [[Class]] of `x` even when `x` is an Array or Number, whereas the
/// generic `toString` dispatch delegates to the receiver's own type.
pub const OBJECT_TO_STRING_NATIVE: &str = "Object.prototype.toString";

/// Populate `Object.prototype` with the methods every object inherits.
pub fn register_object_prototype(proto: &Rc<RefCell<dyn JSObject>>, object_fn: &Value) {
    use super::set_prototype_method;

    // Dedicated native so that `.call(receiver)` keeps Array/Number semantics.
    let _ = proto.borrow_mut().property_set(
        PropertyKey::from_str("toString"),
        Value::Object(Rc::new(RefCell::new(
            crate::vm::object::NativeFunctionObject::new(OBJECT_TO_STRING_NATIVE),
        ))),
    );
    set_prototype_method(proto, "valueOf", |this, _args| Ok(this.clone()));
    set_prototype_method(proto, "hasOwnProperty", |this, args| {
        object_has_own_property(this, args)
    });
    set_prototype_method(proto, "toLocaleString", |this, args| {
        object_prototype_to_string(this, args)
    });
    let _ = proto
        .borrow_mut()
        .property_set(PropertyKey::from_str("constructor"), object_fn.clone());
}

pub fn register_object_statics(object_fn: &Value, _builtins: &super::Builtins) {
    set_static_method(object_fn, "keys", |args| object_keys(args));
    set_static_method(object_fn, "values", |args| object_values(args));
    set_static_method(object_fn, "entries", |args| object_entries(args));
    set_static_method(object_fn, "assign", |args| object_assign(args));
    set_static_method(object_fn, "defineProperty", |args| {
        object_define_property(args)
    });
    set_static_method(object_fn, "defineProperties", |args| {
        object_define_properties(args)
    });
    set_static_method(object_fn, "getOwnPropertyDescriptor", |args| {
        object_get_own_property_descriptor(args)
    });
    set_static_method(object_fn, "getOwnPropertyNames", |args| {
        object_get_own_property_names(args)
    });
    set_static_method(object_fn, "getPrototypeOf", |args| {
        object_get_prototype_of(args)
    });
    set_static_method(object_fn, "setPrototypeOf", |args| {
        object_set_prototype_of(args)
    });
    set_static_method(object_fn, "create", |args| object_create(args));
    set_static_method(object_fn, "hasOwn", |args| object_has_own(args));
    set_static_method(object_fn, "is", |args| object_is(args));
    set_static_method(object_fn, "isExtensible", |args| Ok(Value::Bool(false)));
    set_static_method(object_fn, "isFrozen", |args| Ok(Value::Bool(false)));
    set_static_method(object_fn, "isSealed", |args| Ok(Value::Bool(false)));
    set_static_method(object_fn, "preventExtensions", |args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
    });
    set_static_method(object_fn, "seal", |args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
    });
    set_static_method(object_fn, "freeze", |args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
    });
}
