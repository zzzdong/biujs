use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::vm::object::{JSObject, OrdinaryObject};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;

// ─────────────────────────────────────────────────────────
// Error types
// ─────────────────────────────────────────────────────────

/// Error type identifier for different error constructors
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrorType {
    Error,
    TypeError,
    ReferenceError,
    RangeError,
    SyntaxError,
    URIError,
    EvalError,
}

impl ErrorType {
    pub fn name(&self) -> &'static str {
        match self {
            ErrorType::Error => "Error",
            ErrorType::TypeError => "TypeError",
            ErrorType::ReferenceError => "ReferenceError",
            ErrorType::RangeError => "RangeError",
            ErrorType::SyntaxError => "SyntaxError",
            ErrorType::URIError => "URIError",
            ErrorType::EvalError => "EvalError",
        }
    }
}

// ─────────────────────────────────────────────────────────
// Error constructors
// ─────────────────────────────────────────────────────────

/// `{ writable: true, enumerable: false, configurable: true }` — the attribute
/// set of an error's own `message` (ES 20.5.1.1).
fn error_message_descriptor(value: Value) -> crate::vm::property::PropertyDescriptor {
    crate::vm::property::PropertyDescriptor {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    }
}

/// Create an Error object with the given message and prototype.
///
/// Only `message` is an own property: `name` and `toString` are inherited from
/// the error prototype (ES 20.5.3.1 only defines it on the prototype), and an
/// error constructed without a message has no own `message` at all.
pub fn create_error_object(
    message: Option<String>,
    prototype: Rc<RefCell<dyn JSObject>>,
    _name: &str,
) -> Value {
    let mut obj = OrdinaryObject::with_prototype(Rc::clone(&prototype));
    if let Some(msg) = message {
        obj.define_property(
            PropertyKey::from_str("message"),
            error_message_descriptor(Value::string(&msg)),
        )
        .ok();
    }
    Value::Object(Rc::new(RefCell::new(obj)))
}

/// Generic error constructor dispatcher
/// Note: The actual prototype is determined by the constructor function's [[Prototype]]
/// which is set up in Builtins::register. This function creates the error object
/// and the VM will set the correct prototype based on the constructor.
pub fn error_constructor(error_type: ErrorType, args: &[Value]) -> Result<Value, RuntimeError> {
    // `new Error()` has no own `message`; `new Error(undefined)` neither. The
    // VM installs the constructor's `prototype` afterwards.
    let message = match args.first() {
        None | Some(Value::Undefined) => None,
        Some(v) => Some(v.to_js_string()),
    };

    // `Error(...)` called *without* `new` must still produce an object whose
    // `[[Prototype]]` is the matching error prototype (ES 20.5.1.1 step 3).
    let mut obj = match crate::builtins::wrapper_prototype(error_type.name()) {
        Some(proto) => OrdinaryObject::with_prototype(proto),
        None => OrdinaryObject::new(),
    };
    if let Some(msg) = message {
        obj.define_property(
            PropertyKey::from_str("message"),
            error_message_descriptor(Value::string(&msg)),
        )
        .ok();
    }

    Ok(Value::Object(Rc::new(RefCell::new(obj))))
}

// ─────────────────────────────────────────────────────────
// RuntimeError to JS Error conversion
// ─────────────────────────────────────────────────────────

/// Convert a RuntimeError to a JS Error object Value
pub fn runtime_error_to_js_error(
    err: &RuntimeError,
    builtins: &crate::builtins::Builtins,
) -> Value {
    match err {
        RuntimeError::TypeError(msg) => create_error_object(
            Some(msg.clone()),
            Rc::clone(&builtins.type_error_prototype),
            "TypeError",
        ),
        RuntimeError::ReferenceError(msg) => create_error_object(
            Some(msg.clone()),
            Rc::clone(&builtins.reference_error_prototype),
            "ReferenceError",
        ),
        RuntimeError::RangeError(msg) => create_error_object(
            Some(msg.clone()),
            Rc::clone(&builtins.range_error_prototype),
            "RangeError",
        ),
        RuntimeError::SyntaxError(msg) => create_error_object(
            Some(msg.clone()),
            Rc::clone(&builtins.error_prototype),
            "SyntaxError",
        ),
        RuntimeError::InternalError(msg) => create_error_object(
            Some(msg.clone()),
            Rc::clone(&builtins.error_prototype),
            "Error",
        ),
        RuntimeError::NotImplemented(msg) => create_error_object(
            Some(format!("Not implemented: {}", msg)),
            Rc::clone(&builtins.error_prototype),
            "Error",
        ),
        RuntimeError::Thrown(val) => val.clone(),
    }
}

// ─────────────────────────────────────────────────────────
// Error prototype methods
// ─────────────────────────────────────────────────────────

/// Error.prototype.toString()
pub fn error_prototype_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();

            // Get name property, default to "Error"
            let name = borrowed
                .property_get(&PropertyKey::from_str("name"))
                .map(|d| d.value.to_js_string())
                .unwrap_or_else(|| "Error".to_string());

            // Get message property, default to empty string
            let message = borrowed
                .property_get(&PropertyKey::from_str("message"))
                .map(|d| d.value.to_js_string())
                .unwrap_or_default();

            // Format: "name: message" or just "name" if no message
            let result = if message.is_empty() {
                name
            } else {
                format!("{}: {}", name, message)
            };

            Ok(Value::string(&result))
        }
        _ => {
            // For non-objects, return a generic error string
            Ok(Value::string("Error"))
        }
    }
}

// ─────────────────────────────────────────────────────────
// Setup error prototype methods
// ─────────────────────────────────────────────────────────

/// Setup `Error.prototype`: `name`, `message` and `toString`
/// (ES 20.5.3.1–20.5.3.4; `constructor` is wired up by the caller).
pub fn setup_error_prototype(
    proto: &Rc<RefCell<dyn JSObject>>,
    _builtins: &crate::builtins::Builtins,
) {
    use crate::builtins::set_prototype_method;
    use crate::vm::property::PropertyDescriptor;

    // `Error.prototype.name` / `.message` are writable, non-enumerable,
    // configurable; `toString` is an ordinary built-in method.
    let writable_string = |value: Value| PropertyDescriptor {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    };
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str("name"), writable_string(Value::string("Error")));
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str("message"), writable_string(Value::string("")));

    // Error.prototype.toString
    set_prototype_method(proto, "toString", |this, _args| {
        error_prototype_to_string(this)
    });
}

/// Install the own `name` of a native error prototype (`TypeError.prototype.name`
/// is `"TypeError"`; everything else is inherited from `Error.prototype`).
pub fn setup_native_error_prototype(proto: &Rc<RefCell<dyn JSObject>>, name: &str) {
    use crate::vm::property::PropertyDescriptor;

    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("name"),
        PropertyDescriptor {
            value: Value::string(name),
            writable: true,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        },
    );
}
