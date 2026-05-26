use std::cell::RefCell;
use std::rc::Rc;

use crate::vm::object::{JSObject, OrdinaryObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;
use crate::RuntimeError;

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

/// Create an Error object with the given message and prototype
pub fn create_error_object(
    message: Option<String>,
    prototype: Rc<RefCell<dyn JSObject>>,
    name: &str,
) -> Value {
    let mut obj = OrdinaryObject::with_prototype(Rc::clone(&prototype));

    // Set message property
    let msg = message.unwrap_or_default();
    obj.property_set(
        PropertyKey::from_str("message"),
        Value::string(&msg),
    ).ok();

    // Set name property
    obj.property_set(
        PropertyKey::from_str("name"),
        Value::string(name),
    ).ok();

    Value::Object(Rc::new(RefCell::new(obj)))
}

/// Generic error constructor dispatcher
/// Note: The actual prototype is determined by the constructor function's [[Prototype]]
/// which is set up in Builtins::register. This function creates the error object
/// and the VM will set the correct prototype based on the constructor.
pub fn error_constructor(error_type: ErrorType, args: &[Value]) -> Result<Value, RuntimeError> {
    let message = args.first().map(|v| v.to_js_string());

    // Create a placeholder error object - the VM will set the correct prototype
    // based on the constructor function's prototype property
    let mut obj = OrdinaryObject::new();

    // Set message property
    let msg = message.unwrap_or_default();
    obj.property_set(
        PropertyKey::from_str("message"),
        Value::string(&msg),
    ).ok();

    // Set name property
    obj.property_set(
        PropertyKey::from_str("name"),
        Value::string(error_type.name()),
    ).ok();

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
        RuntimeError::TypeError(msg) => {
            create_error_object(
                Some(msg.clone()),
                Rc::clone(&builtins.type_error_prototype),
                "TypeError",
            )
        }
        RuntimeError::ReferenceError(msg) => {
            create_error_object(
                Some(msg.clone()),
                Rc::clone(&builtins.reference_error_prototype),
                "ReferenceError",
            )
        }
        RuntimeError::RangeError(msg) => {
            create_error_object(
                Some(msg.clone()),
                Rc::clone(&builtins.range_error_prototype),
                "RangeError",
            )
        }
        RuntimeError::InternalError(msg) => {
            create_error_object(
                Some(msg.clone()),
                Rc::clone(&builtins.error_prototype),
                "Error",
            )
        }
        RuntimeError::NotImplemented(msg) => {
            create_error_object(
                Some(format!("Not implemented: {}", msg)),
                Rc::clone(&builtins.error_prototype),
                "Error",
            )
        }
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
            let name = borrowed.property_get(&PropertyKey::from_str("name"))
                .map(|d| d.value.to_js_string())
                .unwrap_or_else(|| "Error".to_string());

            // Get message property, default to empty string
            let message = borrowed.property_get(&PropertyKey::from_str("message"))
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

/// Setup Error.prototype with standard methods
pub fn setup_error_prototype(
    proto: &Rc<RefCell<dyn JSObject>>,
    _builtins: &crate::builtins::Builtins,
) {
    use crate::builtins::set_prototype_method;

    // Error.prototype.toString
    set_prototype_method(proto, "toString", |this, _args| {
        error_prototype_to_string(this)
    });
}
