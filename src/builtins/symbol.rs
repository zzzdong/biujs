//! Symbol builtin implementation

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::RuntimeError;
use crate::vm::object::{JSObject, NativeFunctionObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::{SymbolData, Value};

// Global counter for unique Symbol IDs
static SYMBOL_COUNTER: AtomicU64 = AtomicU64::new(1);

// Global Symbol registry: key -> Symbol
// TODO: Move this into VM state for better encapsulation
thread_local! {
    static SYMBOL_REGISTRY: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

// ─────────────────────────────────────────────────────────
// Well-known symbols (ES 19.4.2.1)
// ─────────────────────────────────────────────────────────

/// Ids of the well-known symbols. They live just below `u64::MAX` so they can
/// never collide with the runtime counter (`SYMBOL_COUNTER` starts at 1), and
/// every subsystem can name the symbol it needs instead of repeating a literal.
///
/// `Symbol.iterator` is defined in `vm::iterator` (the iteration protocol was
/// the first consumer); the rest live here, next to the Symbol builtin.
pub const HAS_INSTANCE_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0002;
pub const IS_CONCAT_SPREADABLE_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0003;
pub const TO_PRIMITIVE_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0004;
pub const TO_STRING_TAG_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0005;
pub const SPECIES_SYMBOL_ID: u64 = 0xFFFF_FFFF_FFFF_0006;

/// `Symbol.toPrimitive` as a property key.
pub fn to_primitive_symbol_key() -> PropertyKey {
    PropertyKey::Symbol(TO_PRIMITIVE_SYMBOL_ID)
}

/// `Symbol.toStringTag` as a property key.
pub fn to_string_tag_symbol_key() -> PropertyKey {
    PropertyKey::Symbol(TO_STRING_TAG_SYMBOL_ID)
}

/// `Symbol.hasInstance` as a property key.
pub fn has_instance_symbol_key() -> PropertyKey {
    PropertyKey::Symbol(HAS_INSTANCE_SYMBOL_ID)
}

// ─────────────────────────────────────────────────────────
// Symbol value registry
// ─────────────────────────────────────────────────────────

thread_local! {
    /// Every symbol value ever created, keyed by id.
    ///
    /// A `PropertyKey::Symbol` only carries the id, so recovering the *value*
    /// (with its description) for `Object.getOwnPropertySymbols` and symbol
    /// property iteration needs this lookup.
    static SYMBOL_VALUES: RefCell<HashMap<u64, Value>> = RefCell::new(HashMap::new());
}

/// Remember a freshly created symbol so [`symbol_value_by_id`] can find it.
pub fn register_symbol_value(value: &Value) {
    if let Value::Symbol(sym) = value {
        SYMBOL_VALUES.with(|values| {
            values.borrow_mut().insert(sym.id, value.clone());
        });
    }
}

/// The symbol value with `id`, falling back to a description-less symbol when
/// the value predates the registry.
pub fn symbol_value_by_id(id: u64) -> Value {
    SYMBOL_VALUES
        .with(|values| values.borrow().get(&id).cloned())
        .unwrap_or_else(|| Value::Symbol(Rc::new(SymbolData::new(None, id))))
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
            Value::Symbol(_) => {
                return Err(RuntimeError::TypeError(
                    "Cannot convert a Symbol value to a string".to_string(),
                ));
            }
            _ => Some("[object Object]".to_string()),
        }
    };

    // Generate unique ID
    let id = SYMBOL_COUNTER.fetch_add(1, Ordering::SeqCst);

    // Create Symbol value
    let symbol_data = SymbolData::new(description, id);
    let symbol = Value::Symbol(Rc::new(symbol_data));
    register_symbol_value(&symbol);
    Ok(symbol)
}
/// Symbol.for(key) - returns a Symbol from the global registry
pub fn symbol_for(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Symbol.for requires a key argument".to_string(),
        ));
    }

    // Convert argument to string key
    let key = match &args[0] {
        Value::String(s) => s.to_string(),
        Value::Undefined => "undefined".to_string(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => format_number(*n),
        Value::Symbol(_) => {
            return Err(RuntimeError::TypeError(
                "Cannot use Symbol as key for Symbol.for".to_string(),
            ));
        }
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
        register_symbol_value(&symbol);

        registry.borrow_mut().insert(key, symbol.clone());

        Ok(symbol)
    })
}

/// Symbol.keyFor(sym) - returns the key from the global registry
pub fn symbol_key_for(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Symbol.keyFor requires a symbol argument".to_string(),
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
            "Symbol.keyFor requires a symbol argument".to_string(),
        )),
    }
}

/// Native name of the `Symbol.prototype.description` getter, dispatched by the
/// VM in `call_native_by_name`.
pub const SYMBOL_DESCRIPTION_NATIVE: &str = "__symbol_description__";

/// Register Symbol prototype methods.
///
/// `toString` / `valueOf` are registered as *callable* natives (not as
/// descriptive strings) so that `Symbol.prototype.toString.call(sym)` works;
/// the VM's `call_prototype_method` performs the actual conversion.
pub fn register_symbol_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use crate::builtins::mark_prototype_method;

    mark_prototype_method(proto, "toString");
    mark_prototype_method(proto, "valueOf");

    // `Symbol.prototype.description` is an accessor: the getter unwraps the
    // symbol and returns its description (or undefined).
    let getter = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
        SYMBOL_DESCRIPTION_NATIVE,
    ))));
    let description_desc = PropertyDescriptor {
        value: Value::Undefined,
        writable: false,
        enumerable: false,
        configurable: true,
        getter: Some(getter),
        setter: None,
    };
    let _ = proto
        .borrow_mut()
        .define_property(PropertyKey::from_str("description"), description_desc);

    // `Symbol.prototype[Symbol.toStringTag] === "Symbol"`.
    let tag_desc = PropertyDescriptor {
        value: Value::string("Symbol"),
        writable: false,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
    };
    let _ = proto
        .borrow_mut()
        .define_property(to_string_tag_symbol_key(), tag_desc);
}

/// Register Symbol static methods
pub fn register_symbol_statics(symbol_fn: &Value, symbol_proto: &Rc<RefCell<dyn JSObject>>) {
    if let Value::Object(obj) = symbol_fn {
        // Symbol.for
        let for_fn = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            "Symbol.for",
        ))));
        obj.borrow_mut()
            .property_set(PropertyKey::from_str("for"), for_fn);

        // Symbol.keyFor
        let key_for_fn = Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
            "Symbol.keyFor",
        ))));
        obj.borrow_mut()
            .property_set(PropertyKey::from_str("keyFor"), key_for_fn);

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
            "Symbol.prototype.toString requires a Symbol".to_string(),
        )),
    }
}

/// Symbol.prototype.valueOf() implementation
pub fn symbol_value_of(obj: &Value) -> Result<Value, RuntimeError> {
    match obj {
        Value::Symbol(_) => Ok(obj.clone()),
        _ => Err(RuntimeError::TypeError(
            "Symbol.prototype.valueOf requires a Symbol".to_string(),
        )),
    }
}

/// Symbol.prototype.description getter
pub fn symbol_description(obj: &Value) -> Result<Value, RuntimeError> {
    // A Symbol wrapper (`Object(sym)`) unwraps to its [[SymbolData]] first.
    if let Value::Object(obj_ref) = obj {
        let kind = obj_ref.borrow().kind();
        if kind == crate::vm::property::ObjectKind::Symbol {
            let prim = obj.to_primitive("default");
            return symbol_description(&prim);
        }
    }
    match obj {
        Value::Symbol(sym) => match &sym.description {
            Some(desc) => Ok(Value::String(Rc::new(desc.clone()))),
            None => Ok(Value::Undefined),
        },
        _ => Err(RuntimeError::TypeError(
            "Symbol.prototype.description requires a Symbol".to_string(),
        )),
    }
}
