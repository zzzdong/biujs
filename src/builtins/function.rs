use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::{JSObject, NativeFunctionObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// `%ThrowTypeError%`：严格模式函数的 `caller` / `arguments` 访问器（ES 9.2.7）。
/// 读写这个访问器都必须抛 TypeError，所以 getter 与 setter 指向同一个内建。
pub const FUNCTION_THROW_TYPE_ERROR_NATIVE: &str = "__throw_type_error__";

/// Function constructor
/// In ES6 without eval, calling `new Function(str)` throws a SyntaxError
pub fn function_constructor(_heap: &mut Heap, _args: &[Value]) -> Result<Value, RuntimeError> {
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
        obj.define_property(heap, PropertyKey::well_known("prototype").expect("well-known name"),
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
        let key = PropertyKey::intern(heap, name);
        let value = super::method_descriptor(Value::Object(heap.alloc(
            Object::NativeFunction(NativeFunctionObject::new(name)),
        )));
        p.define_property(heap, key, value).ok();
    }

    // ES 9.2.7 / 16.2：严格模式函数继承 `Function.prototype` 上的
    // `caller` / `arguments` 访问器，其 [[Get]] / [[Set]] 都是 %ThrowTypeError%。
    // 没有它时 `fn.caller` 只是 undefined，一整族 strict-mode 用例不抛错。
    let thrower = Value::Object(heap.alloc(Object::NativeFunction(
        NativeFunctionObject::new(FUNCTION_THROW_TYPE_ERROR_NATIVE),
    )));
    for name in ["caller", "arguments"] {
        let key = PropertyKey::intern(heap, name);
        let desc = PropertyDescriptor {
            value: Value::Undefined,
            writable: false,
            enumerable: false,
            // ES 19.2.3.3 / 19.2.3.4：两个访问器都是 configurable（以前写成
            // false，`Function.prototype.caller.prop-desc.js` 因此失败）。
            configurable: true,
            getter: Some(thrower.clone()),
            setter: Some(thrower.clone()),
        };
        p.define_property(heap, key, desc).ok();
    }

    // ES 20.2.3.6：`Function.prototype[@@hasInstance]`。直接调
    // `OrdinaryHasInstance(this, V)` —— 它不走 `@@hasInstance` 再分派
    // （那样会自递归），`this` 不可调用时返回 false。
    let has_instance = Value::Object(heap.alloc(Object::NativeFunction(
        NativeFunctionObject::new(super::FUNCTION_HAS_INSTANCE_NATIVE),
    )));
    if let Value::Object(fn_ref) = &has_instance {
        if let Object::NativeFunction(nf) = &mut *fn_ref.borrow_mut() {
            nf.set_length(1);
        }
    }
    p.define_property(
        heap,
        super::has_instance_symbol_key(),
        PropertyDescriptor {
            value: has_instance,
            writable: false,
            enumerable: false,
            configurable: false,
            getter: None,
            setter: None,
        },
    )
    .ok();
}
