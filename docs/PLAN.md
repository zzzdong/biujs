# 下一阶段任务计划

起点是**当前实现**（`docs/ARCHITECTURE.md`），不是旧方案。旧方案在
`docs/archive/architecture-redesign.md`，只用来溯源，不再照着施工。

## 0. 现状一句话

test262 **84.06%**（16567 / 执行 19708）。剩下的 16% 绝大多数是**缺语义**
（BigInt、可增长 ArrayBuffer、Realm、生成器细节、`Symbol.toStringTag`…），
不是吞吐，也不是帧模型的问题。

## 1. 判据（决定顺序）

1. **先修语义 bug，再做性能优化**。84% 的天花板是"特性没实现"，不是"跑得慢"。
2. **先消结构性隐患，再加功能**。手工断环（`Builtins::teardown` 的 23 项清单）
   已经造成过一次全量崩溃，越晚改越贵。
3. **能独立验证的先做**。每一步都要能单独跑 `phase2-status.sh` 确认零回退。
4. **改动核心类型不要分批**（编译器会列全位点，半迁移成本更高）。

## 2. 任务清单

### P0 — 收尾与减负（小、独立、立刻能做）

| # | 任务 | 验收 |
|---|---|---|
| P0-1 | 删 `Instr::Call` 死 arm（唯一生产者 `IrBuilder::call_function` 已无调用方） | 编译通过、全量零回退 |
| P0-2 | `AddC`/`SubC` 退役：让帧真正拥有一段栈窗口（开帧时按 `stack_size` 划好，收帧自动回收） | 全量零回退；`Bookkeeping` 归零 |
| P0-3 | 删 `invoke_boundaries`；`tests/test262_runner.rs` 的 256 MB 栈恢复默认 | 全量零回退、护栏计数不增 |
| P0-4 | **拆分 `src/vm/mod.rs`（11269 行，占全库 28%）** | 见 §3 |
| P0-5 | 生成器两处自装帧与 `open_frame` 合并 | 全量零回退 |

### P1 — 消环：堆 + 句柄（M3）

**为什么先做它**：手工断环清单是事故源，且每加一个构造函数都要记着补一项。
换成真 GC 之后整套机制消失。

1. `Heap`（只分配不回收）+ `Gc<T>`（`Copy`，句柄）。
2. `HandleScope` + `Rooted`；定 builtins 签名 —— **先在一个 builtins 文件上验证，
   再铺开**（建议 `src/builtins/array.rs`，它最大，能把签名形状试出来）。
3. `Value::Object(Rc<RefCell<dyn JSObject>>)` → `Value::Object(Gc<Object>)`；
   `dyn` 消失 ⇒ trait 的方法变成具体类型的函数。
4. `borrow()` / `borrow_mut()` 全部消失（现值约 386 处）。

**验收**：功能等价；**不给性能承诺**（此时还没 GC，内存会涨）；全量零回退。

### P2 — 收集器（M4）

1. `sweep` + free list + 阈值触发。
2. **GC stress 模式**（每次分配都收集 + 释放时填 `0xDD`）跑 feature 全套，
   把所有 rooting bug 炸出来。
3. 弱引用 → `WeakMap`/`WeakSet` 从"条目永不回收"改成真回收。
4. 护栏的堆预算改读 `Heap::bytes_allocated`（现在是全局分配器的代理）。

**验收**：`BIUJS_GC_STRESS=1` 下 feature 全绿（这是真正的验收）；
循环引用（`obj.self = obj`、长原型链、闭包环）在显式收集后被回收。
⇒ 到此 `Builtins::teardown` 的断环清单可以整条删掉。

### P3 — 语义线（并行，随时可插）

按"解锁多少用例"排序：

| 任务 | 现状 | 备注 |
|---|---|---|
| `DataView` 收尾 | 186 / 101 / 274 = **40.43%**，未入册 | 剩：不带 `new` 调用要 `TypeError`；`length`/`name`/`Symbol.toStringTag` 描述符；约 79 条靠 `$262.detach`（harness 未定义）、30 条要可增长 ArrayBuffer。跨过 50% 需再 +44 条 |
| `%TypedArray%.prototype` 方法族 | 几乎全缺（`map/filter/reduce/every/indexOf/join/sort/copyWithin`） | 这是 `built-ins/TypedArray` 874 条失败的主因 |
| 闭包共享可变单元 | **语义 bug**：`counter()` 返回 `0,0,0` | 见 §4 |

### P4 — 对象形状（M6）与调用分派

- 属性存储从 `HashMap<String, Value>` 改成 shape + 内联槽（**第一次给出性能数字**）。
- 内建分派从"按名字字符串 `match`"改成 entry point（消掉 arity 表 /
  `__proto__xxx` 前缀约定 / 旁路名单）。

## 3. 拆分 `src/vm/mod.rs`（P0-4）

**实测**（2026-10-11）：11269 行，**229 个函数**。最大的几个：

| 行数 | 函数 | 说明 |
|---|---|---|
| **1740** | `run_instruction` | **指令分派 —— 一个函数顶全库 4.3%，是真正的抓手** |
| 436 | `call_native_by_name` | 内建函数按名字分派 |
| 376 | `try_array_callback_method` | `Array.prototype.{map,filter,…}` 的快速路径 |
| 343 | `proxy_static_trap` | Proxy 静态陷阱 |
| 224 | `get_global` / 210 `generator_abrupt` / 194 `set_like_method` | |
| 192 | `step` | 单步循环外壳（本身不大） |

**所以拆分的重点不是"按模块切文件"，而是先把 `run_instruction` 这一个 1740 行的
巨型 `match` 拆开。** 建议顺序（先拆函数，再挪文件）：

### 3.1 第一步：拆 `run_instruction`（1740 行）

按 `Kind` / 语义分组，把每个分支体抽成 `fn run_<group>(&mut self, inst: &Instr, …)`
放在同一文件里，让 `run_instruction` 只剩一个薄分派表。候选分组：

`Call` / `Jump` / `Return` / `Throw` / `Suspend` / `Bookkeeping` / 属性访问 /
算术 / 比较 / 对象字面量与数组 / 迭代器与 `for-of` / 生成器与 async。

**纪律**：一次只抽一组，抽完立刻跑全量；**纯移动不改逻辑**。

### 3.2 第二步：抽大函数

`call_native_by_name`（436）、`try_array_callback_method`（376）、
`proxy_static_trap`（343）各自成文件/模块。

### 3.3 第三步：按职责切文件

| 建议模块 | 内容 |
|---|---|
| `vm/frame.rs` | `Frame` / `FrameBirth` / `open_frame` / `note_frame` / `unwind_frames_to` / `frame_base` |
| `vm/exec.rs` | `step` / `run_instruction` 与各 `run_<group>` |
| `vm/call.rs` | `enter_call` / `invoke_*` / `Control` / `CallArgs` / 构造器路径 |
| `vm/exception.rs` | `SehRecord` / `handle_throw` / `throw` 路径 |
| `vm/generator.rs` | `SavedExecutionState` / 挂起恢复 / `discard_generator_frame` |
| `vm/state.rs` | `State` 与 `data_stack` / 寄存器 / `push` / `pop` / `ensure` |
| `vm/builtins_dispatch.rs` | `call_native_by_name` / `call_prototype_method` 与相关旁路名单 |

先在**同一个 crate 内**分 `mod` 文件，不要动 `pub` 边界。
**每移动一个模块跑一次全量确认零回退。**

## 4. 两条待定的语义 bug（属于"修 bug"，不是重构）

1. **闭包共享可变单元**：闭包变量是 `HashMap<String, Value>` 的创建时快照，
   所以多个闭包不共享同一个可变单元。正确实现是 scope slot。
2. **`handle_throw` 在 catch 路径多弹一次 `seh_stack`**（历史 known bug #4），
   由帧上的 try 栈按构造消除 —— 验收标准是"去掉那条 `#[ignore]` 后变绿"。

## 5. 明确不做 / 后置

- **NaN-boxing（8 B 值表示）**：判定为**可选**。它是纯性能项，不是正确性所需
  （"寄存器是帧内定长窗口"与 `Value` 是 8 B 还是 24 B 无关），且依赖"先有堆与
  句柄"。如果哪天要做，顺序是 **P1 之后、P4（对象形状）之前** —— 否则对象形状
  的字段类型要改两遍。
- **并行跑测试**：实测过，`TEST262_JOBS=4`（4 核）比串行**更慢**（512s vs 452s），
  且护栏分类会漂（timeout 7→11、memory 22→18）。原因：分片按"套件位置"切，而
  套件大小极不均衡；且内存护栏是**进程级全局分配器**，多线程会共用同一个预算。
  想真加速要先按**用例数**切匀。`TEST262_JOBS` 已留作开关，默认 1。

## 6. 每一步的验证协议

```
cargo test --release --lib            # 单元
cargo test --release --test features  # feature 断言
cargo test --release --test guards    # 护栏
./scripts/phase2-status.sh            # 全量 + 逐套件对比基线
```

**必须同时看**：头条数字（passed/skipped/failed）、护栏三计数、墙钟。
只看头条会漏掉"通过数不变但护栏漂了"这类回归（历史上真实发生过）。
