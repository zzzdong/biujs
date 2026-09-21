use crate::RuntimeError;
use crate::vm::property::PropertyDescriptor;
use crate::vm::object::{JSObject, new_array_object_from_vec};
use crate::vm::property::PropertyKey;
use crate::vm::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

use super::set_static_method;

pub fn object_constructor(args: &[Value]) -> Result<Value, RuntimeError> {
    match args.first() {
        // `Object()` / `Object(null)` / `Object(undefined)`: an empty ordinary
        // object. The prototype comes from the wrapper registry (a plain
        // `Object()` call reaches this builtin without the VM's `New` prologue,
        // which would otherwise leave the object prototype-less).
        None => {
            let proto = super::wrapper_prototype("Object");
            let mut obj = crate::vm::object::OrdinaryObject::new();
            obj.set_prototype(proto);
            Ok(Value::Object(Rc::new(RefCell::new(obj))))
        }
        Some(v) if matches!(v, Value::Undefined | Value::Null) => {
            let proto = super::wrapper_prototype("Object");
            let mut obj = crate::vm::object::OrdinaryObject::new();
            obj.set_prototype(proto);
            Ok(Value::Object(Rc::new(RefCell::new(obj))))
        }
        // `Object(value)`: ToObject — primitives are boxed in wrapper objects.
        Some(v) => super::to_object(v),
    }
}

/// Own enumerable string keys of a value, applying `ToObject` first.
///
/// Shared by `Object.keys` / `values` / `entries`: a string primitive exposes
/// its index keys, other primitives have none, and null/undefined throw.
fn to_object_string_keys(value: &Value) -> Result<Vec<PropertyKey>, RuntimeError> {
    match value {
        Value::Object(obj_ref) => Ok(obj_ref
            .borrow()
            .own_keys()
            .into_iter()
            .filter(|k| k.as_str().is_some())
            .filter(|k| {
                obj_ref
                    .borrow()
                    .property_get(k)
                    .is_some_and(|d| d.enumerable)
            })
            .collect()),
        Value::String(s) => Ok((0..s.chars().count())
            .map(|i| PropertyKey::from_str(&i.to_string()))
            .collect()),
        Value::Number(_) | Value::Bool(_) | Value::Symbol(_) => Ok(Vec::new()),
        Value::Undefined | Value::Null => Err(RuntimeError::TypeError(
            "Cannot convert undefined or null to object".to_string(),
        )),
        Value::Function(_) => Ok(Vec::new()),
    }
}

/// Read `key` from a value for `Object.values` / `Object.entries`, honouring
/// string index access on primitives.
fn to_object_get(value: &Value, key: &PropertyKey) -> Value {
    match value {
        Value::Object(obj_ref) => obj_ref
            .borrow()
            .property_get(key)
            .map(|d| d.value)
            .unwrap_or(Value::Undefined),
        Value::String(s) => key
            .as_str()
            .and_then(|k| k.parse::<usize>().ok())
            .and_then(|i| s.chars().nth(i))
            .map(|c| Value::string(&c.to_string()))
            .unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    }
}

pub fn object_keys(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.keys requires at least 1 argument".to_string(),
        ));
    }
    let keys = to_object_string_keys(&args[0])?;
    let str_keys: Vec<Value> = keys
        .iter()
        .filter_map(|k| k.as_str().map(Value::string))
        .collect();
    Ok(new_array_object_from_vec(str_keys))
}

pub fn object_values(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.values requires at least 1 argument".to_string(),
        ));
    }
    let keys = to_object_string_keys(&args[0])?;
    let values: Vec<Value> = keys.iter().map(|k| to_object_get(&args[0], k)).collect();
    Ok(new_array_object_from_vec(values))
}

pub fn object_entries(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.entries requires at least 1 argument".to_string(),
        ));
    }
    let keys = to_object_string_keys(&args[0])?;
    let entries: Vec<Value> = keys
        .iter()
        .map(|k| {
            new_array_object_from_vec(vec![
                Value::string(&k.display()),
                to_object_get(&args[0], k),
            ])
        })
        .collect();
    Ok(new_array_object_from_vec(entries))
}

pub fn object_define_property(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 3 {
        return Err(RuntimeError::TypeError(
            "Object.defineProperty requires at least 3 arguments".to_string(),
        ));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.defineProperty: first argument must be an object".to_string(),
            ));
        }
    };
    // `ToPropertyKey`: strings pass through, symbols stay symbols, anything
    // else is stringified. Computed class members routinely use symbols.
    let key = match &args[1] {
        Value::String(s) => PropertyKey::from_str(s),
        Value::Symbol(sym) => PropertyKey::Symbol(sym.id),
        other => PropertyKey::from_str(&other.to_js_string()),
    };
    let desc = &args[2];

    // Honour the full descriptor (value/writable/enumerable/configurable and
    // get/set) instead of only copying the value. An *empty* descriptor is
    // valid: it creates the property with every attribute `false`.
    if let Value::Object(_) = desc {
        let desc_val = desc.clone();
        // Every object kind keeps its own property table, so the descriptor
        // survives on arrays, functions and prototype objects too — a class
        // constructor is a `FunctionObject`, and `static get x()` is defined
        // through exactly this path.
        let mut obj_mut = obj.borrow_mut();
        apply_property_descriptor(&mut *obj_mut, &key, &desc_val)?;
    } else {
        return Err(RuntimeError::TypeError(
            "Property description must be an object".to_string(),
        ));
    }

    Ok(Value::Object(Rc::clone(obj)))
}

pub fn object_get_own_property_descriptor(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.getOwnPropertyDescriptor requires at least 2 arguments".to_string(),
        ));
    }
    // ES 19.1.2.6: ToObject(O) — string primitives expose their index keys
    // and `length`; only undefined/null raise TypeError.
    let obj_val = super::to_object(&args[0])?;
    let Value::Object(obj) = &obj_val else {
        return Err(RuntimeError::TypeError(
            "Object.getOwnPropertyDescriptor: first argument must be an object".to_string(),
        ));
    };
    let key = match &args[1] {
        Value::String(s) => PropertyKey::from_str(s),
        Value::Symbol(sym) => PropertyKey::Symbol(sym.id),
        other => PropertyKey::from_str(&other.to_js_string()),
    };

    let borrowed = obj.borrow();
    if let Some(desc) = borrowed.property_get(&key) {
        // Create a descriptor object
        let mut desc_obj = crate::vm::object::OrdinaryObject::new();
        if desc.is_accessor_descriptor() {
            // Accessor descriptor: report get/set, never value/writable.
            let undefined = Value::Undefined;
            desc_obj
                .property_set(
                    PropertyKey::from_str("get"),
                    desc.getter.clone().unwrap_or(undefined.clone()),
                )
                .map_err(|e| RuntimeError::TypeError(e))?;
            desc_obj
                .property_set(
                    PropertyKey::from_str("set"),
                    desc.setter.clone().unwrap_or(undefined),
                )
                .map_err(|e| RuntimeError::TypeError(e))?;
        } else {
            desc_obj
                .property_set(PropertyKey::from_str("value"), desc.value.clone())
                .map_err(|e| RuntimeError::TypeError(e))?;
            desc_obj
                .property_set(
                    PropertyKey::from_str("writable"),
                    Value::Bool(desc.writable),
                )
                .map_err(|e| RuntimeError::TypeError(e))?;
        }
        desc_obj
            .property_set(
                PropertyKey::from_str("enumerable"),
                Value::Bool(desc.enumerable),
            )
            .map_err(|e| RuntimeError::TypeError(e))?;
        desc_obj
            .property_set(
                PropertyKey::from_str("configurable"),
                Value::Bool(desc.configurable),
            )
            .map_err(|e| RuntimeError::TypeError(e))?;
        Ok(Value::Object(Rc::new(RefCell::new(desc_obj))))
    } else {
        Ok(Value::Undefined)
    }
}

pub fn object_get_own_property_names(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.getOwnPropertyNames requires at least 1 argument".to_string(),
        ));
    }
    // ES 19.1.2.7: ToObject(O) — string primitives report index keys and
    // `length`; undefined/null raise TypeError.
    let obj_val = super::to_object(&args[0])?;
    let Value::Object(obj_ref) = &obj_val else {
        return Ok(new_array_object_from_vec(vec![]));
    };
    let borrowed = obj_ref.borrow();
    // String-keyed own properties only (symbols are reported by
    // `Object.getOwnPropertySymbols`).
    let str_keys: Vec<Value> = borrowed
        .own_keys()
        .into_iter()
        .filter_map(|k| k.as_str().map(|s| Value::string(s)))
        .collect();
    Ok(new_array_object_from_vec(str_keys))
}

/// `Object.getOwnPropertySymbols(obj)` — own symbol-keyed properties, in
/// creation order.
pub fn object_get_own_property_symbols(args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(Value::Object(obj_ref)) = args.first() else {
        return Err(RuntimeError::TypeError(
            "Object.getOwnPropertySymbols called on non-object".to_string(),
        ));
    };
    let symbols: Vec<Value> = obj_ref
        .borrow()
        .own_keys()
        .into_iter()
        .filter_map(|k| match k {
            PropertyKey::Symbol(id) => Some(crate::builtins::symbol_value_by_id(id)),
            _ => None,
        })
        .collect();
    Ok(new_array_object_from_vec(symbols))
}

pub fn object_get_prototype_of(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.getPrototypeOf requires at least 1 argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let borrowed = obj_ref.borrow();
            if let Some(proto) = borrowed.get_prototype() {
                Ok(Value::Object(Rc::clone(&proto)))
            } else {
                Ok(Value::Null)
            }
        }
        _ => Ok(Value::Null),
    }
}

pub fn object_set_prototype_of(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.setPrototypeOf requires at least 2 arguments".to_string(),
        ));
    }
    let obj = match &args[0] {
        Value::Object(obj_ref) => obj_ref,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.setPrototypeOf: first argument must be an object".to_string(),
            ));
        }
    };
    let proto = match &args[1] {
        Value::Object(proto_ref) => Some(Rc::clone(proto_ref)),
        Value::Null => None,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.setPrototypeOf: prototype must be an object or null".to_string(),
            ));
        }
    };
    obj.borrow_mut().set_prototype(proto);
    Ok(Value::Object(Rc::clone(obj)))
}

pub fn object_create(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::TypeError(
            "Object.create requires at least 1 argument".to_string(),
        ));
    }
    let proto = match &args[0] {
        Value::Object(proto_ref) => Some(Rc::clone(proto_ref)),
        Value::Null => None,
        _ => {
            return Err(RuntimeError::TypeError(
                "Object.create: prototype must be an object or null".to_string(),
            ));
        }
    };
    let mut new_obj = crate::vm::object::OrdinaryObject::new();
    new_obj.set_prototype(proto);

    // Second argument: property descriptors, each applied in order.
    // `undefined` means "none"; anything else must be an object (ES 19.1.2.2).
    if let Some(props) = args.get(1) {
        if !props.is_undefined() {
            let props_ref = match props {
                Value::Object(o) => Rc::clone(o),
                _ => {
                    return Err(RuntimeError::TypeError(
                        "Object.create: properties must be an object".to_string(),
                    ));
                }
            };
            let keys: Vec<PropertyKey> = props_ref
                .borrow()
                .own_keys()
                .into_iter()
                .filter(|k| {
                    props_ref
                        .borrow()
                        .property_get(k)
                        .is_some_and(|d| d.enumerable)
                })
                .collect();
            for key in keys {
                let desc = props_ref
                    .borrow()
                    .property_get(&key)
                    .map(|d| d.value)
                    .unwrap_or(Value::Undefined);
                apply_property_descriptor(&mut new_obj, &key, &desc)?;
            }
        }
    }

    Ok(Value::Object(Rc::new(RefCell::new(new_obj))))
}

/// Apply a property descriptor object (as consumed by `Object.defineProperty`
/// and the `properties` argument of `Object.create`) to any object kind.
///
/// Only the attributes the descriptor actually mentions are changed; the
/// others keep the value the existing property already has, and default to
/// `false` for a brand-new property — the ES `[[DefineOwnProperty]]` rule for
/// a partial descriptor.
pub fn apply_property_descriptor(
    obj: &mut dyn JSObject,
    key: &PropertyKey,
    desc: &Value,
) -> Result<bool, RuntimeError> {
    let desc_ref = match desc {
        Value::Object(o) => o,
        _ => {
            return Err(RuntimeError::TypeError(
                "Property description must be an object".to_string(),
            ));
        }
    };
    let read = |name: &str| -> Option<Value> {
        desc_ref
            .borrow()
            .property_get(&PropertyKey::from_str(name))
            .map(|d| d.value)
    };

    // Attributes the descriptor does not mention keep the value they already
    // have; for a brand-new property they default to `false` (ES `[[DefineOwnProperty]]`
    // with a partial descriptor).
    let existing = obj.property_get(key);
    let current = |pick: fn(&PropertyDescriptor) -> bool| -> bool {
        existing.as_ref().is_some_and(pick)
    };

    let getter = read("get");
    let setter = read("set");
    if getter.is_some() || setter.is_some() {
        // Accessor descriptor. Omitting `get`/`set` preserves the half that is
        // already installed, so `defineProperty(o, k, {set})` keeps an existing
        // getter instead of dropping it.
        let existing_is_accessor = existing
            .as_ref()
            .is_some_and(|d| d.getter.is_some() || d.setter.is_some());
        let mut current_get = if existing_is_accessor {
            existing.as_ref().and_then(|d| d.getter.clone())
        } else {
            None
        };
        let mut current_set = if existing_is_accessor {
            existing.as_ref().and_then(|d| d.setter.clone())
        } else {
            None
        };
        if let Some(v) = getter {
            current_get = Some(v);
        }
        if let Some(v) = setter {
            current_set = Some(v);
        }

        let mut descriptor = PropertyDescriptor::accessor_descriptor(current_get, current_set);
        descriptor.enumerable = read("enumerable")
            .map(|v| v.to_boolean())
            .unwrap_or_else(|| current(|d| d.enumerable));
        descriptor.configurable = read("configurable")
            .map(|v| v.to_boolean())
            .unwrap_or_else(|| current(|d| d.configurable));
        return obj
            .define_property(key.clone(), descriptor)
            .map_err(RuntimeError::TypeError);
    }

    let value = match read("value") {
        Some(v) => v,
        None => existing.as_ref().map(|d| d.value.clone()).unwrap_or(Value::Undefined),
    };
    let mut descriptor = PropertyDescriptor::data_descriptor(value);
    descriptor.writable = read("writable")
        .map(|v| v.to_boolean())
        .unwrap_or_else(|| current(|d| d.writable));
    descriptor.enumerable = read("enumerable")
        .map(|v| v.to_boolean())
        .unwrap_or_else(|| current(|d| d.enumerable));
    descriptor.configurable = read("configurable")
        .map(|v| v.to_boolean())
        .unwrap_or_else(|| current(|d| d.configurable));
    obj.define_property(key.clone(), descriptor)
        .map_err(RuntimeError::TypeError)
}

pub fn object_has_own(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.hasOwn requires at least 2 arguments".to_string(),
        ));
    }
    match &args[0] {
        Value::Object(obj_ref) => {
            let key = match &args[1] {
                Value::String(s) => PropertyKey::from_str(s),
                _ => PropertyKey::from_str(&args[1].to_js_string()),
            };
            let borrowed = obj_ref.borrow();
            // Check if property exists directly on this object (not in prototype chain)
            // by checking if the property is in own_keys
            let own_keys = borrowed.own_keys();
            Ok(Value::Bool(own_keys.iter().any(|k| k == &key)))
        }
        _ => Ok(Value::Bool(false)),
    }
}

pub fn object_is(args: &[Value]) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::TypeError(
            "Object.is requires at least 2 arguments".to_string(),
        ));
    }
    // SameValue algorithm: NaN === NaN, +0 !== -0
    let result = match (&args[0], &args[1]) {
        (Value::Number(a), Value::Number(b)) => {
            if a.is_nan() && b.is_nan() {
                true
            } else if *a == 0.0 && *b == 0.0 {
                // Check signs: +0 !== -0
                a.is_sign_positive() == b.is_sign_positive()
            } else {
                a == b
            }
        }
        (a, b) => a == b,
    };
    Ok(Value::Bool(result))
}

/// `Object.assign(target, ...sources)` — copies own enumerable properties
/// (string and symbol keys). Values of *own accessor* properties cannot be
/// invoked at this layer, so the VM wraps the whole builtin (see
/// `VM::object_assign`) when getters need to run; this function is the
/// data-property fallback used for direct dispatch.
pub fn object_assign(args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(target) = args.first() else {
        return Err(RuntimeError::TypeError(
            "Object.assign requires at least 1 argument".to_string(),
        ));
    };
    // ES 19.1.2.1: ToObject(target) — primitive targets are boxed.
    let target = super::to_object(target)?;
    for source in args.iter().skip(1) {
        if matches!(source, Value::Undefined | Value::Null) {
            continue;
        }
        let source = super::to_object(source)?;
        let Value::Object(source_ref) = &source else {
            continue;
        };
        let entries: Vec<(PropertyKey, Value)> = {
            let borrowed = source_ref.borrow();
            borrowed
                .own_keys()
                .into_iter()
                .filter_map(|k| {
                    borrowed
                        .property_get(&k)
                        .filter(|d| d.enumerable && d.is_data_descriptor())
                        .map(|d| (k, d.value))
                })
                .collect()
        };
        if let Value::Object(target_ref) = &target {
            for (key, value) in entries {
                let _ = target_ref.borrow_mut().property_set(key, value);
            }
        }
    }
    Ok(target)
}

/// `Object.defineProperties(obj, props)`
pub fn object_define_properties(args: &[Value]) -> Result<Value, RuntimeError> {
    let (Some(target), Some(props)) = (args.first(), args.get(1)) else {
        return Err(RuntimeError::TypeError(
            "Object.defineProperties requires 2 arguments".to_string(),
        ));
    };
    let Value::Object(target_ref) = target else {
        return Err(RuntimeError::TypeError(
            "Object.defineProperties: target is not an object".to_string(),
        ));
    };
    // `Properties` must be an object (ES 19.1.2.3 step 2); null/undefined and
    // primitives are a TypeError.
    let Value::Object(props_ref) = props else {
        return Err(RuntimeError::TypeError(
            "Object.defineProperties: properties must be an object".to_string(),
        ));
    };
    // Every descriptor is converted *before* any property is defined, so a bad
    // descriptor leaves the target untouched. Only own *enumerable* keys count.
    let descriptors: Vec<(PropertyKey, Value)> = {
        let borrowed = props_ref.borrow();
        borrowed
            .own_keys()
            .into_iter()
            .filter(|k| borrowed.property_get(k).is_some_and(|d| d.enumerable))
            .filter_map(|k| borrowed.property_get(&k).map(|d| (k, d.value)))
            .collect()
    };
    for (key, descriptor) in descriptors {
        let key_value = match &key {
            PropertyKey::Str(s) => Value::string(s),
            PropertyKey::Symbol(id) => crate::builtins::symbol_value_by_id(*id),
        };
        object_define_property(&[target.clone(), key_value, descriptor])?;
    }
    Ok(target.clone())
}

/// `Object.prototype.toString.call(value)` — the spec's [[Class]] based form.
pub fn object_prototype_to_string(obj: &Value, _args: &[Value]) -> Result<Value, RuntimeError> {
    let class = match obj {
        Value::Object(o) => o.borrow().class_name().to_string(),
        Value::Undefined => "Undefined".to_string(),
        Value::Null => "Null".to_string(),
        Value::Bool(_) => "Boolean".to_string(),
        Value::Number(_) => "Number".to_string(),
        Value::String(_) => "String".to_string(),
        Value::Symbol(_) => "Symbol".to_string(),
        Value::Function(_) => "Function".to_string(),
    };
    Ok(Value::string(&format!("[object {class}]")))
}

/// `Object.prototype.hasOwnProperty(V)`
pub fn object_has_own_property(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let key = match args.first() {
        Some(Value::String(s)) => PropertyKey::from_str(s),
        Some(Value::Symbol(sym)) => PropertyKey::Symbol(sym.id),
        Some(other) => PropertyKey::from_str(&other.to_js_string()),
        None => return Ok(Value::Bool(false)),
    };
    match obj {
        Value::Object(obj_ref) => Ok(Value::Bool(obj_ref.borrow().has_property(&key))),
        Value::String(s) => {
            // String wrappers expose an own `length` and their index keys.
            let own = match key.as_str() {
                Some("length") => true,
                Some(k) => k
                    .parse::<usize>()
                    .ok()
                    .is_some_and(|i| i < s.chars().count()),
                None => false,
            };
            Ok(Value::Bool(own))
        }
        _ => Ok(Value::Bool(false)),
    }
}

/// `Object.prototype.isPrototypeOf(V)` — is `this` anywhere on `V`'s
/// prototype chain?
pub fn object_is_prototype_of(obj: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let Value::Object(target) = obj else {
        // ES: ToObject(this) then walk V's chain; a primitive receiver has no
        // object identity, so the answer is false for object arguments and a
        // TypeError for primitives (handled by the caller's ToObject).
        return Ok(Value::Bool(false));
    };
    let Some(Value::Object(candidate)) = args.first() else {
        return Ok(Value::Bool(false));
    };
    let mut current = candidate.borrow().get_prototype();
    let mut depth = 0;
    while let Some(proto) = current {
        if depth > 1000 {
            break;
        }
        if Rc::ptr_eq(&proto, target) {
            return Ok(Value::Bool(true));
        }
        current = proto.borrow().get_prototype();
        depth += 1;
    }
    Ok(Value::Bool(false))
}

/// `Object.prototype.propertyIsEnumerable(V)`.
pub fn object_property_is_enumerable(
    obj: &Value,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let key = match args.first() {
        Some(Value::String(s)) => PropertyKey::from_str(s),
        Some(Value::Symbol(sym)) => PropertyKey::Symbol(sym.id),
        Some(other) => PropertyKey::from_str(&other.to_js_string()),
        None => return Ok(Value::Bool(false)),
    };
    match obj {
        Value::Object(obj_ref) => Ok(Value::Bool(
            obj_ref
                .borrow()
                .property_get(&key)
                .is_some_and(|d| d.enumerable),
        )),
        _ => Ok(Value::Bool(false)),
    }
}

/// Name of the `Object.prototype.toString` native.
///
/// It needs a dedicated name because `Object.prototype.toString.call(x)` must
/// report the [[Class]] of `x` even when `x` is an Array or Number, whereas the
/// generic `toString` dispatch delegates to the receiver's own type.
pub const OBJECT_TO_STRING_NATIVE: &str = "Object.prototype.toString";

/// Populate `Object.prototype` with the methods every object inherits.
pub fn register_object_prototype(proto: &Rc<RefCell<dyn JSObject>>, object_fn: &Value) {
    use super::set_prototype_method;

    // Dedicated native so that `.call(receiver)` keeps Array/Number semantics.
    let _ = proto.borrow_mut().property_set(
        PropertyKey::from_str("toString"),
        Value::Object(Rc::new(RefCell::new(
            crate::vm::object::NativeFunctionObject::new(OBJECT_TO_STRING_NATIVE),
        ))),
    );
    set_prototype_method(proto, "valueOf", |this, _args| Ok(this.clone()));
    set_prototype_method(proto, "hasOwnProperty", |this, args| {
        object_has_own_property(this, args)
    });
    set_prototype_method(proto, "isPrototypeOf", |this, args| {
        object_is_prototype_of(this, args)
    });
    set_prototype_method(proto, "propertyIsEnumerable", |this, args| {
        object_property_is_enumerable(this, args)
    });
    set_prototype_method(proto, "toLocaleString", |this, args| {
        object_prototype_to_string(this, args)
    });
    let _ = proto
        .borrow_mut()
        .property_set(PropertyKey::from_str("constructor"), object_fn.clone());
}

pub fn register_object_statics(object_fn: &Value, _builtins: &super::Builtins) {
    set_static_method(object_fn, "keys", |args| object_keys(args));
    set_static_method(object_fn, "values", |args| object_values(args));
    set_static_method(object_fn, "entries", |args| object_entries(args));
    set_static_method(object_fn, "assign", |args| object_assign(args));
    set_static_method(object_fn, "defineProperty", |args| {
        object_define_property(args)
    });
    set_static_method(object_fn, "defineProperties", |args| {
        object_define_properties(args)
    });
    set_static_method(object_fn, "getOwnPropertyDescriptor", |args| {
        object_get_own_property_descriptor(args)
    });
    set_static_method(object_fn, "getOwnPropertyNames", |args| {
        object_get_own_property_names(args)
    });
    set_static_method(object_fn, "getOwnPropertySymbols", |args| {
        object_get_own_property_symbols(args)
    });
    set_static_method(object_fn, "getPrototypeOf", |args| {
        object_get_prototype_of(args)
    });
    set_static_method(object_fn, "setPrototypeOf", |args| {
        object_set_prototype_of(args)
    });
    set_static_method(object_fn, "create", |args| object_create(args));
    set_static_method(object_fn, "hasOwn", |args| object_has_own(args));
    set_static_method(object_fn, "is", |args| object_is(args));
    set_static_method(object_fn, "isExtensible", |args| Ok(Value::Bool(false)));
    set_static_method(object_fn, "isFrozen", |args| Ok(Value::Bool(false)));
    set_static_method(object_fn, "isSealed", |args| Ok(Value::Bool(false)));
    set_static_method(object_fn, "preventExtensions", |args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
    });
    set_static_method(object_fn, "seal", |args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
    });
    set_static_method(object_fn, "freeze", |args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
    });
}
