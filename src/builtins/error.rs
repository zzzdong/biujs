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

/// The `cause` descriptor from an options bag, if the bag has an own `cause`.
///
/// The builtin layer cannot run accessors, so only a data property is honoured.
fn options_cause(options: Option<&Value>) -> Option<crate::vm::property::PropertyDescriptor> {
    let Value::Object(obj_ref) = options? else {
        return None;
    };
    obj_ref
        .borrow()
        .property_get(&PropertyKey::from_str("cause"))
        .map(|d| error_message_descriptor(d.value))
}

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
    // Every error instance reports `[[Class]]` "Error", whatever the concrete
    // constructor: `Object.prototype.toString.call(new TypeError())` is
    // "[object Error]" (ES 19.1.3.6).
    let mut obj = OrdinaryObject::with_class_name("Error");
    obj.set_prototype(Some(Rc::clone(&prototype)));
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
    let mut obj = OrdinaryObject::with_class_name("Error");
    if let Some(proto) = crate::builtins::wrapper_prototype(error_type.name()) {
        obj.set_prototype(Some(proto));
    }
    if let Some(msg) = message {
        obj.define_property(
            PropertyKey::from_str("message"),
            error_message_descriptor(Value::string(&msg)),
        )
        .ok();
    }
    // `new Error(message, { cause })` installs an own, non-enumerable `cause`
    // (ES 20.5.1.1 step 3).
    if let Some(cause) = options_cause(args.get(1)) {
        obj.define_property(PropertyKey::from_str("cause"), cause).ok();
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
        // `SyntaxError` must be a *SyntaxError*: JSON.parse and the compiler's
        // diagnostics are matched by `e instanceof SyntaxError` / `e.name`.
        RuntimeError::SyntaxError(msg) => create_error_object(
            Some(msg.clone()),
            Rc::clone(&builtins.syntax_error_prototype),
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

/// `Error.prototype.toString()` (ES 20.5.3.4).
///
/// `name` and `message` are read with `[[Get]]`, i.e. through the prototype
/// chain, and an `undefined` value falls back to `"Error"` / `""`.
pub fn error_prototype_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    let Value::Object(obj_ref) = obj else {
        // "If the this value is not an object, throw a TypeError."
        return Err(RuntimeError::TypeError(
            "Error.prototype.toString requires that 'this' be an Object".to_string(),
        ));
    };

    let get = |prop: &str| -> Option<Value> {
        crate::vm::prototype::find_descriptor(Rc::clone(obj_ref), &PropertyKey::from_str(prop))
            .ok()
            .flatten()
            .map(|(_, desc)| desc.value)
    };

    let name = match get("name") {
        None | Some(Value::Undefined) => "Error".to_string(),
        Some(v) => v.to_js_string(),
    };
    let message = match get("message") {
        None | Some(Value::Undefined) => String::new(),
        Some(v) => v.to_js_string(),
    };

    let result = if name.is_empty() {
        message
    } else if message.is_empty() {
        name
    } else {
        format!("{name}: {message}")
    };
    Ok(Value::string(&result))
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

/// Install the own `name` / `message` properties of a native error prototype.
///
/// `TypeError.prototype.name` is `"TypeError"` and its `message` starts as the
/// empty string (ES 19.5.6.3.2); every other property is inherited from
/// `Error.prototype`.
pub fn setup_native_error_prototype(proto: &Rc<RefCell<dyn JSObject>>, name: &str) {
    use crate::vm::property::PropertyDescriptor;

    let writable = |value: Value| PropertyDescriptor {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    };
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str("name"), writable(Value::string(name)));
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str("message"), writable(Value::string("")));
}
