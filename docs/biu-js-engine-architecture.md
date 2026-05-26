# Biu — 嵌入式 JavaScript 引擎架构方案

> 基于 evalit (IR + Bytecode + VM) 架构蓝图，使用 oxc 解析器
> 定位：轻量级、嵌入式、ES6 兼容、Rust 实现

---

## 一、设计原则

1. **静态编译模型**：AOT 编译，无运行时编译器
   - 排除 `eval`、`Function(code)`、`with`（与静态编译不兼容）
   - 排除 `var`、`arguments`（与静态帧索引冲突）
   - 仅支持 `let`/`const`，强制块级作用域

2. **寄存器式 VM**：基于寄存器的执行引擎
   - 静态帧索引替代动态作用域链
   - SSA 形式 IR，支持 Phi 节点
   - 线性寄存器分配

3. **完整原型链支持**：`[[Prototype]]`、`Object.setPrototypeOf`、`Object.create` 全部支持
   - 所有对象类型统一使用 `Rc<RefCell<dyn JSObject>>`
   - 完整的属性描述符系统

4. **严格模式 ES6 子集**：
   - 已支持：箭头函数、let/const、类、模板字符串、解构
   - 延期：Promise、generator、async/await、模块、Proxy

5. **嵌入式优先**：可剥离编译期组件、小型运行时

6. **增量验证**：已跑通 `parse → IR → bytecode → execute` 的完整链路

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
│  │  • 静态帧分配（无动态作用域链）                    │ │
│  │  • 类编译为原型链操作（构造函数+prototype）         │ │
│  │  • 箭头函数支持（词法 this）                       │ │
│  │  • 解构赋值支持                                   │ │
│  │  • 模板字符串支持                                 │ │
│  └──────────────────────┬──────────────────────────┘ │
│                         │ IR (SSA 形式)               │
│  ┌──────────────────────▼──────────────────────────┐ │
│  │  Compiler Pipeline                              │ │
│  │  • SSA Builder (SSA 转换 + 异常边)               │ │
│  │  • Register Allocator (线性扫描寄存器分配)        │ │
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
│  │  • SEH 异常处理 (try-catch-finally-throw)        │ │
│  │  • break/continue/return 与 finally 交互          │ │
│  │  • 原型链查找 ([[Prototype]] 遍历)               │ │
│  │  • JS 专用运算 (Add/Sub/Div/Rem + 比较运算)      │ │
│  └─────────────────────────────────────────────────┘ │
│  ┌─────────────────────────────────────────────────┐ │
│  │ 内置对象库 (Built-ins)                           │ │
│  │  ✅ Object (keys/values/entries/defineProperty/...)│ │
│  │  ✅ Array (push/pop/shift/unshift/join/slice/...) │ │
│  │  ✅ String (charAt/indexOf/slice/split/trim/...)  │ │
│  │  ✅ Number (isFinite/isInteger/toFixed/...)       │ │
│  │  ✅ Boolean                                      │ │
│  │  ✅ Function                                     │ │
│  │  ✅ Error (Error/TypeError/ReferenceError/       │ │
│  │         RangeError/URIError/EvalError)           │ │
│  │  ✅ Symbol (类型存在，暂不可构造)                  │ │
│  │  ⏳ Math, Date, JSON, RegExp (未实现)            │ │
│  │  ⏳ Map, Set, WeakMap, WeakSet (未实现)          │ │
│  │  ⏳ Promise (未实现)                             │ │
│  │  ⏳ Proxy/Reflect (未实现)                       │ │
│  └─────────────────────────────────────────────────┘ │
│  ┌─────────────────────────────────────────────────┐ │
│  │ 宿主接口 (Host API)                             │ │
│  │  • NativeFunction (原生函数支持)                 │ │
│  │  • Memory management (Rc<RefCell> 引用计数)     │ │
│  └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

---

## 三、数据流

```
源文件 (.js)
    │
    ▼
[oxc_parser]  (v0.132)
    │
    │  oxc_ast::Program<'a>
    │  (arena 分配的 AST)
    ▼
[JSASTLower]
    │
    │  • 静态帧分配（寄存器索引）
    │  • 类编译为原型链操作
    │  • 箭头函数（词法 this）
    │  • 解构赋值、模板字符串
    │
    │  IR (Instruction enum)
    ▼
[SSA Builder]
    │
    │  • SSA 转换（支配树、Phi 插入）
    │  • 异常边处理（try-catch-finally）
    ▼
[Register Allocator]
    │
    │  • 活跃区间分析
    │  • 线性扫描寄存器分配
    ▼
[Code Generator]
    │
    │  Bytecode (Vec<Bytecode>)
    │  • 45+ 种操作码
    │  • 常量池 + 符号表
    ▼
[VM Executor]
    │
    │  • 寄存器执行
    │  • 控制栈 + SEH 异常处理
    │  • 原型链查找
    │  • JS 语义运算
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
    Function(u32),        // 内部：字节码函数引用
}

/// Symbol 内部数据
pub struct SymbolData {
    pub description: Option<String>,
    pub id: u64,          // 全局唯一 ID
}

/// 对象 trait — 所有 JS 对象的核心接口
pub trait JSObject: fmt::Debug + Any {
    fn kind(&self) -> ObjectKind;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    
    // 属性操作
    fn property_get(&self, key: &PropertyKey) -> Option<PropertyDescriptor>;
    fn property_set(&mut self, key: PropertyKey, value: Value) -> Result<bool, String>;
    fn property_delete(&mut self, key: &PropertyKey) -> bool;
    fn has_property(&self, key: &PropertyKey) -> bool;
    fn own_keys(&self) -> Vec<PropertyKey>;
    
    // 原型操作
    fn get_prototype(&self) -> Option<Rc<RefCell<dyn JSObject>>>;
    fn set_prototype(&mut self, proto: Option<Rc<RefCell<dyn JSObject>>>);
    
    // 不变性检查
    fn is_extensible(&self) -> bool;
    fn prevent_extensions(&mut self);
    fn is_frozen(&self) -> bool;
    fn freeze(&mut self);
    fn is_sealed(&self) -> bool;
    fn seal(&mut self);
    
    // 内部类型标记
    fn type_of(&self) -> &'static str { "object" }
}

/// 属性键 — 支持 String 和 Symbol
pub enum PropertyKey {
    Str(Rc<String>),
    Symbol(Rc<SymbolData>),
}

/// 属性描述符
pub struct PropertyDescriptor {
    pub value: Value,
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub getter: Option<Value>,
    pub setter: Option<Value>,
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

从 oxc AST 直接降级到 IR。

```rust
pub struct JSASTLower<'a> {
    builder: Box<dyn InstBuilder>,
    symbols: SymbolTable,
    // JS 特有状态
    this_binding: Option<ValueId>,   // 当前 this
    loop_context: Option<LoopContext>, // 循环上下文（break/continue）
}

impl<'a> JSASTLower<'a> {
    // 入口
    pub fn lower_program(&mut self, program: &oxc_ast::Program<'a>) {
        // 递归遍历语句体
    }
    
    fn lower_stmt(&mut self, stmt: &oxc_ast::Statement<'a>) { ... }
    fn lower_expr(&mut self, expr: &oxc_ast::Expression<'a>) -> ValueId { ... }
    fn lower_pattern(&mut self, pat: &oxc_ast::BindingPattern<'a>, value: ValueId) { ... }
    fn lower_class(&mut self, class: &oxc_ast::Class<'a>) -> ValueId { ... }
    fn lower_function(&mut self, func: &oxc_ast::Function<'a>) -> ValueId { ... }
    fn lower_arrow_function(&mut self, arrow: &oxc_ast::ArrowFunctionExpression<'a>) -> ValueId { ... }
    fn lower_try_catch(&mut self, stmt: &oxc_ast::TryStatement<'a>) { ... }
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
// 当前实现：静态帧索引，无闭包环境
// 所有变量通过寄存器分配
//       ↓
```

**注意**：当前实现采用静态帧索引，暂不支持闭包（函数内嵌套函数并引用外部变量）。
所有变量通过寄存器分配管理。

---

## 七、VM 运行时设计

### 7.1 核心结构

```rust
pub struct VM {
    state: VMState,
    builtins: Builtins,  // 内置对象
}

pub struct VMState {
    // 寄存器文件
    registers: Vec<Value>,
    rsp: usize,           // 栈指针
    rbp: usize,           // 基址指针
    
    // 控制栈（调用帧）
    ctrl_stack: Vec<usize>,
    
    // 程序计数器
    pc: usize,
    
    // 异常处理栈
    seh_stack: Vec<SehRecord>,
    
    // 全局变量
    globals: HashMap<String, Value>,
    
    // 当前 this 值
    this_val: Value,
}

/// SEH (Structured Exception Handling) 记录
struct SehRecord {
    handler_pc: usize,        // catch handler PC
    finally_pc: usize,        // finally handler PC
    saved_rsp: usize,
    saved_rbp: usize,
    in_finally: bool,
    pending_exception: Option<Value>,
    catch_executed: bool,
    delayed_jump_target: Option<isize>,  // break/continue
    delayed_return: bool,                // return in finally
}
```

### 7.2 字节码指令集

当前实现 45+ 种操作码：

```rust
pub enum Opcode {
    // 数据移动
    LoadConst,      // 加载常量
    LoadEnv,        // 加载环境变量
    Mov,            // 寄存器移动
    Push, Pop,      // 栈操作
    PushC, PopC,    // 控制栈操作
    
    // 算术运算
    Add, Sub, Mul, Div, Rem, Neg,  // 基础运算
    Addx, Subx, Mulx, Divx, Remx,  // 对象运算（带类型转换）
    
    // 位运算
    BitAnd, BitOr, BitXor, BitNot,
    ShiftLeft, ShiftRight, ShiftRightZeroFill,
    
    // 比较运算
    Less, LessEqual, Greater, GreaterEqual,
    Equal, NotEqual, StrictEqual, NotStrictEqual,
    
    // 逻辑运算
    Not, And, Or,
    
    // 控制流
    Jump, BrIf,     // 跳转
    Call, CallEx, CallNative,  // 函数调用
    Ret, Halt,      // 返回/停止
    
    // 对象操作
    PropGet, PropSet, PropDelete,
    New, InstanceOf, TypeOf,
    
    // 异常处理
    TryCatch,       // 设置异常处理
    EndTry,         // 结束 try 块
    ThrowExc,       // 抛出异常
    ResumeExc,      // 恢复异常处理
    DelayedJump,    // 延迟跳转（break/continue in finally）
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

## 八、错误系统

```rust
/// JS 兼容的错误类型
pub enum RuntimeError {
    // 标准 JS Error 类型（已映射到 JS Error 对象）
    Error(String),           // Error.prototype
    TypeError(String),       // TypeError.prototype
    ReferenceError(String),  // ReferenceError.prototype
    RangeError(String),      // RangeError.prototype
    SyntaxError(String),     // SyntaxError.prototype
    URIError(String),        // URIError.prototype
    EvalError(String),       // EvalError.prototype
    
    // 引擎内部错误
    NotImplemented(String),
    InternalError(String),
}
```

---

## 九、实现状态与排除特性

### 9.1 已实现的 ES6 特性

| 特性 | 状态 | 说明 |
|------|------|------|
| `let` / `const` | ✅ | 块级作用域 |
| 箭头函数 | ✅ | 词法 this |
| 类 (class) | ✅ | extends, super, 方法定义 |
| 模板字符串 | ✅ | 基本插值支持 |
| 解构赋值 | ✅ | 对象/数组解构 |
| 展开运算符 | ✅ | 函数调用和数组字面量 |
| rest 参数 | ✅ | `...args` |
| 默认参数 | ✅ | 函数参数默认值 |
| `for...of` | ✅ | 可迭代对象遍历 |
| `Symbol` | ⚠️ | 类型存在，暂不可构造 |
| `Object` | ✅ | 完整静态方法和原型方法 |
| `Array` | ✅ | 主要原型方法 |
| `String` | ✅ | 主要原型方法 |
| `Number` | ✅ | 静态方法和原型方法 |
| `Boolean` | ✅ | 构造函数和原型 |
| `Function` | ✅ | 构造函数和原型 |
| Error 类型 | ✅ | 6种标准 Error 类型 |
| try-catch-finally | ✅ | 完整异常处理 |
| instanceof | ✅ | 运算符支持 |
| typeof | ✅ | 运算符支持 |

### 9.2 排除特性（与静态编译不兼容）

| 排除特性 | 理由 |
|----------|------|
| `eval(code)` | 需要运行时编译器 |
| `Function(code)` | 与 eval 同理 |
| `with(obj) { }` | 与静态编译不兼容 |
| `var` | 与静态帧索引冲突 |
| `arguments` | 与静态帧索引冲突 |

### 9.3 延期特性（未来实现）

| 特性 | 优先级 | 说明 |
|------|--------|------|
| `Promise` | P1 | 异步基础 |
| `async/await` | P1 | 语法糖 |
| `Generator` | P2 | 状态机变换 |
| `Map` / `Set` | P2 | 新集合类型 |
| `Proxy` / `Reflect` | P3 | 元编程 |
| 模块系统 (import/export) | P2 | 模块加载器 |

---

## 十、项目结构

```
biujs/
├── Cargo.toml                   # oxc_parser v0.132
├── src/
│   ├── lib.rs                   # 库入口：Compiler, VM, Value, RuntimeError
│   ├── main.rs                  # CLI 工具
│   │
│   ├── bytecode.rs              # Bytecode, Opcode, Module 定义
│   │
│   ├── compiler/                # 编译期
│   │   ├── mod.rs              # Compiler 主入口
│   │   ├── error.rs            # CompileError
│   │   ├── symbol.rs           # SymbolTable
│   │   ├── codegen.rs          # IR → Bytecode 代码生成
│   │   ├── regalloc.rs         # 寄存器分配
│   │   └── ir/                 # 中间表示
│   │       ├── mod.rs
│   │       ├── instruction.rs   # Instruction enum
│   │       ├── builder.rs      # InstBuilder trait
│   │       ├── cfg.rs          # ControlFlowGraph
│   │       └── ssabuilder.rs   # SSA 转换
│   │
│   ├── vm/                     # 运行时
│   │   ├── mod.rs              # VM 主入口（SEH 异常处理）
│   │   ├── value.rs            # Value 类型系统
│   │   ├── object.rs           # JSObject trait + NativeFunctionObject
│   │   ├── property.rs         # PropertyDescriptor, PropertyKey, ObjectKind
│   │   └── prototype.rs        # 原型链查找逻辑
│   │
│   ├── builtins/               # 内置对象库
│   │   ├── mod.rs              # Builtins 注册表
│   │   ├── object.rs           # Object.* 静态方法
│   │   ├── array.rs            # Array.* 原型方法
│   │   ├── string.rs           # String.* 原型方法
│   │   ├── number.rs           # Number.* 静态/原型方法
│   │   ├── boolean.rs          # Boolean.* 原型方法
│   │   ├── function.rs         # Function.* 原型方法
│   │   └── error.rs            # Error, TypeError, RangeError 等
│   │
│   ├── module/                 # 模块系统（预留）
│   │   └── mod.rs
│   │
│   └── host/                   # 宿主接口
│       └── mod.rs
│
├── tests/
│   ├── test262_runner.rs       # test262 测试套件运行器
│   ├── helpers.rs              # 测试辅助函数
│   └── features/               # 功能测试
│       ├── control_flow.rs     # 控制流 + 异常处理测试
│       └── functions.rs        # 函数测试
│
└── docs/
    ├── biu-js-engine-architecture.md
    ├── es6-feature-support.md
    └── exception-handler.md
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

## 十一、实现里程碑

### M1（已完成）
- [x] oxc_parser 集成（v0.132）
- [x] IR + SSA + 寄存器分配 + 代码生成
- [x] 寄存器式 VM 执行引擎
- [x] let/const 变量声明
- [x] 基础运算（+ - * / %）
- [x] 控制流（if/while/for）
- [x] 函数声明和调用
- [x] 箭头函数
- [x] 模板字符串
- [x] 解构赋值

### M2（已完成）
- [x] 类（class）+ extends + super
- [x] 原型链系统（Object.create, setPrototypeOf）
- [x] 属性描述符（defineProperty, getOwnPropertyDescriptor）
- [x] instanceof 运算符
- [x] try-catch-finally 异常处理
- [x] break/continue/return 与 finally 交互
- [x] 6种 Error 类型（Error, TypeError, ReferenceError, RangeError, URIError, EvalError）
- [x] Object 静态方法（keys/values/entries/defineProperty/...）
- [x] Array 原型方法（push/pop/shift/unshift/join/slice/...）
- [x] String 原型方法（charAt/indexOf/slice/split/trim/...）
- [x] Number 静态/原型方法（isFinite/isInteger/toFixed/...）
- [x] test262 测试框架集成

### M3（规划中）
- [ ] Promise 基础实现
- [ ] async/await 语法支持
- [ ] Math 对象
- [ ] Date 对象
- [ ] JSON 对象

### M4（远期）
- [ ] Map / Set / WeakMap / WeakSet
- [ ] Generator / yield
- [ ] Proxy / Reflect
- [ ] 模块系统（import/export）

---

## 十二、技术特点

| 技术特点 | 实现说明 |
|----------|----------|
| 静态编译 | AOT 编译，无运行时编译器，排除 eval/Function/with |
| 寄存器式 VM | 基于寄存器的执行引擎，45+ 种操作码 |
| SSA IR | 静态单赋值形式，支持 Phi 节点和异常边 |
| 线性寄存器分配 | 基于活跃区间的线性扫描算法 |
| 完整原型链 | `[[Prototype]]` 机制，Object.create/setPrototypeOf |
| SEH 异常处理 | try-catch-finally，break/continue/return 与 finally 交互 |
| 属性描述符 | writable/enumerable/configurable + getter/setter |
| 引用计数 | `Rc<RefCell>` 内存管理，无 GC 暂停 |
| test262 集成 | 标准 ECMAScript 测试套件验证 |