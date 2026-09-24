use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::vm::property::{ObjectKind, PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// ValueRef — shorthand for a reference-counted mutable JS value
pub type ValueRef = Rc<RefCell<Value>>;

/// Largest index a dense array element store will materialize.
///
/// Arrays are backed by a contiguous `Vec`, so a sparse write such as
/// `arr[2**32 - 1] = 1` would otherwise try to allocate ~96 GB and abort the
/// whole process.
pub const MAX_DENSE_ELEMENTS: usize = 1 << 20;

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

    /// Install a property with a full descriptor (`Object.defineProperty`).
    ///
    /// Every object kind keeps its own properties in a `PropertyTable`, so the
    /// descriptor attributes (`writable`/`enumerable`/`configurable`, getter,
    /// setter) survive on functions, arrays and prototype objects too — a class
    /// constructor is a `FunctionObject`, and `static get x()` is defined
    /// through this path.
    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String>;

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
// PropertyTable — own properties with a spec-ordered key list
// ─────────────────────────────────────────────────────────

/// `Some(i)` when `key` is a canonical array index (ES `ArrayIndex`).
///
/// The index is an integer string in `0 ..= 2^32 - 2` that round-trips through
/// its numeric value, so `"0"`/`"42"` qualify while `"01"`, `"-0"`, `"1.0"` and
/// `"4294967295"` do not.
fn array_index_of(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    let index: u32 = key.parse().ok()?;
    if index == u32::MAX {
        return None;
    }
    // `parse` accepts forms such as `+7` or `007`; only the canonical spelling
    // counts as an array index.
    (index.to_string() == key).then_some(index)
}

/// Own properties of an object, in creation order.
///
/// Property *order* is observable (`Object.getOwnPropertyNames`, `for-in`,
/// `Object.keys`, …). ES `OrdinaryOwnPropertyKeys` defines it as: array indices
/// ascending, then the remaining string keys in creation order, then symbol keys
/// in creation order. A bare `BTreeMap` cannot express that, so the map (fast
/// lookup) is paired with a creation-order list.
#[derive(Debug, Clone, Default)]
pub(crate) struct PropertyTable {
    values: BTreeMap<PropertyKey, PropertyDescriptor>,
    creation_order: Vec<PropertyKey>,
}

impl PropertyTable {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl FromIterator<(PropertyKey, PropertyDescriptor)> for PropertyTable {
    fn from_iter<I: IntoIterator<Item = (PropertyKey, PropertyDescriptor)>>(iter: I) -> Self {
        let mut table = PropertyTable::new();
        for (key, desc) in iter {
            table.insert(key, desc);
        }
        table
    }
}

impl PropertyTable {

    pub(crate) fn get(&self, key: &PropertyKey) -> Option<&PropertyDescriptor> {
        self.values.get(key)
    }

    pub(crate) fn get_mut(&mut self, key: &PropertyKey) -> Option<&mut PropertyDescriptor> {
        self.values.get_mut(key)
    }

    pub(crate) fn contains_key(&self, key: &PropertyKey) -> bool {
        self.values.contains_key(key)
    }

    /// Insert or replace a property, remembering a first-time key's position.
    pub(crate) fn insert(&mut self, key: PropertyKey, desc: PropertyDescriptor) {
        if self.values.insert(key.clone(), desc).is_none() {
            self.creation_order.push(key);
        }
    }

    pub(crate) fn remove(&mut self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        let removed = self.values.remove(key);
        if removed.is_some() {
            self.creation_order.retain(|existing| existing != key);
        }
        removed
    }

    /// Keys in `OrdinaryOwnPropertyKeys` order.
    pub(crate) fn keys(&self) -> Vec<PropertyKey> {
        let mut indices: Vec<(u32, PropertyKey)> = Vec::new();
        let mut strings: Vec<PropertyKey> = Vec::new();
        let mut symbols: Vec<PropertyKey> = Vec::new();

        for key in &self.creation_order {
            match key {
                PropertyKey::Str(s) => match array_index_of(s.as_str()) {
                    Some(index) => indices.push((index, key.clone())),
                    None => strings.push(key.clone()),
                },
                PropertyKey::Symbol(_) => symbols.push(key.clone()),
            }
        }

        indices.sort_by_key(|(index, _)| *index);
        let mut ordered: Vec<PropertyKey> = indices.into_iter().map(|(_, key)| key).collect();
        ordered.extend(strings);
        ordered.extend(symbols);
        ordered
    }
}

// ─────────────────────────────────────────────────────────
// Property descriptor validation (ES 6.1.7.3 / 9.1.6.3)
// ─────────────────────────────────────────────────────────

/// ES `SameValue` (7.2.9): like `===` but `NaN` equals `NaN` and `+0`/`-0`
/// are distinct.
pub(crate) fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if x.is_nan() && y.is_nan() {
                true
            } else if *x == 0.0 && *y == 0.0 {
                x.is_sign_positive() == y.is_sign_positive()
            } else {
                x == y
            }
        }
        _ => a.strict_eq(b),
    }
}

/// Validation half of `ValidateAndApplyOwnPropertyDescriptor` for an *existing*
/// property: may `desc` be applied over `current`?
///
/// Callers perform the apply step themselves (insert/replace), so this only
/// decides. The rejection reason doubles as the JS-visible error message.
pub(crate) fn validate_property_redefinition(
    current: &PropertyDescriptor,
    desc: &PropertyDescriptor,
) -> Result<(), String> {
    if current.configurable {
        return Ok(());
    }
    if desc.configurable {
        return Err("Cannot redefine non-configurable property".to_string());
    }
    if desc.enumerable != current.enumerable {
        return Err("Cannot redefine non-configurable property".to_string());
    }
    if desc.is_accessor_descriptor() {
        if current.is_data_descriptor() {
            return Err(
                "Cannot redefine non-configurable property: accessor over data".to_string(),
            );
        }
        if let (Some(d), Some(c)) = (&desc.getter, &current.getter) {
            if !same_value(d, c) {
                return Err("Cannot redefine non-configurable property: different getter"
                    .to_string());
            }
        }
        if let (Some(d), Some(c)) = (&desc.setter, &current.setter) {
            if !same_value(d, c) {
                return Err("Cannot redefine non-configurable property: different setter"
                    .to_string());
            }
        }
        if desc.getter.is_some() && current.getter.is_none() {
            return Err("Cannot redefine non-configurable property: added getter".to_string());
        }
        if desc.setter.is_some() && current.setter.is_none() {
            return Err("Cannot redefine non-configurable property: added setter".to_string());
        }
        return Ok(());
    }
    if current.is_accessor_descriptor() {
        // A data (or generic) descriptor over an existing accessor.
        return Err(
            "Cannot redefine non-configurable property: data over accessor".to_string(),
        );
    }
    if !current.writable {
        if !same_value(&desc.value, &current.value) {
            return Err(
                "Cannot redefine the value of a non-writable property".to_string(),
            );
        }
        if desc.writable {
            return Err("Cannot make a non-writable property writable".to_string());
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────
// OrdinaryObject — standard JS object
// ─────────────────────────────────────────────────────────

/// Standard JS object with named properties, prototype chain, and extensibility.
#[derive(Debug, Clone)]
pub struct OrdinaryObject {
    properties: PropertyTable,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    extensible: bool,
    frozen: bool,
    sealed: bool,
    /// `[[Class]]` as reported by `Object.prototype.toString` (ES 19.1.3.6).
    /// `"Object"` for everything the engine creates except error instances,
    /// which all report `"Error"`.
    class_name: &'static str,
}

impl OrdinaryObject {
    pub fn new() -> Self {
        Self {
            properties: PropertyTable::new(),
            prototype: None,
            extensible: true,
            frozen: false,
            sealed: false,
            class_name: "Object",
        }
    }

    /// An ordinary object with a non-default `[[Class]]`.
    pub fn with_class_name(class_name: &'static str) -> Self {
        Self {
            class_name,
            ..Self::new()
        }
    }

    pub fn with_prototype(proto: Rc<RefCell<dyn JSObject>>) -> Self {
        Self {
            properties: PropertyTable::new(),
            prototype: Some(proto),
            extensible: true,
            frozen: false,
            sealed: false,
            class_name: "Object",
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
            class_name: "Object",
        }
    }

    /// Install a property with a full descriptor (`Object.defineProperty`).
    ///
    /// Implements the ES `OrdinaryDefineOwnProperty` rules: a redefinition of a
    /// non-configurable property only succeeds when nothing observable changes
    /// (same value on a non-writable data property, same accessor, …); adding
    /// requires extensibility.
    pub fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        let current = self.properties.get(&key).cloned();
        match current {
            None => {
                if !self.extensible {
                    return Err("Cannot add property to non-extensible object".to_string());
                }
            }
            Some(current) => validate_property_redefinition(&current, &desc)?,
        }
        self.properties.insert(key, desc);
        Ok(true)
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

    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        OrdinaryObject::define_property(self, key, desc)
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        if self.frozen || self.sealed {
            return false;
        }
        // ES6 [[Delete]]: an own property whose [[Configurable]] is false cannot
        // be removed — the request reports failure instead.
        if let Some(desc) = self.properties.get(key) {
            if !desc.configurable {
                return false;
            }
        }
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys()
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
        // Freezing makes every own property non-configurable, and data
        // properties non-writable as well.
        for key in self.properties.keys() {
            if let Some(desc) = self.properties.get_mut(&key) {
                desc.configurable = false;
                if desc.is_data_descriptor() {
                    desc.writable = false;
                }
            }
        }
    }

    fn is_sealed(&self) -> bool {
        self.sealed
    }

    fn seal(&mut self) {
        self.sealed = true;
        self.extensible = false;
        // Sealing makes every own property non-configurable.
        for key in self.properties.keys() {
            if let Some(desc) = self.properties.get_mut(&key) {
                desc.configurable = false;
            }
        }
    }

    fn class_name(&self) -> &'static str {
        self.class_name
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
    /// Non-index own properties (`length` aside): `arr.x = 1`, methods hung
    /// off an instance, symbol keys. Elements *with non-default descriptors or
    /// accessors* also land here, keyed by their index string.
    properties: PropertyTable,
    /// Indices whose dense slot is not backed by a data property (accessor
    /// definitions, deletions). Their keys come from `properties` instead.
    holes: std::collections::BTreeSet<usize>,
    /// Array `length` starts writable; `Object.defineProperty(arr, "length",
    /// {writable: false})` can freeze it.
    length_writable: bool,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    extensible: bool,
    frozen: bool,
    sealed: bool,
}

impl ArrayObject {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            properties: PropertyTable::new(),
            holes: std::collections::BTreeSet::new(),
            length_writable: true,
            // Arrays created by the engine (literals, `Array(...)`, `map`,
            // `slice`, …) inherit from `Array.prototype` unconditionally.
            prototype: crate::builtins::wrapper_prototype("Array"),
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    /// `length` still writable, and the array not frozen — the precondition the
    /// mutating `Array.prototype` methods rely on.
    ///
    /// Those methods operate directly on the dense store, which bypasses the
    /// `[[Set]]` checks that would otherwise reject an assignment to a
    /// read-only `length`; every one of them ends with `? Set(O, "length", …,
    /// true)`, so it has to be validated explicitly.
    pub fn length_is_writable(&self) -> bool {
        self.length_writable && !self.frozen
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            elements: Vec::with_capacity(cap),
            properties: PropertyTable::new(),
            holes: std::collections::BTreeSet::new(),
            length_writable: true,
            // Arrays created by the engine (literals, `Array(...)`, `map`,
            // `slice`, …) inherit from `Array.prototype` unconditionally.
            prototype: crate::builtins::wrapper_prototype("Array"),
            extensible: true,
            frozen: false,
            sealed: false,
        }
    }

    pub fn from_vec(vec: Vec<Value>) -> Self {
        Self {
            elements: vec,
            properties: PropertyTable::new(),
            holes: std::collections::BTreeSet::new(),
            length_writable: true,
            // Arrays created by the engine (literals, `Array(...)`, `map`,
            // `slice`, …) inherit from `Array.prototype` unconditionally.
            prototype: crate::builtins::wrapper_prototype("Array"),
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
        self.join(",")
    }

    /// Mark a dense slot as a hole: it contributes no element (no own key,
    /// `in` reports false), while `length` is unchanged.
    ///
    /// `Array.prototype.map` needs this: holes are skipped by the callback but
    /// the result keeps the receiver's length.
    pub fn mark_hole(&mut self, index: usize) {
        if index < self.elements.len() {
            self.elements[index] = Value::Undefined;
        }
        self.properties.remove(&Self::index_key(index));
        self.holes.insert(index);
    }

    /// Drop every index-keyed entry from the property table (after a dense
    /// mutation that renumbers elements, e.g. `shift`/`splice`/`sort`).
    ///
    /// Attributes of plain data elements are not observable through these
    /// operations' fast paths, so the descriptors are simply discarded.
    fn drop_index_property_entries(&mut self) {
        self.holes.clear();
        let index_keys: Vec<PropertyKey> = self
            .properties
            .keys()
            .into_iter()
            .filter(|k| {
                k.as_str()
                    .map(|s| s.parse::<usize>().is_ok())
                    .unwrap_or(false)
            })
            .collect();
        for key in index_keys {
            self.properties.remove(&key);
        }
    }

    pub fn pop(&mut self) -> Value {
        self.elements.pop().unwrap_or(Value::Undefined)
    }

    pub fn shift(&mut self) -> Value {
        if self.elements.is_empty() {
            return Value::Undefined;
        }
        let value = Some(self.elements.remove(0)).unwrap_or(Value::Undefined);
        self.drop_index_property_entries();
        value
    }

    pub fn unshift(&mut self, value: Value) -> f64 {
        self.elements.insert(0, value);
        self.drop_index_property_entries();
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

    /// In-place `Array.prototype.reverse`.
    pub fn reverse(&mut self) {
        self.elements.reverse();
        self.drop_index_property_entries();
    }

    /// Replace all elements (used by `Array.prototype.sort`).
    pub fn replace_elements(&mut self, elements: Vec<Value>) {
        self.elements = elements;
        self.drop_index_property_entries();
    }

    /// `Array.prototype.join`: `null` and `undefined` (and holes, which are
    /// stored as `undefined`) contribute the empty string (ES 23.1.3.15).
    pub fn join(&self, separator: &str) -> String {
        self.elements
            .iter()
            .map(|v| match v {
                Value::Undefined | Value::Null => String::new(),
                other => other.to_js_string(),
            })
            .collect::<Vec<_>>()
            .join(separator)
    }

    pub fn slice(&self, start: usize, end: usize) -> Self {
        if start >= self.elements.len() {
            return Self::new();
        }
        let end = end.min(self.elements.len());
        // `Array.prototype.slice` allows `end <= start` (e.g. `a.slice(3, 1)`),
        // which must yield an empty array rather than panicking on a bad range.
        if end <= start {
            return Self::new();
        }
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
        self.drop_index_property_entries();
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
                // Array `length`: writable (until frozen so), non-enumerable,
                // non-configurable (ES 10.4.2.1).
                Some(PropertyDescriptor {
                    value: Value::Number(self.elements.len() as f64),
                    writable: self.length_writable,
                    enumerable: false,
                    configurable: false,
                    getter: None,
                    setter: None,
                })
            }
            // An index key with a stored override (non-default attributes or
            // an accessor) is reported from the property table; plain dense
            // elements keep the implicit default descriptor.
            PropertyKey::Str(_) if self.properties.contains_key(key) => {
                self.properties.get(key).cloned()
            }
            PropertyKey::Str(s) => {
                // Try parsing index
                if let Ok(idx) = s.parse::<usize>() {
                    // A hole is *absent*, so it must not shadow an inherited
                    // property: after `delete arr[1]`, `arr[1]` still sees
                    // `Array.prototype[1]` (and `has_property`/`own_keys` agree).
                    if self.holes.contains(&idx) {
                        return None;
                    }
                    self.elements
                        .get(idx)
                        .map(|v| PropertyDescriptor::data_descriptor(v.clone()))
                } else {
                    self.properties.get(key).cloned()
                }
            }
            _ => self.properties.get(key).cloned(),
        }
    }

    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        if self.frozen {
            return Err("Cannot set property on frozen array".to_string());
        }

        match &key {
            PropertyKey::Str(s) if s.as_str() == "length" => {
                if !self.length_writable {
                    return Err("Cannot assign to read-only property 'length'".to_string());
                }
                // Adjust array length (with the usual ArrayLength validation)
                let n = value.to_number();
                let new_len = crate::builtins::validate_array_length(n)
                    .map_err(|_| "Invalid array length".to_string())?;
                self.elements.resize(new_len, Value::Undefined);
                Ok(true)
            }
            // An existing table entry (index override or named property):
            // update through the descriptor, keeping the dense slot in sync.
            PropertyKey::Str(s) if self.properties.contains_key(&key) => {
                let idx = s.parse::<usize>().ok();
                let desc = self.properties.get_mut(&key).expect("checked above");
                if desc.is_accessor_descriptor() {
                    return Err("Cannot assign to accessor property".to_string());
                }
                if !desc.writable {
                    return Err("Cannot assign to read-only property".to_string());
                }
                desc.value = value.clone();
                if let Some(idx) = idx {
                    if let Some(slot) = self.elements.get_mut(idx) {
                        *slot = value;
                    }
                }
                Ok(true)
            }
            PropertyKey::Str(s) => {
                if let Ok(idx) = s.parse::<usize>() {
                    // Elements are stored densely, so refuse indices that would
                    // require an enormous (or non-representable) backing store.
                    // Without this, `arr[4294967295] = x` tries to allocate
                    // 2**32 slots (≈96 GB) and aborts the process.
                    if idx >= MAX_DENSE_ELEMENTS {
                        return Err(format!(
                            "Array index {idx} is out of the supported range"
                        ));
                    }
                    if idx >= self.elements.len() {
                        self.elements.resize(idx + 1, Value::Undefined);
                    }
                    // Assigning to a hole gives the index a value again, so it
                    // stops being absent (`delete arguments[0]` followed by
                    // `arguments[0] = x` must be observable).
                    self.holes.remove(&idx);
                    self.elements[idx] = value;
                    Ok(true)
                } else if let Some(existing) = self.properties.get(&key) {
                    if !existing.writable {
                        return Err("Cannot assign to read-only property".to_string());
                    }
                    self.properties
                        .insert(key, PropertyDescriptor::data_descriptor(value));
                    Ok(true)
                } else if !self.extensible {
                    Err("Cannot add property to non-extensible array".to_string())
                } else {
                    self.properties
                        .insert(key, PropertyDescriptor::data_descriptor(value));
                    Ok(true)
                }
            }
            _ => {
                if let Some(existing) = self.properties.get(&key) {
                    if !existing.writable {
                        return Err("Cannot assign to read-only property".to_string());
                    }
                } else if !self.extensible {
                    return Err("Cannot add property to non-extensible array".to_string());
                }
                self.properties
                    .insert(key, PropertyDescriptor::data_descriptor(value));
                Ok(true)
            }
        }
    }

    /// `Object.defineProperty` on an array.
    ///
    /// Plain elements (default attributes) stay in the dense store; elements
    /// with non-default attributes or accessors get a full descriptor in the
    /// property table (keyed by the index string) so the attributes survive.
    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        if self.frozen {
            return Err("Cannot define property on frozen array".to_string());
        }
        match &key {
            PropertyKey::Str(s) if s.as_str() == "length" => {
                if desc.is_accessor_descriptor() {
                    return Err("Cannot redefine property: length".to_string());
                }
                if desc.configurable {
                    return Err("Cannot redefine property: length is non-configurable".to_string());
                }
                let n = desc.value.to_number();
                let new_len = crate::builtins::validate_array_length(n)
                    .map_err(|_| "Invalid array length".to_string())?;
                if !self.length_writable && new_len != self.elements.len() {
                    return Err("Cannot redefine property: length is non-writable".to_string());
                }
                self.length_writable = desc.writable;
                self.elements.resize(new_len, Value::Undefined);
                self.holes.retain(|&i| i < new_len);
                // Drop index-keyed entries beyond the new length.
                let stale: Vec<PropertyKey> = self
                    .properties
                    .keys()
                    .into_iter()
                    .filter(|k| {
                        k.as_str()
                            .and_then(|s| s.parse::<usize>().ok())
                            .is_some_and(|i| i >= new_len)
                    })
                    .collect();
                for k in stale {
                    self.properties.remove(&k);
                }
                Ok(true)
            }
            PropertyKey::Str(s) => {
                if let Ok(idx) = s.parse::<usize>() {
                    if idx >= MAX_DENSE_ELEMENTS {
                        return Err(format!(
                            "Array index {idx} is out of the supported range"
                        ));
                    }
                    let current = self.property_get(&key);
                    match current {
                        Some(current) => validate_property_redefinition(&current, &desc)?,
                        None if !self.extensible => {
                            return Err("Cannot add property to non-extensible array".to_string());
                        }
                        None => {}
                    }
                    if desc.is_accessor_descriptor() {
                        // Accessors live in the property table; the dense slot
                        // is turned into a hole so reads find the accessor.
                        if idx >= self.elements.len() {
                            self.elements.resize(idx + 1, Value::Undefined);
                        }
                        self.elements[idx] = Value::Undefined;
                        self.holes.insert(idx);
                        self.properties.insert(key, desc);
                        return Ok(true);
                    }
                    if idx >= self.elements.len() {
                        self.elements.resize(idx + 1, Value::Undefined);
                    }
                    self.elements[idx] = desc.value.clone();
                    if desc.writable && desc.enumerable && desc.configurable {
                        // Default data descriptor: no override needed.
                        self.properties.remove(&key);
                        self.holes.remove(&idx);
                    } else {
                        self.properties.insert(key, desc);
                    }
                    Ok(true)
                } else {
                    let current = self.property_get(&key);
                    match current {
                        Some(current) => validate_property_redefinition(&current, &desc)?,
                        None if !self.extensible => {
                            return Err("Cannot add property to non-extensible array".to_string());
                        }
                        None => {}
                    }
                    self.properties.insert(key, desc);
                    Ok(true)
                }
            }
            _ => {
                let current = self.property_get(&key);
                match current {
                    Some(current) => validate_property_redefinition(&current, &desc)?,
                    None if !self.extensible => {
                        return Err("Cannot add property to non-extensible array".to_string());
                    }
                    None => {}
                }
                self.properties.insert(key, desc);
                Ok(true)
            }
        }
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        if self.frozen || self.sealed {
            return false;
        }
        if let PropertyKey::Str(s) = key {
            if s.as_str() == "length" {
                return false;
            }
            if let Ok(idx) = s.parse::<usize>() {
                if let Some(desc) = self.properties.get(key) {
                    if !desc.configurable {
                        return false;
                    }
                }
                if idx < self.elements.len() {
                    // Deleting an element leaves a hole; the length is unchanged.
                    self.properties.remove(key);
                    self.elements[idx] = Value::Undefined;
                    self.holes.insert(idx);
                    return true;
                }
                return self.properties.remove(key).is_some();
            }
        }
        if let Some(desc) = self.properties.get(key) {
            if !desc.configurable {
                return false;
            }
        }
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        match key {
            PropertyKey::Str(s) if s.as_str() == "length" => true,
            PropertyKey::Str(s) => {
                if self.properties.contains_key(key) {
                    return true;
                }
                s.parse::<usize>().is_ok_and(|i| {
                    i < self.elements.len() && !self.holes.contains(&i)
                })
            }
            _ => self.properties.contains_key(key),
        }
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        let mut keys: Vec<PropertyKey> = self
            .elements
            .iter()
            .enumerate()
            // Holes (deleted/accessor slots) have no data element; slots with
            // a table override are reported through the table instead.
            .filter(|(i, _)| {
                !self.holes.contains(i)
                    && !self.properties.contains_key(&Self::index_key(*i))
            })
            .map(|(i, _)| Self::index_key(i))
            .collect();
        keys.push(PropertyKey::from_str("length"));
        keys.extend(self.properties.keys());
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
        for key in self.properties.keys() {
            if let Some(desc) = self.properties.get_mut(&key) {
                desc.configurable = false;
                if desc.is_data_descriptor() {
                    desc.writable = false;
                }
            }
        }
        // Frozen data elements: the dense slots get `writable: false`.
        for i in 0..self.elements.len() {
            if self.holes.contains(&i) || self.properties.contains_key(&Self::index_key(i)) {
                continue;
            }
            let desc = PropertyDescriptor {
                value: self.elements[i].clone(),
                writable: false,
                enumerable: true,
                configurable: false,
                getter: None,
                setter: None,
            };
            self.properties.insert(Self::index_key(i), desc);
        }
        self.length_writable = false;
    }

    fn is_sealed(&self) -> bool {
        self.sealed
    }

    fn seal(&mut self) {
        self.sealed = true;
        self.extensible = false;
        for key in self.properties.keys() {
            if let Some(desc) = self.properties.get_mut(&key) {
                desc.configurable = false;
            }
        }
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
    properties: PropertyTable,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    /// For arrow functions: captured `this` value
    pub captured_this: Option<Value>,
    /// For arrow functions: the enclosing frame's `new.target` at creation
    /// time. Arrows have no `[[Construct]]`, so `new.target` inside an arrow
    /// must resolve to the constructor that created the enclosing frame.
    pub captured_new_target: Option<Value>,
    /// Captured outer variables as (name, value) pairs
    pub captured_vars: Vec<(String, Value)>,
}

impl FunctionObject {
    pub fn new(func_id: u32, name: &str) -> Self {
        Self {
            func_id,
            name: name.to_string(),
            properties: PropertyTable::new(),
            prototype: None,
            captured_this: None,
            captured_new_target: None,
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
            properties: PropertyTable::new(),
            prototype: None,
            captured_this: Some(captured_this),
            captured_new_target: None,
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

    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        // Class members are defined through this path (`static get x()`), so
        // the descriptor attributes have to survive on function objects.
        self.properties.insert(key, desc);
        Ok(true)
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys()
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
///
/// A normal function owns `length`, `name` and `prototype` own properties, in
/// that order (ES 9.2.3 / 9.2.4 / 10.2.5). Its `prototype` object's
/// `[[Prototype]]` is `Object.prototype` and it links back through
/// `constructor`. The back-reference is stored as a bare `Value::Function(id)`
/// rather than as the boxed function object, so that no `Rc` cycle is created
/// (the value compares equal to the boxed form and is boxed on demand by
/// property access).
pub fn new_function_object(func_id: u32, name: &str, arity: usize) -> Value {
    let func_obj = FunctionObject::new(func_id, name);
    let obj_ref: Rc<RefCell<dyn JSObject>> = Rc::new(RefCell::new(func_obj));

    /// `{ writable: false, enumerable: false, configurable: true }`
    fn fn_meta(value: Value) -> PropertyDescriptor {
        PropertyDescriptor {
            value,
            writable: false,
            enumerable: false,
            configurable: true,
            getter: None,
            setter: None,
        }
    }

    obj_ref.borrow_mut().define_property(
        crate::vm::property::PropertyKey::from_str("length"),
        fn_meta(Value::Number(arity as f64)),
    )
    .ok();
    obj_ref.borrow_mut().define_property(
        crate::vm::property::PropertyKey::from_str("name"),
        fn_meta(Value::string(name)),
    )
    .ok();

    // `F.prototype` inherits from `Object.prototype` (ES 9.2.3 step 4).
    let proto_obj = match crate::builtins::wrapper_prototype("Object") {
        Some(object_proto) => new_ordinary_object_with_prototype(object_proto),
        None => new_ordinary_object(),
    };

    if let Value::Object(ref proto_ref) = proto_obj {
        // `F.prototype.constructor` — writable, non-enumerable, configurable.
        proto_ref.borrow_mut().define_property(
            crate::vm::property::PropertyKey::from_str("constructor"),
            PropertyDescriptor {
                value: Value::Function(func_id),
                writable: true,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        )
        .ok();

        // `F.prototype` itself — writable, non-enumerable, non-configurable.
        obj_ref
            .borrow_mut()
            .define_property(
                crate::vm::property::PropertyKey::from("prototype"),
                PropertyDescriptor {
                    value: proto_obj.clone(),
                    writable: true,
                    enumerable: false,
                    configurable: false,
                    getter: None,
                    setter: None,
                },
            )
            .ok();
    }

    Value::Object(obj_ref)
}

/// Create a `Value::Object` wrapping an Arrow FunctionObject with captured `this`, the enclosing
/// frame's `new.target`, and captured vars.
///
/// An arrow is not a constructor: it owns `length` and `name` but has no
/// `prototype` property (ES 9.2.3 applies only to non-arrow functions).
pub fn new_arrow_function_object(
    func_id: u32,
    name: &str,
    arity: usize,
    captured_this: Value,
    captured_new_target: Value,
    captured_vars: Vec<(String, Value)>,
) -> Value {
    let mut func_obj = FunctionObject::new_arrow(func_id, name, captured_this, captured_vars);
    func_obj.captured_new_target = Some(captured_new_target);
    let obj_ref: Rc<RefCell<dyn JSObject>> = Rc::new(RefCell::new(func_obj));

    let mut borrowed = obj_ref.borrow_mut();
    borrowed
        .define_property(
            PropertyKey::from_str("length"),
            function_metadata_descriptor(Value::Number(arity as f64)),
        )
        .ok();
    borrowed
        .define_property(
            PropertyKey::from_str("name"),
            function_metadata_descriptor(Value::string(name)),
        )
        .ok();
    drop(borrowed);

    Value::Object(obj_ref)
}
// ─────────────────────────────────────────────────────────
// NativeFunctionObject — built-in function wrapper
// ─────────────────────────────────────────────────────────

/// Wrapper for built-in native functions (constructors and static methods).
/// Stored as `Value::Object` in the global environment.
/// When `CallEx` encounters this, it dispatches to the appropriate Rust handler.
///
/// Native functions still carry **own properties**: `Object.prototype`,
/// `Error.prototype`, `Array.isArray`, … are all ordinary properties hung off
/// the constructor. They used to be dropped on the floor, which made
/// `Object.prototype.toString.call(x)` (and most of `assert.js`) throw.
#[derive(Debug)]
pub struct NativeFunctionObject {
    pub name: String,
    /// `[[Prototype]]` of the function object itself.
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    /// Own properties (`length`, `name`, `prototype`, static methods, …).
    ///
    /// `length` / `name` live here like any other own property so that
    /// `Object.getOwnPropertyDescriptor`, `delete` and redefinition behave
    /// uniformly instead of going through special cases.
    properties: PropertyTable,
}

impl NativeFunctionObject {
    pub fn new(name: &str) -> Self {
        // Every built-in function object inherits from `Function.prototype`
        // (ES 17); the function's own `prototype` property is an ordinary own
        // property installed separately (`link_constructor_prototype`).
        let mut obj = Self {
            name: name.to_string(),
            prototype: crate::builtins::wrapper_prototype("Function"),
            properties: PropertyTable::new(),
        };
        obj.install_metadata();
        obj
    }

    /// Install the standard `length` / `name` own properties (ES 10.2.9/10.2.10):
    /// non-writable, non-enumerable, configurable.
    ///
    /// `length` comes from the built-in arity table. `name` is derived from the
    /// dispatch name, which is *not* the same string: the synthetic
    /// `__proto_method__` prefix and the `Owner.` prefix used for static-method
    /// dispatch are stripped (`Object.keys.name` is `"keys"`, not
    /// `"Object.keys"`), while `self.name` keeps the dispatch name intact.
    fn install_metadata(&mut self) {
        let display = if let Some(method) = self.name.strip_prefix(crate::builtins::PROTO_METHOD_PREFIX)
        {
            method.to_string()
        } else if self.name == crate::vm::iterator::ITERATOR_NATIVE_NAME {
            // `Array.prototype[Symbol.iterator].name` is "[Symbol.iterator]".
            "[Symbol.iterator]".to_string()
        } else if self.name == crate::builtins::ARRAY_SPECIES_NATIVE {
            // `Array[Symbol.species]`'s getter is named like any other accessor.
            "get [Symbol.species]".to_string()
        } else {
            self.name
                .rsplit('.')
                .next()
                .unwrap_or(&self.name)
                .to_string()
        };
        self.set_length(crate::builtins::builtin_arity(&self.name));
        self.properties.insert(
            PropertyKey::from_str("name"),
            function_metadata_descriptor(Value::string(&display)),
        );
    }

    /// Overwrite the `length` own property (used where the arity table cannot
    /// tell two same-named built-ins apart).
    pub fn set_length(&mut self, length: usize) {
        self.properties.insert(
            PropertyKey::from_str("length"),
            function_metadata_descriptor(Value::Number(length as f64)),
        );
    }
}

/// `{ writable: false, enumerable: false, configurable: true }` — the attribute
/// set shared by `Function.length` / `Function.name`.
pub(crate) fn function_metadata_descriptor(value: Value) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: false,
        enumerable: false,
        configurable: true,
        getter: None,
        setter: None,
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
        self.properties.get(key).cloned()
    }

    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        self.properties
            .insert(key, PropertyDescriptor::data_descriptor(value));
        Ok(true)
    }

    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        self.properties.insert(key, desc);
        Ok(true)
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        self.properties.contains_key(key)
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        self.properties.keys()
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

// ─────────────────────────────────────────────────────────
// PrimitiveWrapperObject — boxing wrapper (`Object(value)`, ToObject)
// ─────────────────────────────────────────────────────────

/// Object wrapper around a primitive value (ES `ToObject`).
///
/// `Object(1.1)`, `Object("ab")`, `Object(true)` and `Object(sym)` return an
/// object whose prototype is `Number.prototype` / `String.prototype` / … and
/// which converts back to the wrapped primitive (`obj == 1.1`, stringification,
/// arithmetic). String wrappers additionally expose the synthesized index keys
/// and `length` of the wrapped string.
#[derive(Debug)]
pub struct PrimitiveWrapperObject {
    primitive: Value,
    properties: PropertyTable,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
    extensible: bool,
}

impl PrimitiveWrapperObject {
    pub fn new(primitive: Value, prototype: Option<Rc<RefCell<dyn JSObject>>>) -> Self {
        Self {
            primitive,
            properties: PropertyTable::new(),
            prototype,
            extensible: true,
        }
    }

    fn string_len(&self) -> usize {
        match &self.primitive {
            Value::String(s) => s.chars().count(),
            _ => 0,
        }
    }
}

impl JSObject for PrimitiveWrapperObject {
    fn kind(&self) -> ObjectKind {
        match &self.primitive {
            Value::String(_) => ObjectKind::String,
            Value::Number(_) => ObjectKind::Number,
            Value::Bool(_) => ObjectKind::Boolean,
            Value::Symbol(_) => ObjectKind::Symbol,
            _ => ObjectKind::Ordinary,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        if let Value::String(s) = &self.primitive {
            if let PropertyKey::Str(k) = key {
                if k.as_str() == "length" {
                    return Some(PropertyDescriptor {
                        value: Value::Number(s.chars().count() as f64),
                        writable: false,
                        enumerable: false,
                        configurable: false,
                        getter: None,
                        setter: None,
                    });
                }
                if let Ok(idx) = k.parse::<usize>() {
                    return s.chars().nth(idx).map(|c| PropertyDescriptor {
                        value: Value::string(&c.to_string()),
                        writable: false,
                        enumerable: true,
                        configurable: false,
                        getter: None,
                        setter: None,
                    });
                }
            }
        }
        self.properties.get(key).cloned()
    }

    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String> {
        if matches!(self.primitive, Value::String(_)) {
            if let PropertyKey::Str(s) = &key {
                // String wrapper index keys and `length` are read-only.
                if s.as_str() == "length" || s.parse::<usize>().is_ok() {
                    return Err(
                        "Cannot assign to read-only property of a String wrapper".to_string()
                    );
                }
            }
        }
        self.properties
            .insert(key, PropertyDescriptor::data_descriptor(value));
        Ok(true)
    }

    fn define_property(
        &mut self,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        self.properties.insert(key, desc);
        Ok(true)
    }

    fn property_delete(&mut self, key: &PropertyKey) -> bool {
        if matches!(self.primitive, Value::String(_)) {
            if let PropertyKey::Str(s) = key {
                if s.as_str() == "length" || s.parse::<usize>().is_ok() {
                    return false;
                }
            }
        }
        self.properties.remove(key).is_some()
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        if let PropertyKey::Str(s) = key {
            if matches!(self.primitive, Value::String(_)) {
                if s.as_str() == "length" {
                    return true;
                }
                if let Ok(idx) = s.parse::<usize>() {
                    return idx < self.string_len();
                }
            }
        }
        self.properties.contains_key(key)
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        let mut keys: Vec<PropertyKey> = Vec::new();
        if let Value::String(s) = &self.primitive {
            for i in 0..s.chars().count() {
                keys.push(PropertyKey::Str(Rc::new(i.to_string())));
            }
        }
        keys.extend(self.properties.keys());
        if matches!(self.primitive, Value::String(_)) {
            keys.push(PropertyKey::from_str("length"));
        }
        keys
    }

    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>> {
        self.prototype.clone()
    }

    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>) {
        self.prototype = proto;
    }

    fn is_extensible(&self) -> bool {
        self.extensible
    }

    fn prevent_extensions(&mut self) {
        self.extensible = false;
    }

    fn is_frozen(&self) -> bool {
        false
    }

    fn freeze(&mut self) {
        self.extensible = false;
    }

    fn is_sealed(&self) -> bool {
        false
    }

    fn seal(&mut self) {
        self.extensible = false;
    }

    fn class_name(&self) -> &'static str {
        match &self.primitive {
            Value::String(_) => "String",
            Value::Number(_) => "Number",
            Value::Bool(_) => "Boolean",
            Value::Symbol(_) => "Symbol",
            _ => "Object",
        }
    }

    fn to_primitive(&self, _hint: &str) -> Result<Value, String> {
        Ok(self.primitive.clone())
    }
}

// ─────────────────────────────────────────────────────────
// GeneratorObject — `function*` instances (ES 25.4)
// ─────────────────────────────────────────────────────────

/// ES 25.4.2 generator states.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GeneratorState {
    /// `function*` called, body not started.
    SuspendedStart,
    /// Paused at a `yield`.
    SuspendedYield,
    /// Running (re-entry is a TypeError in the spec; here it cannot happen
    /// because resume is not re-entrant).
    Executing,
    /// The body returned or threw.
    Completed,
}

/// A generator frame that has been lifted out of the VM's value stack.
///
/// The VM keeps locals, arguments and expression temporaries in one contiguous
/// slice of `data_stack`, so a generator can be suspended by copying that slice
/// out and resumed by copying it back — no separate frame representation, and no
/// change to how the body is compiled.
#[derive(Debug, Clone)]
pub struct SuspendedFrame {
    /// The frame's slice of the value stack, arguments first.
    pub data: Vec<Value>,
    /// Number of leading slots that are arguments: `[rbp-argc, rbp)`.
    pub argc: usize,
    /// `rsp - rbp` at suspension time.
    pub bp_offset: usize,
    /// Index of the `Yield` instruction that suspended the frame; resuming
    /// writes the sent value and continues at `pc + 1`.
    pub pc: usize,
    pub this: Value,
    pub function_val: Value,
    pub closure_maps: Vec<std::collections::HashMap<String, Value>>,
    /// The 19-slot register file. It is **global**, not per frame, so a value
    /// held in a register across a `yield` has to travel with the frame.
    pub registers: [Value; 19],
    /// Iterators this frame's `yield*` left open. An abrupt completion has to
    /// close them (ES 14.4.14 step 5.c).
    pub delegates: Vec<Value>,
    // NOTE: no SEH records here — a `try` around a `yield` is out of scope for
    // the first generator increment (M4 follow-up), so a generator body with an
    // exception handler is simply not resumable across it.
}

/// A generator object.
///
/// `next()` is a native (`__gen_next__<id>`) rather than an own property, so it
/// can reach the VM and drive the body; `[Symbol.iterator]` returns the object
/// itself, which is what makes it usable in `for-of` and spread.
#[derive(Debug)]
pub struct GeneratorObject {
    pub id: u64,
    pub func_id: u32,
    pub state: GeneratorState,
    /// Arguments captured at call time (used for the very first resume).
    pub args: Vec<Value>,
    pub this: Value,
    pub captured_new_target: Option<Value>,
    pub captured_vars: Vec<(String, Value)>,
    /// `Some` only while suspended at a `yield`.
    pub suspended: Option<SuspendedFrame>,
    prototype: Option<Rc<RefCell<dyn JSObject>>>,
}

impl GeneratorObject {
    pub fn new(id: u64, func_id: u32, args: Vec<Value>, this: Value) -> Self {
        Self {
            id,
            func_id,
            state: GeneratorState::SuspendedStart,
            args,
            this,
            captured_new_target: None,
            captured_vars: Vec::new(),
            suspended: None,
            prototype: None,
        }
    }
}

impl JSObject for GeneratorObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Generator
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor> {
        // Generators are iterable: `gen[Symbol.iterator]()` is `gen` itself.
        if *key == crate::vm::iterator::iterator_symbol_key() {
            return Some(PropertyDescriptor::data_descriptor(Value::Object(
                Rc::new(RefCell::new(NativeFunctionObject::new(&format!(
                    "{}{}",
                    crate::vm::iterator::GENERATOR_ITERATOR_PREFIX,
                    self.id
                )))),
            )));
        }
        let native_name = match key.as_str() {
            Some("next") => format!(
                "{}{}",
                crate::vm::iterator::GENERATOR_NEXT_PREFIX,
                self.id
            ),
            Some("return") => format!(
                "{}{}",
                crate::vm::iterator::GENERATOR_RETURN_PREFIX,
                self.id
            ),
            Some("throw") => format!(
                "{}{}",
                crate::vm::iterator::GENERATOR_THROW_PREFIX,
                self.id
            ),
            _ => return None,
        };
        Some(PropertyDescriptor::data_descriptor(Value::Object(
            Rc::new(RefCell::new(NativeFunctionObject::new(&native_name))),
        )))
    }

    fn property_set(&mut self, _key: PropertyKey, _value: Value) -> Result<bool, String> {
        Ok(true)
    }

    fn define_property(
        &mut self,
        _key: PropertyKey,
        _desc: PropertyDescriptor,
    ) -> Result<bool, String> {
        Ok(true)
    }

    fn property_delete(&mut self, _key: &PropertyKey) -> bool {
        true
    }

    fn has_property(&self, key: &PropertyKey) -> bool {
        *key == crate::vm::iterator::iterator_symbol_key()
            || matches!(key.as_str(), Some("next") | Some("return") | Some("throw"))
    }

    fn own_keys(&self) -> Vec<PropertyKey> {
        vec![
            PropertyKey::from_str("next"),
            PropertyKey::from_str("return"),
            PropertyKey::from_str("throw"),
        ]
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

    fn class_name(&self) -> &'static str {
        "Generator"
    }

    fn to_primitive(&self, _hint: &str) -> Result<Value, String> {
        Ok(Value::string("[object Generator]"))
    }
}
