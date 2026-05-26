//! Symbol builtin implementation

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::vm::object::{JSObject, NativeFunctionObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::{SymbolData, Value};
use crate::RuntimeError;

// Global counter for unique Symbol IDs
static SYMBOL_COUNTER: AtomicU64 = AtomicU64::new(1);

// Global Symbol registry: key -> Symbol
// TODO: Move this into VM state for better encapsulation
thread_local! {
    static SYMBOL_REGISTRY: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

/// Get reference to the global symbol registry
fn with_registry<F, R>(f: F) -> R
where
    F: FnOnce(&RefCell<HashMap<String, Value>>) -> R,
{
    SYMBOL_REGISTRY.with(f)
}

/// Symbol constructor: Symbol(description?)
pub fn symbol_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    // Get optional description argument
    let description = if args.is_empty() {
        None
    } else {
        match &args[0] {
            Value::Undefined => None,
            Value::Null => Some("null".to_string()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(format_number(*n)),
            Value::String(s) => Some(s.to_string()),
            Value::Symbol(_) => return Err(RuntimeError::TypeError(
                "Cannot convert a Symbol value to a string".to_string()
            )),
            _ => Some("[object Object]".to_string()),
        }
    };

    // Generate unique ID
    let id = SYMBOL_COUNTER.fetch_add(1, Ordering::SeqCst);

    // Create Symbol value
    let symbol_data = SymbolData::new(description, id);
    Ok(Value::Symbol(Rc::new(symbol_data)))
}

/// Symbol.for(key) - returns a Symbol from the global registry
pub fn symbol_for(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Symbol.for requires a key argument".to_string()
        ));
    }

    // Convert argument to string key
    let key = match &args[0] {
        Value::String(s) => s.to_string(),
        Value::Undefined => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => format_number(*n),
        Value::Symbol(_) => return Err(RuntimeError::TypeError(
            "Cannot use Symbol as key for Symbol.for".to_string()
        )),
        _ => args[0].to_string(),
    };

    // Check global registry first
    with_registry(|registry| {
        {
            let borrowed = registry.borrow();
            if let Some(existing) = borrowed.get(&key) {
                return Ok(existing.clone());
            }
        } // Release borrow
        
        // Create new symbol and register it
        let id = SYMBOL_COUNTER.fetch_add(1, Ordering::SeqCst);
        let symbol_data = SymbolData::new(Some(key.clone()), id);
        let symbol = Value::Symbol(Rc::new(symbol_data));
        
        registry.borrow_mut().insert(key, symbol.clone());
        
        Ok(symbol)
    })
}

/// Symbol.keyFor(sym) - returns the key from the global registry
pub fn symbol_key_for(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Symbol.keyFor requires a symbol argument".to_string()
        ));
    }

    match &args[0] {
        Value::Symbol(sym) => {
            with_registry(|registry| {
                let borrowed = registry.borrow();
                
                // Find the key for this symbol
                for (key, value) in borrowed.iter() {
                    if let Value::Symbol(existing_sym) = value {
                        if existing_sym.id == sym.id {
                            return Ok(Value::String(Rc::new(key.clone())));
                        }
                    }
                }
                
                // Symbol not found in registry (not created by Symbol.for)
                Ok(Value::Undefined)
            })
        }
        _ => Err(RuntimeError::TypeError(
            "Symbol.keyFor requires a symbol argument".to_string()
        )),
    }
}

/// Register Symbol prototype methods
pub fn register_symbol_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    // Symbol.prototype.toString()
    let to_string_desc = PropertyDescriptor {
        value: Value::String(Rc::new("function toString() { [native code] }".to_string())),
        writable: true,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    };
    proto.borrow_mut().property_set(
        PropertyKey::from_str("toString"),
        to_string_desc.value.clone(),
    );

    // Symbol.prototype.valueOf()
    let value_of_desc = PropertyDescriptor {
        value: Value::String(Rc::new("function valueOf() { [native code] }".to_string())),
        writable: true,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    };
    proto.borrow_mut().property_set(
        PropertyKey::from_str("valueOf"),
        value_of_desc.value.clone(),
    );

    // Symbol.prototype.description (getter)
    let description_desc = PropertyDescriptor {
        value: Value::Undefined,
        writable: false,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    };
    proto.borrow_mut().property_set(
        PropertyKey::from_str("description"),
        description_desc.value.clone(),
    );
}

/// Register Symbol static methods
pub fn register_symbol_statics(symbol_fn: &Value, symbol_proto: &Rc<RefCell<dyn JSObject>>) {
    if let Value::Object(obj) = symbol_fn {
        // Symbol.for
        let for_fn = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Symbol.for")
        )));
        obj.borrow_mut().property_set(
            PropertyKey::from_str("for"),
            for_fn,
        );

        // Symbol.keyFor
        let key_for_fn = Value::Object(Rc::new(RefCell::new(
            NativeFunctionObject::new("Symbol.keyFor")
        )));
        obj.borrow_mut().property_set(
            PropertyKey::from_str("keyFor"),
            key_for_fn,
        );

        // Set Symbol.prototype
        obj.borrow_mut().property_set(
            PropertyKey::from_str("prototype"),
            Value::Object(Rc::clone(symbol_proto)),
        );
    }
}

/// Format a number for Symbol description
fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n.is_sign_positive() {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else if n == n.trunc() {
        // Integer
        format!("{:.0}", n)
    } else {
        format!("{}", n)
    }
}

/// Symbol.prototype.toString() implementation
pub fn symbol_to_string(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Symbol(sym) => {
            let desc = match &sym.description {
                Some(d) => format!("Symbol({})", d),
                None => "Symbol()".to_string(),
            };
            Ok(Value::string(&desc))
        }
        _ => Err(RuntimeError::TypeError(
            "Symbol.prototype.toString requires a Symbol".to_string()
        )),
    }
}

/// Symbol.prototype.valueOf() implementation
pub fn symbol_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Symbol(_) => Ok(obj.clone()),
        _ => Err(RuntimeError::TypeError(
            "Symbol.prototype.valueOf requires a Symbol".to_string()
        )),
    }
}

/// Symbol.prototype.description getter
pub fn symbol_description(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Symbol(sym) => {
            match &sym.description {
                Some(desc) => Ok(Value::String(Rc::new(desc.clone()))),
                None => Ok(Value::Undefined),
            }
        }
        _ => Err(RuntimeError::TypeError(
            "Symbol.prototype.description requires a Symbol".to_string()
        )),
    }
}
