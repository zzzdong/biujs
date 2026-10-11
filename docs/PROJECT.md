# biujs 项目说明

一份**当前实现状态**的说明（2026-10-11 重写）。历史决策与操作日志在 `docs/archive/`。

## 1. 这是什么

用 Rust 写的 JavaScript 引擎，目标是**严格模式 ES6**，明确**不做** `eval` / 类 eval 特性 / `with`。

- crate 名 `biujs`，版本 `0.1.0-alpha.0`，edition 2024
- 库（`src/lib.rs`）+ 一个 CLI 二进制 `biujs`（feature `cli`，默认开）
- 解析器用 **oxc**（`oxc_parser` / `oxc_ast` / `oxc_semantic` 等 0.132），不自己写 parser；
  CFG 用 `petgraph`

## 2. 目标与边界

| | |
|---|---|
| 语言目标 | 严格模式 ES6 |
| 明确不做 | `eval`、类 eval 特性、`with` |
| 已入册的 test262 套件 | `language/expressions`、`language/statements`、`built-ins/{Array,Boolean,Error,Object,String,Symbol}` 等（见 `docs/es6-conformance-phase2.md` §2.2） |
| 未入册但已实现 | `TypedArray` 视图族、`DataView`、`Proxy`、`Reflect` |

## 3. 当前数字（2026-10-11，基线）

| 指标 | 值 |
|---|---|
| test262 执行 / 通过 / 跳过 / 失败 | 19708 / **16567** / 7843 / 3141 |
| 通过率 | **84.06%** |
| 单元 / feature / 护栏 | 211 / 538（4 ignored）/ 7 |
| 护栏计数 | timeout 7、step-limit 0、memory 22（`TEST262_MEMORY_MB=384`，见 §5.7） |
| 全量墙钟 | 约 450s（串行 4 分片，单片 `ulimit -v` 4 GB） |
| 源码规模 | 45 个 `.rs`，约 40.7k 行 |

## 4. 怎么跑

```sh
# 三级快验
cargo test --release --lib           # 单元 204
cargo test --release --test features # feature 断言 538
cargo test --release --test guards   # 护栏 7

# 全量：跑齐三级 + 与基线逐套件对比（推荐）
./scripts/phase2-status.sh
./scripts/phase2-status.sh --update  # 完成一批后刷新基线快照

# 单条脚本
cargo run --release -- file.js
BIUJS_DUMP=1 ./target/release/biujs file.js   # 打印字节码
```

`test262` 是 245 MB 的 git submodule，缺失时 runner 降级跳过，`exclude` 里也排除了它。

## 5. 几条必须知道的纪律

这几条是从实际事故里长出来的，不是风格偏好：

1. **崩溃优先于失败**。一条用例把进程带崩（`SIGABRT`、OOM）会带走整个分片的结果，
   比它单纯失败严重得多。所以任何"无界尺寸 → 大分配"的路径都必须先变成
   `RangeError`（ES 允许：`AllocateArrayBuffer` 说分配不出来就抛 `RangeError`）。
   已经这么处理过的：`new ArrayBuffer(n)` / `new TA(n)` / `new TA({length:n})` /
   `ArraySpeciesCreate` 的结果数组。
   ⚠️ `[profile.release]` 是 **`panic = "abort"`**：release 下 panic 会直接中止进程，
   所以测试里"能返回 `Err` 就不要 panic"。
2. **每步路径上的东西只读一次**。曾经为了调试在 `step()` 里每步 `env::var_os(...)`，
   结果是 29% 的墙钟回归、护栏从 7/0/22 变成 11/0/18 —— 而通过数完全没变。
   看头条数字会漏掉这类回归，必须同时看护栏与墙钟。
3. **先量后改**。想入册一个套件，先把套件加进 `SUITES`、摘掉特性名，跑一次定向回归
   看通过率；达到 ≥50% 才留下，并同批更新快照与 `EXPECTED_SKIPPED`。
4. **实现 + 解锁 + 入册三件事同批**。只做后两件等于往池里倒垃圾。
5. **结构变化与语义变化不要放在同一个提交里**。
6. **改动核心类型不要分批**：半迁移状态的成本大于一次铺开（编译器会列出全部位点）。
7. **`TEST262_MEMORY_MB` 现在是 384，不是 256**。P1 之后堆只在 VM 析构时整片释放、
   单次 `run` 内不回收，单用例内存峰值上升。实测同一次改动下：256 MB → 通过 16562 /
   memory 30（比基线 −5）；384 MB → 通过 16567 / memory 25（与基线逐项相同、无逐套件
   回退）。这是**已量化的预期漂移**，不是掩盖。
   **收集器上线后**（`docs/GC-DESIGN.md`）：384 MB 下护栏已回到 timeout 7 / memory 22
   ——即本文 §3 记的项目基线。但 256 MB 下仍差 5 条（memory 27、`built-ins/Array` −5），
   所以预算维持 384。差的那 5 条要靠"宿主侧入根"（把 `Scope` 接进原生内建路径）才能补上。

## 6. 文档地图

| 文件 | 用途 |
|---|---|
| `README.md` | 入口 |
| **`docs/PROJECT.md`**（本文） | 项目说明、现状数字、纪律 |
| **`docs/ARCHITECTURE.md`** | 当前架构（照实现写，不是照理想写） |
| **`docs/PLAN.md`** | 下一阶段的任务计划 |
| **`docs/GC-DESIGN.md`** | 堆与 GC 设计：槽布局 / 句柄契约 / 根集 / 安全点 / 不变量 / unsafe 清单 |
| **`docs/REFACTOR-HEAP-GC.md`** | P1 堆与句柄重构的执行计划（阶段 A/B/C，每步验收与回退） |
| `docs/es6-conformance-phase2.md` | test262 口径、批次表、快照纪律（仍在使用） |
| `docs/phase2-status.tsv` | 逐套件基线快照（`--update` 刷新） |
| `docs/archive/` | 11 份历史文档（操作日志、旧方案），仅供溯源 |
