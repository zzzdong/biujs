//! 原生内建的**实现载体**：函数对象自带行为，不再靠名字回查。
//!
//! 以前 `NativeFunctionObject` 只存一个 `name: String`，实现散在四张按名字
//! `match` 的表里（`call_native_by_name` / `call_native` / `call_static_method` /
//! `call_prototype_method`），每次调用都要重新解析一遍名字，还要派生 arity 表、
//! 前缀约定、`name` 显示串推导等一整套「字符串 → 事实」的解码器。
//!
//! 现在改成：**注册时**就把实现放进 [`NativeEntry`]，调用点只做一次间接调用。
//!
//! 分派的键必须是**函数对象身份**，不能是接收者的类型：`Array.prototype.slice`
//! 可以被取出并挂到任意对象上调用（`Array.prototype.slice.call("abc")`），
//! 规范里它与 `String.prototype.slice` 是两个不同的函数对象。接收者的种类只用于
//! `thisNumberValue` 一类的 brand check，该检查在实现内部，不在分派口。
//!
//! 两种实现都在用：[`NativeEntry::Heap`] 是不跑用户代码的纯算子，
//! [`NativeEntry::Vm`] 用于要回调 JS / 造迭代器 / 入 promise job 队列的方法。
//! 后者的函数指针由 [`VmSlot::entry`] 在**注册期**解析，`builtins/` 只登记
//! 一个纯数据符号，不认识 `VM` 类型。

use crate::bytecode::Module;
use crate::vm::gc::Heap;
use crate::vm::{PromiseState, RuntimeError, VM, Value};

/// 只碰堆的实现：不跑用户代码，也就不需要 `VM` / `Module`。
///
/// 第一个参数是 `this`（静态方法与构造器忽略它），第二个是实参。绝大多数
/// `builtins/*` 里的实现已经是这个形状，或只差一个非捕获闭包适配
/// （`|heap, _this, args| object_keys(heap, args)`）。
pub(crate) type HeapImpl = fn(&mut Heap, &Value, &[Value]) -> Result<Value, RuntimeError>;

/// 需要 VM 的实现：要回调 JS、造迭代器 / promise、驱动迭代协议等。
///
/// `builtins/` 不直接引用 VM：它登记的是 [`VmSlot`] 这个**纯数据符号**，
/// `VmSlot::entry()` 在这里把符号解析成函数指针。于是「slot → 实现」只有
/// 一处、编译期穷尽，而且解析发生在**注册期**，调用期只有一次间接调用。
pub(crate) type VmImpl = fn(&mut VM, &Value, &[Value], &Module) -> Result<Value, RuntimeError>;

/// 一个原生内建的实现。
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeEntry {
    /// 纯算法实现（见 [`HeapImpl`]）。
    Heap(HeapImpl),
    /// 需要 VM 的实现（见 [`VmImpl`]）。
    Vm(VmImpl),
    /// `Array.prototype` 方法：算法由 `ArrayOp` 选中（注册期钉在函数对象上）。
    VmArray(ArrayOp),
    /// `String.prototype` 方法：算法由 `StringOp` 选中。
    VmString(StringOp),
    /// 无状态的特殊内建（getter / species / Function.prototype 工具等）。
    VmSpecial(SpecialOp),
    /// Map/Set/WeakMap/WeakSet 原型方法：归属由 op 决定。
    VmCollection(CollectionOp),
    /// TypedArray 原型方法：方法名在注册期钉在函数对象上。
    VmTypedArray(&'static str),
    /// 尚未迁移的原型方法：名字在注册期固定，调用期直接查纯内建表。
    LegacyProto(&'static str),
    /// 捕获状态的内建（`__bound__<id>` / `__revoke__<id>` 等的替代）。
    State(NativeState),
}

/// 函数对象携带的状态：id 不再是名字的一部分，调用时无需解析字符串。
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeState {
    Bound(u32),
    Revoke(u32),
    GeneratorNext(u64),
    GeneratorReturn(u64),
    GeneratorThrow(u64),
    GeneratorIterator(u64),
    IteratorNext(u64),
    IteratorReturn(u64),
    PromiseResolve(u64),
    PromiseReject(u64),
    PromiseAggregate {
        id: u64,
        index: usize,
        fulfilled: bool,
    },
    PromiseCapability(u64),
    PromiseFinally {
        id: u64,
        on_rejected: bool,
    },
    RegExpGetter(&'static str),
}

impl NativeEntry {
    /// 调用这个内建。调用点只有这一处判断「是哪个实现」。
    pub(crate) fn call(
        self,
        vm: &mut VM,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        match self {
            NativeEntry::Heap(f) => f(&mut vm.heap, this, args),
            NativeEntry::Vm(f) => f(vm, this, args, module),
            NativeEntry::VmArray(op) => vm.array_proto_host(op, this, args, module),
            NativeEntry::VmString(op) => vm.string_proto_host(op, this, args, module),
            NativeEntry::VmSpecial(op) => vm.special_host(op, this, args, module),
            NativeEntry::VmCollection(op) => vm.collection_host(op, this, args, module),
            NativeEntry::VmTypedArray(name) => vm.typed_array_proto_host(name, this, args, module),
            NativeEntry::LegacyProto(name) => {
                crate::builtins::call_prototype_method(&mut vm.heap, this, name, args)
            }
            NativeEntry::State(state) => vm.call_state(state, this, args, module),
        }
    }
}

/// builtins 侧登记的 VM 内建符号（不含任何 `VM` 类型，保持分层）。
#[derive(Clone, Copy, Debug)]
pub(crate) enum VmSlot {
    PromiseResolve,
    PromiseReject,
    PromiseAll,
    PromiseRace,
    PromiseAllSettled,
    PromiseAny,
    PromiseThen,
    PromiseCatch,
    PromiseFinally,
    ObjectAssign,
    ArrayIsArray,
    ArrayFrom,
    ArrayOf,
    StringFromCharCode,
    StringFromCodePoint,
    ObjectGetOwnPropertyDescriptor,
    ObjectHasOwn,
    ObjectDefineProperty,
    ObjectDefineProperties,
    ObjectCreate,
    ReflectHas,
    ReflectDeleteProperty,
    ReflectGetOwnPropertyDescriptor,
    ReflectDefineProperty,
    StringRaw,
    JsonStringify,
    JsonParse,
    TypedArrayFrom,
    TypedArrayOf,
    MapGroupBy,
    DetachArrayBufferHost,
    ObjectKeys,
    ObjectGetOwnPropertyNames,
    ObjectGetOwnPropertySymbols,
    ObjectGetPrototypeOf,
    ObjectSetPrototypeOf,
    ObjectIsExtensible,
    ObjectPreventExtensions,
    ProxyRevocable,
    ReflectGet,
    ReflectSet,
    ReflectApply,
    ReflectConstruct,
    ReflectOwnKeys,
    ReflectGetPrototypeOf,
    ReflectSetPrototypeOf,
    ReflectIsExtensible,
    ReflectPreventExtensions,
    ArrayEntries,
    ArrayKeys,
    ArrayValues,
    TypedArrayEntries,
    TypedArrayKeys,
    TypedArrayValues,
    MapEntries,
    MapKeys,
    MapValues,
    SetEntries,
    SetKeys,
    SetValues,
    ArrayProto(ArrayOp),
    StringProto(StringOp),
    Special(SpecialOp),
    Collection(CollectionOp),
    TypedArrayProto(&'static str),
    LegacyProto(&'static str),
    MatchAll,
    ErrorToString,
    ForInKeys,
    NumberCtor,
    FunctionToString,
}

/// Map/Set/WeakMap/WeakSet 原型方法。方法归属由枚举变体固定，调用期不再从
/// `__map_method__<name>` 这类前缀里还原。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollectionOp {
    MapGet,
    MapHas,
    MapSet,
    MapDelete,
    MapClear,
    MapForEach,
    MapGetOrInsert,
    MapGetOrInsertComputed,
    SetAdd,
    SetHas,
    SetDelete,
    SetClear,
    SetForEach,
    SetUnion,
    SetIntersection,
    SetDifference,
    SetSymmetricDifference,
    SetIsSubsetOf,
    SetIsSupersetOf,
    SetIsDisjointFrom,
    WeakMapSet,
    WeakMapGet,
    WeakMapHas,
    WeakMapDelete,
    WeakMapGetOrInsert,
    WeakMapGetOrInsertComputed,
    WeakSetAdd,
    WeakSetHas,
    WeakSetDelete,
}

impl CollectionOp {
    pub(crate) fn as_str(self) -> &'static str {
        use CollectionOp as C;
        match self {
            C::MapGet => "get",
            C::MapHas => "has",
            C::MapSet => "set",
            C::MapDelete => "delete",
            C::MapClear => "clear",
            C::MapForEach => "forEach",
            C::MapGetOrInsert => "getOrInsert",
            C::MapGetOrInsertComputed => "getOrInsertComputed",
            C::SetAdd => "add",
            C::SetHas => "has",
            C::SetDelete => "delete",
            C::SetClear => "clear",
            C::SetForEach => "forEach",
            C::SetUnion => "union",
            C::SetIntersection => "intersection",
            C::SetDifference => "difference",
            C::SetSymmetricDifference => "symmetricDifference",
            C::SetIsSubsetOf => "isSubsetOf",
            C::SetIsSupersetOf => "isSupersetOf",
            C::SetIsDisjointFrom => "isDisjointFrom",
            C::WeakMapSet => "set",
            C::WeakMapGet => "get",
            C::WeakMapHas => "has",
            C::WeakMapDelete => "delete",
            C::WeakMapGetOrInsert => "getOrInsert",
            C::WeakMapGetOrInsertComputed => "getOrInsertComputed",
            C::WeakSetAdd => "add",
            C::WeakSetHas => "has",
            C::WeakSetDelete => "delete",
        }
    }
}

/// `Array.prototype` 上需要 VM 实现的算法选择器。
///
/// 以前这些方法是靠 `method: &str` + 两张名单在多个分派口挑出来的；现在身份
/// 由函数对象携带，进入唯一实现后由这个穷尽枚举选择算法，编译期保证不漏。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArrayOp {
    Map,
    Filter,
    ForEach,
    Some,
    Every,
    Reduce,
    ReduceRight,
    Find,
    FindIndex,
    FlatMap,
    Sort,
    IndexOf,
    LastIndexOf,
    FindLast,
    FindLastIndex,
    ToReversed,
    ToSorted,
    ToSpliced,
    With,
    CopyWithin,
    Flat,
    Push,
    Pop,
    Shift,
    Unshift,
    Fill,
    ToLocaleString,
    // 读族：也走同一个 host（`try_array_read_method` / builtin 兜底）。
    Join,
    Slice,
    Includes,
    At,
    Concat,
    Reverse,
    Splice,
    ToString,
}

impl ArrayOp {
    pub(crate) const ALL: &'static [(&'static str, Self)] = &[
        ("map", Self::Map),
        ("filter", Self::Filter),
        ("forEach", Self::ForEach),
        ("some", Self::Some),
        ("every", Self::Every),
        ("reduce", Self::Reduce),
        ("reduceRight", Self::ReduceRight),
        ("find", Self::Find),
        ("findIndex", Self::FindIndex),
        ("flatMap", Self::FlatMap),
        ("sort", Self::Sort),
        ("indexOf", Self::IndexOf),
        ("lastIndexOf", Self::LastIndexOf),
        ("findLast", Self::FindLast),
        ("findLastIndex", Self::FindLastIndex),
        ("toReversed", Self::ToReversed),
        ("toSorted", Self::ToSorted),
        ("toSpliced", Self::ToSpliced),
        ("with", Self::With),
        ("copyWithin", Self::CopyWithin),
        ("flat", Self::Flat),
        ("push", Self::Push),
        ("pop", Self::Pop),
        ("shift", Self::Shift),
        ("unshift", Self::Unshift),
        ("fill", Self::Fill),
        ("toLocaleString", Self::ToLocaleString),
        ("join", Self::Join),
        ("slice", Self::Slice),
        ("includes", Self::Includes),
        ("at", Self::At),
        ("concat", Self::Concat),
        ("reverse", Self::Reverse),
        ("splice", Self::Splice),
        ("toString", Self::ToString),
    ];

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, op)| *op)
    }

    pub(crate) fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, op)| *op == self)
            .map(|(n, _)| *n)
            .expect("ArrayOp is in ALL")
    }

    /// 会 `ArraySpeciesCreate` 的方法：`len > 2**32-1` 要 RangeError。
    pub(crate) fn uses_species(self) -> bool {
        matches!(
            self,
            Self::Map
                | Self::Filter
                | Self::FlatMap
                | Self::Flat
                | Self::ToReversed
                | Self::ToSorted
                | Self::ToSpliced
                | Self::With
        )
    }

    /// 无回调则必须抛 TypeError 的方法（其它方法允许省略回调）。
    pub(crate) fn needs_callback(self) -> bool {
        matches!(
            self,
            Self::Map
                | Self::Filter
                | Self::ForEach
                | Self::Some
                | Self::Every
                | Self::Reduce
                | Self::ReduceRight
                | Self::Find
                | Self::FindIndex
                | Self::FindLast
                | Self::FindLastIndex
                | Self::FlatMap
        )
    }
}

/// 不带捕获状态的特殊内建：getter、species、`Function.prototype` 工具等。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpecialOp {
    FunctionProto,
    FunctionHasInstance,
    ObjectToString,
    ObjectProtoGet,
    ObjectProtoSet,
    ThrowTypeError,
    SymbolDescription,
    MapSize,
    SetSize,
    TaLength,
    TaByteLength,
    TaByteOffset,
    TaBuffer,
    AbByteLength,
    DvBuffer,
    DvByteLength,
    DvByteOffset,
    ArrayIteratorFactory,
    TaToStringTag,
    StringCtor,
    ProxyCtor,
    Species,
    RegExpMatch,
    RegExpReplace,
    RegExpSearch,
    RegExpSplit,
    FunctionCall,
    FunctionApply,
    FunctionBind,
    ObjectDefineGetter,
    ObjectDefineSetter,
    ObjectLookupGetter,
    ObjectLookupSetter,
}

/// `String.prototype` 上需要 VM 参与的方法（ToString(this) 与参数 ToPrimitive
/// 都可能跑用户代码，协议方法还要调用 `@@match` / `@@replace` 等）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StringOp {
    ValueOf,
    ToString,
    CharAt,
    CharCodeAt,
    Concat,
    Includes,
    IndexOf,
    Slice,
    Substring,
    ToUpperCase,
    ToLowerCase,
    Trim,
    Split,
    Match,
    MatchAll,
    Search,
    Replace,
    ReplaceAll,
    StartsWith,
    EndsWith,
    Repeat,
    TrimStart,
    TrimEnd,
    CodePointAt,
    At,
    Normalize,
    ToLocaleLowerCase,
    ToLocaleUpperCase,
    LocaleCompare,
    PadStart,
    PadEnd,
    LastIndexOf,
}

impl StringOp {
    pub(crate) const ALL: &'static [(&'static str, Self)] = &[
        ("valueOf", Self::ValueOf),
        ("toString", Self::ToString),
        ("charAt", Self::CharAt),
        ("charCodeAt", Self::CharCodeAt),
        ("concat", Self::Concat),
        ("includes", Self::Includes),
        ("indexOf", Self::IndexOf),
        ("slice", Self::Slice),
        ("substring", Self::Substring),
        ("toUpperCase", Self::ToUpperCase),
        ("toLowerCase", Self::ToLowerCase),
        ("trim", Self::Trim),
        ("split", Self::Split),
        ("match", Self::Match),
        ("matchAll", Self::MatchAll),
        ("search", Self::Search),
        ("replace", Self::Replace),
        ("replaceAll", Self::ReplaceAll),
        ("startsWith", Self::StartsWith),
        ("endsWith", Self::EndsWith),
        ("repeat", Self::Repeat),
        ("trimStart", Self::TrimStart),
        ("trimEnd", Self::TrimEnd),
        ("codePointAt", Self::CodePointAt),
        ("at", Self::At),
        ("normalize", Self::Normalize),
        ("toLocaleLowerCase", Self::ToLocaleLowerCase),
        ("toLocaleUpperCase", Self::ToLocaleUpperCase),
        ("localeCompare", Self::LocaleCompare),
        ("padStart", Self::PadStart),
        ("padEnd", Self::PadEnd),
        ("lastIndexOf", Self::LastIndexOf),
    ];

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, op)| *op)
    }

    pub(crate) fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, op)| *op == self)
            .map(|(n, _)| *n)
            .expect("StringOp is in ALL")
    }

    /// 除 `matchAll`（单独走 `try_match_all_method`）外的协议方法。
    pub(crate) fn protocol_op(self) -> Option<StringProtocolOp> {
        Some(match self {
            Self::Match => StringProtocolOp::Match,
            Self::Search => StringProtocolOp::Search,
            Self::Split => StringProtocolOp::Split,
            Self::Replace => StringProtocolOp::Replace,
            Self::ReplaceAll => StringProtocolOp::ReplaceAll,
            _ => return None,
        })
    }
}

/// `String.prototype` 里走 `@@match` / `@@replace` / `@@search` / `@@split`
/// 协议的五个方法。注册期由 `StringOp` 决定，调用期不再解析字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StringProtocolOp {
    Match,
    Search,
    Split,
    Replace,
    ReplaceAll,
}

/// `Object.*` / `Reflect.*` 静态方法里「描述符参数要跑用户代码」的那几种。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DescKind {
    None,
    /// 第三个参数是属性描述符，`[[Get]]` 字段要走 VM。
    DefineProperty,
    /// 第二个参数是「名字 → 描述符」的对象。
    DefineProperties,
    /// `Object.create` 的第二个参数同 `DefineProperties`。
    Create,
}

impl VmSlot {
    /// 解析成真正实现（注册期调用一次）；每个闭包都不捕获，coerce 成 `fn` 指针。
    pub(crate) fn entry(self) -> NativeEntry {
        // `Array.prototype` 一族直接带算法选择器，函数指针由 `NativeEntry::call`
        // 统一分发到 `VM::array_proto_host`，不用为每个方法生成一个闭包。
        if let VmSlot::ArrayProto(op) = self {
            return NativeEntry::VmArray(op);
        }
        if let VmSlot::StringProto(op) = self {
            return NativeEntry::VmString(op);
        }
        if let VmSlot::Special(op) = self {
            return NativeEntry::VmSpecial(op);
        }
        if let VmSlot::Collection(op) = self {
            return NativeEntry::VmCollection(op);
        }
        if let VmSlot::TypedArrayProto(name) = self {
            return NativeEntry::VmTypedArray(name);
        }
        if let VmSlot::LegacyProto(name) = self {
            return NativeEntry::LegacyProto(name);
        }
        if matches!(self, VmSlot::MatchAll) {
            return NativeEntry::Vm(|vm, this, args, module| {
                vm.try_match_all_method(this, args, module)
            });
        }
        if matches!(self, VmSlot::ErrorToString) {
            return NativeEntry::Vm(|vm, this, _args, module| {
                vm.error_to_string_host(this, module)
            });
        }
        if matches!(self, VmSlot::ForInKeys) {
            return NativeEntry::Vm(|vm, _this, args, _module| vm.for_in_keys(args));
        }
        if matches!(self, VmSlot::NumberCtor) {
            return NativeEntry::Vm(|vm, _this, args, module| vm.number_call(args, module));
        }
        if matches!(self, VmSlot::FunctionToString) {
            return NativeEntry::Vm(|vm, this, _args, _module| vm.function_to_string_host(this));
        }
        let f: VmImpl = match self {
            VmSlot::PromiseResolve => |vm, this, args, module| {
                crate::vm::VM::promise_static_resolve(vm, this, args, module)
            },
            VmSlot::PromiseReject => |vm, this, args, module| {
                crate::vm::VM::promise_static_reject(vm, this, args, module)
            },
            VmSlot::PromiseAll => |vm, this, args, module| {
                crate::vm::VM::promise_static_all(vm, this, args, module)
            },
            VmSlot::PromiseRace => |vm, this, args, module| {
                crate::vm::VM::promise_static_race(vm, this, args, module)
            },
            VmSlot::PromiseAllSettled => |vm, this, args, module| {
                crate::vm::VM::promise_static_all_settled(vm, this, args, module)
            },
            VmSlot::PromiseAny => |vm, this, args, module| {
                crate::vm::VM::promise_static_any(vm, this, args, module)
            },
            VmSlot::PromiseThen => |vm, this, args, module| {
                crate::vm::VM::promise_proto_then(vm, this, args, module)
            },
            VmSlot::PromiseCatch => |vm, this, args, module| {
                crate::vm::VM::promise_proto_catch(vm, this, args, module)
            },
            VmSlot::PromiseFinally => |vm, this, args, module| {
                crate::vm::VM::promise_proto_finally(vm, this, args, module)
            },
            VmSlot::ObjectAssign => |vm, _this, args, module| vm.object_assign(args, module),
            VmSlot::ArrayIsArray => |vm, _this, args, module| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(vm.is_array_including_proxies(&value, module)?))
            },
            VmSlot::ArrayFrom => |vm, this, args, module| {
                let source = args.first().cloned().unwrap_or(Value::Undefined);
                let mapfn = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
                vm.array_from(this, &source, &mapfn, &this_arg, module)
            },
            VmSlot::ArrayOf => |vm, this, args, module| vm.array_of(this, args, module),
            VmSlot::StringFromCharCode => |vm, _this, args, module| {
                vm.string_from_char_code_vm(args, module)
            },
            VmSlot::StringFromCodePoint => |vm, _this, args, module| {
                vm.string_from_code_point_vm(args, module)
            },
            VmSlot::ObjectGetOwnPropertyDescriptor => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.getOwnPropertyDescriptor",
                    true,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ObjectHasOwn => |vm, _this, args, module| {
                vm.object_static_vm("Object.hasOwn", true, DescKind::None, args, module)
            },
            VmSlot::ObjectDefineProperty => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.defineProperty",
                    true,
                    DescKind::DefineProperty,
                    args,
                    module,
                )
            },
            VmSlot::ObjectDefineProperties => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.defineProperties",
                    false,
                    DescKind::DefineProperties,
                    args,
                    module,
                )
            },
            VmSlot::ObjectCreate => |vm, _this, args, module| {
                vm.object_static_vm("Object.create", false, DescKind::Create, args, module)
            },
            VmSlot::ReflectHas => |vm, _this, args, module| {
                vm.object_static_vm("Reflect.has", true, DescKind::None, args, module)
            },
            VmSlot::ReflectDeleteProperty => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.deleteProperty",
                    true,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ReflectGetOwnPropertyDescriptor => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.getOwnPropertyDescriptor",
                    true,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ReflectDefineProperty => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.defineProperty",
                    true,
                    DescKind::DefineProperty,
                    args,
                    module,
                )
            },
            VmSlot::StringRaw => |vm, _this, args, module| vm.string_raw(args, module),
            VmSlot::JsonStringify => |vm, _this, args, module| {
                vm.json_stringify_full(args, module)
            },
            VmSlot::JsonParse => |vm, _this, args, module| vm.json_parse_full(args, module),
            VmSlot::TypedArrayFrom => |vm, this, args, module| {
                let source = args.first().cloned().unwrap_or(Value::Undefined);
                let mapfn = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
                vm.typed_array_from(this, &source, &mapfn, &this_arg, module)
            },
            VmSlot::TypedArrayOf => |vm, this, args, module| {
                vm.typed_array_of(this, args, module)
            },
            VmSlot::MapGroupBy => |vm, _this, args, module| {
                let items = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                vm.map_group_by(&items, &callback, module)
            },
            VmSlot::DetachArrayBufferHost => |vm, _this, args, _module| {
                vm.detach_array_buffer_host(args)
            },
            // 下面七个只需要 Proxy trap + 纯 builtin 表，共用 `object_static_vm`
            // 的 `DescKind::None` 路径。
            VmSlot::ObjectKeys => |vm, _this, args, module| {
                vm.object_static_vm("Object.keys", false, DescKind::None, args, module)
            },
            VmSlot::ObjectGetOwnPropertyNames => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.getOwnPropertyNames",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ObjectGetOwnPropertySymbols => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.getOwnPropertySymbols",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ObjectGetPrototypeOf => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.getPrototypeOf",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ObjectSetPrototypeOf => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.setPrototypeOf",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ObjectIsExtensible => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.isExtensible",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ObjectPreventExtensions => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Object.preventExtensions",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ProxyRevocable => |vm, _this, args, _module| vm.proxy_revocable_host(args),
            VmSlot::ReflectGet => |vm, _this, args, module| {
                vm.object_static_vm("Reflect.get", true, DescKind::None, args, module)
            },
            VmSlot::ReflectSet => |vm, _this, args, module| {
                vm.object_static_vm("Reflect.set", true, DescKind::None, args, module)
            },
            VmSlot::ReflectApply => |vm, _this, args, module| {
                vm.object_static_vm("Reflect.apply", false, DescKind::None, args, module)
            },
            VmSlot::ReflectConstruct => |vm, _this, args, module| {
                vm.object_static_vm("Reflect.construct", false, DescKind::None, args, module)
            },
            VmSlot::ReflectOwnKeys => |vm, _this, args, module| {
                vm.object_static_vm("Reflect.ownKeys", false, DescKind::None, args, module)
            },
            VmSlot::ReflectGetPrototypeOf => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.getPrototypeOf",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ReflectSetPrototypeOf => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.setPrototypeOf",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ReflectIsExtensible => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.isExtensible",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            VmSlot::ReflectPreventExtensions => |vm, _this, args, module| {
                vm.object_static_vm(
                    "Reflect.preventExtensions",
                    false,
                    DescKind::None,
                    args,
                    module,
                )
            },
            // 三个同名的迭代器工厂靠函数身份区分：Array 的走 array iterator，
            // Map/Set/TypedArray 的同名方法将来各有自己的 slot。
            VmSlot::ArrayEntries => |vm, this, _args, module| {
                vm.array_iterator_host("entries", this, module)
            },
            VmSlot::ArrayKeys => |vm, this, _args, module| {
                vm.array_iterator_host("keys", this, module)
            },
            VmSlot::ArrayValues => |vm, this, _args, module| {
                vm.array_iterator_host("values", this, module)
            },
            VmSlot::TypedArrayEntries => |vm, this, _args, _module| {
                vm.typed_array_iterator_host("entries", this)
            },
            VmSlot::TypedArrayKeys => |vm, this, _args, _module| {
                vm.typed_array_iterator_host("keys", this)
            },
            VmSlot::TypedArrayValues => |vm, this, _args, _module| {
                vm.typed_array_iterator_host("values", this)
            },
            VmSlot::MapEntries => |vm, this, args, module| vm.map_method(this, "entries", args, module),
            VmSlot::MapKeys => |vm, this, args, module| vm.map_method(this, "keys", args, module),
            VmSlot::MapValues => |vm, this, args, module| vm.map_method(this, "values", args, module),
            VmSlot::SetEntries => |vm, this, args, module| vm.set_method(this, "entries", args, module),
            VmSlot::SetKeys => |vm, this, args, module| vm.set_method(this, "keys", args, module),
            VmSlot::SetValues => |vm, this, args, module| vm.set_method(this, "values", args, module),
            VmSlot::ArrayProto(_)
            | VmSlot::StringProto(_)
            | VmSlot::Special(_)
            | VmSlot::Collection(_)
            | VmSlot::TypedArrayProto(_)
            | VmSlot::LegacyProto(_)
            | VmSlot::MatchAll
            | VmSlot::ErrorToString
            | VmSlot::ForInKeys
            | VmSlot::NumberCtor
            | VmSlot::FunctionToString => unreachable!(),
        };
        NativeEntry::Vm(f)
    }
}

impl VM {
    /// 带状态内建的调用：`NativeState` 里已经装着 id，调用期不再 `strip_prefix`
    /// 加 `parse`（原来的 `__bound__<id>` / `__revoke__<id>` 全靠名字传状态）。
    pub(crate) fn call_state(
        &mut self,
        state: NativeState,
        this: &Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        match state {
            NativeState::Bound(id) => {
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
                self.invoke(&target, this_arg, &all, module)
            }
            NativeState::Revoke(id) => {
                if let Some(Value::Object(obj_ref)) = self.revocable_proxies.get(&id) {
                    if let Some(proxy) = obj_ref.borrow_mut().as_proxy_mut() {
                        proxy.revoked = true;
                    }
                }
                Ok(Value::Undefined)
            }
            NativeState::GeneratorNext(id) => {
                let send = args.first().cloned().unwrap_or(Value::Undefined);
                self.generator_next(id, send, module)
            }
            NativeState::GeneratorReturn(id) => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                self.generator_abrupt(id, Ok(value), module)
            }
            NativeState::GeneratorThrow(id) => {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                self.generator_abrupt(id, Err(RuntimeError::Thrown(reason)), module)
            }
            NativeState::GeneratorIterator(_) => Ok(this.clone()),
            NativeState::IteratorNext(id) => {
                let iter_val = self
                    .iterator_registry
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| RuntimeError::TypeError("iterator is no longer alive".to_string()))?;
                let send = args.first().cloned();
                let (item, done) = self.iterator_next(iter_val, send, module)?;
                Ok(Self::iterator_result(&mut self.heap, item, done))
            }
            NativeState::IteratorReturn(id) => {
                if let Some(iter_val) = self.iterator_registry.get(&id).cloned() {
                    self.iterator_close(iter_val, args.first(), false, module)?;
                }
                Ok(Self::iterator_result(&mut self.heap, Value::Undefined, true))
            }
            NativeState::PromiseResolve(id) => {
                if let Some(promise) = self.state.promises.get(&id).cloned() {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    self.promise_resolve(&promise, value, module);
                }
                Ok(Value::Undefined)
            }
            NativeState::PromiseReject(id) => {
                if let Some(promise) = self.state.promises.get(&id).cloned() {
                    let value = args.first().cloned().unwrap_or(Value::Undefined);
                    self.promise_settle(&promise, PromiseState::Rejected, value);
                }
                Ok(Value::Undefined)
            }
            NativeState::PromiseAggregate {
                id,
                index,
                fulfilled,
            } => Ok(self
                .dispatch_aggregation(id, index, fulfilled, args, module)
                .unwrap_or(Ok(Value::Undefined))?),
            NativeState::PromiseCapability(id) => {
                self.state.capabilities.insert(
                    id,
                    crate::vm::PromiseCapability {
                        promise: Value::Undefined,
                        resolve: args.first().cloned().unwrap_or(Value::Undefined),
                        reject: args.get(1).cloned().unwrap_or(Value::Undefined),
                    },
                );
                Ok(Value::Undefined)
            }
            NativeState::RegExpGetter(prop) => {
                crate::builtins::regexp_prototype_getter(this, prop)
            }
            NativeState::PromiseFinally { id, on_rejected } => {
                let callback = self.state.finally_handlers.get(&id).cloned();
                let argument = args.first().cloned().unwrap_or(Value::Undefined);
                if let Some(cb) = callback {
                    self.invoke(&cb, Value::Undefined, &[], module)?;
                    if on_rejected {
                        return Err(RuntimeError::Thrown(argument));
                    }
                    return Ok(argument);
                }
                Ok(Value::Undefined)
            }
        }
    }

    /// 原生内建的**唯一**调用入口（`[[Call]]` 与 `CallMethod` 都走这里）。
    ///
    /// 已迁移的内建直接调它自带的实现；`entry` 为 `None` 的那批仍交给
    /// `call_native_by_name` 按名字分派 —— 每迁一族就少一批，最后这条回退消失。
    pub(crate) fn invoke_native(
        &mut self,
        callee: &Value,
        this: Value,
        args: &[Value],
        module: &Module,
    ) -> Result<Value, RuntimeError> {
        let entry = match callee {
            Value::Object(obj_ref) => obj_ref
                .borrow()
                .as_native_function()
                .and_then(|f| f.entry()),
            _ => None,
        };
        let Some(entry) = entry else {
            let name = crate::builtins::native_function_name(callee).ok_or_else(|| {
                RuntimeError::InternalError("invoke_native: callee is not a native".to_string())
            })?;
            return self.call_native_by_name(&name, this, args, module);
        };
        // 与 `call_native_by_name` 同样的装箱：裸 `Value::Function(id)` 要先成为
        // 函数对象，`Object.keys(fn)` / `Object.getOwnPropertyDescriptor(fn, …)`
        // 才看得到它（`ToObject` 对裸函数引用是原样透传的）。
        let boxed_args: Vec<Value> = args.iter().map(|a| self.as_object_value(a)).collect();
        let this = self.as_object_value(&this);
        entry.call(self, &this, &boxed_args, module)
    }
}
