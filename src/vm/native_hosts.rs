//! 各 `NativeEntry::Vm*` 的宿主实现。原形如名字分派文件 `builtins_dispatch.rs`
//! 已删除：这里的 `call_native_by_name` 只是少数全局函数/构造器的兜底。
//!
//! 它们住在 VM 里而不是 `builtins/` 里，是因为这些路径要么要回调 JS
//! （`Array.prototype.map` 的回调、`String.prototype.replace` 的函数替换器），
//! 要么要造只有 VM 能造的对象（迭代器注册在 VM 的迭代器表里，`next()`
//! 靠它找状态）。
//!
//! 按名字字符串 `match` 本身是一处已知的设计债（ARCHITECTURE §5 第 3 条）：
//! 由它派生出 arity 表、`__proto__xxx` 前缀约定、`vm_handled_static` 旁路
//! 名单这一整套约定。搬到这里是为了让它们都在一处，而不是认可这个形状。
//!
//! **正在迁走**：`Math` / `console` / `Date` / `Number.prototype` /
//! `Boolean.prototype` / `Promise`（六静态方法 + `then/catch/finally`） /
//! `Object.assign` / `Array.isArray,from,of` / `String.fromCharCode,fromCodePoint`
//! / 五个 `Object.*` 与四个 `Reflect.*` 静态方法 / `String.raw` / `JSON.*` /
//! `TypedArray.from,of` / `Map.groupBy` / `$262.detachArrayBuffer` /
//! 七个剩余 `Object.*` 静态方法、`Proxy.revocable`、
//! `Reflect.get/set/apply/construct`（以及全部 `Reflect.*`）、`Proxy.revocable`
//! 与剩余纯静态方法已经把实现放进函数对象
//! （[`crate::vm::native::NativeEntry`]），调用走
//! `VM::invoke_native`，不再经过本文件。每迁一族就从这里删掉一族，`entry`
//! 为 `None` 的内建才落到按名分派。静态方法那条按名分支已整体删除
//! （属性解析 → `invoke_native` → entry 是唯一路径）；原型方法族正逐批迁入，
//! `Array`/`Map`/`Set`/`TypedArray` 的 `entries/keys/values` 已各用三个独立
//! slot 区分（同名不同函数对象，`make_iterator_with` 也改为按函数身份识别）；
//! `Array.prototype` 其余方法统一由 `NativeEntry::VmArray(ArrayOp)` 分发。

use std::cell::RefCell;
use std::rc::Rc;

use crate::bytecode::Module;
use crate::vm::native::{ArrayOp, CollectionOp, SpecialOp, StringOp};
use crate::vm::object::JSObject;
use crate::vm::{ObjectKind, PropertyDescriptor, PropertyKey, RuntimeError, Value, VM};

impl VM {
    /// Array `length` 的 [[Value]] 必须先走 ES `ToNumber`：描述符里可以是一个带
    /// `valueOf` / `toString` / `Symbol.toPrimitive` 的对象，而
    /// `ArrayObject::define_property` 只有 Rust 层的 `to_number()`（不跑用户代码，
    /// 得到 NaN 之后变成 `RangeError: Invalid array length`）。
    ///
    /// 这里在把描述符交给 builtins 之前，把 `value` 换成转换后的 Number。
    /// 只对 Array 的 `length` 生效，其余对象/键的行为一字不改。
    fn normalize_array_length_value(
        &mut self,
        target: &Value,
        descriptor: &Value,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        let Value::Object(target_ref) = target else {
            return Ok(());
        };
        if target_ref.borrow().kind() != ObjectKind::Array {
            return Ok(());
        }
        let Value::Object(desc_ref) = descriptor else {
            return Ok(());
        };
        let value_key = PropertyKey::intern(&mut self.heap, "value");
        let raw = {
            let borrowed = desc_ref.borrow();
            match borrowed.property_get(&value_key) {
                Some(d) => d.value,
                None => return Ok(()),
            }
        };
        let prim = self.to_primitive(&raw, "number", module)?;
        let n = crate::builtins::to_number_throwing(&prim)?;
        let _ = desc_ref
            .borrow_mut()
            .property_set(&mut self.heap, value_key, Value::Number(n));
        Ok(())
    }

    /// 仍未挂 `NativeEntry` 的全局函数/构造器的兜底分派。
    ///
    /// 原型方法与静态方法都已迁到函数对象自带的 entry；这里只剩少数按名字
    /// 注册的全局入口需要映射到纯内建实现表。
    pub(crate) fn call_native_by_name(
        &mut self,
        name: &str,
        _this: Value,
        args: &[Value],
        _module: &Module,
    ) -> Result<Value, RuntimeError> {
        let boxed_args: Vec<Value> = args.iter().map(|a| self.as_object_value(a)).collect();
        crate::builtins::call_native(name, &boxed_args, &mut self.heap)
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
        // `concat` 的参数不是索引：`ArraySpeciesCreate` 要在循环之前做，
        // 每个元素还要 `IsConcatSpreadable` / `LengthOfArrayLike`
        // （Proxy 的 `IsArray`、用户 `valueOf`、`2**53-1` 的 TypeError）。
        //
        // 字符串接收者除外：`"a".concat("b")` 是 `String.prototype.concat`
        // （拼接），不是数组的展开 —— 让出，由字符串那条路接手。
        if method == "concat" {
            // 字符串接收者（`"a".concat("b")`）走 `String.prototype.concat`；
            // 没有 `length` 的接收者（`Object(42).concat(false, true)` 走的是
            // 引擎的同名回落）也原样让出 —— 否则会得到数组的 `"42,false,true"`。
            if crate::vm::receiver_is_string(receiver)
                || !self.receiver_concat_like(receiver)
            {
                return Ok(None);
            }
            return self.array_concat_vm(receiver, args, module).map(Some);
        }
        // `slice(start, end)` / `splice(start, deleteCount, …)` 的索引参数。
        let coerced = self.coerce_index_method_args("Array", method, args, module)?;
        let args: &[Value] = match &coerced {
            Some(values) => values,
            None => args,
        };
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
        // `CreateDataPropertyOrThrow`（slice 22.1.3.22 第 12.a 步 /
        // splice 22.1.3.28 第 11.b.iii 步）：species 目标拒绝定义时是 TypeError，
        // 目标上是**可配置的**只读属性时则重新定义成功
        // （`splice/target-array-with-non-{writable,configurable}-property`）。
        for (i, value) in elements {
            self.create_data_property_or_throw(&target, i, value, module)?;
        }
        Ok(Some(target))
    }

    /// 一个 `String.prototype` 方法调用：接收者与参数都按规范转换，然后交给
    /// 内建层的按名分派。
    ///
    /// 抽成一个函数是因为**两条**调用路径都要它：
    ///
    /// * 具名分派（`__proto_method__` 被 `String.prototype.<name>` 取代之后，
    ///   仍会从 `Function.prototype.call` 之类进来）；
    /// * 原语接收者的方法调用（`"a,b".split(obj)`）：`[[Get]]` 在规范里先
    ///   `ToObject`，但这条路径**不装箱**（热路径），直接在 exec 里调这里。
    ///
    /// 未知方法照旧返回 "unknown prototype method: …"，调用方据此回落到用户方法。
    /// `%TypedArray%.prototype` 方法的唯一入口：brand/detached 检查都在实现里。
    pub(crate) fn typed_array_proto_host(
        &mut self,
        method: &'static str,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        if !crate::builtins::typedarray::is_typedarray_receiver(this) {
            return Err(RuntimeError::TypeError(format!(
                "%TypedArray%.prototype.{method} called on a receiver that is not a TypedArray"
            )));
        }
        let detached = match this {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .as_typed_array()
                .is_some_and(|ta| ta.is_detached()),
            _ => false,
        };
        if detached {
            return Err(RuntimeError::TypeError(format!(
                "%TypedArray%.prototype.{method} called on a detached buffer"
            )));
        }
        if let Some(result) = self.try_typedarray_callback_method(this, method, args, module)? {
            return Ok(result);
        }
        let coerced = self.coerce_index_method_args("TypedArray", method, args, module)?;
        let dispatch_args: &[Value] = match &coerced {
            Some(values) => values,
            None => args,
        };
        crate::builtins::call_prototype_method_owned(
            "TypedArray",
            &mut self.heap,
            this,
            method,
            dispatch_args,
        )
    }

    /// Map/Set/WeakMap/WeakSet 原型方法的唯一入口：归属由 `CollectionOp` 决定。
    pub(crate) fn collection_host(
        &mut self,
        op: CollectionOp,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        use CollectionOp as C;
        let method = op.as_str();
        match op {
            C::MapGet
            | C::MapHas
            | C::MapSet
            | C::MapDelete
            | C::MapClear
            | C::MapForEach
            | C::MapGetOrInsert
            | C::MapGetOrInsertComputed => self.map_method(this, method, args, module),
            C::SetAdd
            | C::SetHas
            | C::SetDelete
            | C::SetClear
            | C::SetForEach
            | C::SetUnion
            | C::SetIntersection
            | C::SetDifference
            | C::SetSymmetricDifference
            | C::SetIsSubsetOf
            | C::SetIsSupersetOf
            | C::SetIsDisjointFrom => self.set_method(this, method, args, module),
            C::WeakMapSet
            | C::WeakMapGet
            | C::WeakMapHas
            | C::WeakMapDelete
            | C::WeakMapGetOrInsert
            | C::WeakMapGetOrInsertComputed => self.weakmap_method(this, method, args, module),
            C::WeakSetAdd | C::WeakSetHas | C::WeakSetDelete => {
                self.weakset_method(this, method, args)
            }
        }
    }

    /// 无状态特殊内建的唯一入口：getter、species、`Function.prototype` 工具等。
    pub(crate) fn special_host(
        &mut self,
        op: SpecialOp,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::native::SpecialOp as Op;
        match op {
            Op::FunctionProto => Ok(Value::Undefined),
            Op::FunctionHasInstance => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                if !this.is_callable() {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(self.ordinary_has_instance(&value, this, module)?))
            }
            Op::ObjectToString => self.object_prototype_to_string(this, args, module),
            Op::FunctionToString => crate::builtins::function_prototype_to_string(this),
            Op::ObjectProtoGet => crate::builtins::object_proto_getter(&mut self.heap, this),
            Op::ObjectProtoSet => crate::builtins::object_proto_setter(this, args),
            Op::ThrowTypeError => Err(RuntimeError::TypeError(
                "'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions".to_string(),
            )),
            Op::SymbolDescription => crate::builtins::symbol_description(this),
            Op::MapSize => crate::builtins::map_size(this),
            Op::SetSize => crate::builtins::set_size(this),
            Op::TaLength => crate::builtins::typedarray::ta_length(this),
            Op::TaByteLength => crate::builtins::typedarray::ta_byte_length(this),
            Op::TaByteOffset => crate::builtins::typedarray::ta_byte_offset(this),
            Op::TaBuffer => crate::builtins::typedarray::ta_buffer(this),
            Op::AbByteLength => crate::builtins::typedarray::ab_byte_length(this),
            Op::DvBuffer => crate::builtins::typedarray::dv_buffer(this),
            Op::DvByteLength => crate::builtins::typedarray::dv_byte_length(this),
            Op::DvByteOffset => crate::builtins::typedarray::dv_byte_offset(this),
            Op::Species => Ok(this.clone()),
            Op::ArrayIteratorFactory => self.make_iterator(this.clone(), module),
            Op::TaToStringTag => crate::builtins::typedarray::ta_to_string_tag(this),
            Op::StringCtor => {
                let primitive = self.to_primitive(&args[0], "string", module)?;
                Ok(Value::string(&primitive.to_js_string()))
            }
            Op::ProxyCtor => Err(RuntimeError::TypeError(
                "Constructor Proxy requires 'new'".to_string(),
            )),
            Op::RegExpMatch | Op::RegExpReplace | Op::RegExpSearch | Op::RegExpSplit => {
                let symbol = match op {
                    Op::RegExpMatch => "match",
                    Op::RegExpReplace => "replace",
                    Op::RegExpSearch => "search",
                    _ => "split",
                };
                self.regexp_symbol_method(symbol, this, args, module)
            }
            Op::FunctionCall | Op::FunctionApply => {
                if !this.is_callable() {
                    return Err(RuntimeError::TypeError(format!(
                        "Function.prototype.{} called on non-callable",
                        if op == Op::FunctionCall { "call" } else { "apply" }
                    )));
                }
                self.check_class_ctor(this)?;
                let method = if op == Op::FunctionCall { "call" } else { "apply" };
                let call_args = self.call_or_apply_args(method, args, module)?;
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                self.invoke(this, this_arg, &call_args, module)
            }
            Op::FunctionBind => {
                if !this.is_callable() {
                    return Err(RuntimeError::TypeError(
                        "Function.prototype.bind called on non-callable".to_string(),
                    ));
                }
                let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
                let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
                self.make_bound(this.clone(), this_arg, bound_args, module)
            }
            Op::ObjectDefineGetter | Op::ObjectDefineSetter => {
                self.object_accessor_method(
                    this,
                    if op == Op::ObjectDefineGetter { "__defineGetter__" } else { "__defineSetter__" },
                    args,
                    module,
                )
            }
            Op::ObjectLookupGetter | Op::ObjectLookupSetter => {
                self.object_accessor_method(
                    this,
                    if op == Op::ObjectLookupGetter { "__lookupGetter__" } else { "__lookupSetter__" },
                    args,
                    module,
                )
            }
        }
    }

    /// `String.prototype` 方法的唯一 VM 入口：协议方法先走 `@@match` / `@@replace`
    /// 等；其余方法在这里做 `ToString(this)` 与参数 `ToPrimitive`（都会跑用户
    /// 代码），再把字符串交给纯内建层。
    pub(crate) fn string_proto_host(
        &mut self,
        op: StringOp,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let method = op.as_str();
        if op.uses_protocol() {
            if let Some(result) = self.try_string_callback_method(this, method, args, module)? {
                return Ok(result);
            }
            if let Some(result) = self.try_match_all_method(this, method, args, module)? {
                return Ok(result);
            }
        }
        if op == StringOp::Split {
            return self.string_prototype_split(this, args, module);
        }
        crate::builtins::require_object_coercible(this)?;
        let text = if matches!(op, StringOp::ToString | StringOp::ValueOf) {
            crate::builtins::require_string(this)?
        } else {
            let primitive = match this {
                Value::Object(_) | Value::Function(_) => {
                    self.to_primitive(this, "string", module)?
                }
                other => other.clone(),
            };
            crate::builtins::to_string_throwing(&primitive)?
        };
        let coerced = self.coerce_string_method_args(method, args, module)?;
        let dispatch_args: &[Value] = match &coerced {
            Some(values) => values,
            None => args,
        };
        crate::builtins::call_prototype_method(
            &mut self.heap,
            &Value::string(&text),
            method,
            dispatch_args,
        )
    }

    pub(crate) fn string_prototype_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // `split` 的步骤顺序和别的方法不一样：`@@split` 协议在
        // `ToString(this)` **之前**，`ToUint32(limit)` 在 `ToString(separator)`
        // 之前 —— 三步都可能跑用户代码，只能在这里做。
        if method == "split" {
            return self.string_prototype_split(this, args, module);
        }
        // `RequireObjectCoercible(this)`：`String.prototype.trim.call(null)`
        // 是 TypeError，不能因为 `ToString(null)` 恰好是 "null" 就放过去。
        crate::builtins::require_object_coercible(this)?;
        // `toString` / `valueOf` 例外：它们要求接收者**本身**是字符串
        // （21.1.3.26 / 21.1.3.27），不能把 `123` 悄悄转成 `"123"`。
        let text = if matches!(method, "toString" | "valueOf") {
            crate::builtins::require_string(this)?
        } else {
            let primitive = match this {
                Value::Object(_) | Value::Function(_) => {
                    self.to_primitive(this, "string", module)?
                }
                other => other.clone(),
            };
            crate::builtins::to_string_throwing(&primitive)?
        };
        // 参数也要按规范逐个转换：`ToString(searchString)` /
        // `ToIntegerOrInfinity(position)` 对**对象**参数会跑用户的
        // `toString` / `valueOf` / `@@toPrimitive`，内建层没有 `module`。
        let coerced = self.coerce_string_method_args(method, args, module)?;
        let dispatch_args: &[Value] = match &coerced {
            Some(values) => values,
            None => args,
        };
        crate::builtins::call_prototype_method(
            &mut self.heap,
            &Value::string(&text),
            method,
            dispatch_args,
        )
    }

    /// `Array.prototype` / `%TypedArray%.prototype` 的**索引类**参数转换
    /// （ES 23.1.3.x / 23.2.3.x 的 `ToIntegerOrInfinity` / `ToNumber`）。
    ///
    /// 对象参数要跑 `valueOf` / `toString` / `@@toPrimitive` —— 可以抛（
    /// `copyWithin/return-abrupt-from-start`）也可以给任意值
    /// （`copyWithin/coerced-values-start`）；内建层只有不跑用户代码的
    /// `to_number_throwing`。`Symbol` **不转**：留给内建层抛 TypeError
    /// （`return-abrupt-from-start-is-symbol`）。
    ///
    /// 角色：`n` = 数字（`ToIntegerOrInfinity`），`s` = 字符串，`x` = 不转换。
    pub(crate) fn coerce_index_method_args(
        &mut self,
        family: &str,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Vec<Value>>, RuntimeError> {
        const ARRAY: &[(&str, &str)] = &[
            ("copyWithin", "nnn"),
            // `fill(value, start, end)`：`value` 可以为任意值（含对象）。
            ("fill", "xnn"),
            ("slice", "nn"),
            // `splice(start, deleteCount, ...items)`：items 不转换。
            ("splice", "nn"),
            ("at", "n"),
            ("join", "s"),
            // `indexOf` / `lastIndexOf` / `includes` **故意不在这里转**：规范要求
            // `LengthOfArrayLike` 先读长度、再 `ToIntegerOrInfinity(fromIndex)`，
            // 而长度是内建层读的（`indexOf/15.4.4.14-5-26` 一族按顺序打点，
            // 提前转就是"Step 5 occurred out of order"）。Symbol 仍由内建层的
            // `to_number_throwing` 抛。
        ];
        const TYPED_ARRAY: &[(&str, &str)] = &[
            ("copyWithin", "nnn"),
            ("fill", "xnn"),
            ("slice", "nn"),
            ("subarray", "nn"),
            // `set(array, offset)`：`array` 不转换。
            ("set", "xn"),
            ("at", "n"),
            ("join", "s"),
        ];
        let table = if family == "TypedArray" {
            TYPED_ARRAY
        } else {
            ARRAY
        };
        let Some((_, roles)) = table.iter().find(|(name, _)| *name == method) else {
            return Ok(None);
        };
        let mut out: Option<Vec<Value>> = None;
        for (index, role) in roles.chars().enumerate() {
            if role == 'x' {
                continue;
            }
            let Some(value) = args.get(index) else { break };
            // `Symbol` 是原语：不做转换，内建层的 `to_number_throwing` /
            // `to_string_throwing` 会按规范抛。
            if !matches!(value, Value::Object(_) | Value::Function(_)) {
                continue;
            }
            let hint = if role == 's' { "string" } else { "number" };
            let primitive = self.to_primitive(value, hint, module)?;
            out.get_or_insert_with(|| args.to_vec())[index] = primitive;
        }
        Ok(out)
    }

    /// `String.prototype` 各方法的参数转换（ES 21.1.3.x）。
    ///
    /// 参数是对象时 `ToString` / `ToIntegerOrInfinity` 会跑用户的 `toString` /
    /// `valueOf` / `@@toPrimitive` —— 内建层没有 `module`，`Value::to_number()`
    /// 得到 NaN 就算了，所以转换在这里做，内建层只见到原语。
    ///
    /// 角色表按规范顺序排列（`indexOf` 是 `searchString` 先于 `position`），
    /// 这一点是可观察的：`separator-override-tostring-limit-override-valueof`
    /// 那族用例数的就是调用次序。返回 `None` 表示这个方法没有需要转换的参数
    /// （不分配）。
    fn coerce_string_method_args(
        &mut self,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Vec<Value>>, RuntimeError> {
        // 's' = ToString, 'n' = ToIntegerOrInfinity / ToNumber.
        const ROLES: &[(&str, &[char])] = &[
            ("slice", &['n', 'n']),
            ("substring", &['n', 'n']),
            ("substr", &['n', 'n']),
            ("charAt", &['n']),
            ("charCodeAt", &['n']),
            ("codePointAt", &['n']),
            ("repeat", &['n']),
            ("indexOf", &['s', 'n']),
            ("lastIndexOf", &['s', 'n']),
            ("includes", &['s', 'n']),
            ("startsWith", &['s', 'n']),
            ("endsWith", &['s', 'n']),
            ("localeCompare", &['s']),
            ("normalize", &['s']),
            // `replace` 的两个参数都是 `ToString` —— 但那是在 `@@replace` 协议与
            // "replacer 是函数"两条路都让过之后（见调用点的 `callable` 例外）。
            ("replace", &['s', 's']),
        ];
        let roles: &[char] = match ROLES.iter().find(|(name, _)| *name == method) {
            Some((_, roles)) => roles,
            // `concat` 是变参：每一个都是 `ToString`。
            None if method == "concat" => &[],
            None => return Ok(None),
        };
        let is_concat = method == "concat";
        // 这三个方法的第 1 步是 `IsRegExp(searchString)`，为真就抛 TypeError ——
        // 必须**先于** `ToString`，否则参数会被转成 "/a/" 而检查失效。
        let rejects_regexp = matches!(method, "startsWith" | "endsWith" | "includes");
        let mut out: Option<Vec<Value>> = None;
        for (index, value) in args.iter().enumerate() {
            let role = if is_concat {
                's'
            } else {
                match roles.get(index) {
                    Some(role) => *role,
                    None => break,
                }
            };
            if rejects_regexp && index == 0 && crate::builtins::is_regexp_receiver(value) {
                return Err(RuntimeError::TypeError(
                    "First argument must not be a regular expression".to_string(),
                ));
            }
            // `replace` 的 replaceValue 可以是函数（回调由 `try_string_callback_method`
            // 处理，走到这里说明它不是函数），其余情况才是 `ToString`。
            if method == "replace"
                && index == 1
                && (value.is_callable() || matches!(value, Value::Function(_)))
            {
                continue;
            }
            if !matches!(value, Value::Object(_) | Value::Function(_)) {
                continue;
            }
            let hint = if role == 's' { "string" } else { "number" };
            let primitive = self.to_primitive(value, hint, module)?;
            out.get_or_insert_with(|| args.to_vec())[index] = primitive;
        }
        Ok(out)
    }

    /// `String.prototype.split(separator, limit)` (ES 22.1.3.23).
    ///
    /// 三个转换的顺序**可观察**，而且每一个都可能跑用户代码，所以整段前导只能在
    /// 这里做（内建层没有 `module`）：
    ///
    /// ```text
    /// 1. RequireObjectCoercible(this)
    /// 2. separator 带 @@split  →  Call(splitter, separator, «this, limit»)
    /// 3. S = ToString(this)          ← 只在没有 @@split 时（`this-value-tostring-error`）
    /// 4. lim = ToUint32(limit)       ← 在 ToString(separator) **之前**
    /// 5. R = ToString(separator)     ← 在 lim == 0 检查**之前**
    /// 6. lim == 0 → []
    /// 7. separator === undefined → [S]
    /// ```
    ///
    /// 第 4/5 步的先后由 `limit-touint32-error` / `separator-tostring-error`
    /// 两条用例钉住；第 3 步之前做协议查找由 `this-value-tostring-error` 钉住。
    pub(crate) fn string_prototype_split(
        &mut self,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        crate::builtins::require_object_coercible(this)?;
        let separator = args.first().cloned().unwrap_or(Value::Undefined);
        // `@@split` 只对**对象**分隔符查找（ES 22.1.3.23 第 2 步 "If separator
        // is not Object, then …"）：原语分隔符不会走到原型上的 getter，
        // `cstm-split-on-{string,number,boolean}-primitive` 正是断言这一点 ——
        // 以前对 `","` 也会做 `GetMethod`，把 `String.prototype[Symbol.split]`
        // 上的 getter 触发了一次。
        if matches!(separator, Value::Object(_) | Value::Function(_)) {
            let splitter =
                self.get_member(&separator, &crate::builtins::split_symbol_key(), module)?;
            if splitter.is_callable() || matches!(splitter, Value::Function(_)) {
                let limit = args.get(1).cloned().unwrap_or(Value::Undefined);
                return self.invoke(&splitter, separator, &[this.clone(), limit], module);
            }
        }
        let text = match this {
            Value::Object(_) | Value::Function(_) => {
                self.to_primitive(this, "string", module)?.to_js_string()
            }
            other => other.to_js_string(),
        };
        let lim = match args.get(1) {
            None | Some(Value::Undefined) => u32::MAX,
            Some(value) => {
                let primitive = match value {
                    Value::Object(_) | Value::Function(_) => {
                        self.to_primitive(value, "number", module)?
                    }
                    other => other.clone(),
                };
                crate::builtins::to_uint32_number(crate::builtins::to_number_throwing(&primitive)?)
            }
        };
        let separator_text = match &separator {
            Value::Object(_) | Value::Function(_) => {
                self.to_primitive(&separator, "string", module)?.to_js_string()
            }
            other => other.to_js_string(),
        };
        if lim == 0 {
            return Ok(self.new_array_from_vec(Vec::new()));
        }
        if separator.is_undefined() {
            return Ok(self.new_array_from_vec(vec![Value::string(&text)]));
        }
        let parts: Vec<Value> = crate::builtins::split_string(&text, &separator_text, lim)
            .into_iter()
            .map(|part| Value::string(&part))
            .collect();
        Ok(self.new_array_from_vec(parts))
    }

    /// 一个普通数组值 —— `map` / `filter` / `split` 这类"造结果数组"的收口点。
    fn new_array_from_vec(&mut self, values: Vec<Value>) -> Value {
        Value::Object(self.heap.alloc(crate::vm::gc::Object::Array(
            crate::vm::object::ArrayObject::from_vec(values),
        )))
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
        // ── 第一步：`@@match` / `@@replace` / `@@search` / `@@split` 协议 ──
        //
        // ES 21.1.3.x 的第 2 步：参数上若有对应的方法就**由它接管**，
        // `"ab".replace({ [Symbol.replace](s, r) { … } }, "x")` 与
        // `"ab".replace(/a/, "x")` 走的是同一条路（后者由
        // `RegExp.prototype[@@replace]` 实现）。以前四个方法都是"看参数形状"
        // 直接强转字符串，自定义对象上的 `@@replace` 完全看不到。
        let symbol = match method {
            "match" => Some(crate::builtins::match_symbol_key()),
            "search" => Some(crate::builtins::search_symbol_key()),
            "split" => Some(crate::builtins::split_symbol_key()),
            "replace" | "replaceAll" => Some(crate::builtins::replace_symbol_key()),
            _ => None,
        };
        let Some(symbol_key) = symbol else {
            return Ok(None);
        };
        let search = args.first().cloned().unwrap_or(Value::Undefined);
        // GetMethod only applies to an **object** argument (ES 2024 text:
        // "If regexp is not Object, then […]"). A primitive goes straight to
        // `ToString` / `RegExpCreate`, so a `Symbol.match` getter planted on
        // `String.prototype` must not be consulted for `"a,b".match(",")`.
        if matches!(search, Value::Object(_) | Value::Function(_)) {
            let protocol = self.get_member(&search, &symbol_key, module)?;
            if protocol.is_callable() || matches!(protocol, Value::Function(_)) {
                let mut call_args = vec![this.clone()];
                call_args.extend(args.iter().skip(1).cloned());
                return Ok(Some(self.invoke(&protocol, search, &call_args, module)?));
            }
        }
        // `match` / `search` 在参数没有对应方法时按规范 `RegExpCreate(参数)`
        // 再走一次（`"abc".match("b")` 等价于 `"abc".match(/b/)`）。
        if matches!(method, "match" | "search") {
            let created =
                crate::builtins::regexp_construct_value(&mut self.heap, &[search], None)?;
            let protocol = self.get_member(&created, &symbol_key, module)?;
            let mut call_args = vec![this.clone()];
            call_args.extend(args.iter().skip(1).cloned());
            return Ok(Some(self.invoke(&protocol, created, &call_args, module)?));
        }
        if !matches!(method, "replace" | "replaceAll") {
            return Ok(None);
        }
        // 走到这里说明 `replace` / `replaceAll` 的参数是没有 `@@replace` 的
        // 普通值 → 按字符串字面量替换（`split` 的字面量路径则由 builtins 里
        // 的实现处理）。
        let text = match crate::builtins::string_receiver(this) {
            Ok(text) => text,
            Err(_) => return Ok(None),
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

    /// `RegExp.prototype[@@match/@@replace/@@search/@@split]` (ES 22.2.5.6 …
    /// 22.2.5.11), reached both from `String.prototype.{match,replace,search,split}`
    /// (which dispatch through these symbols) and from an explicit
    /// `re[Symbol.split](s)`.
    ///
    /// `this` is the regexp, `args[0]` the string (`ToString`, which can run
    /// user code). The pure parts are delegated to the shape-based builtins
    /// (`regexp_match` / `regexp_replace` / …, which take the string as the
    /// receiver and the regexp as the first argument); only the callable
    /// replacement needs the VM, because it has to invoke user code per match.
    fn regexp_symbol_method(
        &mut self,
        symbol: &str,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let is_regexp = matches!(this, Value::Object(o)
            if matches!(o.borrow().kind(), crate::vm::property::ObjectKind::RegExp));
        if !is_regexp {
            return Err(RuntimeError::TypeError(format!(
                "RegExp.prototype[Symbol.{symbol}] called on a non-RegExp receiver"
            )));
        }
        let string_arg = args.first().cloned().unwrap_or(Value::Undefined);
        // `ToString(S)` — a user `toString` runs here.
        let text = match &string_arg {
            Value::Object(_) | Value::Function(_) => {
                self.to_primitive(&string_arg, "string", module)?.to_js_string()
            }
            other => other.to_js_string(),
        };
        let receiver = Value::string(&text);
        match symbol {
            "match" => {
                crate::builtins::regexp_match(&mut self.heap, &receiver, std::slice::from_ref(this))
            }
            "search" => {
                crate::builtins::regexp_search(&mut self.heap, &receiver, std::slice::from_ref(this))
            }
            "split" => {
                let mut call_args = vec![this.clone()];
                if let Some(limit) = args.get(1) {
                    call_args.push(limit.clone());
                }
                crate::builtins::regexp_split(&mut self.heap, &receiver, &call_args)
            }
            _ => self.regexp_symbol_replace(this, &text, args.get(1), module),
        }
    }

    /// `RegExp.prototype[@@replace](string, replaceValue)` (ES 22.2.5.8): the
    /// callable-replacement half needs this frame, the string half is
    /// `regexp_replace`'s `$` substitution.
    fn regexp_symbol_replace(
        &mut self,
        this: &Value,
        text: &str,
        replace_value: Option<&Value>,
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let replace_value = replace_value.cloned().unwrap_or(Value::Undefined);
        if !(replace_value.is_callable() || matches!(replace_value, Value::Function(_))) {
            return crate::builtins::regexp_replace(
                &mut self.heap,
                &Value::string(text),
                &[this.clone(), replace_value],
            );
        }
        // 函数式 replacer：逐个匹配回调，参数是
        // (matched, ...captures, position, string)（ES 22.2.5.8 step 14.c）。
        let (matcher, global) = {
            let Value::Object(obj_ref) = this else {
                return Err(RuntimeError::TypeError("not a RegExp".to_string()));
            };
            let borrowed = obj_ref.borrow();
            let re = borrowed
                .as_regexp()
                .ok_or_else(|| RuntimeError::TypeError("not a RegExp".to_string()))?;
            let Some(compiled) = re.compiled.clone() else {
                return Err(RuntimeError::TypeError(format!(
                    "pattern {:?} is not supported by this engine",
                    re.source
                )));
            };
            (compiled, re.flags.contains('g'))
        };
        let _root_replacer = self.root(&replace_value);
        let _root_receiver = self.root(this);
        let mut out = String::new();
        let mut last = 0usize;
        for captures in matcher.captures_iter(text) {
            let whole = captures.get(0).expect("a match implies group 0");
            out.push_str(&text[last..whole.start()]);
            let mut call_args: Vec<Value> = vec![Value::string(whole.as_str())];
            for i in 1..captures.len() {
                call_args.push(match captures.get(i) {
                    Some(m) => Value::string(m.as_str()),
                    None => Value::Undefined,
                });
            }
            call_args.push(Value::Number(whole.start() as f64));
            call_args.push(Value::string(text));
            let replacement = self
                .invoke(&replace_value, Value::Undefined, &call_args, module)?
                .to_js_string();
            out.push_str(&replacement);
            last = whole.end();
            if !global {
                break;
            }
        }
        out.push_str(&text[last..]);
        Ok(Value::string(&out))
    }

    /// `Object.*` / `Reflect.*` 静态方法的**唯一** VM 入口。
    ///
    /// 以前这些步骤（ToPropertyKey、Proxy trap、描述符 `[[Get]]`、Array.length
    /// 的 ES ToNumber）在 `call_native_by_name` 里按名字逐条展开；现在由注册
    /// 时给定的 `name` / `coerce_key` / `desc` 三个事实驱动一次走完，最后落到
    /// 纯 builtin 表。`call_static_method` 仍是纯实现表，不再负责策略。
    pub(crate) fn object_static_vm(
        &mut self,
        name: &'static str,
        coerce_key: bool,
        desc: crate::vm::native::DescKind,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::native::DescKind;

        let converted;
        let args: &[Value] = if coerce_key {
            converted = self.reflect_key_args(args, module)?;
            &converted
        } else {
            args
        };
        if let Some(result) = self.proxy_static_trap(name, args, module)? {
            return Ok(result);
        }
        // `Reflect.has` 的 `[[HasProperty]]` 要走原型链，每一跳都可能是 Proxy。
        if name == "Reflect.has" {
            let target = args.first().cloned().unwrap_or(Value::Undefined);
            let Value::Object(obj_ref) = target else {
                return Err(RuntimeError::TypeError(format!(
                    "Reflect.has called on {}",
                    target.type_of()
                )));
            };
            let key = crate::builtins::reflect::to_key(
                &mut self.heap,
                args.get(1).unwrap_or(&Value::Undefined),
            );
            return Ok(Value::Bool(
                self.object_has_property(obj_ref, &key, module)?,
            ));
        }
        match desc {
            // `Reflect.defineProperty` 的描述符由 `reflect_static` 在 VM 里处理，
            // 只有 `Object.defineProperty` 走这里再落到 builtin 表。
            DescKind::DefineProperty if name == "Object.defineProperty" && args.len() >= 3 => {
                let descriptor = self.to_property_descriptor(&args[2], module)?;
                if matches!(&args[1], Value::String(s) if s.as_str() == "length") {
                    self.normalize_array_length_value(&args[0], &descriptor, module)?;
                }
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), args[1].clone(), descriptor],
                    &mut self.heap,
                );
            }
            DescKind::DefineProperties if args.len() >= 2 => {
                let props = self.to_property_descriptors(&args[1], module)?;
                if desc == DescKind::DefineProperties {
                    let length_key = PropertyKey::intern(&mut self.heap, "length");
                    let length_desc = if let Value::Object(props_ref) = &props {
                        props_ref
                            .borrow()
                            .property_get(&length_key)
                            .map(|d| d.value)
                    } else {
                        None
                    };
                    if let Some(descriptor) = length_desc {
                        self.normalize_array_length_value(&args[0], &descriptor, module)?;
                    }
                }
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), props],
                    &mut self.heap,
                );
            }
            DescKind::Create if args.len() >= 2 && !args[1].is_undefined() => {
                let props = self.to_property_descriptors(&args[1], module)?;
                return crate::builtins::call_static_method(
                    name,
                    &[args[0].clone(), props],
                    &mut self.heap,
                );
            }
            DescKind::None
            | DescKind::DefineProperty
            | DescKind::DefineProperties
            | DescKind::Create => {}
        }
        if let Some(result) = self.reflect_static(name, args, module)? {
            return Ok(result);
        }
        crate::builtins::call_static_method(name, args, &mut self.heap)
    }

    /// `Proxy.revocable(t, h)`：`revoke` 必须记住撤销的是哪个 proxy，内建函数
    /// 只有名字，所以这个闭包由 VM 铸造（`make_revoke`）。
    pub(crate) fn proxy_revocable_host(
        &mut self,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        let target = self.as_object_value(&args.first().cloned().unwrap_or(Value::Undefined));
        let handler = self.as_object_value(&args.get(1).cloned().unwrap_or(Value::Undefined));
        let proxy = crate::builtins::proxy::proxy_construct(&mut self.heap, target, handler)?;
        let revoke = self.make_revoke(proxy.clone());
        let mut result = crate::vm::object::OrdinaryObject::new();
        result.set_prototype(Some(self.builtins.object_prototype));
        for (field, value) in [("proxy", proxy), ("revoke", revoke)] {
            let key = PropertyKey::intern(&mut self.heap, field);
            let _ = result.define_property(
                &mut self.heap,
                key,
                PropertyDescriptor::data_descriptor(value),
            );
        }
        Ok(Value::Object(self.heap.alloc(crate::vm::gc::Object::Ordinary(result))))
    }

    /// test262 宿主 hook：`$262.detachArrayBuffer(buf)`（ES 25.1.2.1 的内部
    /// DetachArrayBuffer）。默认引擎不可见，只有 runner 装了 `$262` 才有。
    pub(crate) fn detach_array_buffer_host(
        &mut self,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        if let Some(Value::Object(obj_ref)) = args.first() {
            if let Some(buffer) = obj_ref.borrow_mut().as_array_buffer_mut() {
                buffer.detach();
            }
        }
        Ok(Value::Undefined)
    }

    /// `Array.prototype` 方法的**唯一** VM 入口。
    ///
    /// 以前这段逻辑在 `call_native_by_name` 的 `"Array.prototype."` 前缀分支里，
    /// 每次调用都要 strip 字符串再走一遍；现在算法由注册期钉在函数对象上的
    /// `ArrayOp` 决定，`ToObject` / 参数强转 / 三条 VM 路径 / 内建兜底全在这里。
    pub(crate) fn array_proto_host(
        &mut self,
        op: ArrayOp,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let method = op.as_str();
        // `ToObject(this value)`：原语接收者先装箱（`indexOf.call('ab','b')`）。
        let receiver = match this {
            Value::Object(_) | Value::Function(_) => this.clone(),
            other => crate::builtins::to_object(&mut self.heap, other)?,
        };
        // `Get(O, "join")` 后调用它，可跑用户代码；否则回 Object.prototype.toString。
        if op == ArrayOp::ToString {
            let join_key = PropertyKey::intern(&mut self.heap, "join");
            let join = self.get_member(&receiver, &join_key, module)?;
            if join.is_callable() || matches!(join, Value::Function(_)) {
                return self.invoke(&join, receiver.clone(), &[], module);
            }
            return crate::builtins::object_prototype_to_string(&receiver, &[]);
        }
        // `concat` 的 species / spreadable 检查要在 VM 里做（Proxy / 用户 valueOf）。
        if op == ArrayOp::Concat
            && !crate::vm::receiver_is_string(&receiver)
            && self.receiver_concat_like(&receiver)
        {
            return self.array_concat_vm(&receiver, args, module);
        }
        // 顺序与规范一致（也与原先 `CallMethod` 的早段拦截一致）：回调族和读族
        // 自己处理参数强转，宿主不能在它们之前先把 valueOf 跑一遍 —— 否则
        // `copyWithin(0, {valueOf(){…}})` 里的 valueOf 会被多调用一次。
        if let Some(result) = self.try_array_callback_method(&receiver, op, args, module)? {
            return Ok(result);
        }
        if let Some(result) = self.try_array_read_method(&receiver, method, args, module)? {
            return Ok(result);
        }
        // species 族（slice / splice / concat / with / toSpliced）在这里才做
        // `ToIntegerOrInfinity` / `ToString`（对象参数要跑 valueOf / @@toPrimitive）。
        let coerced = self.coerce_index_method_args("Array", method, args, module)?;
        let args: &[Value] = match &coerced {
            Some(values) => values,
            None => args,
        };
        if let Some(result) = self.try_array_species_method(&receiver, method, args, module)? {
            return Ok(result);
        }
        crate::builtins::call_prototype_method_owned(
            "Array",
            &mut self.heap,
            &receiver,
            method,
            args,
        )
    }

    /// `Array.prototype.entries/keys/values`：先 `ToObject(this)`，再建一个
    /// 惰性迭代器（每步重新读 length 与下标）。同名方法在 Map/Set/TypedArray
    /// 上是**不同函数对象**，由各自的 slot 决定，不靠接收者形状猜。
    pub(crate) fn array_iterator_host(
        &mut self,
        kind: &'static str,
        this: &Value,
        _module: &Module,
    ) -> Result<Value, RuntimeError> {
        crate::builtins::require_object_coercible(this)?;
        let target = match this {
            Value::Object(_) | Value::Function(_) => this.clone(),
            other => crate::builtins::to_object(&mut self.heap, other)?,
        };
        self.make_array_iterator(&target, kind)
    }

    /// `%TypedArray%.prototype.entries/keys/values`：brand check + detached
    /// check 都留在实现里，同名方法在 Array/Map/Set 上是别的函数对象。
    pub(crate) fn typed_array_iterator_host(
        &mut self,
        kind: &'static str,
        this: &Value,
    ) -> Result<Value, RuntimeError> {
        let detached = match this {
            Value::Object(o) => o
                .borrow()
                .as_typed_array()
                .is_some_and(|ta| ta.is_detached()),
            _ => false,
        };
        if detached {
            return Err(RuntimeError::TypeError(format!(
                "%TypedArray%.prototype.{kind} called on a detached buffer"
            )));
        }
        self.make_typed_array_iterator(this, kind)
    }

    /// `String.fromCharCode` 的 VM 版本：每个参数先走真 `ToNumber`（对象参数
    /// 要跑 `valueOf` / `toString`，Symbol 是 TypeError），再把结果交给内建层
    /// 做 `ToUint16`。内建层的 `to_number_throwing` 不会跑用户代码。
    pub(crate) fn string_from_char_code_vm(
        &mut self,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let mut converted = Vec::with_capacity(args.len());
        for arg in args {
            let primitive = match arg {
                Value::Object(_) | Value::Function(_) => {
                    self.to_primitive(arg, "number", module)?
                }
                other => other.clone(),
            };
            converted.push(primitive);
        }
        crate::builtins::string_from_char_code(&converted)
    }

    /// `String.fromCodePoint` 的 VM 版本：与上面同样要先跑真 `ToNumber`。
    pub(crate) fn string_from_code_point_vm(
        &mut self,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let mut converted = Vec::with_capacity(args.len());
        for arg in args {
            let primitive = match arg {
                Value::Object(_) | Value::Function(_) => {
                    self.to_primitive(arg, "number", module)?
                }
                other => other.clone(),
            };
            converted.push(primitive);
        }
        crate::builtins::string_from_code_point(&converted)
    }

    /// `String.raw(template, ...substitutions)`（ES 22.1.2.3）。
    ///
    /// 结果是 `template.raw` 的各段依次与替换值交错拼接：
    /// `raw[0] + sub0 + raw[1] + sub1 + …`。注意用的是 **raw**（未转义的原始串），
    /// 这正是它存在的意义 —— `` String.raw`\n` `` 得到字面的反斜杠 n。
    pub(crate) fn string_raw(&mut self, args: &[Value], module: &Module) -> Result<Value, RuntimeError> {
        let template = args.first().cloned().unwrap_or(Value::Undefined);
        // 1. `ToObject(template)`：非对象（含 undefined / null）是 TypeError。
        let cooked = crate::builtins::to_object(&mut self.heap, &template)?;
        // 2. `Get(cooked, "raw")` 再 `ToObject` —— raw 常常是**字符串**
        //    （`String.raw({raw: "abc"})`），它的索引属性就是各段。
        let raw_key = PropertyKey::well_known("raw").expect("well-known name");
        let raw = self.get_member(&cooked, &raw_key, module)?;
        let raw = crate::builtins::to_object(&mut self.heap, &raw)?;
        // 3. `ToLength(Get(raw, "length"))`：`ToNumber` 会跑用户 `valueOf`，
        //    Symbol 长度是 TypeError（`template-length-is-symbol-throws`）。
        let length_key = PropertyKey::well_known("length").expect("well-known name");
        let length_value = self.get_member(&raw, &length_key, module)?;
        let length_value = match &length_value {
            Value::Object(_) | Value::Function(_) => {
                self.to_primitive(&length_value, "number", module)?
            }
            other => other.clone(),
        };
        // 上限与 `array_like_length` 一致：只是声称 `length = 2**53-1` 的接收者
        // 不该把一次调用变成无限循环/无界分配。
        let segments = crate::builtins::to_length_throwing(&length_value)?.min(1 << 24);
        if segments == 0 {
            return Ok(Value::string(""));
        }
        let substitutions = args.get(1..).unwrap_or(&[]);
        let mut out = String::new();
        for index in 0..segments {
            let key = PropertyKey::element(&mut self.heap, index);
            let segment = self.get_member(&raw, &key, module)?;
            // `ToString(Get(raw, nextKey))`：Symbol 段是 TypeError
            // （`nextkey-is-symbol-throws`）。
            out.push_str(&crate::builtins::to_string_throwing(&segment)?);
            // 9.e：**最后一段之后立刻返回** —— 末尾不再插替换值，多余的替换值
            // 被忽略（`substitutions-are-limited-to-template-raw-length`：三个替换值
            // 配三段，第三段的 `toString` 根本不该被调用）。
            if index + 1 == segments {
                return Ok(Value::string(&out));
            }
            if let Some(sub) = substitutions.get(index as usize) {
                out.push_str(&crate::builtins::to_string_throwing(sub)?);
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
            // `%TypedArray%.prototype.toLocaleString` 要逐个 Invoke 元素的同名方法
            "toLocaleString",
            // `set(source, offset)` 的 source 是类数组时，`length` / 每个下标 /
            // 元素 ToNumber 都是可观察操作（getter 与 valueOf 要跑、Symbol 要抛）。
            "set",
            // `join(separator)` 的 `ToString` 对 Symbol 要抛，内建层用
            // `to_js_string()` 静默产生 "Symbol(s)"。
            "join",
            // `slice` / `subarray` 的结果要走 `TypedArraySpeciesCreate`，
            // 而且 begin/end 的 ToInteger 是可观察操作，内建层做不到。
            "slice",
            "subarray",
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
        if method == "slice" || method == "subarray" {
            let relative = |v: i64| -> usize {
                if v < 0 {
                    (len as i64 + v).max(0) as usize
                } else {
                    (v as usize).min(len)
                }
            };
            let begin = self.to_integer_or_infinity_arg(args.first(), 0, module)?;
            let end = self.to_integer_or_infinity_arg(args.get(1), len as i64, module)?;
            let begin = relative(begin);
            let end = relative(end);
            let count = end.saturating_sub(begin);
            if method == "slice" {
                let result = self.typed_array_species_create(receiver, count, module)?;
                self.write_typed_array_elements(&result, &elements[begin..end]);
                return Ok(Some(result));
            }
            // `subarray` 用同一个 buffer 建一个视图：`SpeciesCreate(O, 0)` 后
            // 以 (buffer, byteOffset, length) 构造（ES 23.2.3.26）。
            let (buffer, byte_offset, kind_size) = {
                let Value::Object(o) = receiver else { unreachable!() };
                let borrowed = o.borrow();
                let ta = borrowed.as_typed_array().expect("checked above");
                (ta.buffer, ta.byte_offset, ta.kind.bytes_per_element())
            };
            let result = self.typed_array_species_create_with(
                receiver,
                &[
                    Value::Object(buffer),
                    Value::Number((byte_offset + begin * kind_size) as f64),
                    Value::Number(count as f64),
                ],
                module,
            )?;
            return Ok(Some(result));
        }
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
            // ES 23.2.3.26：分隔符与元素都走 `ToString`（Symbol 抛 TypeError）。
            "join" => {
                let coerced = self.coerce_index_method_args("TypedArray", "join", args, module)?;
                let join_args = coerced.as_deref().unwrap_or(args);
                let sep = match join_args.first() {
                    None | Some(Value::Undefined) => ",".to_string(),
                    Some(v) => crate::builtins::to_string_throwing(v)?,
                };
                let mut out = String::new();
                for (i, e) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&sep);
                    }
                    if e.is_undefined() || matches!(e, Value::Null) {
                        continue;
                    }
                    out.push_str(&crate::builtins::to_string_throwing(e)?);
                }
                return Ok(Some(Value::string(&out)));
            }
            // ES 23.2.3.24 `%TypedArray%.prototype.set(source, offset)` 的
            // 类数组分支：`Get(source, "length")` → `ToInteger(offset)` →
            // 逐下标 `Get` + `ToNumber` → `Set(O, …, true)`。内建层用
            // `source_numbers()` 只查自有描述符，getter / valueOf 全看不到。
            "set" => {
                let source = args.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(source, Value::Object(_) | Value::Function(_)) {
                    return Err(RuntimeError::TypeError(
                        "TypedArray.prototype.set: source is not array-like".to_string(),
                    ));
                }
                // 先读 source 的 length（`array_like_length_raw` 走 `[[Get]]`，
                // Symbol 长度是 TypeError），再转 offset —— 每次转换都跑用户代码。
                let src_len = self.array_like_length_raw(&source, module)?.min(1 << 24);
                let offset = self.to_integer_or_infinity_arg(args.get(1), 0, module)?;
                if offset < 0 {
                    return Err(RuntimeError::RangeError("Invalid offset".to_string()));
                }
                // ES 第 4/7 步：offset 的转换跑完用户代码后要复查 detachment。
                let detached = |vm: &Self, value: &Value| -> bool {
                    match value {
                        Value::Object(o) => o
                            .borrow()
                            .as_typed_array()
                            .is_some_and(|ta| ta.is_detached()),
                        _ => false,
                    }
                };
                if detached(self, &source) || detached(self, receiver) {
                    return Err(RuntimeError::TypeError(
                        "set: a viewed ArrayBuffer was detached".to_string(),
                    ));
                }
                let mut values = Vec::with_capacity(src_len as usize);
                let mut keep = self.roots();
                for k in 0..src_len {
                    let key = PropertyKey::element(&mut self.heap, k);
                    let value = self.get_member(&source, &key, module)?;
                    keep.push(&value);
                    let primitive = match &value {
                        Value::Object(_) | Value::Function(_) => {
                            self.to_primitive(&value, "number", module)?
                        }
                        other => other.clone(),
                    };
                    values.push(crate::builtins::to_number_throwing(&primitive)?);
                }
                let Value::Object(target_ref) = receiver else {
                    return Ok(None);
                };
                {
                    let ta = target_ref.borrow();
                    let ta = ta.as_typed_array().expect("checked by caller");
                    if (offset as u64).saturating_add(src_len) > ta.length as u64 {
                        return Err(RuntimeError::RangeError(
                            "Source is too large".to_string(),
                        ));
                    }
                }
                let mut ta = target_ref.borrow_mut();
                let ta = ta.as_typed_array_mut().expect("checked by caller");
                for (k, value) in values.into_iter().enumerate() {
                    ta.set_element(offset as usize + k, Value::Number(value));
                }
                return Ok(Some(Value::Undefined));
            }
            // ES 23.2.3.28（同 Array.prototype.toLocaleString）：逐元素
            // `Invoke(element, "toLocaleString")` 再 `ToString`。内建层那份
            // 只做 `to_js_string()`，用户的 Number.prototype.toLocaleString
            // 抛错看不到（6 条 return-abrupt-from-* 用例）。
            "toLocaleString" => {
                if elements.is_empty() {
                    return Ok(Some(Value::string("")));
                }
                let locale_key = PropertyKey::intern(&mut self.heap, "toLocaleString");
                let mut out = String::new();
                for (i, e) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if e.is_undefined() || matches!(e, Value::Null) {
                        continue;
                    }
                    let method_value = self.get_member(e, &locale_key, module)?;
                    let piece = self.invoke(&method_value, e.clone(), &[], module)?;
                    out.push_str(&crate::builtins::to_string_throwing(&piece)?);
                }
                return Ok(Some(Value::string(&out)));
            }
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
        self.typed_array_species_create_with(receiver, &[Value::Number(len as f64)], module)
    }

    /// 与上面同源，但构造参数由调用方给定（`subarray` 要传 buffer/byteOffset/length）。
    fn typed_array_species_create_with(
        &mut self,
        receiver: &Value,
        ctor_args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        // ES 9.4.2.3 `SpeciesConstructor(O, defaultConstructor)`：
        // `constructor` 缺失（包括被 getter 改成 undefined）时**落到默认构造器**
        // —— 不是 `Construct(undefined)`（`callbackfn-called-before-ctor`、
        // `speciesctor-get-ctor-inherited`）。默认构造器按视图种类的内建名取。
        let default_ctor = match receiver {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .as_typed_array()
                .and_then(|ta| self.global(ta.kind.name()))
                .unwrap_or(Value::Undefined),
            _ => Value::Undefined,
        };
        let ctor = self.get_member(
            receiver,
            &PropertyKey::well_known("constructor").expect("well-known name"),
            module,
        )?;
        // 构造器在本引擎里是 `Value::Function`，不是 `Value::Object` —— 以前的
        // `ctor.is_object()` 把所有真构造器都当"没有 @@species"，于是
        // `map` / `filter` / `slice` 的 species 一整族用例全失败。
        let species = if ctor.is_undefined() || matches!(ctor, Value::Null) {
            default_ctor
        } else if matches!(ctor, Value::Object(_) | Value::Function(_)) {
            let s = self.get_member(&ctor, &crate::builtins::species_symbol_key(), module)?;
            if matches!(s, Value::Undefined | Value::Null) {
                default_ctor
            } else {
                s
            }
        } else {
            ctor.clone()
        };
        self.construct(&species, ctor_args, module)
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
                // CreateDataPropertyOrThrow，不是 [[Set]]：species 目标把索引
                // 定义成不可配置时是一次 TypeError，定义成可配置的只读属性时
                // 则**重新定义**成功（map/target-array-with-non-writable-property）。
                for (i, v) in vals.iter().enumerate() {
                    self.create_data_property_or_throw(&target, i, v.clone(), module)?;
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
        method: crate::vm::native::ArrayOp,
        args: &[Value],
        module: &Module,
    ) -> Result<Option<Value>, RuntimeError> {
        // 调用方先用 `ArrayOp::from_name` 把方法名换成类型；没换成的名字
        // 根本不归这一族，直接由调用方继续走别的分派。
        // `Array.prototype.map.call(null, …)` and friends: ToObject raises.
        if matches!(receiver, Value::Undefined | Value::Null) {
            return Err(RuntimeError::TypeError(format!(
                "Array.prototype.{} called on null or undefined",
                method.as_str()
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

        // ES 2020 把 `sort` 的比较器检查放在**第一步**（23.1.3.30 第 1 步）：
        // `[].sort.call({ get length() { throw … } }, null)` 必须是 TypeError，
        // 而不是长度访问器抛出的那个错误（`comparefn-nonfunction-call-throws`）。
        // 这一步因此要排在 `LengthOfArrayLike` 之前。
        if method == crate::vm::native::ArrayOp::Sort {
            if let Some(cmp) = args.first() {
                if !cmp.is_undefined()
                    && !(cmp.is_callable() || matches!(cmp, Value::Function(_)))
                {
                    return Err(RuntimeError::TypeError(
                        "The comparison function must be either a function or undefined"
                            .to_string(),
                    ));
                }
            }
        }
        // 只取 length：元素在每一步重新 `HasProperty` + `[[Get]]`
        // （`array_live_at`），物化整张清单会让访问器元素被调用两次。
        // 一次读**真实长度**（u64），再派生出截断后的迭代预算 —— 读两次会让
        // 计数的 `length` getter 多看到一次。
        let len_raw = match self.array_like_length_raw(receiver, module) {
            Ok(len) => len,
            // 接收者**不是**类数组（`filter.call(undefined, …)`、字符串包装）：
            // 让调用方继续走普通分派，由那里报 TypeError。
            Err(RuntimeError::TypeError(_)) if !self.receiver_is_array_like(receiver) => {
                return Ok(None);
            }
            // 是类数组，但 `length` 转换失败 —— `ToLength(Symbol)` 就是 TypeError，
            // 不能吞掉（`copyWithin/return-abrupt-from-this-length-as-symbol`）。
            Err(err) => return Err(err),
        };
        // 会 `ArraySpeciesCreate(O, len)` 的方法：`len > 2**32-1` 是 RangeError
        // （`map/15.4.4.19-3-8`：`{0: 9, length: Infinity}`）。
        if method.uses_species() && len_raw > 0xFFFF_FFFF {
            return Err(RuntimeError::RangeError("Invalid array length".to_string()));
        }
        let len = len_raw.min(1 << 24) as usize;

        // `indexOf` / `lastIndexOf` take a search element, not a callback, and
        // must not hit the callable check below. Strings keep their own
        // substring semantics (`"abcab".indexOf("ab")` is 0, not a per-char
        // scan), so a string receiver falls through to the string builtins.
        if matches!(
            method,
            crate::vm::native::ArrayOp::IndexOf | crate::vm::native::ArrayOp::LastIndexOf
        ) {
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
            // 真实长度（不截到 1<<24）：`a[4294967294] = 2; a.lastIndexOf(2)`
            // 要答 4294967294。这两个方法的扫描从 `fromIndex` 开始而不是从 0，
            // 所以大长度本身不贵（`15.4.4.15-8-9` 一族把匹配放在末尾）。
            return self
                .array_index_of_live(&target, len_raw.min(usize::MAX as u64) as usize, method.as_str(), args, module)
                .map(Some);
        }

        // `copyWithin(target, start, end)`（ES 23.1.3.3）：逐元素
        // `HasProperty` → `[[Get]]` → `Set(..., true)`，空位则
        // `DeletePropertyOrThrow`。四种操作都可观察（Proxy / 访问器 /
        // 非配置属性），而内建层的 `array_copy_within` 只认密集存储。
        if method == crate::vm::native::ArrayOp::CopyWithin {
            if !self.receiver_is_array_like(receiver) {
                return Ok(None);
            }
            let target = match receiver {
                Value::Object(_) | Value::Function(_) => receiver.clone(),
                other => crate::builtins::to_object(&mut self.heap, other)?,
            };
            return self
                .array_copy_within_live(&target, len, args, module)
                .map(Some);
        }

        // `toLocaleString`（ES 23.1.3.32）：逐个元素 `Invoke(element,
        // "toLocaleString")`，`undefined` / `null` 贡献空串，其余用 `,` 连起来。
        // 元素自己的方法可能抛、也可能返回任意值，内建层做不到。
        // `toLocaleString` 是**共享名**：`Boolean.prototype.toLocaleString` /
        // `Number.prototype.toLocaleString` 也走这条分派。只有类数组接收者
        // 才归数组口径（`[true].toLocaleString()` 里的 `true` 必须落到
        // `Boolean.prototype` 上，否则得到空串）。
        if method == crate::vm::native::ArrayOp::ToLocaleString {
            if !self.receiver_is_array_like(receiver) {
                return Ok(None);
            }
            if len == 0 {
                return Ok(Some(Value::string("")));
            }
            let separator = ",";
            let locale_key = PropertyKey::intern(&mut self.heap, "toLocaleString");
            let mut out = String::new();
            for k in 0..len {
                if k > 0 {
                    out.push_str(separator);
                }
                let key = PropertyKey::element(&mut self.heap, k as u64);
                let element = self.get_member(receiver, &key, module)?;
                if element.is_undefined() || matches!(element, Value::Null) {
                    continue;
                }
                let method_value = self.get_member(&element, &locale_key, module)?;
                let piece = self.invoke(&method_value, element, &[], module)?;
                out.push_str(&crate::builtins::to_string_throwing(&piece)?);
            }
            return Ok(Some(Value::string(&out)));
        }

        // `fill(value, start, end)`（ES 23.1.3.7）：逐下标 `Set(O, k, value, true)`。
        if method == crate::vm::native::ArrayOp::Fill {
            if !self.receiver_is_array_like(receiver) {
                return Ok(None);
            }
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            // 真实长度：`fill` 的**跨度**可能很小但下标很大
            // （`fill.call({length: 2**53-1}, v, 2**53-4, 2**53-1)` 只写三个）。
            let len_i = len_raw as i64;
            let start = self.relative_index_arg(args.get(1), 0, len_i, module)?;
            let end = match args.get(2) {
                None | Some(Value::Undefined) => len_i,
                Some(_) => self.relative_index_arg(args.get(2), len_i, len_i, module)?,
            };
            // 跨度过大就拒绝，而不是静默截断或真写几十亿个下标
            // （`absurd_lengths_do_not_allocate` 要求 RangeError）。
            if end.saturating_sub(start) > (1 << 24) {
                return Err(RuntimeError::RangeError(
                    "fill: the range is too large".to_string(),
                ));
            }
            for k in start..end {
                let key = PropertyKey::element(&mut self.heap, k as u64);
                self.set_member(receiver, key, value.clone(), module)?;
            }
            return Ok(Some(receiver.clone()));
        }

        // 栈族：`push` / `pop` / `shift` / `unshift`（ES 23.1.3.20 / .17 / .22 / .29）。
        if matches!(
            method,
            crate::vm::native::ArrayOp::Push
                | crate::vm::native::ArrayOp::Pop
                | crate::vm::native::ArrayOp::Shift
                | crate::vm::native::ArrayOp::Unshift
        ) {
            if !self.receiver_is_array_like(receiver) {
                return Ok(None);
            }
            let target = match receiver {
                Value::Object(_) | Value::Function(_) => receiver.clone(),
                other => crate::builtins::to_object(&mut self.heap, other)?,
            };
            // 真实长度，**不截到 1<<24**：`obj.length = 4294967296; obj.push(…)`
            // 必须返回 4294967299（`S15.4.4.7_A4_T1`）。搬运循环另有上限，
            // 见 `array_stack_method` 里的 `iter_len`。
            let raw_len = self.array_like_length_raw(&target, module)?;
            return self
                .array_stack_method(&target, method.as_str(), raw_len, args, module)
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
            ArrayOp::Sort => {
                // ES 2020 `Array.prototype.sort`（tc39/ecma262#1585）的四步：
                //
                // 1. `len` 已在上面取好；
                // 2. `SortIndexedProperties`：逐个 `HasProperty` + `[[Get]]`，
                //    **空位跳过**（于是 `Object.prototype[2] = 4` 会让一个空位
                //    变成有值的元素，`precise-prototype-element`）；
                // 3. 排序：默认按 `ToString` 序，`undefined` 一律排最后；
                // 4. 写回 `0..count`，尾部 `count..len` 走 `DeletePropertyOrThrow`。
                //
                // 以前的实现把整张清单物化成 `undefined`（空位与 undefined 不分）、
                // 再用 `replace_elements` 直接改密集存储，于是访问器元素
                // （`precise-getter-*`）、原型元素、写回失败的 TypeError 全都看不到。
                let mut items: Vec<Value> = Vec::with_capacity(len);
                let mut keep = self.roots();
                for k in 0..len {
                    if let Some(value) = self.array_live_at(receiver, k, module)? {
                        keep.push(&value);
                        items.push(value);
                    }
                }
                let count = items.len();
                if count > 1 {
                    // 自底向上归并：合并循环里才能 `invoke` / 跑 `ToString`
                    // （`sort_by` 的闭包拿不到 `&mut self`），且归并天然稳定。
                    let mut buf = items.clone();
                    let mut width = 1usize;
                    while width < count {
                        let mut i = 0usize;
                        while i < count {
                            let mid = (i + width).min(count);
                            let end = (i + 2 * width).min(count);
                            let (mut a, mut b, mut k) = (i, mid, i);
                            while a < mid && b < end {
                                // `SortCompare(items[b], items[a]) < 0` ⇒ b 先。
                                if self.sort_compare(&items[b], &items[a], &callback, module)? {
                                    buf[k] = items[b].clone();
                                    b += 1;
                                } else {
                                    buf[k] = items[a].clone();
                                    a += 1;
                                }
                                k += 1;
                            }
                            while a < mid {
                                buf[k] = items[a].clone();
                                a += 1;
                                k += 1;
                            }
                            while b < end {
                                buf[k] = items[b].clone();
                                b += 1;
                                k += 1;
                            }
                            i = end;
                        }
                        std::mem::swap(&mut items, &mut buf);
                        width *= 2;
                    }
                }
                // 写回：`Set(..., throw=true)` 与 `DeletePropertyOrThrow`。
                for (j, value) in items.iter().enumerate() {
                    let key = PropertyKey::element(&mut self.heap, j as u64);
                    self.set_member(receiver, key, value.clone(), module)?;
                }
                for j in count..len {
                    let key = PropertyKey::element(&mut self.heap, j as u64);
                    if !self.delete_member(receiver, &key, module)? {
                        return Err(RuntimeError::TypeError(format!(
                            "Cannot delete property '{j}'"
                        )));
                    }
                }
                Ok(Some(receiver.clone()))
            }
            // 只有"必须给回调"的方法才在这里拒；`toSorted` / `sort` 的比较器、
            // `toReversed` / `with` / `indexOf` 等都不需要（此前写成通配 `_`，
            // 于是 `[3,1,2].toSorted()` 报 "callback is not a function"）。
            method if method.needs_callback() && !callback.is_callable() => {
                Err(RuntimeError::TypeError(format!(
                    "{} callback is not a function",
                    method.as_str()
                )))
            }
            ArrayOp::ForEach => {
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
            ArrayOp::Map => {
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
                        // 只写**存在**的下标，且用 `CreateDataPropertyOrThrow`：
                        // species 目标拒绝定义时是一次 TypeError，目标可配置时则是
                        // 重新定义成功（`map/target-array-with-non-writable-property`）。
                        for (i, v) in values {
                            self.create_data_property_or_throw(&target, i, v, module)?;
                        }
                        target
                    }
                }))
            }
            ArrayOp::Flat => {
                // 第 3 步：`depthNum`（缺省 **1**；显式 `undefined` 也是 1，
                // 其余走 `ToIntegerOrInfinity`，对象参数会跑用户 `valueOf`）。
                let depth = match args.first() {
                    None | Some(Value::Undefined) => 1,
                    Some(_) => self.to_integer_or_infinity_arg(args.first(), 1, module)?,
                };
                // 第 4 步：`ArraySpeciesCreate(O, 0)` —— 结果由子类的 species 决定，
                // 以前内建层直接造普通数组，于是 `class My extends Array {}` 的
                // `m.flat()` 返回的不是 `My`。
                let target = match self.array_species_create(receiver, 0, module)? {
                    Some(target) => target,
                    None => {
                        crate::vm::object::new_array_object_from_vec(&mut self.heap, Vec::new())
                    }
                };
                let _root_target = self.root(&target);
                self.flatten_into_array(&target, receiver, len, 0, depth, 0, module)?;
                Ok(Some(target))
            }
            ArrayOp::FlatMap => {
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
                        // CreateDataPropertyOrThrow（见 map 的同一处）。
                        for (i, v) in values.into_iter().enumerate() {
                            self.create_data_property_or_throw(&target, i, v, module)?;
                        }
                        target
                    }
                }))
            }
            ArrayOp::Filter => {
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
                        // CreateDataPropertyOrThrow（见 map 的同一处）。
                        for (i, v) in out.iter().enumerate() {
                            self.create_data_property_or_throw(&target, i, v.clone(), module)?;
                        }
                        target
                    }
                }))
            }
            ArrayOp::Some => {
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
            ArrayOp::Every => {
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
            ArrayOp::Find => {
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
            ArrayOp::FindIndex => {
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
            ArrayOp::FindLast => {
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
            ArrayOp::FindLastIndex => {
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
            ArrayOp::ToReversed => {
                let entries = self.array_like_entries(receiver, module)?;
                let mut vals = Self::dense_entries(&entries);
                vals.reverse();
                Ok(Some(self.array_result_from(receiver, &vals, module)?))
            }
            // `toSorted(comparefn)`：返回**新**数组。注意 **Array 的默认比较是
            // 字符串序**（与 `TypedArray.prototype.toSorted` 的数值序不同）。
            ArrayOp::ToSorted => {
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
            ArrayOp::With => {
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
            ArrayOp::ToSpliced => {
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
            ArrayOp::Reduce | ArrayOp::ReduceRight => {
                // Indices are visited in order (or reverse); presence and value
                // are re-read at every step, so a callback that deletes a later
                // element skips it (`reduce` 那一族 `callCnt` 用例）。
                let indices: Vec<usize> = if method == crate::vm::native::ArrayOp::ReduceRight {
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

    /// `ToIntegerOrInfinity(arg)`，对象参数先 `ToPrimitive(number)`
    /// （会跑用户的 `valueOf` / `@@toPrimitive`）。
    fn to_integer_or_infinity_arg(
        &mut self,
        arg: Option<&Value>,
        default: i64,
        module: &Module,
    ) -> Result<i64, RuntimeError> {
        let Some(value) = arg else {
            return Ok(default);
        };
        let primitive = match value {
            Value::Object(_) | Value::Function(_) => self.to_primitive(value, "number", module)?,
            other => other.clone(),
        };
        crate::builtins::to_integer_or_infinity_throwing(Some(&primitive))
    }

    /// `ToIntegerOrInfinity(arg)` 再按 `copyWithin` 的口径夹到 `[0, len]`
    /// （负值从末尾算）。
    fn relative_index_arg(
        &mut self,
        arg: Option<&Value>,
        default: i64,
        len: i64,
        module: &Module,
    ) -> Result<i64, RuntimeError> {
        let n = self.to_integer_or_infinity_arg(arg, default, module)?;
        let clamped = if n < 0 { n.saturating_add(len) } else { n };
        Ok(clamped.clamp(0, len))
    }

    /// 往 `concat` / `flat` 的结果数组追加一个元素。
    ///
    /// 结果是**密集普通数组**（species 取默认值时的常见情形）就直接写密集存储：
    /// 逐个 `define_property` 会把数组推进属性表那条慢路径，实测全量墙钟
    /// 从 ~85s 涨到 ~93–106s。其余情况（子类实例、Proxy、非数组 target）仍走
    /// `CreateDataPropertyOrThrow` —— 那里必须能抛。
    fn append_result_element(
        &mut self,
        target: &Value,
        index: usize,
        value: Value,
        module: &Module,
    ) -> Result<(), RuntimeError> {
        if let Value::Object(obj_ref) = target {
            let mut borrowed = obj_ref.borrow_mut();
            if let Some(arr) = borrowed.as_array_mut()
                && arr.dense_elements().is_some()
            {
                // `index` 之前没写过的位置是空洞（`concat` 的空位、`flat` 的跳过）。
                while arr.len() < index {
                    arr.push(Value::Undefined);
                    let hole = arr.len() - 1;
                    arr.mark_hole(hole);
                }
                arr.push(value);
                return Ok(());
            }
        }
        self.create_data_property_or_throw(target, index, value, module)
    }

    /// 这个接收者该不该走数组口径的 `concat`。
    ///
    /// 规范里 `concat` 是通用的，但引擎有一条历史回落：`Object(42).concat(…)`
    /// 落到 `String.prototype.concat`（`"42falsetrue"`，见 feature 测试
    /// `string_methods_win_for_string_receivers`）。所以只有**像数组**的接收者
    /// （有 `length`，或带 `@@isConcatSpreadable`）才走 VM 实现 —— 后者是必须的，
    /// 因为 getter 可能抛（`spreadable-getter-throws` 的接收者没有 `length`）。
    fn receiver_concat_like(&mut self, receiver: &Value) -> bool {
        if self.receiver_is_array_like(receiver) {
            return true;
        }
        match receiver {
            Value::Object(obj_ref) => crate::vm::prototype::find_descriptor(
                *obj_ref,
                &crate::builtins::is_concat_spreadable_symbol_key(),
            )
            .ok()
            .flatten()
            .is_some(),
            _ => false,
        }
    }

    /// Annex B.2.2 `Object.prototype.__{define,lookup}{Getter,Setter}__`。
    ///
    /// 四件事只有 VM 能做：`ToPropertyKey(key)` 跑用户 `toString`；define
    /// 走 Proxy 的 `defineProperty` trap；lookup 的每一跳都要走 Proxy 的
    /// `getOwnPropertyDescriptor` / `getPrototypeOf` trap（trap 抛原样传播）。
    fn object_accessor_method(
        &mut self,
        this: &Value,
        method: &str,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        use crate::vm::property::PropertyDescriptor;
        // 名字形如 __defineGetter__（尾部也有下划线），不能直接 ends_with。
        let want_getter = method.trim_end_matches('_').ends_with("Getter");
        let defining = method.starts_with("__define");
        crate::builtins::require_object_coercible(this)?;
        let get_key = PropertyKey::intern(&mut self.heap, "get");
        let set_key = PropertyKey::intern(&mut self.heap, "set");
        let obj = match this {
            Value::Object(_) | Value::Function(_) => this.clone(),
            other => crate::builtins::to_object(&mut self.heap, other)?,
        };

        if defining {
            // ES B.2.2.2 第 2 步：accessor 的 callable 检查**在** ToPropertyKey
            // 之前 —— 不可调用时连 `key.toString` 都不该跑
            // （`getter-non-callable` 断言 `toStringCount === 0`）。
            let accessor = args.get(1).cloned().unwrap_or(Value::Undefined);
            if !accessor.is_callable() && !matches!(accessor, Value::Function(_)) {
                return Err(RuntimeError::TypeError(format!(
                    "{method}: accessor must be callable"
                )));
            }
        }

        let key_arg = args.first().cloned().unwrap_or(Value::Undefined);
        let key_value = self.to_property_key_value(&key_arg, module)?;
        let key = crate::builtins::reflect::to_key(&mut self.heap, &key_value);

        if defining {
            let accessor = args.get(1).cloned().unwrap_or(Value::Undefined);
            let mut desc = PropertyDescriptor {
                value: Value::Undefined,
                writable: false,
                enumerable: true,
                configurable: true,
                getter: want_getter.then(|| accessor.clone()),
                setter: (!want_getter).then(|| accessor),
            };
            // ValidateAndApply：没写的那一半从当前自有描述符继承。
            if let Value::Object(obj_ref) = &obj {
                if let Some(old) = obj_ref.borrow().property_get(&key)
                    && old.is_accessor_descriptor()
                {
                    if want_getter {
                        desc.setter = old.setter;
                    } else {
                        desc.getter = old.getter;
                    }
                }
            }
            if let Value::Object(obj_ref) = &obj {
                if obj_ref.borrow().is_proxy() {
                    let desc_value =
                        crate::builtins::descriptor_to_value(&mut self.heap, Some(desc));
                    self.proxy_static_trap(
                        "Object.defineProperty",
                        &[obj.clone(), key_value, desc_value],
                        module,
                    )?;
                } else {
                    obj_ref
                        .borrow_mut()
                        .define_property(&mut self.heap, key, desc)
                        .map_err(|_| {
                            RuntimeError::TypeError(format!("{method}: cannot define property"))
                        })?;
                }
            }
            return Ok(Value::Undefined);
        }

        let mut current = obj;
        loop {
            let mut desc_value: Option<Value> = None;
            if let Value::Object(obj_ref) = &current {
                let is_proxy = obj_ref.borrow().is_proxy();
                if is_proxy {
                    if let Some((target, handler, trap)) =
                        self.proxy_trap(&current, "getOwnPropertyDescriptor", module)?
                    {
                        let answered =
                            self.invoke(&trap, handler, &[target, key_value.clone()], module)?;
                        if !answered.is_undefined() {
                            desc_value = Some(answered);
                        }
                    } else if let Some(desc) = obj_ref.borrow().property_get(&key) {
                        desc_value =
                            Some(crate::builtins::descriptor_to_value(&mut self.heap, Some(desc)));
                    }
                } else if let Some(desc) = obj_ref.borrow().property_get(&key) {
                    desc_value = Some(crate::builtins::descriptor_to_value(&mut self.heap, Some(desc)));
                }
            }
            if let Some(desc_value) = desc_value {
                let getter = self.get_member(&desc_value, &get_key, module)?;
                let setter = self.get_member(&desc_value, &set_key, module)?;
                if !getter.is_undefined() || !setter.is_undefined() {
                    return Ok(if want_getter { getter } else { setter });
                }
                // 数据描述符让查找**停下**并返回 undefined，不会继续上原型链。
                return Ok(Value::Undefined);
            }
            let next = match &current {
                Value::Object(obj_ref) => {
                    let is_proxy = obj_ref.borrow().is_proxy();
                    if is_proxy {
                        match self.proxy_trap(&current, "getPrototypeOf", module)? {
                            Some((target, handler, trap)) => {
                                Some(self.invoke(&trap, handler, &[target], module)?)
                            }
                            None => obj_ref.borrow().get_prototype().map(Value::Object),
                        }
                    } else {
                        obj_ref.borrow().get_prototype().map(Value::Object)
                    }
                }
                _ => None,
            };
            match next {
                None | Some(Value::Null) => return Ok(Value::Undefined),
                Some(next) => current = next,
            }
        }
    }

    /// `Array.prototype.concat(...items)`（ES 23.1.3.1）。
    ///
    /// 内建层那份做不到三件事，全都可观察：
    ///
    /// * 第 2 步 `ArraySpeciesCreate(O, 0)` 在循环**之前** —— 于是 `constructor`
    ///   的读取先于每个元素的 `@@isConcatSpreadable`
    ///   （`is-concat-spreadable-get-order` 数的是 `["constructor",
    ///   "isConcatSpreadable"]`）；species 结果也不再是普通数组。
    /// * `IsConcatSpreadable` 里的 `IsArray` 要**看穿 Proxy**
    ///   （`new Proxy([], {})` 是数组，revoked 是 TypeError）。
    /// * `LengthOfArrayLike` 要跑用户的 `valueOf` / `toString`，`Symbol` 是
    ///   TypeError；`n + len > 2**53-1` 是 TypeError 而不是 RangeError。
    fn array_concat_vm(
        &mut self,
        receiver: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
        let target = match self.array_species_create(receiver, 0, module)? {
            Some(target) => target,
            None => crate::vm::object::new_array_object_from_vec(&mut self.heap, Vec::new()),
        };
        let _root_target = self.root(&target);
        // `O` 自己是第一个 item（第 4 步的 `items`）。
        let mut items: Vec<Value> = Vec::with_capacity(args.len() + 1);
        items.push(receiver.clone());
        items.extend(args.iter().cloned());
        let _roots = self.root_all(&items);
        let mut n: u64 = 0;
        for item in &items {
            if self.is_concat_spreadable_value(item, module)? {
                let len = self.array_like_length_raw(item, module)?;
                if n as f64 + len as f64 > MAX_SAFE {
                    return Err(RuntimeError::TypeError(
                        "concat: the resulting array would be too long".to_string(),
                    ));
                }
                // 迭代另有上限：声称 `length = 2**32` 的元素列表不该真跑四十亿次
                // 属性查找（超过 2**53 的情形上面已经按规范抛了）。
                let len = len.min(1 << 24);
                for k in 0..len {
                    if let Some(value) = self.array_live_at(item, k as usize, module)? {
                        if n as f64 + 1.0 > MAX_SAFE {
                            return Err(RuntimeError::TypeError(
                                "concat: the resulting array would be too long".to_string(),
                            ));
                        }
                        self.append_result_element(&target, n as usize, value, module)?;
                    }
                    // `n` 无条件前进（第 5.b.iv.4 步）：空位在结果里也留一个**空洞**，
                    // 而不是把后面的元素挤上来（`[1,,3].concat([4])` 长度是 4）。
                    n += 1;
                }
            } else {
                if n as f64 + 1.0 > MAX_SAFE {
                    return Err(RuntimeError::TypeError(
                        "concat: the resulting array would be too long".to_string(),
                    ));
                }
                self.append_result_element(&target, n as usize, item.clone(), module)?;
                n += 1;
            }
        }
        // 第 6 步：`Set(A, "length", n, true)` —— 结果数组的 length 由写入维护，
        // 但 species 目标可能不是数组（`Symbol.species = Object`），要显式定长。
        let length_key = PropertyKey::well_known("length").expect("well-known name");
        self.set_member(&target, length_key, Value::Number(n as f64), module)?;
        Ok(target)
    }

    /// `FlattenIntoArray(target, source, sourceLen, start, depth)`（ES 23.1.3.12.1）。
    ///
    /// 逐元素 `HasProperty` → `[[Get]]` → （可选映射）→ 深度够且是数组就递归，
    /// 否则 `CreateDataPropertyOrThrow(target, ToString(index), element)`。
    /// 返回新的 `targetIndex`。
    ///
    /// `level` 只是递归深度守卫：`flat(Infinity)` 遇到自引用数组（`a.push(a)`）
    /// 会无限深入，JS 引擎靠调用栈上限报 RangeError，这里给同一个结论。
    fn flatten_into_array(
        &mut self,
        target: &Value,
        source: &Value,
        source_len: usize,
        start: usize,
        depth: i64,
        level: usize,
        module: &Module,
    ) -> Result<usize, RuntimeError> {
        if level > 1000 {
            return Err(RuntimeError::RangeError(
                "Maximum call stack size exceeded".to_string(),
            ));
        }
        let mut target_index = start;
        for source_index in 0..source_len {
            // `HasProperty` 为假（空洞）就跳过：`flat` 连空位都不保留。
            let Some(element) = self.array_live_at(source, source_index, module)? else {
                continue;
            };
            let should_flatten = depth > 0
                && matches!(&element, Value::Object(o) if o.borrow().kind() == ObjectKind::Array);
            if should_flatten {
                let element_len = self.array_like_length(&element, module)?;
                let inner_depth = if depth == i64::MAX { i64::MAX } else { depth - 1 };
                target_index = self.flatten_into_array(
                    target,
                    &element,
                    element_len,
                    target_index,
                    inner_depth,
                    level + 1,
                    module,
                )?;
            } else {
                if target_index as u64 >= (1u64 << 53) - 1 {
                    return Err(RuntimeError::TypeError(
                        "flat: target index exceeds 2**53 - 1".to_string(),
                    ));
                }
                self.append_result_element(target, target_index, element, module)?;
                target_index += 1;
            }
        }
        Ok(target_index)
    }

    /// `push` / `pop` / `shift` / `unshift`（ES 23.1.3.20 / .17 / .22 / .29）。
    ///
    /// 四个方法都不是"改密集存储"，而是对**接收者**做一串可观察操作：
    ///
    /// * 写元素是 `Set(O, key, value, true)` —— 接收者自己没有这个下标时会走到
    ///   原型链上的 setter（`set-length-array-is-frozen` 正是让
    ///   `Array.prototype[0]` 的 setter 去冻结数组）；
    /// * 读元素是 `Get`，空位会读到原型链上的值（`S15.4.4.6_A4_T1`）；
    /// * 移除位置是 `DeletePropertyOrThrow`（不可配置 → TypeError）；
    /// * 收尾的 `Set(O, "length", …)` 在冻结数组或不可写 `length` 上必须抛
    ///   （`set-length-array-length-is-non-writable`）。
    fn array_stack_method(
        &mut self,
        target: &Value,
        method: &str,
        len: u64,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        const MAX_SAFE: u64 = 9_007_199_254_740_991;
        let length_key = PropertyKey::well_known("length").expect("well-known name");
        let argc = args.len() as u64;
        // `shift` / `unshift` 要真正搬 `len` 个元素：声称 `length = 2**32` 的
        // 接收者不能把一次调用变成四十亿次属性操作（超长的那几条用例数的是
        // `push` 的返回值与 `length` 写入失败，不数搬运）。
        let iter_len = len.min(1 << 24);
        match method {
            "push" => {
                // ES 23.1.3.20 第 7 步：`len + argCount > 2**53-1` 是 TypeError。
                if len as f64 + argc as f64 > MAX_SAFE as f64 {
                    return Err(RuntimeError::TypeError(
                        "push: the resulting length would be too large".to_string(),
                    ));
                }
                let mut n = len;
                for value in args {
                    let key = PropertyKey::element(&mut self.heap, n);
                    self.set_member(target, key, value.clone(), module)?;
                    n += 1;
                }
                self.set_member(target, length_key, Value::Number(n as f64), module)?;
                Ok(Value::Number(n as f64))
            }
            "pop" => {
                if len == 0 {
                    self.set_member(target, length_key, Value::Number(0.0), module)?;
                    return Ok(Value::Undefined);
                }
                let new_len = len - 1;
                let key = PropertyKey::element(&mut self.heap, new_len);
                let element = self.get_member(target, &key, module)?;
                if !self.delete_member(target, &key, module)? {
                    return Err(RuntimeError::TypeError(format!(
                        "Cannot delete property '{new_len}'"
                    )));
                }
                self.set_member(target, length_key, Value::Number(new_len as f64), module)?;
                Ok(element)
            }
            "shift" => {
                if len == 0 {
                    self.set_member(target, length_key, Value::Number(0.0), module)?;
                    return Ok(Value::Undefined);
                }
                let first_key = PropertyKey::element(&mut self.heap, 0);
                let first = self.get_member(target, &first_key, module)?;
                for k in 1..iter_len {
                    let to_key = PropertyKey::element(&mut self.heap, k - 1);
                    if let Some(value) = self.array_live_at(target, k as usize, module)? {
                        self.set_member(target, to_key, value, module)?;
                    } else if !self.delete_member(target, &to_key, module)? {
                        return Err(RuntimeError::TypeError(format!(
                            "Cannot delete property '{}'",
                            k - 1
                        )));
                    }
                }
                let last_key = PropertyKey::element(&mut self.heap, iter_len - 1);
                if !self.delete_member(target, &last_key, module)? {
                    return Err(RuntimeError::TypeError(format!(
                        "Cannot delete property '{}'",
                        iter_len - 1
                    )));
                }
                self.set_member(target, length_key, Value::Number((len - 1) as f64), module)?;
                Ok(first)
            }
            // `unshift`：从末尾往前搬，再在头部写入参数。
            _ => {
                if len as f64 + argc as f64 > MAX_SAFE as f64 {
                    return Err(RuntimeError::TypeError(
                        "unshift: the resulting length would be too large".to_string(),
                    ));
                }
                let mut k = iter_len;
                while k > 0 {
                    let from_index = k - 1;
                    let to_index = from_index + argc;
                    let to_key = PropertyKey::element(&mut self.heap, to_index);
                    if let Some(value) = self.array_live_at(target, from_index as usize, module)? {
                        self.set_member(target, to_key, value, module)?;
                    } else if !self.delete_member(target, &to_key, module)? {
                        return Err(RuntimeError::TypeError(format!(
                            "Cannot delete property '{to_index}'"
                        )));
                    }
                    k -= 1;
                }
                for (j, value) in args.iter().enumerate() {
                    let key = PropertyKey::element(&mut self.heap, j as u64);
                    self.set_member(target, key, value.clone(), module)?;
                }
                let new_len = len + argc;
                self.set_member(target, length_key, Value::Number(new_len as f64), module)?;
                Ok(Value::Number(new_len as f64))
            }
        }
    }

    /// `Array.prototype.copyWithin(target, start, end)` (ES 23.1.3.3).
    ///
    /// 第 8 步是逐元素的：`HasProperty` 为真才 `[[Get]]` + `Set(..., true)`，
    /// 为假则 `DeletePropertyOrThrow`。空位复制过去是**删除**而不是写
    /// `undefined`（`fill-holes`），任何一步抛出都原样传播
    /// （`return-abrupt-from-has-start` / `-get-start-value` / `-set-target-value`
    /// / `-delete-target`）。
    fn array_copy_within_live(
        &mut self,
        target: &Value,
        len: usize,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let len_i = len as i64;
        // 即便 `len == 0` 也要先转换三个参数：第 3-5 步的 `ToIntegerOrInfinity`
        // 是无条件的，`[].copyWithin(0, {valueOf(){throw …}})` 仍然要抛
        // （`return-abrupt-from-start` / `-as-symbol` 一族）。
        // 参数顺序按规范：target → start → end（每次转换都可能跑用户 `valueOf`）。
        let to = self.relative_index_arg(args.first(), 0, len_i, module)?;
        let from = self.relative_index_arg(args.get(1), 0, len_i, module)?;
        let final_index = match args.get(2) {
            None | Some(Value::Undefined) => len_i,
            Some(_) => self.relative_index_arg(args.get(2), len_i, len_i, module)?,
        };
        let count = (final_index - from).min(len_i - to).max(0);
        // 跨度过大同样拒绝：`copyWithin.call({length: 2**53-1}, 0)` 的 count
        // 是 2**53-1，真跑就是几十亿次属性操作（与 `fill` 同一个理由）。
        if count > (1 << 24) {
            return Err(RuntimeError::RangeError(
                "copyWithin: the range is too large".to_string(),
            ));
        }
        // 第 8/9 步的 `direction`：区间重叠且 `to > from` 时必须**倒着**拷，
        // 否则边读边写会把自己覆盖掉 —— `[0,1,2,3].copyWithin(1, 0)` 的前向
        // 循环会得到 `[0,0,0,0]`，规范要的是 `[0,0,1,2]`（memmove 语义）。
        let mut from_cursor = from;
        let mut to_cursor = to;
        let direction: i64 = if from < to && to < from + count {
            from_cursor += count - 1;
            to_cursor += count - 1;
            -1
        } else {
            1
        };
        let mut remaining = count;
        while remaining > 0 {
            let to_key = PropertyKey::element(&mut self.heap, to_cursor as u64);
            match self.array_live_at(target, from_cursor as usize, module)? {
                Some(value) => self.set_member(target, to_key, value, module)?,
                None => {
                    if !self.delete_member(target, &to_key, module)? {
                        return Err(RuntimeError::TypeError(
                            "copyWithin: cannot delete the target element".to_string(),
                        ));
                    }
                }
            }
            from_cursor += direction;
            to_cursor += direction;
            remaining -= 1;
        }
        Ok(target.clone())
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
        // `indexOf` / `lastIndexOf` 用的是 **严格相等**（`NaN` 谁也不等于，
        // `+0 === -0`）：`[NaN].indexOf(NaN)` 是 `-1` 而不是 `0`。
        // `includes` 才用 SameValueZero，它在读族里，不走这个函数。
        fn same_value_strict(a: &Value, b: &Value) -> bool {
            a.strict_eq(b)
        }

        let search = args.first().cloned().unwrap_or(Value::Undefined);
        let len_i = len as i64;
        // 13/14 步：`len = 0` → -1，**先于** `ToIntegerOrInfinity(fromIndex)`
        // （`indexOf/length-zero-returns-minus-one`：`fromIndex` 的 `valueOf`
        // 根本不该被调用）。
        if len_i == 0 {
            return Ok(Value::Number(-1.0));
        }
        // `? ToIntegerOrInfinity(fromIndex)`：只有**缺省**参数才用默认值，
        // 显式的 `undefined` 走 `ToIntegerOrInfinity(undefined) = 0`
        // （`lastIndexOf/15.4.4.15-5-4`：`a.lastIndexOf(2, undefined)` 从 0 往前找）。
        let from = match args.get(1) {
            Some(v) => {
                // `ToNumber` runs `ToPrimitive`, i.e. a user `valueOf` /
                // `toString` on an object argument (`fromIndex` of `{toString() {
                // toStringAccessed = true; return 1 }}` must be observed).
                let primitive = match v {
                    Value::Object(_) | Value::Function(_) => {
                        self.to_primitive(v, "number", module)?
                    }
                    _ => v.clone(),
                };
                crate::builtins::to_integer_or_infinity_throwing(Some(&primitive))?
            }
            None => {
                if method == "indexOf" {
                    0
                } else {
                    len_i - 1
                }
            }
        };

        if method == "indexOf" {
            // 第 5 步：`n >= len` → -1；`n < 0` → `max(len + n, 0)`。
            // （`from.max(0)` 会把 `-1` 当成 0，`[0,'x',2].indexOf('x', -1)`
            // 于是从 0 开始找。）
            let start = if from < 0 {
                len_i.saturating_add(from).max(0)
            } else {
                from.min(len_i)
            };
            if start >= len_i {
                return Ok(Value::Number(-1.0));
            }
            for i in start..len_i {
                if let Some(element) = self.array_live_at(target, i as usize, module)?
                    && same_value_strict(&element, &search)
                {
                    return Ok(Value::Number(i as f64));
                }
            }
        } else {
            // 第 5 步：`n >= 0` → `min(n, len-1)`；否则 `len + n`，仍为负则 -1。
            let mut i = if from >= 0 {
                from.min(len_i - 1)
            } else {
                let n = len_i.saturating_add(from);
                if n < 0 { -1 } else { n.min(len_i - 1) }
            };
            while i >= 0 {
                if let Some(element) = self.array_live_at(target, i as usize, module)?
                    && same_value_strict(&element, &search)
                {
                    return Ok(Value::Number(i as f64));
                }
                i -= 1;
            }
        }
        Ok(Value::Number(-1.0))
    }

    /// `SortCompare(x, y) < 0`（ES 23.1.3.30）：答「y 是否应排在 x 之前」。
    ///
    /// 判定顺序按规范：
    ///
    /// 1. `undefined` **先判**（两个都是 → 相等；只有 x → x 靠后；只有 y → y
    ///    靠后）。这一步在比较器之前 —— `new Array(undefined, 1).sort(fn)` 的
    ///    `undefined` 仍然排最后（`S15.4.4.11_A1.4_T2`）。
    /// 2. 有比较器 → `ToNumber(comparefn(x, y)) < 0`。
    /// 3. 否则 `ToString(x)` / `ToString(y)`，字典序 —— `ToString` 会跑用户的
    ///    `toString`（`S15.4.4.11_A2.*` 的 "Check ToString operator"），所以只能
    ///    在这里做，不能先用 `to_js_string()` 物化成字符串。
    fn sort_compare(
        &mut self,
        x: &Value,
        y: &Value,
        comparefn: &Value,
        module: &Module,
    ) -> Result<bool, RuntimeError> {
        if x.is_undefined() || y.is_undefined() {
            // `SortCompare(x, y) < 0` ⇔ y 是 undefined 而 x 不是。
            return Ok(y.is_undefined() && !x.is_undefined());
        }
        if comparefn.is_callable() || matches!(comparefn, Value::Function(_)) {
            let cmp = self
                .invoke(
                    comparefn,
                    Value::Undefined,
                    &[x.clone(), y.clone()],
                    module,
                )?
                .to_number();
            return Ok(cmp < 0.0);
        }
        let sx = self.sort_to_string(x, module)?;
        let sy = self.sort_to_string(y, module)?;
        Ok(sx < sy)
    }

    /// `ToString(value)` for the default sort comparator (runs user code).
    fn sort_to_string(&mut self, value: &Value, module: &Module) -> Result<String, RuntimeError> {
        let primitive = match value {
            Value::Object(_) | Value::Function(_) => self.to_primitive(value, "string", module)?,
            other => other.clone(),
        };
        crate::builtins::to_string_throwing(&primitive)
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
        // `concat` 不在这里：它要走 `ArraySpeciesCreate` + `IsConcatSpreadable`
        // （见 `array_concat_vm`），而这条读族路径比 species 那条更早。
        const READ_METHODS: &[&str] = &["join", "slice", "includes", "at", "reverse"];
        if !READ_METHODS.contains(&method) {
            return Ok(None);
        }
        // `join(separator)` 是 `ToString`、`slice(start, end)` / `at(index)` /
        // `includes(_, fromIndex)` 是 `ToIntegerOrInfinity`：对象参数要跑用户代码。
        let coerced = self.coerce_index_method_args("Array", method, args, module)?;
        let args: &[Value] = match &coerced {
            Some(values) => values,
            None => args,
        };
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
                // `ToString(separator)`：Symbol 必须是 TypeError，不能
                // `to_js_string()` 静默变成 "Symbol(s)"。
                let sep = match args.first() {
                    Some(sep) if !sep.is_undefined() => {
                        crate::builtins::to_string_throwing(sep)?
                    }
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
