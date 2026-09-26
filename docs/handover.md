# 接手须知（从这里开始）

> 目标读者：第一次接手本项目的人。读完这一页（约 5 分钟）你应该知道：跑什么命令、读哪些文档、
> 当前做到哪、以及动代码前必须知道的三条纪律。

---

## 1. 这是什么

一个 Rust 写的 JavaScript 引擎，目标是 **strict mode ES6**，**不支持** `eval` / `new Function` / `with`
（静态编译模型的前提，永久排除）。一致性用 tc39/test262 的**钉住修订**衡量（`tests/test262.pin`）。

## 2. 五分钟上手

```sh
cargo build --release
cargo test --release --lib                 # 190 个单元测试
cargo test --release --test features       # 421 条语义断言
echo 'console.log' > /dev/null; printf 'var a=[1,2]; a.map(function(x){return x*2;}).join(",")\n' > /tmp/t.js
./target/release/biujs /tmp/t.js           # 跑一段 JS
BIUJS_DUMP=1 ./target/release/biujs /tmp/t.js   # 打印字节码（看帧布局时必用）
```

KPI 归类（把"范围内真实缺口"和范围外杂音分开，A1 的测量口径见计划书 §3.1）：

```sh
ulimit -v 6000000
TEST262_FAILURES=99999 cargo test --release --test test262_runner -- --nocapture > /tmp/full.txt
python3 scripts/kpi-noise.py /tmp/full.txt     # 3807 范围内 / 100 RegExp / 176 后 ES6（2026-09-26）
```

护栏（`tests/test262_runner.rs` 顶部有完整表格）：`TEST262_TIMEOUT_MS`（默认 15000，0 = 关）、
`TEST262_STEP_LIMIT`（默认 2×10^7）、`TEST262_MEMORY_MB`（默认 256，**单条用例的增量**）。
`TEST262_TIMINGS=1` 会打印最慢的十条用例，调默认值时用它而不是猜。

全量一致性回归（约 3 分钟，**必须带内存上限**）：

```sh
./scripts/phase2-status.sh                 # 推荐：跑齐三级验证，并与基线快照逐套件对比
./scripts/phase2-status.sh --quick         # 只跑单元 + feature
./scripts/phase2-status.sh --update        # 完成一批后，把新基线写回快照并一起提交
```

## 3. 读什么（顺序很重要）

| 顺序 | 文档 | 解决什么问题 |
|------|------|--------------|
| 1 | **本文件** | 怎么跑、当前在哪、有哪些纪律 |
| 2 | `es6-conformance-phase2.md` | **当前有效的计划**：目标 A1–A10、批次表 B0–B13、批次记录 §6.1、度量口径 §2 |
| 3 | `architecture.md` | **实现地图**：管线、运行时模型、**任务→改哪里**、**地雷与不变量** |
| 4 | `es6-feature-support.md` | 逐特性支持矩阵（给使用者看的口径） |
| 5 | `es6-conformance-plan.md` | M0–M4 的工作日志（**已冻结**，只在需要追溯"某个结论怎么来的"时查） |
| — | `biu-js-engine-architecture.md` | 早期**设计蓝图**。注意其中"排除 `var`/`arguments`"等表述已过时，实现问题以 `architecture.md` 为准 |

## 4. 当前状态（2026-09-26）

| 指标 | 数值 |
|------|------|
| test262 执行 / 通过 / 失败 / 跳过 | 18277 / **14555** / 3722 / 8133 |
| 通过率 | 79.64%（参考值；分母口径见计划书 §2.1） |
| 单元 / feature / 护栏测试 | 190 / 478 / 7，全绿 |
| 全量耗时 | 约 3m（三层护栏封顶：单例 15s 墙钟、2×10^7 指令、256MiB 堆增量） |
| runner 覆盖面 | 25586 / 53568 个测试文件（47%）——**未覆盖里约 4920 条属承诺的 M5/M6** |
| 基线快照 | `phase2-status.tsv`（`scripts/phase2-status.sh` 生成的逐套件表） |

**已完成**：M0 → M4（语法、内置对象、JSON、生成器与迭代协议）；阶段二的 B0（度量铺底）、B1（spread 走 `GetIterator`）、B5a（数组 `length` 的错误种类）、B5b（Object 描述符长尾 + 六个完整性方法）、B2a（`Map` 180/187）、B2b（`Set` 344/364，含 `set-methods` 七算子）、B3（`WeakMap` 131/132、`WeakSet` 79/80）、B4（KPI 归类脚本 + `Date` 判"实现"→ B14）、B15a（`for-of`/`for-in` 头部的**赋值模式**此前整族不赋值，+177）、B15b（**标签语句**此前被整段丢弃，+24）、B16（**`rest` 参数**在所有函数形态下都没绑定，+15；同时修掉 `SUITES` 里一个静默的覆盖空洞）、B17（生成器参数改为**调用时绑定**，+88）、B18（**`var` 的函数作用域与提升**，+62）、B19（`try` 内的 `break`/`continue` 曾跳到指令流之外，+6）。

**下一步**：**B19 —— `try` 内 abrupt 完成**（`break`/`continue` 从 `try` 里跳出会跳错地方，基线上即可复现）：

```js
var log = [];
for (var x of [1]) { try { log.push("a"); break; } catch (e) {} log.push("b"); }
log.push("c");
throw log.join(",");        // node: "a,c"；本站：返回 1，后面的语句根本没跑
```

它是控制流/SEH 的正确性问题，B18 有 2 条用例正被它遮挡。之后按 §6.1 B18 残留的顺序：
**TDZ**（`let`/`const`/类名绑定，约 20+ 条）→ **per-iteration 绑定** → **闭包共享可变单元**。§3.1 的标签分布给出了更精确的优先级 —— 范围内失败里 **解构 974 / 生成器 611 / class 元素 489** 三族占了一半以上，建议按这个顺序拆，而不是按原来的 B7/B8 顺序。`Date` 已判"实现"，落点 B14。
另有四条已定位、量不小的独立根因，可与 B15b 并行挑选：

- **`Array.from` 不走迭代协议**（`builtins/array.rs::array_from` 只按 `length` + 索引取快照）。ES6 范围内，`built-ins/Array/from` 有 30 条同源失败，也是 `Map.groupBy` 那 4 条失败的最后一环。修它要上移 VM（要 `GetMethod(@@iterator)` 与 `next()`）。**注意先看闭包债**：那一族里有多条用例靠"闭包内自增外部标量"驱动迭代器，改完协议它们会从"快速失败"变成撞步数上限（实测 `Set` 的同类用例就是这样）。
- **缺全局对象**：脚本顶层的 `this` 是 `undefined`（`State::this_val` 初值），全局变量又活在 `State::globals` 的 `HashMap` 里 —— 这不是一行修得掉的：`this` 要真的拥有属性，全局读写就得改走对象 + 原型链。实测仅 `built-ins/Object` 里就有 40+ 条失败是被它拖住的（`var global = this` 一类写法）。
- **数组字面量的省略元素不出 hole**：`[0,,2].hasOwnProperty("1")` 现在是 `true`（降级时把省略元素当成 push `undefined`）。需要给 `lower_array` 的 `Elision` 分支一条能在运行时标记 hole 的指令（五层改动，比这条纪律本身贵）。

## 5. 每批的固定动作

1. **先读**该批在计划书 §5 的任务表（规模、依赖、验收）。
2. 动手前确认根因，别按"应该是什么"猜 —— 本项目多次出现"以为是大缺口、实际是一行 `map_err` 丢类型"（§2.1ab/§2.1ac/B5a）。
3. 补 feature 断言（**每批 ≥ 5 条**，写进 `tests/features/` 对应文件）。
4. **跑 `./scripts/phase2-status.sh`**：三级验证 + 逐套件对比。有回退就解释或回退，不要带着回退提交。
5. 在计划书 §6.1 写批次记录：起点数字、根因、改动、效果表、逐套件核对结论、残留。**残留必须写**，它们是下一批的输入。
6. `--update` 刷新快照，和本批改动一起提交。
7. 若这批**交付了某个特性**，按下面第 2 条纪律同批解锁。

## 6. 三条不可违反的纪律

1. **主指标是通过的绝对数，但"总数不降"是必要不充分条件。** 逐套件对比才能发现"修好 A 弄坏 B"。别省这一步。
2. **交付特性 = 实现 + 解锁 + 入册，三件事同批。** 未实现的特性记在 runner 的 `IN_SCOPE_PENDING` 里（`tests/test262_runner.rs`）；实现后必须从那里删掉，必要时同时把套件加进 `SUITES`。只做第一件等于白干（进度不进任何数字），只做后两件等于往池里倒垃圾 —— 实测过：单加 M5/M6 的 11 个套件，通过数一条不变、只多出 1552 条纯失败。
3. **引擎崩溃优先于失败，挂死同样优先于失败。** 无界递归会以 SIGABRT 结束整个回归且不留线索（用 `BIUJS_TEST262_TRACE=1` 逐条打印路径定位元凶）；无界循环则会把 3 分钟的全量拖成 15 分钟（实测一条用例烧 60s）。三层护栏（§7.1：墙钟 / 步数 / 堆增量）让它变成一条普通失败，`tests/guards.rs` 覆盖它们本身。任何新批次的第一次全量都要确认进程走完了全程、且 `guards:` 行没有异常计数。

## 7. 动代码前先看这些地雷（详见 `architecture.md` §4）

- **错误种类会被 `String` 通道抹平**：`define_property` / `property_set` / `prototype::*` 的失败是 `Result<_, String>`，需要 `RangeError` 的场合得用 `into_property_error` / `from_property_error`。
- **builtin 层看不到原型链与访问器**：凡语义要求按 `[[Get]]`/`[[Set]]` 取值写入、或可能跑用户代码，都必须上移 VM。
- **帧深度必须在装帧之前取**（`invoke_boundaries`）。这条教训出现过四次。
- **`invoke` 必须镜像 `Call` opcode 的特例**（生成器、构造器…）。
- **新增一条 VM 指令是五层改动**（`bytecode.rs` / `ir/instruction.rs` / `ir/builder.rs` / `codegen.rs` / `vm/mod.rs`，外加 `ssabuilder.rs`）。
- **寄存器是全局的**，挂起/恢复必须显式快照。
- **闭包捕获是创建时值快照**（`dyn-capture.md`），"在闭包里累加"不成立。
- **脚本完成值不可靠**：写 feature 测试别靠末尾表达式取值，改成在 JS 里 `throw` 断言。
