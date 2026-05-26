use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::{ArrayObject, JSObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;
use crate::RuntimeError;

// ─────────────────────────────────────────────────────────
// Prototype registration
// ─────────────────────────────────────────────────────────

pub fn register_array_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;
    
    set_prototype_method(proto, "push", |this, args| array_push(this, args));
    set_prototype_method(proto, "pop", |this, _args| array_pop(this));
    set_prototype_method(proto, "shift", |this, _args| array_shift(this));
    set_prototype_method(proto, "unshift", |this, args| array_unshift(this, args));
    set_prototype_method(proto, "indexOf", |this, args| array_index_of(this, args));
    set_prototype_method(proto, "includes", |this, args| array_includes(this, args));
    set_prototype_method(proto, "join", |this, args| array_join(this, args));
    set_prototype_method(proto, "slice", |this, args| array_slice(this, args));
    set_prototype_method(proto, "concat", |this, args| array_concat(this, args));
    set_prototype_method(proto, "splice", |this, args| array_splice(this, args));
}

// ─────────────────────────────────────────────────────────
// Array constructor
// ─────────────────────────────────────────────────────────

pub fn array_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() == 1 {
        if let Value::Number(n) = args[0] {
            let len = n as usize;
            if (n - len as f64).abs() > f64::EPSILON || n.is_nan() || n.is_infinite() || n.is_sign_negative() {
                return Err(RuntimeError::RangeError("Invalid array length".to_string()));
            }
            let mut arr = ArrayObject::with_capacity(len);
            for _ in 0..len {
                arr.push(Value::Undefined);
            }
            return Ok(Value::Object(Rc::new(RefCell::new(arr))));
        }
    }
    let mut arr = ArrayObject::new();
    for arg in args {
        arr.push(arg.clone());
    }
    Ok(Value::Object(Rc::new(RefCell::new(arr))))
}

// ─────────────────────────────────────────────────────────
// Prototype methods
// ─────────────────────────────────────────────────────────

pub fn array_push(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                for arg in args {
                    array_obj.push(arg.clone());
                }
                Ok(Value::Number(array_obj.len() as f64))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_pop(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                Ok(array_obj.pop())
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_shift(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                Ok(array_obj.shift())
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_unshift(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                for arg in args.iter().rev() {
                    array_obj.unshift(arg.clone());
                }
                Ok(Value::Number(array_obj.len() as f64))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_index_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                if args.is_empty() { return Ok(Value::Number(-1.0)); }
                let from = if args.len() < 2 { 0 } else { args[1].to_number() as usize };
                Ok(Value::Number(array_obj.index_of(&args[0], from) as f64))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_includes(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                if args.is_empty() { return Ok(Value::Bool(false)); }
                Ok(Value::Bool(array_obj.includes(&args[0])))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_join(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                let sep = if args.is_empty() { "," } else { &args[0].to_js_string() };
                Ok(Value::string(&array_obj.join(sep)))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_slice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                let len = array_obj.len();
                let start = if args.is_empty() { 0 } else {
                    let n = args[0].to_number() as i64;
                    if n < 0 { ((len as i64) + n).max(0) as usize } else { (n as usize).min(len) }
                };
                let end = if args.len() < 2 { len } else {
                    let n = args[1].to_number() as i64;
                    if n < 0 { ((len as i64) + n).max(0) as usize } else { (n as usize).min(len) }
                };
                let sliced = array_obj.slice(start, end);
                Ok(Value::Object(Rc::new(RefCell::new(sliced))))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_concat(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let arr = obj_ref.borrow();
            if let Some(array_obj) = arr.as_any().downcast_ref::<ArrayObject>() {
                let mut elements: Vec<Value> = (0..array_obj.len()).map(|i| array_obj.get(i).cloned().unwrap_or(Value::Undefined)).collect();
                for arg in args {
                    // Flatten array arguments
                    if let Value::Object(arg_ref) = arg {
                        let borrowed = arg_ref.borrow();
                        if borrowed.kind() == crate::vm::ObjectKind::Array {
                            if let Some(other_arr) = borrowed.as_any().downcast_ref::<ArrayObject>() {
                                for i in 0..other_arr.len() {
                                    elements.push(other_arr.get(i).cloned().unwrap_or(Value::Undefined));
                                }
                                continue;
                            }
                        }
                    }
                    elements.push(arg.clone());
                }
                Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(elements)))))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}

pub fn array_splice(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let mut arr = obj_ref.borrow_mut();
            if let Some(array_obj) = arr.as_any_mut().downcast_mut::<ArrayObject>() {
                let len = array_obj.len();
                let start = if args.is_empty() { 0 } else {
                    let n = args[0].to_number() as i64;
                    if n < 0 { ((len as i64) + n).max(0) as usize } else { (n as usize).min(len) }
                };
                let delete_count = if args.len() < 2 { len - start } else {
                    (args[1].to_number() as usize).min(len - start)
                };
                let insert_items: Vec<Value> = if args.len() > 2 { args[2..].to_vec() } else { vec![] };
                let removed = array_obj.splice(start, delete_count, &insert_items);
                Ok(Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(removed)))))
            } else {
                Err(RuntimeError::TypeError("not an array".to_string()))
            }
        }
        _ => Err(RuntimeError::TypeError("not an array".to_string())),
    }
}
