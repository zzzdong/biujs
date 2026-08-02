use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::vm::property::{ObjectKind, PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// ValueRef — shorthand for a reference-counted mutable JS value
pub type ValueRef = Rc<RefCell<Value>>;

// ─────────────────────────────────────────────────────────
// JSObject trait
// ─────────────────────────────────────────────────────────

/// Core trait for all JS object types.
///
/// All JS objects (Ordinary, Array, Function, etc.) implement this trait.
/// Objects are stored as `Rc<RefCell<dyn JSObject>>` inside `Value::Object`.
pub trait JSObject: fmt::Debug + Any {
    /// Returns the runtime kind of this object (Ordinary, Array, Function, etc.)
    fn kind(&self) -> ObjectKind;

    /// Downcasting support
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    // ── Property operations ──

    /// Get a property descriptor by key
    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor>;

    /// Set a property value, creating or updating as needed
    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String>;

    /// Delete a property, returns true if the property was successfully deleted
    fn property_delete(&mut self, key: &PropertyKey) -> bool;

    /// Check if a property exists
    fn has_property(&self, key: &PropertyKey) -> bool;

    /// Get all own property keys (not from prototype)
    fn own_keys(&self) -> Vec<PropertyKey>;

    // ── Prototype operations ──

    /// Get the prototype ([[Prototype]] internal slot)
    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>>;

    /// Set the prototype ([[SetPrototypeOf]])
    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>);

    // ── Extensibility ──

    /// Check if the object is extensible (can accept new properties)
    fn is_extensible(&self) -> bool;

    /// Prevent extensions on this object
    fn prevent_extensions(&mut self);

    /// Check if the object is frozen
    fn is_frozen(&self) -> bool;

    /// Freeze the object
    fn freeze(&mut self);

    /// Check if the object is sealed
    fn is_sealed(&self) -> bool;

    /// Seal the object
    fn seal(&mut self);

    // ── Type information ──

    /// Returns the typeof result for this object ("object" or "function")
    fn type_of(&self) -> &'static str {
        "object"
    }

    /// Returns the [[Class]] name (for Object.prototype.toString)
    fn class_name(&self) -> &'static str {
        "Object"
    }

    /// ToPrimitive abstract operation (ES6 7.1.1).
    ///
    /// Converts the object to a primitive value. The `hint` is one of
    /// `"default"`, `"number"`, or `"string"`.
    ///
    /// The default implementation tries `valueOf()` then `toString()` via
    /// the prototype chain. Override for custom behavior (e.g. Date uses
    /// `"string"` hint for `"default"`).
    fn to_primitive(&self, _hint: &str) -> Result<Value, String> {
        // OrdinaryToPrimitive (ES6 7.1.1.1)
        // With no builtins installed, we try the prototype chain for valueOf/toString.
        // Since we can't call functions yet, we use a fallback approach:
        // 1. Try to find a primitive through valueOf (lookup via internal_get)
        // 2. Then try toString
        // 3. Fall back to to_js_string representation

        // For now, since function calls aren't implemented,
        // the prototype chain won't have callable valueOf/toString.
        // Return an error to let the caller use the to_js_string fallback.
        Err("ToPrimitive: function calls not available".to_string())
    }
}

// ─────────────────────────────────────────────────────────
// OrdinaryObject — standard JS object
// ─────────────────────────────────────────────────────────

/// Standard JS object with named properties, prototype chain, and extensibility.
#[derive(Debug, Clone)]
pub struct OrdinaryObject {
    properties: BTreeMap<PropertyKey, PropertyDescriptor>,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    extensible: bool,
    frozen: bool,
    sealed: bool,
}

impl OrdinaryObject {
    pub fn new() -> Self {
        Self {
            properties: BTreeMap::new(),
            prototype: None,
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    pub fn with_prototype(proto: Rc<RefCell<dyn JSObject>>) -> Self {
        Self {
            properties: BTreeMap::new(),
            prototype: Some(proto),
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    pub fn from_map(map: BTreeMap<PropertyKey, Value>) -> Self {
        let properties = map
            .into_iter()
            .map(|(k, v)| (k, PropertyDescriptor::data_descriptor(v)))
            .collect();
        Self {
            properties,
            prototype: None,
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }
}

impl JSObject for OrdinaryObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Ordinary
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        self.properties.get(key).cloned()
    }

    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        if self.frozen {
            return Err("Cannot set property on frozen object".to_string());
        }

        if let Some(desc) = self.properties.get_mut(&key) {
            if !desc.writable {
                return Err("Cannot assign to read-only property".to_string());
            }
            desc.value = value;
            Ok(true)
        } else {
            if !self.extensible {
                return Err("Cannot add property to non-extensible object".to_string());
            }
            self.properties
                .insert(key, PropertyDescriptor::data_descriptor(value));
            Ok(true)
        }
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        if self.frozen || self.sealed {
            return false;
        }
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys().cloned().collect()
    }

    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        if !self.frozen {
            self.prototype = proto;
        }
    }

    fn is_extensible(&self) -> bool {
        self.extensible
    }

    fn prevent_extensions(&mut self) {
        self.extensible = false;
    }

    fn is_frozen(&self) -> bool {
        self.frozen
    }

    fn freeze(&mut self) {
        self.frozen = true;
        self.extensible = false;
        self.sealed = true;
    }

    fn is_sealed(&self) -> bool {
        self.sealed
    }

    fn seal(&mut self) {
        self.sealed = true;
        self.extensible = false;
    }

    fn class_name(&self) -> &'static str {
        "Object"
    }
}

impl Default for OrdinaryObject {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────
// ArrayObject — JS array (extends OrdinaryObject)
// ─────────────────────────────────────────────────────────

/// JS Array object.
///
/// Elements are stored at string-keyed indices (e.g. "0", "1", "2").
/// `length` is managed dynamically.
#[derive(Debug, Clone)]
pub struct ArrayObject {
    elements: Vec<Value>,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    extensible: bool,
    frozen: bool,
    sealed: bool,
}

impl ArrayObject {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            prototype: None,
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            elements: Vec::with_capacity(cap),
            prototype: None,
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    pub fn from_vec(vec: Vec<Value>) -> Self {
        Self {
            elements: vec,
            prototype: None,
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    pub fn push(&mut self, value: Value) {
        self.elements.push(value);
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.elements.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
        self.elements.get_mut(index)
    }

    /// Join array elements with comma separator (like Array.prototype.toString)
    pub fn join_elements(&self) -> String {
        self.elements
            .iter()
            .map(|v| v.to_js_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn pop(&mut self) -> Value {
        self.elements.pop().unwrap_or(Value::Undefined)
    }

    pub fn shift(&mut self) -> Value {
        if self.elements.is_empty() {
            return Value::Undefined;
        }
        Some(self.elements.remove(0)).unwrap_or(Value::Undefined)
    }

    pub fn unshift(&mut self, value: Value) -> f64 {
        self.elements.insert(0, value);
        self.elements.len() as f64
    }

    pub fn index_of(&self, needle: &Value, from_index: usize) -> isize {
        for i in from_index..self.elements.len() {
            if self.elements[i].strict_eq(needle) {
                return i as isize;
            }
        }
        -1
    }

    pub fn includes(&self, needle: &Value) -> bool {
        self.elements.iter().any(|v| v.strict_eq(needle))
    }

    pub fn join(&self, separator: &str) -> String {
        self.elements
            .iter()
            .map(|v| v.to_js_string())
            .collect::<Vec<_>>()
            .join(separator)
    }

    pub fn slice(&self, start: usize, end: usize) -> Self {
        if start >= self.elements.len() {
            return Self::new();
        }
        let end = end.min(self.elements.len());
        Self::from_vec(self.elements[start..end].to_vec())
    }

    pub fn concat(&self, other: &[Value]) -> Self {
        let mut new_elements = self.elements.clone();
        new_elements.extend_from_slice(other);
        Self::from_vec(new_elements)
    }

    pub fn splice(
        &mut self,
        start: usize,
        delete_count: usize,
        insert_items: &[Value],
    ) -> Vec<Value> {
        let end = (start + delete_count).min(self.elements.len());
        let removed: Vec<Value> = self.elements.drain(start..end).collect();
        let mut pos = start;
        for item in insert_items {
            self.elements.insert(pos, item.clone());
            pos += 1;
        }
        removed
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    fn index_key(index: usize) -> PropertyKey {
        PropertyKey::Str(std::rc::Rc::new(index.to_string()))
    }
}

impl JSObject for ArrayObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Array
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        match key {
            PropertyKey::Str(s) if s.as_str() == "length" => {
                Some(PropertyDescriptor::writable_data_descriptor(
                    Value::Number(self.elements.len() as f64),
                    false,
                ))
            }
            PropertyKey::Str(s) => {
                // Try parsing index
                if let Ok(idx) = s.parse::<usize>() {
                    self.elements
                        .get(idx)
                        .map(|v| PropertyDescriptor::data_descriptor(v.clone()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        if self.frozen {
            return Err("Cannot set property on frozen array".to_string());
        }

        match &key {
            PropertyKey::Str(s) if s.as_str() == "length" => {
                // Adjust array length
                if let Value::Number(n) = value {
                    let new_len = n as usize;
                    self.elements.resize(new_len, Value::Undefined);
                    Ok(true)
                } else {
                    Err("Invalid length value".to_string())
                }
            }
            PropertyKey::Str(s) => {
                if let Ok(idx) = s.parse::<usize>() {
                    if idx >= self.elements.len() {
                        self.elements.resize(idx + 1, Value::Undefined);
                    }
                    self.elements[idx] = value;
                    Ok(true)
                } else {
                    Err(format!("Cannot set non-index property on array: {s}"))
                }
            }
            _ => Err("Cannot set symbol property on array".to_string()),
        }
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        if self.frozen || self.sealed {
            return false;
        }
        if let PropertyKey::Str(s) = key {
            if let Ok(idx) = s.parse::<usize>() {
                if idx < self.elements.len() {
                    self.elements.remove(idx);
                    return true;
                }
            }
        }
        false
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        match key {
            PropertyKey::Str(s) if s.as_str() == "length" => true,
            PropertyKey::Str(s) => s
                .parse::<usize>()
                .ok()
                .map_or(false, |i| i < self.elements.len()),
            _ => false,
        }
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        let mut keys: Vec<PropertyKey> = self
            .elements
            .iter()
            .enumerate()
            .map(|(i, _)| Self::index_key(i))
            .collect();
        keys.push(PropertyKey::from_str("length"));
        keys
    }

    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        if !self.frozen {
            self.prototype = proto;
        }
    }

    fn is_extensible(&self) -> bool {
        self.extensible
    }

    fn prevent_extensions(&mut self) {
        self.extensible = false;
    }

    fn is_frozen(&self) -> bool {
        self.frozen
    }

    fn freeze(&mut self) {
        self.frozen = true;
        self.extensible = false;
        self.sealed = true;
    }

    fn is_sealed(&self) -> bool {
        self.sealed
    }

    fn seal(&mut self) {
        self.sealed = true;
        self.extensible = false;
    }

    fn type_of(&self) -> &'static str {
        "object"
    }

    fn class_name(&self) -> &'static str {
        "Array"
    }

    fn to_primitive(&self, _hint: &str) -> Result<Value, String> {
        // Array ToPrimitive:
        // 1. valueOf() returns the array itself (not primitive)
        // 2. toString() joins elements with comma
        // For hint "default" (used by ==), we go directly to toString behavior
        Ok(Value::string(&self.join_elements()))
    }
}

impl Default for ArrayObject {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────
// Helper: create Value::Object from concrete types
// ─────────────────────────────────────────────────────────

/// Create a `Value::Object` wrapping an OrdinaryObject
pub fn new_ordinary_object() -> Value {
    Value::Object(Rc::new(RefCell::new(OrdinaryObject::new())))
}

/// Create a `Value::Object` wrapping an OrdinaryObject with a prototype
pub fn new_ordinary_object_with_prototype(proto: Rc<RefCell<dyn JSObject>>) -> Value {
    Value::Object(Rc::new(RefCell::new(OrdinaryObject::with_prototype(proto))))
}

/// Create a `Value::Object` wrapping an ArrayObject
pub fn new_array_object() -> Value {
    Value::Object(Rc::new(RefCell::new(ArrayObject::new())))
}

/// Create a `Value::Object` wrapping an ArrayObject from a Vec
pub fn new_array_object_from_vec(vec: Vec<Value>) -> Value {
    Value::Object(Rc::new(RefCell::new(ArrayObject::from_vec(vec))))
}

// ─────────────────────────────────────────────────────────
// FunctionObject — user-defined bytecode function wrapper with properties
// ─────────────────────────────────────────────────────────

/// Wrapper for user-defined bytecode functions that need properties
/// (e.g., class constructors that need a `.prototype` property).
/// Stored as `Value::Object` with `ObjectKind::Function`.
/// When `CallEx` encounters this, it extracts the `func_id` and jumps to it.
#[derive(Debug)]
pub struct FunctionObject {
    pub func_id: u32,
    pub name: String,
    properties: BTreeMap<PropertyKey, PropertyDescriptor>,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    /// For arrow functions: captured `this` value
    pub captured_this: Option<Value>,
    /// Captured outer variables as (name, value) pairs
    pub captured_vars: Vec<(String, Value)>,
}

impl FunctionObject {
    pub fn new(func_id: u32, name: &str) -> Self {
        Self {
            func_id,
            name: name.to_string(),
            properties: BTreeMap::new(),
            prototype: None,
            captured_this: None,
            captured_vars: Vec::new(),
        }
    }

    /// Create an arrow function object with captured `this` and optional captured vars
    pub fn new_arrow(
        func_id: u32,
        name: &str,
        captured_this: Value,
        captured_vars: Vec<(String, Value)>,
    ) -> Self {
        Self {
            func_id,
            name: name.to_string(),
            properties: BTreeMap::new(),
            prototype: None,
            captured_this: Some(captured_this),
            captured_vars,
        }
    }
}

impl JSObject for FunctionObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Function
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        self.properties.get(key).cloned()
    }

    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        self.properties
            .insert(key, PropertyDescriptor::data_descriptor(value));
        Ok(true)
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys().cloned().collect()
    }

    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        self.prototype = proto;
    }

    fn is_extensible(&self) -> bool {
        true
    }

    fn prevent_extensions(&mut self) {}

    fn is_frozen(&self) -> bool {
        false
    }

    fn freeze(&mut self) {}

    fn is_sealed(&self) -> bool {
        false
    }

    fn seal(&mut self) {}

    fn type_of(&self) -> &'static str {
        "function"
    }

    fn class_name(&self) -> &'static str {
        "Function"
    }
}

/// Create a `Value::Object` wrapping a FunctionObject with a prototype property
pub fn new_function_object(func_id: u32, name: &str) -> Value {
    let func_obj = FunctionObject::new(func_id, name);
    let obj_ref: Rc<RefCell<dyn JSObject>> = Rc::new(RefCell::new(func_obj));

    // Create a prototype object for this function
    let proto_obj = new_ordinary_object();

    // Set the prototype property on the function object
    if let Value::Object(ref proto_ref) = proto_obj {
        obj_ref
            .borrow_mut()
            .property_set(
                crate::vm::property::PropertyKey::from("prototype"),
                proto_obj.clone(),
            )
            .ok();

        // Note: We don't set constructor property on prototype to avoid circular reference
        // This is a simplification - in a full implementation, we'd need to handle this differently
    }

    Value::Object(obj_ref)
}

/// Create a `Value::Object` wrapping an Arrow FunctionObject with captured `this` and vars
pub fn new_arrow_function_object(
    func_id: u32,
    name: &str,
    captured_this: Value,
    captured_vars: Vec<(String, Value)>,
) -> Value {
    let func_obj = FunctionObject::new_arrow(func_id, name, captured_this, captured_vars);
    let obj_ref: Rc<RefCell<dyn JSObject>> = Rc::new(RefCell::new(func_obj));

    // Arrow functions don't have a prototype property
    Value::Object(obj_ref)
}

// ─────────────────────────────────────────────────────────
// NativeFunctionObject — built-in function wrapper
// ─────────────────────────────────────────────────────────

/// Wrapper for built-in native functions (constructors and static methods).
/// Stored as `Value::Object` in the global environment.
/// When `CallEx` encounters this, it dispatches to the appropriate Rust handler.
#[derive(Debug)]
pub struct NativeFunctionObject {
    pub name: String,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
}

impl NativeFunctionObject {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            prototype: None,
        }
    }

    pub fn with_prototype(name: &str, proto: Rc<RefCell<dyn JSObject>>) -> Self {
        Self {
            name: name.to_string(),
            prototype: Some(proto),
        }
    }
}

impl JSObject for NativeFunctionObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::NativeFunction
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        // NativeFunction has no own properties by default
        None
    }

    fn property_set(&mut self, _key: PropertyKey, _value: Value) -> Result<bool, String> {
        Ok(false)
    }

    fn property_delete(&mut self, _key: &PropertyKey) -> bool {
        false
    }

    fn has_property(&self, _key: &PropertyKey) -> bool {
        false
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        vec![]
    }

    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        self.prototype = proto;
    }

    fn is_extensible(&self) -> bool {
        false
    }

    fn prevent_extensions(&mut self) {}

    fn is_frozen(&self) -> bool {
        false
    }

    fn freeze(&mut self) {}

    fn is_sealed(&self) -> bool {
        false
    }

    fn seal(&mut self) {}

    fn type_of(&self) -> &'static str {
        "function"
    }

    fn class_name(&self) -> &'static str {
        "Function"
    }
}
