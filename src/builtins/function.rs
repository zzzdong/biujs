use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::NativeFunctionObject;
use crate::vm::property::{ObjectKind, PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// `%ThrowTypeError%`：严格模式函数的 `caller` / `arguments` 访问器（ES 9.2.7）。
/// 读写这个访问器都必须抛 TypeError，所以 getter 与 setter 指向同一个内建。
pub const FUNCTION_THROW_TYPE_ERROR_NATIVE: &str = "__throw_type_error__";

/// `Function.prototype.toString` 的内建名。
///
/// 它**必须**是独立的名字，不能沿用属性名 `toString`：分派是按内建名做的，
/// `Object.prototype.toString` 与它同名，两者会被混成一件事（这正是本方法
/// 长时间未接线、`f.toString()` 返回 `"[object Function]"` 的原因）。
pub const FUNCTION_PROTO_TO_STRING_NATIVE: &str = "Function.prototype.toString";

/// Function constructor
/// In ES6 without eval, calling `new Function(str)` throws a SyntaxError
pub fn function_constructor(_heap: &mut Heap, _args: &[Value]) -> Result<Value, RuntimeError> {
    Err(RuntimeError::TypeError(
        "Function constructor is not supported (eval-like features are disabled)".to_string(),
    ))
}

/// `Function.prototype.toString()` (ES 19.2.3.5).
///
/// 引擎不保留源码文本，所以答案取规范允许的**实现自定义**形式：
/// `function name(...) { [native code] }`。test262 的
/// `assertToStringOrNativeFunction` 明确接受这种形式；要点是它必须是**函数语法**
/// 且带上函数自己的 `name`，而不是 `Object.prototype.toString` 的
/// `"[object Function]"`。
///
/// `name` 只有在能原样写成 `IdentifierName`（访问器是 `get `/`set ` + 名字）
/// 时才内联，否则省略名字 —— 例如 bound function 的 `"bound f"` 是合法 `name`
/// 但不是合法标识符，直接拼接会产出非函数语法的字符串。
pub fn function_prototype_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    let Value::Object(obj_ref) = obj else {
        return Err(RuntimeError::TypeError(
            "Function.prototype.toString called on non-function".to_string(),
        ));
    };
    let name = {
        let borrowed = obj_ref.borrow();
        if !matches!(
            borrowed.kind(),
            ObjectKind::Function | ObjectKind::NativeFunction
        ) {
            return Err(RuntimeError::TypeError(
                "Function.prototype.toString called on non-function".to_string(),
            ));
        }
        borrowed
            .property_get(&PropertyKey::well_known("name").expect("well-known name"))
            .map(|desc| desc.value.to_js_string())
            .unwrap_or_default()
    };
    Ok(Value::string(&format!(
        "function {}() {{ [native code] }}",
        function_source_name(&name)
    )))
}

/// `name` → 可以紧跟在 `function ` 后面的文本（含访问器前缀）。
fn function_source_name(name: &str) -> &str {
    for prefix in ["get ", "set "] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if is_identifier_name(rest) {
                return name;
            }
        }
    }
    if is_identifier_name(name) {
        name
    } else {
        ""
    }
}

/// 够用的 `IdentifierName` 判定：首字符是字母/`_`/`$`，其余是字母数字/`_`/`$`。
fn is_identifier_name(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
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

    // `call` / `apply` / `bind` 的实现住 VM（要拿 frame 与 `this`），这里只放
    // 名字与描述符。
    for name in ["call", "apply", "bind"] {
        let key = PropertyKey::intern(heap, name);
        let value = super::method_descriptor(Value::Object(heap.alloc(
            Object::NativeFunction(NativeFunctionObject::new(name)),
        )));
        p.define_property(heap, key, value).ok();
    }

    // `Function.prototype.toString` —— 独立的**内建名**（见常量注释）。
    {
        let key = PropertyKey::intern(heap, "toString");
        let value = super::method_descriptor(Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(FUNCTION_PROTO_TO_STRING_NATIVE),
        ))));
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
