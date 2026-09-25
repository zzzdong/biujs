# ES6 一致性计划（M2 收尾 → M6）

> **日期**：2026-09-20（初版）；2026-09-21 更新 —— M2' 完成、M3-B1 首批交付（见 §2.1b/§2.2b）；2026-09-21 二批 —— M3-B1 收尾（ToObject 装箱、描述符校验、数组元素访问器、Object.assign 下沉 VM），见 §2.1c/§2.2c；2026-09-21 三批 —— SEH 跨帧展开修复（见 §2.1d）；2026-09-22 四批 —— M3-B2 首批（Array 回调方法泛型化、indexOf/lastIndexOf 分派、String.prototype.indexOf 补齐），见 §2.1e
> **目标来源**：`README.md:3-4` —— *"A JavaScript engine implemented in Rust. Targeted to support **strict mode ES6** features but without `eval` or eval-like features or `with` statement."*
> **基线**：test262 **7677 / 10552**（72.75% 通过，2875 失败，14415 跳过；M3-B6 后）——全部数字都在**冻结修订 `7e115f4`（2026-05-21）**上测得，见 §6.7 与 `tests/test262.pin`；单元测试 190；feature 集成 28 个文件（359 / 364 条通过，余 5 条既有 try-finally 失败）；runner 启用 90 个套件
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
| class 私有字段/方法（`#x`）、static block | **不在** | 非 ES6（ES2022）；跳过表按 `class-methods-private` / `class-fields-private` / `class-static-methods-private` / `class-static-fields-private` 四条门控（§2.1y） |
| class **公开**字段（`x = 1` / `static x = 1`） | **不在** | 非 ES6（ES2022）；**但不进跳过表** —— 失败的公开字段用例 833 / 838 与范围内特性（`computed-property-names`、`generators`、`destructuring-binding`）共存，跳掉会埋掉范围内覆盖（§2.1y）。它因此留在失败池里，是池中已知的「非 ES6」成分 |
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

### 2.1a 当前基线（2026-09-25，§2.1y 之后）

| 指标 | 数值 |
|------|------|
| test262 已执行 | 17477 |
| 通过 | **13211** |
| 失败 | 4266 |
| 跳过 | 8109（全部为 §1.2 范围外或 §1.1 G3 排除项，见 §3.2） |
| 通过率 | 75.59%（参考值，分母见 §2.1y） |
| 单元测试 | 190（全绿） |
| feature 集成测试 | 23 个文件 / 417 个用例（全绿） |
| 全量耗时基线 | 2m05s |

> **分母变更提醒（§2.1y）**：2026-09-25 之前，通过率的分母是 10825 —— 跳过表里混着两条与特性无关的规则（`Flag::Generated`、`$DONOTEVALUATE`），把 6652 个可执行测试排除在外。修正后分母是 17477，因此**通过率旧值（73.73%）与新值（69.90%）不可直接比较**；同一批 10825 个测试上的成绩在两个版本里都是 7981 / 10825 = 73.73%，没有变化。

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

### 2.1f M3-B1 收尾：函数原型反向链接、函数值装箱、`length`/`name` 元数据（2026-09-22）

**起点**：全量 `6021 / 10365`（58.09%，失败 4344）。对 4344 条失败做原因聚合后，榜首是两类看不出关联、但同为"一处修复、多套件受益"的根因。

**根因**：

1. 函数对象只创建了 `F.prototype`，**没有** `F.prototype.constructor` —— `new_function_object` 里原本有一行注释写着"为避免 Rc 循环刻意省略"。而 harness 的 `sta.js` 里 `Test262Error` 完全依赖函数声明建立原型关系，于是 `thrown.constructor` 为 `undefined`，`assert.throws`（`harness/assert.js:131` 读 `thrown.constructor.name`）自己抛 `Cannot read properties of undefined (reading 'name')`，把测试的真实失败原因掩盖成引擎无关错误。
2. `Value::Function(id)` 是函数值的**未装箱**内部形式，而 builtin 层统一按 `Value::Object` 模式匹配，`ToObject` 对裸函数引用又是原样透传 —— `Object.keys(fn)`、`Object.getOwnPropertyDescriptor(fn, 'length')` 一类调用报 `first argument must be an object`。
3. 函数元数据全线失真：`NativeFunctionObject` 的 `length` 在 `property_get` 里写死为 0，`name` 直接取派发用名（`__proto_method__push`、`Object.keys`）且不可删除、不可重定义；用户函数的 `length` 用 `params.len()`，未按 ES 9.2.4 的 ExpectedArgumentCount 排除带默认值的形参；箭头函数完全没有 `length`/`name`（`new_arrow_function_object` 只设了 `<arrow>` 占位名）。

**交付**：

1. `new_function_object` 创建 `F.prototype` 时写入 `constructor`，反向引用存**裸 `Value::Function(func_id)`**：属性访问会按需装箱、`strict_eq` 与装箱形式相等，因此不产生 `Rc` 循环。同时把 `F.prototype` 三个 own property 按规范定义为 `length → name → prototype`（前两者 `{writable:false, enumerable:false, configurable:true}`，`prototype` 为 `{writable:true, enumerable:false, configurable:false}`）。
2. 内置派发的两个入口（`call_native_by_name` 与 `CallMethod` 收集实参之后）先把实参与接收者经 `as_object_value` 装箱。
3. 新增 `builtins::builtin_arity` 表，键为**注册名**（`"Array"`、`"Object.keys"`、`"__proto_method__push"`、`"Math.atan"`），值取自 test262 各 `**/length.js`；`NativeFunctionObject` 构造时即把 `length`/`name` 写成真实 own property（`name` 去掉派发前缀）。`FuncSignature` 增加 `arity` 字段，lowering 按"遇到首个 initializer 即停止"计算（`f(a, b = 1, c)` 为 1，rest 参数在 `FormalParameters::rest` 中本就不计入）；箭头函数改由 `new_arrow_function_object` 安装 `length`/`name`（`name` 为空串），且不安装 `prototype`。`Number.prototype.toString` 是唯一"同名不同 arity"的项（1，其余 `toString` 为 0），用新增的 `set_prototype_method_arity` 显式注册。

**效果**（逐套件实测，无一套件下降；三步合计 +338）：

| 套件 | 起点 | 本批后 | 变化 |
|------|------|--------|------|
| `built-ins/Object` | 2031 | **2087** | +56 |
| `built-ins/Array` | 1548 | **1633** | +85 |
| `built-ins/String` | 371 | **424** | +53 |
| `built-ins/Math` | 155 | **192** | +37 |
| `built-ins/Number` | 110 | **128** | +18 |
| `built-ins/Function` | 151 | **166** | +15 |
| `built-ins/NativeErrors` | 24 | **36** | +12 |
| `built-ins/Symbol` | 22 | **30** | +8 |
| `language/statements/class` | 50 | **58** | +8 |
| `language/statements/function` | 97 | **106** | +9 |
| `language/statements/for-of` | 17 | **21** | +4 |
| `language/expressions/object` | 41 | **45** | +4 |

**全量复测**：已执行 10365，通过 **6021 → 6359**（+338），失败 4344 → 4006，通过率 58.09% → **61.35%**；单元 190 全绿。

**残留**：`name` 推导（`var f = function () {}` 应为 `"f"`）未做；class 方法仍带 `prototype`（按规范应无）；`Function.prototype.isPrototypeOf(Array)` 类断言要等下一批的原型链接线。

### 2.1g 内置属性特性、原生构造器原型链、真正的 ToNumber（2026-09-22）

**根因**：

1. 内置属性几乎全部经 `property_set` 落入 `{writable:true, enumerable:true, configurable:true}`：`Math.atan` 可枚举、`Math.PI` 可写、`C.prototype` 可写 —— `Object/getOwnPropertyDescriptor/15.2.3.3-4-*`（96 例 `desc.enumerable expected false but got true`）整片失败。
2. 错误对象的 `name` 被写在**实例**上，`Error.prototype.name` 根本不存在（`name should be an own property` 15 例）。
3. 内置函数对象的 `[[Prototype]]` 被设成"对应的 `.prototype` 对象"（`NativeFunctionObject::with_prototype` 的用法，同时被 `New` 当作实例原型读取），于是 `Array instanceof Function` 为 false、`Object.getPrototypeOf(Array)` 不是 `Function.prototype`、经 `Function.prototype` 挂的自定义属性取不到；`new Array()` / `Array()` 的结果也没有 `Array.prototype`（`Array.prototype.isPrototypeOf(new Array())` 为 false、`(new Array()).constructor === Array` 为 false）。
4. `+expr` 被降级成 `0 + expr`（`Addx`），字符串操作数会走拼接：`+"5" === "05"`、`+"0x10" === "00x10"`；`x++` 直接对旧值做 `Addx` 且 postfix 返回旧值原样，`var s = "5"; s++` 得到 `"51|51"`；`new Number(1.1)` 在 `x++` 后返回包装对象而非数值。

**交付**：

1. 新增 `method_descriptor` / `constant_descriptor` / `constructor_prototype_descriptor`，替换 `mod.rs`（`set_prototype_method`、`set_static_method`、`link_constructor_prototype`、`Array/String.prototype[Symbol.iterator]`）、`math.rs`、`number.rs`、`symbol.rs`、`object.rs`、`function.rs` 的注册点：方法 `{w:t,e:f,c:t}`、内置常量 `{w:f,e:f,c:f}`、`C.prototype` `{w:f,e:f,c:f}`。
2. 错误的 `name` 迁到原型（`Error.prototype.name`，并给每个 NativeError 原型写 own `name`），实例只在显式传入非 `undefined` 消息时写 own `message`；随之 `RuntimeError` 的 `Display` 与 runner 的 `thrown_error_name` 改为沿原型链查找 `name`（否则失败原因退化、负向用例类型匹配失效）。带 `new` 调用原生错误构造器时，改为给"结果对象"挂上构造器的 `prototype`，不再把 name/message 合并进新对象。
3. `NativeFunctionObject::new` 统一把 `[[Prototype]]` 设为 `Function.prototype`（`Builtins::new` 建好 Function.prototype 后立即注册到 wrapper 表），删除易误用的 `with_prototype`；`New` 的实例原型改读 `C.prototype`，结果经新增的 `finish_native_construct` 归一（`Object`/`Error`/`Array` 等已带原型的对象不重写，因此 `new Object(x)` 返回 `x` 不会换掉 `x` 的原型；原始值装箱成 `new String("x")`/`new Number(1)`/`new Boolean(true)`）；`ArrayObject` 三个构造器默认继承 `Array.prototype`；修掉 `New` 路径上多叠加的一次 `args.reverse()`（`new Array(2,4)` 元素顺序原为反的）。
4. 新增 `Opcode::ToNumber`（语义与已有的 `ToString` 对称：先 `ToPrimitive("number")` 再取数值），一元加号与自增/自减改走它（`ToNumeric(旧值) ± 1`，postfix 返回转换后的旧值）；`Value::to_number` 的字符串分支补 `StringNumericLiteral` 的 `0x`/`0o`/`0b` 前缀与 `Infinity`。

**效果**（逐套件实测，无一套件下降；两步合计 +619）：

| 套件 | 起点 | 属性特性后 | 原型链+ToNumber 后 | 变化 |
|------|------|-----------|-------------------|------|
| `built-ins/Object` | 2087 | 2234 | **2340** | +253 |
| `built-ins/Array` | 1633 | 1661 | **1763** | +130 |
| `built-ins/String` | 424 | 434 | **469** | +45 |
| `built-ins/Math` | 192 | **235** | 235 | +43 |
| `built-ins/Number` | 128 | 144 | **174** | +46 |
| `built-ins/NativeErrors` | 36 | **49** | 49 | +13 |
| `built-ins/Function` | 166 | 167 | **174** | +8 |
| `built-ins/Error` | 9 | **18** | 19 | +10 |
| `built-ins/Symbol` | 30 | **35** | 35 | +5 |
| `language/statements/class` | 58 | **64** | 64 | +6 |
| `language/types` | 70 | 73 | **79** | +9 |
| `built-ins/Boolean` | 21 | 22 | **25** | +4 |

**全量复测**：通过 **6359 → 6978**（+619），失败 4006 → 3387，通过率 61.35% → **67.32%**。

**本轮引入并当场修掉的回归（记录）**：`a6138fa` 把错误 `name` 搬去原型后，① 不带 `new` 调用 `Error(...)` 时没有任何原型赋值（`VM` 只在 `New` 路径处理原生构造器），`e.name` 变 `undefined`；② `New` 路径原本靠"把结果的 name/message 合并进新对象"补名字，正是 own `name` 的来源。`9ab858a` 把各错误原型注册进 wrapper 表让 `error_constructor` 自给原型，并改为给结果对象挂构造器的 `prototype`。该次修复同时让 unit 从 1 红、features 从 6 红回到全绿/既有 5 红。

**残留**：`ToString(Symbol)` 在本引擎返回 `Symbol(x)` 而不抛 TypeError（`to_js_string` 无 `Result` 通道）；`new (f.bind(…))()` 仍不支持；class 方法仍带 `prototype`。

### 2.1h M3-B3 两批 + `Number::toString` + 内置方法错误传播（2026-09-22）

**根因**：

1. 内置函数的 `name` 取了派发用名，`Object.keys.name === "Object.keys"`、`Math.atan.name === "Math.atan"` —— 所有 `**/name.js` 失败（Object +21、Math +35）。
2. `String.fromCharCode` 未做 `ToUint16`：Rust 的 `-1.0 as u32` 饱和到 0，`String.fromCharCode(-1).charCodeAt(0)` 应为 65535；`String.fromCodePoint` 完全缺失。
3. `Number::toString` 用的是 Rust 默认浮点格式化再去尾零，不遵守 ES 7.1.12.1 的定长/指数记数法切换：`String(1e21)` 输出 22 位数字、`String(1e-7)` 输出 `"0.0000001"`。
4. **内置方法的错误被静默吞掉**：`CallMethod` 的快速派发写成 `if let Ok(result) = call_prototype_method(..)` / `call_static_method(..)`，内置方法抛出的真实错误与"没有这个方法"共用 `Err` 通道，于是前者被丢弃、代码继续落到原型链兜底并最终返回 `undefined` —— `"a".normalize("bad")` 不抛 RangeError、`" x ".trimStart()` 报 `unknown prototype method`、`Object.create` 的 getter 副作用丢失。
5. String 原型方法注册与派发两张表长期不同步：`trimStart`/`trimEnd`/`padStart`/`padEnd` 有注册无分支；`codePointAt`/`at`/`normalize`/`toLocale*`/`localeCompare` 未实现；`trim*` 用的是 Rust `char::is_whitespace`（U+0085 属空白、U+FEFF 不属，与 ES 相反）。

**交付**：

1. `install_metadata` 的 `name` 取派发名末段（`self.name` 保持完整以继续用于派发），并特判 `__proto_method__` 前缀与迭代器工厂（`"[Symbol.iterator]"`）。
2. 新增 `String.fromCodePoint`（含范围检查 → RangeError、`length = 1`）；新增共享的 `builtins::to_uint16` 并用于 `fromCharCode`。
3. 按 ES 7.1.12.1 重写 `Number::toString`：取 Rust `{:e}` 的最短往返有效数字 `(s, k)`，按 `k ≤ n ≤ 21` / `0 < n ≤ 21` / `-6 < n ≤ 0` / 其余指数记数法四分支输出，指数恒带符号；`value.rs` 的 `format_number` 委托它，两处实现合一（`String(n)`、模板字面量、`Array.join`、`n.toString()` 同时受益）。
4. 新增 `builtins::is_unknown_builtin` 区分"无此内置"与真实错误，`CallMethod` 的两处快速派发改为 `Err(unknown) => 继续兜底` / `Err(other) => return Err`；并把 `Object.defineProperty`/`Object.defineProperties`/`Object.create` 从快速路径排除（它们必须由 `call_native_by_name` 的 VM 版本读 `[[Get]]`，否则会抢先报错且丢失访问器副作用）。
5. 补 `trimStart`/`trimEnd`/`padStart`/`padEnd` 派发分支；实现并注册 `codePointAt`（码元索引，代理对返回完整码点、第二码元返回尾代理单元值）、`at`、`normalize`（只做 form 校验，未内嵌 Unicode 表）、`toLocaleLowerCase`/`toLocaleUpperCase`（无 locale 数据时退化为非 locale 版本）、`localeCompare`（码元序）；`trim*` 改用 `string::is_js_whitespace`（ES WhiteSpace + LineTerminator 集合）。
6. 同步修正 `at` 的接收者判定（字符串原始值/包装对象走 String，其余带 `length` 的对象走 Array），并让 `NativeIteratorObject` 暴露 `Symbol.iterator`、`make_iterator` 对原生迭代器直接复用自身（否则 `for (x of arr.keys())` 会在 `[Symbol.iterator]` 上无限递归）。

**效果**（两步；`Number::toString` 与 B3 首批 +108，错误传播 + B3 二批 +332）：

| 套件 | 起点 | B3 首批 + Number::toString 后 | B3 二批 + 错误传播后 | 变化 |
|------|------|------------------------------|---------------------|------|
| `built-ins/Object` | 2340 | 2377 | **2532** | +192 |
| `built-ins/String` | 469 | 497 | **561** | +92 |
| `built-ins/Math` | 235 | **270** | 270 | +35 |
| `built-ins/Number` | 174 | 178 | **180** | +6 |
| `built-ins/Symbol` | 35 | **37** | 37 | +2 |

**全量复测**：通过 **6978 → 7310**（+332），失败 3387 → 3055，通过率 67.32% → **70.53%**。

**本轮新发现的既有缺陷（已修）**：`CallMethod` 吞错误（上述第 4 点）是本轮影响面最大的修复；它的修复又暴露出"快速路径抢在 VM 版 `Object.create` 之前"的次生问题，已一并处理。

**残留**：`normalize` 未做真实 Unicode 规范化；`at` 落在代理对中间时用 U+FFFD 代替孤立代理；`String.prototype.X.call(obj)` 的接收者 ToString 仍走不到用户 `toString`（需要 VM 参与）；`replace`/`match`/`search`/`split` 的 RegExp 形态属范围外。

### 2.1i M3-B5：`Function` / `Error` / `NativeErrors`（2026-09-22）

**根因**：

1. 错误实例是普通 `OrdinaryObject`，`[[Class]]` 为 `"Object"`：`Object.prototype.toString.call(new TypeError())` 得 `"[object Object]"`，应为 `"[object Error]"`；而 `new Error("x").toString()` 又走到按名派发的 `"[object Error]"`（`dispatch_to_string` 按对象种类出类名），应走 `Error.prototype.toString` 得 `"Error: x"`。
2. `Error.prototype.toString` 只看 own 的 `name`/`message`（沿原型链的 `TypeError` 实例取不到 `name`），且非对象 `this` 不抛 TypeError；NativeError 原型缺 own `message`（ES 19.5.6.3.2）。
3. 内置构造器的 `C.prototype` 可写（ES 17 要求只读且不可配置），`Error.isError`、`new Error(msg, {cause})` 缺失。
4. `VM::construct`（`NewSpread` 使用）复制了一份 `New` 操作码的原生构造逻辑，仍在做"把结果的 name/message 合并进新对象"的旧 hack，且不认 `__bound__` 名字 —— `new (f.bind(…))(…)` 报 `unknown built-in: __bound__0`。

**交付**：

1. `OrdinaryObject` 增加 `class_name` 字段（默认 `"Object"`，新增 `with_class_name`），错误实例统一为 `"Error"`；`dispatch_to_string` 遇到该 `[[Class]]` 的对象改走 `error_prototype_to_string`。
2. 按 ES 20.5.3.4 重写 `Error.prototype.toString`（非对象 `this` 抛 TypeError；`name`/`message` 沿原型链 `[[Get]]`；`undefined` 分别回退 `"Error"`/`""`；空串短路）；每个 NativeError 原型写 own `name` 与 own `message = ""`。
3. `constructor_prototype_descriptor` 改为 `{writable:false, enumerable:false, configurable:false}`；新增 `Error.isError`（以 `[[Class]]` 作为 `[[ErrorData]]` 的标记）；`new Error(msg, {cause})` 安装 own `cause`（非枚举）。
4. `New` 操作码与 `VM::construct` 共用新增的 `VM::native_construct`：实例原型取 `C.prototype`、结果经 `finish_native_construct` 归一、bound 函数构造其目标并把绑定实参前置。

**效果**（逐套件实测，无一套件下降）：

| 套件 | 起点 | 本批后 | 变化 |
|------|------|--------|------|
| `built-ins/Object` | 2532 | **2548** | +16 |
| `built-ins/Function` | 174 | **189** | +15 |
| `built-ins/Error` | 19 | **34** | +15 |
| `built-ins/NativeErrors` | 49 | **62** | +13 |
| `built-ins/Boolean` | 25 | **26** | +1 |
| `built-ins/String` | 561 | **562** | +1 |
| `language/statements/try` | 33 | **34** | +1 |

**全量复测**：通过 **7310 → 7372**（+62），失败 3055 → 2993，通过率 70.53% → **71.12%**；新增 `tests/features/error_builtins.rs`（8 条断言），features 335 → 343 通过。

**残留**：`Error.prototype.stack`（ES2026 提案，22 例）未做；`Function.prototype/toString`（36 例）需要源码文本；`Error.isError` 对 `class E extends Error` 的实例仍返回 false（子类对象没有 `[[ErrorData]]` 标记）。

### 2.1j M3-B2 二批：`at` / `copyWithin` / `entries` / `keys` / `values`（2026-09-22）

**起点**：`built-ins/Array` 1766，计划 B2 明确列出的 `copyWithin`/`entries`/`keys`/`values` 完全没有实现；`Array.prototype.at` 也缺（11 条 `typeof` 断言直接失败）。

**根因 / 交付**：

1. `Array.prototype.at(index)`：`ArrayObject` 快路径 + 类数组泛型路径（`ToIntegerOrInfinity`、负索引从尾部算、越界 `undefined`）。同名方法在 `String.prototype` 上也有，派发按接收者区分：字符串原始值/包装对象走 String，其余带 `length` 的对象走 Array —— 原先用 `kind == Array` 判断，导致 `Array.prototype.at.call({length:2,…})` 落到字符串路径并返回 `"]"`。
2. `Array.prototype.copyWithin(target, start, end)`：先把源区间读进缓冲区再写回，重叠区间因此符合规范的**快照语义**；索引经 `ToIntegerOrInfinity` 归一并按 `length` 截断，`count` 取 `end - from` 与 `len - to` 的较小值，长度接近 2^53 也不会挂。
3. `Array.prototype.entries`/`keys`/`values`：返回**迭代器对象**，而迭代器只能由 VM 铸造（状态存在 iterator registry 里、`next` 是注册表里的原生函数）。注册名走 `mark_prototype_method`，实际在 `call_native_by_name` 的 `__proto_method__` 分支里用 `array_like_elements` 取快照后 `make_iterator`。
4. 顺带修 `NativeIteratorObject`：补 `Symbol.iterator` 自身工厂，且 `make_iterator` 遇到原生迭代器时直接复用（否则 `for (x of arr.keys())` 会在 `[Symbol.iterator]` 上无限递归并报 TypeError）。

**效果**（逐套件实测）：

| 套件 | 起点 | 本批后 | 变化 |
|------|------|--------|------|
| `built-ins/Array` | 1766 | **1807** | +41 |
| `language/statements/for-of` | 21 | **24** | +3 |

**全量复测**：通过 **7372 → 7416**（+44），失败 2993 → 2949，通过率 71.12% → **71.55%**；新增 `tests/features/array_methods.rs` 三条断言（11 个 `assert`）。

**残留**：非回调类方法（`pop`/`push`/`splice`/`concat`/`shift`/`unshift`/`reverse`）的泛型（类数组）路径仍未做，是 B2 剩余主体（约 70 例）；`entries`/`keys`/`values` 的迭代器对象还不满足 `it[Symbol.iterator]() === it`。

### 2.1k 本轮总览（2026-09-22，十二个提交）

**逐套件对照**（起点 = §2.1e 结束时的全量；§2.1j 结束于 `0612fab`，§2.1l 结束于本批提交）：

| 套件 | 起点 | 现在 | 变化 | 通过率 |
|------|------|------|------|--------|
| `built-ins/Object` | 2031 | **2548** | +517 | 83% |
| `built-ins/Array` | 1548 | **1807** | +259 | 63% |
| `built-ins/String` | 371 | **562** | +191 | 56% |
| `built-ins/Math` | 155 | **270** | +115 | 93% |
| `built-ins/Number` | 110 | **180** | +70 | 63% |
| `built-ins/Function` | 151 | **189** | +38 | 59% |
| `built-ins/NativeErrors` | 24 | **62** | +38 | 76% |
| `built-ins/Error` | 7 | **34** | +27 | 47% |
| `built-ins/Symbol` | 22 | **37** | +15 | — |
| `built-ins/Boolean` | 16 | **26** | +10 | — |
| `language/statements/class` | 50 | **64** | +14 | — |
| `language/statements/for-of` | 17 | **24** | +7 | — |
| **全量** | **6021** | **7481** | **+1460** | 58.09% → **72.18%** |

**M3 完成标准核对**：Object/Array/String 三套件通过率 **83% / 63% / 56%**，均 ≥ 50%（达成）；B4 的 Math/Number 失败数大幅下降（Math 134 → 19、Number 175 → 104）；B5 三套件 59% / 47% / 76%，其中 Function 与 Error 未到 60% —— 两者的剩余失败都不是 ES6 核心语义（`Function/prototype/toString` 需要源码文本；`Error/prototype/stack` 是 ES2026 提案）。

**残留失败构成（TOP）**：`Expected a TypeError to be thrown` 294（缺少参数/接收者类型校验）、`Uint8Array` 88（TypedArray，范围外但未打标签）、`Cannot convert undefined or null to object` 87、`not an array` 65（B2 泛型路径未覆盖）、`JSON` 35（M3-B6 未做）。

**未采纳的尝试（记录以免重复踩坑）**：把 runner 的跳过表按"范围外（ES2019+ / Annex B / RegExp 依赖）/ 未实现 / 架构排除"三组扩容后，失败数 2949 → 2554、通过率升到 74.34%，**但通过绝对数从 7416 掉到 7401** —— `Symbol.match`/`Symbol.replace`/`Array.prototype.flat`/`Object.fromEntries`/`__proto__` 这类标签会连带跳过少量**本来通过**的用例（这些 well-known symbol 与部分 Annex B 形态已实现）。按 §2.1 的 KPI 约定（通过绝对数为主指标）已回退该改动；今后扩容跳过表需逐标签核验"是否会跳过已通过用例"。

**下轮建议顺序**：B2 泛型收尾（约 70 例）→ B6 `JSON.parse/stringify`（165 例）→ 参数/接收者类型校验的成体系补强（294 例的最大桶）→ M4 生成器、M5 `Map`/`Set`。

> **进度更新（2026-09-23）**：前三步已全部交付（B2 三批见 §2.1l、JSON 见 §2.1m、类型校验见 §2.1n），并在 M3 内部把当时识别出的最高 ROI 一处也做掉了（`ArraySpeciesCreate`，ES6 核心 —— 见 §2.1o）。全量 6021 → **7785**，通过率 58.09% → **73.78%**。下一步按原顺序进入 **M4 生成器**（跳过池最大单项，4061 例）。若要继续在 M3 内部刮：剩下最大的一块是 ES2019+ 缺失方法（`flat`/`flatMap` 18+4、`Object.fromEntries` 24、`findLast*` 42、change-array-by-copy 62），但它们都在 §1.2 的范围外清单里。

### 2.1l M3-B2 三批：Array 泛型（类数组）路径收尾（2026-09-22）

**起点**：`built-ins/Array` 1807（§2.1j 之后）。非回调类方法合计约 70 例失败，典型是 `TypeError: not an array`。

**根因**：

1. `push`/`pop`/`shift`/`unshift`/`reverse`/`join`/`slice`/`concat`/`splice`/`fill` 一律先 `downcast_ref::<ArrayObject>()`，不是真数组就抛 `TypeError`。但规范把这一族全部定义为按 `length` + `HasProperty` + 索引 `[[Get]]`/`[[Set]]`/`[[Delete]]` 的**泛型**算法，`Array.prototype.X.call(arrayLike, …)` 是它们的正式用法。
2. `splice()` 无参时不删除任何元素（ES 23.1.3.28 step 5：`start` 不存在 → `actualDeleteCount = 0`，只有 `splice(start)` 才删 `len - start`）。实现按 `len - start` 处理，等于"清空整个类数组"。
3. 同名方法在 `Array.prototype` 与 `String.prototype` 上都存在（`at`/`concat`/`includes`/`indexOf`/`slice`），派发却按 `kind == Array` 二选一 —— `Array.prototype.at.call({length: 2, …})` 因此落到字符串实现并返回 `"]"`。
4. 规范允许类数组声明 `length = 2^53 - 1`（test262 有多个这样的用例），逐元素实现会直接按该长度分配/循环。

**交付**：

1. `array.rs` 新增泛型层：`to_object`、`generic_length`（ToLength + 饱和到 2^53-1）、`generic_get`/`generic_set`/`generic_has`（走原型链）/`generic_delete`/`generic_set_length`、`relative_index`；上述十个方法改为"真数组快路径 + 泛型回退"。
2. **稀疏感知**：`own_index_keys_below` + `generic_window_move` 只搬运**存在的**索引，并删除"源缺失的目标索引"；`reverse` 按索引对重写（一侧存在则搬到另一侧、两侧存在则交换、两侧皆无则不动）。于是 `length` 接近 2^53 的 `shift`/`unshift`/`reverse`/`splice` 是 O(存在索引) 而非 O(length)。
3. **物化上限**：新增 `MAX_GENERIC_ELEMENTS = 2^22`，逐元素构建结果的路径（`fill` 区间、`copyWithin` 缓冲、`splice` 的结果数组、`slice`/`concat` 输出）超限即抛 `RangeError("Invalid array length")`；`join` 先用 `(len - 1) × 分隔符字节数` 预判，超过 `MAX_JOIN_LENGTH` 抛 `TypeError`（与参考引擎的报错类型一致）。
4. `splice` 语义修正（无参不删除、`deleteCount` 缺省为 `len - start`、`new_len > 2^53 - 1` → TypeError）；`concat` 接入 `IsConcatSpreadable`（新增 `symbol::is_concat_spreadable_symbol_key`）：数组默认可展开、非数组默认不可展开、`Symbol.isConcatSpreadable` 优先。
5. `at`/`concat`/`includes`/`indexOf`/`slice` 的派发统一改用新增的 `string_prototype_receiver`：字符串原始值或包装对象走 String 实现，其余对象走 Array 泛型实现。

**OOM 事故与护栏（重要）**：本批第一次全量回归把测试进程打爆（OOM，宿主内存被吃满）。根因是 `splice` 的"结果数组"按 `delete_count` 逐元素物化，而 `Array.prototype.splice.call({length: 2**53 - 1})` 的 `delete_count` 是 2^53-1 —— 恰好叠加了上面第 2 条语义错误。修复后该场景 `delete_count = 0`（只做 `Set(length)`），逐元素路径另有 `MAX_GENERIC_ELEMENTS` 护栏。**并立下约定：此后所有回归（全量与定向）都带内存上限运行**（见 §6.6）。

**效果**（逐套件实测，无一套件下降）：

| 套件 | 起点 | 本批后 | 变化 |
|------|------|--------|------|
| `built-ins/Array` | 1807 | **1870** | +63 |
| `built-ins/String` | 562 | **564** | +2 |

**全量复测**：通过 **7416 → 7481**（+65），失败 2949 → 2884，通过率 71.55% → **72.18%**；本次运行带 `ulimit -v 6000000`，全程未触限（94s）；单元 190 全绿；features 346 → 352 通过（`tests/features/array_methods.rs` 新增 6 个用例 / 20 条断言）。

**验证方式**：泛型语义先把 25 条断言与 node 逐字对照（`join` 的分隔符计数、"空洞也要占位"、`splice` 的增/减/等长三支、`@@isConcatSpreadable` 三种形态），再覆盖 2^53 量级的边界（`splice` 无参/单参/超限、`splice` 近上限增减、`fill` 近上限），全部与 node 一致后才跑全量。

**残留**：泛型路径**无法执行访问器**（`[[Get]]` 直接读属性表），`Array/prototype/reverse/length-exceeding-integer-limit-with-object.js` 这类"靠 getter 抛错提前中止"的用例仍失败；`length` 超过 `MAX_GENERIC_ELEMENTS` 的逐元素操作抛 RangeError，属引擎偏差（参考引擎会做稀疏写、或实际超时不可完成）。

### 2.1m M3-B6：`JSON`（2026-09-22）

**起点**：通过 7481 / 10365（72.18%）。`JSON` 全局不存在（`ReferenceError: undefined variable: JSON` 35 例，主要集中在 `Function/prototype/toString`），计划的 B6 明确列了 `parse`/`stringify`（165 例未启用）。

**根因**：

1. 没有 `JSON` 内建：`JSON.parse`/`JSON.stringify` 都无从谈起。两者都不是纯函数——`stringify` 要跑 `toJSON`/`replacer`、每个属性读都要经 `[[Get]]`（访问器会执行）、还要检测循环引用；`parse` 的 reviver 是用户代码。所以算法必须落在 VM 侧，builtin 层只能提供纯数据版本。
2. runner 的 `UNSUPPORTED_PATTERNS` 里还留着字面量 `"JSON"`（JSON 未实现时期的权宜之计），只要用例源码里出现 "JSON" 就整片跳过——`Function/prototype/toString` 的一批用例因此既不在通过数里、也不在失败池里，掩盖了真实缺口。
3. 三个既有缺陷被本批用例暴露：
   - `RuntimeError::SyntaxError` 被映射到 `Error.prototype`，于是 `JSON.parse` 的错误 `e.name` 是 `"Error"`、`e instanceof SyntaxError` 为 false。
   - `OrdinaryToPrimitive` 不跳过不可调用的方法、且方法查找不走 `[[Get]]`：`{ toString: null, get valueOf() { throw } }` 取不到 `valueOf`（getter 不执行），`JSON.parse(obj)` 因此得不到 Abrupt completion。
   - `ArrayObject::property_get` 对洞返回 `undefined` 而不是 `None`，洞会**遮蔽**原型属性：`delete arr[1]` 之后 `arr[1]` 取不到 `Array.prototype[1]`（`in`/`hasOwnProperty` 却已经按"洞即缺失"处理，两者不一致）。

**交付**：

1. 新增 `src/builtins/json.rs`：
   - `register_json()`：`JSON` 命名空间对象（两个 own 方法 `{writable:true, enumerable:false, configurable:true}`、`JSON[Symbol.toStringTag] === "JSON"`、不可调用/不可构造）；
   - `json_parse()`：严格 ECMA-404 递归下降解析器（只认四种空白、无尾逗号/注释/前导零、`\uXXXX` 代理对合并、重复键后者胜、`__proto__` 作为普通 own 属性）；
   - `quote_json_string()`/`json_number()`：字符串转义与非有限数 → `null`；
   - `json_stringify()`：纯数据版序列化（builtin 派发路径的兜底）。
2. VM 侧实现完整算法：`json_stringify_full`（replacer 函数/属性列表、space 数字或字符串、`toJSON`、包装对象按 `ToNumber`/`ToString` 解包而 `[[BooleanData]]` 直接读槽、循环引用 → TypeError、gap/缩进按规范 `finalize`）、`json_parse_full`（`ToString(text)`，Symbol 抛 TypeError）、`json_internalize`（自底向上走 reviver，写回用 `CreateDataProperty` 语义——失败即忽略，因此 reviver 里造出的不可配置属性保持不变）。
3. runner：启用 `built-ins/JSON`，删除 `"JSON"` 跳过模式，并按"范围外"新增两个跳过标签（`json-parse-with-source` = ES2025 `JSON.rawJSON`、`well-formed-json-stringify` = ES2025 孤立代理转义）。
4. 顺带修掉上面第 3 条的三处既有缺陷（`SyntaxError` 原型映射、`OrdinaryToPrimitive` 的调用性检查 + `[[Get]]` 查找、`ArrayObject` 洞的可见性），并让 `Array.prototype.join` 对 `null`/`undefined` 贡献空串（原先输出 `"null"`/`"undefined"`，`[true, null, "x"].join("|")` 应为 `true||x`）。
5. 测试基建：`tests/helpers.rs` 新增 `describe()`——期望类型不符时不再 `{:?}` 打印对象（内置原型的环会让 Debug 递归并把测试进程打爆，§2.3 的老债），改为按类型摘要。

**验证方式**：先与 node 逐字对照 45 条断言（parse 的错误分类、重复键、转义、代理对、reviver 的替换/删除/嵌套、stringify 的省略/装箱/`toJSON`/replacer/space/循环/键序/缩进、命名空间对象形状），全部一致（仅 `"\ud83d\ude00".length` 因码点存储偏差为 1 而非 2）后才跑套件。

**效果**（逐套件实测，无一套件下降）：

| 套件 | 起点 | 本批后 | 变化 |
|------|------|--------|------|
| `built-ins/JSON` | 未启用 | **112** | +112（114 执行 / 51 跳过） |
| `built-ins/Object` | 2548 | **2597** | +49 |
| `built-ins/Array` | 1870 | **1903** | +33 |
| `language/expressions/delete` | 36 | **37** | +1 |
| `language/computed-property-names` | 33 | **34** | +1 |
| `built-ins/Function` | 189 | 189 | ±0（新增 1 例执行但失败：`Function.prototype.toString` 需要源码文本） |

**全量复测**：通过 **7481 → 7677**（+196），失败 2884 → 2875，已执行 10365 → 10552（JSON 用例进入分母），通过率 72.18% → **72.75%**；本次运行带 `ulimit -v 6000000`；单元 190 全绿；features 352 → 359 通过（新增 `tests/features/json.rs` 7 个用例 / 30 条断言）。

**过程记录**：首次全量跑出 `language/statements/function` 通过数 -1（`S13_A13_T3.js`：`delete arguments[0]` 后再 `arguments[0] = "A"`）。原因是洞修复只做了一半——`property_get` 认洞、`property_set` 却不把洞"复活"。修好后该套件回到 105. 这次回退是靠**逐套件通过数比对**（而非总数）发现的，再次说明 §6.1 的三级验证有必要。

**残留**：`built-ins/JSON` 仅 2 例失败——`prop-desc.js`（`verifyProperty(this, "JSON", …)` 依赖顶层 `this` 是全局对象，与"strict only"目标冲突）、`stringify/value-tojson-not-function.js`（用 `/re/` 正则字面量，RegExp 范围外）。51 例跳过中，`json-parse-with-source`/`well-formed-json-stringify` 为 ES2025，其余为 `Proxy`/`BigInt`/`Reflect.construct`/`cross-realm`。

### 2.1n M3-B7：抽象操作的错误语义（2026-09-23）

**起点**：通过 7677 / 10552（72.75%）。§2.1k 把"参数/接收者类型校验的成体系补强"排在 B2 泛型收尾与 JSON 之后，理由是失败原因聚合里 `Expected a TypeError to be thrown` 以 **291 例**成为最大单项（其次才是 Uint8Array 88、`Cannot convert undefined or null to object` 87）。

**根因**（291 例按强制转换语义归拢后是四个共性问题，而不是 291 个独立缺陷）：

1. `Value::to_number` / `Value::to_js_string` 是**全函数**：无论如何都产出一个值，因为它们的使用方（`array_join`、`ToString` 操作码、诊断格式化）没有 `Result` 通道。而规范里 `? ToNumber(x)` / `? ToString(x)` 是**可能中断**的 —— `ToNumber(Symbol)`、`ToString(Symbol)` 都是 TypeError。缺了这条通道，`[].copyWithin(0, Symbol())` 静默读成 `0`、`String.prototype.trim.call(Symbol())` 安静返回。这正是 §2.3 已登记的 `ToString(Symbol)` 老债。
2. 内置方法的接收者缺少 `RequireObjectCoercible`：`String.prototype.X.call(undefined)` 走到了 `to_js_string()`、产出 `"[object Object]"` 之类的噪声而不是 TypeError。`Object.prototype.valueOf/hasOwnProperty/propertyIsEnumerable/isPrototypeOf/toLocaleString` 与 `Object.hasOwn` 同样不加校验地返回 `false`/原值。
3. Array **快路径绕过 `[[Set]]`**：`ArrayObject` 直接改 `Vec`，于是 `Object.freeze(a); a.push()`、`Object.defineProperty(a,'length',{writable:false}); a.pop()` 都静默成功；而规范里这四个方法都以 `? Set(O,"length",…,true)` 收尾，必须抛 TypeError。
4. `set_member` 对"无处安放的赋值"静默 no-op —— 非可写属性、无 setter 的访问器、原始值的属性。§2.3 记为"引擎不追踪 strict 来源"，但 G1 已经写明引擎是 strict-only（§1.1 的硬约束 1），所以那句注释本身就是错的方向。

**交付**：

1. `src/builtins/mod.rs` 新增抽象操作的**抛错通道**（与全函数版并存，不改动后者的任何调用方）：
   - `require_object_coercible`（ES 7.2.1）、`to_string_throwing`、`to_number_throwing`；
   - `string_receiver` —— String 原型方法共用的开场（RequireObjectCoercible + ToString）；
   - `to_integer_or_infinity_throwing` / `to_length_throwing`（底层 `to_integer_or_infinity_from` / `to_length_from` 与全函数版共用，避免两套截断逻辑漂移）。
2. String 原型（全部 24 个方法）的接收者一律经 `string_receiver`，参数按规范取对应通道：`searchString` → ToString、`position`/`index` → ToInteger/ToNumber、`padEnd` 的 fill 串 → ToString。新增两个私有辅助 `position_arg`（码元位置，`charAt`/`charCodeAt`）、`slice_bound` / `to_substring_bound`（`slice` 负数从尾部算、`substring` 负数按 0）。
3. Object 侧接收者校验：`hasOwnProperty` / `isPrototypeOf` / `propertyIsEnumerable` / `toLocaleString` / `valueOf` 前置 `RequireObjectCoercible`；`Object.hasOwn` 改为先 `ToObject(O)`（顺带补上符号键走 `ToPropertyKey`）。注意 `Object.prototype.toString` **不**加校验 —— 它按规范对 `undefined`/`null` 返回 `[[object Undefined]]`/`[[object Null]]`，是唯一例外。
4. Array 侧参数校验：新增 `relative_index`（已存在，改为传播 ToNumber 的中断）、`clamped_index`；`at` / `copyWithin` / `fill` / `slice` / `splice` / `indexOf` / `lastIndexOf` 全部接上。VM 内的 `array_index_of`（`indexOf`/`lastIndexOf` 的类数组版）一并改为 `Result`，用同一套 `to_integer_or_infinity_throwing`。
5. `ArrayObject::length_is_writable()`（`length_writable && !frozen`）+ `array.rs` 的 `set_length_throwing`，打在 `push`/`pop`/`shift`/`unshift` 四条快路径的**入口** —— 规范里这四个方法无论如何都会做那次 `Set(O,"length",…,true)`。
6. `set_member`（G1 strict-only）：非可写属性、只有 getter 的访问器、原始值上的属性都改为抛 TypeError，错误信息带属性名。

**效果**（逐套件实测，无一套件下降）：

| 套件 | 起点（§2.1m 后） | 本批后 | 变化 |
|------|------------------|--------|------|
| `built-ins/String` | 564 | **606** | +42 |
| `built-ins/Object` | 2597 | **2612** | +15 |
| `built-ins/Array` | 1903 | **1917** | +14 |

**全量复测**：通过 **7677 → 7756**（+79），失败 2875 → 2796，通过率 72.75% → **73.50%**；单元 190 全绿；features 359 → 366 通过（新增 `tests/features/coercion_errors.rs` 7 个用例 / 32 条断言）。失败原因里那条 291 例的 `Expected a TypeError to be thrown` 降到 **244**。

**过程记录**：`Object.freeze(a); a.push()` 的处置比预想细 —— 检查必须落在方法**入口**而不是末尾。`shift/set-length-array-is-frozen.js` 是在 `Array.prototype[0]` 的 getter 里冻结数组的：若把检查放在末尾，那时 `length` 已被改成 0，用例后半段的 `assert.sameValue(array.length, 1)` 就崩了；放在入口则只有"getter 未被调用"这一半失败（`arrayPrototypeGet0Calls` 为 0），状态断言是对的。真正补齐那一半需要让索引读写走原型链，见残留。

**残留**（全部登记进 §2.3）：

- `push`/`pop`/`shift`/`unshift` 的另 8 例（`*-is-frozen` 系列里靠原型链 getter/setter 冻结的那些）仍未解决：它们要求索引的读写经原型链执行访问器，而 builtin 层无法调用用户 getter —— 与 §2.3 的"Array 泛型路径不执行访问器"、"按名派发忽略属性归属"同源，需把这组方法上移到 VM，属跨层改动。
- ~~`ArraySpeciesCreate` 缺失~~：已在本会话的下一批解决，见 §2.1o。
- 引擎内部（模板插值、`String(x)` 的隐式转换、`Value::to_js_string` 的其余调用方）仍是全函数语义：`ToString(Symbol)` 的完整修复需要给这些路径也铺 `Result` 通道，留到 M6。

### 2.1o M3-B7 二批：`ArraySpeciesCreate` 与结果对象写入（2026-09-23）

**起点**：通过 7756 / 10552（73.50%）。§2.1n 收尾时列出的残留里，`ArraySpeciesCreate` 缺失是唯一**属于 ES6 核心**、且带 test262 用例簇可测的一项（`Symbol.species` 是 §1.2 "在范围" 清单里的 well-known symbol）。失效规模：`built-ins/Array` 下凡提到 `Symbol.species` 的用例有 **43 例失败**。

**根因**：

1. `Array[Symbol.species]` 这个 well-known 访问器本身没有注册（`built-ins/Array/Symbol.species/{symbol-species,return-value,length,symbol-species-name}.js` 全失败）。它不是装饰品 —— 它决定了"子类默认继承 species"能否被观察到，也决定了引擎能不能把"默认路径"和"有覆写"分开。
2. `ArraySpeciesCreate(O, len)` 完全没实现。`map`/`filter`/`slice`/`splice`/`concat` 规范上都是以 `A = ? ArraySpeciesCreate(O, len)` 起手的，但本引擎这五个方法都直接返回新建的 `ArrayObject`，于是：物种构造器不被调用（`create-species.js`）、取值不抛错（`create-species-poisoned.js`）、结果对象上的写入失败被吞掉（`target-array-non-extensible.js`）。
3. 两个 `Get`（`O.constructor`、`C[@@species]`）与最后的 `Construct` 都可能跑用户代码，builtin 层没有 `Construct` 也没有可重入的 `[[Get]]` —— 这决定了这套东西**只能落在 VM 侧**，和当年把回调方法组上移是同一个道理。

**交付**：

1. `register_species_accessor`：`Array` 构造器上注册 `Symbol.species` 访问器（getter 返回接收者，`{enumerable:false, configurable:true}`）；原生名 `__array_species__`，在 `NativeFunctionObject::install_metadata` 里补上展示名映射，所以 `Array[Symbol.species].get.name === "get [Symbol.species]"`。
2. `VM::array_species_create`（ES 9.4.2.3）：`Get(O,"constructor")` → 仅当是对象才 `Get(C,@@species)`，`null`/`undefined` 回落到普通数组，`? Construct(C, «len»)`，非构造器 → TypeError。返回 `Option<Value>`：`None` = 走原来的 `ArrayCreate` 路径。
3. **`SameValue(C, %Array%)` 短路**：`Array[Symbol.species]` 返回自身，所以默认情况下 species 会绕回 `Array`；先比较原生名再决定是否 `Construct`，避免每一次 `slice` 都白白穿一次构造流程。
4. `map`/`filter` 接入（VM 本来就在 VM 侧实现这两个）。`map` 改为先做回调再按 `Some/None` 两路写结果；默认路径的洞保留语义不变 —— 原先靠 `entries` 顺序线性推导，现在改成显式记录缺席索引，两种写法等价。
5. `slice`/`splice`/`concat` 经新增的 `try_array_species_method` 接入：只在"确实有 species 覆写"时才接管 —— 先用 builtin 算出普通结果（对接收者的副作用与原先逐字一致），再把存在的索引逐个 `CreateDataPropertyOrThrow` 写进目标对象。没有覆写时一律返回 `Ok(None)`，路径完全不变。
6. `CreateDataPropertyOrThrow` 复用 §2.1n 新修的 `set_member`（strict-only 语义）：目标不可扩展 / 属性不可写 / 不可配置时抛 TypeError。

**效果**（逐套件实测，无一套件下降）：

| 套件 | 起点（§2.1n 后） | 本批后 | 变化 |
|------|------------------|--------|------|
| `built-ins/Array` | 1917 | **1946** | +29 |

**全量复测**：通过 **7756 → 7785**（+29），失败 2796 → 2767，通过率 73.50% → **73.78%**；单元 190 全绿；features 366 → 374 通过（新增 `tests/features/array_species.rs` 8 个用例 / 24 条断言）。

**验证方式**：species 语义是"错误时序敏感"的 —— 物种查找必须发生在算法之前（回调一次都不许跑）。写了 8 条 feature 用例专门钉这件事（毒化 getter、`null` 回落、非数组接收者不查 species、非构造器 TypeError、目标不可扩展时四个方法都抛、以及默认路径的洞保留）。

**残留**：

- `flat`/`flatMap`（ES2019）未实现，它们的 species 用例自然也还失败；`findLast`/`findLastIndex`、`toSorted`/`toReversed`/`toSpliced` 同属 ES2023 范围外。
- `Array/Symbol.species/length.js` 与 `return-value.js` 仍失败：前者要 `get.length === 0` 经 arity 表（部分），后者依赖 §2.3 已登记的 getter 接收者/覆盖语义长尾。
- `Symbol/species/basic.js` 等三个符号层面用例报"descriptor should not be configurable" —— 是 well-known symbol 的属性描述符（不应可配置）还没纠正。

### 2.1p M4-G1：`function*` / `yield` 子集（2026-09-23）

**起点**：通过 7785 / 10552（73.78%）。按 §2.1k 的顺序，M3 的三步（B2 泛型收尾、JSON、类型校验）与 M3 内部最高 ROI 的 species 都已交付，进入 **M4 生成器** —— 跳过池最大单项（§3.2 记 4061 例，实际两个目录 556 例 + 散落在已启用套件里的 `yield` 用例）。

**为什么这一批能落下来**：§4 把 M4 标成"本项目迄今最大的运行时改动"，但实际上**不需要新的帧表示**。事前摸清的三件事决定了方案：

1. 一个 JS 帧在数据栈上就是一段连续区间：实参在 `[rbp-argc, rbp)`、局部在 `[rbp+0 …]`、`rsp` 之上是表达式临时值（codegen 的 `store_args` → `PushC Rbp` → `CallEx` → 序言 `AddC rsp, stack_size`，收尾 `MovC Rsp, Rbp` + `PopC Rbp` + `SubC Rsp, argc`）。**挂起 = 把 `[rbp-argc, rsp)` 拷出来，恢复 = 拷回去**，函数体怎么编译完全不变。
2. `invoke_with_new_target` 已经在用 `while self.step(module)? {}` 驱动嵌套帧，并以 `module.instructions.len()` 作哨兵返回地址 —— "跑到这一帧结束"这条缝是现成的。
3. `step` 在 `pc` 越过指令表末尾时返回 `false`。所以 `Yield` 只要 `jump(instructions.len())`，嵌套循环就停了，与 `Ret` 落到哨兵完全同构。

**交付**：

1. 编译器：`FuncSignature` 增加 `is_generator`（`as_generator()`），`function*` 经 `lower_function_inner` 的新参数一路传到 `Module.generators`；`Expression::YieldExpression`（非 `delegate`）降级为新的 IR `Instruction::Yield { dst, src }`；codegen 发射 `Opcode::Yield`（无操作数时以 `Immd(-1)` 占位）；SSA 重命名补上该指令。
2. `src/vm/object.rs`：`GeneratorObject`（`ObjectKind::Generator`）+ `SuspendedFrame`。`next` 与 `[Symbol.iterator]` 是 `__gen_next__<id>` / `__gen_iterator__<id>` 两个原生（走 `call_native_by_name`），因为恢复函数体需要 VM。
3. `src/vm/mod.rs`：
   - `create_generator`：调用 `function*` 只造对象，不跑函数体（拦截在 `Opcode::CallEx` 的 `Value::Function(id)` 分支）；
   - `generator_next`：保存调用方上下文 → 装帧（首次推实参 / 之后拷回帧区间，并把 `next(v)` 的 `v` 写进挂起那条 `Yield` 的目的操作数）→ 嵌套循环 → 若挂起则把帧摘出来 → 恢复调用方上下文 → 返回 `{value, done}`；
   - `Opcode::Yield`：记下产出值、**记下自己的 `pc`**，然后跳到指令表末尾；
   - `SavedExecutionState` 抽出 `invoke` 的那套保存清单，并**额外保存 19 个寄存器** —— 寄存器文件是全局的，不跟着帧走，嵌套运行会把调用方还在用的值冲掉（`[it.next().done, it.next().done]` 里的数组字面量就是这么丢的）。
4. runner：`generators` 从 `UNSUPPORTED_FEATURES` 移除，`UNSUPPORTED_PATTERNS` 里的 `"function*"` 与 `"yield "` 一并删除，`SUITES` 新增 `language/statements/generators` 与 `language/expressions/generators`。
5. 顺带修掉一处既有栈扩容缺陷：`State::push` 只按"翻倍"扩容，而函数序言的 `AddC rsp, stack_size` 可以一步把 `rsp` 推过当前容量（生成器用例触发了 `index out of bounds: len is 1024 but the index is 3605`）；改为同时覆盖 `rsp + 1`。

**效果**：

| 套件 | 之前 | 本批后 |
|------|------|--------|
| `language/statements/generators` | 未执行 | **19** 通过 / 13 失败 / 234 跳过 |
| `language/expressions/generators` | 未执行 | **16** 通过 / 18 失败 / 256 跳过 |
| 全量 | 7785 / 10552（73.78%） | **7897 / 10768**（73.34%） |

两个生成器目录的通过率 **35/66 ≈ 53%**，达到 §4 的"解锁后 ≥ 40%"标准（不需要动用子集阶段的 25% 放宽）。分母 +216、失败 +104 是解锁的必然代价 —— 通过**绝对数 +112** 才是主指标（§2.1 KPI 约定）。单元 190 全绿；features 374 → 383 通过（新增 `tests/features/generators.rs` 9 个用例 / 30 条断言）。

**踩过的坑（记下来免得重踩）**：

- 一开始把 `save_execution_state()` 放在装帧**之后**，于是 `saved` 里已经含了生成器自己的帧，恢复后那些记录永远留在栈上 → 后续所有表达式读到的都是垃圾（连 `123` 都返回 `[object Object]`）。必须像 `invoke` 那样在推任何东西之前保存。
- 挂起时 `pc` 已经是哨兵了：先 `jump` 再读 `self.state.pc` 拿到的是指令表末尾，恢复后从错的地方继续。改为在 `Yield` 里先记 `generator_yield_pc`。
- `yield` 表达式的值属于**下一次** `next(v)`，而 `Yield` 指令恢复后不会再执行，所以写值只能放在恢复路径（从 `instructions[frame.pc]` 读出目的操作数，用恢复后的 `rbp` 定位）。

**本子集不覆盖（M4 后续）**：`yield*`；`gen.return()` / `gen.throw()`；`try` 内的 `yield`（`SuspendedFrame` 故意不带 SEH 记录，带异常处理器的生成器体跨挂起不可靠）；生成器作为构造器；`Generator.prototype` / `%IteratorPrototype%` 原型链与 `next.length`/`name` 元数据。

### 2.1q M4-G2/G3：`yield*`、生成器函数对象、迭代完成值（2026-09-23）

**起点**：通过 7897 / 10768（73.34%），两个生成器目录 35 通过 / 27 失败。§2.1p 交付后剩下的失败**基本不是恢复机制的问题** —— 把它们按名字摊开看，是三件独立的事：

1. 生成器**函数对象**的表面：`prototype` / `instanceof` / 不可 `new` / `arguments`（`default-proto.js`、`prototype-value.js`、`has-instance.js`、`invoke-as-constructor.js`、`arguments-*`）。
2. **`yield*`**（G2）完全没做 —— 而 §2.1p 里它连降级都没接（只处理了 `delegate: false`）。
3. 迭代器的**完成值**在 `done` 时被丢掉，于是 `yield*` 拿不到委托方的 `return` 值。

**交付**：

1. 生成器实例继承 `g.prototype`（ES 14.4.11 / 9.1.13 的 `GetPrototypeFromConstructor`）：`create_generator` 读函数对象的 `prototype` 属性，是对象就设为实例的原型。`instanceof` 与挂在 `g.prototype` 上的方法随之可用。
2. `function*` 不可构造：`Opcode::New` 在解析出 `func_id` 后先查 `Module.generators`，命中即抛 TypeError。
3. **`yield*` 用脱糖实现，而不是新增指令**。规范里 `yield*` 不是单条指令能表达的：下一次 `next(v)` 的 `v` 要送进**委托迭代器**的 `next`，这本身是个循环。降级成
   ```text
   iter = GetIterator(expr); sent = undefined
   cond: step = iter.next(sent); if (step.done) → done
   body: sent = yield step.value → cond
   done: result = step.value; IteratorClose(iter) → after
   ```
   全程只用既有的 `MakeIterator` / `CallMethod` / `GetProperty` / `Yield` / `IteratorClose`。`sent = yield …` 这一句正是"送进去"的通道：外层 `next(v)` 给的就是委托方 `next` 该看到的实参。
4. `iterator_next` 不再把 `done` 时的 `value` 丢掉。原先 `if done { return Ok((Undefined, true)) }`，于是 `function* i(){ return "R" }` 的 `yield* i()` 只能拿到 `undefined`。`IterateNext` 的调用方只在 `has_next` 为真时用 `item`，不受影响。
5. 原生迭代器的 `next(v)` 转发实参 —— 原来 `iterator_next` 内部是 `invoke(&next, iterator, &[])`，实参被吞掉，这是第 3 条能真正生效的前提。
6. `SuspendedFrame` 带上 19 个寄存器槽。§2.1p 只围绕嵌套循环保存/恢复了**调用方**的寄存器，但生成器**自己**在 `yield` 时握在寄存器里的值同样会被下一次嵌套运行冲掉 —— 有了它，"寄存器里的值跨 `yield`"这一整类隐患才真正关掉。

**效果**：

| 套件 | §2.1p 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/expressions/yield` | 未启用 | **28** 通过 / 29 失败 | +28 |
| `language/statements/for-of` | 24 | **50** | +26 |
| `language/statements/generators` | 19 | **21** | +2 |
| `language/expressions/generators` | 16 | **18** | +2 |
| 全量 | 7897 / 10768（73.34%） | **7933 / 10825**（73.28%） | +36 |

单元 190 全绿；features 383 → 389 通过（新增 6 个生成器用例 / 15 条断言，`tests/features/generators.rs` 合计 15 个用例）。

**过程记录**：`yield*` 打通后 `sent` 仍然传不过去，先怀疑 SSA 的跨块 phi，导出字节码看才发现脱糖的 CFG 是对的 —— 真正断在原生 `next` 的 `&[]`。**先导出字节码再猜**，比反过来省事得多（§6.1 的三级验证又一次成立）。

**残留**：`gen.return()` / `gen.throw()`；`try` 内的 `yield`（`SuspendedFrame` 仍不带 SEH 记录）；`default-proto.js` 一类需要 `%GeneratorFunction.prototype%` / `%GeneratorPrototype%` 真实原型链的用例；`arguments`、`restricted-properties`、`scope-*` 等与生成器无关的长尾。

### 2.1r 派生构造器的 `this` 绑定（`class C extends P`）（2026-09-23）

**起点**：通过 7933 / 10825（73.28%）。生成器告一段落后回到失败池里最大的一簇：`language/statements/class` 147 例失败，其中 `subclass/builtin-objects/*/super-must-be-called.js` 是整齐的一家人（12 例，覆盖 Array/Boolean/Error/Function/GeneratorFunction/Number/String 等）。它们长这样：

```js
class CustomError extends Error { constructor() {} }
assert.throws(ReferenceError, function() { new CustomError('foo'); });
```

也就是 ES 9.2.2 的 `[[ConstructorKind]] = derived`：派生构造器的 `this` 绑定存在但**未初始化**，只有 `super()` 能绑定它（`BindThisValue`）；在此之前读 `this`、或从构造器返回（隐式返回 `this`）都要抛 ReferenceError。本引擎原先直接把 `Opcode::New` 预先造好的对象当 `this`，从未区分过。

**交付**：

1. 编译器：新增 `FuncSignature::is_derived_ctor`（`as_derived_ctor()`），由 `lower_class` 在 `class.super_class.is_some()` 时打标；`Module.derived_ctors` 随模块下发（与 `generators` 同一套机制）。
2. VM：`State::this_uninitialized` —— 与 `this_stack` 平行的布尔栈，`enter_frame` 压 `false`，未被 `super()` 绑定的派生构造器帧压 `true`。
   - `Opcode::New` 建帧后按 `Module.derived_ctors` 调 `mark_this_uninitialized()`；
   - `Opcode::CallSuperSpread`（全部 `super(...)` 都走这一条）**在调用之前**清除最内层那个 `true`；
   - `Opcode::LoadThis` 命中未初始化即抛；
   - `Opcode::Ret` 在构造帧返回时若仍是未初始化则抛，且**经 `as_js_exception` + `handle_throw`** 走 SEH —— 否则 `assert.throws(ReferenceError, …)` 看不到它，只会让整个程序中断。
3. `super()` 的接收者改为取**当前帧的 `this`**，不再由降级 emit 一条 `load_this`。原因很实际：派生构造器里 `load_this` 会在 `super()` 之前执行（那条值只是喂给 `super` 的接收者），一读就抛。箭头函数捕获 `this` 同理 —— `MakeArrowFuncObj` 改为从帧取，于是 `constructor() { (() => super())(); }` 这种（test262 的 `derived-class-return-override-catch-super-arrow.js` 形态）不再被迫提前读 `this`。
4. 顺带修掉一个既有缺陷：**默认派生构造器什么都不做**。原来是 `constructor() { return; }`，`class C extends P {}` 根本不会调用父类；现在是 `constructor(...args) { super(...args); }`（ES 14.5.15）。

**踩到的两个坑**（都在"栈要配套"上，值得单列）：

- `unwind_frames_to` 截断了 `this_stack` / `frame_argc` 却没有截断新加的 `this_uninitialized`。结果是异常展开后残留一个 `true`，**后面任意一个 `Ret`** 都会再抛一次 —— 表现为"错误在顶层 try 里能抓到，一旦 try 在被调用的函数里就逃逸"。同类问题在 §2.1p 已经出现过一次（那时是控制栈没配对）。
- `Ret` 分支里直接 `return Err(...)` 不会经过 SEH 路由（`step` 只对 `run_instruction` 的返回做 `as_js_exception`），异常因此不可捕获。任何"以 JS 异常形式暴露"的规范错误都必须过 `handle_throw`。

**效果**：

| 套件 | §2.1q 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/statements/class` | 80 | **92** | +12 |
| 全量 | 7933 / 10825（73.28%） | **7949 / 10825**（73.43%） | +16 |

单元 190 全绿；features 389 → 392 通过（新增 3 个用例 / 8 条断言，挂在 `tests/features/class_super.rs`）。

**提交说明（偏离记录）**：§2.1n ~ §2.1r 这五批落在同一批文件里（`src/vm/mod.rs`、`src/compiler/lowering/mod.rs`、`src/builtins/mod.rs` 每批都改，且彼此不构成可独立编译的切片），因此合并为**一个提交**（见 `git log`），而不是 §4 完成标准里的"每个 B 任务独立提交"。五批的起止数据仍按批分别记录在上面各自的小节里。

**残留**：`super-must-be-called` 里 ArrayBuffer / DataView / Map / Promise / Set 那几例是"父类本身未实现"，不是这条语义的问题。`class C extends Error {}` 仍拿不到 `message`/`name`，`class C extends Array` 也仍不会把元素装进派生实例 —— 那是下一块：**内置构造器作父类时，`super()` 要用 `new.target.prototype` 建对象并把初始化写到派生 `this` 上**（`regular-subclassing.js`、`message-property-assignment.js` 等约 20 例）。

### 2.1s 内置构造器作父类：`super()` 的 `new.target` 与 `BindThisValue`（2026-09-24）

**起点**：通过 7949 / 10825（73.43%）。§2.1r 收尾时新登记的那条债 —— `class C extends Error/Array/Boolean/…` 拿不到父类的初始化。这是 `super-must-be-called` 之后顺理成章的下一步：同一种形态（`subclass/builtin-objects/*`）的另一半。

把失败摊开看，是两件事叠加，缺一不可：

1. **原型不对**：`super()` 调用原生父类时走的是 `invoke_with_new_target` 的"原生分支"，而那条分支**完全忽略了 new.target** —— 实例拿到的永远是父类内建的原型（`Error.prototype`），而不是 `C.prototype`。
2. **初始化没落到 `this` 上**：原生构造器（`error_constructor` 等）自己 new 一个对象返回；而派生帧里的 `this_val` 仍是 `Opcode::New` 预先造的那个空对象。于是 `new C('m').message` 是空的。

**交付**：

1. `Value::strict_eq_obj` —— 只在两侧是同一个对象时为真。用来判断 `newTarget` 与正在执行的构造器是否同一个函数，避免 `===` 的强制转换语义混进来。
2. `VM::prototype_from_constructor`（ES 9.1.14 `GetPrototypeFromConstructor`）：`newTarget.prototype` 是对象就用它，否则回退到构造器自己的 `prototype`。`native_construct` 改用它，并新增 `new_target` 参数；`reparent` 标志在 `newTarget !== 构造器` 时把新造出来的实例**重新挂到** `newTarget.prototype` 上 —— 注意 `finish_native_construct` 只在实例"还没有原型"时才挂，而内建构造器造出来的实例是带着内建原型的，所以这一步必须单独做。
3. `super()` 的结果绑定为 `this`（ES 12.3.5.1 `BindThisValue`）：`CallSuperSpread` 在结果是对象时把 `this_val` 换成它。字节码父类返回的本来就是同一个对象，所以这条只影响内置父类 —— 正好是需要的那一半。

**效果**：

| 套件 | §2.1r 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/statements/class` | 92 | **109** | +17 |
| `language/expressions/class` | 14 | **15** | +1 |
| `built-ins/Error` | 35 | **36** | +1 |
| 全量 | 7949 / 10825（73.43%） | **7968 / 10825**（73.61%） | +19 |

单元 190 全绿；features 392 → 395 通过（`class_super.rs` 新增 3 个用例 / 12 条断言）。

**残留**：`Array` 的子类化仍不完整（`super-must-be-called` 之外的 `regular-subclassing.js`、`length.js`、`contructor-calls-super-*.js` 要的是"元素装进派生实例"，即 `ArrayCreate` 也要走 `newTarget`）；`Function` 子类的 `length`/`name` own 属性（`instance-length.js`、`instance-name.js`）；`message should be an own property` 一类要求 `message` 是**自有**属性（现在多半落在 `Error.prototype` 上）。这三处都属于同一条主线：**每个内建构造器的初始化都得搬到"作用于传入的 `this`"这条路上**，逐个补。

### 2.1t 派生构造器只能返回对象（ES 9.2.2 step 13c）（2026-09-24）

**起点**：通过 7968 / 10825（73.61%）。§2.1s 之后 `language/statements/class` 还剩 115 例失败，其中 `derived-class-return-override-*` 是一整家人（10 例）：

```js
class Base { constructor() {} }
class Derived extends Base { constructor() { super(); return 0; } }
assert.throws(TypeError, function() { new Derived(); });
```

ES 9.2.2 的第 13 步：基础构造器返回原始值 → 忽略、用 `this`；**派生**构造器返回非 `undefined` 的原始值 → **TypeError**。

**交付**：

1. `State::this_uninitialized` 改名为 `this_state`，从 `Vec<bool>` 变成 `Vec<u8>` 的**三态**：`THIS_NONE`（普通帧）/ `THIS_DERIVED_UNBOUND`（派生构造器、`super()` 之前）/ `THIS_DERIVED_BOUND`（`super()` 之后）。
   为什么要合并成一条栈：判断"这一帧是不是派生构造器"本来需要**再加一条**与 `this_stack` 平行的栈，而 §2.1r 已经踩过一次"平行栈没同步"的坑（`unwind_frames_to` 漏截断）。两个事实用一条栈表达，就少一处必须同步的地方。
2. `Opcode::Ret` 在派生构造器帧且返回值为"非 undefined 且非对象"时抛 TypeError。

**这一条的难点全在抛错的时机上**，值得单独记：

- 必须在 `[[Construct]]` 把原始值替换成 `this` **之前**判断 —— 否则 Rv 已经变成对象，永远触发不了。所以返回值要在帧展开前抓下来（`returned`）。
- 必须在帧**完全展开之后**才抛 —— 也就是 `seh_stack.truncate(saved_seh_depth)` 之后。我第一版放在 `frame_argc.pop()` 之后、`seh_stack` 截断之前，结果派生构造器**自己**的 `catch` 把错误吞了，紧接着第二次 `Ret` 又把控制栈弹空，报 `RangeError: control stack underflow`。
- 前一点正是 `derived-class-return-override-catch.js` 的考点：**这个 TypeError 属于 `[[Construct]]`，不属于构造器函数体**，所以构造器内的 `try` 抓不到，而调用方的 `assert.throws(TypeError, () => new C())` 抓得到。抛错位置就落在这一线之间。

**效果**：

| 套件 | §2.1s 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/statements/class` | 109 | **115** | +6 |
| 全量 | 7968 / 10825（73.61%） | **7974 / 10825**（73.66%） | +6 |

单元 190 全绿；features 395 → 397 通过（`class_super.rs` 新增 2 个用例 / 8 条断言）。

**残留**：`derived-class-return-override-*` 里 `-catch-super` / `-catch-super-arrow` / `-for-of` 三例仍未过（都还需要 `try`/`finally` 与 `super()` 的组合语义，与 §2.3 里"finally 块执行两次"的老债同源，归 M6）；`superclass-bound-function.js`、`class-definition-null-proto-*` 是另外的小簇。

### 2.1u M4-G3：`gen.return()` / `gen.throw()` 与 `yield*` 的委托关闭（2026-09-24）

**起点**：通过 7974 / 10825（73.66%）。M4 只剩 G3（IteratorClose 完整化）与 G4（联调）。`language/expressions/yield` 的 29 例失败里，`star-rhs-iter-rtrn-*`（11）是"生成器被 `return()` 中断时要关闭 `yield*` 打开的迭代器"，`star-rhs-iter-thrw-*`（12）是同一件事的 throw 版本。

**交付**：

1. 生成器对象补上 `return` / `throw` 两个原生方法（`__gen_return__<id>` / `__gen_throw__<id>`），与 `next` 同构。
2. `VM::generator_abrupt`：`return(v)` 以 `{ value: v, done: true }` 完成生成器；`throw(e)` 完成生成器并把 `e` 抛给调用方（ES 25.4.3.4 / 25.4.3.5）。尚未启动的生成器同样直接完成。
3. **委托追踪**：`yield*` 的迭代器现在是"挂起的"，会被 abrupt completion 关闭。
   - 新增 `Opcode::DelegateOpen` / `DelegateClose`（IR 层 `Instruction::DelegateOpen/Close`），脱糖在 `make_iterator` 之后 open、在 done 分支 close；
   - VM 用 `delegate_stack` 记着；`SuspendedFrame` 带上属于本帧的那一段（挂起时摘出、恢复时放回）；
   - `generator_abrupt` 在返回之前按" innermost first"逐个 `GetMethod(iter, "return")` → 调用 → 校验结果是对象（ES 14.4.14 step 5.c）。
4. `iterator_close` 增加"完成值"参数：`yield*` 那条路要把 `received.[[Value]]` 传给 `return()`，而普通 `IteratorClose`（ES 7.4.6）不传 —— 同一个函数、两种调用，参数区分。
5. 顺带修掉：原生迭代器的 `return()` 原先返回 `undefined`，而它是迭代器方法、必须返回 iterator result；`yield*` 的"结果必须是对象"检查因此一直失败。

**踩到的坑**（都是"栈/深度要在装帧之前取"）：

- `delegate_depth` 一开始放在帧装好之后取，于是恢复路径刚把本帧的委托放回去就被算成了"resumer 的深度"，下一次挂起时切片越界（`range start index 1 out of range for slice of length 0`，feature 用例 `yield_delegate_value_is_the_inner_return` 直接崩）。必须和 `save_execution_state()` 一样**在装帧之前**取。这是 §2.1p 那次（`save_execution_state` 装帧后取）的同一个教训第三次出现。
- `close_delegates` 里拿到的 `return` 方法是**包装对象**的原生 `__iter_return__<id>`，不是用户写的那个函数；它替用户迭代器转发调用，所以修它而不是绕过它。

**效果**：

| 套件 | §2.1t 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/expressions/yield` | 28 | **30** | +2 |
| 全量 | 7974 / 10825（73.66%） | **7976 / 10825**（73.68%） | +2 |

单元 190 全绿；features 397 → 401 通过（`generators.rs` 新增 4 个用例 / 11 条断言）。

**净收益只有 +2，值得说明**：`return()`/`throw()` 本身在 test262 里几乎没有单独计分的用例，它们的价值是让 `for-of` 里 `break` 真正触发 `IteratorClose`、以及让 `yield*` 在中断时关闭委托 —— 这两条都是"别的用例依赖它"的语义，而不是自带一批用例。§2.1q 时 `for-of` 靠完成值涨了 26，这一批是把那条路补完整。

**残留**：`star-rhs-iter-thrw-*`（12 例）仍未过 —— 它们要求 `throw()` **恢复函数体**并把异常抛在 `yield` 的位置上，这样 `try`/`catch` 能接住、`finally` 能跑；这需要 `SuspendedFrame` 带上 SEH 记录（§2.1p 起就明确排除）。同样地，`return()` 也还没跑 `finally`。这两件事归同一个后续任务：**给生成器的挂起帧补上异常处理器**。

### 2.1v `yield*` 的 `throw` 转发（2026-09-25）

**起点**：通过 7976 / 10825（73.68%）。§2.1u 之后 `language/expressions/yield` 的 26 例失败分成两半：`star-rhs-iter-thrw-*`（12）与 `star-rhs-iter-rtrn-*`（11）。

读了一遍用例才发现两半的要求**不一样**：

- `thrw` 那半要的是 `yield*` 的委托协议本身 —— 停在 `yield*` 上时 `throw(e)` 要调委托迭代器自己的 `throw` 方法（`star-rhs-iter-thrw-thrw-invoke.js` 断言 `callCount === 1`、`args[0] === 7777`、`thisValue === spyIterator`）。**这不需要 SEH**，只需要把委托转发的另一半（throw）也接上。
- `rtrn` 那半要的是"生成器被恢复"（`-rtrn-no-rtrn.js` 断言 `hitFinally === true`），那才是真正需要 SEH 的部分。

所以先做不需要 SEH、且能独立验证的那一半。

**交付**：

1. `generator_abrupt` 的 throw 分支：`GetMethod(iterator, "throw")` → 调用 → 结果必须是对象 → `done` 为真则以 `IteratorValue` 完成生成器，为假则产出该值且**生成器保持挂起**（仍停在同一个委托上）。委托没有 `throw` 方法时，完成生成器并把异常抛给调用方。
2. `VM::delegate_js_iterator`：`MakeIterator` 的包装对象只认识 `next` / `return`，没有 `throw`，所以委托的 `throw` 必须到它包裹的那个 JS 迭代器上找 —— 这也顺带让接收者变成 `spyIterator` 本身（用例断言的就是这个）。

**踩到的坑**：`generator_abrupt` 里 `let mut gobj = obj_ref.borrow_mut();` 之后用 `let Some(gobj) = gobj.as_any_mut()…` **遮蔽**了同一个名字 —— 外层的 `RefMut` 并不会因此释放，而是活到函数结束。下游的 throw 分支要把挂起帧放回去，于是二次 `borrow_mut()` 直接 panic（`RefCell already borrowed`）。改成把借用包在一个块里、只把 `(suspended, delegates)` 带出来。

**效果**：

| 套件 | §2.1u 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/expressions/yield` | 30 | **31** | +1 |
| 全量 | 7976 / 10825（73.68%） | **7977 / 10825**（73.69%） | +1 |

单元 190 全绿；features 401 → 402 通过（`generators.rs` 新增 1 个用例 / 2 条断言）。

**净收益仍然很小（+1），如实记录**：用例里剩下的失败大多已经不再是"委托没被调用"，而是更细的东西 —— 委托的 `throw` 自己抛出的异常要**以 JS 异常的形式**回到 `iter.throw()` 的调用方（现在多半直接逃逸到顶层），以及 `-rtrn-*` 那半需要的"恢复函数体跑 finally"。前者是异常传播路径的问题，后者是 SEH。两个都在下面的同一条任务里。

**下一步（同一个任务）**：给 `SuspendedFrame` 带上 SEH 记录，让 `return()` / `throw()` 真正恢复函数体。这一件事同时解决：`-rtrn-*` 的"执行被恢复"、`-thrw-*` 的异常回到调用方、以及 §2.1p 以来"生成器体不能带 `try`"这条限制。

### 2.1w 挂起帧携带异常处理器（`finally` 不再丢失）（2026-09-25）

**起点**：通过 7977 / 10825（73.69%）。§2.1u / §2.1v 两批都把同一句写进了残留："`return()` / `throw()` 不恢复函数体，根因是 `SuspendedFrame` 不带 SEH 记录"。本批先把这个根因拆掉 —— 它是"恢复函数体"的前置条件，而且它本身已经是一个可观察的 bug。

**先确认 bug 真实存在**：`function* g(){ try { yield 1; yield 2; } finally { log.push('fin') } }` 连调三次 `next()`，`log` 是空的。挂起时 `SuspendedFrame` 有意不带 SEH 记录，于是 `finally` 随帧一起被丢掉 —— 函数自然结束也不会跑它。这不是"没实现"，是**错误的可观察行为**。

**交付**：

1. `SehRecord` 加 `Clone`（它本来就 `Debug`）。字段全是 `usize` / `bool` / `Option<Value>` / `Option<isize>`，克隆即可。
2. `SuspendedFrame.seh: Vec<SehRecord>`：
   - 挂起时取 `seh_stack[seh_depth..]`，`seh_depth` 用的正是 `save_execution_state()` 里那个 —— 它同样必须在**装帧之前**捕获（§2.1p / §2.1r / §2.1u 已经因为这件事踩过三次，这次直接复用了 `saved.seh`）。
   - 恢复时 `seh_stack.extend(frame.seh)`，并且**必须在 `pushc(seh_stack.len())` 之前** —— 那条 ctrl 记录存的是"本帧开始时保存的 SEH 深度"，顺序错了 `Ret` 就会截断过头。

**效果**：

| 指标 | §2.1v 后 | 本批后 |
|------|----------|--------|
| `language/expressions/yield` | 31 | 31（±0） |
| 全量 | 7977 / 10825（73.69%） | **7977 / 10825**（73.69%，±0） |

单元 190 全绿；features 402 → 403 通过（`generators.rs` 新增 1 个用例 / 2 条断言）。

**为什么全量 ±0，如实说明**：test262 里那几个 `try`/`finally` 跨 `yield` 的用例，断言的是 `iter.return()` 之后 `hitFinally === true` —— 也就是"**被中断时**跑 finally"，而不只是"正常结束时跑 finally"。本批修的是后者（当前没有对应用例计分），前者仍要等"恢复函数体"。

**下一步的具体障碍（已经定位到）**：`return(v)` 恢复函数体需要把 `yield` 当作 `return v` 执行，也就是要**触发一次 `Ret`**，好让 `Opcode::Ret` 里既有的 finally 分发逻辑（扫 `seh_stack` 找未执行的 `finally_pc`、标 `in_finally`/`delayed_return`、跳到 finally、finally 末尾再跳回同一条 `Ret`）发挥作用。难点在于：finally 块的**回跳目标在编译期就固定成了某条 `Ret`**，而挂起点在 `yield` 上、那里没有 `Ret` 可跳。可选的三条路：(a) 让降级把每个含 `try` 的函数末尾那条隐式 `Ret` 的 pc 记进 `func_info`，恢复时跳到它；(b) 把 finally 的回跳目标改成动态的（挂起帧里存）；(c) 在 VM 里手写一遍"跑 finally 链"的循环，不依赖回跳。下次开工从这三条里选。

### 2.1x 方案 (a)：让 `return()`/`throw()` 真正恢复函数体（2026-09-25）

**起点**：通过 7977 / 10825（73.69%）。§2.1w 把阻碍写成了三条候选路，本轮取 **(a)**：把每个函数末尾那条 `Ret` 的 pc 记进 `Module`，恢复时跳到它，好让 `Opcode::Ret` 里既有的 finally 分发逻辑跑起来。

**这一批的过程比预期曲折，四道坎是逐个撞出来的**（记下来，因为它们各自都是独立的缺陷）：

1. **`Module::exit_pc`**：`compiler/mod.rs` 在发射每个函数的字节码时记下"最后一条 `Ret`"的绝对 pc。这是 (a) 的全部"新机制"。
2. **`Ret` 的 finally 扫描取错了记录**。原来是 `seh_stack.iter()`（从最外层开始）找第一条待执行 finally；但 finally 的 epilogue 是 `ResumeExc`，它读的是 `seh_stack.last()` —— 只能收尾"栈顶那条"。于是 `try { try { return x } finally {…} } finally {…}` 会先跑**外层** finally，此时内层记录还在栈顶，`ResumeExc` 什么也不做、顺势落进出口块的 `Mov rv, undefined`，返回值丢失。改成 `.rev()`（最内层优先）后，`language/expressions/yield` 里嵌套 finally 的用例与 5 条长期失败的 `return_in_try_finally*` 一起转绿 —— 这个 bug 在 §2.3 里挂了很久（"finally 块被执行两次 / 返回值丢失"）。`Br` 走出 try 的延迟跳转扫描同样有这条义务，一并改了。
3. **`ResumeExc` 的 delayed-return 分支不该"往下落"**。它的注释写着"re-execute Ret opcode"，但实际布局里 `ResumeExc` 后面先是**正常完成路径**的 `Br`（跳到 `try` 之后的语句），再是出口块。直接落下去会执行本应被 `return` 跳过的语句，还会被出口块的 `Mov rv, undefined` 冲掉值。改为显式跳到 `Module::exit_pc`（新增 `current_function_id()`，从 `function_val` 取回本帧所在的函数 id）。
4. **生成器体必须是一道 invoke 边界**。`run_generator_frame` 里推 `invoke_boundaries`，否则体内抛出的异常会从嵌套循环里直接跳到**调用方**的 handler，把控制栈条目提前截断，随后 `discard_generator_frame` 再弹就 `control stack underflow`。边界值必须是 `save_execution_state()` 记录的**装帧之前**的控制栈深度（`SavedExecutionState.ctrl`）—— 用装帧之后的深度会让生成器自己的 `try` 也被判成"帧外"，`star-rhs-iter-nrml-next-call-err.js` 一族因此集体失败。这是"深度要在装帧之前取"这条教训的第四次出现。
5. **委托关闭要在装帧之后做**，否则 `yield*` 的迭代器 `return()` 抛出的异常到不了生成器自己的 `try`。顺带发现 `iterator_close`（原生包装对象的 `return`）按 `IteratorClose` 语义**吞掉**异常，而 `yield*` 的委托关闭必须传播（`? Call(return, …)`），所以 `close_delegates` 改成解包后直调用户迭代器。
6. **返回值取请求的那个，不取 `run.rv`**：出口块以 `Mov rv, undefined` 开头，`finally` 只要跑过就会把它冲掉。这是 (a) 的已知偏差 —— `finally { return x }` 时我们会报 `v` 而不是 `x`，记入 §2.3。

**效果**：

| 套件 | §2.1w 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/expressions/yield` | 31 | **36** | +5 |
| `built-ins/String` | 606 | **608** | +2 |
| `built-ins/Function` | 189 | **190** | +1 |
| 全量 | 7977 / 10825（73.69%） | **7981 / 10825**（73.73%） | +4 |

**单元 190 全绿；features 首次全绿：403 失败 5 → 411 全过**。这 5 条正是 `control_flow::return_in_catch_finally`、`return_in_try_finally`、`return_in_try_catch_finally_no_throw`、`return_in_try_finally_nested`、`return_in_try_finally_with_result_modification` —— 从 §2.3 登记起就一直红着，本轮由第 2、3 条修复一并解决。新增 3 个生成器用例（`return` 恢复/`finally` 执行、`throw` 交付给函数体、委托关闭异常进 `catch`）。

**为什么 test262 只涨 +4**：`language/expressions/yield` 的 21 条剩余失败要么要求 `yield*` 在**没有** `throw` 方法时按规范走 `IteratorClose`（`-thrw-violation-*` 5 条），要么依赖更细的属性访问计数（`star-rhs-iter-nrml-res-done-no-value.js` 之类）。机制本身已被 feature 用例逐条验证；计分少是因为这些用例在测第三种语义。真正的收益在别处：**一个挂了两个多月的 `finally` 缺陷被关掉了**，而这正是 M6 里"finally 块被执行两次"那条债的另一半。

**残留（更新进 §2.3）**：

- `finally { return x }` 期间的返回值以请求值为准，不看 `finally` 自己的 `return`（出口块 `Mov rv, undefined` 所致，见第 6 条）。
- `yield*` 的委托没有 `throw` 方法时，规范要求先 `IteratorClose(代理)` 再把异常交给函数体；目前直接交给函数体（`-thrw-violation-*` 5 条）。

### 2.1y 跳过表里有两条不是"特性门控"的（2026-09-25）

**起点**：通过 7981 / 10825（73.73%），跳过 14761。

§2.1p 起就能看到一件怪事：两个生成器目录共 556 个测试文件，怎么跑都只有 66 个在执行。本轮顺着这条线查 `should_skip`，发现跳过表里混进了两条**与特性无关**的规则 —— 它们不是"引擎支持不了"，而是当初为了少看失败随手加的：

1. **`Flag::Generated` 被当成跳过理由**。test262 的 `generated` flag 的含义是"这个文件由工具自动生成、别手改"，是**出处标记**，不是能力要求。这些是普通测试，必须跑。它压掉的量：两个生成器目录 556 个文件里有 **463 个**带 `flags: [generated]`。
2. **`$DONOTEVALUATE` 被列进 `UNSUPPORTED_PATTERNS`**。这些是**普通的 negative 用例**：测试源里先写一段必须被拒绝的语法，再调 `$DONOTEVALUATE()`；harness 让它抛错，所以那行只有"引擎错误地接受了语法"时才会执行 —— 而 runner **本来就支持 `negative:` 元数据**（`tests/test262_runner.rs:301`，parse / resolution / runtime 三个阶段都做）。这条规则纯属多余。

**改动**（无新增机制，只有三处表项增删）：

1. `should_skip` 的 flag 分支只保留 `Async` / `Module` —— 这两条是真的：源码需要本引擎按 G3 永久排除的语法。
2. 删掉 `UNSUPPORTED_PATTERNS` 里的 `$DONOTEVALUATE`。
3. 补上 `class-static-methods-private` / `class-static-fields-private` —— 见下。

**第 3 条是顺出来的**：解锁后失败池里冒出 971 条"method invoked exactly once"，全是 `dstr/private-*-static-*`。查下去发现这批用例声明的是 `class-static-methods-private`，而跳过表里只有 `class-methods-private` / `class-fields-private`，`contains` 不会把 static 变体认作同一件事（`"class-static-methods-private"` 既不 startswith 也不 contains 前者）。§1.2 已判定"class 私有字段/方法"非 ES6、不在范围内，所以这是跳过表的**漏项**，补上。

**效果**（三次改动分别实测）：

| 阶段 | 执行数 | 通过 | 失败 | 跳过 | 通过率 |
|------|--------|------|------|------|--------|
| 起点（§2.1x 后） | 10825 | 7981 | 2844 | 14761 | 73.73% |
| 去掉 `Flag::Generated` | 17005 | **11349** | 5656 | 8581 | 66.74% |
| 再去掉 `$DONOTEVALUATE` | 18504 | **12299** | 6205 | 7082 | 66.47% |
| 再补两行 private-static | 17477 | **12217** | 5260 | 8109 | **69.90%** |

**最终：通过 7981 → 12217（+4236）**，执行数 10825 → 17477。逐套件比对，前两步**零回退**；第三步只影响两个 class 套件（各 -41 通过 / -470 失败）。

**那 82 个通过是第 3 条的成本，说明一下**：带这两个特性的用例共 1824 个，其中 179 个 negative、1645 个 positive。跳掉它们会连带失去 82 个原本能过的用例。选择跳过，理由是 §6.2 自己定的规则（"范围外特性在表内注明理由"）以及失败池的可用性 —— 池里少 944 条与 ES2022 私有成员有关的噪声，通过率也更能代表引擎的真实水平。**若要绝对数最大化，删掉那两行即可拿回 82**，代价是失败池重新被这 944 条淹没。记录在此，便于日后反悔。

**同一批顺手量清的边界**（避免下次重复判断）：`class-fields-public` / `class-static-fields-public`（公开 class 字段，ES2022）**不跳** —— 失败的公开字段用例里 **833 个与范围内特性共存**（`computed-property-names`、`generators`、`destructuring-binding` 等），只有 5 个是纯范围外；跳掉会连带埋掉 833 个范围内测试。已把"公开 class 字段"补进 §1.2 的范围外表并注明这一取舍。

**关键套件**：

| 套件 | §2.1x 后 | 本批后 |
|------|----------|--------|
| `language/statements/generators` | 21 | **156** |
| `language/expressions/generators` | 18 | **165** |
| `language/statements/for-of` | 50 | **121** |
| `language/statements/try` | 34 | **61** |
| `language/expressions/yield` | 36 | **39** |
| `language/statements/class` | 115 | **995** |
| `language/expressions/class` | 15 | **889** |
| `language/identifiers` | 92 | **201** |

**通过率下降是分母的假象**。旧的 10825 个测试全部仍在执行且一个没退，它们上面的成绩仍是 **7981 / 10825 = 73.73%**；新解锁的 6652 个里 4236 个通过（63.7%）。引擎本身的水平没有变化，是**度量范围**修正了。往后引用通过率时都要说明分母 —— 旧数字与新数字不可直接比较。

**M4 完成标准达成**：`generators` 解锁后的通过率要求是 ≥ 40%，现在是 `statements/generators` 156/(156+96) = **61.9%**、`expressions/generators` 165/(165+107) = **60.7%**。

**耗时**：全量从约 40s 增至 **2m05s**（+212%），工作量 +61%。新解锁的多为失败用例（异常路径与解析失败比正常路径贵），单测耗时上升。§6.4 的时间基线按 2m05s 重记。

**跳过池 8109 例的构成**（按声明特性归拢）：`Temporal`、`async-iteration` / `async-functions` / `await`、`class-methods-private` / `class-fields-private` 及其 static 变体、`BigInt`、`TypedArray`、`dynamic-import` / `modules`、`Proxy` / `Reflect`、`Atomics` / `SharedArrayBuffer`、`Intl`；源码模式则是 `Date` / `RegExp` / `eval(` / `new Function(` / `with (`（§1.2 范围外，§1.1 的 G3 永久排除）。**跳过表至此可以认为只剩真话了**，§6.2 的治理到此收敛。

**残留**：`$DONOTEVALUATE` 解锁后出现了 153 条 `expected Some("SyntaxError") at Parse phase but compilation succeeded` —— 引擎接受了本应拒绝的语法。这批从"看不见"变成"红着"是好事，属 M6-T2 的一致性收尾。
### 2.1z 声明位解构：`catch` / `for-in` / `for-of` 的头（2026-09-25）

**起点**：通过 12217 / 17477（69.90%）。§2.1y 解锁后 `language/statements/for-of` 从 121 / 576 变成池里比例最差的套件，顺着 149 条 `Expected SameValue` 和 77 条 `ReferenceError: undefined variable: x` 查下去，是一个**单一缺陷**造成的整片失效。

**根因**：解构在四个"声明位"里的表现不一致 ——

| 位置 | 修复前 |
|------|--------|
| `var [x] = …` / `let {a} = …` | ✅ 走 `bind_pattern` |
| 形参 `function f([x])` / 箭头参数 | ✅ 走 `bind_pattern` |
| 赋值位 `[x] = …` | ✅ |
| **`catch ([x])`** | ❌ `ReferenceError` |
| **`for (var [x] in …)`** | ❌ `ReferenceError` |
| **`for (const [x] of …)`** | ❌ `ReferenceError` |

后三处调的是 `binding_pattern_name(&pattern)` —— 这个函数对非标识符模式**直接返回占位符 `"<destructured>"`**（并打一条 `log::warn`），于是符号表里插进了一个叫 `"<destructured>"` 的变量，真正的叶子 `x` 从未声明。三处都只有一行，都在"把模式当标识符"：

- `bind_for_of_left`（`for-of`/`for-in` 共用）带一条注释写着 "Destructuring patterns land here in T7; for now bind the simple identifier" —— T7 从没来。
- `lower_try` 的 catch 参数同样先取名字、再 `alloc` + `assign` + `symbols.insert`。

**改动**：两处都改走既有的 `bind_pattern`（`mod.rs:2595`，`var [x] = …` 用的就是它，含默认值、rest、嵌套、对象/数组两套）。`for-of` 的简单标识符保留原来的直接槽位，热路径不变；catch 无参数时不再插占位符。

**这一处的收益是被 §2.1y 放大的**：`destructuring-binding` 在失败池里出现 1975 次，但绝大多数测试文件里 `dstr` 的位置是解构**赋值**或**语句级声明**（本来就对），真正踩中的是头部绑定。

**效果**：

| 套件 | §2.1y 后 | 本批后 | 变化 |
|------|----------|--------|------|
| `language/statements/for-of` | 121 | **318** | +197 |
| `language/statements/try` | 61 | **125** | +64 |
| `language/statements/for-in` | 40 | **42** | +2 |
| 全量 | 12217（69.90%） | **12480**（71.41%） | +263 |

逐套件比对**零回退**；单元 190 全绿；features 411 → 412（新增 `destructuring_binds_in_declaration_heads`，含嵌套模式 + 默认值 + rest + `for-in` 解构键字符串）。失败 5260 → 4997。

**残留**（仍在池中，`for-of`/`try`/`for-in` 合计 484 条）：
- 77 条 `expected Some("SyntaxError") at Parse phase but compilation succeeded` —— 集中在 `yield` 作标识符（strict / generator 体内）、`labelled-fn-stmt-*`（标签函数的声明位置）、`obj-id-*-strict`。属**解析期校验缺口**，与 §2.1y 暴露的 153 条同类，是同一批工作。
- 56 条 `Expected a TypeError` / 36 条 `Test262Error` 未抛 —— 迭代协议的异常路径。
- 5 条 `execution step limit exceeded`（`S12.14_A9_T2` 等 + `for-of/body-dstr-assign-error.js`）—— 被 §6.5 的步数护栏兜住，需要逐条确认是测试自身的循环还是引擎缺陷。

### 2.1aa 解析期早期错误：接入 oxc 语义构建器 + runner 的 `onlyStrict`（2026-09-25）

**起点**：通过 12480 / 17477（71.41%）。§2.1z 收尾时把失败池里最大的一类"能编译但规范要求 SyntaxError"数出来是 **533 条**，逐族看全部是 **strict 模式早期错误**：`yield` 作标识符、保留字作标识符（`eval` / `implements` / `let` …）、重复形参、标签函数声明、箭头函数参数的重复绑定。

**两个独立缺口，缺一不可**：

1. **runner 无视了 `onlyStrict`**。test262 的约定是 `onlyStrict` 用例的源里**没有** `"use strict"`，由 runner 补在第一行。而且这行必须落在**整段装配程序的指令序言**里 —— 把它插在 harness 之后就只是一条普通表达式语句，整体仍是 sloppy。修前：这些用例按 sloppy 解析，`for ({ eval } of [])` 自然被接受，于是"该报错却没报"。
2. **早期错误在 oxc 的语义构建器里，不在解析器里**。实测 `'use strict'; function f(a, a){}` 在 `SourceType::cjs()` 下不报错 —— oxc 把这些非纯语法的早期错误放进了 `oxc_semantic::SemanticBuilder`（`check_syntax_error`），而本项目只依赖 `oxc_parser/ast/span/allocator/syntax/diagnostics`，**从未跑过语义阶段**。

**改动**：三处，都很小。

1. `Cargo.toml` 增加 `oxc_semantic = "0.132"`（与既有 oxc 组件同版本）。
2. `parse_js`（`src/compiler/parser.rs:153`）在解析成功后跑一遍
   `SemanticBuilder::new().with_check_syntax_error(true).build(&program)`，把它的 `errors` 并入既有的 `ParseErrors` 通道 —— 于是这些诊断走的是和其它语法错误完全相同的路径（同样有 `negative: phase: parse` 的判定逻辑接收）。
3. `build_source`（`tests/test262_runner.rs:117`）对带 `flag: onlyStrict` 的用例在装配结果**最前面**加一行 `"use strict";`。

**效果**：

| 指标 | §2.1z 后 | 本批后 | 变化 |
|------|----------|--------|------|
| 全量通过 | 12480（71.41%） | **13003**（74.40%） | **+523** |
| 失败 | 4266 | **4474** | −523 |
| "能编译但应报 SyntaxError" | 533 | **10** | −523 |

即**这一类几乎被一次扫空**（剩下 10 条不是早期错误，是别的 negative 用例）。提升最大的套件：`language/statements/class` 995 → **1066**、`language/expressions/class` 889 → **955**、`for-of` 318 → **357**、`switch` 13 → **49**、`expressions/object` 595 → **631**、`for-in` 42 → **75**、`variable` 87 → **118**、`arrow-function` 137 → **168**、`function` 279 → **307`、`assignment` 348 → **374**。

逐套件比对**零回退**（这点特意核对过：新增的语义检查有误报风险，会把本来能过的用例打掉；实测没有任何一套件的通过数下降）。单元 190 全绿；features 412 全绿。

**为什么这一步值 +523 而不是 +150**：手写那 5 族的估计是基于"每族只影响自己那批用例"，但早期错误是**横切**的 —— 每个套件里都散着一批 `onlyStrict` 的 negative 用例（class 的求值顺序测试、`switch` 的保留字测试、`variable` 的 `let` 作标识符测试…），一次性全部到位。

**残留**：剩余 10 条 parse-phase 缺口里已无早期错误；但**引入语义阶段后出现了新的可见簇**（此前被"能编译"掩盖）：92 条 `arrow function invoked exactly once`、42 条 `a should be an own property`、39 条 `Actual argument [undefined] shouldn't be primitive.`，以及 112 条 `Cannot convert undefined or null to object`。这些属实现型工作，按 §3.1 排队。

**一个提醒**：`build_source` 只补了 `onlyStrict`。按 test262 的规范，**既无 `onlyStrict` 也无 `noStrict`** 的用例要跑两遍（sloppy + strict），两边都得过。本引擎本就是 strict-only（§1.1 G1），实际跑的已经是接近 strict 的那一边，所以这一批不追加第二遍；若要完全对齐 test262 的口径，这是个可选项（会引入一批新的失败面）。

### 2.1ab 解构协议的缺口：`RequireObjectCoercible` 与 `IteratorClose`（2026-09-25）

**起点**：通过 13003 / 17477（74.40%）。§2.1aa 扫平早期错误后，失败池里 `Expected a TypeError to be thrown` 还剩 421 条，而它**集中在 `dstr` 目录**（`for-of/dstr` 50、`class/dstr` 24+24、`generators/dstr` 18+18、`assignment/dstr` 15 …）。逐条看是三个独立的规范步骤没做。

**一、对象模式没有 `RequireObjectCoercible`**（84 条）

ES 8.5.2 的 `ObjectAssignmentPattern` 第一步就是 `? RequireObjectCoercible(value)`，**在读取任何属性之前**。这条检查必须无条件执行：空模式 `{} = null` **什么都不读**，所以如果不显式检查，就没有任何东西会注意到源是空值。修前 `(0, {} = null)` 直接得到 `null`。

修法：新增一条 IR 指令 + 字节码 `RequireObjectCoercible`（`instruction.rs` / `builder.rs` / `codegen.rs` / `bytecode.rs` / `vm/mod.rs` 五层，复用既有的 `builtins::require_object_coercible`），在 `bind_pattern` 的 `ObjectPattern` 分支和 `bind_object_assignment_target` 的开头各发一次。这是本项目第一次为"一个规范步骤"新增指令，五层加起来不到 40 行 —— 模板是现成的（`IteratorClose` 的四层写法）。

**二、数组模式不关闭未耗尽的迭代器**（44 条）

ES 8.5.1：模式在源耗尽前停下时，要 `IteratorClose`。原来 `bind_pattern` 的数组分支上有一句注释写着 "The iterator completed normally; no IteratorClose needed." —— 那只在**恰好耗尽**时成立。`[x] = 只生产不结束的迭代器` 一直不关。

修法：新增 `emit_iterator_close_if(cond, it)`，用最后一步 `IterateNext` 的 `has_next` 做条件分支（`br_if` + 新建两个基本块），在 `bind_pattern` 与 `bind_array_assignment_target` 两处、且**只在没有 rest 元素时**发出（rest 会把迭代器抽干）。

**三、`IteratorClose` 不检查 `return()` 的返回值**（18 条）

ES 7.4.6：`return()` 既非 `undefined` 又不可调用 → TypeError（step 4.b）；调用**正常返回**但不是对象 → TypeError（step 6）；调用**自己抛错** → 吞掉（step 5，原完成优先）。`VM::iterator_close` 原来只有"可调用就调、错误全吞"，缺了 4.b 与 6。三个语义差别都在同一个函数里补齐了。

**效果**：

| 套件 | §2.1aa 后 | 本批后 |
|------|-----------|--------|
| `language/statements/class` | 1066 | **1090** |
| `language/expressions/class` | 955 | **979** |
| `language/expressions/assignment` | 374 | **388** |
| `language/statements/for-of` | 357 | **370** |
| `language/statements/for` | 296 | **308** |
| `language/expressions/object` | 631 | **643** |
| `language/statements/function` | 307 | **313** |
| `language/expressions/function` | 191 | **197** |
| `language/expressions/arrow-function` | 168 | **172** |
| `language/statements/variable` | 118 | **121** |
| `language/statements/try` | 130 | **133** |
| `language/statements/let` | 106 | **109** |
| 全量 | 13003（74.40%） | **13136**（75.16%） |

**净增 +133，失败 4474 → 4341，逐套件零回退**；单元 190 全绿；features 412 → 414（新增两组：对象模式对空值源的六种位置、数组模式的关闭时机与 `return` 返回值的三种语义）。

**收益为什么分布在 12 个套件而不是 `dstr` 一处**：这三条都是**协议步骤**，凡是用到解构/迭代的地方都要走 —— `class` 的方法参数解构、`for`/`for-of` 的头部、函数形参，所以 `statements/class` 和 `expressions/class` 各 +24。

**残留（更新进 §3.1）**：
- `*-put-const` 10 条：向 `const` 绑定做解构赋值要在**运行期**抛 TypeError（ES `SetMutableBinding`）。本项目的 const 重赋值检测在 `semantic.rs` 里是**编译期**静态检查，覆盖不到解构这类间接写入；要做需要让降级层知道绑定的可变性并发出运行期抛出。
- 迭代器剩余簇：`ary-init-iter-get-err` 44 条（数组模式里取 `Symbol.iterator` 抛错要传播）、`-close-non-object` 若干。

### 2.1ac `GetIterator` 必须做那次查询（2026-09-25）

**起点**：通过 13136 / 17477（75.16%）。§2.1ab 收尾时列出的 `ary-init-iter-get-err` 44 条，表面看是"数组模式里取 `Symbol.iterator` 抛错没传播"，实际读用例才发现它测的是整个 `GetIterator` 的第一步：

```js
delete Array.prototype[Symbol.iterator];
var f = ([x, y, z]) => {};
assert.throws(TypeError, () => f([1, 2, 3]));
```

**根因**：`VM::make_iterator` 对**真数组**和**字符串**直接构造原生迭代器，**完全跳过了 `GetMethod(obj, @@iterator)`**。ES 7.4.2 的这一步是**可观察**的，跳过它让三件事一起失效：

1. `delete Array.prototype[Symbol.iterator]` 之后 `var [x] = [1]` 仍能成功（应为 TypeError）；
2. 被替换的 `Array.prototype[Symbol.iterator]` 被完全忽略；
3. 取该属性时抛错的 getter 也被忽略。

**改动**：`make_iterator` 现在先无条件做那次属性查询（含 getter 的副作用与抛错），只有在解析到**内置工厂**（原生名 `__iterator_factory__`）时才走数组/字符串快路径 —— 普通 `for (x of arr)` 的开销只是一次属性读取，不是一次 `invoke`。被替换成 JS 函数时改走通用路径（`invoke` 那个工厂）。

**顺带修掉的两处**：

- **`new String('ab')` 迭代导致栈溢出 → 整个 test262 跑不完**。字符串*包装对象*的 `@@iterator` 与数组是同一个原生工厂，而该工厂的实现是"回到 `make_iterator(this)`"，包装对象既不匹配数组分支也不匹配原始字符串分支 —— 于是无限递归。修前它只是"被快路径掩盖"（数组干脆不查 `@@iterator`），补上查询后立刻变成 `fatal runtime error: stack overflow`，把整轮全量回归打断。现在包装对象走 `ObjectKind::String` 分支读字符。
- **`invoke` 对生成器函数没有生成器分支**。`Call` opcode 会检查 `module.generators` 并构造生成器对象，`invoke_with_new_target` 不检查 —— 于是经 `invoke` 调用生成器函数会**立刻执行函数体**。函数体的第一步是参数绑定，参数若是解构模式又会进 `make_iterator`，而那时 `Array.prototype[Symbol.iterator]` 可能正是那个生成器函数：无界递归。修法是在 `invoke` 解析出 `func_id` 后补同样的分支。

**排查过程**：这轮踩到一个工具缺口 —— 崩溃发生在测试线程里（Rust 栈比 CLI 主线程小，引擎自身的调用深度护栏还没来得及报 `RangeError`），进程直接 SIGABRT，报告里看不出是哪个用例。为此给 runner 加了 `BIUJS_TEST262_TRACE=1`：逐条打印正在跑的测试路径，崩溃时最后一行就是元凶。这个开关留着。

**效果**：

| 套件 | §2.1ab 后 | 本批后 |
|------|-----------|--------|
| `language/statements/class` | 1090 | **1106** |
| `language/expressions/class` | 979 | **995** |
| `language/expressions/object` | 643 | **651** |
| `language/statements/for-of` | 370 | **376** |
| `language/statements/for` | 308 | **314** |
| `language/statements/function` | 313 | **317** |
| `language/expressions/function` | 197 | **201** |
| 其余 5 套件各 +2 | | |
| 全量 | 13136（75.16%） | **13211**（75.59%） |

**净增 +75，失败 4341 → 4266，逐套件零回退**；单元 190 全绿；features 414 → 417。

**新登记的技术债**（§2.3）：

- **`[...src]` / `f(...src)` 完全不走迭代协议**。`Opcode::ArrayPushSpread` 用的是 `array_like_elements(&src)`（读 `length` + 整数键的快照），既不查 `@@iterator` 也不执行访问器。`delete Array.prototype[Symbol.iterator]` 之后 `[...[1]]` 仍返回 `[1]`。修它要把它改成 `make_iterator` + 循环，是独立一批。
- **脚本完成值不可靠**。`vm.run(&module)` 返回的不是"最后一个产生值的语句"的值：`var o = {…}; …; 'ok'` 会返回中间某个表达式的值（实测返回过对象字面量本身、`out.join('')` 的结果）。写 feature 测试因此不能用"末尾表达式"取值，改成在 JS 里 `throw`。
- **闭包捕获对对象的处理**。§5 已记"按值快照"，但实测 `var box = {i:0}; … function() { box.i += 1 }` 里 `box` 解析成了数字并报 `Cannot create property 'i' on number` —— 比"快照"更糟，快照点像是取错了寄存器。归 M6 一并看。

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
| `try/catch/finally` 的 finally 块被执行两次 | 异常路径跑一次，catch 结束后又落入内联 finally | ✅ **2026-09-25 大部分关闭**（§2.1x）：`Ret` 的 finally 扫描改为最内层优先（原先取最外层，而 epilogue `ResumeExc` 只能收尾栈顶那条记录），`ResumeExc` 的 delayed-return 分支改为显式跳回函数出口而不再"往下落"。5 条 `return_in_try_finally*` feature 用例全部转绿，features 首次 411 全过。残留：`finally { return x }` 期间的返回值以请求值为准 |
| ~~`new Array(1,2,3)` 元素顺序反转~~ | `String(new Array(2,4,8,16,32))` 等用例失败 | ✅ **2026-09-22 修复**（§2.1g）：`Opcode::New` 对原生构造器多了一次 `args.reverse()`（`collect_call_args` 的约定已是 arg0 在 `[rbp-1]`）；同批统一了原生构造路径，`new Error/Number/Object` 一并验证 |
| panic 时 `{:?}` 打印含原型环的对象导致宿主栈溢出 | 测试基建隐患（Debug 递归进 builtin 原型环） | 已知；测试避免直接 Debug 对象值 |
| Rc/RefCell 无环回收 | 循环引用泄漏 | 暂接受 |
| 闭包值快照语义 | 与规范"引用同一绑定"不同（for-let 按轮捕获等） | 架构决定，相关用例允许失败 |
| 迭代器 `return()`/`throw()`（IteratorClose 异常路径） | break/异常提前退出时未 close | 正常结束路径已 close；异常路径待 M4 |
| 无正则引擎 | 1879 个 RegExp 测试 | 明确不在范围 |
| 顶层 `this` 为 `undefined` | 依赖全局对象作 `this` 的 sloppy 用例 | 与"strict only"目标一致，允许失败 |
| Tagged template 未实现（tag 不调用） | `tag\`\`` 用例 | 计划 M2' 补：strings 数组 + tag 调用 |
| 慢：内置方法多经 `invoke` 派发 | 全量 **2m05s** / 17477 例（2026-09-25 实测，§2.1y） | 建性能护栏 |
| **按名派发忽略属性归属** | `call_prototype_method(this, name)` 只按方法名分派、不看该方法实际来自哪个原型：`var f = Error.prototype.toString; f()` 走通用 `toString`（返回 `"[object Undefined]"`）而非 `Error.prototype.toString` 应抛的 TypeError；`at`/`toString` 的接收者判定只能靠"是不是字符串/有没有 length"这类启发式 | 2026-09-22 登记（§2.1h/§2.1i 两处被迫使用启发式）：少量用例；根治需在派发时携带 [[HomeObject]]，归 M6 |
| `ToString(Symbol)` 不抛 TypeError | `to_js_string` 返回 `String`、无 `Result` 通道，于是 `String.prototype.trim.call(Symbol())`、模板插值里的 Symbol、`Array/String` 方法的参数 Symbol 校验用例失败 | ✅ **部分消除**（2026-09-23，§2.1n）：builtin 层新增 `to_string_throwing` / `to_number_throwing` / `to_integer_or_infinity_throwing`，String/Array/Object 内置方法的参数与接收者已走抛错通道。**残留**：`Value::to_js_string` 本身仍是全函数，模板插值、`String(x)` 隐式转换等引擎内部路径对 Symbol 仍不抛错；把这些路径也铺上 `Result` 通道属跨层改动，归 M6 |
| ~~严格模式下对只读属性的赋值不抛 TypeError~~ | `set_member` 对非可写属性静默 no-op（注释称"引擎不追踪 strict 来源"），`*-gs.js` 生成用例（约 33 例）失败 | ✅ **2026-09-23 修复**（§2.1n）：G1 已确定引擎是 strict-only，改为抛 TypeError（非可写属性 / 只有 getter 的访问器 / 原始值上的属性）；逐套件比对确认无回退（`built-ins/Object` 反 +1），仅 1 条 feature 断言按新语义更新 |
| `String.prototype.length` 与索引按**码点**而非**码元** | Rust `String`（UTF-8）无法表示孤立代理：`"\u{1F600}".length` 为 1（应为 2）、`at`/`codePointAt` 落在代理对中间只能用 U+FFFD 代替 | 2026-09-22 登记（§2.1h）：需换成 WTF-8/`Vec<u16>` 表示，属架构级改动，归 M6 |
| `normalize` 无 Unicode 规范化数据 | 只做 form 校验（非法抛 RangeError），合法 form 原样返回 | 2026-09-22 登记：需引入规范化表（新依赖），归 M6 |
| class 方法带 `prototype`、缺 `name` 推导 | 方法按规范不应有 `prototype`；`var f = function () {}` 的 `name` 应为 `"f"` | 2026-09-22 登记：需 `SetFunctionName`，归 M6 |
| `entries`/`keys`/`values` 的迭代器 `it[Symbol.iterator]() !== it` | 迭代器本身可被 for-of 遍历，但不满足自返 | 2026-09-22 登记（§2.1j）：少量用例 |
| **嵌套函数捕获外层局部变量不可靠** | 非箭头闭包读/写外层函数的局部变量（含对象）可能得到 `undefined` 或抛 `ReferenceError: undefined variable: X`，且同一段代码在不同嵌套上下文里表现不同。最小复现（在 HEAD 上同样存在，非本轮引入）：`function outer(){ var o = {n:1}; function inner(){ return o.n; } return inner(); }` → 期望 1，实测 ReferenceError；`var inner = function(){ return o.n; }` 形态则静默返回 undefined。箭头函数（`var f = () => o.n`）与"把闭包作为实参传给别的函数"两种形态正常 | 2026-09-22 登记（§2.1m 由 JSON reviver 用例暴露）：与 §5 的"创建时值快照"决定同源，根治要把捕获改成引用绑定（或按调用读取），归 M6；在此之前用例应避免依赖该形态 |
| **Array 泛型路径不执行访问器** | 泛型（类数组）路径的 `[[Get]]` 直接读属性表，索引上的 getter 不会被调用：`Array/prototype/reverse/length-exceeding-integer-limit-with-object.js`（靠 getter 抛错提前中止）等用例失败 | 2026-09-22 登记（§2.1l）：需要把泛型路径改为经 VM 的 `[[Get]]`，属跨层改动，归 M6 |
| 泛型路径的物化上限（`MAX_GENERIC_ELEMENTS = 2^22`） | `length` 超过上限的逐元素操作（`fill`/`copyWithin`/`splice` 结果）抛 RangeError，而参考引擎会做稀疏写 | 2026-09-22 登记（§2.1l）：有意为之——本引擎数组是 `Vec` 支撑，无法表示 2^53 长度；先保证不 OOM |
| **内置构造器初始化未搬到派生 `this` 上** | `prototype_from_constructor` 与 `BindThisValue` 已到位（§2.1s），但每个内建构造器的初始化仍是"自己 new 一个"：`Array` 子类装不进元素（`regular-subclassing.js`、`length.js`、`contructor-calls-super-*.js`）、`Function` 子类缺 `length`/`name` own 属性（`instance-length.js`、`instance-name.js`）、`message` 不是自有属性（`message-property-assignment.js`） | 2026-09-24 登记（§2.1s）：让 `ArrayCreate`/`Error` 等接受"待初始化的 `this`"，逐个补 |
| **生成器子集未覆盖的语义** | `return()` / `throw()` 现在会恢复函数体（§2.1x）：`finally` 会跑、体内 `try` 接得住 `throw`、委托关闭的异常也进 `catch`。残留：`yield*` 的委托没有 `throw` 方法时需先 `IteratorClose` 再交给函数体（`-thrw-violation-*` 5 例）；`finally { return x }` 的值以请求值为准；生成器作构造器、`Generator.prototype` / `%IteratorPrototype%` 原型链与 `next.name`/`length` 元数据 | 2026-09-23 登记（§2.1p），2026-09-25 更新（§2.1x） |
| **`[...src]` / `f(...src)` 不走迭代协议** | `Opcode::ArrayPushSpread` 用 `array_like_elements` 读 `length` + 整数键的快照，既不查 `@@iterator` 也不执行访问器；`delete Array.prototype[Symbol.iterator]` 后 `[...[1]]` 仍返回 `[1]` | 2026-09-25 登记（§2.1ac）：修法是把它改成 `make_iterator` + 循环，属独立一批 |
| 脚本完成值不可靠 | `vm.run(&module)` 返回的不是"最后一个产生值的语句"的值，实测会返回中间表达式（对象字面量本身、某次 `join` 的结果）。feature 测试因此不能用末尾表达式取值，要改成在 JS 里 `throw` | 2026-09-25 登记（§2.1ac）：影响测试写法与 REPL 输出，归 M6 |
| 闭包捕获对象的快照点错误 | `var box = {i:0}; …; function() { box.i += 1 }` 里 `box` 解析成数字并报 `Cannot create property 'i' on number`，比 §5 记的"按值快照"更糟 | 2026-09-25 登记（§2.1ac）：归 M6，与 §5 的捕获模型一并看 |
| **Array 快路径绕过原型链上的索引访问器** | `push`/`pop`/`shift`/`unshift` 的 `*-is-frozen` 系列（8 例）在 `Array.prototype[0]` 的 getter/setter 里冻结数组，要求错误在那次访问时抛出；快路径直接改 `Vec`，访问器不执行也没有受限副作用 | 2026-09-23 登记（§2.1n）：`length` 可写性已按 `Set(…,throw)` 处理（同批 +8），剩下的一半要索引读写经原型链、且 getter 可被调用 —— 需把这四个方法上移到 VM，与"Array 泛型路径不执行访问器"同源，归 M6 |
| ~~缺 `ArraySpeciesCreate` / `CreateDataPropertyOrThrow`~~ | `map`/`filter`/`slice`/`splice`/`concat` 的 species 与目标对象写入用例全部失败 | 2026-09-23 登记（§2.1n）→ ✅ **本轮已交付**（§2.1o）：`Array[Symbol.species]` 访问器、`VM::array_species_create`（含 `SameValue(C,%Array%)` 短路）、五个方法接入；`CreateDataPropertyOrThrow` 复用 strict-only 的 `set_member`。残留仅 ES2019+ 的 `flat`/`flatMap`（未实现）与 well-known symbol 描述符的 `configurable` 细节 |

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

| 套件 | 失败数（§2.1y 后） | 主要缺口 |
|------|--------------------|----------|
| `built-ins/Array` | 858 | 泛型路径长尾（species 交互、类数组访问器）、`resizable-arraybuffer` 类用例、sloppy-mode 依赖的 ES5 用例 |
| `language/statements/for-of` | 576 | **解锁后新暴露**：迭代协议的异常路径与 `IteratorClose` 计数（§2.1y 前整个套件只有 50 例在执行） |
| `built-ins/Object` | 501 | `__proto__`/`__lookupGetter__` 等 Annex B、`Object.fromEntries`（ES2019）、描述符长尾 |
| `language/statements/class` + `language/expressions/class` | 501 + 421 | **ES6 核心的最大单块**（合计 922）：`super` 与 home object 语义、字段/方法求值顺序、`computed-property-names` 交互；其中约 1000 条失败属 ES2022 公开字段（§1.2 已注明范围外但留在池中） |
| `built-ins/String` | 399 | 主要被 `replace`/`match`/`search`/`split`（RegExp，范围外）占据；其余是对象接收者经 ToPrimitive 的路径 |
| `language/expressions/arrow-function` | 184 | 解锁后新暴露 |
| `language/statements/try` | 118 | `catch` 绑定与 finally 顺序的剩余组合 |

### 3.2 跳过池（未执行 —— 决定下一步解锁顺序）

**2026-09-25 重记（§2.1y）**：跳过 8109 例。经 §2.1y 的治理，表里剩下的**全部是真实的范围外特性或 G3 永久排除项**，不再有"因为当初没实现而随手加"的条目：

| 类别 | 规模 | 判定依据 |
|------|------|----------|
| `Temporal` / `Intl` | 6385 / 150 | §1.2 非 ES6 且依赖宿主能力 |
| `async-iteration` / `async-functions` / `await` | 4507 / 558 | §1.2 非 ES6 |
| class 私有成员（4 条特性） | 1493 + 775 + 278 | §1.2 非 ES6（ES2022） |
| `BigInt` | 1369 | §1.2 非 ES6 |
| `TypedArray` | 1272 | §1.2 降级项，M6-T1 |
| `dynamic-import` / `modules` | 943 / 389 | §1.1 G3 + §1.2 不承诺 |
| `Proxy` / `Reflect` | 412 / 230 | M5-C4 |
| `Atomics` / `SharedArrayBuffer` | 274 / 179 | §1.2 非 ES6 |
| 源码模式：`Date` / `RegExp` / `eval(` / `new Function(` / `with (` | — | §1.2 范围外 + §1.1 G3 |

（规模按"声明了该特性的测试文件数"计，同一测试可计入多类，故合计大于 8109。）

**解锁顺序**：跳过池里已**没有 ES6 核心项** —— M5（`Map`/`Set`/`Promise`/`Proxy`/`Reflect`）与 M6（`TypedArray`）是仅剩的两个池子，二者都已在路线图上。也就是说从本节之后，**"解锁"这个动作本身已经做完，剩下的全是"实现"**。
### 3.3 优先级结论

**2026-09-25 重排（§2.1y 之后）**。跳过池里已无 ES6 核心项，"解锁"这个动作做完了，下面是纯实现顺序：

1. **ES6 核心语义的残余（M2/M3/M4 收尾）**：`for-of`/迭代协议异常路径 576、`class` 两目录合计 922（`super`/home object/求值顺序）、`built-ins/Array` 858、`Object` 501、`String` 399、`try` 118 —— 都是"已实现但语义没对齐"，单点成本低、收益确定，且互相牵连少。**优先**。
2. **M5 集合与反射**：`Map`/`Set`/`WeakMap`/`Promise`/`Proxy`/`Reflect`。这是跳过池里最大的两块（Proxy 412 + Reflect 230 + Map/Set/Promise/WeakMap）之一，且相互独立、可增量交付。
3. **M4 剩余项**：`yield*` 委托无 `throw` 时的 `IteratorClose`、`finally { return x }` 的值、生成器作构造器与原型链元数据（§2.3 已逐条登记）。规模小，混在 1 里边做。
4. **M6 TypedArray + 一致性收尾**：跳过池的另一大块（TypedArray 1272）；同时做 §2.1y 暴露出来的 153 条 `negative: phase: parse` 缺口，以及文档一致性。

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
| B2 `Array` | `from`/`of`/`fill`/`find`/`findIndex`/`copyWithin`/`entries`/`keys`/`values`/`reduceRight`/`sort` 语义、泛型（类数组）路径、稀疏与长度处理 | Array 失败 1975 → 目标减半；**首批（§2.1e）** 回调方法泛型化/洞感知/`indexOf` 分派；**二批（§2.1f）** `at`/`copyWithin`/`entries`/`keys`/`values` —— 失败 1756 → **976**（通过 1028 → **1807**），达成目标。剩余主体是 `pop`/`push`/`splice`/`concat` 等非回调方法的泛型路径 |
| B3 `String` | `repeat`/`startsWith`/`endsWith`/`codePointAt`/代理对/`normalize`/`at`、`String.raw`、`String` 迭代器与 `[Symbol.iterator]` | String 失败 686 → 目标减半；**两批已交付（§2.1f）**：失败 → 443（通过 371 → 562），达成目标。`String.raw`、`normalize` 的真实规范化数据、`replace/match/search` 的 RegExp 形态仍未做 |
| B4 `Number` / `Math` | ES6 常量（`EPSILON`/`MAX_SAFE_INTEGER`…）与 `isInteger`/`isSafeInteger`/`parseFloat`；Math 的 `hypot`/`sign`/`clz32`/`imul`/`log2`/`log10`/`cbrt`/`trunc`/`fround` | Math/Number 失败显著下降；**已交付（§2.1f）**：`Number::toString` 规范格式化 + 属性特性 + arity，Math 失败 134 → **19**、Number 175 → **104** |
| B5 `Function` / `Error` / `NativeErrors` | `name`/`length` 推导、`Error` 子类原型链与 `message` 缺省、`NativeErrors` 各类型 | 三项目标套件 ≥ 60%；**首批已交付（§2.1f）**：59% / 47% / 76%（Error 剩余主要是 `stack` 提案与 `Function.prototype.toString` 源码文本，非 ES6 语义） |
| B6 `JSON` / `Date`（新增模块） | `parse`/`stringify`、`Date` 构造与常用取值方法 | **JSON 已交付（§2.1m）**：`JSON.parse`/`stringify` 完整语义（含 reviver/replacer/space/toJSON），套件启用后 112 通过 / 2 失败 / 51 跳过；`Date` 仍未做（594 例） |

**完成标准**：每个 B 任务独立提交；提交前全量不回退；`tests/features/` 每任务 ≥ 5 条断言；Object/Array/String 三套件通过率各 ≥ 50%。

### M4：生成器与迭代协议完成

| 任务 | 内容 |
|------|------|
| G1 生成器运行时 | 调用帧挂起/恢复（`Yield`/`Resume`）、`yield` 表达式值双向传递、`return()` 提前终止 | **首批已交付（§2.1p）**：`next()` 子集（挂起/恢复、双向传值、`{value,done}`、自反 `[Symbol.iterator]`），两个生成器目录 53% 通过；`yield*` / `return()` / `throw()` / `try` 内 yield 待补 |
| G2 `function*` / `yield` / `yield*` 降级 | **已交付**（§2.1p + §2.1q）：`yield*` 走"next() + yield"脱糖，委托值双向传递 | 函数体编译为可恢复的帧；委托迭代复用 `Symbol.iterator` |
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
4. **性能护栏**：全量耗时基线 **2m05s / 17477 例**（2026-09-25，§2.1y 解锁后重记；解锁前为约 40s / 10825 例）。劣化 > 2× 需定位（热点：慢路径迭代的 `invoke`、内置方法派发、失败用例的异常路径）。
5. **单元测试**：190 个保持全绿；新增运行时机制（生成器帧、微任务）需补单元测试。
6. **内存护栏**：全量与定向回归**一律带内存上限**运行，例如
   `ulimit -v 6000000; TEST262_FAILURES=20000 cargo test --release --test test262_runner -- --nocapture`。
   VM 仍在演进，一个无界物化/循环（如 §2.1l 的 `splice` 结果数组）就足以吃满宿主内存并打断整个回归；
   加上限后失败会以"进程被限"的形式立刻暴露，而不是拖垮开发机。
7. **test262 修订已冻结**（2026-09-22 定）：`tests/test262.pin` 记录基线所依据的 submodule 修订
   （`7e115f46ac64340827d505fa928ad436cb7ba5a6`，2026-05-21，tc39/test262）；
   汇总报表会打印本次实际用的修订，`cargo test --release --test test262_runner test262_at_pinned_revision`
   在 submodule 漂移时失败——这样每个 §2.1x 的通过数都对应唯一可复现的用例集。
   **选型取舍**：
   - 更老的修订会丢覆盖，且风格更差：把 pin 往前推到 2016-06（ES6 刚定稿）时，我们启用的目录里
     Array 2619 / Object 3079 / String 971 / Number 228 个用例，而现在（pin）是 3081 / 3411 / 1223 / 340
     ——ES6 的一致性用例在定稿后仍在大量补写；同时老用例普遍依赖 sloppy mode 与 ES5 语义，
     与"strict only"的目标相性更差。
   - 逐日跟随上游会让分母无声漂移：pin 之后上游又有 73 个提交，其中只有 5 个触及我们启用的目录
     （4 例 object-rest 解构用例、1 例 `Array.prototype[Symbol.unscopables].at`、26 例 ES5 `Object`
     用例重写），其余集中在 Intl / TypedArray / modules / Temporal 等范围外领域。范围外的
     *特性*由 `UNSUPPORTED_FEATURES` / `UNSUPPORTED_PATTERNS` 处理，不需要靠"固定老修订"来回避。
   - 结论：现代修订 + 受治理的跳过表，比"固定到 ES6 时代的修订"信号更强、更可解释。
   **升级流程**（显式决策，不做隐式漂移）：`git -C tests/test262 checkout <rev>` → 全量回归
   （带内存上限）→ 记录新 §2.1x 的逐套件数字 → 更新 `tests/test262.pin`（rev/date）与
   §2.1x/`es6-feature-support.md`/README 的计数 → 同一个提交里完成。

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
