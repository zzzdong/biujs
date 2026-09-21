# ES6 一致性计划（M2 收尾 → M6）

> **日期**：2026-09-20（初版）；2026-09-21 更新 —— M2' 完成、M3-B1 首批交付（见 §2.1b/§2.2b）；2026-09-21 二批 —— M3-B1 收尾（ToObject 装箱、描述符校验、数组元素访问器、Object.assign 下沉 VM），见 §2.1c/§2.2c；2026-09-21 三批 —— SEH 跨帧展开修复（见 §2.1d）；2026-09-22 四批 —— M3-B2 首批（Array 回调方法泛型化、indexOf/lastIndexOf 分派、String.prototype.indexOf 补齐），见 §2.1e
> **目标来源**：`README.md:3-4` —— *"A JavaScript engine implemented in Rust. Targeted to support **strict mode ES6** features but without `eval` or eval-like features or `with` statement."*
> **基线**：test262 **6015 / 10366**（58.03% 通过，4351 失败，14441 跳过；M3-B2 首批后）；单元测试 190；feature 集成 25 个文件；runner 启用 89 个套件
> **上一阶段**：M1（迭代器 / for-of / 解构 / 展开 / 模板 / 默认参数）与 M2（class：静态成员、访问器、extends/super、public 字段）已交付

---

## 1. 目标的可验收定义

### 1.1 README 目标拆成三条硬约束

| # | 约束 | 判定方式 |
|---|------|----------|
| G1 | **strict mode** | 代码一律按严格模式语义执行（未声明变量赋值、删除不可删属性、重复形参等抛错）；不提供非严格模式的降级路径 |
| G2 | **ES6（ES2015）特性集** | 以 ES2015 语言 + 内置对象为完成线；见 1.2 的范围判定 |
| G3 | **无 `eval` / eval-like / `with`** | 词法/静态绑定是架构前提；`eval`、`new Function(...)` 形式的动态代码、`with` 永久不支持（解析器可拒绝），`Function` 构造器不作为代码执行入口 |

### 1.2 ES2015 范围判定（在范围 / 不在范围）

| ES2015 特性 | 判定 | 现状 | 备注 |
|-------------|------|------|------|
| `let` / `const` / 块级作用域 | 在 | ✅ | |
| `var` / `arguments` | 在 | ✅ | 文档 `es6-feature-support.md` 中"不支持 var/arguments"的描述**已过时**，需订正 |
| 箭头函数、词法 `this` | 在 | ✅ | 闭包为**创建时值快照**语义（架构决定，见 §5） |
| 类（构造 / 方法 / 静态 / 访问器 / extends / super） | 在 | ✅ | M2 已交付；计算属性名、`new.target` 待补 |
| 模板字符串（含 tagged 基础形态） | 在 | ⚠️ | 插值已支持；`raw` 冻结数组与 call-site 缓存未做 |
| 解构（数组 / 对象 / 参数 / 赋值目标 / 嵌套 / 默认值 / rest） | 在 | ✅ | M1 + M1 收尾 |
| 默认参数、剩余参数、展开（调用 / `new` / 数组 / 对象） | 在 | ✅ | |
| `for-of` / `for-in`、迭代协议 | 在 | ⚠️ | 双路径迭代已可用；`return()`/`throw()` 与生成器未完整 |
| **生成器 `function*` / `yield`** | 在 | ❌ | 最大单项差距（4061 个测试被跳过） |
| `Symbol` 与 well-known symbols | 在 | ⚠️ | `Symbol()` 存在，`Symbol.iterator` 已注册；`toStringTag`/`toPrimitive`/`species` 等未齐 |
| `Map` / `Set` / `WeakMap` / `WeakSet` | 在 | ❌ | 目录规模 204/383/141/34 |
| `Promise` | 在 | ❌ | 目录规模 677 |
| `Proxy` / `Reflect` | 在 | ❌ | 目录规模 311/153 |
| `TypedArray` / `ArrayBuffer` / `DataView` | 在（降级） | ❌ | 规模 1446/221/561；可定位为 M6 的"尽力而为"项 |
| 内置对象 ES6 增补（Object/Array/String/Number/Math 新方法） | 在 | ⚠️ | 失败池的主体，见 §3.1 |
| `JSON` / `Date` | 在（ES5 但属必备） | ❌ | 规模 165/594 |
| 计算属性名、`**`（ES2016）、`new.target` | 在 | ❌ | 低成本、`**` 与 `new.target` 属 ES2015 边界项（`**` 为 ES2016，顺带） |
| ES 模块 `import` / `export` | **不在** | ❌ | 静态编译 + 无 eval 前提下仅可能做静态链接；本计划不承诺 |

| ES2016+ 特性 | 判定 | 理由 |
|--------------|------|------|
| `async` / `await`、异步迭代、`for-await` | **不在** | 非 ES6；依赖 Promise 与生成器，可在 M5 后评估 |
| BigInt、可选链 `?.`、空值合并 `??`、逻辑赋值 | **不在** | 非 ES6（`??`/`?.` 可顺带，成本极低） |
| class 私有字段/方法（`#x`）、static block | **不在** | 非 ES6（ES2022） |
| `Temporal` / `Intl` / `SharedArrayBuffer` / `Atomics` | **不在** | 非 ES6 且依赖宿主能力 |
| RegExp 及 `u`/`y` 标志 | **不在** | 引擎无正则实现（1879 个测试，独立子项目量级） |

---

## 2. 当前状态

### 2.1 测试基线（2026-09-20 全量）

| 指标 | 数值 |
|------|------|
| test262 已执行 | 10240 |
| 通过 | **3908**（38.16%，M2 结束时） |
| 失败 | 6332 |
| 跳过（特性表 + 未启用套件） | 14432 |
| 单元测试 | 190（全绿） |
| feature 集成测试 | 17 个文件 |

> **KPI 约定**：解锁特性会让分母变大、通过率下降，因此**主指标是通过的绝对数 + 目标套件通过率**，通过率仅作参考。

### 2.1b M2' + M3-B1 进展（本次工作，逐套件实测）

M2' 三项语法收尾（S1 计算属性名、S2 `new.target`、S4 `**`、S5 `??`）与 S3 well-known symbols 接入已提交，随后完成 M3-B1（`Object`）第一批。
定向套件通过数（同一命令、同一机器）：

| 套件 | M2 结束 | 本次工作后 |
|------|---------|-----------|
| `language/statements/class` | 33 | **42** |
| `language/expressions/object` | 33 | 35 |
| `language/expressions/assignment` | 13 | 14 |
| `built-ins/Object` | 1029 | **1465** |
| `built-ins/Array` | 786 | **884** |
| `built-ins/String` | 319 | **346** |
| `built-ins/Symbol` | 9 | **17** |
| `language/expressions/addition` | 29 | 30 |
| `language/expressions/new.target`（新启用） | 跳过 | 8 / 9 |
| `language/computed-property-names`（新启用） | 跳过 | 29 / 46 |
| `language/expressions/exponentiation`（新启用） | 跳过 | 27 / 30 |
| `language/expressions/coalesce`（新启用） | 跳过 | 16 / 18 |

M2' 验收：三项从跳过表移除（`exponentiation`、`nullish-coalescing` 标签删除，套件启用），`computed-property-names` 与 `exponentiation` 目标套件通过率 ≥ 50%（56% / 90%）。

**全量复测（2026-09-21，本次工作后）**：

| 指标 | M2 结束 | 本次工作后 |
|------|---------|-----------|
| test262 已执行 | 10240 | 10365 |
| 通过 | 3908 | **4611**（+703） |
| 失败 | 6332 | 5754 |
| 通过率（已执行） | 38.16% | **44.49%** |
| 单元测试 | 190 | 190（全绿） |
| feature 集成测试 | 17 个文件 | 22 个文件（308 条断言，5 条既有 `return_in_try_finally` 失败） |

### 2.1c M3-B1 二批（2026-09-21）

> 注：本轮重建了 test262 子模块（钉在 `7e115f46`），分母由 10365 → 10366，个别套件计数与上表 ±1~6。

交付内容（提交前逐套件实测，`git stash` 双向对比确认零回退）：

| 项 | 内容 |
|----|------|
| ToObject / 装箱包装 | 新增 `PrimitiveWrapperObject`（vm/object.rs）：`Object(value)` 按规范装箱原始值，包装对象原型经 thread-local 注册表取 `Number/String/Boolean/Symbol.prototype`；`Value::to_number` / `to_js_string`、`valueOf`/`toString` 派发、`Symbol.prototype.description` 均对包装对象解包 |
| `[[DefineOwnProperty]]` 校验 | `OrdinaryObject::define_property` 实现 ES 6.1.7.3 校验半部（`same_value` + `validate_property_redefinition`）：非可配置属性允许同值重定义与 `writable true→false`，拒绝值变更/属性种类切换；`freeze`/`seal` 现在同步修改描述符 |
| 数组元素访问器 | `ArrayObject`：索引键的非默认描述符/访问器存入属性表（`holes` 集合 + 密集槽中性化），`length` 可写性可被 `defineProperty` 冻结；`shift/unshift/splice/reverse/sort` 丢弃索引键覆盖项；`delete` 改为洞语义 |
| `Object.assign` 下沉 VM | `VM::object_assign`：真 `[[Get]]`/`[[Set]]`（source 的 own 访问器会执行、target 的 setter/只读属性生效），target/source 走 ToObject；`CallMethod` 与 `call_native_by_name` 两条派发路径均接入 |
| 描述符长尾 | `defineProperties`/`create` 的 properties 只取 own **enumerable** 键；`getOwnPropertyDescriptor`/`getOwnPropertyNames` 支持 ToObject（字符串原始值给出索引键与 `length`） |

定向套件通过数（同一命令、同一机器）：

| 套件 | 一批后 | 二批后 |
|------|--------|--------|
| `built-ins/Object` | 1465 | **1838**（+373） |
| `built-ins/Array` | 884 | **926**（+42） |
| `built-ins/String` | 346 | 348（+2） |
| `built-ins/Symbol` | 17 | 18（子模块更新后与基线持平） |

**全量复测（2026-09-21，二批后）**：

| 指标 | 一批后 | 二批后 |
|------|--------|--------|
| test262 已执行 | 10365 | 10366 |
| 通过 | 4611 | **5035**（+424） |
| 失败 | 5754 | 5331 |
| 通过率（已执行） | 44.49% | **48.57%** |
| 单元测试 | 190 | 190（全绿） |
| feature 集成测试 | 308 条断言 | `object_builtins.rs` 新增 7 个用例 / 18 条断言（仍 5 条既有 `return_in_try_finally` 失败） |

**本轮发现的既有缺陷（非本批引入，已登记 §2.3）**：间接调用（闭包先存变量/传参再调用）抛出的原生 `RuntimeError` 不会被外层 JS `try/catch` 捕获，且异常路径疑似伴随数据栈泄漏（`Symbol/keyFor/arg-non-symbol.js` 全文件执行出现 `RangeError: stack overflow`；`git stash` 在基线复现同样失败）。→ **已在三批修复（§2.1d）**。

### 2.1d M3-B1 三批：SEH 跨帧展开（2026-09-21）

**缺陷**：JS 帧内的 `try` 看不到更深层帧抛出的规范错误。最小复现（`git stash` 确认基线同样失败）：

```js
function f(g) { try { g(); return 'nothrow'; } catch (e) { return 'caught'; } }
f(function () { Symbol.keyFor({}); });   // 期望 'caught'，实为错误逃逸到顶层
```

**根因**（`src/vm/mod.rs`）：
1. `SehRecord` 只记录 `rsp`/`rbp`，**不记录调用帧深度**。`handle_throw` 分派到外层 handler 时，中间帧的返回地址仍在 `ctrl_stack` 上，handler 执行完毕后的 `Ret` 弹出的是**抛出函数的**返回地址，控制流回到 `try` 体、再次抛出，直到 SEH 记录耗尽（`Symbol/keyFor/arg-non-symbol.js` 因此把数据栈推到 `RangeError: stack overflow`）。
2. Rust 驱动的调用（`invoke_with_new_target` / `invoke_construct`，用于原生回调、`call`/`apply`、原生里的 `new`）各自跑一个嵌套循环；异常若在**循环之外**的帧里被处理，嵌套循环会继续执行外层代码，随后 `invoke` 又恢复现场并重复执行（如 `Array.prototype.forEach` 回调抛错）。
3. `invoke_*` 在 `outcome?` 之后再恢复现场，异常路径的现场从未回滚。

**修复**：
1. `SehRecord` 增加 `saved_ctrl_depth` / `saved_this_depth` / `saved_construct_depth` / `saved_new_target_depth`（`saved_closure_depth` 原本已记录但从未使用）；`Opcode::Try` 时一并写入。
2. 新增 `VM::unwind_frames_to(...)`：`handle_throw` 分派（catch 或 finally）前按记录截断 `ctrl_stack`、`this_stack`/`function_stack`/`frame_argc`、`construct_stack`、`new_target_stack`、`closure_var_stack`，并逐层恢复 `this_val`/`function_val` —— handler 运行在**自己的帧**里。
3. 新增 `VM::invoke_boundaries`：Rust 驱动调用入栈当前 `ctrl_stack` 深度；`handle_throw` 若发现目标记录的 `saved_ctrl_depth <= boundary`（handler 在该调用之外），改为返回 `RuntimeError::Thrown` 让其向外传播，使原生（如 `forEach`）能中止自身工作；外层 `step` 再转为 JS 异常并分派。
4. `invoke_*` 改为**先恢复调用者上下文再传播**错误。

**效果**（逐套件实测，无一套件下降）：

| 套件 | 二批后 | 三批后 |
|------|--------|--------|
| `built-ins/Object` | 1838 | **2025**（+187） |
| `built-ins/Array` | 926 | **1028**（+102） |
| `built-ins/Function` | 83 | **151**（+68） |
| `built-ins/String` | 348 | 365（+17） |
| `language/statements/class` | 42 | 50（+8） |
| `built-ins/Symbol` | 18 | 22（+4） |
| `language/statements/for-of` | 13 | 17（+4） |
| `language/expressions/addition` | 30 | 33（+3） |
| `built-ins/Error` | 5 | 7（+2） |

**全量复测（三批后）**：已执行 10366，通过 **5489**（+454），失败 4877，通过率 **52.95%**（48.57% →）；单元 190 全绿；feature 320 条（新增 `tests/features/exception_unwinding.rs`：6 个用例 / 11 条断言；仍 5 条既有 `return_in_try_finally` 失败）。

**残留（既有、非本次引入）**：`try/catch/finally` 的代码生成让 finally 块执行两次（异常路径一次、catch 结束后落入内联 finally 再一次），顶层与跨帧同现；跨帧场景本次反而由 `fcff` 收敛为 `fcf`。另立技术债（§2.3）。

### 2.1e M3-B2 首批：Array 回调方法泛型化 + indexOf 分派（2026-09-22）

**起点**：`built-ins/Array` 失败 1756，其中 `prototype` 占 1658；回调类方法（`reduce`/`reduceRight`/`filter`/`map`/`some`/`every`/`indexOf`/`lastIndexOf`/`forEach`）合计约 1000 例，典型失败是 `TypeError: unknown prototype method: filter` —— `try_array_callback_method` 的 `array_like_elements` 只认真实数组与字符串，泛型（array-like）接收者直接落回普通派发。

**交付**：
1. 新增 `VM::array_like_entries`（洞感知）：原始值接收者先 ToObject 装箱；`length` 与各索引键经**原型链 `[[Get]]`** 读取（因此 `Boolean.prototype[0] = true` 之类的继承形态可生效）；`length` 走 ToLength 并限幅；不存在的索引记为洞（`None`）。
2. 回调方法全部改用 `entries`：**跳过洞**（`forEach`/`map`/`filter`/`some`/`every`/`find`/`findIndex`/`reduce`/`reduceRight`），`map` 结果保留长度与洞（`ArrayObject::mark_hole`），`reduce` 无初始值时以首个**存在**元素为累加器；回调第二个参数 `thisArg` 现在会传入（`reduce` 例外，它按规范没有 thisArg）。
3. `indexOf`/`lastIndexOf` 纳入 VM 路径（ToInteger 的 `position`、SameValueZero、跳洞），并按 `receiver_is_array_like` 分派：字符串与非 array-like 接收者让位给 `String.prototype`（子串语义）；`null`/`undefined` 接收者抛 TypeError。
4. `String.prototype.indexOf/lastIndexOf` 补齐 `position` 语义（ToInteger 钳位、空串在 `position` 处命中、`lastIndexOf` 默认 +∞）与“无参即 `undefined`”的处理。

**效果**（逐套件实测；String/Array 均按失败 ID 与基线逐一比对，净增无回退）：

| 套件 | 三批后 | 四批后 |
|------|--------|--------|
| `built-ins/Array` | 1028 | **1548**（+520） |
| `built-ins/String` | 365 | 371（+6） |
| `built-ins/Object` | 2025 | 2025（持平） |

**全量复测**：已执行 10366，通过 **6015**（+110），失败 4351，通过率 **58.03%**；单元 190 全绿；feature 326 条（新增 `tests/features/array_methods.rs`：6 个用例 / 17 条断言；仍 5 条既有 `return_in_try_finally` 失败）。

**本轮发现的既有缺陷**：`new Array(1,2,3)` 的元素顺序被反转（`Opcode::New` 对原生构造器多了一次 `args.reverse()`，而 `collect_call_args` 的约定是 arg0 在 `[rbp-1]`）；`git stash` 确认基线同样输出 `3,2,1`。已登记 §2.3，未在本批改动（会影响 `new Error/Number/Object` 的实参顺序，需单独验证）。

### 2.2 已交付（M0 → M2'）

- **M0**：值/对象/原型链/SEH/寄存器 VM 骨架
- **M1**：双路径迭代协议、`for-of`/`for-in`、模板插值、默认参数、剩余参数、展开（调用/`new`/数组/对象）、数组与对象解构（含赋值目标、默认值、rest、嵌套）
- **M2**：class 静态成员、`prototype.constructor` 反向链接、getter/setter（合并单一描述符）、`extends` + `super()`/`super.m()`、public 实例/静态字段、类构造器必须 `new` 调用
- **M2'**：
  - S1 计算属性名（对象字面量/类成员/访问器/静态，键按源序求值，符号键保留）
  - S2 `new.target`（帧级 `new.target` 栈；`New` 写入构造器，普通调用为 `undefined`，箭头创建时捕获，`super()` 透传）
  - S3 well-known symbols 接入（`Symbol.toPrimitive`→ToPrimitive、`Symbol.toStringTag`→`Object.prototype.toString`、`Symbol.hasInstance`→`instanceof`；`Symbol.species` 等已注册）
  - S4 `**`/`**=`、S5 `??`（真短路）
  - 属性语义：`OrdinaryOwnPropertyKeys` 顺序、`Object.defineProperty` 部分描述符合并、`Object.keys/values/entries` 可枚举过滤、`define_property` 下沉到 `JSObject` trait（数组/函数/原型对象同样保留完整描述符）
  - `super` 修正：`[[HomeObject]]` 在类定义时记录到成员函数，经 `LoadCurrentFunction` 读回；修复多级继承链中 `super.m()`/`super()` 自我递归（原会栈溢出）与箭头内 `super`
  - 新增 `Opcode::LoadNewTarget` / `LoadCurrentFunction` / `CallSuperSpread`；`strict_eq` 视裸函数引用与装箱 `FunctionObject` 为同一引用
- **通用修复**：隐式返回清空 `Rv`（构造函数原会返回原型对象）、访问器以接收者作 `this`、构造函数返回自身 `this`、调用点寄存器保存、规范错误可被 JS `try/catch` 捕获
- **寄存器分配跨块活跃性**（2026-09-20 修复，见 §2.4）：重写存活分析 + 值跨块经内存交接；顺带修复非可配置属性可被 `delete` 删除、以及 phi 参数顺序随哈希种子变化导致的编译不确定

### 2.2b M3-B1（`Object`）第一批

- `Object.prototype.isPrototypeOf` / `propertyIsEnumerable`（原缺失）
- `ToPropertyDescriptor` 移入 VM：描述符字段经 `[[Get]]`（访问器字段以描述符对象为 `this`），`get`/`set` 必须可调用；空描述符合法（全 `false`）
- `Object.defineProperties` / `Object.create` 的 properties 参数类型校验与逐项读取
- `Object.keys`/`values`/`entries` 支持原始值（ToObject：字符串给出索引键）
- `Object.getOwnPropertySymbols` 与符号键的 descriptor/define 路径
- 属性特性修正：数组 `length`（可写/不可枚举/不可配置）、内置函数 `name`/`length`（不可写/不可枚举/可配置）
- `Symbol()` 不可 `new`；`Symbol.prototype.description` 为真 getter（原始值接收者的访问器现在会被调用）

**未完成（下一轮）**：~~`Object.assign`、`Object(values)` 装箱、`defineProperties`/`create` 长尾~~ 已在二批交付（见 §2.1c/§2.2c）；`Object` 剩余失败 1223 的主体是描述符属性长尾（`configurable`/`enumerable` 细粒度校验）与引用 `JSON`/`Date` 的用例（38+），后者归 B6。

### 2.3 已知技术债（计划内需正视，不掩埋）

| 债务 | 影响 | 当前处置 |
|------|------|----------|
| ~~间接调用抛出的原生错误不被 JS `try/catch` 捕获~~ | 闭包存变量/传参后调用，内部原生 `RuntimeError` 逃逸到顶层；伴随数据栈泄漏（`RangeError: stack overflow`） | ✅ **2026-09-21 修复**（§2.1d）：SEH 记录调用帧深度 + 分派前回滚帧 + invoke 边界传播 |
| `try/catch/finally` 的 finally 块被执行两次 | 异常路径跑一次，catch 结束后又落入内联 finally；`return` 与 finally 组合的 5 条 feature 用例仍失败 | 代码生成层缺陷（2026-09-21 登记）：catch 块结束需跳过 finally 块；归 M6 收尾 |
| `new Array(1,2,3)` 元素顺序反转 | `String(new Array(2,4,8,16,32))` 等用例失败 | 既有缺陷（2026-09-22 登记，`stash` 确认基线同样反转）：`Opcode::New` 对原生构造器多了一次 `args.reverse()`；改动会影响 `new Error/Number/Object` 的实参顺序，需单独验证 |
| panic 时 `{:?}` 打印含原型环的对象导致宿主栈溢出 | 测试基建隐患（Debug 递归进 builtin 原型环） | 已知；测试避免直接 Debug 对象值 |
| Rc/RefCell 无环回收 | 循环引用泄漏 | 暂接受 |
| 闭包值快照语义 | 与规范"引用同一绑定"不同（for-let 按轮捕获等） | 架构决定，相关用例允许失败 |
| 迭代器 `return()`/`throw()`（IteratorClose 异常路径） | break/异常提前退出时未 close | 正常结束路径已 close；异常路径待 M4 |
| 无正则引擎 | 1879 个 RegExp 测试 | 明确不在范围 |
| 顶层 `this` 为 `undefined` | 依赖全局对象作 `this` 的 sloppy 用例 | 与"strict only"目标一致，允许失败 |
| Tagged template 未实现（tag 不调用） | `tag\`\`` 用例 | 计划 M2' 补：strings 数组 + tag 调用 |
| 慢：内置方法多经 `invoke` 派发 | 全量 ~数分钟（高负载机器上更久） | 建性能护栏 |

### 2.4 跨块活跃性修复（2026-09-20）

原债务描述为"寄存器分配的跨块活跃性不可靠"，实际是四个独立缺陷叠加，逐个修复：

| # | 缺陷 | 修复 |
|---|------|------|
| 1 | 存活区间**有空洞**：只按定义/使用点打点，`live_out` 仅做部分延伸，跨块"live-through"的索引不被覆盖；`must_alloc` 据此把仍是活值的变量选为牺牲者 | 按块做**后向存活扫描**（以 `live_out` 为种子），把变量在其真正活跃的每个指令索引上都记录进区间；`ranges` 精确、无洞 |
| 2 | `alloc` **吞掉 spill 动作**：需要 `Restore` 时直接 `return`，把 `evict_occupant`/`must_alloc` 的 `Action::Spill` 丢弃——被驱逐的值被标记"已写栈"却没有真正写栈，之后重载得到陈旧值 | `alloc` 返回**按顺序执行的动作列表**（先 `Spill` 后 `Restore`），调用方全部发射 |
| 3 | **编译期寄存器状态跨块不可信**：代码生成按布局顺序线性推进，块入口的 `reg_set` 可能来自"运行时未必执行过"的兄弟块；`find()` 命中假值而跳过重载 | 块边界**只经内存交接**：块入口清空 `reg_set`，终止符处把 `live_out` 中仍在寄存器的值写回栈槽（`begin_block` / `spill_live_out`） |
| 4 | **块参数（phi）交接不一致**：一条边写寄存器、另一条边写栈槽，块入口却从陈旧的栈槽重载（循环计数因此不更新） | `store_jump_args` 统一经**参数专属栈槽**交接：每条边写槽、块入口重载；栈槽一经分配永不变更（`must_alloc` 改为复用已有槽） |

配套决定：**变量默认内存驻留**（每个变量固定栈槽，寄存器仅作指令级暂存）。
单遍、按布局顺序生成的代码无法实现"跨块寄存器状态"的可信推理；改成内存驻留后该类缺陷整体消失。实测更快（调用点无需保存 16 个寄存器），故不再作为 workaround，而是默认行为；
`BIUJS_REG_VARS=1` 可切回"块内寄存器 + 边界落栈"路径（仍需 `begin_block`/`spill_live_out`）；含 `try` 的函数强制内存驻留，
因为异常可在块中途逃逸到 handler，终止符处的落栈覆盖不到。

顺带修复：
- `delete` 忽略 `[[Configurable]]`，不可配置属性可被删除（`OrdinaryObject::property_delete`）；修好后 `built-ins/Object` 由 1026 → 1029。
- phi 参数顺序受 `HashSet` 迭代顺序影响，同一份源码在不同进程生成的字节码不同（`SSABuilder::determine_phi_nodes`）；排序后编译确定化。
- `ResumeException` 未把 `args` 计为使用（`defined_and_used`）。

回归护栏：`tests/features/control_flow.rs` 新增"循环 × 多活跃值/循环携带值/循环内展开/嵌套循环"四条；`regalloc.rs` 新增区间覆盖与 `alloc` 双动作两条单元测试。

---

## 3. 差距分析

### 3.1 失败池（已执行但未通过 —— 最高 ROI）

| 套件 | 失败数 | 主要缺口 |
|------|--------|----------|
| `built-ins/Object` | 2032 | ES6 静态方法（`assign`/`getOwnPropertySymbols`/`is` 细节）、属性描述符语义、原型方法 |
| `built-ins/Array` | 1975 | `from`/`of`/`fill`/`find`/`findIndex`/`copyWithin`/`entries`/`keys`/`values`、泛型调用、类数组路径 |
| `built-ins/String` | 686 | ES6 增补（`repeat`/`startsWith`/`endsWith`/`codePointAt`/`normalize`/`raw`）、迭代器 |
| `built-ins/Function` | 245 | `name`/`length` 语义、`hasInstance`、`toString` |
| `built-ins/Number` | 175 | ES6 常量与 `isInteger`/`isSafeInteger`/`parseFloat` |
| `language/statements/class` | 155 | 计算属性名、`new.target`、字段初始化次序、描述符可枚举性细节 |
| `built-ins/Math` | 134 | ES6 数值方法（`hypot`/`sign`/`clz32`/`imul`/`log2`/`cbrt`…） |
| `built-ins/Error` / `NativeErrors` | 70 / 58 | 子类原型链、`message` 缺省、`stack` 约定 |
| `built-ins/Symbol` | 62 | well-known symbols、`Symbol.for/keyFor`、描述 |
| `language/statements/for-of` | 51 | 用户自定义迭代器、`IteratorClose`、字符串码元 |

### 3.2 跳过池（未执行 —— 决定下一步解锁顺序）

| 特性 | 规模 | 归属 |
|------|------|------|
| `generators` | 4061 | M4（ES6 核心，最大单项） |
| `Symbol.iterator` / `Symbol` | 1829 / 1452 | M2 收尾 + M4 |
| `class` | 4734 | 已在执行；剩余失败见 3.1 |
| `computed-property-names` | 478 | M2 收尾 |
| `Reflect.construct` / `Proxy` / `Reflect` | 692 / 457 / 436 | M5 |
| `object-rest` | 355 | 已实现（ES2018），可执行 |
| `new.target` | 61 | M2 收尾 |
| `exponentiation` (`**`) | 102 | M2 收尾（ES2016，成本低） |
| `u180e` / `String.prototype.replaceAll` | 25 / 31 | M3 顺带 |

目录级规模（内置对象，供排期）：Object 3411、Array 3081、TypedArray 1446、String 1223、TypedArrayConstructors 738、Promise 677、Date 594、DataView 561、Function 509、Set 383、Number 340、Math 327、Proxy 311、ArrayBuffer 221、Map 204、JSON 165、Reflect 153、WeakMap 141、Symbol 98、NativeErrors 94、Error 93。

### 3.3 优先级结论

1. **M3 内置对象**：失败池主体（Object+Array+String ≈ 4700 失败），且多为"缺方法 / 语义细节"，单点成本低、收益确定 ——**优先**。
2. **M4 生成器**：ES6 核心语义，解锁 4061 个测试，但需引入挂起/恢复机制 ——**成本最高，排第二**。
3. **M5 集合与反射**：Map/Set/WeakMap/Promise/Proxy/Reflect，规模中等、相互独立，可增量交付。
4. **M6 TypedArray + 收尾**：规模大但同质，可批量；同时做一致性收尾与文档。

---

## 4. 里程碑计划

### M2'：语法收尾（小、快、解锁面广）—— ✅ 已完成

| 任务 | 内容 | 验收 | 状态 |
|------|------|------|------|
| S1 计算属性名 | 类/对象字面量的 `{[expr]: v}`、`class { [expr](){} }`（走 `Object.defineProperty`） | `computed-property-names` 解锁且不回退 | ✅ 29/46 通过 |
| S2 `new.target` | 新增 IR/操作码，由 `New` 写入当前帧；普通调用为 `undefined` | 61 个 `new.target` 用例 | ✅（帧栈 + 箭头捕获 + `super()` 透传） |
| S3 well-known symbols 补齐 | `Symbol.toStringTag` / `Symbol.toPrimitive` / `Symbol.hasInstance` / `Symbol.species` / `Symbol.isConcatSpreadable`，并接入 `Object.prototype.toString`、`ToPrimitive`、`instanceof` | Object/String/Array 相关失败下降 | ✅ 前三者已接入；`species` 仅注册（物种构造路径未接，记录偏差） |
| S4 `**` 与 `**=` | 新增二元指令或复用 `Math.pow` 语义 | 102 个 `exponentiation` 用例 | ✅ 27/30 通过 |
| S5 顺带项 | `u180e`、`String.prototype.replaceAll`、`??` 与逻辑赋值的正确性复核 | 无回退 | ⚠️ `??` 已修正；`u180e`/`replaceAll` 仍在跳过表 |

**完成标准**：全量通过数不低于 3901；`computed-property-names`/`new.target`/`exponentiation` 三项从跳过表移除后目标套件通过率 ≥ 50%。→ 达标（56% / 89% / 90%）。

**M2' 额外交付（计划外但必要）**：`super` 的 `[[HomeObject]]` 记录机制，修复多级继承链中 `super.m()`/`super()` 的无限递归（原为栈溢出）与箭头内 `super`；`define_property` 下沉到 `JSObject` trait，数组获得非索引属性表。

### M3：内置对象补齐（主战场）

| 任务 | 内容 | 目标 |
|------|------|------|
| B1 `Object` | `assign`、`getOwnPropertySymbols`、`is`/`isExtensible` 族、描述符语义（`writable/enumerable/configurable` 与 `[[DefineOwnProperty]]` 完整规则） | Object 失败 2035 → 目标减半；**首批已交付**：失败 2032 → 1596（通过 1029 → 1465）；**二批交付（§2.1c）**：失败 → 1223（通过 → 1838，达标 50% 通过率） |
| B2 `Array` | `from`/`of`/`fill`/`find`/`findIndex`/`copyWithin`/`entries`/`keys`/`values`/`reduceRight`/`sort` 语义、泛型（类数组）路径、稀疏与长度处理 | Array 失败 1975 → 目标减半；**首批已交付（§2.1e）**：回调方法泛型化/洞感知、`indexOf`/`lastIndexOf` 分派 —— 失败 1756 → 1236（通过 1028 → 1548） |
| B3 `String` | `repeat`/`startsWith`/`endsWith`/`codePointAt`/`codePointAt` 代理对/`normalize`/`at`、`String.raw`、`String` 迭代器与 `[Symbol.iterator]` | String 失败 686 → 目标减半 |
| B4 `Number` / `Math` | ES6 常量（`EPSILON`/`MAX_SAFE_INTEGER`…）与 `isInteger`/`isSafeInteger`/`parseFloat`；Math 的 `hypot`/`sign`/`clz32`/`imul`/`log2`/`log10`/`cbrt`/`trunc`/`fround` | Math/Number 失败显著下降 |
| B5 `Function` / `Error` / `NativeErrors` | `name`/`length` 推导、`Error` 子类原型链与 `message` 缺省、`NativeErrors` 各类型 | 三项目标套件 ≥ 60% |
| B6 `JSON` / `Date`（新增模块） | `parse`/`stringify`（无 reviver 的完整语义优先）、`Date` 构造与常用取值方法 | 解锁 165 / 594 个用例（Date 允许部分失败） |

**完成标准**：每个 B 任务独立提交；提交前全量不回退；`tests/features/` 每任务 ≥ 5 条断言；Object/Array/String 三套件通过率各 ≥ 50%。

### M4：生成器与迭代协议完成

| 任务 | 内容 |
|------|------|
| G1 生成器运行时 | 调用帧挂起/恢复（`Yield`/`Resume`）、`yield` 表达式值双向传递、`return()` 提前终止 |
| G2 `function*` / `yield` / `yield*` 降级 | 函数体编译为可恢复的帧；委托迭代复用 `Symbol.iterator` |
| G3 IteratorClose 完整化 | `return()` 调用与异常吞除；break/异常/正常结束三条路径 |
| G4 与 `for-of`、解构、展开的联调 | 复用既有双路径；生成器对象作为慢路径迭代器 |

**风险**：挂起/恢复需要改动帧布局与 `invoke`，是本项目迄今最大的运行时改动；建议先做"仅 `next()`、无 `try` 内 yield"的子集，再补齐异常路径。
**完成标准**：`generators` 解锁后通过率 ≥ 40%（子集阶段可放宽至 25%），且不回退 M1 的 for-of/解构/展开。

### M5：集合与反射

| 任务 | 内容 | 规模 |
|------|------|------|
| C1 `Map` / `Set` | 语义完整的插入/查找/删除/迭代（`size`、`forEach`、插入序） | 204 / 383 |
| C2 `WeakMap` / `WeakSet` | 弱引用语义（Rc 场景下退化为强引用需记录偏差） | 141 / 34 |
| C3 `Promise` | 状态机、`then` 链、微任务队列（需在 VM 主循环后 draining） | 677 |
| C4 `Proxy` / `Reflect` | 陷阱分发与 `Reflect` 静态方法（依赖属性描述符与 `[[Get]]/[[Set]]` 抽象操作成型） | 311 / 153 |

**依赖**：C4 依赖 M3-B1 的 `[[DefineOwnProperty]]` 完整化；C3 需要一个明确的微任务调度点。
**完成标准**：各任务独立提交；解锁后目标套件通过率 ≥ 30%（Proxy/Reflect 因依赖内部方法，允许更低）。

### M6：TypedArray 与一致性收尾

| 任务 | 内容 |
|------|------|
| T1 `ArrayBuffer` / `DataView` / `TypedArray` | 共享底层 buffer、视图偏移、越界与 detach 语义 |
| T2 一致性收尾 | 按 §2.1 KPI 复测；整理 runner 跳过表，只保留真实缺失或范围外的特性 |
| T3 文档 | 同步 `es6-feature-support.md`、README 特性列表、本计划的状态 |

**完成标准**：README 的"strict mode ES6"声明与 `es6-feature-support.md` 逐项一致；全量无回退。

---

## 5. 与静态编译模型的对齐

| 架构前提 | 结论 |
|----------|------|
| 无 `eval` / `new Function` / `with` | **永久排除**（G3）；静态绑定是寄存器分配与帧布局的前提 |
| `var` / `arguments` | 已实现并可用（订正旧文档）；`arguments` 由每帧 `frame_argc` 物化 |
| 闭包捕获 | 保持**创建时值快照**（`dyn-capture.md`）；for-let 按轮捕获等规范细节允许失败 |
| 动态派发 | 内置方法经 `invoke` 统一入口；新增内置只需注册，无需同步白名单 |
| 跨块活跃性 | **已修复**（§2.4）：变量默认内存驻留，跨块值经栈槽交接，"生成顺序 ≠ 运行顺序"不再影响正确性。新增"循环 + 多块"降级不再需要回避；但仍建议优先原生指令——块越少，边界交接越少 |

---

## 6. 验证与回归策略

1. **三级验证**：`tests/features/` 语义断言 → 目标套件定向跑（`TEST262_SUITES=...`）→ 提交前全量回归（通过绝对数不低于上一提交）。
2. **跳过表治理**：特性实现后即从 `UNSUPPORTED_FEATURES` 移除；范围外特性在表内注明理由（如 `Temporal`、`async-functions`）。
3. **失败原因聚合**：每里程碑开始/结束各跑一次
   `TEST262_SUITES=... TEST262_FAILURES=300 ... | grep FAIL | sed ... | sort | uniq -c | sort -rn`，用于排下一轮优先级。
4. **性能护栏**：记录全量耗时基线；劣化 > 2× 需定位（热点：慢路径迭代的 `invoke`、内置方法派发）。
5. **单元测试**：190 个保持全绿；新增运行时机制（生成器帧、微任务）需补单元测试。

---

## 7. 风险登记

| 风险 | 影响 | 对策 |
|------|------|------|
| 生成器改动触及帧布局 | 可能回退调用/`this`/`arguments` | 子集先行（无 try 内 yield）；保留旧帧路径做 A/B |
| 内置方法"看似完成"但描述符语义不对 | 大量 test262 断言失败 | 先做 `[[Get]]/[[Set]]/[[DefineOwnProperty]]` 抽象操作，再堆方法 |
| 解锁大特性后通过率下滑 | 指标误读 | 以通过绝对数为主指标，报告同时给出分母 |
| Proxy 需要完整内部方法 | 成本高、收益不确定 | 排 M5 末位，允许低通过率 |
| JSON/Date 涉及时区与浮点格式 | 长尾失败 | 先做无 reviver/UTC 子集，明确记录偏差 |
| 跨块活跃性缺陷在新多块降级中复发 | 隐蔽错误值 | 已修复（§2.4）；护栏：`features` 四条多活跃值循环断言 + `regalloc` 两条单元测试 |

---

## 8. 文档维护承诺

- 本计划每完成一个里程碑更新状态与基线数字；
- `docs/es6-feature-support.md` 随里程碑同步：范围一节订正（`var`/`arguments` 已支持）、ES6 class 一节补齐静态/访问器/extends/super/字段、"Out of scope"中已实现项移除；
- README 的 Supported Features 在 M3 完成后按实际能力重写。
