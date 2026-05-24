# Biu — 嵌入式 JavaScript 引擎架构方案

> 基于 evalit (IR + Bytecode + VM) 架构蓝图，使用 oxc 解析器
> 定位：轻量级、嵌入式、ES6 兼容、Rust 实现

---

## 一、设计原则

1. **去掉动态不安全特性**：排除 `eval`、`Function(code)`、`with`
2. **保留完整原型链**：`[[Prototype]]`、`Object.setPrototypeOf`、`__proto__`、`Object.create` 全部支持
3. **ES6 核心完整**：class、箭头函数、Promise、generator、async/await、模块、Proxy（延期）
4. **嵌入式优先**：无运行时编译器、可剥离编译期组件、小型运行时
5. **增量验证**：第一天就跑通 `parse → IR → bytecode → execute` 的完整链路

---

## 二、整体架构

```
                    ┌───────────────────────────┐
                    │  oxc_parser                │
                    │  (JS/TS → oxc_ast::Program)│
                    └────────────┬──────────────┘
                                 │  AST with arena + 'a lifetime
                                 ▼
┌─────────────────────────────────────────────────────┐
│  编译期 (Compile Time)                                │
│  ┌─────────────────────────────────────────────────┐ │
│  │  JSASTLower                                     │ │
│  │  • 递归遍历 oxc AST                              │ │
│  │  • 闭包检测 + 变量逃逸分析 + 堆提升               │ │
│  │  • class 编译为原型链操作                         │ │
│  │  • generator → 状态机变换                        │ │
│  │  • async/await → 状态机 + Promise 调度           │ │
│  │  • 解构/展开/模板字符串降级                       │ │
│  └──────────────────────┬──────────────────────────┘ │
│                         │ IR                          │
│  ┌──────────────────────▼──────────────────────────┐ │
│  │  Compiler Pipeline                              │ │
│  │  • SSA Builder (SSA 转换)                       │ │
│  │  • Register Allocator (寄存器分配)               │ │
│  │  • Code Generator (IR → Bytecode)               │ │
│  └──────────────────────┬──────────────────────────┘ │
└─────────────────────────┼────────────────────────────┘
                          │ Bytecode
                          ▼
┌─────────────────────────────────────────────────────┐
│  运行时 (Runtime)                                    │
│  ┌─────────────────────────────────────────────────┐ │
│  │  VM Core                                        │ │
│  │  • 寄存器式执行引擎 (Register-based)              │ │
│  │  • 控制栈 (ctrl_stack) + 调用帧管理               │ │
│  │  • SEH 异常处理 (try-catch-throw)                │ │
│  │  • 闭包环境 (ClosureEnv / Upvalue)               │ │
│  │  • 原型链查找 ([[Prototype]] 遍历)               │ │
│  │  • JS 专用运算 (js_add / js_eq / strict_eq 等)   │ │
│  └─────────────────────────────────────────────────┘ │
│  ┌─────────────────────────────────────────────────┐ │
│  │ 内置对象库 (Built-ins)                           │ │
│  │  • Object, Array, String, Number, Boolean      │ │
│  │  • Function, Error (TypeError, ReferenceError) │ │
│  │  • Math, Date, JSON, RegExp                    │ │
│  │  • Map, Set, WeakMap, WeakSet                  │ │
│  │  • Promise + 微任务调度                         │ │
│  │  • Symbol                                      │ │
│  │  • Proxy/Reflect (延期)                        │ │
│  └─────────────────────────────────────────────────┘ │
│  ┌─────────────────────────────────────────────────┐ │
│  │ 宿主接口 (Host API)                             │ │
│  │  • NativeFunction (ctx 参数)                    │ │
│  │  • ModuleLoader trait (模块加载)                │ │
│  │  • Memory management (GC / Rc)                 │ │
│  └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

---

## 三、数据流

```
源文件 (.js)
    │
    ▼
[oxc_parser]  ← 替换自定义 parser
    │
    │  oxc_ast::Program<'a>
    │  (arena 分配的 AST，约 150 种节点)
    ▼
[JSASTLower]
    │
    │  检测逃逸变量 → 堆提升
    │  检测 class → 原型链操作
    │  检测 generator → 状态机变换
    │  检测解构/展开/模板字符串 → 降级
    │
    │  IR (Instruction enum)
    ▼
[SSA Builder]
    │
    │  SSA IR + Phi 节点 + 异常边
    ▼
[Register Allocator]
    │
    │  活跃区间分析 + 冲突解决 + 寄存器分配
    ▼
[Code Generator]
    │
    │  Bytecode (Vec<Bytecode>)
    ▼
[VM Executor]
    │
    │  寄存器执行 + 控制栈 + SEH
    │  原型链查找 + 闭包环境 + JS 语义运算
    ▼
Result (Value)
```

---

## 四、数据类型系统

```rust
/// JS 的值类型
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),          // JS 统一用 f64（符合 ES6 规范）
    String(Rc<String>),
    Symbol(Rc<SymbolData>), // 唯一标识 + 可选的描述
    Object(Rc<RefCell<dyn JSObject>>), // 所有对象类型
}

/// Symbol 内部数据
pub struct SymbolData {
    pub description: Option<String>,
    pub id: u64,          // 全局唯一 ID
}

/// 对象 trait — 所有 JS 对象的核心接口
pub trait JSObject {
    // 属性操作
    fn property_get(&self, key: &PropertyKey) -> Option<ValueRef>;
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
    fn type_of(&self) -> &'static str; // "function", "object", etc.
}

/// 属性键 — 支持 String 和 Symbol
pub enum PropertyKey {
    Str(Rc<String>),
    Symbol(Rc<SymbolData>),
}
```

---

## 五、对象模型与原型链

### 5.1 普通对象

```rust
pub struct OrdinaryObject {
    properties: HashMap<PropertyKey, PropertyDescriptor>,
    prototype: Option<ValueRef>,
    extensible: bool,
}

pub struct PropertyDescriptor {
    pub value: ValueRef,
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    // 访问器属性（可选）
    pub getter: Option<ValueRef>,  // 函数引用
    pub setter: Option<ValueRef>,
}
```

### 5.2 原型链查找逻辑

```rust
// JS 引擎核心：[[Get]] 内部方法
fn internal_get(obj: ValueRef, key: &PropertyKey) -> ValueRef {
    let mut current = obj.clone();
    let mut depth = 0;
    
    loop {
        if depth > MAX_PROTO_DEPTH { return Value::Undefined; }
        
        if let Some(obj_ref) = current.as_object() {
            let obj = obj_ref.borrow();
            // 1. 查自身属性
            if let Some(desc) = obj.property_get(key) {
                if let Some(getter) = desc.getter {
                    // 访问器属性：调用 getter
                    return call_getter(getter, current.clone());
                }
                return desc.value;
            }
            // 2. 沿原型链上溯
            match obj.get_prototype() {
                Some(proto) => current = proto,
                None => return Value::Undefined,
            }
        } else {
            // 非对象无法继续上溯
            // 但 JS 中基本类型也会被自动装箱（Autoboxing）
            return Value::Undefined;
        }
        depth += 1;
    }
}
```

### 5.3 原型链设置检查

```rust
fn internal_set_prototype(obj: &mut dyn JSObject, proto: Option<ValueRef>) -> bool {
    // 禁止引起原型链循环
    if let Some(ref p) = proto {
        if would_cause_circular_prototype_chain(obj, p) {
            return false;
        }
    }
    obj.set_prototype(proto);
    true
}
```

---

## 六、编译管道设计

### 6.1 JSASTLower — 核心降级器

从 oxc AST 直接降级到 IR，复用 evalit 的 IR 指令集 + 扩展。

```rust
pub struct JSASTLower<'a> {
    builder: Box<dyn InstBuilder>,
    symbols: SymbolTable,
    // JS 特有状态
    scope_chain: Vec<ScopeInfo>,     // 作用域链（支持闭包）
    this_binding: Option<ValueRef>,  // 当前 this
    hoisted_decls: Vec<DeclInfo>,     // hoisted 声明
    class_info: Vec<ClassInfo>,       // class 定义信息
    is_strict: bool,                  // 当前是否严格模式
}

impl<'a> JSASTLower<'a> {
    // 入口
    pub fn lower_program(&mut self, program: &oxc_ast::Program<'a>) {
        // 1. 收集所有 hoisted 声明（函数提升、var 提升）
        // 2. 处理 class 定义（编译为方法表 + 原型链）
        // 3. 递归遍历语句体
    }
    
    fn lower_stmt(&mut self, stmt: &oxc_ast::Statement<'a>) { ... }
    fn lower_expr(&mut self, expr: &oxc_ast::Expression<'a>) -> ValueId { ... }
    fn lower_pattern(&mut self, pat: &oxc_ast::BindingPattern<'a>, value: ValueId) { ... }
    fn lower_class(&mut self, class: &oxc_ast::Class<'a>) -> ValueId { ... }
    fn lower_function(&mut self, func: &oxc_ast::Function<'a>) -> ValueId { ... }
    fn lower_arrow_function(&mut self, arrow: &oxc_ast::ArrowFunctionExpression<'a>) -> ValueId { ... }
    fn lower_generator(&mut self, gen: &oxc_ast::Function<'a>) -> ValueId { ... }
    fn lower_async_function(&mut self, func: &oxc_ast::Function<'a>) -> ValueId { ... }
}
```

### 6.2 class 编译策略

```javascript
// 源代码
class Foo extends Bar {
    constructor(x) {
        super(x);
        this.y = 0;
    }
    method() { return this.x; }
    get prop() { return this.y; }
    set prop(v) { this.y = v; }
    static staticMethod() { return 42; }
}
```

编译为 IR：

```rust
// === 模块初始化阶段 ===
// 1. 创建 Foo.prototype 对象，设置 __proto__ = Bar.prototype
foo_proto = CreateObject
set_proto foo_proto, bar_proto

// 2. 将方法挂到 prototype 上
set_property foo_proto, "method", method_func
set_getter foo_proto, "prop", getter_func
set_setter foo_proto, "prop", setter_func

// 3. 创建 Foo 构造函数
foo_ctor = CreateFunction constructor_func
set_property foo_ctor, "prototype", foo_proto

// 4. 将静态方法挂到构造函数上
set_property foo_ctor, "staticMethod", static_func

// 5. 设置 Foo.__proto__ = Bar（静态继承）
set_proto foo_ctor, bar

// === 构造函数体 ===
function Foo_constructor(x) {
    // super(x) → Bar.call(this, x)
    call_method bar, "call", [this, x]
    
    // this.y = 0
    set_property this, "y", 0
}
```

### 6.3 class 运行时创建指令（新增）

需要在 IR/Bytecode 中新增的指令：

| 指令 | 说明 |
|------|------|
| `CreateObject` | 创建空对象（无 prototype 或设为 Object.prototype） |
| `SetPrototype` | 设置对象 `__proto__`（含循环检测） |
| `CreateFunction` | 从函数 body/closures 创建函数对象 |
| `DefineProperty` | 定义属性（含 writable/configurable/enumerable） |
| `DefineAccessor` | 定义 getter/setter 属性 |
| `InstanceOf` | `instanceof` 运算符 |

### 6.4 闭包编译策略

```javascript
function makeCounter() {
    let count = 0;
    return function() { return ++count; };
}
//           ↓
// 检测：count 被内嵌函数引用（逃逸）
//       ↓
// count 不在寄存器分配，改为在 ClosureEnv 中
//       ↓
```

```rust
// 编译后 IR 示意
function makeCounter() {
    // 1. 创建闭包环境（堆分配）
    env = CreateClosureEnv  // env.values[0] = count
    
    // 2. 初始化逃逸变量
    env_set env, 0, 0  // count = 0
    
    // 3. 创建内嵌函数，绑定闭包环境
    inner = CreateClosure inner_func_body, env
    
    return inner
}

// inner 函数体
function inner_func_body() {
    // count 不在寄存器中，通过 closure_env 访问
    tmp = env_get closure_env, 0  // load count
    tmp = js_add tmp, 1           // ++count
    env_set closure_env, 0, tmp   // store count
    return tmp
}
```

新增指令：

| 指令 | 说明 |
|------|------|
| `CreateClosureEnv` | 创建闭包环境（固定大小的 vector） |
| `EnvGet` | 从闭包环境读取值 |
| `EnvSet` | 写入闭包环境 |
| `CreateClosure` | 创建闭包函数（函数体 + 闭包环境） |

### 6.5 Generator 编译策略

```javascript
function* counter() {
    let i = 0;
    while (true) {
        yield i++;
    }
}
//           ↓
// 状态机变换（和 Rust async、C# 迭代器一致）
//           ↓
```

```rust
// 编译为状态机
function counter_state_machine(state, result_slot) {
    switch (state) {
        case 0: goto _entry_0;
        case 1: goto _resume_1;
    }
_entry_0:
    i = 0;  // 提升为持久变量（在闭包环境中）
    // 不直接 goto loop_head，而是 yield 时保存状态
_generate_0:
    tmp = i;
    i = i + 1;
    state = 1;                // 保存 resume 点
    return { value: tmp, done: false };  // yield 返回
_resume_1:
    goto _generate_0;  // 循环继续
    // ...
}
```

Generator 需要新增指令：

| 指令 | 说明 |
|------|------|
| `CreateGenerator` | 从函数创建 generator 对象 |
| `GeneratorNext` | 执行 generator 的 `.next()` |
| `Yield` | 暂停执行并产出一个值 |

---

## 七、VM 运行时设计

### 7.1 核心结构

```rust
pub struct VM {
    state: VMState,
    builtins: BuiltinRegistry,  // 内置对象
    module_loader: Box<dyn ModuleLoader>,
}

pub struct VMState {
    // 寄存器
    registers: Vec<ValueRef>,
    
    // 控制栈（调用帧）
    ctrl_stack: Vec<CtrlFrame>,
    
    // 指令流
    codes: Vec<Bytecode>,
    pc: usize,
    
    // 异常处理
    seh_stack: Vec<SehRecord>,
    
    // 闭包环境
    closure_envs: Vec<Rc<RefCell<ClosureEnv>>>,
    
    // 微任务队列（Promise）
    microtask_queue: VecDeque<ValueRef>,
    
    // 宿主环境
    host: Rc<RefCell<dyn HostContext>>,
}

pub struct CtrlFrame {
    pub return_addr: usize,
    pub saved_rbp: usize,
    pub saved_rsp: usize,
    pub saved_seh_depth: usize,
    pub saved_exc_register: ValueRef,
}
```

### 7.2 JS 专用运算指令

在 evalit 的 ALU 指令基础上，新增 JS 语义专用指令：

```rust
pub enum JsArithOp {
    // JS 加法（字符串优先）
    JsAdd,
    // JS 减法/乘法等（全部 ToNumber）
    JsSub,
    JsMul,
    JsDiv,
    JsRem,
    // JS 比较
    JsEq,       // ==   （带类型转换）
    JsNe,       // !=
    JsStrictEq, // ===  （无类型转换）
    JsStrictNe, // !==
    JsLt,       // <
    JsGt,       // >
    JsLe,       // <=
    JsGe,       // >=
    // 位运算（先 ToInt32）
    JsBitAnd,
    JsBitOr,
    JsBitXor,
    JsBitNot,
    JsShiftLeft,
    JsShiftRight,
    JsShiftRightZeroFill, // >>> 无符号右移
    // 其他
    JsTypeOf,
    JsDelete,
    JsVoid,
    JsInstanceOf,
    JsIn,
}
```

### 7.3 JS 加法运算的具体实现

```rust
fn js_add(left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    // 1. 如果任一操作数是字符串 → 字符串拼接
    if left.is_string() || right.is_string() {
        let l_str = left.to_js_string();   // ToString()
        let r_str = right.to_js_string();
        return Ok(Value::String(Rc::new(format!("{}{}", l_str, r_str))));
    }
    
    // 2. 如果任一操作数是 Symbol → TypeError
    if left.is_symbol() || right.is_symbol() {
        return Err(RuntimeError::TypeError(
            "Cannot convert a Symbol value to a number".into()
        ));
    }
    
    // 3. 否则调用 ToPrimitive → ToNumber → 数值相加
    let a = left.to_js_number();  // ToNumber()
    let b = right.to_js_number();
    Ok(Value::Number(a + b))
}
```

---

## 八、模块系统

```rust
/// 模块加载器接口 — 可插拔
pub trait ModuleLoader {
    fn resolve(&self, specifier: &str, referrer: &ModuleRef)
        -> Result<ModuleRef, ModuleError>;
    
    fn load(&mut self, module: &ModuleRef)
        -> Result<ModuleSource, ModuleError>;
    
    fn compile(&mut self, source: &ModuleSource)
        -> Result<CompiledModule, ModuleError>;
}

/// 编译后的模块
pub struct CompiledModule {
    pub exports: HashMap<String, ValueRef>,
    pub bytecode: Vec<Bytecode>,
}

/// 模块导入编译策略
// import { foo } from './bar.js'
//           ↓
// JSASTLower 阶段：
//   1. 将 import 语句编译为 LoadModule 指令
//   2. LoadModule 在运行时调用 ModuleLoader
//   3. 导入的变量绑定到模块的 exports 槽位
//           ↓
fn lower_import_decl(&mut self, decl: &oxc_ast::ImportDeclaration) {
    let specifier = &decl.source.value;
    let module_ref = self.builder.emit(Instruction::LoadModule {
        specifier: specifier.clone(),
    });
    
    for spec in &decl.specifiers {
        match spec {
            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                let val = self.builder.emit(Instruction::ModuleGetDefault { module: module_ref });
                self.symbols.define(&s.local.name, val);
            }
            ImportDeclarationSpecifier::ImportSpecifier(s) => {
                let val = self.builder.emit(Instruction::ModuleGetNamed {
                    module: module_ref,
                    name: s.imported.name(),
                });
                self.symbols.define(&s.local.name, val);
            }
            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                let val = self.builder.emit(Instruction::ModuleGetNamespace { module: module_ref });
                self.symbols.define(&s.local.name, val);
            }
        }
    }
}
```

---

## 九、错误系统

```rust
/// JS 兼容的错误类型
pub enum RuntimeError {
    // JS Error 类型
    Error { message: String, stack: Vec<StackFrame> },
    TypeError { message: String, stack: Vec<StackFrame> },
    ReferenceError { message: String, stack: Vec<StackFrame> },
    RangeError { message: String, stack: Vec<StackFrame> },
    SyntaxError { message: String, stack: Vec<StackFrame> },
    URIError { message: String, stack: Vec<StackFrame> },
    
    // 引擎内部错误（不应暴露到 JS）
    InternalError { message: String },
    StackOverflow,
    OutOfMemory,
    UnhandledPromiseRejection { value: ValueRef },
}
```

---

## 十、排除特性清单与理由

| 排除特性 | 理由 |
|----------|------|
| `eval(code)` | 需要运行时编译器 + 安全风险 |
| `Function(code)` | 与 eval 同理 |
| `with(obj) { }` | ES6 严格模式已禁用 + 无实际使用场景 |
| `Proxy` / `Reflect` | 影响 VM 所有内部操作路径，P0 不做，P2 以后可选加回 |
| `Object.observe` | 已从规范移除 |

**以下全部保留**（ES6 完整支持）：

`let` / `const` / `class` / `extends` / `super` / 箭头函数 / 模板字符串 / 解构赋值 / 展开运算符 / rest 参数 / 默认参数 / `for...of` / `for...in` / `Symbol` / `Map` / `Set` / `WeakMap` / `WeakSet` / `Promise` / `Generator` / `yield` / `async` / `await` / `Object.setPrototypeOf` / `Object.create` / `Object.freeze` / `Object.seal` / `Object.defineProperty` 完整 / `__proto__` getter/setter / `getter` / `setter` / `typeof` / `delete` / `===` / `instanceof` / `import` / `export` / `Error` 类型体系

---

## 十一、项目结构建议

```
biu/
├── Cargo.toml
├── src/
│   ├── lib.rs                   # 库入口
│   ├── main.rs                  # REPL / CLI（开发阶段）
│   │
│   ├── compiler/                # 编译期
│   │   ├── mod.rs
│   │   ├── ir/                  # IR 定义（从 evalit 移植）
│   │   │   ├── mod.rs
│   │   │   ├── instruction.rs   # Instruction enum
│   │   │   ├── builder.rs       # InstBuilder trait
│   │   │   └── cfg.rs           # ControlFlowGraph
│   │   ├── lowering/            # 降级器
│   │   │   ├── mod.rs
│   │   │   ├── js_lower.rs      # JSASTLower 核心
│   │   │   ├── class.rs         # class 降级
│   │   │   ├── closure.rs       # 闭包检测 + 堆提升
│   │   │   ├── generator.rs     # generator 状态机变换
│   │   │   └── patterns.rs      # 解构/展开降级
│   │   ├── ssabuilder.rs        # SSA 转换（从 evalit 移植）
│   │   ├── regalloc.rs          # 寄存器分配（从 evalit 移植）
│   │   └── codegen.rs           # IR → Bytecode（从 evalit 移植）
│   │
│   ├── vm/                      # 运行时
│   │   ├── mod.rs
│   │   ├── vm.rs                # VM 核心执行引擎
│   │   ├── value.rs             # Value 类型系统
│   │   ├── object.rs            # JSObject trait + OrdinaryObject
│   │   ├── property.rs          # PropertyDescriptor, PropertyKey
│   │   ├── prototype.rs         # 原型链查找逻辑
│   │   ├── closure.rs           # ClosureEnv, ClosureFunction
│   │   ├── bytecode.rs          # Opcode / Bytecode 定义
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
│   │   ├── date_builtins.rs     # Date.*
│   │   ├── json_builtins.rs     # JSON.*
│   │   ├── regexp_builtins.rs   # RegExp.*
│   │   ├── map_set_builtins.rs  # Map, Set, WeakMap, WeakSet
│   │   ├── promise_builtins.rs  # Promise
│   │   ├── symbol_builtins.rs   # Symbol
│   │   └── console_builtins.rs  # console.*（调试用）
│   │
│   ├── module/                  # 模块系统
│   │   ├── mod.rs
│   │   ├── loader.rs            # ModuleLoader trait
│   │   └── resolver.rs          # 模块解析
│   │
│   ├── host/                    # 宿主接口
│   │   ├── mod.rs
│   │   ├── native.rs            # NativeFunction trait
│   │   └── context.rs           # HostContext
│   │
│   └── gc/                      # 内存管理（可选）
│       └── mod.rs               # GC wrapper
│
├── tests/                       # 测试
│   ├── basic_test.rs
│   ├── class_test.rs
│   ├── prototype_test.rs
│   ├── promise_test.rs
│   ├── generator_test.rs
│   └── builtins_test.rs
│
├── examples/                    # 示例
│   └── embedded.rs              # 嵌入式集成示例
│
└── docs/                        # 文档
    ├── architecture.md
    └── embedding.md
```

---

## 十二、P0 里程碑验收标准

```
[✓] 集成 oxc parser，能解析 JS 文件
[✓] JSASTLower 能处理：
      - let/const 声明
      - 函数声明和调用
      - if/while/for 控制流
      - try-catch-throw
      - class + extends + super
      - 箭头函数
      - 模板字符串
      - 数组/对象字面量
      - 原型链完整（Object.create, setPrototypeOf, __proto__）
[✓] VM 能执行生成的字节码
[✓] 以下程序可正确运行：

// P0 验收测试
class Animal {
    constructor(name) { this.name = name; }
    speak() { return `${this.name} makes a noise`; }
}

class Dog extends Animal {
    speak() { return `${this.name} barks`; }
}

const d = new Dog("Rex");
assert(d.speak() === "Rex barks");
assert(d instanceof Animal);
assert(d instanceof Dog);
assert(Object.getPrototypeOf(d) === Dog.prototype);
```

---

## 十三、P0 开发路线（推荐顺序）

| 步骤 | 内容 | 说明 |
|------|------|------|
| 1 | 项目骨架 + Cargo.toml + oxc 集成 | 先跑通 `oxc_parser` |
| 2 | 移植 IR + Builder + CFG（从 evalit） | 确保编译管道骨架可用 |
| 3 | JSASTLower 初版：`1 + 2` 能跑通 | 验证完整链路 |
| 4 | JSASTLower：变量声明 + 函数调用 | 能执行简单程序 |
| 5 | JSASTLower：控制流 (if/while/for) | 基础语句完整 |
| 6 | JSASTLower：模板字符串 + 展开 + 解构 | 语法特性 |
| 7 | 移植 SSA + RegAlloc + Codegen（从 evalit） | 完整编译管道 |
| 8 | Value 类型系统 + JS 专用运算 | JS 语义核心 |
| 9 | 对象系统 + 属性描述符 | 原型链基础 |
| 10 | 原型链完整实现 | 查找 + 设置 + 不变性 |
| 11 | class + extends + super 降级 | class 全支持 |
| 12 | 闭包捕获 + 堆提升 | 函数作为值 |
| 13 | this 绑定 + new 操作符 | 函数调用完整 |

---

## 十四、从 evalit 继承的优势

| evalit 已验证的设计 | biu 直接受益 |
|-------------------|-------------|
| 寄存器式 VM + 双栈架构 | 稳定的执行引擎 |
| SSA 转换 + RegAlloc | 优秀的代码质量 |
| SEH 异常处理 | 完整的 try-catch-throw |
| IR Builder trait | 降级器接口清晰 |
| Codegen 管道 | IR → Bytecode 自动生成 |
| NativeFunction 机制 | 宿主 API 可插拔 |

| evalit 需要改进的 | biu 的做法 |
|------------------|-----------|
| 值类型过于简化 | `Value::Number(f64)` 统一 JS 数字 |
| 无对象属性描述符 | 完整 `PropertyDescriptor` |
| 无原型链 | 完整 `[[Prototype]]` 机制 |
| 无闭包 | `ClosureEnv` + 闭包函数 |
| 函数调用无 `this` | `Call` 指令增加 this 操作数 |
| 无模块系统 | `ModuleLoader` trait |
| 无内置对象库 | 分层实现 ES6 标准库 |