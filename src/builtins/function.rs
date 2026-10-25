use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::{JSObject, NativeFunctionObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;

/// Function constructor
/// In ES6 without eval, calling `new Function(str)` throws a SyntaxError
pub fn function_constructor(heap: &mut Heap, _args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::TypeError(
        "Function constructor is not supported (eval-like features are disabled)".to_string(),
    ))
}

/// `Function.prototype.toString()`
pub fn function_prototype_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    let name = match obj {
        Value::Function(_) => "anonymous".to_string(),
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            if borrowed.kind() == crate::vm::property::ObjectKind::NativeFunction {
                if let Some(nf) = borrowed.as_native_function() {
                    nf.name.clone()
                } else {
                    "anonymous".to_string()
                }
            } else {
                "anonymous".to_string()
            }
        }
        _ => {
            return Err(RuntimeError::TypeError(
                "Function.prototype.toString called on non-function".to_string(),
            ));
        }
    };
    Ok(Value::string(&format!(
        "function {}() {{ [native code] }}",
        name
    )))
}

/// Register static methods on the Function constructor
pub fn register_function_statics(
    function_fn_val: &Value,
    function_prototype: &Gc<Object>,
    heap: &mut Heap,
) {
    if let Value::Object(obj_ref) = function_fn_val {
        let mut obj = obj_ref.borrow_mut();
        // `.prototype` is writable, non-enumerable, non-configurable.
        obj.define_property(
            PropertyKey::well_known("prototype").expect("well-known name"),
            super::constructor_prototype_descriptor(Value::Object(*function_prototype)),
        )
        .ok();
    }
}

/// Set up Function.prototype with standard methods
pub fn setup_function_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    let mut p = proto.borrow_mut();

    // These are placeholder prototypes — actual implementation requires this-aware calling
    for name in ["call", "apply", "bind", "toString"] {
        p.define_property(
            PropertyKey::intern(heap, name),
            super::method_descriptor(Value::Object(heap.alloc(Object::NativeFunction(
                NativeFunctionObject::new(name),
            )))),
        )
        .ok();
    }
}
