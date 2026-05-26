use crate::vm::object::{new_array_object_from_vec, JSObject, OrdinaryObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;
use crate::RuntimeError;
use std::cell::RefCell;
use std::rc::Rc;

use super::set_static_method;

pub fn object_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        let obj = crate::vm::object::OrdinaryObject::new();
        Ok(Value::Object(std::rc::Rc::new(std::cell::RefCell::new(obj))))
    } else {
        Ok(args[0].clone())
    }
}

pub fn object_keys(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError("Object.keys requires at least 1 argument".to_string()));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            let keys = borrowed.own_keys();
            let str_keys: Vec<Value> = keys.iter().map(|k| Value::string(&k.display())).collect();
            Ok(new_array_object_from_vec(str_keys))
        }
        _ => Err(RuntimeError::TypeError("Object.keys: argument is not an object".to_string())),
    }
}

pub fn object_values(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError("Object.values requires at least 1 argument".to_string()));
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
        _ => Err(RuntimeError::TypeError("Object.values: argument is not an object".to_string())),
    }
}

pub fn object_entries(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError("Object.entries requires at least 1 argument".to_string()));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            let keys = borrowed.own_keys();
            let entries: Vec<Value> = keys.iter()
                .filter_map(|k| {
                    borrowed.property_get(k).map(|desc| {
                        let key_str = k.display();
                        new_array_object_from_vec(vec![
                            Value::string(&key_str),
                            desc.value,
                        ])
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
        return Err(RuntimeError::TypeError("Object.defineProperty requires at least 3 arguments".to_string()));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => return Err(RuntimeError::TypeError("Object.defineProperty: first argument must be an object".to_string())),
    };
    let key = match &args[1] {
        Value::String(s) => PropertyKey::from_str(s),
        _ => return Err(RuntimeError::TypeError("Object.defineProperty: property key must be a string".to_string())),
    };
    let desc = &args[2];
    
    // For now, just set the value directly (simplified implementation)
    if let Value::Object(desc_obj) = desc {
        let borrowed = desc_obj.borrow();
        if let Some(value_desc) = borrowed.property_get(&PropertyKey::from_str("value")) {
            let mut obj_mut = obj.borrow_mut();
            obj_mut.property_set(key, value_desc.value.clone())
                .map_err(|e| RuntimeError::TypeError(e))?;
        }
    }
    
    Ok(Value::Object(Rc::clone(obj)))
}

pub fn object_get_own_property_descriptor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError("Object.getOwnPropertyDescriptor requires at least 2 arguments".to_string()));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => return Err(RuntimeError::TypeError("Object.getOwnPropertyDescriptor: first argument must be an object".to_string())),
    };
    let key = match &args[1] {
        Value::String(s) => PropertyKey::from_str(s),
        _ => return Err(RuntimeError::TypeError("Object.getOwnPropertyDescriptor: property key must be a string".to_string())),
    };
    
    let borrowed = obj.borrow();
    if let Some(desc) = borrowed.property_get(&key) {
        // Create a descriptor object
        let mut desc_obj = crate::vm::object::OrdinaryObject::new();
        desc_obj.property_set(PropertyKey::from_str("value"), desc.value.clone())
            .map_err(|e| RuntimeError::TypeError(e))?;
        desc_obj.property_set(PropertyKey::from_str("writable"), Value::Bool(desc.writable))
            .map_err(|e| RuntimeError::TypeError(e))?;
        desc_obj.property_set(PropertyKey::from_str("enumerable"), Value::Bool(desc.enumerable))
            .map_err(|e| RuntimeError::TypeError(e))?;
        desc_obj.property_set(PropertyKey::from_str("configurable"), Value::Bool(desc.configurable))
            .map_err(|e| RuntimeError::TypeError(e))?;
        Ok(Value::Object(Rc::new(RefCell::new(desc_obj))))
    } else {
        Ok(Value::Undefined)
    }
}

pub fn object_get_own_property_names(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError("Object.getOwnPropertyNames requires at least 1 argument".to_string()));
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
        return Err(RuntimeError::TypeError("Object.getPrototypeOf requires at least 1 argument".to_string()));
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
        return Err(RuntimeError::TypeError("Object.setPrototypeOf requires at least 2 arguments".to_string()));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => return Err(RuntimeError::TypeError("Object.setPrototypeOf: first argument must be an object".to_string())),
    };
    let proto = match &args[1] {
        Value::Object(proto_ref) => Some(Rc::clone(proto_ref)),
        Value::Null => None,
        _ => return Err(RuntimeError::TypeError("Object.setPrototypeOf: prototype must be an object or null".to_string())),
    };
    obj.borrow_mut().set_prototype(proto);
    Ok(Value::Object(Rc::clone(obj)))
}

pub fn object_create(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError("Object.create requires at least 1 argument".to_string()));
    }
    let proto = match &args[0] {
        Value::Object(proto_ref) => Some(Rc::clone(proto_ref)),
        Value::Null => None,
        _ => return Err(RuntimeError::TypeError("Object.create: prototype must be an object or null".to_string())),
    };
    let mut new_obj = crate::vm::object::OrdinaryObject::new();
    new_obj.set_prototype(proto);
    Ok(Value::Object(Rc::new(RefCell::new(new_obj))))
}

pub fn object_has_own(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError("Object.hasOwn requires at least 2 arguments".to_string()));
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
        return Err(RuntimeError::TypeError("Object.is requires at least 2 arguments".to_string()));
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

pub fn register_object_statics(object_fn: &Value, _builtins: &super::Builtins) {
    set_static_method(object_fn, "keys", |args| object_keys(args));
    set_static_method(object_fn, "values", |args| object_values(args));
    set_static_method(object_fn, "entries", |args| object_entries(args));
    set_static_method(object_fn, "defineProperty", |args| object_define_property(args));
    set_static_method(object_fn, "getOwnPropertyDescriptor", |args| object_get_own_property_descriptor(args));
    set_static_method(object_fn, "getOwnPropertyNames", |args| object_get_own_property_names(args));
    set_static_method(object_fn, "getPrototypeOf", |args| object_get_prototype_of(args));
    set_static_method(object_fn, "setPrototypeOf", |args| object_set_prototype_of(args));
    set_static_method(object_fn, "create", |args| object_create(args));
    set_static_method(object_fn, "hasOwn", |args| object_has_own(args));
    set_static_method(object_fn, "is", |args| object_is(args));
}
