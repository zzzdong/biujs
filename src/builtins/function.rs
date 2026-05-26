use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::{NativeFunctionObject, JSObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;
use crate::RuntimeError;

/// Function constructor
/// In ES6 without eval, calling `new Function(str)` throws a SyntaxError
pub fn function_constructor(_args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::TypeError(
        "Function constructor is not supported (eval-like features are disabled)".to_string(),
    ))
}

/// Function.prototype.call(thisArg, ...args)
pub fn function_prototype_call(args: &[Value]) -> Result<Value, RuntimeError> {
    // call needs at least 1 arg (thisArg) and a `this` context that is the function
    Err(RuntimeError::TypeError(
        "Function.prototype.call is not yet fully supported".to_string(),
    ))
}

/// Function.prototype.apply(thisArg, argsArray)
pub fn function_prototype_apply(args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::TypeError(
        "Function.prototype.apply is not yet fully supported".to_string(),
    ))
}

/// Function.prototype.bind(thisArg, ...args)
pub fn function_prototype_bind(args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::TypeError(
        "Function.prototype.bind is not yet fully supported".to_string(),
    ))
}

/// Function.prototype.toString()
pub fn function_prototype_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    let name = match obj {
        Value::Function(_) => "anonymous".to_string(),
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            if borrowed.kind() == crate::vm::property::ObjectKind::NativeFunction {
                if let Some(nf) = borrowed.as_any().downcast_ref::<NativeFunctionObject>() {
                    nf.name.clone()
                } else {
                    "anonymous".to_string()
                }
            } else {
                "anonymous".to_string()
            }
        }
        _ => return Err(RuntimeError::TypeError(
            "Function.prototype.toString called on non-function".to_string(),
        )),
    };
    Ok(Value::string(&format!("function {}() {{ [native code] }}", name)))
}

/// Register static methods on the Function constructor
pub fn register_function_statics(
    function_fn_val: &Value,
    function_prototype: &Rc<RefCell<dyn JSObject>>,
) {
    if let Value::Object(obj_ref) = function_fn_val {
        let mut obj = obj_ref.borrow_mut();
        // .prototype property
        obj.property_set(
            PropertyKey::from_str("prototype"),
            Value::Object(Rc::clone(function_prototype)),
        ).ok();
    }
}

/// Set up Function.prototype with standard methods
pub fn setup_function_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    let mut p = proto.borrow_mut();

    // These are placeholder prototypes — actual implementation requires this-aware calling
    p.property_set(
        PropertyKey::from_str("call"),
        Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("call"),
        ))),
    ).ok();

    p.property_set(
        PropertyKey::from_str("apply"),
        Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("apply"),
        ))),
    ).ok();

    p.property_set(
        PropertyKey::from_str("bind"),
        Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("bind"),
        ))),
    ).ok();

    p.property_set(
        PropertyKey::from_str("toString"),
        Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("toString"),
        ))),
    ).ok();
}
