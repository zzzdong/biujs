//! Proxy 陷阱分派（ES 28.2 / 10.5）。
//!
//!  ─────────────────────────────────────────────────────
//!  Proxy (ES 28.2 / 10.5) — trap dispatch
//!  ─────────────────────────────────────────────────────
//!
//!  The rule every method here leans on is ES 10.5's: an operation whose trap
//!  the handler does not have is *not* an error, it is the same operation on
//!  the target. So each `proxy_*` answers an `Option`: `None` means "forward",
//!  and the caller then does exactly what it would have done for an ordinary
//!  object — which works because `ProxyObject`'s object-layer methods already
//!  forward to the target.
//!
//!  The traps that are *not* here (`ownKeys`, `getOwnPropertyDescriptor`,
//!  `defineProperty`, `getPrototypeOf`, `setPrototypeOf`, `isExtensible`,
//!  `preventExtensions`) are therefore "forward" today: a handler that sets
//!  them is not called. See the batch record in the plan for why.

use std::rc::Rc;

use crate::bytecode::Module;
use crate::vm::gc::{Gc, Object};
use crate::vm::object::JSObject;
use crate::vm::{ObjectKind, PropertyDescriptor, PropertyKey, REVOKE_PREFIX, RuntimeError, Value, VM};

impl VM {
    pub(crate) fn is_proxy(value: &Value) -> bool {
        match value {
            Value::Object(obj_ref) => obj_ref.borrow().kind() == ObjectKind::Proxy,
            _ => false,
        }
    }

    /// `[[ProxyTarget]]` and `[[ProxyHandler]]`.
    ///
    /// A revoked proxy throws on *every* operation, trap or not — that is the
    /// whole point of `revoke` (ES 28.2.2.2).
    pub(crate) fn proxy_parts(&self, proxy: &Value, what: &str) -> Result<(Value, Value), RuntimeError> {
        match proxy {
            Value::Object(obj_ref) => {
                let borrowed = obj_ref.borrow();
                match borrowed
                    .as_any()
                    .downcast_ref::<crate::vm::object::ProxyObject>()
                {
                    Some(p) if !p.revoked => Ok((p.target.clone(), p.handler.clone())),
                    Some(_) => Err(RuntimeError::TypeError(format!(
                        "Cannot perform '{what}' on a proxy that has been revoked"
                    ))),
                    None => Err(RuntimeError::InternalError(
                        "not a proxy object".to_string(),
                    )),
                }
            }
            _ => Err(RuntimeError::InternalError(
                "not a proxy object".to_string(),
            )),
        }
    }

    /// The handler's trap `trap_name`, together with the target and the handler
    /// it must be called on. `None` = the handler has no such trap.
    pub(crate) fn proxy_trap(
        &mut self,
        proxy: &Value,
        trap_name: &str,
        module: &Module,
    ) -> Result<Option<(Value, Value, Value)>, RuntimeError> {
        let (target, handler) = self.proxy_parts(proxy, trap_name)?;
        // Reading the trap is an ordinary `[[Get]]` on the handler: a handler
        // with an accessor for `get` is legal, and only the VM can run it.
        let trap = match &handler {
            Value::Object(_) => {
                self.get_member(&handler, &PropertyKey::from_str(trap_name), module)?
            }
            _ => Value::Undefined,
        };
        // `GetMethod`: `null` and `undefined` both mean "no trap" — that is why
        // `{ get: null }` is a pass-through rather than an error.
        if trap.is_undefined() || matches!(trap, Value::Null) {
            return Ok(None);
        }
        if !trap.is_callable() {
            return Err(RuntimeError::TypeError(format!(
                "Proxy handler's '{trap_name}' trap is not a function"
            )));
        }
        Ok(Some((target, handler, trap)))
    }

    /// A `PropertyKey` as the value a trap is called with: the key is an
    /// ordinary argument, symbols included.
    pub(crate) fn key_to_value(key: &PropertyKey) -> Value {
        match key {
            PropertyKey::Str(text) => Value::string(text),
            PropertyKey::Symbol(id) => {
                Value::Symbol(Rc::new(crate::vm::value::SymbolData::new(None, *id)))
            }
        }
    }

    /// The target's *own* descriptor for `key`, for the invariant checks below.
    pub(crate) fn proxy_target_descriptor(target: &Value, key: &PropertyKey) -> Option<PropertyDescriptor> {
        match target {
            Value::Object(obj_ref) => obj_ref.borrow().property_get(key),
            _ => None,
        }
    }

    /// The target's own keys, for the `ownKeys` invariants.
    pub(crate) fn proxy_target_keys(target: &Value) -> Vec<PropertyKey> {
        match target {
            Value::Object(obj_ref) => obj_ref.borrow().own_keys(),
            _ => Vec::new(),
        }
    }

    /// `[[Get]]` through a proxy (ES 10.5.8).
    ///
    /// With no trap the operation is the *target's* `[[Get]]`, which is a
    /// recursive call rather than a fallthrough: the target may be a proxy too,
    /// and its own traps have to fire (`get/trap-is-null-target-is-proxy`).
    pub(crate) fn proxy_get(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        receiver: &Value,
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "get", module)? else {
            return Ok(Some(self.get_with_receiver(
                &self.proxy_parts(proxy, "get")?.0,
                key,
                receiver,
                module,
            )?));
        };
        let key_value = Self::key_to_value(key);
        let value = self.invoke(
            &trap,
            handler,
            &[target.clone(), key_value, receiver.clone()],
            module,
        )?;
        // Invariant (ES 10.5.8 step 17): a non-writable, non-configurable own
        // data property of the target cannot be reported as anything else —
        // otherwise `Object.freeze(t)` would be observable as unfrozen.
        if let Some(desc) = Self::proxy_target_descriptor(&target, key) {
            if desc.is_data_descriptor() && !desc.writable && !desc.configurable {
                if !value.strict_eq(&desc.value) {
                    return Err(RuntimeError::TypeError(format!(
                        "'get' on proxy: property '{}' is a read-only and non-configurable data property on the proxy target but the proxy did not return its actual value (expected '{}' but got '{}')",
                        key.display(),
                        desc.value.to_js_string(),
                        value.to_js_string()
                    )));
                }
            }
        }
        Ok(Some(value))
    }

    /// `[[Set]]` through a proxy (ES 10.5.9), answering whether the write
    /// succeeded.
    ///
    /// The two callers disagree about a falsish trap: an assignment turns it
    /// into a TypeError (the engine has no sloppy mode), while `Reflect.set`
    /// reports it as `false`. So this answers the boolean and lets them decide.
    pub(crate) fn proxy_set(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        value: &Value,
        receiver: &Value,
        module: &Module,
    ) -> Result<bool, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "set", module)? else {
            // No trap: `[[Set]]` on the target, receiver included — the target
            // may itself be a proxy, which is why this is a recursive dispatch.
            let target = self.proxy_parts(proxy, "set")?.0;
            return self.set_with_receiver(&target, key, value, receiver, module);
        };
        let answer = self.invoke(
            &trap,
            handler,
            &[
                target.clone(),
                Self::key_to_value(key),
                value.clone(),
                receiver.clone(),
            ],
            module,
        )?;
        // Invariant (step 21): a non-writable, non-configurable own data
        // property of the target cannot be reported as written.
        if let Some(desc) = Self::proxy_target_descriptor(&target, key) {
            if desc.is_data_descriptor() && !desc.writable && !desc.configurable {
                if !value.strict_eq(&desc.value) {
                    return Err(RuntimeError::TypeError(format!(
                        "'set' on proxy: trap returned truish for property '{}' which exists in the proxy target as a non-configurable and non-writable data property with a different value",
                        key.display()
                    )));
                }
            }
        }
        Ok(answer.to_boolean())
    }

    /// `[[HasProperty]]` through a proxy (ES 10.5.7).
    pub(crate) fn proxy_has(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Option<bool>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "has", module)? else {
            // No trap: `[[HasProperty]]` on the target, which may be a proxy.
            let target = self.proxy_parts(proxy, "has")?.0;
            if Self::is_proxy(&target) {
                return self.proxy_has(&target, key, module);
            }
            return match &target {
                Value::Object(obj_ref) => Ok(Some(
                    crate::vm::prototype::internal_has_property(*obj_ref, key)
                        .map_err(RuntimeError::TypeError)?,
                )),
                _ => Ok(Some(false)),
            };
        };
        let answer = self
            .invoke(
                &trap,
                handler,
                &[target.clone(), Self::key_to_value(key)],
                module,
            )?
            .to_boolean();
        // Invariants (ES 10.5.7): an existing non-configurable property, or any
        // own property of a non-extensible target, cannot be reported absent.
        if !answer {
            let extensible = match &target {
                Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                _ => true,
            };
            let desc = Self::proxy_target_descriptor(&target, key);
            let must_report = match &desc {
                Some(desc) => !desc.configurable || !extensible,
                None => false,
            };
            if must_report {
                return Err(RuntimeError::TypeError(format!(
                    "'has' on proxy: trap returned falsish for property '{}' which exists in the proxy target as non-configurable",
                    key.display()
                )));
            }
        }
        Ok(Some(answer))
    }

    /// `[[Delete]]` through a proxy (ES 10.5.10).
    pub(crate) fn proxy_delete(
        &mut self,
        proxy: &Value,
        key: &PropertyKey,
        module: &Module,
    ) -> Result<Option<bool>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "deleteProperty", module)?
        else {
            // No trap: `[[Delete]]` on the target, which may be a proxy.
            let target = self.proxy_parts(proxy, "deleteProperty")?.0;
            if Self::is_proxy(&target) {
                return self.proxy_delete(&target, key, module);
            }
            return match &target {
                Value::Object(obj_ref) => Ok(Some(
                    crate::vm::prototype::internal_delete(*obj_ref, key)
                        .map_err(RuntimeError::TypeError)?,
                )),
                _ => Ok(Some(true)),
            };
        };
        let answer = self
            .invoke(
                &trap,
                handler,
                &[target.clone(), Self::key_to_value(key)],
                module,
            )?
            .to_boolean();
        // Invariant (ES 10.5.10): a non-configurable own property cannot be
        // reported as deleted. (A *frozen* target's property is covered too:
        // frozen implies non-configurable.)
        if answer {
            if let Some(desc) = Self::proxy_target_descriptor(&target, key) {
                if !desc.configurable {
                    return Err(RuntimeError::TypeError(format!(
                        "'deleteProperty' on proxy: trap returned truish for property '{}' which is non-configurable in the proxy target",
                        key.display()
                    )));
                }
            }
        }
        Ok(Some(answer))
    }

    /// `[[Call]]` through a proxy (ES 10.5.12). `None` = no `apply` trap, which
    /// forwards to calling the *target* with the same `this` and arguments.
    pub(crate) fn proxy_apply(
        &mut self,
        proxy: &Value,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "apply", module)? else {
            return Ok(None);
        };
        let list = Value::Object(self.heap.alloc(Object::Array(
            crate::vm::object::ArrayObject::from_vec(args.to_vec()),
        )));
        Ok(Some(self.invoke(
            &trap,
            handler,
            &[target, this.clone(), list],
            module,
        )?))
    }

    /// `[[Construct]]` through a proxy (ES 10.5.13). `None` = no `construct`
    /// trap, which forwards to constructing the *target*.
    pub(crate) fn proxy_construct_trap(
        &mut self,
        proxy: &Value,
        args: &[Value],
        new_target: &Value,
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some((target, handler, trap)) = self.proxy_trap(proxy, "construct", module)? else {
            return Ok(None);
        };
        let list = Value::Object(self.heap.alloc(Object::Array(
            crate::vm::object::ArrayObject::from_vec(args.to_vec()),
        )));
        let result = self.invoke(&trap, handler, &[target, list, new_target.clone()], module)?;
        // `[[Construct]]` has to answer an object: a trap returning a primitive
        // is a TypeError, not a coercion.
        if !result.is_object() {
            return Err(RuntimeError::TypeError(
                "'construct' on proxy: trap returned a non-object".to_string(),
            ));
        }
        Ok(Some(result))
    }

    /// The traps that hang off `Object.*` / `Reflect.*` instead of an operator:
    /// `[[GetPrototypeOf]]` … `[[DefineOwnProperty]]`. Every one of them takes
    /// the target first, so "the receiver is a proxy" is all it takes to
    /// recognise the call.
    ///
    /// `None` = the ordinary path answers — no such trap, or not a proxy — and
    /// for a proxy that path is the same operation on the target.
    ///
    /// The invariant checks here are the ones a *user* could otherwise break:
    /// without them a proxy could report a frozen target as extensible, or a
    /// non-configurable property as absent.
    pub(crate) fn proxy_static_trap(
        &mut self,
        name: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        let trap_name = match name {
            "Object.getPrototypeOf" | "Reflect.getPrototypeOf" => "getPrototypeOf",
            // `Reflect.has` / `Reflect.deleteProperty` are the same two
            // operations the `in` and `delete` operators perform.
            "Reflect.has" => "has",
            "Reflect.deleteProperty" => "deleteProperty",
            "Object.setPrototypeOf" | "Reflect.setPrototypeOf" => "setPrototypeOf",
            "Object.isExtensible" | "Reflect.isExtensible" => "isExtensible",
            "Object.preventExtensions" | "Reflect.preventExtensions" => "preventExtensions",
            "Object.getOwnPropertyDescriptor" | "Reflect.getOwnPropertyDescriptor" => {
                "getOwnPropertyDescriptor"
            }
            // The four "list the keys" operations are one trap.
            "Object.keys"
            | "Object.getOwnPropertyNames"
            | "Object.getOwnPropertySymbols"
            | "Reflect.ownKeys" => "ownKeys",
            "Object.defineProperty" | "Reflect.defineProperty" => "defineProperty",
            _ => return Ok(None),
        };
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        if !Self::is_proxy(&receiver) {
            return Ok(None);
        }
        let (target, handler, trap) = match self.proxy_trap(&receiver, trap_name, module)? {
            Some(parts) => parts,
            None => {
                // No trap: the same operation on the target. It is a *recursive*
                // dispatch rather than a fallthrough to the builtin layer, so a
                // target that is itself a proxy still gets its own trap called.
                if !Self::is_proxy(&self.proxy_parts(&receiver, trap_name)?.0) {
                    return Ok(None);
                }
                let mut forwarded = args.to_vec();
                forwarded[0] = self.proxy_parts(&receiver, trap_name)?.0;
                return self.proxy_static_trap(name, &forwarded, module);
            }
        };
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        // `Reflect.*` answers a boolean where `Object.*` throws; that is the
        // only difference between the two spellings of the same operation.
        let reflects = name.starts_with("Reflect");
        match trap_name {
            "getPrototypeOf" => {
                let result = self.invoke(&trap, handler, &[target.clone()], module)?;
                if !result.is_object() && !matches!(result, Value::Null) {
                    return Err(RuntimeError::TypeError(
                        "'getPrototypeOf' on proxy: trap returned neither an object nor null"
                            .to_string(),
                    ));
                }
                // ES 10.5.17: a non-extensible target's prototype is whatever it
                // is — the trap may not report a different one.
                if let Value::Object(obj_ref) = &target {
                    if !obj_ref.borrow().is_extensible()
                        && !Self::same_prototype(&obj_ref.borrow().get_prototype(), &result)
                    {
                        return Err(RuntimeError::TypeError(
                            "'getPrototypeOf' on proxy: trap returned a different prototype for a non-extensible target"
                                .to_string(),
                        ));
                    }
                }
                Ok(Some(result))
            }
            "setPrototypeOf" => {
                let proto = key.clone();
                if !proto.is_object() && !matches!(proto, Value::Null) {
                    return Err(RuntimeError::TypeError(
                        "Object prototype may only be an Object or null".to_string(),
                    ));
                }
                let answer = self
                    .invoke(&trap, handler, &[target.clone(), proto.clone()], module)?
                    .to_boolean();
                if answer {
                    // ES 10.5.18: a non-extensible target's prototype cannot be
                    // replaced.
                    if let Value::Object(obj_ref) = &target {
                        if !obj_ref.borrow().is_extensible()
                            && !Self::same_prototype(&obj_ref.borrow().get_prototype(), &proto)
                        {
                            return Err(RuntimeError::TypeError(
                                "'setPrototypeOf' on proxy: trap returned truish for a non-extensible target whose prototype differs"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Some(Self::proxy_boolean_answer(
                    reflects,
                    answer,
                    receiver,
                    "setPrototypeOf",
                )?))
            }
            "has" => Ok(Some(Value::Bool(
                self.proxy_has(&receiver, &crate::builtins::reflect::to_key(&key), module)?
                    .unwrap_or(false),
            ))),
            "deleteProperty" => Ok(Some(Value::Bool(
                self.proxy_delete(&receiver, &crate::builtins::reflect::to_key(&key), module)?
                    .unwrap_or(true),
            ))),
            "isExtensible" => {
                let answer = self
                    .invoke(&trap, handler, &[target.clone()], module)?
                    .to_boolean();
                // ES 10.5.16: the trap must agree with the target, or
                // `Object.isExtensible` and `preventExtensions` disagree.
                let actual = match &target {
                    Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                    _ => false,
                };
                if answer != actual {
                    return Err(RuntimeError::TypeError(format!(
                        "'isExtensible' on proxy: trap result does not reflect extensibility of proxy target (which is '{}')",
                        actual
                    )));
                }
                Ok(Some(Value::Bool(answer)))
            }
            "preventExtensions" => {
                let answer = self
                    .invoke(&trap, handler, &[target.clone()], module)?
                    .to_boolean();
                // ES 10.5.19: reporting success while the target is still
                // extensible would make the two answers contradict each other.
                if answer {
                    if let Value::Object(obj_ref) = &target {
                        if obj_ref.borrow().is_extensible() {
                            return Err(RuntimeError::TypeError(
                                "'preventExtensions' on proxy: trap returned truish but the proxy target is extensible"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Some(Self::proxy_boolean_answer(
                    reflects,
                    answer,
                    receiver,
                    "preventExtensions",
                )?))
            }
            "getOwnPropertyDescriptor" => {
                let result = self.invoke(&trap, handler, &[target.clone(), key.clone()], module)?;
                let target_desc =
                    Self::proxy_target_descriptor(&target, &crate::builtins::reflect::to_key(&key));
                let extensible = match &target {
                    Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                    _ => false,
                };
                if !result.is_object() {
                    if !result.is_undefined() {
                        return Err(RuntimeError::TypeError(
                            "'getOwnPropertyDescriptor' on proxy: trap returned neither an object nor undefined"
                                .to_string(),
                        ));
                    }
                    // ES 10.5.5: an existing non-configurable property — or any
                    // own property of a non-extensible target — cannot be
                    // reported as absent.
                    if let Some(desc) = &target_desc {
                        if !desc.configurable || !extensible {
                            return Err(RuntimeError::TypeError(
                                "'getOwnPropertyDescriptor' on proxy: trap returned undefined for a property that is non-configurable in the proxy target"
                                    .to_string(),
                            ));
                        }
                    }
                    return Ok(Some(Value::Undefined));
                }
                if let Some(desc) = &target_desc {
                    if !desc.configurable {
                        let reported = self.get_member(
                            &result,
                            &PropertyKey::from_str("configurable"),
                            module,
                        )?;
                        if reported.to_boolean() {
                            return Err(RuntimeError::TypeError(
                                "'getOwnPropertyDescriptor' on proxy: trap reported a non-configurable property as configurable"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Ok(Some(result))
            }
            "ownKeys" => {
                let result = self.invoke(&trap, handler, &[target.clone()], module)?;
                if !result.is_object() {
                    return Err(RuntimeError::TypeError(
                        "'ownKeys' on proxy: trap result must be an object".to_string(),
                    ));
                }
                // `CreateListFromArrayLike` restricted to strings and symbols.
                let items = self.array_like_elements(&result).ok_or_else(|| {
                    RuntimeError::TypeError(
                        "'ownKeys' on proxy: trap result is not array-like".to_string(),
                    )
                })?;
                let mut keys: Vec<PropertyKey> = Vec::new();
                for item in items {
                    match item {
                        Value::String(text) => keys.push(PropertyKey::from_str(&text)),
                        Value::Symbol(sym) => keys.push(PropertyKey::Symbol(sym.id)),
                        _ => {
                            return Err(RuntimeError::TypeError(
                                "'ownKeys' on proxy: trap result contains a key that is neither a string nor a symbol"
                                    .to_string(),
                            ))
                        }
                    }
                }
                let mut seen: Vec<PropertyKey> = Vec::new();
                for key in &keys {
                    if seen.contains(key) {
                        return Err(RuntimeError::TypeError(
                            "'ownKeys' on proxy: trap result contains duplicate entries"
                                .to_string(),
                        ));
                    }
                    seen.push(key.clone());
                }
                let target_keys = Self::proxy_target_keys(&target);
                let extensible = match &target {
                    Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                    _ => false,
                };
                for target_key in &target_keys {
                    // ES 10.5.11: every non-configurable own key has to be
                    // reported, and a non-extensible target's keys all are.
                    let required = match Self::proxy_target_descriptor(&target, target_key) {
                        Some(desc) => !desc.configurable,
                        None => false,
                    } || !extensible;
                    if required && !keys.contains(target_key) {
                        return Err(RuntimeError::TypeError(format!(
                            "'ownKeys' on proxy: trap result did not include '{}'",
                            target_key.display()
                        )));
                    }
                }
                if !extensible {
                    // … and a non-extensible target cannot appear to gain keys.
                    for key in &keys {
                        if !target_keys.contains(key) {
                            return Err(RuntimeError::TypeError(
                                "'ownKeys' on proxy: trap result includes a key the non-extensible target does not have"
                                    .to_string(),
                            ));
                        }
                    }
                }
                // Each caller reports a different slice of the same list:
                // `Object.getOwnPropertyNames` strings, `…Symbols` symbols,
                // `Object.keys` the enumerable strings, `Reflect.ownKeys` all.
                let values: Vec<Value> = keys
                    .iter()
                    .filter(|key| match name {
                        "Object.keys" => {
                            key.as_str().is_some()
                                && Self::proxy_target_descriptor(&target, key)
                                    .map(|desc| desc.enumerable)
                                    .unwrap_or(false)
                        }
                        "Object.getOwnPropertyNames" => key.as_str().is_some(),
                        "Object.getOwnPropertySymbols" => key.as_str().is_none(),
                        _ => true,
                    })
                    .map(Self::key_to_value)
                    .collect();
                Ok(Some(Value::Object(self.heap.alloc(Object::Array(
                    crate::vm::object::ArrayObject::from_vec(values),
                )))))
            }
            _ => {
                // `defineProperty`: the descriptor is normalised *before* the
                // trap sees it (ES 10.5.6 step 5) — the trap gets an object
                // built from the descriptor, not the one the caller passed.
                let descriptor =
                    self.to_property_descriptor(args.get(2).unwrap_or(&Value::Undefined), module)?;
                let answer = self
                    .invoke(
                        &trap,
                        handler,
                        &[target.clone(), key.clone(), descriptor],
                        module,
                    )?
                    .to_boolean();
                if answer {
                    let target_desc = Self::proxy_target_descriptor(
                        &target,
                        &crate::builtins::reflect::to_key(&key),
                    );
                    let extensible = match &target {
                        Value::Object(obj_ref) => obj_ref.borrow().is_extensible(),
                        _ => false,
                    };
                    // ES 10.5.6: a non-configurable property cannot be
                    // redefined, and a non-extensible target gains no property.
                    let forbidden = match &target_desc {
                        Some(desc) => !desc.configurable,
                        None => !extensible,
                    };
                    if forbidden {
                        return Err(RuntimeError::TypeError(
                            "'defineProperty' on proxy: trap returned truish for a non-configurable property in the proxy target"
                                .to_string(),
                        ));
                    }
                }
                Ok(Some(Self::proxy_boolean_answer(
                    reflects,
                    answer,
                    receiver,
                    "defineProperty",
                )?))
            }
        }
    }

    /// `Object.*` and `Reflect.*` disagree only about *failure*: `Reflect`
    /// answers `false`, `Object` throws.
    pub(crate) fn proxy_boolean_answer(
        reflects: bool,
        answer: bool,
        receiver: Value,
        trap_name: &str,
    ) -> Result<Value, RuntimeError> {
        if reflects || answer {
            return Ok(if reflects {
                Value::Bool(answer)
            } else {
                receiver
            });
        }
        Err(RuntimeError::TypeError(format!(
            "'{trap_name}' on proxy: trap returned falsish"
        )))
    }

    /// Whether a reported prototype is the one the target actually has.
    pub(crate) fn same_prototype(actual: &Option<Gc<Object>>, reported: &Value) -> bool {
        match (actual, reported) {
            (None, Value::Null) => true,
            (Some(a), Value::Object(b)) => Gc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// The `revoke` function of `Proxy.revocable` (ES 28.2.2.2): a native whose
    /// synthetic name carries the id of the proxy it revokes.
    pub(crate) fn make_revoke(&mut self, proxy: Value) -> Value {
        let id = self.next_revoke_id;
        self.next_revoke_id += 1;
        self.revocable_proxies.insert(id, proxy);
        let mut revoke =
            crate::vm::object::NativeFunctionObject::new(&format!("{REVOKE_PREFIX}{id}"));
        // A revocation function is *anonymous* (ES 28.2.2.1.1): its `name` is
        // the empty string, not the synthetic name the VM dispatches on.
        let _ = revoke.define_property(
            PropertyKey::from_str("name"),
            PropertyDescriptor {
                value: Value::string(""),
                writable: false,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
        Value::Object(self.heap.alloc(Object::NativeFunction(revoke)))
    }
}
