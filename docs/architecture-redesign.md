# 深度重构方案 v2：值模型 / 对象 / GC / 帧 / 异常的整体重设计

> **本文是上位方案**，也是本仓库唯一的"目标架构"文档。它取代同名前身（v1，只覆盖
> GC / Value / 帧与 EH），并把 v1 的全部结论**并入 + 订正 + 补齐三大块**：
> **对象模型（形状 / 属性存储 / 内联缓存）**、**作用域与闭包（scope slot）**、
> **调用分派（字符串名字 → entry point）**。
>
> **参照实现**：`/home/alex/code/open_source/ChakraCore`（本地，可逐块核对，不要凭印象）。
> **下位文档**：`docs/interpreter-refactor.md`（P0–P3 增量方案）。
> 本文对它及其自身 v1 的裁决见 §1.4。
>
> **过程约束**：允许破坏性变更；**不以"零回归 / 零性能回退"为目标**，
> 只以"最终架构正确 + 最终性能提升"为目标。但"允许破坏"不等于"放任"，
> 新的验证协议见 §7。
>
> **锚点**：文中的 `文件:行号` 是写作时的快照，会漂。**以符号名为准，行号只作初次导航。**
> 我方规模基线：`src/` **39,343 LOC**（`find src -name '*.rs' | xargs wc -l`）。

---

## 0. 一页速览

| # | 里程碑 | 内容 | 消掉什么 | 破坏面 |
|---|--------|------|----------|--------|
| **M0** | 前置（**已完成**） | 3 个语义 bug 钉测试；`FunctionBody`/`EhRegion` 派生视图；`Instr` enum + `Kind`/`Role`/`RelPc`·`AbsPc`；`open_frame`/`enter_call` 收敛 | "三份知识互不校验"的一半；EH 表的"不可信" | 小（已落地） |
| **M1** | 编译期自包含 | `CodeBlock` per function；**EH 区域表由 lowering 回填**（含出口集合）；`Module` 降为装配器；函数 flags 进 `CodeBlock` | `Module` 全局可变状态；`generators`/`asyncs`/`derived_ctors` 三个 HashSet；`EndTry`/`Try` 不配对 | 中（不碰生命周期） |
| **M2** | **执行模型** | 寄存器 arena + `Frame{base,len}`；**单层主循环**；唯一调用入口；EH 走区域表 + completion 传递 | **9 条平行栈**、哨兵 pc、`PushC/PopC/MovC`、4 层嵌套 `step`、全局 `registers[19]` | **大**（VM 重写） |
| **M3** | 堆与句柄 | `Heap` + `Gc<T>` + `HandleScope`；`Value::Object` 从 `Rc<RefCell<dyn JSObject>>` 换成 `Gc<Object>`；**先只分配不回收** | `dyn` 分发、`RefCell` 借用崩溃、`borrow()×220` | **大**（全库类型改动） |
| **M4** | 收集器 | size-class 堆块 + mark-sweep + 根集 + **GC stress 模式** | 循环泄漏；"堆预算护栏只是代理" | 中（收集器独立） |
| **M5** | 值表示 | `Value` → NaN-boxed `u64`（8 B）；`Hole`/`Uninitialized` 成立即数 | `Value` 24 B、`Function(u32)` 双表示、`MarkHole` 指令、TDZ 特判 | 大（全库 match） |
| **M6** | 对象形状 | `Shape` + 内联槽 + `PropertyKey` 驻留 + 单态 IC | `HashMap` 属性表、`ObjectKind` 21 变体的巨型 match、原型链每次重走 | **大** |
| **M7** | 作用域与闭包 | scope slot + `Environment` 链（编译期定 `(depth, index)`） | `closure_var_stack: Vec<HashMap<String, Value>>`、**"闭包是创建时值快照"这条语义债** | **大** |
| **M8** | 收尾 | 生成器/async **堆帧**；`Result<_, Exception>`（异常就是 JS 值）；`Engine`/`Realm`/`Cx` 边界 | `SuspendedFrame` 全量快照、`invoke_boundaries` 类地雷、`Result<_, String>` 错误种类丢失、每用例新建 VM | 中 |

**顺序的两条硬约束**

1. **M1 → M2**：EH 区域表必须先可信，帧才有"按区域找 handler"的依据；而区域表只能由 lowering
   回填（§5.1），不是从指令流能派生出来的（v1 §10.3 已证伪）。
2. **M2 → M3/M4**：`Frame` 的寄存器 arena **就是 GC 的根集**。先定帧、再定根，
   否则"哪些槽要扫"会随帧模型反复。
   （**v1 在这里写错了**：v1 说"帧的局部槽要活在托管堆上，所以 GC 必须在帧之前"——
   ChakraCore 的帧恰恰**不是**堆分配的，见 §2.4、§4.1 订正。）

**M5–M8 之间无强依赖**，可按收益排序插入：
M5（值）× M6（对象）合起来决定**单次属性访问与算术的绝对开销**；
M7 是**现有语义债里最大的一笔**（不是优化，是修 bug）；M8 是**删机制**。

---

## 1. 结论

### 1.1 走深改，且深改的范围比 v1 更大

v1 的结论是"走 Track B（深改），分 4 步"。**保持这个结论，但扩充范围**：
v1 只把 GC / 帧 / EH 当架构项，而实测 ChakraCore 之后可以确认，
**另外三块与它们同级**，且其中两块直接对应**已登记的现存缺陷**：

| 新增的架构项 | 为什么不是"顺带优化" |
|---|---|
| **对象模型**：形状 + 内联槽 + 内联缓存 | 现状每次属性读是 `HashMap<String, Value>` + `RefCell` 借用；ChakraCore 用 `PathTypeHandler` 把它压成"一次内联槽下标"。**这是解释器最大的单项开销来源**，也是 `ObjectKind` 21 变体巨型 match 的根源 |
| **作用域与闭包**：scope slot | 现状闭包是 `HashMap<String, Value>` 的**创建时快照**，所以"闭包共享可变单元"不成立（`handover.md` §4 明写：`counter()` 仍返回 `0,0,0`）。ChakraCore 的 scope slot 是这个语义的**正确实现**，不是优化 |
| **调用分派**：entry point | 现状原生函数**按名字字符串分发**（`call_native_by_name`），由此派生出 arity 表、前缀约定、`vm_handled_static` 旁路名单——`architecture.md` §4 记的多个地雷都是它的表现。ChakraCore 是 `RecyclableObject::GetEntryPoint()`，一次间接调用 |

### 1.2 三条路线判据（决定顺序）

1. **不碰生命周期的先做**：M1（编译期）零生命周期风险，却解锁 M2。
2. **先定形状，再定内容**：M2 定"帧与寄存器窗口"这个形状，M3/M4 才能定"根集"这个内容。
3. **值表示排在对象模型之前**：M5（NaN-boxing）会让 M6（形状）的字段类型一次定型；
   反过来做要改两遍。

### 1.3 与 ChakraCore 的移植边界（一句话）

**搬思想，不搬代码；搬解释器共用的部分，不搬为 JIT / 并发 / TTD 才存在的那部分。**
不移植清单见 §8.2。

### 1.4 对 v1 与 `interpreter-refactor.md` 的裁决

| 对象 | 裁决 |
|---|---|
| `interpreter-refactor.md` 的 **P0a/P0b** | **保留，已完成**。与 ChakraCore 的 `OpCodes.h → OpCodeList.h → enum` 多路 include 同构，且用 Rust 的穷尽 `match` 做得更强（"漏登记"是编译错误，ChakraCore 没有这个保证） |
| 其 **P1**（调用归一） | **保留结论，代码并入 M2**。`Control`/`CallArgs`/`callee_kind` 的形状是对的，M2 把它们做大（`Construct` 形态、`dst` 进签名） |
| 其 **P2**（CodeBlock per function） | **提前到 M1**（v1 已裁决，维持） |
| 其 **P3a/P3b** | **作废增量版，并入 M2**。P3a 是"保留嵌套循环的帧归一"，P3b 是"堆帧 + 单层循环"；M2 一次做到 P3b，因为**单层循环是帧模型的自然结果**，分两半等于把同一套机器写两遍 |
| v1 §4（GC 分阶段 A/B） | **保留"分阶段"思想，改分法**：不再是"循环回收器 → 真 GC"，而是"**堆 + 句柄（M3，只分配不回收）→ 收集器（M4）→ 根集加固（stress）**"。理由见 §4.1 |
| v1 §5（帧/EH/寄存器三问） | **保留，但顺序改**：帧与寄存器合并进 M2；EH 进 M1+M2；"寄存器模型排在 GC 之后"的理由（"要求 Value 定长"）**被证伪**（§4.2） |
| v1 §10.3（EH 配对问题） | **保留，升格**：从"已知限制：单个 `end_offset` 只是近似"升级为"**EH 表存出口集合**"，因为 lowering 手上本来就有 `exit_edges`（§5.1） |
| v1 §8（3 个语义 bug） | **保留**，M0 已钉测试 |
| v1 §7（验证协议） | **保留骨架，扩充**为 §7（加 GC stress / 差分探针矩阵 / 编码往返测试） |

### 1.5 v1 被订正的三处（都有证据）

| # | v1 的说法 | 事实 | 影响 |
|---|---|---|---|
| 1 | "GC 必须在帧模型之前 —— 帧的局部槽要活在托管堆上" | ChakraCore 的普通帧在**栈/arena**（`m_localSlots[0]` 柔性数组），**只有生成器帧**在 Recycler 堆上（`InterpreterStackFrame.cpp:1875`） | 依赖关系从"生命周期"变成"根集形状"：**先帧后 GC** |
| 2 | "寄存器模型排第三，因为它要求 `Value` 变成定长标记值" | "寄存器是帧内定长窗口"与 `Value` 是 8 B 还是 24 B **无关** | 寄存器模型并入 M2；NaN-boxing 后置到 M5 |
| 3 | 只把 GC / 帧 / EH 当架构项 | 对象模型、作用域/闭包、调用分派**与它们同级**，且其中闭包一项是现存语义 bug | 方案从 4 步扩为 9 步（§0） |

---

## 2. 参照实现的实测事实（取自 ChakraCore，非印象）

### 2.1 规模

`find <dir> -name '*.cpp' -o -name '*.h' -o -name '*.inl' | xargs wc -l`

| 目录 | LOC | 我们能用的部分 |
|---|---|---|
| `lib/` 合计 | **810,519** | —— |
| `lib/Backend`（JIT） | 238,764 | **全部不用** |
| `lib/Runtime/Library` | 138,685 | 内置对象的**语义**（不搬实现） |
| `lib/Common` | 105,688 | 其中 `Memory/` **57,856** 是 GC |
| `lib/Runtime/Language` | 80,455 | **解释器**：`InterpreterStackFrame.cpp` **9,663** |
| `lib/Parser` | 54,214 | 不用（我们用 oxc） |
| `lib/Runtime/Base` | 43,533 | **`FunctionBody.cpp` 9,817 / `.h` 3,942** |
| `lib/Runtime/ByteCode` | 40,559 | **`OpCodes.h` 905**、`OpCodeUtil.h` 62、布局/发射 |
| `lib/Runtime/Types` | 26,164 | **`PathTypeHandler.cpp` 3,944**、`TypeHandler.h` 683、`DynamicObject.h` 398、**`Type.h` 仅 117** |

关键单文件：`Recycler.cpp` 9,338 / `MarkContext.inl` 298 / `JavascriptGenerator.cpp` 532 / `ScriptContext.h` 1,995。

**参照我们**：`src/` **39,343**，其中 `vm/mod.rs` **10,868**、`compiler/lowering/mod.rs` **4,609**、
`vm/object.rs` **3,970**、`bytecode.rs` **2,226**、`builtins/mod.rs` **2,013**、`ir/instruction.rs` **1,122**。
⇒ **ChakraCore 的 `InterpreterStackFrame` 单文件（9,663）约等于我们整个 VM（10,868）**。
这不是"我们的代码少"，而是提醒：**表驱动解决的是元数据来源，不是代码量**。

### 2.2 指令表：多路 include（我们已同构，保留）

```
OpCodes.h（905 行，~800 条 MACRO(opcode, layout, attr, dbgAttr)）
   ├─ OpCodeList.h              → DEF_OP 展开成 enum class OpCode      (OpLayouts.h:18)
   ├─ BackendOpCodeAttr.cpp:56  → static const int OpcodeAttributes[] = { attr, ... }
   └─ OpCodeUtil.cpp            → OpCodeLayouts[]（布局表）
```

- **属性是位标志**（`BackendOpCodeAttr.cpp:16-54`）：`OpSideEffect=0x1`、`OpCallInstr=0x800`、
  `OpNoFallThrough=0x400000`、`OpHasMultiSizeLayout=0x1000000`、`OpByteCodeOnly=0x40000000`…
  一大批（`OpCanCSE`/`OpBailOutRec`/`OpProfiled*`/`OpFastFldInstr`）**只为 JIT 的优化器存在**。
- **命名澄清**：没有 `OpTempObject` / `OpCall` / `OpJump` 这三个名字；对应的是
  `OpTempObject*` 系列 / `OpCallInstr` / `OpNoFallThrough`。
- 一致性校验靠 `CompileAssert`（`OpCodes.cpp:8-15`）：基本指令 ≤ 255、扩展指令 > 255。
  **这是静态断言，不是穷尽性检查** —— 我们靠 Rust 的 `match` 穷尽性，比它强（§1.4）。

⇒ **移植结论**：P0a/P0b 的做法（一张表 + 穷尽派生）就是这条路的最优形态。**不要**再引入
布局编码（`LayoutTypes.h` 的 `Reg1/Reg2/Br/_Small/_Medium/_Large`）—— 那是为**序列化与字节码读取器**
服务的（`ByteCodeReader`、`EncodedSize = IsSmall ? (Small?1:2) : 3`，`OpCodeUtil.h:39-45`）。
我们内存里是 `Instr` enum，没有读取器，布局表纯属负债。

### 2.3 值模型 `Var`（位图）

```
typedef void * Var;                                    // RuntimeCommon.h:155，就是一个指针
AtomTag_Object = 0x0                                   // GC 对象：低位天然是 0，无需标记
AtomTag_IntPtr = 0x1,  AtomTag = 0x1,  VarTag_Shift = 1
INT32VAR（64 位）:  VarTag_Shift = 48
                    AtomTag_IntPtr = 1<<48, AtomTag_Int32 = 0, AtomTag_Multiply = 1
FLOATVAR:           FloatTag_Value = 0xFFFC<<48        // NaN-boxing：double 直接当 Var 存
```

- `TaggedInt`：32 位 `(uint32)n | 1`；INT32VAR 下 `n << 48 | (1<<48)`（`TaggedInt.inl:118,205`）；
  值域 `k_nMaxValue = INT_MAX / AtomTag_Multiply`（`TaggedInt.h:79-80`）。
- **`SmallInt` 这个类型不存在**（整数即 `TaggedInt`）；**`VarIsNumber` 也不存在**
  （全库只在一处文本出现），数值判定用 `TaggedInt::Is` + `JavascriptNumber::Is`。
- `TypeId.h:8-140` 是一个**全局类型编号表**，约定 `typeof == "object"` 的对象
  `typeId >= TypeIds_Object(28)`；类型经 `Type` 对象携带
  （`Type.h:32-41`：`typeId / flags / prototype / entryPoint`），
  `RecyclableObject::GetTypeId/GetPrototype/GetEntryPoint` 转发到它（`RecyclableObject.h:259-263`）。
- `VarIs<T>(v)`（`RecyclableObject.h:491-502`）先判"是不是 RecyclableObject"
  （INT32VAR 下 `(v>>48)==0`，否则 `(v&AtomTag)==AtomTag_Object`），再 `VarIsImpl<T>`。

⇒ **移植结论**：`Var` 的编码细节不必照抄（§4.9 给出我方位图），但**两条思想必须搬**：

1. **值是一个定长（8 B）的机器字**，类型判定是**位运算**，不是 enum 判别式；
2. **类型信息挂在共享的 `Type` 对象上**（`typeId`/`prototype`/`entryPoint`），
   而不是散在每处 `match Value`。第 2 条是 M6/M8 的直接依据。

### 2.4 解释器帧（**关键：寄存器在帧内**）

```cpp
struct InterpreterStackFrame {                       // InterpreterStackFrame.h:40
    ByteCodeReader m_reader;                         // :113 当前函数字节码游标
    int m_inSlotsCount; Js::CallFlags m_callFlags; Var* m_inParams;   // :114-116
    Var* m_outParams; Var* m_outSp; Var* m_outSpCached;               // :117-119 出参区
    Var  m_arguments;                                // :120
    StackScriptFunction* stackNestedFunctions; FrameDisplay* localFrameDisplay;
    Var localClosure; Var paramClosure; Var* innerScopeArray;          // :121-126
    ScriptContext* scriptContext; ScriptFunction* function;
    FunctionBody* m_functionBody; void** inlineCaches;                 // :127-130
    InterpreterStackFrame* previousInterpreterFrame;                   // :133 帧链
    EHBailoutData* ehBailoutData;                                      // :194
    __declspec(align(16)) Var m_localSlots[0];       // :197 柔性数组：locals + temps 的寄存器堆
};
```

- **寄存器寻址就是下标**：`GetReg(id) { return m_localSlots[id]; }`（`.cpp:7935-7941`）、
  `SetReg`（`.cpp:7943-7948`）。R0=返回值、R1=root object，常量槽用负向 `REGSLOT_TO_CONSTREG`。
- **分配策略有三级**（`.cpp:2013-2083`）：
  1. 默认 `_alloca`（**栈上**，纯 C++ 技巧）；
  2. `varAllocCount + stackVarAllocCount > LocalsThreshold` → `EnsureInterpreterArena()`
     （ArenaAllocator，`:2060-2072`）；
  3. **生成器/协程帧从 Recycler 堆分配**：`RecyclerNewPlus(...)`（`:1875`，`.h:37-39` 有注释）。
- **帧没有 push/pop 函数**（无 `PushCallFrame` 符号）：进入靠 `InterpreterThunk` 里的
  `Setup` + `InitializeAllocation`，出栈靠 `previousInterpreterFrame` 链 + `PopOut(argCount)`。
- 常量表在 `FunctionBody::m_constTable`（`FunctionBody.h:2087-2092`），
  装帧时 `InitConstantSlots` → `memcpy` 进 `m_localSlots`（`FunctionBody.cpp:4475-4483`）。

⇒ **移植结论（三条，都很重要）**

1. **"寄存器是帧内的一段定长内存"** 是 ChakraCore 帧模型的核心，搬运（§4.3）。
   我们的 `registers: [Value; 19]`（全局、不随帧保存）正好是它的对立面。
2. **帧的三级分配**在 Rust 里没有 `_alloca` 的等价物，但**语义可以照搬**：
   普通帧在**共享寄存器 arena**里开窗口（对应 `_alloca` 的"便宜"），
   生成器帧**整块搬到 GC 堆**（对应第 3 级）。Rust 的替代方案天然更简单（§4.3）。
3. **v1 的"帧局部槽要在托管堆上"是错的**：ChakraCore 的普通帧在**栈/arena**，
   只有生成器帧在堆上。所以 M2 与 M4 之间**没有生命周期依赖**，只有"根集形状"的依赖。

### 2.5 调用入口（统一到 entry point）

- `OP_CallCommon`（`.cpp:3942-3996`）：用 `m_outParams` 造 `Arguments(CallInfo(flags, argCount), m_outParams)`，
  然后 **一个调用点**：`JavascriptFunction::CallFunction<true>(function, function->GetEntryPoint(), args)`
  （`:3973/3988`）；`OP_CallCommonI` 只是转发（`:3998-4002`）。
- `[[Construct]]` 走**同一条路**：`NewScObject_Helper` 造
  `Arguments(CallInfo(CallFlags_New, argc), m_outParams)`（`.cpp:6490,6561,6591`）。
- **原生函数与脚本函数共用同一个调用点**：`JavascriptMethod` 的签名是
  `Var(*)(RecyclableObject*, CallInfo, ...)`（`RuntimeCommon.h:158`）；
  `CallFunction` 直接跳 `function->GetEntryPoint()` —— 对解释器是 `InterpreterThunk`，
  对原生/JIT 是各自的 `entryPoint`。
- 宿主入口：`CallRootFunction`（`JavascriptFunction.cpp:691`）、`EntryCall`（`:632`）、`NewInstance`（`:321`）。

⇒ **移植结论**：**可调用性是一个函数指针，不是一组 `if`**。
这直接删掉我们的 `call_native_by_name`（字符串分发）、`native_function_name`、
`PROTO_METHOD_PREFIX` / `__bound__` / `MAP_METHOD_PREFIX` 三套前缀约定、
`vm_handled_static` 名单、arity 表、以及 `ObjectKind` 上的调用分支。
`architecture.md` §4 里"`Object.*` 静态分发绕过 VM""同名原型方法必须靠前缀 + 接收者守卫"
这两条地雷**从此不存在**（§4.4）。

### 2.6 异常处理

- **EH 不全是"表"**：try/catch/finally 在 ChakraCore 里**内联在字节码流里**
  （`TryCatch / TryFinally / TryFinallyWithYield / Catch / Finally / ResumeCatch / ResumeFinally / Leave / BrOnException / BrOnNoException`，
  `OpCodes.h:186-187,654-662`）。
- **给 JIT bailout 用的树形描述子**：`EHBailoutData`（`EHBailoutData.h:15-52`）
  `{ nestingDepth, catchOffset, finallyOffset, handlerType{HT_None,HT_Catch,HT_Finally}, parent, child }`，
  挂在帧上（`InterpreterStackFrame.h:194`）。
- 处理逻辑：`ProcessCatch`（`.cpp:6806`）、`ProcessFinally`（`:6827`）、
  `ProcessTryHandlerBailout(EHBailoutData*, uint32)`（`:6846`，可递归进子 try）、
  **`ProcessTryFinally(ip, jumpOffset, regException, regOffset, hasYield)`**（`:7161-7300`）。
- 帧上的状态是**标志 + 三个深度**：`WithinTryBlock/WithinCatchBlock/WithinFinallyBlock`
  （`.h:22-35`）+ `nestedTryDepth/nestedCatchDepth/nestedFinallyDepth`（`.h:160-163`）。
- **命名澄清**：没有 `SetExceptionObject` / `hasBailedOut`（那是 ChakraFull 的名字）；
  bailout 用局部 `bool bailedOut` + `HasBailedOutPtrStack`（`JavascriptExceptionOperators.h:58-67`）。
  **`FunctionBody` 里没有 `GetTryCatchOffset`**。

⇒ **移植结论（两条）**

1. **finally 是"带续体进入"的**：`ProcessTryFinally` 的参数里带着
   `jumpOffset`（正常续体）/ `regException`（异常续体）/ `hasYield`（生成器里含 yield）。
   这正好是我们 `SehRecord` 上那组可变字段（`in_finally` / `pending_exception` /
   `delayed_jump_target` / `delayed_return`）想表达的东西，但
   **我们的表达是"状态"，它的是"参数"**。参数化之后，"该跑哪个 finally"的扫描与
   嵌套时的脆弱性一起消失（§4.6）。
2. **帧只记"我在第几层 try/catch/finally"，不记 pc 地址**：处理器地址从**区域元数据**来。
   这与 v1 的"EH 元数据化"同向，但比 v1 走得更彻底。

### 2.7 对象模型（**最大的一块，v1 完全没覆盖**）

- **`RecyclableObject` 虚接口**：`GetPropertyQuery`（`:302-303`）、`SetProperty`（`:307-308`）、
  `GetItemQuery/SetItem`（`:324-327`）、`GetPrototype()`（`:261`）、`HasPropertyQuery`（`:295`）。
- **`Type` 只存元信息，不存属性**：`Type.h:32-41` = `{typeId, flags, prototype, entryPoint}`（**117 行**）。
- **形状在 `DynamicTypeHandler`**（`TypeHandler.h:28`，纯虚）：
  `GetProperty(DynamicObject*, ...)`（`:453`）、`HasProperty`（`:451`）、`GetPropertyIndex`（`:443`）、
  `GetPropertyCount`（`:435`）、`FindNextProperty`（`:439`）、slot 换算（`:629-630`）。
- **`PathTypeHandler`**（`.cpp` **3,944 行**）：把"属性访问路径 = 形状转换链"编译成
  **内联槽的定长查找**，配合 `PathType` 与 `TypePropertyCache` / `PropertyRecordUsageCache` 做缓存。
  **这是避免每次走字典查找的关键。**
- 其他 handler：`SimpleTypeHandler`、`DictionaryTypeHandler`、`SimpleDictionaryTypeHandler`、
  `NullTypeHandler`、`ES5ArrayTypeHandler`、`MissingPropertyTypeHandler`、
  `DeferredTypeHandler`（延迟建形状）。
- **内联槽的位置**（`DynamicObject.h:107-110,128,159-163`）：既可在对象头内
  （`IsObjectHeaderInlinedTypeHandler()`），也可在对象之后；`GetSlot/GetInlineSlot/GetAuxSlot`。
- **原型链**：`RecyclableObject::GetPrototype()` → `Type::GetPrototype()`；
  `PrototypeChainCache`（`Language/PrototypeChainCache.cpp`）缓存整条链；
  `TypeFlagMask_SkipsPrototype`（`Type.h:14,78`）。

⇒ **移植结论（四条）**

1. **对象 ≠ 属性表**。对象是 `(形状, 槽数组)`；属性名到槽号是**形状的职责**，且形状**可共享**
   —— 两个 `{x:1}` 字面量应当共用同一个形状。
2. **形状要能"前缀共享"**（PathType 的思想）：`A → A+b → A+b+c` 是一条链，
   所以 `{a,b,c}` 与 `{a,b,d}` 共享前两个属性的布局。
3. **属性名必须驻留成整数**（ChakraCore 的 `PropertyId`）：否则每次访问都要哈希字符串。
4. **必须有内联缓存**（ChakraCore 有多态 IC）：字节码里的 `GetProp` 要能记住
   "上次的形状 → 槽号"，命中即一次整数比较（§4.5）。

### 2.8 函数体与代码组织

- `FunctionBody`（`.h` 3,942 / `.cpp` 9,817）核心字段（`.h:2087-2092`）：

  ```
  FieldWithBarrier(ByteBlock*) byteCodeBlock;      // 字节码
  FieldWithBarrier(FunctionEntryPointList*) entryPoints;
  FieldWithBarrier(Field(Var)*) m_constTable;      // 常量表
  FieldWithBarrier(void**) inlineCaches;           // 内联缓存！
  FieldWithBarrier(InlineCachePointerArray<PolymorphicInlineCache>) polymorphicInlineCaches;
  FieldWithBarrier(PropertyId*) cacheIdToPropertyIdMap;
  ```

- 寄存器约定：`ReturnValueRegSlot=0`、`RootObjectRegSlot=1`（`.h:2131-2134`）。
- 常量表：`CreateConstantTable`（`.cpp:4353`，首槽是 root object）、
  `InitConstantSlots`（`:4475`）、`GetConstantVar`（`:4486`）。
- **嵌套函数与 scope slot**：`ParseableFunctionInfo`（`.h:1471`）持有 `nestedArray`
  （`GetNestedArray/GetNestedCount`，`.h:1521-1522`）；scope slot 的属性映射走
  `GetPropertyIdsForScopeSlotArray`（`.h:1650-1652`）
  —— **即：作用域槽也是 RegisterSlot 编号，不是字符串**。
- 字节码容器是 `ByteBlock`（`ByteBlock.h:14-17`：`{uint m_contentSize; byte* m_content}`），
  可由 Recycler 或 **ArenaAllocator** 分配（`AnewArray`，`:34`）。
- **命名澄清**：没有 `m_propertyIds` / `m_constantEncoding` / `m_ihostArena`
  （对应的是 `nestedArray` / `m_constTable` / `ByteBlock`）。**没有 `Module.h`**
  （用的是 `ModuleRecordBase` / `SourceTextModuleRecord` / `ModuleRoot`）。

⇒ **移植结论**：M1 的 `CodeBlock` 就是它的 `FunctionBody` 的**精简版**：
`instructions + constants + flags + eh_table + scope_slot_count + nested + body_range`。
**`inlineCaches` 要一起进 `CodeBlock`**（M6 用），别等以后再加。

### 2.9 生成器 / async

- **挂起态就存在生成器对象里**：`JavascriptGenerator : DynamicObject`（`JavascriptGenerator.h:17`）

  ```cpp
  enum GeneratorState { SuspendedStart, Suspended, Executing, Completed }   // :20-26
  Field(InterpreterStackFrame*) frame;   Field(GeneratorState) state;
  Field(Arguments) args;                 Field(ScriptFunction*) scriptFunction;
  Field(DynamicObject*) resumeYieldObject;                                  // :57-62
  ```

  `SetFrame/SetFrameSlots`（`.cpp:119-165`，断言局部槽数与 `GetLocalsCount()` 一致）、
  `CallGenerator(data, ResumeYieldKind{Normal,Throw,Return})`（`:55` / `.cpp:167-220`）、
  `EntryNext/EntryReturn/EntryThrow`（`:120-122` / `.cpp:237-332`）。
- **命名澄清**：**没有 `GeneratorUtils`**（那是 ChakraFull 的模块）。

⇒ **移植结论**：**生成器 = "一个没有弹掉的帧" + 一个状态机**。
它**不做寄存器快照**，因为那个帧本来就是**独立分配的一块内存**。
我们的 `SuspendedFrame`（`vm/object.rs`）要拷 `data` / `argc` / `bp_offset` / `pc` / `this` /
`function_val` / `closure_maps` / **`registers[19]`** / `delegates` / **`seh`** —— 拷贝清单越长，
"漏拷一项"的地雷越多（`architecture.md` §2.4 明写"每个生成器改动都要问三个问题"）。
M2 把帧做成"可整块搬走的一块"，M8 把它搬进生成器对象，**这张拷贝清单整条消失**（§4.7）。

### 2.10 "共用"与"JIT 专有"的分界

| 分类 | 内容 |
|---|---|
| **可移植** | 指令表多路 include；`Var` 标记值 + `TypeId` + `VarIs`；mark-sweep + `RootPtr` + `MarkContext` 标记栈；`InterpreterStackFrame` 的"寄存器在帧内"；`FunctionBody` 自包含 + `nestedArray` + scope slot；`Type`/`DynamicTypeHandler`/`PathTypeHandler` 形状系统；`PrototypeChainCache`；`JavascriptGenerator{frame,state}`；`EHBailoutData` 的**树形嵌套**思想 |
| **JIT 专有，不移植** | `lib/Backend/*`（238k）；`OpCodeAttr` 里给优化器/IC/分析用的位；`DynamicProfileInfo`；`BailOut`/bailout 路径；`MACRO_BACKEND_ONLY` 指令；`TypePropertyCache` 的 fixed-field 部分 |
| **并发/调试专有，不移植** | 并发与后台 GC、写屏障（STW 不需要，§4.1）、`RecyclerSweepManager`、TTD |

---

## 3. 现状的代价（可核对）

度量基线（`docs/phase2-status.tsv`）：执行 19708 / 通过 **16561** / 失败 3147 / 跳过 7843；
单元 190 → M0 后 >201 / feature 523 / 护栏 7。

### 3.1 值模型与借用面（实测计数）

| 项 | 数值 | 证据 |
|---|---|---|
| `Value` 的变体 | **8** 个：`Undefined / Null / Bool(bool) / Number(f64) / String(Rc<String>) / Symbol(Rc<SymbolData>) / Object(Rc<RefCell<dyn JSObject>>) / Function(u32)` | `vm/value.rs:45-56` |
| `Value` 的大小 | **24 B**（`Rc<RefCell<dyn JSObject>>` 是**胖指针 16 B** + `f64` 8 B + 判别式；未标 `#[repr]`，所以是 Rust 默认布局的结果而非保证） | 由布局推得 |
| `.borrow()` | **220** | `grep -ro "\.borrow()" src \| wc -l` |
| `.borrow_mut()` | **156** | 同上 |
| `Rc<RefCell` | **162 行** | `grep -ro "Rc<RefCell" src \| wc -l` |
| `JSObject` trait 方法 | ~20 个，**全是属性/原型/可扩展性**；**没有 `call`/`construct`** | `vm/object.rs:28-127` |
| `ObjectKind` 变体 | **21** | `vm/property.rs:124-148` |
| `Register` | `R0..R15` + `Rsp`/`Rbp`/`Rv` = **19**，全局、不随帧保存 | `bytecode.rs:1193-1216` |
| `MAX_CALL_DEPTH` | 512 | `vm/mod.rs:34` |

代价：

1. **循环必然泄漏**：原型链、闭包、`obj.self = obj` 收不掉；`run` 只能"重置整个 `State`"
   并逐个摘掉 `func_objs` 的原型 / `revocable_proxies` / `iterator_registry` / `generator_registry`。
2. **借用冲突不是编译错误，是运行时 panic**。220 + 156 个点，且 `dyn` 分发下重入会 panic
   （`interpreter-refactor.md` §6 已记），所以 Proxy 陷阱只能在 VM 层截获。
3. **每次对象访问两层间接**（`Rc` 解引用 + `RefCell` 借用标志检查）。
4. **`Function(u32)` 与 `Object(FunctionObject)` 双表示**：`strict_eq` 要特判裸/装箱函数
   （`value.rs:372-378`）。
5. 没有堆，所以"堆预算护栏"只是代理（`State` 里没有字节计数）。

### 3.2 执行模型

| 项 | 现状 | 证据 |
|---|---|---|
| 平行栈 | **9 条**：`this_stack` / `this_state` / `function_stack` / `frame_argc` / `construct_stack` / `new_target_stack` / `closure_var_stack` / `seh_stack` / `ctrl_stack` | `vm/mod.rs:9808-9878` |
| 嵌套 `step` 循环 | **4 处**：`run` / `invoke_with_new_target` / 生成器恢复 / `invoke_construct` | —— |
| 哨兵 pc | `return_pc = module.instructions.len()`（越界 pc）；`resume_sentinel()` 同一个技巧 | `vm/mod.rs:1385`、`:9033` |
| `ctrl_stack` | **每帧 3 项**（`return_pc`、`saved_seh_depth`、`saved_closure_depth`） | `open_frame` 的 `pushc` ×3 |
| Rust 栈 = JS 栈 | test262 runner 必须给套件线程 **256 MB** 栈 | `tests/test262_runner.rs` |
| `invoke_boundaries` | 保证 Rust 驱动帧里抛的异常不被外侧 handler 抓走；"帧深度必须在装帧之前取"这条教训**出现过四次** | `architecture.md` §4 |
| 帧驱动 | 开帧内联在 opcode arm 里（为了省一层嵌套循环），另在 `drive_bytecode_frame` 有一份 | —— |

**M0 已收敛的（保留）**：`open_frame` 一处开帧（5 个调用点共用）、`enter_call` 统一入口
（`Control` + `CallArgs`）、`callee_kind` 单一判定出口、`FrameMode` 合一
`[[Call]]`/`[[Construct]]` 的驱动。

### 3.3 对象模型与分派

- 属性是 `HashMap`（`OrdinaryObject.properties`），**没有形状共享**：两个同构字面量各持一张表。
- **原生函数按名字字符串分发**：`call_native_by_name` 匹配 `NativeFunctionObject.name`。
  派生出：arity 表（`builtins/mod.rs` 的 `builtin_arity`）、`PROTO_METHOD_PREFIX`
  （`__proto_method__`）、`CLASS_CTOR_FLAG`、`__bound__`、`MAP_METHOD_PREFIX`、
  以及 `vm_handled_static` **旁路名单**。
- `architecture.md` §4 里由此产生的四条地雷：
  "`Object.*` 静态分发会绕过 VM，除非名字也在 `vm_handled_static` 里"、
  "同名原型方法要靠独立前缀 + 接收者守卫"、
  "`CallMethod` 的 `try_array_callback_method` 跑在原生派发之前"、
  "builtin 层看不到原型链与访问器"。
- `ObjectKind` 的 **21 个变体**在 VM 里被大量 `match`，每加一个对象类型要改多处
  （`architecture.md` §3.1 的"新增集合类型改动清单"有 7 步）。
- **Proxy 的 13 个陷阱必须在 VM 层截获**（对象层调不动 JS）：8 个截获点散在
  `get_member` / `set_member` / `delete_member` / `In` / `CallEx` / `New` / `invoke` / `construct`。
  这是"`RefCell` 下重入 panic"的直接后果，**M3 之后才有条件改**（§4.5 的 `Cx` 模型）。

### 3.4 作用域与闭包（**现存语义债**）

- `closure_var_stack: Vec<HashMap<String, Value>>`（`vm/mod.rs:9848`）+ `SuspendedFrame.closure_maps`。
- **闭包捕获是创建时值快照**（`docs/dyn-capture.md`），所以"共享可变单元"不成立：
  `handover.md` §4 明写 `counter()` 仍返回 `0,0,0`；也导致测试写法受限
  （"计数器不能写在闭包里，只能往捕获的数组里 `push`"）。
- 同时挂着 `let`/`const` 的 **TDZ 残留**与 **per-iteration 绑定**未做。

⇒ 这不是性能问题，是**正确性问题**，而 ChakraCore 有完整答案（scope slot，§2.8）。

### 3.5 错误通道

- `JSObject::define_property` / `property_set` / `prototype::*` 的失败是 **`Result<_, String>`**，
  调用点一律包成 `TypeError`；而 `ArraySetLength` 要求 `RangeError`。
- 现用 `RuntimeError::into_property_error()` 把 RangeError 渲染成 `"RangeError: <msg>"`，
  `from_property_error()` 还原 —— **靠字符串往返传错误种类**（`architecture.md` §4 第 1 条）。
- 新增任何走 String 通道的错误都要重新问一遍"种类是否会丢"。

### 3.6 异常处理

- 运行时进出仍靠 `Try` 指令自己的相对偏移（`vm/mod.rs:3709-3734`），EH 表只做 `debug_assert` 等价校验。
- `SehRecord` **14 个字段**（`vm/mod.rs:10197-10228`），且要 `Clone` 以便生成器挂起时携带。
- **`EndTry` 与 `Try` 不是一一配对**（v1 §10.3 的实测结论，详见 §5.1）：
  某些 `Try` 没有 `EndTry`，嵌套时那条 `EndTry` 也不是外层 try 的收尾。
  所以 `EhRegion::end` **目前不能当作受保护范围**。
- `break`/`continue`/`return` 穿 finally 靠 `DelayedJump` + `delayed_return`
  （写在 `SehRecord` 的可变字段上），`ResumeExc` 收尾。

### 3.7 编译期

- `Module` 一把梭（`bytecode.rs:11-44`）：`constants` / `symtab` / `func_info` /
  `generators` / `asyncs` / `derived_ctors` / `exit_pc` / `instructions` /
  `debug_instructions` / `eh_index`。
- `body_of` / `eh_regions_of` 都是 `symtab` 的**派生视图**（不存新状态）。
- 三份互相不校验的知识（`interpreter-refactor.md` §1.4）：IR 变体的
  `defined_and_used_vars`（喂 liveness/regalloc）、`codegen` 的位置约定 + 回填表、
  VM 的 arm 解构。M0 的 `Kind`/`Role` 已经把后两者锁在一起，但**编译期与运行期
  对 EH 区域的认知仍然不一致**（§3.6 最后一条）。

---

## 4. 目标设计

每一项的写法：**ChakraCore 的什么 → Rust 怎么表达 → 迁移与陷阱**。

### 4.0 目标数据流（一张图）

```
 Compiler                                    Runtime (Cx)
 ─────────                                   ────────────
 AST ─► IR ─► SSA ─► regalloc ─► CodeBlock   Heap:  Gc<Object>/Gc<Function>/Gc<Shape>/Gc<CodeBlock>
                        │                      │        （size-class 堆块 + mark-sweep）
                        │ instructions         │
                        │ eh_table             │
                        │ scope_slot_count     │
                        │ inline_caches ───────┼──► 单态 IC（形状 → 槽号）
                        ▼                      ▼
                    Module（装配期）        stack: Vec<Value>  ← 寄存器 arena（locals/temps + 出参区）
                    ├ symtab                frames: Vec<Frame> ← 「帧 = 窗口(base,len) + 元数据」
                    └ func_infos            loop { step() }    ← 只有一层
                                            roots:  { 全局, stack, frames, 各注册表 }
```

一句话：**编译期把一切都变成"表 + 下标"；运行期只有"一块内存 + 一层循环 + 一组根"。**

### 4.1 堆与 GC：移植什么、砍掉什么

ChakraCore 的 57,856 行 `lib/Common/Memory/` 里，**我们只需要一两千行的东西**：

| ChakraCore | 移植？ | 理由 / 我方的形态 |
|---|---|---|
| mark-sweep（`MarkContext` + `MarkCandidate` 标记栈） | **是** | 迭代式标记（不用递归），页栈实现（`MarkContext.h:128-141`） |
| size-class 堆块 + free list（`HeapBlock` / `HeapBucket` / `SmallNormalHeapBucket`） | **是** | O(1) 分配 + O(1) 扫块回收；这是我方 `Rc`（每次 malloc/free）的替代 |
| 大对象单独走（`LargeHeapBlock`） | **是**（简化） | 超阈值 → 独立分配 + 链表 |
| `RecyclerWeakReference`（closed-addressing hash） | **是** | `WeakMap`/`WeakSet`/形状缓存/IC 都要；现实现是"条目永不回收"（`architecture.md` §2.6 明记偏差） |
| `RecyclerRootPtr` / `AutoRecyclerRootPtr`（RAII 钉根） | **是**（改形态） | Rust 里是 `Rooted<T>` + `HandleScope`（§4.2 末） |
| **写屏障**（`RecyclerPointers.h`、`RecyclerWriteBarrierManager`） | **否** | 写屏障是**并发 / 分代**的代价。STW 非移动 mark-sweep **不需要**。搬它等于把 3,000 行的机器搬进来换 0 收益 |
| 并发 / 后台标记（`StartConcurrent`、`PrepareBackgroundFindRoots`） | **否** | 解释器单线程；等 JIT/多线程真需要时再说 |
| 分代 / `HeapInfo` 的阈值调参（`RecyclerHeuristic.h`） | **部分** | 只搬"阈值触发 + 可被外部强制"这个形状；具体数值自己测 |
| `ArenaAllocator` | **是**（独立用途） | 编译期 AST/IR、临时缓冲；**不**替代 GC 堆 |
| TTD / `CustomHeap` / `PageAllocator` 的平台细节 | **否** | —— |

**我方 `Heap` 的形态（建议）**

```rust
pub struct Heap {
    /// 按 size class 组织的堆块（64 KiB 一块），每块内部是链表式 free list。
    blocks: Vec<HeapBlock>,
    buckets: [Bucket; NUM_SIZE_CLASSES],   // 每个 size class: 当前块 + free list 头
    large: Vec<LargeObject>,               // 超过阈值的对象：单独分配 + 双向链表
    mark_stack: Vec<GcHeader>,             // ChakraCore 的 MarkCandidate 栈
    grey: Vec<GcHeader>,                   // 弱引用/终结器需要二次处理的对象
    epoch: u32,                            // 对象头里的标记位只做"当前轮"判定
    bytes_allocated: usize,
    next_threshold: usize,
    stress_collect_on_alloc: bool,         // §7 的 GC stress
}

#[repr(C)]
struct GcHeader {
    /// 形状（M6 之后）或类型号；sweep 时用不到，mark 时用来找 trace 函数。
    type_tag: u16,
    mark: u16,
    size_class: u16,
    _pad: u16,
}
```

**关键取舍（逐条给出理由，避免"抄一半"）**

1. **非移动**（`Gc<T>` 是稳定地址）。移动式（compact / copying）在 Rust 里需要
   "指针必须可被更新"的全局纪律，而我们的 `Gc` 已经要被句柄纪律约束；两个纪律叠加
   是单纯的风险。ChakraCore 也是非移动。
2. **不加写屏障**。代价：不能增量、不能分代。收益：**删掉整整一类"忘了写屏障"的 bug**，
   且 sweep 之外没有额外开销。当"分配慢/停顿长"真的成为瓶颈时再加，那时是**加法**不是重写。
3. **不扫 Rust 栈**。Rust 没有可移植的栈扫描（值可能只在寄存器里，且优化器可以任意重排）。
   所以走**显式 rooting**（§4.2 末）。ChakraCore 靠 C++ 的栈扫描 + `Field()` 纪律；
   我们不能照搬那一半，**只能照搬它的纪律精神**。
4. **收尾靠 GC stress 而不是靠审阅**。见 §7.2。

**触发策略**：`bytes_allocated > next_threshold` 时收集；阈值按存活量调整
（`next = max(live * 2, MIN)`）。另外 `Cx` 提供 `cx.maybe_gc()` 供长循环显式让路。
**先不做增量/并发**，但**接口留好**（`CollectKind::{Normal, Stress}`）。

### 4.2 `Value`：NaN-boxing 位图（我方设计）

**为什么**：ChakraCore 的 `Var` 是 8 B（§2.3）。我们 24 B，且 `Object` 变体是胖指针
（`Rc<RefCell<dyn JSObject>>` → vtable + 借用标志）。属性访问与算术是解释器的全部热路径，
24 B vs 8 B 直接决定寄存器 arena 的容量与缓存占用。

**我方位图**（`u64`，非照抄 ChakraCore 的 `AtomTag`）：

```rust
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Value(u64);

// 空间划分（按高 16 位 t = bits >> 48 判定，**先判 NaN 特例，再判 tag，最后当 double**）
//
//   t == 0x7FF8 && low48 == 0                  → canonical NaN  ← 全局唯一的 NaN 位型
//   t == 0x7FF9                                → Object      （low48 = 48 位 Gc 句柄）
//   t == 0x7FFA                                → Int32       （low32 = i32）
//   t == 0x7FFB                                → 单例立即数（见下）
//   t == 0x7FFC                                → String      （low48 = Gc 句柄）
//   t == 0x7FFD                                → Symbol      （low48 = Gc 句柄）
//   t == 0x7FFE / 0x7FFF                       → 保留（BigInt / 内部标记）
//   其余（t ∈ [0x0000,0x7FF7] ∪ [0x8000,0xFFF7]）→ double（原始 f64 位型，含 ±0 / ±Inf / 次正规）
//
// 为什么不能简单写成 `t < 0x7FF8` 就是 double：负数的符号位使 t ≥ 0x8000，
// 所以"高位小于阈值"会把 -1.0 误判成立即数。必须**两端都排除 NaN 空间**——
// 而 NaN 空间的边界恰好就是"指数全 1"：+NaN 落在 [0x7FF8,0x7FFF]，-NaN 落在 [0xFFF8,0xFFFF]。
// 于是只需把 [+Inf, -Inf]（t = 0x7FF0 / 0xFFF0）留下当 double，其余 NaN 位型全部**规范化掉**。

const T_OBJ: u64 = 0x7FF9; /* … */

// t == 0x7FFB 下的单例（low48 下标）：
pub const UNDEFINED:  u64 = 0x7FFB_0000_0000_0000;
pub const NULL:       u64 = 0x7FFB_0000_0000_0001;
pub const FALSE:      u64 = 0x7FFB_0000_0000_0002;
pub const TRUE:       u64 = 0x7FFB_0000_0000_0003;
pub const UNINITIALIZED: u64 = 0x7FFB_0000_0000_0004;  // TDZ / 空绑定
pub const HOLE:       u64 = 0x7FFB_0000_0000_0005;     // 数组 elision / 缺参
```

**三条不变量（必须有测试钉住）**

1. **NaN 规范化**：所有产生 `f64` 的入口（`Value::number` / 算术 / `Number(...)` /
   内建数值函数）都必须把 NaN 折成 `canonical NaN`。`debug_assert` 在
   `Value::number` 里检查：`!bits_in_nan_space || bits == CANONICAL_NAN`。
   没有这条，`t` 判定就会把某个 NaN 读成立即数。
2. **句柄是下标不是指针**（低 48 位）。这样"值能不能装进寄存器"与平台指针宽度无关，
   也便于 debug 时把一个 `Value` 直接打印成 `Obj#1234`。
   （若最终选裸指针，48 位地址空间的假设必须写成一条 assert。）
3. **没有 `Value::Function(u32)`**：函数就是 `Gc<Object>`（对象头里有 `entry`，§4.6）。
   双表示导致的 `strict_eq` 特判、`materialize_function`、`callable_func_id` 一起消失。

**白拿的两个收益**

- `UNINITIALIZED` 变成立即数 → **TDZ 不再需要特判**（`script_env: HashMap<String, Option<Value>>`
  的 `None` 语义直接变成一个值）。
- `HOLE` 变成立即数 → **`MarkHole` 指令可以删掉**，数组 elision 直接写 `HOLE`
  （`interpreter-refactor.md` 里"数组字面量的省略元素不出 hole"这条欠账从"五层改动"
  降成"一个常量"）。

**代价与陷阱**

- **全库 `match Value` 都要改**（`value.rs` / `vm/*` / `builtins/*`）。这比 P0a 的
  `Opcode` 替换更大，但性质相同：**改完核心类型，编译器会列出全部位点**。
  按 `interpreter-refactor.md` §0.1 的经验，**不要分批**：半迁移状态的成本大于一次铺开。
- `Debug`/`Display` 会变得难写：提供 `Value::kind_name()` 与 `value_to_debug_string(cx, v)`
  两个**唯一出口**，不要在每个报错点各写一遍。
- 与 `f64` 的边界：`-0.0` 必须保留（`Object.is(-0, 0)` 为 false），
  而 `0.0` 与 `-0.0` 的位型不同 → 位判定天然正确，**不要**做"归一化零"。

### 4.3 帧与执行模型（M2，收益最大的一步）

**ChakraCore 的什么**：`m_localSlots[0]` 柔性数组（§2.4）——寄存器在帧内、下标寻址、
帧可整块搬走；帧链 `previousInterpreterFrame`；出参区 `m_outParams`。

**Rust 怎么表达**

```rust
/// 一帧 = 「在共享寄存器 arena 里的一段窗口」+ 「元数据」。**没有自己的 Vec。**
pub struct Frame {
    pub code: Gc<CodeBlock>,          // 当前函数的字节码 / 常量 / EH 表 / flags
    pub func: Value,                  // 被调方（`super` 的 home object、`new.target` 判定）
    pub pc: u32,
    pub base: u32,                    // 窗口在 `stack` 里的起点
    pub locals_len: u32,              // 窗口长度（regalloc 决定）
    pub argc: u32,
    pub this: Value,
    pub this_state: ThisState,        // Uninitialized / Initialized（派生类构造器）
    pub new_target: Value,
    pub env: Option<Gc<Environment>>, // M7
    pub try_stack: SmallVec<[TryFrame; 4]>,
    /// 帧的"调用方写的目标寄存器"：`Ret` 用它把 Rv 送回调用方。
    pub dst: RegId,
    pub return_frame: u32,            // 调用方帧的下标（省掉"在 ctrl_stack 里存 return_pc"）
}

pub struct State {
    /// 寄存器 arena **兼** 出参区：ChakraCore 把 `m_localSlots` 与 `m_outParams` 分开，
    /// 我们合成一条 `Vec<Value>`，用 `base` / `sp` 区分（现 `data_stack` + `rsp`/`rbp` 的升级版）。
    stack: Vec<Value>,
    frames: Vec<Frame>,               // 替代 9 条平行栈 + ctrl_stack
    sp: u32,                          // 下一个空闲槽
    // 全局对象 / 声明记录 / 微任务队列 / 各注册表 —— 保留，但它们不再是"每帧要同步的栈"
}
```

**访问一律索引式**（borrowck 会逼出这个写法）：

```rust
// 读寄存器 r（本帧）
let v = self.stack[(self.frames[f].base + r) as usize];
// 写寄存器 r
self.stack[(self.frames[f].base + r) as usize] = v;
```

**单层主循环**

```rust
fn run_until(&mut self, depth_boundary: usize) -> Completion {
    loop {
        if self.frames.len() == depth_boundary { return Ok(self.rv()); }
        let f = self.frames.len() - 1;
        let inst = self.frames[f].code.instructions[self.frames[f].pc as usize];
        self.frames[f].pc += 1;
        match inst {
            Instr::Ret { .. } => { /* 把 Rv 写回调用方 dst；pop 帧；若回到边界则返回 */ }
            Instr::Call { .. } => { /* 压帧，continue —— 不递归、不嵌套循环 */ }
            // …
        }
    }
}
```

**这一步删掉的东西（逐条）**

| 删掉 | 为什么不再需要 |
|---|---|
| 9 条平行栈 + `ctrl_stack` | 全部进 `Frame`；深度就是 `frames.len()` |
| 哨兵 pc（`instructions.len()`） | 循环的终止条件是 `frames.len() == boundary`，不是 pc 越界 |
| `PushC` / `PopC` / `MovC`（各 7 处）+ `AddC`/`SubC` | 它们只为保存/恢复**Rust 驱动帧**而存在 |
| 4 层嵌套 `step` 循环 | 一层。`invoke` 变成"跑到 `frames.len() == boundary`" |
| 全局 `registers: [Value; 19]` | 寄存器在帧的窗口里 |
| test262 runner 的 256 MB 栈 | Rust 栈不再等于 JS 栈（只受 `MAX_CALL_DEPTH` 与 `stack` 容量约束） |
| `invoke_boundaries` | `frames.len()` 就是边界；"帧深度必须在装帧之前取"**结构性消失** |
| `PushC/PopC` 携带的 `saved_seh_depth` / `saved_closure_depth` | `Frame.try_stack` / `Frame.env` 自带 |

**必须同时改的东西（清单，漏一项就会出诡异 bug）**

- `Ret` 的**构造返回语义**（"返回对象才覆盖 `this`"）：现在是 `construct_stack` 的替身，
  改成 `Frame.this_state` + 判定。
- 派生类 `this_state`（`super()` 未绑定时返回要抛 `ReferenceError`）。
- `super()` 与 `Reflect.construct` 的 `new.target` 传递（B46 刚接的线）。
- 生成器挂起/恢复（§4.9 一并改成堆帧，**不要**先按老模型改一版）。
- `async`/`await`（`await` 就地排空微任务队列）；微任务在**帧栈为空**时排空
  （现为"顶层 `run` 末尾 + `await` 内"）。
- `IteratorClose`（`delegate_stack` → 帧内状态或显式局部）。
- 原生回调的异常边界（`invoke_boundaries` → `frames.len()`）。
- 三层护栏的会计（步数/墙钟/堆）：单层循环下计数点更少更准。
- `VM::run` 的状态重置清单（B37 修的 OOM：必须重置"一次运行触及的全部状态"）
  —— M8 的 `Realm` 拆分会让这条**整条消失**（新 realm 就是新对象，不用重置）。

**陷阱**

1. **不要给 `Frame` 放 `regs: Vec<Value>`**（每次调用一次 malloc）。就是一整块 arena + 窗口。
2. **`regs` 与 `frames` 两个字段不能同时可变借用**：所有 arm 写成 `self.stack[i]` /
   `self.frames[i]`。这是 M2 最耗时的机械劳动，**先只改 3–5 条指令验证写法再铺开**。
3. **`Frame` 必须可整块移动**（为了 M8 的生成器）：所以它**不能**持有指向 `self.stack` 的
   裸指针；`base` 是下标，搬帧时把窗口内容一并搬走（或让生成器帧**独占**一段 arena）。
   **建议后者**：生成器帧的窗口不与其他帧共享，于是"搬帧"= 移动一段 `Vec<Value>`。
4. **`this` 快照顺序**：现在 `New` 的开帧顺序与其它 5 处相反（`this_val` 先被覆盖、
   `enter_frame` 后跑），是 `interpreter-refactor.md` §3.3.3 的待查项。
   M2 重写时**必须把这个语义决定下来并写成测试**，不能"顺手抄旧顺序"。
5. **顶层脚本也是一个帧**（`code` = 顶层 `CodeBlock`），否则"边界"要特判。

### 4.4 对象与形状（M6）

**ChakraCore 的什么**：`Type{typeId, flags, prototype, entryPoint}` + `DynamicTypeHandler`
（形状，可共享）+ `PathTypeHandler`（前缀共享链 + 内联槽）+ `PropertyId` 驻留 + 内联缓存（§2.7）。

**Rust 怎么表达**

```rust
/// 形状：不可变、可共享、可 GC。等价于 ChakraCore 的 `DynamicType` + `DynamicTypeHandler` 合并。
pub struct Shape {
    pub id: ShapeId,
    pub proto: Option<Value>,            // 等价于 ChakraCore `Type::prototype`
    pub flags: ShapeFlags,               // extensible / 数组元素种类 / 构造器 / 调用器种类…
    pub kind: ShapeKind,
    /// 转换表：`(key, attrs)` → 新形状。等价于 ChakraCore 的 `TypePropertyCache` / `PathType` 延续。
    pub transitions: FxHashMap<(Atom, u8), ShapeId>,
}

pub enum ShapeKind {
    /// **前缀共享**（PathType 的思想）：父形状 + 尾部一个属性。
    /// 于是 `{a,b,c}` 与 `{a,b,d}` 共享前两个属性的槽位。
    Extend { parent: ShapeId, key: Atom, attrs: u8, slot: u32 },
    /// 属性过多 / 发生删除后：转字典（ChakraCore 的 `DictionaryTypeHandler`）。
    Dictionary { table: IndexMap<Atom, PropEntry> },
    /// 内建专用形状（对应 ChakraCore 的一族专用 TypeHandler）。
    Array { elements: ElementKind, length_writable: bool },
    Proxy { target: Value, handler: Value, revoked: bool },
    // …TypedArray / Arguments / Generator 各自一个变体
}

/// 对象：**一个形状号 + 一段槽**。没有 `HashMap`。
#[repr(C)]
pub struct Object {
    header: ObjectHeader,     // { shape: u32, _pad }
    /// 内联槽：长度由形状决定（ChakraCore 的 inline slot）。
    /// 用 `SmallVec` 起步、超出后指向堆上的 aux 段（ChakraCore 的 `GetAuxSlot`）。
    slots: SlotVec,
    /// 只有需要时才有（数组元素、Map 条目、TypedArray 数据）
    extra: ExtraSlots,
}
```

**属性读的唯一路径**

```rust
fn get(cx: &mut Cx, mut o: Value, key: Atom) -> Completion<Value> {
    loop {
        let obj = cx.obj(o);
        let shape = cx.shape(obj.header.shape);
        match shape.kind.find_slot(key) {      // Extend 沿 parent 链；Dictionary 走表
            SlotHit::Found(slot) => return Ok(obj.slots[slot]),
            SlotHit::Accessor { getter, .. } => return call_getter(cx, getter, o),
            SlotHit::Absent => {}
        }
        match shape.proto {
            Some(p) if cx.is_object(p) => o = p,
            _ => return Ok(Value::UNDEFINED),
        }
    }
}
```

**内联缓存（IC）—— 必做，不是后续优化**

```rust
// CodeBlock 里每条属性访问指令带一个 IC 槽（ChakraCore: m_functionBody->inlineCaches）
pub struct GetPropIc {
    /// 单态起步；命中即「一次 u32 比较 + 一次数组索引」。
    mono: Cell<Option<(ShapeId, u32)>>,
    /// 多态（ChakraCore 的 PolymorphicInlineCache）；超过阈值退化成 megamorphic（不缓存）。
    poly: RefCell<SmallVec<[(ShapeId, u32); 4]>>,
}
```

**迁移与陷阱**

1. **形状与原型的关系**：ChakraCore 把 `prototype` 放在 `Type` 上，所以**改原型 = 换形状**
   （`TypeFlagMask_SkipsPrototype` 是它的加速器）。我方照做，于是
   "原型链上的原型被改"必须让 IC 失效 —— **用 `proto_epoch` 计数器**
   （在 `setPrototypeOf` 与 `__proto__` 上自增；每次对象读都比一下），比逐层检查便宜。
2. **`ObjectKind` 不要一次删掉**：先把它变成 `Shape.flags` 上的一个 tag
   （`shape.flags.kind()`），VM 里的 21 变体 `match` 逐个改到"读形状"。
   **最终目标是：新增一种对象类型 = 加一个 `ShapeKind` 变体 + 一个自然的方法表，
   不改 VM**。这是对 `architecture.md` §3.1"7 步清单"的回答。
3. **数组**：`ArrayObject` 的 `elements: Vec<Value>` + `holes: BTreeSet<usize>` 是现成的
   好设计（密集存储 + 洞）。M6 要做的只是把它挂到 `ShapeKind::Array` 下、
   把 `elements` 里放 `Value::HOLE`（M5 提供的立即数）而**不是**再维护一个 `BTreeSet`。
4. **`PropertyKey` 驻留**：`AtomTable { map: FxHashMap<Box<str>, Atom>, atoms: Vec<Box<str>> }`，
   `Atom(u32)`。所有属性名、形状转换表、IC 都用 `Atom`。**这一步会顺带加速 `for-in` 与 `Object.keys`**。
5. **内联槽的溢出**：对象创建时不知道会加多少属性。ChakraCore 的
   `IsObjectHeaderInlinedTypeHandler()` 决定槽在头内还是头后。我方建议
   **内联小数组（SmallVec 起步 4–8）+ 超出转 aux 段**，形状里记 `slots_len` 与
   `aux_len`；这样"加第 5 个属性"不会让已有对象的地址变化。
6. **删除属性**：ChakraCore 走 `IsCompatiblePropertyDescriptor` 后可能转字典。
   我方照做（`ShapeKind::Dictionary`），但要**先测**再定阈值：删除很少见，转字典的代价可能
   大于"留一个 tombstone 槽"。
7. **`dyn JSObject` 的消失**（与 M3 同一步完成）：`Object` 是**具体类型**，
   方法表按 `ShapeKind` 分派（ChakraCore 的 `TypeHandler` 虚函数）。
   于是"对象层看不到原型链 / 跑不了 JS"这个限制**只剩一层**：对象层与 VM 的分工变成
   "属性查找 vs 需要跑 JS 的操作"，而**重入不再 panic**（因为不再有 `RefCell`）。
   这条是 §3.3 最后一个地雷（Proxy 8 个截获点）的正解。

### 4.5 调用与分派（M2 起，M6 收口）

**ChakraCore 的什么**：`GetEntryPoint()`（§2.5）——可调用性是一个函数指针。

**Rust 怎么表达**

```rust
/// 所有可调用体共用的入口签名。`Construct` 只是多一个 `new_target`。
pub type EntryPoint = fn(&mut Cx, callee: Value, args: &Args, new_target: Option<Value>)
    -> Completion;

#[repr(C)]
pub struct FunctionObject {
    object: Object,
    entry: EntryPoint,
    kind: FunctionKind,          // Interpreted{code} | Native{id} | Bound | Proxy | ClassCtor
    flags: FnFlags,              // Generator | Async | DerivedCtor | Arrow | Strict | Constructable
    home: Option<Value>,         // `super` 的 home object
    env: Option<Gc<Environment>>,// M7 闭包环境
    bound: Option<BoundInfo>,    // Function.prototype.bind 的产物
}

#[inline]
fn call(cx: &mut Cx, f: Value, this: Value, args: &Args, nt: Option<Value>) -> Completion {
    cx.func(f).entry(cx, f, this, args, nt)   // 一次间接调用。没有字符串、没有名字匹配。
}
```

**这一步与 M2 的接口约定**

`enter_call` 现有的协议（`Control::{Entered, Done}` + `CallArgs`）**保留其形状**，
但语义变化是：

| 现在 | M2 之后 |
|---|---|
| `Entered` = "帧已压好，回到分派循环"（特例） | **"压帧"是唯一形态**：所有可调用体都在 `stack` 上开窗口，`Run` 帧的是解释器循环，`Native` 帧的是"直接算完但不 pop"（或用 `Done` 表达"不压帧"） |
| `CallArgs::{OnStack, OnStackWithCopy, Collected}` | `Args` = 帧上的窗口 `(base, argc)`。**不再有"收集一份 Vec"这条路**：`Collected` 的存在本身是"参数也可以不在栈上"的历史包袱 |
| `dst: Register` + `return_pc` | `Frame.dst` + `Frame.return_frame`（帧里带，不靠调用方记账） |
| `callee_kind` 六级 `match` 在 `enter_call` 里 | 第一次仍是 `match`（读 `FunctionObject.kind`），之后**没有第二次**：原生函数的"名字前缀"约定全部消失 |

**必须删掉的东西（一次删干净，否则会留下"第三种分发"）**

- `call_native_by_name`、`native_function_name`、`builtin_arity`（arity 进 `FunctionObject`）。
- `PROTO_METHOD_PREFIX` / `__bound__` / `MAP_METHOD_PREFIX` / `CLASS_CTOR_FLAG`。
- `vm_handled_static` 名单（`Object.*` 的静态方法**就是**原生函数，走同一条路）。
- `try_array_callback_method` / `call_prototype_method` 的"按名字查表 + 接收者守卫"。
- `materialize_function` / `callable_func_id`（函数不再是 `Value::Function(u32)`）。

**陷阱**

- **原生函数不再是"一个名字"**，而是一个 `id` + 一张 `&'static [NativeMethod]` 表。
  建议 `NativeMethod { name: &'static str, length: u32, entry: EntryPoint }`，
  注册期建好、运行期直接跳。**arity 与 `name` 是 `FunctionObject` 的字段**，
  这样 `f.length` / `f.name` 不再需要查表。
- `Function.prototype.bind`：`BoundInfo` 让 `entry` 仍是**同一个原生入口**
  （只是多一层参数拼接），不要为它造第 N 种 callee 种类。
- `[[Construct]]` 的判定：`FnFlags::Constructable`，**不是** `kind` 的第三个分支。
  `is_constructor` 里"生成器 / async 不是构造器"这条（ES 7.3.20）现在是**一个 flag 检查**。

### 4.6 异常处理（M1 定表，M2 用表）

**ChakraCore 的什么**：区域元数据 + `ProcessTryFinally(ip, jumpOffset, regException, regOffset, hasYield)`
（带续体进入）+ 帧上"我在第几层 try"（§2.6）。

**编译期：`EhTable` 挂在 `CodeBlock` 上，由 lowering 回填**

```rust
pub struct EhTable { pub regions: Vec<EhRegion> }

pub struct EhRegion {
    /// 受保护范围 [start, end)（半开）。**由 lowering 回填，不是扫 EndTry 派生**（§5.1）。
    pub start: Pc,
    pub end: Pc,
    /// 正常出口（`Leave` 的目标）：try 体正常结束时去哪。
    pub normal_exit: Pc,
    /// catch 块（无则 None）。
    pub catch: Option<Pc>,
    /// finally 块（无则 None）。
    pub finally: Option<Pc>,
    /// **区域内的逃逸点**（break/continue/return 的 `Leave` 指令 pc）→ 用于 finally 链。
    /// ChakraCore 没有这个字段（它的 `Leave` 是内联指令）；我们把它显式化，
    /// 因为"哪些 finally 还没跑"这个问题必须在**编译期**回答一次。
    pub exits: SmallVec<[Pc; 4]>,
    pub parent: Option<RegionId>,     // 嵌套（对应 EHBailoutData 的 parent/child）
}
```

**运行期：帧上的 try 栈 + completion 传递**

```rust
pub enum TryFrame {
    Catch { region: RegionId },
    Finally { region: RegionId, pending: Option<Completion> },  // ★ 续体在这里，不在 SehRecord 的 4 个 bool/字段里
}
```

- `throw v` = `Completion::Throw(v)`；unwinding 由 `run_until` 的返回路径驱动：
  离开帧时把 `Err` 交回调用方；**同一个帧内**先走本帧的 `try_stack`，
  最内层区域若带 finally → `pending = Some(Throw(v))`，`pc = finally`；
  finally 末尾的 `ResumeExc` 检查 `pending`：
  - 有 catch 且没跑过 → `pending = None`，`pc = catch`；
  - 否则 → 继续 unwind（如果有外层区域 → 进外层；否则**返回 `Err` 给调用方帧**）。

**这一步解决 v1 §10.3 的全部遗留**

v1 §10.3 的四项改动（`PushSeh` 加 `body` 字段 / `Try` 加第三个 `end_offset` / 用 `exit_edges`
回填 / 记"多出口"为已知限制）在 M1 里做，但**第四项不再是限制**：
既然 lowering 手上就有 `SehFrameInfo { finally_blk, pending_exits, exit_edges }`，
那就把 **`exits` 整个集合**写进表。于是：

- `EhRegion::end` 从"扫 `EndTry` 猜出来的"变成"lowering 声明的"；
- `ResumeExc` 不再需要扫 `seh_stack` 找"哪个 finally 还没跑"；
- `DelayedJump` / `delayed_return` / `delayed_jump_target` / `catch_executed` /
  `in_finally` / `pending_exception` **全部作废**。

⇒ `SehRecord` 的 14 个字段缩减为 `TryFrame` 的 `(region, pending)` 两项，
且 `Clone` 的需求随 M8 的堆帧一起消失。

**陷阱**

1. **try/finally 无 catch 时"handler 与 finally 同址"**：lowering 里
   `seh_handler = if has_catch { catch_blk } else { finally_blk }`。
   新表要**显式区分** `catch: None` 与 `catch: Some(finally_pc)`，
   否则 `ResumeExc` 会把异常"捕获"到 finally 自己（现用 `handler_pc != finally_pc` 绕开）。
2. **`yield`/`await` 在 try 内的挂起**：`Frame.try_stack` 随帧一起搬进生成器对象（M8），
   所以"挂起在 try 里"变成免费的。**这正是 `hasYield` 参数在 ChakraCore 里要特判的原因**。
3. **`ResumeExc` 的 `delayed_return` 分支**（`architecture.md` §2.3 明写：
   "必须显式跳回 `Module::exit_pc`，不能顺着往下落"）：completion 传递后，
   这个"必须显式"变成**天然**——`pending` 里存的是 `Return(v)`，续体是明确的 pc。
4. **区域与 pc 的一致性**：M2 之后，`pc` 只用于"取指令"和"查 `exits`"，
   不再用于"当返回地址"。所以**跳转可以相对化**（`interpreter-refactor.md` §5.6 说
   "不要与 P0 混做"——M2 就是它该做的时机）。

### 4.7 作用域与闭包（M7，对应一条现存语义 bug）

**ChakraCore 的什么**：作用域槽是 **RegisterSlot 编号**（§2.8 的
`GetPropertyIdsForScopeSlotArray`），嵌套函数通过 `FrameDisplay` / `innerScopeArray`
访问外层帧的槽位。**不是"把外层变量的值拷进一个 map"**。

**Rust 怎么表达**

```rust
/// 一个作用域环境：**定长槽数组** + 外层链。槽号在编译期分配（`(depth, index)`）。
pub struct Environment {
    slots: SlotVec<Value>,
    parent: Option<Gc<Environment>>,
    /// 该环境是否"需要逃逸"（被闭包捕获）。不逃逸的环境可以直接放在帧的窗口里，
    /// 逃逸的才分配。这正是"per-iteration 绑定"的实现方式：
    /// 每轮循环头**新建**一个逃逸环境，于是闭包各自看到自己那一轮。
    escaped: bool,
}

// CodeBlock 上：
pub struct ScopeLayout {
    pub slot_count: u32,
    /// 每个被捕获的绑定：名字 → (深度, 槽号)。编译期一次算好。
    pub captures: FxHashMap<Atom, (u8, u16)>,
}
```

**这一步修掉的语义债（逐条对照 §3.4）**

| 现状 | M7 之后 |
|---|---|
| `closure_var_stack: Vec<HashMap<String, Value>>`（每帧 Hash 表） | `Environment` 链（定长槽 + 下标访问） |
| **闭包是创建时值快照** → `counter()` 返回 `0,0,0` | 闭包捕获的是**环境对象**，共享同一个 `slot`；`counter()` 返回 `1,2,3` |
| TDZ 靠 `HashMap<String, Option<Value>>` 的 `None` | `Value::UNINITIALIZED`（M5 的立即数） |
| per-iteration 绑定未做 | 循环头新建逃逸环境 |
| `SuspendedFrame.closure_maps` 要单独拷 | 环境在 GC 堆上，生成器只持有 `Gc<Environment>`（一个值） |

**为什么这条路在我们这里是可行的（而 ChakraCore 之外很多引擎不敢走）**

- **没有 `eval` / `with` / `new Function`**（`handover.md` §1 明写"永久排除"）。
  于是"一个名字对应哪个槽"是**完全静态**的 —— 否则就得有运行时名字查找。
  这是本项目历史上少有的"排除掉的功能反而解锁了正确架构"的例子，**要在文档里写明理由**，
  免得后人以为可以直接加回 `eval`。
- `dyn` 捕获（`docs/dyn-capture.md`）的那份文档描述的"创建时快照"是**实现限制**，
  不是设计目标；M7 之后这份文档应改为"已支持共享绑定"，并附上对照 node 的探针。

**陷阱**

1. **逃逸分析**：不是所有环境都要上堆。"被闭包引用 / 被 `for`-iteration 引用"才逃逸。
   不逃逸的环境可以直接用帧窗口里的槽（`Frame.env` 为空即"当前环境就在窗口里"）。
   **先做最简版**（全部逃逸），把 `escaped` 字段留好，别一次做优化。
2. **编译期要新增一个 pass**：`scope_layout` —— 从 lowering 产出的 IR 上算
   "哪些变量被嵌套函数引用"，并分配 `(depth, index)`。这是 M7 唯一的新编译器组件，
   与 `regalloc` 同层，**不要塞进 lowering**。
3. **`Environment` 必须被 GC 追**：它是 `Gc<Object>` 之外的一种对象（或做成 `ShapeKind::Env`）。
   选后者更省一处机制。

### 4.8 生成器 / async（M8，依赖 M2 的"可搬帧"）

**ChakraCore 的什么**：`JavascriptGenerator { frame, state, args, scriptFunction, resumeYieldObject }`
（§2.9）—— **挂起态就是那个没弹掉的帧**。

**Rust 怎么表达**

```rust
pub enum GeneratorState { SuspendedStart, Suspended, Executing, Completed }

pub struct Generator {
    object: Object,
    state: GeneratorState,
    /// ★ 整块搬走的帧。因为 M2 的 `Frame` 只持有下标，搬帧 = 移动窗口内容 + 改 base。
    frame: Option<Box<HeapFrame>>,
}

/// 生成器帧**独占**一段寄存器（不与其他帧共享 `stack`），所以能整体移动。
pub struct HeapFrame {
    frame: Frame,
    slots: Box<[Value]>,      // 本帧的寄存器窗口（从 `stack` 里切出来带走）
}
```

**流程（与 ChakraCore 对齐）**

| 动作 | ChakraCore | 我方 M8 |
|---|---|---|
| 调用生成器函数 | 建 `JavascriptGenerator`，`SuspendedStart` | 建 `Generator`，`state = SuspendedStart`，`frame = None` |
| 首次 `next(v)` | 跑 prologue + 到第一个 `yield` | 开帧（`HeapFrame`），跑到 `Yield` |
| `yield` | 帧留在对象里，`state = Suspended` | 把窗口从 `stack` 切进 `HeapFrame`，`state = Suspended` |
| 恢复 | `SetFrame` 装回 + `CallGenerator(Normal)` | 把 `HeapFrame` 装回 `frames`，续 `pc` |
| `return()` / `throw()` | `ResumeYieldKind::{Return,Throw}` + `EntryReturn/EntryThrow` | `pending` completion 从**调用点**传进帧（不是从 `seh_stack` 摸） |
| 完成 | `SetState(Completed)` 清 frame/args/scriptFunction | 一样 |

**这一步删掉的东西**

- `SuspendedFrame` 的**全量拷贝清单**（`data` / `argc` / `bp_offset` / `pc` / `this` /
  `function_val` / `closure_maps` / **`registers[19]`** / `delegates` / **`seh`**）
  —— 一个值替代（`Box<HeapFrame>`），"漏拷一项"这类地雷**结构性消失**。
- `exit_pc`（`Module.exit_pc`）**整个映射**：它存在是因为"挂起点是 `Yield`、
  没有自己的 `Ret`，所以 return 完成要跳过 `Module.exit_pc` 去跑 finally"。
  有了 `EhRegion.exits` + completion，`Return` 是**显式续体**，不需要"找到那个函数的 `Ret`"。
- `PrologueEnd` 泊车 + `running_generator_prologue` 标志（B17/B29 两个 bug 的来源）：
  参数绑定时机变成"开帧时"，而"开帧"就是 `next()` 时（生成器不共享父帧）。
  **这一条要在 M8 用 B17/B29 的测试复验**，因为它是行为变更。
- `invoke_boundaries`（生成器恢复不再需要"边界"这个概念，`frames.len()` 就是）。
- `resume_sentinel()`。
- 生成器/迭代器注册表（`generator_registry` / `iterator_registry`）：
  现在它们是 `HashMap<u64, Value>`（把状态从 `Value` 里挤到 VM 字段上），
  M8 之后生成器就是普通对象，**注册表整条删除**。

**async**：与生成器同一条机器（ChakraCore 里 `JavascriptAsyncFunction` 也是），
差别只在"完成时 settle 一个 promise"。**不要为 async 维护第二套帧存取**。
微任务队列在 `frames.is_empty()` 时排空。

### 4.9 错误通道（M8，可与 M5 同时做）

**目标**：把 `Result<_, String>` 换成"异常就是一个 JS 值"。

```rust
/// 异常就是一个 JS 值（ChakraCore 的 `JavascriptExceptionObject`）。
pub struct Exception(pub Value);

pub type Completion<T = Value> = Result<T, Exception>;
```

- 于是 `RuntimeError::TypeError(String)` / `into_property_error()` / `from_property_error()`
  / `"RangeError: <msg>"` 字符串往返**全部消失**。
- **形态变化**：`JSObject::define_property` / `property_set` / `prototype::*` 的签名从
  `Result<(), String>` 变成 `Completion<()>`，并且**失败时自己构造正确的 Error 对象**。
  `ArraySetLength` 直接造 `RangeError`，不再靠调用方猜。
- 代价：这些函数要能**分配**（造 Error 对象），所以签名里要带 `&mut Cx`。
  这正是 M3 之后自然的样子（现在它们不能分配，所以只能返回 `String` —— 这条因果要写下来）。

### 4.10 编译期：`CodeBlock` 与 `Module` 的分工（M1）

```rust
/// 一个函数自包含的全部运行期所需（ChakraCore 的 `FunctionBody` 精简版）。
pub struct CodeBlock {
    pub body: BodyRange,                    // [start, end)
    pub instructions: Vec<Instr>,
    pub constants: Vec<Constant>,
    pub name: Box<str>,
    pub length: u32,                        // 形参个数
    pub flags: FnFlags,                     // Generator | Async | DerivedCtor | …
    pub eh_table: EhTable,                  // ★ 由 lowering 回填
    pub scope_layout: ScopeLayout,          // ★ M7
    pub inline_caches: Vec<IC>,             // ★ M6（ChakraCore 的 m_functionBody->inlineCaches）
    pub nested: Vec<FuncId>,                // ChakraCore 的 nestedArray
    pub exit_pc: Option<Pc>,                // M8 之后可删
}

/// `Module` 降级为**装配期产物**：编译结束后它只被用来把 `CodeBlock` 装进函数对象。
/// **运行期不再有 `module: &Module` 参数**（`interpreter-refactor.md` §3.4 的 44 处扇出从此消失）。
pub struct Module {
    pub code_blocks: Vec<CodeBlock>,
    pub entry: FuncId,
}
```

**三条纪律**

1. **运行期不许出现 `Module`**：M1 的做法是"先在 `State` 里放 `current_code`，逐个替换，
   再删参数"（`interpreter-refactor.md` §3.4 的风险对策，保留）。
2. **`generators` / `asyncs` / `derived_ctors` 三个 `HashSet` 与 `exit_pc` 映射删除**，
   改读 `CodeBlock.flags` / `CodeBlock.exit_pc`。
3. **`FunctionObject` 持有 `Gc<CodeBlock>`**，帧从**当前帧**取，不从参数取。

**`Module` 里的 `debug_instructions` 保留**（P0a 的订正：它带的是 IR 行信息，
是 dump 里唯一能看懂数据流的那一半）。

### 4.11 Realm / 宿主边界（M8）

**ChakraCore 的什么**：`ThreadContext`（isolate：堆 / 线程级共享）与
`ScriptContext`（realm：全局 / 内建 / 模块记录）分离。

**Rust 怎么表达**

```rust
/// 引擎级（等价 ChakraCore `ThreadContext`）：**跨 realm 共享**。
pub struct Engine {
    heap: Heap,
    atoms: AtomTable,
    shapes: ShapeTable,          // 形状在 realm 间可共享（ChakraCore 的形状挂在 Type 上，跨 context 有讲究）
    stack: Vec<Value>,           // 寄存器 arena（跨 realm 复用，容量按最大帧调整）
    frames: Vec<Frame>,
    call_depth: u32,
    budgets: Budgets,            // 步数 / 墙钟 / 堆
}

/// realm 级（等价 ChakraCore `ScriptContext`）。
pub struct Realm {
    global: Value,
    intrinsics: Intrinsics,      // Object/Array/Function/... 的原型与构造器
    global_env: Gc<Environment>, // M7
}

/// 一次运行（等价 ChakraCore 的调用上下文）。
pub struct Cx<'a> {
    engine: &'a mut Engine,
    realm: &'a mut Realm,
}
```

**收益（具体，不是"更干净"）**

1. **test262 runner 不再需要"每用例新建 VM"**：现在必须新建，因为用例会改内建对象
   （`handover.md` §4 明写"realm 新鲜仍比状态重置更硬 —— 实测复用只有 14002 通过"）。
   拆开之后 **新建 `Realm` + 复用 `Engine.heap`/`atoms`/`shapes`/`stack`**。
   这是**整轮回归的时间**级别的收益。
2. **`Gc` 的根集变清晰**：`Engine` 持有 `stack`/`frames`/`heap`，
   `Realm` 持有 `global`/`intrinsics`/`global_env`。于是"根 = 这两个结构可达的对象"
   —— 而不是现在"`State` 里 9 条栈 + VM 上 4 个注册表 + 手写断环"。
3. **B37 那类"忘了重置某项状态"的 OOM 不再可能**：新 `Realm` 就是新对象，
   没有"重置清单"可以漏。

**陷阱**：形状在 realm 间共享时，形状里如果有指向 `Realm` 的引用（比如内建函数的槽），
就不能全局共享。**建议 M6 先让形状 per-realm**（放在 `Realm` 里），等多 realm 真有收益
再谈共享。

---

## 5. 分步施工

每一步独立分支、独立提交。提交信息按 §7.3 列出"允许的回归项"。

### 5.1 M1：编译期自包含 + EH 表可信

**为什么必须先做**：M2 的 `run_until` 要在"离开帧 / 遇到 `Throw`"时找 handler，
而 handler 只能来自 `EhTable`。**而 `EhTable` 不可信（`EndTry`/`Try` 不配对），
`cheat` 不了。** 这是 M1 的唯一硬理由。

**v1 §10.3 的实测结论（保留原文，不要重述）**

给 `EndTry` 加 `debug_assert!(eh_region_ending_at(pc).is_some())` 时，在
"嵌套 try，且 catch 里再 throw"的程序上响：`EndTry(pc=73) 关掉的不是 EH 表里的任何区域`。
同一段程序（四个函数 a/b/c/d）的实际发射是：

```
id 1 a:  Try 36                       → regions: [36..56, catch 50]        （没有 EndTry）
id 2 b:  Try 64, EndTry 75            → regions: [64..75, catch/finally 77]
id 3 c:  Try 101                      → regions: [101..113, catch 108]     （没有 EndTry）
id 4 d:  Try 117, EndTry 119, Try 122 → regions: [117..119 catch 145], [122..150 catch 134]
```

⇒ **根因**：`EndTry` 的发射与 `Try` **不是一一配对**。而运行时的语义是
`Try` push / `EndTry` pop，靠帧收尾时的 `saved_seh` 截断兜底。
所以"深度计数找配对 `EndTry`"的派生规则与运行时实际的进出**不是同一件事**，
`EhRegion::end` 目前**不能**当作"受保护范围"来用。

**修法：让 `push_seh` 带上受保护的 body 块，由发射端解析正常出口并回填**
（答案在 `lowering/mod.rs::lower_try`：它建 4 个块 `try_body`/`catch`/`finally`/`after_finally`，
`seh_handler = if has_catch { catch_blk } else { finally_blk }`，
并 push 一个 `SehFrameInfo { finally_blk, pending_exits, exit_edges }`
—— **受保护范围的出口集合就在 `exit_edges` 里**）。

改动点逐个确认过（比预想的小）：

1. **`Instruction::PushSeh { handler, finally }`（`ir/instruction.rs:467`）缺 `body` 字段**
   —— 而 `lower_try` 在调用 `push_seh` 的那一刻**手上就有 `try_body`**。
   所以这是**加一个字段**的事，不需要新的 fixup 机制。
2. **IR 层本来就没有 patch/fixup 机制** —— 所有回填都是 `codegen::generate_code` 内部的
   `Vec<PatchFn>` 闭包（`codegen.rs:75`），在布局完成后统一执行。
   `PushSeh` 现有的 `catch_offset` / `finally_offset` 就是这么回填的，照抄即可。
3. **从 `BlockId` 拿 pc 已有现成路**：`ControlFlowGraph::get_block_pos(block_id)`
   + codegen 已有的 `block_map`。正常出口就是 `body` 的终结指令
   （`Jump{target}` 或条件分支的某一侧）指向的块。
4. **`Try` 指令要加第三个操作数 `end_offset`**（`jr`，相对）：表里把它从 arity 2 组
   挪到 arity 3 组；`test_try_has_no_padding_slot` 那条测试要同步改（它现在断言 arity == 2）。
   **保留这个改动**，但它的角色变了：`end_offset` 从此**只服务"表的范围与校验"**
   （`by_end` 索引、`EndTry` 配对断言、dump），**运行期不再依赖它**——
   运行期靠 `Frame.try_stack` 知道"我在哪个区域里"（§4.6）。所以这个字段错了不会静默出错，
   会被守卫测试抓住。

**v1 的"坑"在 v2 里升级**：body 的正常出口**可能不止一个**（条件分支的两侧、
break/continue 的 trampoline），v1 建议"先上近似版 + 记成已知限制"。
**v2 不采纳近似版**：既然 `SehFrameInfo.exit_edges` 已经在手上，
就让 `EhTable` 存**出口集合**（`EhRegion.exits`，§4.6）。
理由：伪近似的那一版恰好会被"嵌套 try + catch 里再 throw"击中（上面那份实测数据就是），
而**多出口是常态不是例外**。

> ### ⚠️ 施工订正（2026-10-07，M1.1 落地时改的）
>
> 上面这条"四个改动点"里有**一处被推翻、一处被简化**，实测细节见
> `docs/deep-arch-log.md` §3.3。结论直接写在这里，免得后人照着旧文施工：
>
> - **改动点 4 作废**：`Try` **不加** `end_offset`，arity 保持 2。
>   单个 `BlockId` 表达不了多块的受保护范围；而**运行期不需要 `end`** ——
>   "我在哪个区域"由帧上的 try 栈回答（§4.6），拿 pc 查区间这条路本来就不走。
>   加它只会多一个没有消费者的操作数，并把指令表与两条既有测试一起动掉。
> - **出口集合的来源改为 IR 的 `PopSeh { region }`**（不再经 lowering 的 `exit_edges`）：
>   每一条 `PopSeh` 就是一个"正常退出区域"事件，是**权威来源**。
> - **`EhRegion` 的最终形状是 `{ start, exits: Vec<usize>, catch, finally }`**：
>   没有 `end`、没有 `normal_exit`、没有 `parent` —— 这三个都**没有消费者**。
>
> 施工中实测出两条此前的分析**没料到**的事实（都已钉成测试）：
>
> 1. **一条区域可以有 0 个退出点**：`pop_seh` 只在 `!current_block_is_terminated()` 时发射，
>    所以 `try { return 1 } catch (e) { return 2 }` 里**一条 `EndTry` 都没有**。
>    ⇒ 这就是"深度计数配对"必然算错的**机制**（此前只知道现象）。
> 2. **pc 顺序不表达嵌套**：`try { try{…}catch(e){} } finally {…}` 实测为
>    `outer = {start:92, exits:[101]}` / `inner = {start:95, exits:[98,106]}`
>    —— 外层的退出点 101 **夹在内层两个退出点之间**（布局是 RPO，内层 `catch` 靠异常边到达、
>    排在 101 之后）。
>    ⇒ 这是"用 `[start, end)` 表示区域"走不通的**第二个**证据，也是 M2 必须用
>    `Frame.try_stack` 而不是 pc 区间的直接理由。

**其余内容（照 `interpreter-refactor.md` §3.4 做）**

- `CodeBlock` per function；`FunctionObject` 持有 `Gc<CodeBlock>`（M1 时可以是 `Rc`）。
- 删 `generators`/`asyncs`/`derived_ctors` 与 `exit_pc` 映射 → `CodeBlock.flags` / `.exit_pc`。
- `State.current_code` 过渡，逐个替换 `module.`（44 处），**再**删 `module` 参数。

**验收**：`language/statements/generators`、`language/statements/class`、
`language/statements/try`、`language/expressions/generators` 四套件不减；
新增 `function_bodies_are_well_formed` 式守卫（每个体 `start < end`、
每条 `Try` 在表里有且仅有一条对应区域、每个区域的 `end` 落在某条指令边界上）。

### 5.2 M2：执行模型

顺序（**不要一次改全**）：

1. `State` 加 `frames: Vec<Frame>`，`Frame` 与旧结构**并存**（旧栈仍被读）；
   先只让 `run` 用 `frames.len()` 当边界，**不动任何指令**。
2. 改 3–5 条指令（`LoadConst` / `Add` / `Jump` / `Ret` / 一个调用）验证
   "索引式访问 + 单层循环"的写法；**这一步特意慢**，因为它决定后面 90 条的形态。
3. 铺开全部 arm；每铺开一组，删掉对应的平行栈字段。
4. 删 `PushC`/`PopC`/`MovC`/`AddC`/`SubC`；哨兵 pc 改边界；`invoke` 改
   `run_until(boundary)`。
5. 删 `invoke_boundaries`；`test262_runner.rs` 的 256 MB 栈恢复默认（先验证不炸）。
6. `New` 的开帧顺序决定（§4.3 陷阱 4）→ **写成 feature 测试**。

**验收**：全套件零回退；`guards:` 三个计数不增；
新增"深层 JS 递归不消耗 Rust 栈"的护栏（跑 10^5 层 JS 调用，Rust 栈不增长）。

### 5.3 M3：堆与句柄

1. `Heap`（只分配不回收）+ `Gc<T>`（`Copy`，48 位句柄）。
2. `HandleScope` + `Rooted`；定下"builtins 的签名长什么样"
   （`fn(&mut Cx, &mut HandleScope, this: Value, args: &[Value]) -> Completion<Value>`）
   —— **先在一个 builtins 文件上验证，再铺开**。
3. `Value::Object(Rc<RefCell<dyn JSObject>>)` → `Value::Object(Gc<Object>)`：
   `dyn` 消失意味着 `JSObject` trait 的 20 个方法要变成**具体类型的函数**
   （对象层 → `Object` + `ShapeKind`；需要跑 JS 的 → `Cx` 上的方法）。
4. `borrow()`/`borrow_mut()` 全部消失。

**验收**：功能等价（此时还没有 GC，所以内存会涨）；**不给性能承诺**，
M4 之后不再有 `Rc` 的引用计数增减，才会看到收益。

### 5.4 M4：收集器

1. `sweep` + free list + 阈值触发。
2. **GC stress 模式**（每次分配都收集 + 释放时填 `0xDD`）→ 跑 feature 全套，
   把所有 rooting bug 炸出来。
3. 弱引用（`WeakRefTable`）→ `WeakMap`/`WeakSet` 从"条目永不回收"改成真回收。
4. 护栏：`guards.rs` 的堆预算改成读 `Heap::bytes_allocated`
   （现在是"全局分配器的代理"）。

**验收**：`BIUJS_GC_STRESS=1` 下 feature 全绿（这是本里程碑真正的验收）；
`built-ins/WeakMap`/`WeakSet` 套件不减；循环引用（`obj.self = obj`、长原型链、
闭包环）在显式收集后被回收（用 `Heap::live_bytes()` 断言）。

### 5.5 M5：NaN-boxing

1. 新类型 + 位图 + 三条不变量的 property test（编码往返、NaN 规范化、`-0`）。
2. 全库 `match Value` 一次铺开（编译器列位点）。
3. `Hole` / `Uninitialized` 立即数落地：删 `MarkHole` 指令；
   `script_env` 的 `Option<Value>` 换成 `Value`。

**验收**：feature 全绿；新增"数组 elision 是洞"的回归（现有 B40 的测试）；
新增 `Object.is(-0, 0) === false` 与 `NaN` 规范化的回归。

### 5.6 M6：对象形状

1. `AtomTable` + `Atom`；`PropertyKey` 全改 `Atom`。
2. `Shape` + 转换表 + `Object{shape, slots}`；`OrdinaryObject` 的 `HashMap` 删除。
3. `ShapeKind::Array`（吸收 `ArrayObject` 的 `elements` + 用 `HOLE` 替代 `BTreeSet<usize>`）。
4. 单态 IC 进 `CodeBlock.inline_caches`，`GetProp`/`SetProp` 先查 IC。
5. `ObjectKind` 的 21 变体 `match` 逐个改成读 `shape.flags.kind()`。

**验收**：**第一次给出性能数字**（属性读的微基准 + 全套件耗时）；
`built-ins/Object`（`Object.keys` 顺序）/`built-ins/Array`/`Proxy` 套件不减。

### 5.7 M7：作用域与闭包

1. 编译期新 pass `scope_layout`（被嵌套函数引用的变量 → `(depth, index)`）。
2. `Environment` + `Frame.env`；`closure_var_stack` 删除。
3. per-iteration 绑定：循环头新建逃逸环境。
4. TDZ 用 `Value::UNINITIALIZED`。

**验收（这是"修 bug"不是"重构"，要有对照）**：
`counter()` 返回 `1,2,3`；`for (let i…)` 的闭包各自看到自己那轮；
与 node 的逐行对照探针写进 `tests/features/`；
`language/statements/let`、`language/statements/class`（`dstr`）套件提升。

### 5.8 M8：收尾

按 §4.8（生成器堆帧）、§4.9（`Exception`）、§4.11（`Engine`/`Realm`/`Cx`）三块做。
**顺序**：先 `Realm` 拆分（它让 runner 复用引擎，是**验证速度**的前提 ——
后面每一步都要靠全量回归，先把回归跑快是最高杠杆），再生成器堆帧，最后错误通道。

---

## 6. 移植对照表（逐项）

| ChakraCore | 我方现状 | 裁决 | 落在哪一步 |
|---|---|---|---|
| `OpCodes.h` 多路 include（905 行，~800 条） | `define_instrs!`（93 变体，按 arity 四组 + `Kind` + `Role`） | **已同构，保留**，且穷尽性更强 | M0 |
| `OpCodeAttr` 位标志（`OpSideEffect`/`OpCallInstr`/`OpNoFallThrough`/…） | `Kind`（`Normal/Jump/Call/Return/Throw/Suspend/Bookkeeping`） | **保留我方**：只留"执行期控制流效应"，JIT 那些位不要 | M0 |
| `LayoutTypes.h` / `EncodedSize` / `ByteCodeReader` | `Operand` + `RelPc`/`AbsPc` | **不采纳**：无序列化、无读取器 | —— |
| `Var` 标记值 + `TaggedInt` + `FLOATVAR` NaN-boxing | `Value` enum（8 变体、24 B） | **采纳思想**，位图自定（§4.2） | M5 |
| `TypeId` + `Type::entryPoint` + `VarIs<T>` | `ObjectKind`（21 变体）+ `match Value` | **采纳**：类型信息挂共享对象 | M6 |
| `Recycler` mark-sweep + `MarkContext` 标记栈 | 无（`Rc` 引用计数） | **采纳**（迭代标记，非递归） | M4 |
| size-class `HeapBlock`/`HeapBucket` + free list | 每次 `Rc` 一次 malloc/free | **采纳** | M4 |
| `RecyclerWeakReference` + `WeakReferenceHashTable` | `WeakMap`/`WeakSet` 条目永不回收 | **采纳** | M4 |
| 写屏障 / 并发标记 / 分代 / `HeapInfo` 调参 | —— | **不采纳**（STW 不需要；是加法不是重写） | M4 |
| `RecyclerRootPtr` / `AutoRecyclerRootPtr` | —— | **采纳形态**：`Rooted` + `HandleScope` | M3 |
| `InterpreterStackFrame::m_localSlots[0]`（寄存器在帧内） | 全局 `registers: [Value; 19]` | **采纳**：寄存器 arena + `Frame{base,len}` | M2 |
| 帧的三级分配（`_alloca` / arena / Recycler） | 只有"数据栈 + 平行栈" | **采纳语义**：共享 arena + 生成器独占窗口 | M2/M8 |
| `OP_CallCommon` / `GetEntryPoint()` 统一调用 | 6 opcode × 名字分发 | **采纳**：`FunctionObject.entry` | M2/M6 |
| `JavascriptMethod` 原生签名与脚本函数同形 | `call_native_by_name` 名字匹配 | **采纳** | M6 |
| `ProcessTryFinally(…, hasYield)` 带续体进入 | `SehRecord` 的 4 个可变字段 + `DelayedJump` | **采纳**：completion 传递 | M1/M2 |
| `EHBailoutData` 的 `parent/child` 树 | 无（`seh_stack` 平铺） | **采纳**：`EhRegion.parent` | M1 |
| 帧上 `nestedTryDepth` 等三个深度 + 三个 flag | `SehRecord` 14 字段 | **采纳**：`Frame.try_stack` | M2 |
| `FunctionBody` 自包含 + `m_constTable` + `nestedArray` | `Module` 一把梭 | **采纳**：`CodeBlock` | M1 |
| `FunctionBody::inlineCaches` | 无 | **采纳**：进 `CodeBlock`（别等以后） | M1/M6 |
| scope slot（`GetPropertyIdsForScopeSlotArray`） | `HashMap<String, Value>` 快照 | **采纳**：`Environment` 链（修语义 bug） | M7 |
| `PrototypeChainCache` | 每次重走原型链 | **采纳**（简化：`proto_epoch` + IC） | M6 |
| `DynamicTypeHandler` / `PathTypeHandler`（形状） | `OrdinaryObject.properties: HashMap` | **采纳**：`Shape` + 内联槽 | M6 |
| `JavascriptGenerator{frame, state}` | `SuspendedFrame` 十项拷贝 | **采纳**：`Generator{frame: Box<HeapFrame>}` | M8 |
| `ThreadContext` / `ScriptContext` 分离 | `VM` 一坨 | **采纳**：`Engine` / `Realm` / `Cx` | M8 |
| `JavascriptProxy` 在对象层转发陷阱 | **必须在 VM 层截获**（8 个点） | **M3 后反转**：不再有 `RefCell` 重入 panic，可回到"对象层派发 + VM 只提供 `Cx`" | M6 |
| `lib/Backend/*`（238,764 行 JIT） | —— | **不采纳** | —— |
| `ByteCodeGenerator` 单遍无 SSA | 我们有 lowering + SSA + regalloc | **不采纳**（照抄它是降级） | —— |
| `ByteCodeSerializer` / TTD / `CustomHeap` | —— | **不采纳** | —— |

### 6.1 命名澄清（避免去找不存在的符号）

勘察中发现以下名字**在 ChakraCore 里根本不存在**（它们是 `jscript9` / `ChakraFull` 的叫法），
不要照着找：

- `SmallInt`、`VarIsNumber`（数值判定用 `TaggedInt::Is` + `JavascriptNumber::Is`）
- `OpTempObject` / `OpCall` / `OpJump`（是 `OpTempObject*` 系列 / `OpCallInstr` / `OpNoFallThrough`）
- `FunctionBody::m_propertyIds` / `m_constantEncoding` / `m_ihostArena`
  （对应 `nestedArray` / `m_constTable` / `ByteBlock`）
- `FunctionBody::GetTryCatchOffset`；`SetExceptionObject` / `hasBailedOut`
- `PushCallFrame` / `PopCallFrame`（帧进出靠 `InterpreterThunk` + `previousInterpreterFrame` 链）
- `GeneratorUtils`、`Module.h`（模块用 `ModuleRecordBase` / `SourceTextModuleRecord` / `ModuleRoot`）
- `InterpreterStackFrame` 在 `lib/Runtime/Language/`，**不在** `lib/Runtime/ByteCode/`
- `Recycler::Collect` 的主流程其实是 `Collect → DoCollectWrapped → DoCollect`

### 6.2 我方 vs ChakraCore（现状对照）

| | ChakraCore | 我们 |
|---|---|---|
| 整体规模 | `lib/` 810,519 | `src/` **39,343** |
| GC | mark-sweep + 并发 + 分代（`Common/Memory` 57,856） | 无（`Rc` 引用计数） |
| 值表示 | `Var` 8 B | `Value` enum 24 B |
| 解释器 | `InterpreterStackFrame.cpp` 9,663 | `vm/mod.rs` 10,868（含帧 + SEH + 生成器 + 派发） |
| 帧 | 寄存器在帧内（柔性数组） + 帧链 | 全局 `registers[19]` + **9 条平行栈** |
| 调用入口 | `OP_CallCommon` 一处 | 6 opcode × 名字分发（M0 收敛了一部分） |
| 属性 | `Shape` + 内联槽 + IC | `HashMap` + `RefCell` |
| 闭包 | scope slot（共享绑定） | `HashMap` 快照（**不共享**） |
| 异常 | 内联指令 + `EHBailoutData` 树 | `seh_stack`（14 字段/条） + `DelayedJump` |
| 生成器 | `{frame, state}`（帧是堆对象） | `SuspendedFrame`（十项拷贝） |
| 编译前端 | 单遍无 SSA | lowering + SSA + regalloc（**我们更强**） |

---

## 7. 验证协议 v2（因为"允许破坏性变更"）

允许破坏不等于放任。**每一步 = 快照 + 允许回归清单 + 一组新守卫**。

### 7.1 开工前记录快照

1. `docs/phase2-status.tsv` 的基线（通过 / 失败 / 跳过），外加逐套件 diff。
2. **一组对照探针**：同一份 JS 分别跑 `biujs` 与 `node`，逐行对比。
   这轮用这个方法抓到了**生成器闭包 bug**，而当时全量 test262 是绿的
   （v1 §8 的教训）。**它必须成为常规动作，不是偶然**。
   建议落成 `tests/probes/*.js` + `scripts/probe-diff.sh`，跑 `node` 与
   `target/release/biujs` 并逐行 diff。
3. 增量指标：单元 / feature / 护栏测试数，以及**新守卫的清单**（每步 ≥5 条断言）。

### 7.2 每一步新增的守卫（按里程碑）

| 里程碑 | 新增守卫 |
|---|---|
| M1 | EH 表自洽（每条 `Try` 恰好一条区域、`end` 落在指令边界、`exits` 是区域内的 `Leave` pc）；`CodeBlock` 的运行期无 `Module` 断言 |
| M2 | "JS 深递归不消耗 Rust 栈"；`frames.len()` 与 `MAX_CALL_DEPTH` 的一致性；`Ret` 写回调用方 `dst` |
| M3 | `HandleScope` 平衡（scope 进出配对）；"分配点前后句柄有效" |
| M4 | **`BIUJS_GC_STRESS=1` 下 feature 全绿**；循环对象在显式收集后回收；`live_bytes` 单调不增长（长循环） |
| M5 | `Value` 编码往返 property test；NaN 规范化；`-0` 保留；`HOLE` 参与 `Array.prototype` 的洞判定 |
| M6 | 同构字面量共享形状；IC 命中率（可观测计数）；`Object.keys` 顺序 |
| M7 | `counter()` 返回 `1,2,3`；`for (let i…)` 闭包各自一轮；TDZ 抛 `ReferenceError` |
| M8 | 生成器在 try 内挂起/恢复；`async` 完成 settle；`Exception` 是 JS 值（`instanceof TypeError`） |

### 7.3 每一步的结束条件

每个里程碑的结束点必须：
1. **行为快照等价**：探针逐行一致 + test262 不比开工前差（**除非该步声明了允许的回归项**）；
2. 回归项列进提交信息，并标注"预期在哪一步消掉"；
3. 更新本文 §9 进度表与 `docs/handover.md` §4 状态表。

**语义 bug 一律先钉测试再动结构**（v1 §7 第 4 条，保留）—— 否则重写会把旧行为连 bug 一起搬走。

---

## 8. 风险登记

| 风险 | 等级 | 应对 |
|---|---|---|
| **显式 rooting 纪律**（M3/M4）：漏标一个 `Gc` 局部变量 → 悬垂 | **高** | 分三步（只分配 → sweep → stress）；`BIUJS_GC_STRESS=1` 每次分配都收集；释放填 `0xDD`；接口上让"未 root 的 `Gc`"难以构造（`Rooted<T>` 是新类型，`Gc<T>` 尽量不进 builtins 签名） |
| M5（值表示）是全库 `match Value` 替换，比 P0a 更大 | 高 | 一次铺开（半迁移状态成本更高）；`Debug`/转换走两个唯一出口；编译器列位点 |
| M2 的 borrowck（`stack` 与 `frames` 不能同时可变借用） | 中 | 索引式访问；先只改 3–5 条指令验证写法；`Frame` 不持裸指针 |
| M6/M7 是"新写一个对象模型 / 作用域模型"，不是改造 | 高 | 分里程碑且**每个里程碑结束都可发布**（不是"7 步之后才可用"）；M6 先让形状 per-realm，不追求跨 realm 共享 |
| M2 重写会撞上 §4.3 的"必须同时改的东西"清单，漏一项出诡异 bug | 高 | 清单逐项写成测试；尤其 `New` 的开帧顺序必须**显式决定** |
| 取消 `eval`/`with` 的静态假设被推翻（有人要加 `eval`） | 中 | **写进文档**：M7 的 scope slot 依赖"名字→槽位静态可定"；加 `eval` 要退回字典环境 |
| M6 之后 Proxy 陷阱回到对象层，可能与现有 8 个 VM 截获点行为不一致 | 中 | 单独一批做迁移，`built-ins/Proxy` 套件 + `Object.create(proxy)` 探针 |
| 重构期间 test262 数字停住 | 中 | 用 `handover.md` §4 的语义线并行推进（TypedArray 入册解锁等）；每个里程碑结束更新 handover |
| 中途放弃导致"半新半旧" | 中 | 每个里程碑独立提交、独立可发布；§0 的 9 步之间只保留两条硬依赖 |

---

## 9. 进度

| 里程碑 | 状态 | 备注 |
|---|---|---|
| **M0** 前置 | **已完成**（2026-10-01 起） | `refactor/interpreter-p0a`：3 个语义 bug 钉测试；`FunctionBody`/`EhRegion` 派生视图；`Instr` enum（93）；`Kind`/`Role`/`RelPc`·`AbsPc`；`open_frame`（5 处共用）；`enter_call`（`CallEx` 已改道）；`FrameMode` 合一。通过 16561 / 执行 19708 / 跳过 7843；单元 >201 / feature 524 / 护栏 7 |
| **M1** 编译期自包含 | **进行中**。M1.1 / M1.2-a / M1.2-b **已完成**（2026-10-07，`docs/deep-arch-log.md` §3–§5）。M1.1：`PushSeh.body` / `PopSeh.region` / `Module.eh_regions` / `EhRegion.exits`；M1.2-a：VM 改以 EH 表为准；M1.2-b：异常边集合改由 **CFG 数据流**给出，**修掉一个 test262 覆盖不到的静默错编**（外层 catch 读到旧值）| 下一步 = **M1.3（`CodeBlock` per function）**；另登记 known bug #4（`handle_throw` 在 catch 路径多弹一次 `seh_stack`，M2 按构造消除） |
| **M2** 执行模型 | 未开始 | 见 §5.2 的六步顺序 |
| **M3** 堆与句柄 | 未开始 | 见 §5.3 |
| **M4** 收集器 | 未开始 | 见 §5.4；验收靠 GC stress |
| **M5** 值表示 | 未开始 | 见 §5.5 |
| **M6** 对象形状 | 未开始 | 见 §5.6；**第一次给出性能数字** |
| **M7** 作用域与闭包 | 未开始 | 见 §5.7；**这是修 bug，不是重构** |
| **M8** 收尾 | 未开始 | 见 §5.8；顺序：`Realm` → 生成器堆帧 → `Exception` |
| 语义线（并行，随时可插） | 未开始 | TypedArray 族入册解锁；Proxy 原型链两条残留。见 `handover.md` §4 |

---

## 10. 分支与提交约定

- 深改开**独立分支** `refactor/deep-arch`，与 `refactor/interpreter-p0a`（M0 已落地部分）分开，
  便于随时回到可发布状态。
- **每个里程碑一次提交**（或每个里程碑内按 §5.x 的子步骤多次），提交信息按 §7.3 列出"允许的回归项"。
- **不允许**在同一提交里同时出现"结构变化"与"语义变化"（M7/M8 里那几处**故意的语义变化**
  ——如 `counter()`、`PrologueEnd` 的绑定时机——要单独提交并写明"这是修 bug"）。
- 每步结束更新本文 §9 与 `docs/handover.md` §4。

---

## 11. 度量口径与命令（沿用现有）

| 指标 | 基线 |
|---|---|
| test262 执行 / 通过 / 失败 / 跳过 | 19708 / **16561** / 3147 / 7843 |
| 通过率 | 84.03%（分母口径见 `es6-conformance-phase2.md` §2.1） |
| 单元 / feature / 护栏 | 190 → >201 / 523 / 7 |
| 全量耗时 | 约 3m（4 分片；单片内存上限 4 GB） |

```sh
cargo test --release --lib                     # 单元
cargo test --release --test features           # feature 断言
./scripts/phase2-status.sh                     # 三级验证 + 逐套件 diff（推荐）
./scripts/phase2-status.sh --update            # 完成一批后刷新快照
BIUJS_GC_STRESS=1 cargo test --release --test features   # M4 之后：rooting 验收
BIUJS_DUMP=1 ./target/release/biujs file.js    # 打印字节码
```

**报告口径**：同时给"计划内通过数 / 目标套件通过率 / 范围外排除数"三个数，
并附逐套件 diff（`es6-conformance-phase2.md` §2.1 / `interpreter-refactor.md` §5.7）。
