# Biu JS Engine — 任务计划书

## 总体策略

采用增量验证的方式，每完成一个里程碑都能运行一个完整的 `parse → IR → bytecode → execute` 链路。
优先复用 evalit 的成熟组件，最小化重写工作量。

---

## 里程碑 0: 项目基础设施搭建

**目标**: 搭建项目骨架，集成 oxc 解析器，验证解析能力。

### 任务 0.1: 更新 Cargo.toml

```toml
[dependencies]
oxc_parser = "..."
oxc_ast = "..."
oxc_span = "..."
oxc_allocator = "..."
log = "0.4"
petgraph = "0.8"
env_logger = "0.11"

[dev-dependencies]
criterion = "0.6"
```

### 任务 0.2: 创建基础模块结构

```
src/
├── lib.rs
├── main.rs
├── compiler/
│   ├── mod.rs
│   └── ir/
│       └── mod.rs
├── vm/
│   ├── mod.rs
│   ├── value.rs
│   └── bytecode.rs
└── builtins/
    └── mod.rs
```

### 任务 0.3: oxc 解析器集成测试

编写测试，验证 oxc_parser 能正确解析简单 JS 代码:
```rust
#[test]
fn test_oxc_parse() {
    let source = "let x = 1 + 2;";
    let program = parse_js(source);
    assert!(program.is_ok());
}
```

**验收标准**: oxc_parser 能解析基本 JS 语句，输出 AST 结构。

---

## 里程碑 1: IR 与 Bytecode 基础 (从 evalit 移植)

**目标**: 移植 evalit 的 IR + CFG + SSA + RegAlloc + Codegen 核心组件。

### 任务 1.1: 移植 IR 定义

从 evalit 复制:
- `compiler/ir/instruction.rs` → `src/compiler/ir/instruction.rs`
- `compiler/ir/cfg.rs` → `src/compiler/ir/cfg.rs`
- `compiler/ir/builder.rs` → `src/compiler/ir/builder.rs`
- `compiler/ir/ssabuilder.rs` → `src/compiler/ir/ssabuilder.rs`

修改:
- 移除 evalit 特有的指令 (Range 系列)
- 预留 JS 指令扩展位

### 任务 1.2: 移植 Bytecode 定义

从 evalit 复制 `bytecode.rs`，修改:
- 移除 Range 系列 Opcode
- 保留核心控制流、算术、比较指令
- 预留 JS 扩展 Opcode

### 任务 1.3: 移植 RegAlloc

从 evalit 复制 `compiler/regalloc.rs`，无修改。

### 任务 1.4: 移植 Codegen

从 evalit 复制 `compiler/codegen.rs`，修改:
- 移除 Range 相关代码生成
- 保留核心指令生成逻辑

**验收标准**: IR → SSA → RegAlloc → Codegen 管道能编译简单的算术表达式。

---

## 里程碑 2: JS Value 类型系统

**目标**: 实现 JS 的 Value 类型，替代 evalit 的类型系统。

### 任务 2.1: 实现 Value enum

```rust
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Rc<String>),
    Symbol(Rc<SymbolData>),
    Object(Rc<RefCell<dyn JSObject>>),
}
```

### 任务 2.2: 实现类型转换

- `ToNumber()`: 各类型 → f64
- `ToString()`: 各类型 → String
- `ToBoolean()`: 各类型 → bool
- `ToPrimitive()`: 对象 → 基本类型

### 任务 2.3: 实现 JS 语义运算

- `js_add`: 字符串优先，否则 ToNumber
- `js_sub/mul/div/rem`: 强制 ToNumber
- `js_eq/ne`: 抽象相等 (带类型转换)
- `js_strict_eq/ne`: 严格相等 (无类型转换)

### 任务 2.4: 实现 typeof 运算

```rust
fn js_typeof(value: &Value) -> &'static str {
    match value {
        Value::Undefined => "undefined",
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Object(obj) => {
            if obj.borrow().is_callable() { "function" } else { "object" }
        }
    }
}
```

**验收标准**: Value 类型系统能正确处理 JS 的类型转换语义。

---

## 里程碑 3: JSASTLower 初版 — 基本表达式

**目标**: 实现 JSASTLower，能编译基本的 JS 表达式和语句。

### 任务 3.1: 实现 JSASTLower 框架

```rust
pub struct JSASTLower<'a> {
    builder: &'a mut dyn InstBuilder,
    symbols: SymbolTable<Variable>,
    scope_chain: Vec<ScopeInfo>,
    env: &'a Environment,
}
```

### 任务 3.2: 实现基本语句降级

- `let/const/var` 声明
- 表达式语句
- 赋值语句
- `return` 语句

### 任务 3.3: 实现基本表达式降级

- 字面量 (number, string, boolean, null, undefined)
- 二元运算 (+, -, *, /, %)
- 比较运算 (==, ===, !=, !==, <, >, <=, >=)
- 逻辑运算 (&&, ||, !)
- 一元运算 (-, +, !, typeof, void)

### 任务 3.4: 端到端测试

```rust
#[test]
fn test_basic_arithmetic() {
    let result = eval("1 + 2 * 3");
    assert_eq!(result, Value::Number(7.0));
}
```

**验收标准**: 能编译并执行 `1 + 2 * 3`、`let x = 10; x + 5` 等基本代码。

---

## 里程碑 4: 控制流与函数

**目标**: 实现控制流和函数调用。

### 任务 4.1: 控制流降级

- `if/else` 语句
- `while` 循环
- `for` 循环
- `for...of` 循环
- `break/continue`
- `switch/case`

### 任务 4.2: 函数声明与调用

- 函数声明 (`function foo() {}`)
- 函数调用 (`foo()`)
- 参数传递
- `return` 语句

### 任务 4.3: this 绑定

- 普通函数调用: this = undefined (严格模式) / global
- 方法调用: this = 对象
- `call/apply/bind`

### 任务 4.4: try/catch/throw

复用 evalit 的 SEH 机制，适配 JS 语法。

**验收标准**: 能执行包含 if/while/for/函数/try-catch 的完整程序。

---

## 里程碑 5: 对象系统与原型链

**目标**: 实现完整的 JS 对象模型。

### 任务 5.1: OrdinaryObject 实现

```rust
pub struct OrdinaryObject {
    properties: HashMap<PropertyKey, PropertyDescriptor>,
    prototype: Option<ValueRef>,
    extensible: bool,
}
```

### 任务 5.2: PropertyDescriptor

- 数据属性 (value, writable, enumerable, configurable)
- 访问器属性 (get, set, enumerable, configurable)

### 任务 5.3: 原型链查找

- `[[Get]]`: 沿原型链查找属性
- `[[Set]]`: 考虑原型链上的 setter
- `[[HasProperty]]`: 沿原型链检查

### 任务 5.4: 对象字面量

```rust
// { a: 1, b: "hello" }
// 编译为:
obj = CreateObject
DefineProperty obj, "a", 1
DefineProperty obj, "b", "hello"
```

### 任务 5.5: 数组

```rust
// [1, 2, 3]
// 编译为:
arr = CreateArray
ArrayPush arr, 1
ArrayPush arr, 2
ArrayPush arr, 3
```

### 任务 5.6: 点操作符与括号操作符

- `obj.prop` → PropertyGet
- `obj["prop"]` → IndexGet
- `obj.prop = value` → PropertySet
- `obj["prop"] = value` → IndexSet

**验收标准**: 能创建和操作对象，原型链查找正确。

---

## 里程碑 6: Class 与继承

**目标**: 实现 ES6 class 语法。

### 任务 6.1: 基本 class 降级

```javascript
class Foo {
    constructor(x) { this.x = x; }
    method() { return this.x; }
}
```

### 任务 6.2: extends 与 super

```javascript
class Bar extends Foo {
    constructor(x, y) {
        super(x);
        this.y = y;
    }
}
```

### 任务 6.3: 静态方法

```javascript
class Foo {
    static create() { return new Foo(); }
}
```

### 任务 6.4: getter/setter

```javascript
class Foo {
    get value() { return this._value; }
    set value(v) { this._value = v; }
}
```

**验收标准**: 能定义 class、继承、调用方法、instanceof 检查。

---

## 里程碑 7: 闭包与作用域

**目标**: 实现闭包捕获和词法作用域。

### 任务 7.1: 闭包检测

分析内嵌函数引用的外部变量，标记为逃逸变量。

### 任务 7.2: ClosureEnv 实现

```rust
pub struct ClosureEnv {
    values: Vec<ValueRef>,
}
```

### 任务 7.3: 闭包编译

- 逃逸变量从寄存器移到 ClosureEnv
- CreateClosureEnv / EnvGet / EnvSet 指令

### 任务 7.4: 箭头函数

箭头函数继承外层 this。

**验收标准**: 闭包能正确捕获和修改外部变量。

---

## 里程碑 8: 内置对象 (基础)

**目标**: 实现最基本的内置对象。

### 任务 8.1: console

- `console.log()`
- `console.error()`

### 任务 8.2: Object

- `Object.create(proto)`
- `Object.keys(obj)`
- `Object.values(obj)`
- `Object.entries(obj)`
- `Object.assign(target, ...sources)`
- `Object.defineProperty(obj, key, desc)`
- `Object.getPrototypeOf(obj)`
- `Object.setPrototypeOf(obj, proto)`
- `Object.freeze(obj)`
- `Object.seal(obj)`

### 任务 8.3: Array

- `Array.isArray()`
- `Array.prototype.push/pop/shift/unshift`
- `Array.prototype.map/filter/reduce/forEach`
- `Array.prototype.find/findIndex`
- `Array.prototype.slice/splice`
- `Array.prototype.includes`
- `Array.prototype.join`

### 任务 8.4: String

- `String.prototype.length`
- `String.prototype.charAt/charCodeAt`
- `String.prototype.indexOf/lastIndexOf`
- `String.prototype slice/substring`
- `String.prototype split/join`
- `String.prototype toUpperCase/toLowerCase`
- `String.prototype trim`
- `String.prototype replace/replaceAll`
- `String.prototype includes/startsWith/endsWith`

### 任务 8.5: Number

- `Number.isNaN()`
- `Number.isFinite()`
- `Number.isInteger()`
- `Number.prototype.toFixed()`

### 任务 8.6: Math

- `Math.floor/ceil/round`
- `Math.max/min`
- `Math.abs`
- `Math.random`
- `Math.sqrt/pow`

### 任务 8.7: JSON

- `JSON.parse()`
- `JSON.stringify()`

### 任务 8.8: Error

- `Error`, `TypeError`, `ReferenceError`, `RangeError`, `SyntaxError`

**验收标准**: 能使用基本的内置对象和方法。

---

## 里程碑 9: Generator 与迭代器

**目标**: 实现 Generator 函数和迭代器协议。

### 任务 9.1: 迭代器协议

- `Symbol.iterator`
- `next()` 方法

### 任务 9.2: Generator 函数

```javascript
function* counter() {
    let i = 0;
    while (true) {
        yield i++;
    }
}
```

### 任务 9.3: 状态机变换

将 generator 编译为状态机。

### 任务 9.4: yield* 委托

```javascript
function* concat(a, b) {
    yield* a;
    yield* b;
}
```

**验收标准**: Generator 能正确暂停和恢复执行。

---

## 里程碑 10: Promise 与 async/await

**目标**: 实现 Promise 和异步编程支持。

### 任务 10.1: Promise 基础

- `new Promise((resolve, reject) => {})`
- `.then()/.catch()/.finally()`
- `Promise.all()`/`Promise.race()`

### 任务 10.2: 微任务队列

实现微任务调度器。

### 任务 10.3: async/await

```javascript
async function fetchData() {
    const response = await fetch(url);
    return response.json();
}
```

**验收标准**: async/await 能正确执行异步操作。

---

## 里程碑 11: 模块系统

**目标**: 实现 ES6 模块系统。

### 任务 11.1: ModuleLoader trait

```rust
pub trait ModuleLoader {
    fn resolve(&self, specifier: &str, referrer: &str) -> Result<String, ModuleError>;
    fn load(&mut self, path: &str) -> Result<String, ModuleError>;
}
```

### 任务 11.2: import/export 编译

- `import { foo } from './bar.js'`
- `export default ...`
- `export { name }`

### 任务 11.3: 模块缓存

避免重复加载同一模块。

**验收标准**: 能通过 import/export 组织代码。

---

## 里程碑 12: Symbol

**目标**: 实现 Symbol 类型。

### 任务 12.1: Symbol 创建

- `Symbol()` / `Symbol("description")`
- `Symbol.for()` / `Symbol.keyFor()`
- `Symbol.iterator` / `Symbol.toPrimitive` 等内置 Symbol

### 任务 12.2: Symbol 作为属性键

```javascript
const sym = Symbol("key");
obj[sym] = "value";
```

**验收标准**: Symbol 能正确创建和使用。

---

## 里程碑 13: 性能优化与完善

**目标**: 优化性能，完善边界情况。

### 任务 13.1: 内联缓存 (可选)

属性访问的内联缓存优化。

### 任务 13.2: 尾调用优化 (可选)

### 任务 13.3: 完善错误信息

添加源码位置信息到错误堆栈。

### 任务 13.4: GC 或引用计数优化

### 任务 13.5: 性能基准测试

**验收标准**: 通过 P0 验收测试，性能达到可接受水平。

---

## P0 验收测试

```javascript
// 1. 基本类型和运算
assert(1 + 2 === 3);
assert("hello" + " world" === "hello world");
assert(typeof undefined === "undefined");
assert(typeof null === "object");

// 2. 变量和控制流
let x = 10;
if (x > 5) {
    x = x * 2;
}
assert(x === 20);

// 3. 函数
function add(a, b) { return a + b; }
assert(add(3, 4) === 7);

// 4. 闭包
function makeCounter() {
    let count = 0;
    return function() { return ++count; };
}
let counter = makeCounter();
assert(counter() === 1);
assert(counter() === 2);

// 5. 对象和原型链
let obj = { x: 1, y: 2 };
assert(obj.x === 1);
assert(Object.keys(obj).length === 2);

// 6. Class 和继承
class Animal {
    constructor(name) { this.name = name; }
    speak() { return this.name + " makes a noise"; }
}
class Dog extends Animal {
    speak() { return this.name + " barks"; }
}
let d = new Dog("Rex");
assert(d.speak() === "Rex barks");
assert(d instanceof Animal);
assert(d instanceof Dog);
assert(Object.getPrototypeOf(d) === Dog.prototype);

// 7. 数组
let arr = [1, 2, 3];
assert(arr.length === 3);
assert(arr[1] === 2);
arr.push(4);
assert(arr.length === 4);

// 8. 内置对象
assert(Math.floor(3.7) === 3);
assert(JSON.stringify({a: 1}) === '{"a":1}');
assert(Array.isArray([1, 2]));
```

---

## 开发顺序建议

1. **里程碑 0** (1天): 项目搭建 + oxc 集成
2. **里程碑 1** (2天): IR + Bytecode 移植
3. **里程碑 2** (2天): Value 类型系统
4. **里程碑 3** (2天): JSASTLower 基础
5. **里程碑 4** (2天): 控制流与函数
6. **里程碑 5** (3天): 对象系统与原型链
7. **里程碑 6** (2天): Class 与继承
8. **里程碑 7** (2天): 闭包与作用域
9. **里程碑 8** (3天): 内置对象
10. **里程碑 9** (2天): Generator
11. **里程碑 10** (2天): Promise/async/await
12. **里程碑 11** (1天): 模块系统
13. **里程碑 12** (1天): Symbol
14. **里程碑 13** (2天): 优化与完善

**总计**: 约 27 天 (一人全职开发)
