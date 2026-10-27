//! 内建的**按名字**分派：`call_native_by_name` 与各个 `try_<x>_method` 快速路径。
//!
//! 它们住在 VM 里而不是 `builtins/` 里，是因为这些路径要么要回调 JS
//! （`Array.prototype.map` 的回调、`String.prototype.replace` 的函数替换器），
//! 要么要造只有 VM 能造的对象（迭代器注册在 VM 的迭代器表里，`next()`
//! 靠它找状态）。
//!
//! 按名字字符串 `match` 本身是一处已知的设计债（ARCHITECTURE §5 第 3 条）：
//! 由它派生出 arity 表、`__proto__xxx` 前缀约定、`vm_handled_static` 旁路
//! 名单这一整套约定。搬到这里是为了让它们都在一处，而不是认可这个形状。

use std::cell::RefCell;
use std::rc::Rc;

use crate::bytecode::Module;
use crate::vm::object::{JSObject, PromiseState};
use crate::vm::{
    BOUND_PREFIX, ObjectKind, PropertyDescriptor, PropertyKey, REVOKE_PREFIX,
    RuntimeError, Value, VM,
};

impl VM {
    /// Dispatch a built-in by name, honouring prototype methods.
    ///
    /// `set_prototype_method` registers prototype methods under the synthetic
    /// name `__proto_method__<name>`; those need the receiver as their first
    /// argument, whereas real natives (`Object.keys`, `isNaN`, …) do not.
    pub(crate) fn call_native_by_name(
        &mut self,
        name: &str,
        this: Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // Box bare `Value::Function(id)` arguments (and the receiver) into their
        // memoized function objects before handing them to the builtin layer.
        // The builtin layer matches on `Value::Object` — `Object.keys(fn)`,
        // `Object.getOwnPropertyDescriptor(fn, "length")` and friends all used
        // to report "first argument must be an object" for a plain function
        // reference, and `ToObject` passes the bare form through unchanged.
        let boxed_args: Vec<Value> = args.iter().map(|a| self.as_object_value(a)).collect();
        let args: &[Value] = &boxed_args;
        let this: Value = self.as_object_value(&this);

        // The `resolve` / `reject` handed to a `Promise` executor. They carry
        // their target's id in the name (see `promise_construct`), because a
        // native function here is identified by name alone.
        if let Some(id) = name.strip_prefix("__promise_resolve__") {
            if let Ok(id) = id.parse::<u64>() {
                if let Some(promise) = self.state.promises.get(&id).cloned() {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    // A resolved promise fulfils with the value (thenable
                    // adoption is not modelled: there is no way to run a
                    // foreign `then` and observe it synchronously).
                    self.promise_settle(&promise, PromiseState::Fulfilled, value);
                }
            }
            return Ok(Value::Undefined);
        }
        if let Some(id) = name.strip_prefix("__promise_reject__") {
            if let Ok(id) = id.parse::<u64>() {
                if let Some(promise) = self.state.promises.get(&id).cloned() {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    self.promise_settle(&promise, PromiseState::Rejected, value);
                }
            }
            return Ok(Value::Undefined);
        }
        // Per-element handlers of `Promise.all` / `race` / `allSettled` / `any`.
        if name.starts_with("__promise_agg_ok__") || name.starts_with("__promise_agg_err__") {
            if let Some(result) = self.dispatch_aggregation(name, args) {
                return result;
            }
        }
        // `Promise.prototype.finally` callbacks (name carries a registry id).
        if let Some(rest) = name.strip_prefix("__promise_finally__") {
            // `"<id>_f"` / `"<id>_r"`: which reaction (fulfilled or rejected)
            // decides what "pass through unchanged" means.
            if let Some((id, tag)) = rest.split_once('_') {
                if let Ok(id) = id.parse::<u64>() {
                    let callback = self.state.finally_handlers.get(&id).cloned();
                    let argument = args.first().cloned().unwrap_or(Value::Undefined);
                    if let Some(cb) = callback {
                        // A throwing callback rejects the derived promise.
                        self.invoke(&cb, Value::Undefined, &[], module)?;
                        if tag == "r" {
                            return Err(RuntimeError::Thrown(argument));
                        }
                        return Ok(argument);
                    }
                }
            }
            return Ok(Value::Undefined);
        }
        if name == crate::builtins::OBJECT_TO_STRING_NATIVE {
            return self.object_prototype_to_string(&this, args, module);
        }
        // `Symbol.prototype.description` getter: unwrap the receiver.
        if name == crate::builtins::SYMBOL_DESCRIPTION_NATIVE {
            return crate::builtins::symbol_description(&this);
        }
        // `Map.prototype.size` getter (ES 23.1.3.9).
        if name == crate::builtins::MAP_SIZE_NATIVE {
            return crate::builtins::map_size(&this);
        }
        // `Set.prototype.size` getter (ES 23.2.3.9).
        if name == crate::builtins::SET_SIZE_NATIVE {
            return crate::builtins::set_size(&this);
        }
        // TypedArray / ArrayBuffer getters (ES 23.2.3.1-4, 25.1.5.1). They are
        // accessors on the prototype, so the receiver arrives as `this`.
        if name == crate::builtins::typedarray::TA_LENGTH_NATIVE {
            return crate::builtins::typedarray::ta_length(&this);
        }
        if name == crate::builtins::typedarray::TA_BYTE_LENGTH_NATIVE {
            return crate::builtins::typedarray::ta_byte_length(&this);
        }
        if name == crate::builtins::typedarray::TA_BYTE_OFFSET_NATIVE {
            return crate::builtins::typedarray::ta_byte_offset(&this);
        }
        if name == crate::builtins::typedarray::TA_BUFFER_NATIVE {
            return crate::builtins::typedarray::ta_buffer(&this);
        }
        if name == crate::builtins::typedarray::AB_BYTE_LENGTH_NATIVE {
            return crate::builtins::typedarray::ab_byte_length(&this);
        }
        if name == crate::builtins::typedarray::DV_BUFFER_NATIVE {
            return crate::builtins::typedarray::dv_buffer(&this);
        }
        if name == crate::builtins::typedarray::DV_BYTE_LENGTH_NATIVE {
            return crate::builtins::typedarray::dv_byte_length(&this);
        }
        if name == crate::builtins::typedarray::DV_BYTE_OFFSET_NATIVE {
            return crate::builtins::typedarray::dv_byte_offset(&this);
        }
        // `get Map[Symbol.species]` / `get Set[Symbol.species]` (23.1.2.2,
        // 23.2.2.2) and `get Promise[Symbol.species]` (27.2.2.3) all answer
        // their receiver.
        if name == crate::builtins::MAP_SPECIES_NATIVE
            || name == crate::builtins::SET_SPECIES_NATIVE
            || name == crate::builtins::PROMISE_SPECIES_NATIVE
            // `%TypedArray%[Symbol.species]` (23.2.2.2) 同样是"返回接收者"。
            || name == crate::builtins::typedarray::TA_SPECIES_NATIVE
        {
            return Ok(this);
        }
        // `Map`/`Set` prototype methods are dispatched here, not in the builtin
        // layer: they either call a user callback or mint an iterator object,
        // and their dispatch name carries which prototype they came from.
        if let Some(method) = name.strip_prefix(crate::builtins::MAP_METHOD_PREFIX) {
            return self.map_method(&this, method, args, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::SET_METHOD_PREFIX) {
            return self.set_method(&this, method, args, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::WEAKMAP_METHOD_PREFIX) {
            return self.weakmap_method(&this, method, args, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::WEAKSET_METHOD_PREFIX) {
            return self.weakset_method(&this, method, args);
        }
        // Generator methods: `gen.next(v)` resumes the body, `gen.return(v)` /
        // `gen.throw(e)` complete it, and `gen[Symbol.iterator]()` hands the
        // generator itself back.
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_NEXT_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let send = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_next(id, send, module);
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_RETURN_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_abrupt(id, Ok(value), module);
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_THROW_PREFIX) {
            let id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            let reason = args.first().cloned().unwrap_or(Value::Undefined);
            return self.generator_abrupt(id, Err(RuntimeError::Thrown(reason)), module);
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::GENERATOR_ITERATOR_PREFIX) {
            let _id: u64 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed generator id".to_string())
            })?;
            return Ok(this);
        }
        // `get Array[Symbol.species]` returns its receiver (ES 22.1.2.5).
        if name == crate::builtins::ARRAY_SPECIES_NATIVE {
            return Ok(this);
        }
        // `Symbol.iterator` factory: returns an internal iterator over `this`.
        if name == crate::vm::iterator::ITERATOR_NATIVE_NAME {
            return self.make_iterator(this, module);
        }
        // `get %TypedArray%.prototype[@@toStringTag]`：返回接收者自己的
        // TypedArray 名（23.2.3.32）。
        if name == crate::builtins::typedarray::TA_TO_STRING_TAG_NATIVE {
            return crate::builtins::typedarray::ta_to_string_tag(&this);
        }
        // `String(value)` is ToString(value): an object converts through
        // ToPrimitive (hint "string"), which can run user code, so it has to
        // happen here rather than in the builtin layer.
        if name == "String" && args.len() == 1 {
            let primitive = self.to_primitive(&args[0], "string", module)?;
            return Ok(Value::string(&primitive.to_js_string()));
        }
        // Per-iterator `next()` / `return()` methods.
        if let Some(id) = name.strip_prefix(crate::vm::iterator::ITERATOR_NEXT_PREFIX) {
            let id: u64 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed iterator id".to_string()))?;
            let iter_val = self
                .iterator_registry
                .get(&id)
                .cloned()
                .ok_or_else(|| RuntimeError::TypeError("iterator is no longer alive".to_string()))?;
            // `next(v)` forwards `v` to a JS iterator: that is how `yield*` gets
            // the value the outer `next(v)` supplied into the delegate.
            let send = args.first().cloned();
            let (item, done) = self.iterator_next(iter_val, send, module)?;
            return Ok(Self::iterator_result(&mut self.heap, item, done));
        }
        if let Some(id) = name.strip_prefix(crate::vm::iterator::ITERATOR_RETURN_PREFIX) {
            let id: u64 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed iterator id".to_string()))?;
            if let Some(iter_val) = self.iterator_registry.get(&id).cloned() {
                self.iterator_close(iter_val, args.first(), false, module)?;
            }
            // `return()` is an iterator method, so it answers with an iterator
            // result — `yield*` checks that the value is an object
            // (ES 14.4.14 step 5.c.vi).
            return Ok(Self::iterator_result(&mut self.heap, Value::Undefined, true));
        }
        // `Function.prototype` registers `call` / `apply` / `bind` under their
        // plain names, so handle them before the static-method lookup.
        if matches!(name, "call" | "apply") && this.is_callable() {
            self.check_class_ctor(&this)?;
            let call_args = self.call_or_apply_args(name, args)?;
            let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
            return self.invoke(&this, this_arg, &call_args, module);
        }
        if name == "bind" && this.is_callable() {
            let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
            let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
            return Ok(self.make_bound(this, this_arg, bound_args));
        }
        if let Some(id) = name.strip_prefix(BOUND_PREFIX) {
            let id: u32 = id
                .parse()
                .map_err(|_| RuntimeError::InternalError("malformed bound function".to_string()))?;
            let Some((target, this_arg, mut all)) = self
                .bound_functions
                .get(&id)
                .map(|b| (b.target.clone(), b.this_arg.clone(), b.bound_args.clone()))
            else {
                return Err(RuntimeError::InternalError(
                    "bound function is no longer available".to_string(),
                ));
            };
            all.extend(args.iter().cloned());
            return self.invoke(&target, this_arg, &all, module);
        }
        if let Some(method) = name.strip_prefix(crate::builtins::PROTO_METHOD_PREFIX) {
            // `f.call` / `f.apply` / `f.bind` are the same operations as the
            // `CallMethod` opcode path handles; they must work identically when
            // the method value is fetched first (`Function.prototype.call.bind(…)`).
            if method == "bind" && this.is_callable() {
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
                return Ok(self.make_bound(this, this_arg, bound_args));
            }
            if (method == "call" || method == "apply") && this.is_callable() {
                // `C.call(...)` must fail for a class constructor, same as `C()`.
                self.check_class_ctor(&this)?;
                let call_args = self.call_or_apply_args(method, args)?;
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                return self.invoke(&this, this_arg, &call_args, module);
            }
            if let Some(result) = self.try_array_callback_method(&this, method, args, module)? {
                return Ok(result);
            }
            // 读族（`join` / `slice` / `includes` / `at` / `concat` / `reverse`）：
            // 同样要在内建层之前截住，否则访问器元素读成空位、类数组接收者走不通。
            if let Some(result) = self.try_array_read_method(&this, method, args, module)? {
                return Ok(result);
            }
            // TypedArray 的回调一族：上面那条对 TypedArray 接收者直接让出（那里
            // 产出的是 Array），由这里接手 —— `map` / `filter` 的结果必须是
            // **同类视图**，不是普通数组。
            if let Some(result) = self.try_typedarray_callback_method(&this, method, args, module)? {
                return Ok(result);
            }
            // TypedArray 的迭代器三件套（ES 23.2.3.x）：`keys` / `values` /
            // `entries` 返回迭代器，只有 VM 能构造。必须排在数组那条路之后、
            // 且只认 TypedArray 接收者（`Array.prototype` 有同名方法）。
            if matches!(method, "keys" | "values" | "entries")
                && crate::builtins::typedarray::is_typedarray_receiver(&this)
            {
                return self.make_typed_array_iterator(&this, method);
            }
            // `Promise.prototype.then` has to *call* the handlers, so it is
            // dispatched here rather than in the builtin table.
            if let Some(result) = self.try_promise_method(&this, method, args, module)? {
                return Ok(result);
            }
            // `String.prototype.replace(re, fn)` — the replacer is user code.
            if let Some(result) = self.try_string_callback_method(&this, method, args, module)? {
                return Ok(result);
            }
            if let Some(result) = self.try_match_all_method(&this, method, args, module)? {
                return Ok(result);
            }
            // `entries` / `keys` / `values` return a *fresh* iterator over a
            // snapshot; only the VM can mint iterator objects (they live in the
            // iterator registry so `next()` can find its state).
            if matches!(method, "entries" | "keys" | "values") {
                // ES 23.1.3.28-30 start with `Let O be ? ToObject(this value)`:
                // `values.call(null)` is a TypeError, not an empty iterator.
                crate::builtins::require_object_coercible(&this)?;
                // *Then* `ToLength(Get(O, "length"))`. A receiver without a
                // usable `length` iterates as empty (`values.call({})`).
                {
                    let items = self.array_like_elements(&this).unwrap_or_default();
                    let snapshot: Vec<Value> = match method {
                        "entries" => items
                            .iter()
                            .enumerate()
                            .map(|(i, value)| {
                                Value::Object(self.heap.alloc(crate::vm::gc::Object::Array(
                                    crate::vm::object::ArrayObject::from_vec(vec![
                                        Value::Number(i as f64),
                                        value.clone(),
                                    ]),
                                )))
                            })
                            .collect(),
                        "keys" => (0..items.len())
                            .map(|i| Value::Number(i as f64))
                            .collect(),
                        _ => items,
                    };
                    let snapshot_val = Value::Object(self.heap.alloc(
                        crate::vm::gc::Object::Array(
                            crate::vm::object::ArrayObject::from_vec(snapshot),
                        ),
                    ));
                    return self.make_iterator(snapshot_val, module);
                }
            }
            return crate::builtins::call_prototype_method(&mut self.heap, &this, method, args);
        }
        // Static methods are stored under their qualified name ("Array.from").
        if name.contains('.') {
            // `Reflect.*` reads its key argument through `ToPropertyKey`, which
            // runs user code (`{ toString() { throw … } }`) — the builtin layer
            // cannot do that, so the key is converted here and handed on.
            let converted_args;
            let args: &[Value] = if matches!(
                name,
                "Reflect.get"
                    | "Reflect.set"
                    | "Reflect.has"
                    | "Reflect.deleteProperty"
                    | "Reflect.defineProperty"
                    | "Reflect.getOwnPropertyDescriptor"
            ) {
                converted_args = self.reflect_key_args(args, module)?;
                &converted_args
            } else {
                args
            };
            // A proxy receiver turns these `Object.*` / `Reflect.*` operations
            // into trap calls (ES 10.5.5 – 10.5.19); anything else falls
            // through to the ordinary implementation.
            if let Some(result) = self.proxy_static_trap(name, args, module)? {
                return Ok(result);
            }
            // `Object.assign` reads through real `[[Get]]` and writes through
            // real `[[Set]]` (own or prototype accessors may run), which only
            // the VM can do.
            if name == "Object.assign" {
                return self.object_assign(args, module);
            }
            // `String.raw(template, ...subs)`（ES 22.1.2.3）：读 `template.raw`
            // 用的是真 [[Get]]（可能是 getter），且每个替换值要 ToString（可能触发
            // `toString`） —— builtins 层拿不到 VM，故在这里做。
            if name == "String.raw" {
                return self.string_raw(args, module);
            }
            // `%TypedArray%.from` / `%TypedArray%.of`：迭代、可选回调、再
            // `new C(len)` —— 三样都只有 VM 能做。
            if name == "TypedArray.from" {
                let source = args.first().cloned().unwrap_or(Value::Undefined);
                let mapfn = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
                return self.typed_array_from(&this, &source, &mapfn, &this_arg, module);
            }
            if name == "TypedArray.of" {
                return self.typed_array_of(&this, args, module);
            }
            // `Map.groupBy(items, callbackfn)`: iterates and calls back.
            if name == "Map.groupBy" {
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                return self.map_group_by(&items, &callback, module);
            }
            // The descriptor arguments are read through [[Get]] first, which
            // `Promise.resolve` / `Promise.reject` build promises, which only the
            // VM can register.
            if let Some(result) = self.promise_static(name, args, module) {
                return Ok(result);
            }
            // only the VM can do (their fields may be accessors).
            if name == "Object.defineProperty" && args.len() >= 3 {
                let descriptor = self.to_property_descriptor(&args[2], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), args[1].clone(), descriptor],
                    &mut self.heap,
                );
            }
            if name == "Object.defineProperties" && args.len() >= 2 {
                let props = self.to_property_descriptors(&args[1], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), props],
                    &mut self.heap,
                );
            }
            // `JSON.stringify` runs `toJSON`/`replacer` and reads through
            // `[[Get]]`; `JSON.parse`'s reviver is user code.
            if name == "JSON.stringify" {
                return self.json_stringify_full(args, module);
            }
            if name == "JSON.parse" {
                return self.json_parse_full(args, module);
            }
            if name == "Object.create" && args.len() >= 2 && !args[1].is_undefined() {
                let props = self.to_property_descriptors(&args[1], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), props],
                    &mut self.heap,
                );
            }
            // `Reflect.get` / `set` / `apply` / `construct` / `defineProperty`:
            // the internal methods that can run user code.
            if let Some(result) = self.reflect_static(name, args, module)? {
                return Ok(result);
            }
            // `Proxy.revocable(t, h)` answers `{ proxy, revoke }`, and `revoke`
            // has to remember *which* proxy it revokes — a native's only state
            // is its name, so the VM mints it (`make_revoke`).
            if name == "Proxy.revocable" {
                // Same boxing as `new Proxy` above: `f` is an object.
                let target =
                    self.as_object_value(&args.first().cloned().unwrap_or(Value::Undefined));
                let handler =
                    self.as_object_value(&args.get(1).cloned().unwrap_or(Value::Undefined));
                let proxy = crate::builtins::proxy::proxy_construct(&mut self.heap, target, handler)?;
                let revoke = self.make_revoke(proxy.clone());
                let mut result = crate::vm::object::OrdinaryObject::new();
                result.set_prototype(Some(self.builtins.object_prototype));
                for (field, value) in [("proxy", proxy), ("revoke", revoke)] {
                    let key = PropertyKey::intern(&mut self.heap, field);
                    let _ = result.define_property(&mut self.heap, key,
                        PropertyDescriptor::data_descriptor(value),
                    );
                }
                return Ok(Value::Object(
                    self.heap.alloc(crate::vm::gc::Object::Ordinary(result)),
                ));
            }
            return crate::builtins::call_static_method(name, args, &mut self.heap);
        }
        // The `revoke` function of `Proxy.revocable`: its work is one flag on
        // the proxy it names. Everything after revocation throws.
        if let Some(id) = name.strip_prefix(REVOKE_PREFIX) {
            let id: u32 = id.parse().map_err(|_| {
                RuntimeError::InternalError("malformed revoke function".to_string())
            })?;
            if let Some(Value::Object(obj_ref)) = self.revocable_proxies.get(&id) {
                if let Some(proxy) = obj_ref
                    .borrow_mut().as_proxy_mut()
                {
                    proxy.revoked = true;
                }
            }
            return Ok(Value::Undefined);
        }
        // `Proxy` has no `[[Call]]` at all (ES 28.2.2): it can only be
        // constructed.
        if name == "Proxy" {
            return Err(RuntimeError::TypeError(
                "Constructor Proxy requires 'new'".to_string(),
            ));
        }
        crate::builtins::call_native(name, args, &mut self.heap)
    }

    /// `String.prototype.matchAll(re)` / `RegExp.prototype[Symbol.matchAll](s)`.
    ///
    /// Both answer an iterator of match arrays, and only the VM can mint
    /// iterator objects (they live in the iterator registry, which is how
    /// `next()` finds its state) — hence a hook rather than a builtin.
    pub(crate) fn try_match_all_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if method != "matchAll" && method != "Symbol.matchAll" {
            return Ok(None);
        }
        crate::builtins::require_object_coercible(receiver)?;
        let matches = crate::builtins::match_all_matches(
            &mut self.heap,
            receiver,
            args,
            Some(Value::Object(self.builtins.regexp_prototype)),
        )?;
        let snapshot_val = Value::Object(self.heap.alloc(
            crate::vm::gc::Object::Array(crate::vm::object::ArrayObject::from_vec(matches)),
        ));
        Ok(Some(self.make_iterator(snapshot_val, module)?))
    }

    /// Species-aware wrapper for `slice` / `splice` / `concat`.
    ///
    /// Returns `Ok(None)` when the method is not one of them, when the receiver
    /// is not an Array, or when there is no species override — the ordinary
    /// builtin dispatch then runs unchanged. Otherwise the builtin computes the
    /// plain result (its side effects on the receiver are exactly as before)
    /// and the elements are copied into the species-constructed target, where a
    /// blocked write is abrupt (`CreateDataPropertyOrThrow`).
    pub(crate) fn try_array_species_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if !matches!(method, "slice" | "splice" | "concat") {
            return Ok(None);
        }
        // `ArraySpeciesCreate(O, 0)`: all three start from an empty result and
        // grow it index by index.
        let Some(target) = self.array_species_create(receiver, 0, module)? else {
            return Ok(None);
        };
        let plain = crate::builtins::call_prototype_method(&mut self.heap, receiver, method, args)?;
        // Snapshot the present indices first: writing into `target` can run
        // user code (accessors, proxies once they land), and `plain` is a
        // RefCell that must not stay borrowed across that call.
        let elements: Vec<(usize, Value)> = match &plain {
            Value::Object(result_ref) => {
                let borrowed = result_ref.borrow();
                let len = borrowed
                    .property_get(&PropertyKey::well_known("length").expect("well-known name"))
                    .map(|d| d.value.to_number() as usize)
                    .unwrap_or(0);
                (0..len)
                    .filter_map(|i| {
                        borrowed
                            .property_get(&PropertyKey::element(&mut self.heap, i as u64))
                            .map(|d| (i, d.value))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        for (i, value) in elements {
            let key = PropertyKey::element(&mut self.heap, i as u64);
            self.set_member(&target, key, value, module)?;
        }
        Ok(Some(target))
    }

    /// `String.prototype.replace(searchValue, replacer)` when the replacer is a
    /// **function** — the everyday `s.replace(/re/g, fn)` shape.
    ///
    /// The builtin layer cannot call user code, so the whole substitution runs
    /// here: find the matches, call the replacer with
    /// `(match, group…, offset, input)`, and splice its `ToString` in.
    pub(crate) fn try_string_callback_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if !matches!(method, "replace" | "replaceAll") {
            return Ok(None);
        }
        let Value::String(text) = this else {
            return Ok(None);
        };
        let replacer = match args.get(1) {
            // A bare `Value::Function(id)` is callable too; `is_callable()` only
            // answers for the boxed form, and this interception runs before the
            // boxing that `call_native_by_name` does.
            Some(v) if v.is_callable() || matches!(v, Value::Function(_)) => v.clone(),
            _ => return Ok(None),
        };
        let replacer = match replacer {
            Value::Function(_) => self.as_object_value(&replacer),
            other => other,
        };
        let search = args.first().cloned().unwrap_or(Value::Undefined);
        let mut global = method == "replaceAll";

        // Collect the matches: a RegExp contributes its captures (all of them
        // when `g`), anything else is a plain substring search.
        let mut matches: Vec<(usize, usize, Vec<Option<String>>)> = Vec::new();
        let mut regex_flags = String::new();
        let compiled = match &search {
            Value::Object(obj_ref) => {
                let kind_is_regexp =
                    obj_ref.borrow().kind() == crate::vm::property::ObjectKind::RegExp;
                if kind_is_regexp {
                    let borrowed = obj_ref.borrow();
                    let re = borrowed.as_regexp()
                        .expect("ObjectKind::RegExp implies RegExpObject");
                    regex_flags = re.flags.clone();
                    global = global || regex_flags.contains('g');
                    re.compiled.clone()
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(matcher) = compiled {
            for captures in matcher.captures_iter(text.as_str()) {
                let whole = match captures.get(0) {
                    Some(m) => m,
                    None => continue,
                };
                let groups = (1..captures.len())
                    .map(|i| captures.get(i).map(|m| m.as_str().to_string()))
                    .collect();
                matches.push((whole.start(), whole.end(), groups));
                if !global {
                    break;
                }
            }
        } else {
            let needle = search.to_js_string();
            if !needle.is_empty() {
                if global {
                    let mut from = 0usize;
                    while let Some(at) = text.as_str()[from..].find(needle.as_str()) {
                        let start = from + at;
                        matches.push((start, start + needle.len(), Vec::new()));
                        from = start + needle.len().max(1);
                    }
                } else if let Some(at) = text.as_str().find(needle.as_str()) {
                    matches.push((at, at + needle.len(), Vec::new()));
                }
            } else {
                // An empty pattern matches at both ends (ES 22.1.3.17 step 8).
                matches.push((0, 0, Vec::new()));
            }
        }

        let mut out = String::new();
        let mut last = 0usize;
        for (start, end, groups) in matches {
            out.push_str(&text.as_str()[last..start]);
            let mut call_args: Vec<Value> = vec![Value::string(&text.as_str()[start..end])];
            for group in groups {
                call_args.push(match group {
                    Some(g) => Value::string(&g),
                    None => Value::Undefined,
                });
            }
            call_args.push(Value::Number(start as f64));
            call_args.push(Value::string(text.as_str()));
            let replacement = self
                .invoke(&replacer, Value::Undefined, &call_args, module)?
                .to_js_string();
            out.push_str(&replacement);
            last = end;
        }
        out.push_str(&text.as_str()[last..]);
        Ok(Some(Value::string(&out)))
    }

    /// `then` / `catch` / `finally`, when the receiver is a promise.
    pub(crate) fn try_promise_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        _module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        if !matches!(method, "then" | "catch" | "finally") {
            return Ok(None);
        }
        let Some(promise) = self.as_promise(this) else {
            // Not a promise: let the ordinary prototype dispatch answer (it is
            // what reports "not a function" for a missing method).
            return Ok(None);
        };
        // A bare `Value::Function(id)` is callable but `is_callable()` only
        // answers for the boxed form, so box first — that is exactly what
        // `call_native_by_name` does before handing arguments to the builtin
        // layer, and this interception runs *before* that.
        let mut callable = |v: Option<&Value>| -> Option<Value> {
            v.filter(|v| v.is_callable() || matches!(v, Value::Function(_)))
                .map(|v| {
                    let mut boxed = v.clone();
                    if matches!(boxed, Value::Function(_)) {
                        boxed = self.as_object_value(&boxed);
                    }
                    boxed
                })
        };
        match method {
            "then" => {
                let on_fulfilled = callable(args.first());
                let on_rejected = callable(args.get(1));
                Ok(Some(self.promise_then(&promise, on_fulfilled, on_rejected)))
            }
            "catch" => {
                let on_rejected = callable(args.first());
                Ok(Some(self.promise_then(&promise, None, on_rejected)))
            }
            _ => {
                // `finally(cb)` (ES 27.2.5.3): returns a *fresh* promise that
                // settles like the receiver, except that a throwing `cb` rejects
                // it. The two handlers are natives carrying a registry id; the
                // callback itself runs when the reaction is drained.
                let callback = callable(args.first());
                match callback {
                    None => Ok(Some(self.promise_then(&promise, None, None))),
                    Some(cb) => {
                        let id = self.state.next_finally_id;
                        self.state.next_finally_id += 1;
                        self.state.finally_handlers.insert(id, cb);
                        let fulfilled = Self::native_fn(&mut self.heap, &format!("__promise_finally__{id}_f"));
                        let rejected = Self::native_fn(&mut self.heap, &format!("__promise_finally__{id}_r"));
                        Ok(Some(self.promise_then(
                            &promise,
                            Some(fulfilled),
                            Some(rejected),
                        )))
                    }
                }
            }
        }
    }

    /// `String.raw(template, ...substitutions)`（ES 22.1.2.3）。
    ///
    /// 结果是 `template.raw` 的各段依次与替换值交错拼接：
    /// `raw[0] + sub0 + raw[1] + sub1 + …`。注意用的是 **raw**（未转义的原始串），
    /// 这正是它存在的意义 —— `` String.raw`\n` `` 得到字面的反斜杠 n。
    fn string_raw(&mut self, args: &[Value], module: &Module) -> Result<Value, RuntimeError> {
        let template = args.first().cloned().unwrap_or(Value::Undefined);
        // 第一步是 ToObject：非对象（含 undefined / null）是 TypeError。
        if !matches!(template, Value::Object(_)) {
            return Err(RuntimeError::TypeError(
                "String.raw: template must be an object".to_string(),
            ));
        }
        // `Get(template, "raw")` —— 走真 [[Get]]，getter 可以抛。
        let raw = self.get_member(&template, &PropertyKey::well_known("raw").expect("well-known name"), module)?;
        let len = self
            .array_like_elements(&raw)
            .map(|v| v.len())
            .unwrap_or(0);
        let mut out = String::new();
        for i in 0..len {
            let key = PropertyKey::element(&mut self.heap, i as u64);
            let chunk = self.get_member(&raw, &key, module)?;
            // 每个替换值都 ToString（`undefined` / `null` 走 ToString 而非跳过 ——
            // `String.raw` 不做 `join` 那样的空串替换）。
            out.push_str(&chunk.to_js_string());
            if i + 1 < args.len() {
                out.push_str(&args[i + 1].to_js_string());
            }
        }
        Ok(Value::string(&out))
    }

    /// `%TypedArray%.prototype` 的**回调**一族（ES 23.2.3.x）。
    ///
    /// [`Self::try_array_callback_method`] 对 TypedArray 接收者直接让出（那里产出
    /// 的是 Array，且 `indexOf` / `lastIndexOf` 用的是数组的相等判定），故由本函数
    /// 接手：`map` / `filter` 的结果必须是**同类视图**，不是普通数组。
    ///
    /// 建结果的构造器走 `@@species`（`TypedArraySpeciesCreate`），所以子类化视图
    /// 也能拿到自己类型的新实例。
    pub(crate) fn try_typedarray_callback_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        const TA_CALLBACK_METHODS: &[&str] = &[
            "map",
            "filter",
            "forEach",
            "some",
            "every",
            "find",
            "findIndex",
            "reduce",
            "reduceRight",
            // ES2023 的一族
            "findLast",
            "findLastIndex",
            "with",
            "toReversed",
            "toSorted",
            // `sort` 原地排序（比较器可选），行为上更接近这一族
            "sort",
        ];
        if !TA_CALLBACK_METHODS.contains(&method) {
            return Ok(None);
        }
        if !crate::builtins::typedarray::is_typedarray_receiver(receiver) {
            return Ok(None);
        }
        // 先把元素读成一份本地快照。视图元素恒为数字，但回调可以返回任意值。
        let (len, elements) = {
            let Value::Object(o) = receiver else {
                return Ok(None);
            };
            let mut b = o.borrow_mut();
            let Some(ta) = b.as_typed_array_mut() else {
                return Ok(None);
            };
            let len = ta.length;
            let mut els = Vec::with_capacity(len);
            for i in 0..len {
                els.push(ta.get_element(i));
            }
            (len, els)
        };
        // 只有这一族的首参是**回调**。`with` 的首参是下标、`toReversed` 没有参数、
        // `toSorted` 的比较器是可选的 —— 它们不能被这条检查拦住。
        let needs_callback = matches!(
            method,
            "map"
                | "filter"
                | "forEach"
                | "some"
                | "every"
                | "find"
                | "findIndex"
                | "reduce"
                | "reduceRight"
                | "findLast"
                | "findLastIndex"
        );
        let callback = args.first().cloned().unwrap_or(Value::Undefined);
        if needs_callback && !callback.is_callable() {
            return Err(RuntimeError::TypeError(format!(
                "TypedArray.prototype.{method} callback is not a function"
            )));
        }
        // 入根：回调期间 `elements` / 中间结果只活在 Rust 局部（收集器看不见机器栈）。
        let mut acc = self.roots();
        for e in &elements {
            acc.push(e);
        }
        acc.push(&callback);
        let _root_recv = self.root(receiver);

        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        match method {
            "forEach" => {
                for (i, e) in elements.iter().enumerate() {
                    self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                }
                Ok(Some(Value::Undefined))
            }
            // `some` / `every` 只差"命中即返回哪一个布尔"。
            "some" | "every" => {
                let hit = method == "some";
                for (i, e) in elements.iter().enumerate() {
                    let r = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    if r.to_boolean() == hit {
                        return Ok(Some(Value::Bool(hit)));
                    }
                }
                Ok(Some(Value::Bool(!hit)))
            }
            "find" => {
                for (i, e) in elements.iter().enumerate() {
                    let r = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    if r.to_boolean() {
                        return Ok(Some(e.clone()));
                    }
                }
                Ok(Some(Value::Undefined))
            }
            "findIndex" => {
                for (i, e) in elements.iter().enumerate() {
                    let r = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    if r.to_boolean() {
                        return Ok(Some(Value::Number(i as f64)));
                    }
                }
                Ok(Some(Value::Number(-1.0)))
            }
            "map" => {
                let mut out = Vec::with_capacity(len);
                for (i, e) in elements.iter().enumerate() {
                    let v = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    acc.push(&v);
                    out.push(v);
                }
                let result = self.typed_array_species_create(receiver, len, module)?;
                let _r = self.root(&result);
                self.write_typed_array_elements(&result, &out);
                Ok(Some(result))
            }
            "filter" => {
                let mut kept = Vec::new();
                for (i, e) in elements.iter().enumerate() {
                    let r = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    if r.to_boolean() {
                        kept.push(e.clone());
                    }
                }
                let result = self.typed_array_species_create(receiver, kept.len(), module)?;
                let _r = self.root(&result);
                self.write_typed_array_elements(&result, &kept);
                Ok(Some(result))
            }
            // `findLast` / `findLastIndex`：从**末尾**往前找第一个命中的。
            "findLast" => {
                for (i, e) in elements.iter().enumerate().rev() {
                    let r = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    if r.to_boolean() {
                        return Ok(Some(e.clone()));
                    }
                }
                Ok(Some(Value::Undefined))
            }
            "findLastIndex" => {
                for (i, e) in elements.iter().enumerate().rev() {
                    let r = self.invoke(
                        &callback,
                        this_arg.clone(),
                        &[e.clone(), Value::Number(i as f64), receiver.clone()],
                        module,
                    )?;
                    if r.to_boolean() {
                        return Ok(Some(Value::Number(i as f64)));
                    }
                }
                Ok(Some(Value::Number(-1.0)))
            }
            // `with(index, value)`：返回**新**视图，指定下标换成新值（不改动原视图）；
            // 下标越界是 RangeError（ES 23.2.3.30）。
            "with" => {
                let raw = args.first().map(|v| v.to_number()).unwrap_or(f64::NAN);
                let idx = if raw.is_nan() { 0.0 } else { raw.trunc() };
                let i = if idx < 0.0 { len as f64 + idx } else { idx };
                if i < 0.0 || i >= len as f64 {
                    return Err(RuntimeError::RangeError(
                        "TypedArray.prototype.with: index out of range".to_string(),
                    ));
                }
                let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let mut out = elements.clone();
                out[i as usize] = value;
                let result = self.typed_array_species_create(receiver, len, module)?;
                let _r = self.root(&result);
                self.write_typed_array_elements(&result, &out);
                Ok(Some(result))
            }
            // `toReversed()`：返回**新**视图，元素逆序（不改动原视图）。
            "toReversed" => {
                let mut out = elements.clone();
                out.reverse();
                let result = self.typed_array_species_create(receiver, len, module)?;
                let _r = self.root(&result);
                self.write_typed_array_elements(&result, &out);
                Ok(Some(result))
            }
            // `toSorted(comparefn)`：返回**新**视图，元素排序。
            // 无比较器时按数值升序；有比较器时用插入排序 —— 顺序比较便于在循环里
            // invoke，`sort_by` 的闭包拿不到 `&mut self`。
            "toSorted" => {
                let cmp = args.first().cloned().unwrap_or(Value::Undefined);
                let mut out = elements.clone();
                if cmp.is_callable() {
                    let mut i = 1usize;
                    while i < len {
                        let mut j = i;
                        while j > 0 {
                            let r = self.invoke(
                                &cmp,
                                Value::Undefined,
                                &[out[j - 1].clone(), out[j].clone()],
                                module,
                            )?;
                            if r.to_number() > 0.0 {
                                out.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                        i += 1;
                    }
                } else {
                    out.sort_by(|a, b| {
                        a.to_number()
                            .partial_cmp(&b.to_number())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                for v in &out {
                    acc.push(v);
                }
                let result = self.typed_array_species_create(receiver, len, module)?;
                let _r = self.root(&result);
                self.write_typed_array_elements(&result, &out);
                Ok(Some(result))
            }
            // `sort(comparefn)`：**原地**排序，返回接收者（与 `Array.prototype.sort`
            // 同）。无比较器时按**数值**升序（TypedArray 的默认是数值序，不是
            // 字符串序）；`total_cmp` 给出含 NaN 的全序（NaN 排到最后）。
            // 有比较器时用自底向上归并排序：O(n log n)，且合并循环里能 invoke
            // —— `sort_by` 的闭包拿不到 `&mut self`。
            "sort" => {
                let cmp = args.first().cloned().unwrap_or(Value::Undefined);
                let mut out = elements.clone();
                if cmp.is_callable() {
                    // 这些句柄在合并过程中只会被 clone，故一次性入根即可。
                    for v in &out {
                        acc.push(v);
                    }
                    let mut buf = out.clone();
                    let mut width = 1usize;
                    while width < len {
                        let mut i = 0usize;
                        while i < len {
                            let mid = (i + width).min(len);
                            let end = (i + 2 * width).min(len);
                            let (mut a, mut b, mut k) = (i, mid, i);
                            while a < mid && b < end {
                                let r = self.invoke(
                                    &cmp,
                                    Value::Undefined,
                                    &[out[a].clone(), out[b].clone()],
                                    module,
                                )?;
                                if r.to_number() <= 0.0 {
                                    buf[k] = out[a].clone();
                                    a += 1;
                                } else {
                                    buf[k] = out[b].clone();
                                    b += 1;
                                }
                                k += 1;
                            }
                            while a < mid {
                                buf[k] = out[a].clone();
                                a += 1;
                                k += 1;
                            }
                            while b < end {
                                buf[k] = out[b].clone();
                                b += 1;
                                k += 1;
                            }
                            i = end;
                        }
                        std::mem::swap(&mut out, &mut buf);
                        width *= 2;
                    }
                } else {
                    out.sort_by(|a, b| a.to_number().total_cmp(&b.to_number()));
                }
                self.write_typed_array_elements(receiver, &out);
                Ok(Some(receiver.clone()))
            }
            // `reduce` / `reduceRight`：回调是 (acc, value, index, ta)。
            // 无初值时累加器取首/末元素，从第二个开始；空视图且无初值要 TypeError。
            _ => {
                let right = method == "reduceRight";
                let has_init = args.len() >= 2;
                if !has_init && len == 0 {
                    return Err(RuntimeError::TypeError(
                        "Reduce of empty TypedArray with no initial value".to_string(),
                    ));
                }
                let order: Vec<usize> = if right {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                let (mut acc_val, skip) = if has_init {
                    (args[1].clone(), 0usize)
                } else {
                    (elements[order[0]].clone(), 1usize)
                };
                for i in order.into_iter().skip(skip) {
                    acc_val = self.invoke(
                        &callback,
                        Value::Undefined,
                        &[
                            acc_val.clone(),
                            elements[i].clone(),
                            Value::Number(i as f64),
                            receiver.clone(),
                        ],
                        module,
                    )?;
                    acc.push(&acc_val);
                }
                Ok(Some(acc_val))
            }
        }
    }

    /// `TypedArraySpeciesCreate(O, len)`：结果的构造器由 `O.constructor` 的
    /// `@@species` 决定（ES 23.2.3.x），故 `map` / `filter` 产出的是**同类视图**。
    fn typed_array_species_create(
        &mut self,
        receiver: &Value,
        len: usize,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let ctor = self.get_member(receiver, &PropertyKey::well_known("constructor").expect("well-known name"), module)?;
        let species = if ctor.is_object() {
            let s = self.get_member(&ctor, &crate::builtins::species_symbol_key(), module)?;
            if matches!(s, Value::Undefined | Value::Null) {
                ctor.clone()
            } else {
                s
            }
        } else {
            ctor.clone()
        };
        self.construct(&species, &[Value::Number(len as f64)], module)
    }

    /// 把一批值写进已按 `len` 建好的视图（元素会按视图类型做数值转换）。
    fn write_typed_array_elements(&mut self, ta: &Value, vals: &[Value]) {
        if let Value::Object(o) = ta {
            let mut b = o.borrow_mut();
            if let Some(t) = b.as_typed_array_mut() {
                for (i, v) in vals.iter().enumerate() {
                    t.set_element(i, v.clone());
                }
            }
        }
    }

    /// 把类数组快照里的空洞补成 `undefined`（`toReversed` / `toSorted` 等
    /// ES2023 方法的结果都是密集数组，不再保留空洞）。
    fn dense_entries(entries: &[Option<Value>]) -> Vec<Value> {
        entries
            .iter()
            .map(|e| e.clone().unwrap_or(Value::Undefined))
            .collect()
    }

    /// 用 `ArraySpeciesCreate` 建一个新数组并写入 `vals`。
    ///
    /// 走 species 是因为子类（`class My extends Array {}`）的这些方法也必须
    /// 返回子类实例，不能用 `new_array_object_from_vec` 直接造普通数组。
    fn array_result_from(
        &mut self,
        receiver: &Value,
        vals: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let species = self.array_species_create(receiver, vals.len(), module)?;
        match species {
            None => Ok(crate::vm::object::new_array_object_from_vec(
                &mut self.heap,
                vals.to_vec(),
            )),
            Some(target) => {
                for (i, v) in vals.iter().enumerate() {
                    let key = PropertyKey::element(&mut self.heap, i as u64);
                    self.set_member(&target, key, v.clone(), module)?;
                }
                Ok(target)
            }
        }
    }

    /// `splice` / `toSpliced` 的 `start`：负数从末尾算，再夹到 `[0, len]`。
    fn splice_start(&self, arg: Option<&Value>, len: usize) -> usize {
        let raw = arg.map(|v| v.to_number()).unwrap_or(0.0);
        let idx = if raw.is_nan() { 0.0 } else { raw.trunc() };
        let i = if idx < 0.0 {
            (len as f64 + idx).max(0.0)
        } else {
            idx.min(len as f64)
        };
        i as usize
    }

    pub(crate) fn try_array_callback_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        const CALLBACK_METHODS: &[&str] = &[
            "map",
            "filter",
            "forEach",
            "some",
            "every",
            "reduce",
            "reduceRight",
            "find",
            "findIndex",
            "flatMap",
            "sort",
            // No callback, but they need the generic array-like element list:
            // `Array.prototype.indexOf.call({length: 2, 0: 'a'}, 'a')`.
            "indexOf",
            "lastIndexOf",
            // ES2023：`findLast` / `findLastIndex` 需要谓词；`toReversed` /
            // `toSorted` / `toSpliced` / `with` 不需要回调，但同样要那份
            // 类数组元素清单，且结果要走 `ArraySpeciesCreate`。
            "findLast",
            "findLastIndex",
            "toReversed",
            "toSorted",
            "toSpliced",
            "with",
        ];
        if !CALLBACK_METHODS.contains(&method) {
            return Ok(None);
        }
        // `Array.prototype.map.call(null, …)` and friends: ToObject raises.
        if matches!(receiver, Value::Undefined | Value::Null) {
            return Err(RuntimeError::TypeError(format!(
                "Array.prototype.{method} called on null or undefined"
            )));
        }
        // `forEach` also lives on `Map.prototype` / `Set.prototype`, and this
        // interception runs before the native dispatch (it is what makes
        // `arr.forEach(cb)` call the callback at all). A collection has no
        // `length`, so it would be treated as a zero-length array-like and the
        // callback would never run — `m.forEach(cb)` / `s.forEach(cb)` has to
        // fall through to the collection's own method. Every *other* receiver
        // keeps the old behaviour, including primitives
        // (`Array.prototype.forEach.call(true, cb)` iterates nothing).
        // `TypedArray` 同理，而且更微妙：`indexOf` / `lastIndexOf` 也在上面的
        // `CALLBACK_METHODS` 名单里，若不让出去就会被数组那条路截胡 —— 于是用上
        // 数组的相等判定（`NaN` 能被 `indexOf` 找到）而不是 TypedArray 自己的
        // 严格相等。让出去之后由 `call_prototype_method` 的
        // `is_typedarray_receiver` 分支接管（`ta_index_of` / `ta_last_index_of`）。
        if matches!(receiver, Value::Object(o)
            if matches!(o.borrow().kind(), ObjectKind::Map | ObjectKind::Set | ObjectKind::TypedArray))
        {
            return Ok(None);
        }

        // 只取 length：元素在每一步重新 `HasProperty` + `[[Get]]`
        // （`array_live_at`），物化整张清单会让访问器元素被调用两次。
        let len = match self.array_like_length(receiver, module) {
            Ok(len) => len,
            // Not array-like (e.g. `filter.call(undefined, …)`): let the caller
            // continue with the ordinary dispatch, which raises a TypeError.
            Err(RuntimeError::TypeError(_)) => return Ok(None),
            Err(err) => return Err(err),
        };

        // `indexOf` / `lastIndexOf` take a search element, not a callback, and
        // must not hit the callable check below. Strings keep their own
        // substring semantics (`"abcab".indexOf("ab")` is 0, not a per-char
        // scan), so a string receiver falls through to the string builtins.
        if method == "indexOf" || method == "lastIndexOf" {
            // `Array.prototype.indexOf.call(x, …)` and `x.indexOf(…)` reach the
            // same dispatch site. Anything that is not array-like — a string
            // (substring search) or a wrapper without `length`
            // (`String.prototype.indexOf.call(new Boolean, …)`) — belongs to
            // the string builtins.
            if !self.receiver_is_array_like(receiver) {
                return Ok(None);
            }
            // `ToObject(receiver)`: `indexOf` 与迭代族用同一个接收者口径
            // （基本类型先装箱，索引属性长在包装对象上）。
            let target = match receiver {
                Value::Object(_) | Value::Function(_) => receiver.clone(),
                other => crate::builtins::to_object(&mut self.heap, other)
                    .unwrap_or_else(|_| other.clone()),
            };
            return self
                .array_index_of_live(&target, len, method, args, module)
                .map(Some);
        }

        let callback = args.first().cloned().unwrap_or(Value::Undefined);
        // Second argument is `thisArg`: undefined means "call with undefined
        // `this`" (only relevant for non-strict callbacks, but observable).
        let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
        // 宿主侧入根：`callback` / `this_arg` / 接收者都是本地量，而下面每次
        // 调用用户回调都会跑字节码、到达 GC 安全点。收集器看不见机器栈，不登记
        // 的话这些槽会被回收再分配（GC stress 实测：
        // `species_target_receives_the_elements` 输出一串 hole）。
        let _root_cb = self.root(&callback);
        let _root_this = self.root(&this_arg);
        let _root_recv = self.root(receiver);
        // The callback's third argument is `O` — `ToObject(this value)` — not the
        // primitive receiver itself: `Array.prototype.reduce.call(false, cb, 1)`
        // is asserted with `obj instanceof Boolean`, which is only true for the
        // wrapper.
        let callback_receiver = match receiver {
            Value::Object(_) | Value::Function(_) => receiver.clone(),
            other => crate::builtins::to_object(&mut self.heap, other)
                .unwrap_or_else(|_| other.clone()),
        };
        let call = |vm: &mut Self,
                    cb: &Value,
                    elem: &Value,
                    index: usize|
         -> Result<Value, RuntimeError> {
            vm.invoke(
                cb,
                this_arg.clone(),
                &[
                    elem.clone(),
                    Value::Number(index as f64),
                    callback_receiver.clone(),
                ],
                module,
            )
        };

        match method {
            "sort" => {
                let entries = self.array_like_entries(receiver, module)?;
                // Holes sort to the end; the dense store has no hole marker, so
                // they are treated as `undefined` (the previous behaviour).
                let mut items: Vec<Value> = entries
                    .iter()
                    .map(|e| e.clone().unwrap_or(Value::Undefined))
                    .collect();
                if callback.is_callable() {
                    // Insertion sort: stable and lets the comparator be a
                    // fallible JS call.
                    for i in 1..items.len() {
                        let mut j = i;
                        while j > 0 {
                            let cmp = self
                                .invoke(
                                    &callback,
                                    Value::Undefined,
                                    &[items[j - 1].clone(), items[j].clone()],
                                    module,
                                )?
                                .to_number();
                            if cmp > 0.0 {
                                items.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else if !callback.is_undefined() {
                    return Err(RuntimeError::TypeError(
                        "The comparison function must be either a function or undefined"
                            .to_string(),
                    ));
                } else {
                    items.sort_by(|a, b| a.to_js_string().cmp(&b.to_js_string()));
                }
                if let Value::Object(obj_ref) = receiver {
                    let mut borrowed = obj_ref.borrow_mut();
                    if let Some(arr) = borrowed.as_array_mut() {
                        arr.replace_elements(items);
                    }
                }
                Ok(Some(receiver.clone()))
            }
            // 只有"必须给回调"的方法才在这里拒；`toSorted` / `sort` 的比较器、
            // `toReversed` / `with` / `indexOf` 等都不需要（此前写成通配 `_`，
            // 于是 `[3,1,2].toSorted()` 报 "callback is not a function"）。
            method
                if matches!(
                    method,
                    "map"
                        | "filter"
                        | "forEach"
                        | "some"
                        | "every"
                        | "reduce"
                        | "reduceRight"
                        | "find"
                        | "findIndex"
                        | "findLast"
                        | "findLastIndex"
                        | "flatMap"
                ) && !callback.is_callable() =>
            {
                Err(RuntimeError::TypeError(format!(
                    "{method} callback is not a function"
                )))
            }
            "forEach" => {
                // 逐步 `HasProperty` + `[[Get]]`（ES 23.1.3.13 step 6.a.ii）：
                // 回调删掉后面的元素 ⇒ 跳过；新加进来的 ⇒ 会被访问。
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    call(self, &callback, &e, i)?;
                }
                Ok(Some(Value::Undefined))
            }
            "map" => {
                // Step 5: `A = ? ArraySpeciesCreate(O, len)`. When the default
                // array is used the result keeps the receiver's length *and*
                // its holes; with a species constructor only the present
                // indices are written, and a blocked write is abrupt
                // (CreateDataPropertyOrThrow).
                let species = self.array_species_create(receiver, len, module)?;
                let mut values: Vec<(usize, Value)> = Vec::with_capacity(len);
                // 累加器是 Rust 局部量，而 `call` 每次都会跑用户代码 ⇒ 每产出一个
                // 值就必须立刻入根，否则中途一次收集会把已产出的那部分收走。
                // `species` 自身也跨整个循环存活，同样要入根。
                let mut acc = self.roots();
                if let Some(ref sp) = species {
                    acc.push(sp);
                }
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    let v = call(self, &callback, &e, i)?;
                    acc.push(&v);
                    values.push((i, v));
                }
                Ok(Some(match species {
                    None => {
                        let mut out: Vec<Value> = vec![Value::Undefined; len];
                        let mut holes: Vec<usize> = Vec::new();
                        let mut next = 0usize;
                        for (i, v) in values {
                            out[i] = v;
                            for h in next..i {
                                holes.push(h);
                            }
                            next = i + 1;
                        }
                        for h in next..len {
                            holes.push(h);
                        }
                        let result = crate::vm::object::new_array_object_from_vec(&mut self.heap, out);
                        if let Value::Object(result_ref) = &result {
                            if let Some(arr) = result_ref
                                .borrow_mut().as_array_mut()
                            {
                                for i in holes {
                                    arr.mark_hole(i);
                                }
                            }
                        }
                        result
                    }
                    Some(target) => {
                        for (i, v) in values {
                            let key = PropertyKey::element(&mut self.heap, i as u64);
                            self.set_member(&target, key, v, module)?;
                        }
                        target
                    }
                }))
            }
            "flatMap" => {
                // Map, then flatten one level (ES 23.1.3.?): a mapped value that
                // is an array contributes its elements, anything else is
                // appended as-is. This is the same `call` plumbing `map` uses.
                // The result length is `len`, so `ArraySpeciesCreate` is asked for
                // it — that is what makes the `MAX_MATERIALIZED_LEN` cap fire
                // (e.g. `flatMap` over a 2**32-length array-like must be a
                // `RangeError`, not a 16 GiB allocation).
                let species = self.array_species_create(receiver, len, module)?;
                let mut values: Vec<Value> = Vec::new();
                let mut acc = self.roots();
                if let Some(ref sp) = species {
                    acc.push(sp);
                }
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    {
                        let mapped = call(self, &callback, &e, i)?;
                        let mut flattened = false;
                        if let Value::Object(obj_ref) = &mapped {
                            if obj_ref.borrow().kind() == ObjectKind::Array {
                                let borrowed = obj_ref.borrow();
                                if let Some(arr) = borrowed.as_array()
                                {
                                    for k in 0..arr.len() {
                                        if let Some(v) = arr.get(k) {
                                            acc.push(v);
                                            values.push(v.clone());
                                        }
                                    }
                                    flattened = true;
                                }
                            }
                        }
                        if !flattened {
                            acc.push(&mapped);
                            values.push(mapped);
                        }
                    }
                }
                Ok(Some(match species {
                    None => crate::vm::object::new_array_object_from_vec(&mut self.heap, values),
                    Some(target) => {
                        for (i, v) in values.into_iter().enumerate() {
                            let key = PropertyKey::element(&mut self.heap, i as u64);
                            self.set_member(&target, key, v, module)?;
                        }
                        target
                    }
                }))
            }
            "filter" => {
                // Result length is `len`, so `ArraySpeciesCreate` is asked for it —
                // that is what makes the `MAX_MATERIALIZED_LEN` cap fire (e.g.
                // `filter` over a 2**32-length array-like must be a `RangeError`,
                // not a 16 GiB allocation).
                let species = self.array_species_create(receiver, len, module)?;
                let mut out: Vec<Value> = Vec::new();
                let mut acc = self.roots();
                if let Some(ref sp) = species {
                    acc.push(sp);
                }
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if call(self, &callback, &e, i)?.to_boolean() {
                        acc.push(&e);
                        out.push(e);
                    }
                }
                Ok(Some(match species {
                    None => crate::vm::object::new_array_object_from_vec(&mut self.heap, out),
                    Some(target) => {
                        for (i, v) in out.iter().enumerate() {
                            let key = PropertyKey::element(&mut self.heap, i as u64);
                            self.set_member(&target, key, v.clone(), module)?;
                        }
                        target
                    }
                }))
            }
            "some" => {
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if call(self, &callback, &e, i)?.to_boolean() {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            "every" => {
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if !call(self, &callback, &e, i)?.to_boolean() {
                        return Ok(Some(Value::Bool(false)));
                    }
                }
                Ok(Some(Value::Bool(true)))
            }
            "find" => {
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if call(self, &callback, &e, i)?.to_boolean() {
                        return Ok(Some(e));
                    }
                }
                Ok(Some(Value::Undefined))
            }
            "findIndex" => {
                for i in 0..len {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if call(self, &callback, &e, i)?.to_boolean() {
                        return Ok(Some(Value::Number(i as f64)));
                    }
                }
                Ok(Some(Value::Number(-1.0)))
            }
            // ── ES2023 ──
            // `findLast` / `findLastIndex`：从**末尾**往前找第一个命中的。
            "findLast" => {
                for i in (0..len).rev() {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if call(self, &callback, &e, i)?.to_boolean() {
                        return Ok(Some(e));
                    }
                }
                Ok(Some(Value::Undefined))
            }
            "findLastIndex" => {
                for i in (0..len).rev() {
                    let Some(e) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    if call(self, &callback, &e, i)?.to_boolean() {
                        return Ok(Some(Value::Number(i as f64)));
                    }
                }
                Ok(Some(Value::Number(-1.0)))
            }
            // `toReversed()`：返回**新**数组，元素逆序（不改原数组）。
            "toReversed" => {
                let entries = self.array_like_entries(receiver, module)?;
                let mut vals = Self::dense_entries(&entries);
                vals.reverse();
                Ok(Some(self.array_result_from(receiver, &vals, module)?))
            }
            // `toSorted(comparefn)`：返回**新**数组。注意 **Array 的默认比较是
            // 字符串序**（与 `TypedArray.prototype.toSorted` 的数值序不同）。
            "toSorted" => {
                let entries = self.array_like_entries(receiver, module)?;
                let cmp = args.first().cloned().unwrap_or(Value::Undefined);
                let mut vals = Self::dense_entries(&entries);
                if cmp.is_callable() {
                    // 插入排序：顺序比较便于在循环里 invoke —— `sort_by` 的闭包
                    // 拿不到 `&mut self`。
                    let mut i = 1usize;
                    while i < vals.len() {
                        let mut j = i;
                        while j > 0 {
                            let r = self.invoke(
                                &cmp,
                                Value::Undefined,
                                &[vals[j - 1].clone(), vals[j].clone()],
                                module,
                            )?;
                            if r.to_number() > 0.0 {
                                vals.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                        i += 1;
                    }
                } else {
                    vals.sort_by(|a, b| a.to_js_string().cmp(&b.to_js_string()));
                }
                Ok(Some(self.array_result_from(receiver, &vals, module)?))
            }
            // `with(index, value)`：返回**新**数组，指定下标换成新值；下标越界
            // 是 RangeError（ES 23.1.3.31）。负下标从末尾算。
            "with" => {
                let entries = self.array_like_entries(receiver, module)?;
                let mut vals = Self::dense_entries(&entries);
                let len = vals.len();
                let raw = args.first().map(|v| v.to_number()).unwrap_or(f64::NAN);
                let idx = if raw.is_nan() { 0.0 } else { raw.trunc() };
                let i = if idx < 0.0 { len as f64 + idx } else { idx };
                if i < 0.0 || i >= len as f64 {
                    return Err(RuntimeError::RangeError(
                        "Array.prototype.with: index out of range".to_string(),
                    ));
                }
                vals[i as usize] = args.get(1).cloned().unwrap_or(Value::Undefined);
                Ok(Some(self.array_result_from(receiver, &vals, module)?))
            }
            // `toSpliced(start, deleteCount, ...items)`：返回**新**数组，等价于
            // 在副本上做一次 `splice`（不改原数组）。
            "toSpliced" => {
                let entries = self.array_like_entries(receiver, module)?;
                let vals = Self::dense_entries(&entries);
                let len = vals.len();
                let start = self.splice_start(args.first(), len);
                let delete_count = match args.get(1) {
                    None | Some(Value::Undefined) => len - start,
                    Some(v) => {
                        let n = v.to_number();
                        if n.is_nan() {
                            0usize
                        } else {
                            n.trunc().max(0.0) as usize
                        }
                    }
                }
                .min(len - start);
                let mut out = Vec::with_capacity(len - delete_count + args.len().saturating_sub(2));
                out.extend_from_slice(&vals[..start]);
                out.extend(args.iter().skip(2).cloned());
                out.extend_from_slice(&vals[start + delete_count..]);
                Ok(Some(self.array_result_from(receiver, &out, module)?))
            }
            "reduce" | "reduceRight" => {
                // Indices are visited in order (or reverse); presence and value
                // are re-read at every step, so a callback that deletes a later
                // element skips it (`reduce` 那一族 `callCnt` 用例）。
                let indices: Vec<usize> = if method == "reduceRight" {
                    (0..len).rev().collect()
                } else {
                    (0..len).collect()
                };
                // Without an initial value the accumulator is the first
                // *present* element, and the loop resumes right after it.
                let (mut acc, resume_at) = match args.get(1) {
                    Some(init) => (init.clone(), 0usize),
                    None => {
                        let mut found = None;
                        for (pos, &i) in indices.iter().enumerate() {
                            if let Some(v) = self.array_live_at(&callback_receiver, i, module)? {
                                found = Some((v, pos + 1));
                                break;
                            }
                        }
                        let Some((value, resume)) = found else {
                            return Err(RuntimeError::TypeError(
                                "Reduce of empty array with no initial value".to_string(),
                            ));
                        };
                        (value, resume)
                    }
                };
                for &i in indices.iter().skip(resume_at) {
                    let Some(element) = self.array_live_at(&callback_receiver, i, module)? else {
                        continue;
                    };
                    // `reduce` has no `thisArg` parameter: the callback runs
                    // with `undefined` as `this`.
                    acc = self.invoke(
                        &callback,
                        Value::Undefined,
                        &[acc, element, Value::Number(i as f64), callback_receiver.clone()],
                        module,
                    )?;
                }
                Ok(Some(acc))
            }
            _ => Ok(None),
        }
    }

    /// `indexOf` / `lastIndexOf` with the **live** element view: `HasProperty`
    /// and `[[Get]]` are re-evaluated per index (ES 23.1.3.25 step 8.a).
    ///
    /// `len` is the length captured before the loop, as the spec does.
    fn array_index_of_live(
        &mut self,
        target: &Value,
        len: usize,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        fn same_value_zero(a: &Value, b: &Value) -> bool {
            if let (Value::Number(x), Value::Number(y)) = (a, b)
                && x.is_nan()
                && y.is_nan()
            {
                return true;
            }
            a.strict_eq(b)
        }

        let search = args.first().cloned().unwrap_or(Value::Undefined);
        let len_i = len as i64;
        // `? ToIntegerOrInfinity(fromIndex)`: ToNumber(Symbol) is abrupt, while
        // an absent argument reads as ToIntegerOf(undefined) = 0.
        let from = match args.get(1) {
            Some(v) if !v.is_undefined() => {
                crate::builtins::to_integer_or_infinity_throwing(Some(v))?
            }
            _ => {
                if method == "indexOf" {
                    0
                } else {
                    len_i - 1
                }
            }
        };

        if method == "indexOf" {
            let start = from.max(0);
            if start >= len_i {
                return Ok(Value::Number(-1.0));
            }
            for i in start..len_i {
                if let Some(element) = self.array_live_at(target, i as usize, module)?
                    && same_value_zero(&element, &search)
                {
                    return Ok(Value::Number(i as f64));
                }
            }
        } else {
            let mut i = if from < 0 { -1 } else { from.min(len_i - 1) };
            while i >= 0 {
                if let Some(element) = self.array_live_at(target, i as usize, module)?
                    && same_value_zero(&element, &search)
                {
                    return Ok(Value::Number(i as f64));
                }
                i -= 1;
            }
        }
        Ok(Value::Number(-1.0))
    }

    /// `Array.prototype` 的**读族**（`join` / `slice` / `includes` / `at` /
    /// `concat` / `reverse`），走 `[[Get]]` 与 `LengthOfArrayLike`。
    ///
    /// 这一族原来直接在 `ArrayObject` 的密集存储上工作，于是三件事同时错了：
    /// 访问器元素读出来是空位（`Object.defineProperty(a, 0, {get})` 之后
    /// `a.join()` 得到 `",2"`）、类数组接收者（`{length: 2, 0: 'a'}`）走不通、
    /// 原型链上的 `length` 访问器看不到。`map` / `filter` / `indexOf` 早就走
    /// 了 VM 的通用路径（`try_array_callback_method`），读族一直没跟上。
    ///
    /// 只处理**对象**接收者：`"abc".slice(1)` / `"abc".includes(…)` / `"abc".at(…)`
    /// 必须落到 `String.prototype` 的同名方法上，而它们在派发层只按名字区分。
    /// `join` / `reverse` 没有字符串同名方法，所以基本类型也接（`Object(…)` 由
    /// `array_like_entries` 负责）。
    pub(crate) fn try_array_read_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        const READ_METHODS: &[&str] = &["join", "slice", "includes", "at", "concat", "reverse"];
        if !READ_METHODS.contains(&method) {
            return Ok(None);
        }
        if matches!(receiver, Value::Undefined | Value::Null) {
            return Err(RuntimeError::TypeError(format!(
                "Array.prototype.{method} called on null or undefined"
            )));
        }
        // 只有"像数组的东西"才走这条路：`Array`、普通对象、`arguments`、函数
        // 与 `Proxy`。其余内建对象都有自己的一族同名方法（TypedArray 的 `slice`
        // 返回同类视图、`ArrayBuffer.prototype.slice` 走 species），按名字截胡
        // 会把它们全打坏 —— `class AB extends ArrayBuffer {}` 的 `ab.slice(0,1)`
        // 曾经因此变成一个空数组。
        if let Value::Object(o) = receiver
            && !matches!(
                o.borrow().kind(),
                ObjectKind::Array
                    | ObjectKind::Ordinary
                    | ObjectKind::Arguments
                    | ObjectKind::Function
                    | ObjectKind::Proxy
            )
        {
            return Ok(None);
        }
        // `slice` / `includes` / `at` / `concat` 在 `String.prototype` 上也有同名
        // 方法，而派发层只按名字区分 —— 所以**字符串与字符串包装对象**都要让出去，
        // 否则 `new String("abc").slice(1)` 会被当成类数组返回一个数组。
        let string_like = matches!(method, "slice" | "includes" | "at" | "concat");
        if string_like {
            // 与内建层的 `string_prototype_receiver` 保持**同一口径**：String 包装
            // 之外，Number / Boolean 包装也算 —— `new Object(42).concat(false,true)`
            // 绑定的是 `String.prototype.concat`，结果是 "42falsetrue"，不是数组。
            let is_string_receiver = match receiver {
                Value::Object(o) => matches!(
                    o.borrow().kind(),
                    ObjectKind::String | ObjectKind::Number | ObjectKind::Boolean
                ),
                // 其余基本类型也走 String.prototype（`"abc".slice(…)`）。
                _ => true,
            };
            if is_string_receiver {
                return Ok(None);
            }
        }

        let entries = self.array_like_entries(receiver, module)?;
        let len = entries.len() as i64;

        match method {
            "join" => {
                let sep = match args.first() {
                    Some(sep) if !sep.is_undefined() => sep.to_js_string(),
                    _ => ",".to_string(),
                };
                let mut out = String::new();
                for (i, entry) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&sep);
                    }
                    // Holes and `undefined`/`null` all render as the empty string
                    // (ES 23.1.3.15 step 6.c).
                    if let Some(v) = entry
                        && !matches!(v, Value::Undefined | Value::Null)
                    {
                        out.push_str(&v.to_js_string());
                    }
                }
                Ok(Some(Value::string(&out)))
            }
            "slice" => {
                let start = Self::relative_index(args.first(), len, 0)?;
                let end = Self::relative_index(args.get(1), len, len)?;
                let count = (end - start).max(0) as usize;
                let picked: Vec<Option<Value>> = (0..count)
                    .map(|k| entries.get((start as usize) + k).cloned().flatten())
                    .collect();
                Ok(Some(self.array_result_preserving_holes(receiver, &picked, module)?))
            }
            "includes" => {
                let search = args.first().cloned().unwrap_or(Value::Undefined);
                let from = Self::relative_index(args.get(1), len, 0)?.max(0);
                for i in from..len {
                    if let Some(v) = &entries[i as usize]
                        && Self::same_value_zero(v, &search)
                    {
                        return Ok(Some(Value::Bool(true)));
                    }
                }
                Ok(Some(Value::Bool(false)))
            }
            "at" => {
                let index = Self::relative_index(args.first(), len, 0)?;
                let index = if index < 0 { len + index } else { index };
                let value = if index < 0 || index >= len {
                    Value::Undefined
                } else {
                    entries[index as usize].clone().unwrap_or(Value::Undefined)
                };
                Ok(Some(value))
            }
            "concat" => {
                // ES 23.1.3.2: `IsConcatSpreadable(item)` decides between "spread
                // its elements" and "append the item itself". The default is
                // `Array.isArray(item)`, overridable through @@isConcatSpreadable.
                let mut out: Vec<Option<Value>> = Vec::new();
                let mut items = vec![receiver.clone()];
                items.extend(args.iter().cloned());
                for item in &items {
                    let spreadable = self.is_concat_spreadable(item, module)?;
                    if spreadable {
                        let sub = self.array_like_entries(item, module)?;
                        out.extend(sub);
                    } else {
                        out.push(Some(item.clone()));
                    }
                }
                Ok(Some(self.array_result_preserving_holes(receiver, &out, module)?))
            }
            "reverse" => {
                // In place, and holes matter: the spec *deletes* the property when
                // the partner slot is a hole (23.1.3.23 step 5.c) instead of
                // writing `undefined`.
                let len = entries.len();
                for lower in 0..len / 2 {
                    let upper = len - 1 - lower;
                    let lower_key = PropertyKey::element(&mut self.heap, lower as u64);
                    let upper_key = PropertyKey::element(&mut self.heap, upper as u64);
                    let (lower_v, upper_v) = (entries[lower].clone(), entries[upper].clone());
                    match upper_v {
                        Some(v) => {
                            self.set_member(receiver, lower_key, v, module)?;
                        }
                        None => {
                            self.delete_member(receiver, &lower_key, module)?;
                        }
                    }
                    match lower_v {
                        Some(v) => {
                            self.set_member(receiver, upper_key, v, module)?;
                        }
                        None => {
                            self.delete_member(receiver, &upper_key, module)?;
                        }
                    }
                }
                Ok(Some(receiver.clone()))
            }
            _ => Ok(None),
        }
    }

    /// `ToIntegerOrInfinity(index)` clamped into `[0, len]`, with `missing` as the
    /// value for an absent (or `undefined`) argument — the shared preamble of
    /// `slice` / `includes` / `at`.
    fn relative_index(
        arg: Option<&Value>,
        len: i64,
        missing: i64,
    ) -> Result<i64, RuntimeError> {
        let Some(v) = arg else { return Ok(missing) };
        // `ToIntegerOrInfinity` starts with `ToNumber`, which throws for a Symbol
        // (`[1, 2].at(Symbol())` is a TypeError, not index 0).
        let n = crate::builtins::to_number_throwing(v)?;
        if n.is_nan() {
            return Ok(0);
        }
        let n = if n == f64::INFINITY {
            len
        } else if n == f64::NEG_INFINITY {
            0
        } else {
            let t = n.trunc();
            if t < 0.0 { (len as f64 + t) as i64 } else { t as i64 }
        };
        Ok(n.clamp(0, len))
    }

    /// SameValueZero: like `===`, but `NaN` matches itself.
    fn same_value_zero(a: &Value, b: &Value) -> bool {
        if let (Value::Number(x), Value::Number(y)) = (a, b) {
            if x.is_nan() && y.is_nan() {
                return true;
            }
        }
        a.strict_eq(b)
    }

    /// `IsConcatSpreadable` (ES 23.1.3.2.1): `@@isConcatSpreadable` when it is
    /// defined, `Array.isArray` otherwise.
    fn is_concat_spreadable(
        &mut self,
        item: &Value,
        module: &Module,
    ) -> Result<bool, RuntimeError> {
        if !item.is_object() && !matches!(item, Value::Function(_)) {
            return Ok(false);
        }
        // Through `[[Get]]`, so an accessor `@@isConcatSpreadable` runs.
        let key = crate::builtins::is_concat_spreadable_symbol_key();
        let flag = self.get_member(item, &key, module)?;
        if flag.is_undefined() {
            return Ok(Self::is_array_value(item));
        }
        Ok(flag.to_boolean())
    }

    fn is_array_value(value: &Value) -> bool {
        matches!(value, Value::Object(o) if o.borrow().kind() == ObjectKind::Array)
    }

    /// Build the result of a read-family method: `ArraySpeciesCreate` when the
    /// receiver has a custom species, an ordinary array otherwise. Holes stay
    /// holes either way (spec: `CreateDataPropertyOrThrow` only for present
    /// indices).
    fn array_result_preserving_holes(
        &mut self,
        receiver: &Value,
        entries: &[Option<Value>],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        match self.array_species_create(receiver, entries.len(), module)? {
            None => {
                let mut arr = crate::vm::object::ArrayObject::new();
                arr.set_prototype(Some(self.builtins.array_prototype));
                for entry in entries {
                    match entry {
                        Some(v) => arr.push(v.clone()),
                        None => {
                            arr.push(Value::Undefined);
                            let index = arr.len() - 1;
                            arr.mark_hole(index);
                        }
                    }
                }
                Ok(Value::Object(self.heap.alloc(crate::vm::gc::Object::Array(arr))))
            }
            Some(target) => {
                // `CreateDataPropertyOrThrow`: a species target that refuses the
                // write (non-extensible, frozen) has to raise, not silently drop
                // the element (`result_writes_are_abrupt_when_the_target_blocks_them`).
                for (i, entry) in entries.iter().enumerate() {
                    let key = PropertyKey::element(&mut self.heap, i as u64);
                    if let Some(v) = entry
                        && !Self::create_data_property(&mut self.heap, &target, &key, v)
                    {
                        return Err(RuntimeError::TypeError(format!(
                            "Cannot define property {i} on the array-species result"
                        )));
                    }
                }
                Ok(target)
            }
        }
    }
}
