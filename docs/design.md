# Biu JS Engine — 详细设计文档

## 一、项目概述

Biu 是一个嵌入式 JavaScript 引擎，基于 evalit 的 IR + Bytecode + VM 架构进行拓展。
采用 oxc 解析器替代 evalit 的 pest 解析器，保持 SSA + 寄存器分配 + 代码生成的核心管道不变。

### 1.1 设计原则

1. **复用 evalit 核心**: IR 定义、CFG、SSA Builder、RegAlloc、Codegen、VM 执行引擎
2. **增量拓展**: 在 evalit 的 IR 指令集基础上新增 JS 语义指令
3. **嵌入式优先**: 无运行时编译器、可剥离编译期组件、小型运行时
4. **ES6 核心完整**: class、箭头函数、Promise、generator、async/await、模块

### 1.2 从 evalit 继承的组件

| 组件 | evalit 路径 | biujs 路径 | 修改程度 |
|------|-------------|------------|----------|
| IR 定义 | `compiler/ir/instruction.rs` | `compiler/ir/instruction.rs` | 扩展 (新增 JS 指令) |
| CFG | `compiler/ir/cfg.rs` | `compiler/ir/cfg.rs` | 无修改 |
| SSA Builder | `compiler/ir/ssabuilder.rs` | `compiler/ir/ssabuilder.rs` | 无修改 |
| IR Builder | `compiler/ir/builder.rs` | `compiler/ir/builder.rs` | 扩展 (新增 JS 指令构建) |
| RegAlloc | `compiler/regalloc.rs` | `compiler/regalloc.rs` | 无修改 |
| Codegen | `compiler/codegen.rs` | `compiler/codegen.rs` | 扩展 (新增 JS 指令生成) |
| Bytecode 定义 | `bytecode.rs` | `bytecode.rs` | 扩展 (新增 JS Opcode) |
| VM 执行引擎 | `runtime/vm.rs` | `runtime/vm.rs` | 扩展 (新增 JS 指令执行) |
| 值类型系统 | `runtime/value.rs` | `runtime/value.rs` | 重写 (JS 类型系统) |
| 对象系统 | `runtime/object.rs` | `runtime/object.rs` | 重写 (JS 对象模型) |
| 环境 | `runtime/environment.rs` | `runtime/environment.rs` | 扩展 |

---

## 二、模块架构

```
biujs/
├── Cargo.toml
├── src/
│   ├── lib.rs                   # 库入口，导出公共 API
│   ├── main.rs                  # REPL / CLI (开发阶段)
│   │
│   ├── compiler/                # 编译期
│   │   ├── mod.rs
│   │   ├── ir/                  # IR 定义 (从 evalit 移植 + 扩展)
│   │   │   ├── mod.rs
│   │   │   ├── instruction.rs   # Instruction enum (扩展 JS 指令)
│   │   │   ├── builder.rs       # InstBuilder trait (扩展 JS 构建方法)
│   │   │   ├── cfg.rs           # ControlFlowGraph (无修改)
│   │   │   └── ssabuilder.rs    # SSA 转换 (无修改)
│   │   ├── lowering/            # 降级器
│   │   │   ├── mod.rs
│   │   │   ├── js_lower.rs      # JSASTLower 核心
│   │   │   ├── class.rs         # class 降级
│   │   │   ├── closure.rs       # 闭包检测 + 堆提升
│   │   │   ├── generator.rs     # generator 状态机变换
│   │   │   └── patterns.rs      # 解构/展开降级
│   │   ├── regalloc.rs          # 寄存器分配 (从 evalit 移植)
│   │   └── codegen.rs           # IR → Bytecode (从 evalit 移植 + 扩展)
│   │
│   ├── vm/                      # 运行时
│   │   ├── mod.rs
│   │   ├── vm.rs                # VM 核心执行引擎 (扩展 JS 指令)
│   │   ├── value.rs             # Value 类型系统 (重写: JS 类型)
│   │   ├── object.rs            # JSObject trait + OrdinaryObject
│   │   ├── property.rs          # PropertyDescriptor, PropertyKey
│   │   ├── prototype.rs         # 原型链查找逻辑
│   │   ├── closure.rs           # ClosureEnv, ClosureFunction
│   │   ├── bytecode.rs          # Opcode / Bytecode 定义 (扩展)
│   │   ├── error.rs             # RuntimeError + StackFrame
│   │   └── seh.rs               # SEH 异常处理
│   │
│   ├── builtins/                # 内置对象库
│   │   ├── mod.rs
│   │   ├── object_builtins.rs   # Object.*
│   │   ├── array_builtins.rs    # Array.* + Array.prototype.*
│   │   ├── string_builtins.rs   # String.* + String.prototype.*
│   │   ├── number_builtins.rs   # Number.*
│   │   ├── function_builtins.rs # Function.*
│   │   ├── error_builtins.rs    # Error, TypeError, etc.
│   │   ├── math_builtins.rs     # Math.*
│   │   ├── json_builtins.rs     # JSON.*
│   │   ├── promise_builtins.rs  # Promise
│   │   ├── symbol_builtins.rs   # Symbol
│   │   └── console_builtins.rs  # console.* (调试用)
│   │
│   ├── module/                  # 模块系统
│   │   ├── mod.rs
│   │   ├── loader.rs            # ModuleLoader trait
│   │   └── resolver.rs          # 模块解析
│   │
│   └── host/                    # 宿主接口
│       ├── mod.rs
│       ├── native.rs            # NativeFunction trait
│       └── context.rs           # HostContext
│
├── tests/
│   ├── basic_test.rs
│   ├── class_test.rs
│   ├── prototype_test.rs
│   └── builtins_test.rs
│
├── examples/
│   └── embedded.rs              # 嵌入式集成示例
│
└── docs/
    ├── biu-js-engine-architecture.md
    ├── design.md                # 本文档
    └── task-plan.md             # 任务计划
```

---

## 三、类型系统设计

### 3.1 JS Value 类型

```rust
// src/vm/value.rs

use std::rc::Rc;
use std::cell::RefCell;

/// JS 的值类型
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),                      // JS 统一用 f64
    String(Rc<String>),
    Symbol(Rc<SymbolData>),
    Object(Rc<RefCell<dyn JSObject>>),
}

/// Symbol 内部数据
pub struct SymbolData {
    pub description: Option<String>,
    pub id: u64,
}
```

**与 evalit 的差异**:
- evalit: `Null, Bool, Int(i64), Float(f64), Char, Str, Object`
- biujs: `Undefined, Null, Bool, Number(f64), String, Symbol, Object`
- 去掉 `Int/Float/Char`，统一为 `Number(f64)` (符合 ES6 规范)
- 新增 `Undefined` 和 `Symbol`
- 对象使用 `Rc<RefCell<dyn JSObject>>` 而非 `Box<dyn Object>`

### 3.2 JSObject Trait

```rust
// src/vm/object.rs

pub trait JSObject: std::fmt::Debug {
    // 属性操作
    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor>;
    fn property_set(&mut self, key: &PropertyKey, value: ValueRef);
    fn property_delete(&mut self, key: &PropertyKey) -> bool;
    fn has_property(&self, key: &PropertyKey) -> bool;
    fn own_keys(&self) -> Vec<PropertyKey>;

    // 原型操作
    fn get_prototype(&self) -> Option<ValueRef>;
    fn set_prototype(&mut self, proto: Option<ValueRef>);

    // 不变性检查
    fn is_extensible(&self) -> bool;
    fn prevent_extensions(&mut self);
    fn is_frozen(&self) -> bool;
    fn freeze(&mut self);
    fn is_sealed(&self) -> bool;
    fn seal(&mut self);

    // 内部类型标记
    fn type_of(&self) -> &'static str;
    fn class_name(&self) -> &'static str;
}
```

### 3.3 PropertyDescriptor

```rust
// src/vm/property.rs

pub struct PropertyDescriptor {
    pub value: ValueRef,
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub getter: Option<ValueRef>,
    pub setter: Option<ValueRef>,
}

pub enum PropertyKey {
    Str(Rc<String>),
    Symbol(Rc<SymbolData>),
}
```

---

## 四、指令集扩展

### 4.1 新增 Opcode

在 evalit 的 Opcode 基础上新增:

```rust
// src/vm/bytecode.rs

pub enum Opcode {
    // === 从 evalit 继承 ===
    LoadConst, LoadEnv, Halt,
    Push, Pop, PushC, PopC, AddC, SubC, MovC,
    Call, CallEx, CallNative, Ret, Mov,
    Jump, BrIf, Not, Neg,
    Addx, Subx, Mulx, Divx, Remx,
    And, Or,
    Less, LessEqual, Greater, GreaterEqual, Equal, NotEqual,
    Range, RangeInclusive, RangeFrom, RangeFull, RangeTo, RangeToInclusive,
    MakeIter, IterNext,
    MakeArray, ArrayPush, MakeMap,
    IndexGet, IndexSet, MakeSlice,
    MakeStruct, MakeStructField,
    PropGet, PropSet, CallMethod,
    Try, EndTry, ThrowExc, LoadException,

    // === JS 新增 ===
    // 对象系统
    CreateObject,        // 创建空对象
    SetPrototype,        // 设置 __proto__
    CreateFunction,      // 创建函数对象
    DefineProperty,      // 定义属性 (含 descriptor)
    DefineAccessor,      // 定义 getter/setter
    InstanceOf,          // instanceof 运算
    TypeOf,              // typeof 运算
    Delete,              // delete 运算

    // 闭包系统
    CreateClosureEnv,    // 创建闭包环境
    EnvGet,              // 从闭包环境读取
    EnvSet,              // 写入闭包环境
    CreateClosure,       // 创建闭包函数

    // Generator
    CreateGenerator,     // 创建 generator 对象
    GeneratorNext,       // 执行 .next()
    Yield,               // yield 暂停

    // JS 语义运算
    JsAdd,               // JS 加法 (字符串优先)
    JsSub, JsMul, JsDiv, JsRem,
    JsEq, JsNe,          // == / !=
    JsStrictEq, JsStrictNe, // === / !==
    JsLt, JsGt, JsLe, JsGe,
    JsBitAnd, JsBitOr, JsBitXor, JsBitNot,
    JsShiftLeft, JsShiftRight, JsShiftRightZeroFill,
    JsVoid,              // void 运算

    // 模块系统
    LoadModule,          // 加载模块
    ModuleGetDefault,    // 获取默认导出
    ModuleGetNamed,      // 获取命名导出
    ModuleGetNamespace,  // 获取命名空间导出

    // New 操作符
    New,                 // new Constructor(args)
}
```

### 4.2 IR Instruction 扩展

```rust
// src/compiler/ir/instruction.rs

pub enum Instruction {
    // === 从 evalit 继承 ===
    // ... (保持不变)

    // === JS 新增 ===
    CreateObject { dst: Value },
    SetPrototype { object: Value, proto: Value },
    CreateFunction { dst: Value, func_id: FunctionId },
    DefineProperty { object: Value, key: Value, desc: Value },
    DefineAccessor { object: Value, key: Value, getter: Value, setter: Value },
    InstanceOf { dst: Value, object: Value, constructor: Value },
    TypeOf { dst: Value, value: Value },
    Delete { dst: Value, object: Value, key: Value },

    CreateClosureEnv { dst: Value, size: usize },
    EnvGet { dst: Value, env: Value, index: usize },
    EnvSet { env: Value, index: usize, value: Value },
    CreateClosure { dst: Value, func_id: FunctionId, env: Value },

    CreateGenerator { dst: Value, func_id: FunctionId },
    GeneratorNext { dst: Value, gen: Value, arg: Option<Value> },
    Yield { value: Value },

    JsBinaryOp { op: Opcode, dst: Value, lhs: Value, rhs: Value },
    JsUnaryOp { op: Opcode, dst: Value, src: Value },

    LoadModule { dst: Value, specifier: ConstantId },
    ModuleGetDefault { dst: Value, module: Value },
    ModuleGetNamed { dst: Value, module: Value, name: Value },
    ModuleGetNamespace { dst: Value, module: Value },

    New { dst: Value, constructor: Value, args: Vec<Value> },
}
```

---

## 五、编译管道设计

### 5.1 JSASTLower — 核心降级器

从 oxc AST 直接降级到 IR，复用 evalit 的 IR 指令集 + 扩展。

```rust
// src/compiler/lowering/js_lower.rs

pub struct JSASTLower<'a> {
    builder: &'a mut dyn InstBuilder,
    symbols: SymbolTable<Variable>,
    scope_chain: Vec<ScopeInfo>,
    this_binding: Option<ValueRef>,
    hoisted_decls: Vec<DeclInfo>,
    class_info: Vec<ClassInfo>,
    is_strict: bool,
    env: &'a Environment,
}
```

**与 evalit ASTLower 的关系**:
- 复用 evalit 的 `SymbolTable`、`LoopContext`、`Variable` 等基础设施
- 替换 `lowering::ASTLower` 为 `JSASTLower`，输入从 evalit AST 变为 oxc AST
- 新增 class、closure、generator 等 JS 特有的降级逻辑

### 5.2 编译流程

```
JS 源代码 (.js)
    │
    ▼
[oxc_parser] ──────────────────────────── 新增
    │  oxc_ast::Program<'a>
    ▼
[JSASTLower] ──────────────────────────── 新增
    │  class → 原型链操作
    │  closure → ClosureEnv
    │  generator → 状态机
    │  解构/展开 → 降级
    │  IR (扩展的 Instruction enum)
    ▼
[SSA Builder] ─────────────────────────── 从 evalit 移植
    │  SSA IR + Phi 节点
    ▼
[Register Allocator] ──────────────────── 从 evalit 移植
    │  活跃区间分析 + 寄存器分配
    ▼
[Code Generator] ──────────────────────── 从 evalit 移植 + 扩展
    │  Bytecode (扩展的 Opcode)
    ▼
[VM Executor] ─────────────────────────── 从 evalit 移植 + 扩展
    │  寄存器执行 + JS 语义运算
    ▼
Result (JS Value)
```

---

## 六、VM 运行时设计

### 6.1 核心结构

```rust
// src/vm/vm.rs

pub struct VM {
    state: VMState,
    builtins: BuiltinRegistry,
    module_loader: Box<dyn ModuleLoader>,
}

pub struct VMState {
    registers: Vec<ValueRef>,
    ctrl_stack: Vec<CtrlFrame>,
    codes: Vec<Bytecode>,
    pc: usize,
    seh_stack: Vec<SehRecord>,
    closure_envs: Vec<Rc<RefCell<ClosureEnv>>>,
    microtask_queue: VecDeque<ValueRef>,
    host: Rc<RefCell<dyn HostContext>>,
}
```

**与 evalit VM 的关系**:
- 复用 evalit 的 `State` 结构 (data_stack, ctrl_stack, registers, seh_stack)
- 新增 `closure_envs`、`microtask_queue`、`host` 等 JS 特有字段
- 值类型从 `ValueRef` (Rc<RefCell<Value>>) 改为 JS 的 `Value` 类型

### 6.2 this 绑定

evalit 的 Call 指令没有 this 参数。biujs 需要扩展:

```rust
// 新增 CallWithThis 指令
Opcode::CallWithThis => {
    let func = operands[0];
    let this_val = operands[1];
    // ... 保存 this_val 到特殊寄存器/栈位置
}
```

### 6.3 new 操作符

```rust
Opcode::New => {
    let constructor = self.get_value(operands[0])?;
    // 1. 创建新对象，prototype = constructor.prototype
    let mut new_obj = OrdinaryObject::new();
    let proto = constructor.property_get("prototype");
    new_obj.set_prototype(proto);
    // 2. 调用构造函数，this = new_obj
    // 3. 如果返回值不是对象，则返回 new_obj
}
```

---

## 七、原型链实现

### 7.1 原型链查找

```rust
// src/vm/prototype.rs

fn internal_get(obj: &ValueRef, key: &PropertyKey) -> ValueRef {
    let mut current = obj.clone();
    let mut depth = 0;

    loop {
        if depth > MAX_PROTO_DEPTH { return Value::Undefined; }

        if let Value::Object(ref rc_obj) = *current.borrow() {
            let obj = rc_obj.borrow();
            // 1. 查自身属性
            if let Some(desc) = obj.property_get(key) {
                if let Some(ref getter) = desc.getter {
                    return call_getter(getter, current.clone());
                }
                return desc.value.clone();
            }
            // 2. 沿原型链上溯
            match obj.get_prototype() {
                Some(proto) => current = proto,
                None => return Value::Undefined.into(),
            }
        } else {
            return Value::Undefined.into();
        }
        depth += 1;
    }
}
```

---

## 八、闭包实现

### 8.1 闭包环境

```rust
// src/vm/closure.rs

pub struct ClosureEnv {
    pub values: Vec<ValueRef>,
}

pub struct ClosureFunction {
    pub func_id: FunctionId,
    pub env: Rc<RefCell<ClosureEnv>>,
}
```

### 8.2 闭包编译策略

```javascript
function makeCounter() {
    let count = 0;
    return function() { return ++count; };
}
```

编译为 IR:
```
function makeCounter() {
    env = CreateClosureEnv 1    // env.values[0] = count
    EnvSet env, 0, 0            // count = 0
    inner = CreateClosure inner_func_body, env
    return inner
}

function inner_func_body() {
    tmp = EnvGet closure_env, 0  // load count
    tmp = JsAdd tmp, 1           // ++count
    EnvSet closure_env, 0, tmp   // store count
    return tmp
}
```

---

## 九、Class 编译策略

```javascript
class Foo extends Bar {
    constructor(x) {
        super(x);
        this.y = 0;
    }
    method() { return this.x; }
}
```

编译为 IR:
```
// 1. 创建 Foo.prototype, 设置 __proto__ = Bar.prototype
foo_proto = CreateObject
SetPrototype foo_proto, bar_proto

// 2. 挂载方法
DefineProperty foo_proto, "method", method_func

// 3. 创建 Foo 构造函数
foo_ctor = CreateFunction constructor_func
DefineProperty foo_ctor, "prototype", foo_proto

// 4. 设置 Foo.__proto__ = Bar (静态继承)
SetPrototype foo_ctor, bar
```

---

## 十、模块系统

```rust
// src/module/loader.rs

pub trait ModuleLoader {
    fn resolve(&self, specifier: &str, referrer: &str) -> Result<String, ModuleError>;
    fn load(&mut self, path: &str) -> Result<String, ModuleError>;
    fn compile(&mut self, source: &str, path: &str) -> Result<CompiledModule, ModuleError>;
}

pub struct CompiledModule {
    pub exports: HashMap<String, ValueRef>,
    pub bytecode: Vec<Bytecode>,
}
```

---

## 十一、错误系统

```rust
// src/vm/error.rs

pub enum RuntimeError {
    Error { message: String, stack: Vec<StackFrame> },
    TypeError { message: String, stack: Vec<StackFrame> },
    ReferenceError { message: String, stack: Vec<StackFrame> },
    RangeError { message: String, stack: Vec<StackFrame> },
    SyntaxError { message: String, stack: Vec<StackFrame> },
    URIError { message: String, stack: Vec<StackFrame> },
    InternalError { message: String },
    StackOverflow,
    OutOfMemory,
    UnhandledPromiseRejection { value: ValueRef },
}
```

---

## 十二、宿主接口

```rust
// src/host/native.rs

pub trait NativeFunction {
    fn call(&self, ctx: &mut HostContext, args: &[ValueRef]) -> Result<ValueRef, RuntimeError>;
}

pub struct HostContext {
    pub vm_state: *mut VMState,
    pub builtins: *const BuiltinRegistry,
}
```

---

## 十三、依赖关系

```toml
[dependencies]
oxc_parser = "..."       # JS/TS 解析器
oxc_ast = "..."          # AST 定义
oxc_span = "..."         # Span 类型
log = "0.4"
petgraph = "0.8"
```

---

## 十四、与 evalit 的关键差异总结

| 方面 | evalit | biujs |
|------|--------|-------|
| 解析器 | pest (自定义语法) | oxc_parser (完整 JS) |
| 值类型 | Null/Bool/Int/Float/Char/Str/Object | Undefined/Null/Bool/Number/String/Symbol/Object |
| 数字类型 | i64 + f64 分离 | 统一 f64 |
| 对象模型 | 简单 Object trait | 完整 JSObject + 原型链 + PropertyDescriptor |
| 闭包 | 无 | ClosureEnv + Upvalue |
| Class | 无 | 原型链编译策略 |
| Generator | 无 | 状态机变换 |
| 模块 | 无 | ModuleLoader trait |
| 内置对象 | 无 | ES6 标准库 |
| this 绑定 | 无 | CallWithThis + New |
| 运算语义 | Rust 语义 | JS 语义 (类型转换) |
