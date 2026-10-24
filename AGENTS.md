# AGENTS.md

给在本仓库工作的 AI 代理（以及新接手的人）的操作手册。项目背景、现状数字、任务计划
**不在这里复述**，看 `docs/` 下的三份主文档。

## 0. 项目一句话

用 Rust 写的 JavaScript 引擎，目标**严格模式 ES6**；解析器用 oxc（不自研）；明确
**不做** `eval` / 类 eval 特性 / `with`。库 + 一个 CLI（`biujs`，feature `cli`，默认开）。

## 1. 动手前先读

| 顺序 | 文件 | 读它做什么 |
|---|---|---|
| 1 | `docs/PROJECT.md` | 项目边界、当前数字、怎么跑、必须遵守的纪律 |
| 2 | `docs/ARCHITECTURE.md` | 当前架构（照实现写）与已知结构性弱点 |
| 3 | `docs/PLAN.md` | 下一阶段任务与验收标准 |

test262 口径、入册规则、快照纪律在 `docs/es6-conformance-phase2.md`；
GC 设计在 `docs/GC-DESIGN.md`。`docs/archive/` 是历史日志，**只用于溯源，不要照着施工**。
2026-10-24 的全量架构审查与分阶段重构计划在 `docs/ARCH-REVIEW-2026-10-24.md`。

## 2. 常用命令

```sh
cargo build --release                    # 构建
cargo run --release -- file.js           # 跑一个脚本
cargo run --release                      # REPL（从 stdin 读）
BIUJS_DUMP=1 ./target/release/biujs f.js # 打印字节码

# 三级快验（改完先跑这三条）
cargo test --release --lib               # 单元
cargo test --release --test features     # feature 断言
cargo test --release --test guards       # 护栏
cargo test --release --test bytecode_dump  # 字节码 golden（UPDATE_GOLDEN=1 重生成）

# 全量 + 与基线逐套件对比（推荐，约 7–8 分钟）
./scripts/phase2-status.sh
./scripts/phase2-status.sh --quick       # 跳全量，只跑单元 + feature
./scripts/phase2-status.sh --update      # 完成一批后刷新基线快照
```

test262 是 git submodule（约 245 MB），首次需要：

```sh
git submodule update --init
cargo test --release --test test262_runner                          # 全量
TEST262_SUITES=Map,Set cargo test --release --test test262_runner   # 定向
cargo test --release --test test262_runner test262_at_pinned_revision  # 校验 pin
```

pin 版本记录在 `tests/test262.pin`（当前 `7e115f4`）。**不要手改 submodule 内容，
不要随手升 pin**；升 pin 的流程在 `docs/es6-conformance-phase2.md` §6.7。

`phase2-status.sh` 会自己加内存上限（单片 `ulimit` 4 GB）并分 4 片串行跑。
若手动跑全量，注意分片是独立进程；并行（`TEST262_JOBS`）实测更慢且护栏分类会漂，
默认保持 1。

## 3. 仓库结构

```
src/
  main.rs             CLI + REPL
  lib.rs              库入口（Compiler / VM / Value）
  bytecode.rs         指令集与 Module/CodeBlock
  compiler/           oxc 解析 → 语义 → lowering → IR/CFG → regalloc → codegen
  vm/                 寄存器式 VM
    mod.rs            State / Frame / 值操作 / 大部分运行时语义
    exec.rs           step + run_instruction 与各 run_<group>
    gc.rs             Heap + Gc<T> 句柄 + mark-sweep 收集器
    builtins_dispatch.rs  按名字字符串的内建分派（已知设计债）
    proxy.rs          Proxy 对象与静态 trap
    object.rs / value.rs / property.rs / prototype.rs / iterator.rs / trace.rs / utf16.rs
  builtins/           20 个内建：Array / String / Object / TypedArray / Promise …
  host/ module/
tests/
  features.rs         聚合 tests/features/*.rs（新增 feature 测试要在它里面登记）
  guards.rs           护栏（timeout / step / memory）
  test262_runner.rs   test262 驱动器（SUITES / 特性门控 / 三档预算）
  test262/            固定 revision 的 submodule
scripts/phase2-status.sh   全量 + 逐套件回退检测
docs/                  项目文档与基线快照
```

## 4. 硬性纪律（都是事故换来的）

1. **崩溃优先于失败**。一条用例把进程带崩会带走整个分片的结果，比它单纯失败严重得多。
   任何"无界尺寸 → 大分配"的路径必须先变成 `RangeError`
   （`new ArrayBuffer(n)` / `new TA(n)` / `new TA({length:n})` / `ArraySpeciesCreate` 已这样处理）。
2. **release 是 `panic = "abort"`**。引擎里能返回 `Err` 就不要 `panic`；测试里也不要用
   `unwrap` 去赌一个本可恢复的错误。
3. **每步路径上的东西只读一次**。历史上在 `step()` 里每步读一次环境变量，造成 29% 墙钟
   回归、护栏漂移，而通过数完全没变。热路径里不要反复读 env / 全局注册表。
4. **验证必须同时看三个数**：头条（passed / skipped / failed）、护栏三计数、墙钟。
   只看头条会漏掉"通过数不变但护栏或耗时变坏"的回归。全量还必须看
   **逐套件无回退**（`phase2-status.sh` 的退出码就是判据）。
5. **实现 + 解锁 + 入册三件事同批**。`test262_runner.rs` 有两套特性名单：
   `OUT_OF_SCOPE_FEATURES`（范围外，永久跳过）与 `IN_SCOPE_PENDING`（范围内但未实现）。
   落实现时从 `IN_SCOPE_PENDING` 摘掉特性标签；套件不在 `SUITES` 里就同时加进去，并
   更新 `docs/phase2-status.tsv` 快照。只做实现，进度不会体现在任何数字上；只入册不
   实现，等于往池里倒垃圾（实测 +1552 条纯失败、通过数零增长）。入册前先定向跑，通过率
   **≥ 50%** 才留下。注意标签匹配是 `starts_with` 或 `contains` 的宽松匹配，增删标签前
   对照整张表检查（`"Array.prototype.flatMap".contains("Map")` 为真，历史上踩过）。
6. **结构变化与语义变化不要放在同一个提交里**。拆文件/搬家 = 纯移动，逻辑一行不改，
   每移动一个模块跑一次全量确认零回退。
7. **改动核心类型不要分批**。`Value` / `Object` / `Frame` 这类类型一次性铺开迁移，
   编译器会列出全部位点；半迁移状态成本更高。
8. **`Gc` 生命周期契约**：`Gc<T>` 无引用计数，任何 `Gc`（及其所在的 `Value`）
   **不得活过它所属的 `Heap` / `VM`**。堆归 `VM` 持有，分配一律显式传 `&mut Heap`；
   不要引入 `thread_local` 堆或 fallback 堆。库内 `eval` 用 `mem::forget(vm)` 兜住
   返回值的生命周期。
9. **`docs/archive/` 与源码注释里的历史文档引用只作出处**，不代表当前计划。
10. **一个功能一个提交**。修一个 bug、加一个特性、做一次纯结构移动，各是一个提交；
    不要把结构变化与语义变化、多个不相关的修复混在一起。唯一的例外是项目已定的
    "实现 + 解锁 + 入册三件事同批"——那是同一个特性的定义，不是多个特性。
    提交前跑三级快验；全量要求无逐套件回退、护栏计数不增。提交信息写清动机、
    影响面与验证结果；快照/文档与代码同批提交。

## 5. 代码约定

- edition **2024**；oxc 0.132；CFG 用 `petgraph`。不要自写 parser。
- `Object` 是**具体枚举**（不再有 `dyn`）。`Object` 尺寸上限由 `gc.rs` 断言守着，
  大变体必须装箱（例如 `GeneratorObject::suspended`）。
- 新增内建：在 `src/builtins/mod.rs` 注册；方法分派在
  `src/vm/builtins_dispatch.rs`（名字字符串 + arity 表 + `__proto__xxx` 前缀约定 +
  `vm_handled_static` 旁路名单），改动时四处一起看。
- 新增字节码指令：`Kind` 分类必须穷尽（`kind()` 没有兜底分支，漏登记即编译错误）；
  `src/bytecode.rs` 有元测试直接读 VM 源码，经 `vm_dispatch_sources()` 枚举文件，
  新增 dispatch 文件要同步登记。
- 字符串的 JS 可见语义按 **UTF-16 码元**；位置运算走 `src/vm/utf16.rs`，不要直接用
  Rust `String` 的字节下标或码点下标。
- Array / 类数组方法：规范要求在循环**内部**做 `HasProperty` / `[[Get]]` 的，不要先
  物化快照；同时保留 `dense_elements` 快速路径，否则大数组会明显变慢。
- 新增 feature 测试：写 `tests/features/<topic>.rs`，并在 `tests/features.rs` 里用
  `#[path = "features/<topic>.rs"] mod ...;` 登记，否则不会被编译。
- 文档与提交信息用中文没问题；提交标题沿用 `feat(scope): …` / `refactor: …` 风格。

## 6. 环境变量

| 变量 | 作用 |
|---|---|
| `TEST262_SUITES` | 逗号分隔，按子串过滤套件（定向回归） |
| `TEST262_CHUNKS` / `TEST262_CHUNK_INDEX` | 分片（每片独立进程；默认 4 片串行） |
| `TEST262_TIMEOUT_MS` | 单条用例墙钟预算；`phase2-status.sh` 默认 15000（runner 自身默认 10000），0 = 关 |
| `TEST262_STEP_LIMIT` | 单条用例指令预算，0 = 用 `DEFAULT_STEP_LIMIT` |
| `TEST262_MEMORY_MB` | 单条用例增量内存上限，默认 384，0 = 关 |
| `TEST262_JOBS` | 并行度（默认 1；实测并行更慢且护栏分类会漂，别当优化） |
| `BIUJS_DUMP` | CLI 打印编译后的字节码 |
| `BIUJS_TIMEOUT_MS` | CLI/REPL 的墙钟预算，0 = 关 |
| `BIUJS_GC_STRESS` | 每次分配都触发收集（炸 rooting bug） |
| `BIUJS_TRACE` / `BIUJS_TRACE_STEPS` / `BIUJS_TRACE_ITER` | 运行时跟踪（默认关闭，热路径只有一次原子读） |
| `BIUJS_TEST262_TRACE` | 逐条打印 test262 用例，定位崩溃用 |
| `BIUJS_TEST_MEMORY_MB` | `tests/guards.rs` 的内存预算 |

## 7. 当前方向（细节与验收看 `docs/PLAN.md`）

- **P0 收尾**：`src/vm/mod.rs` 已从 11269 行拆出 `exec.rs` / `proxy.rs` /
  `builtins_dispatch.rs`（mod.rs 现约 6.7k 行）；更细的 `frame.rs` / `call.rs` 等切分
  属可选收尾，纯移动。
- **P2 收集器**：mark-sweep + ephemeron + 阈值已接在指令边界（`step()` 开头的安全点；
  `alloc()` 内部绝不触发）。`invoke_boundaries` 非空时跳过收集 —— 原生回调把 `Value`
  放在 Rust 局部变量里，收集器看不见；先接宿主侧入根（`Scope` / `Roots`）才能撤掉
  这道闸门。验收是 `BIUJS_GC_STRESS=1` 下 feature 全绿。
- **P3 语义线**：`DataView` 收尾、`%TypedArray%.prototype` 方法族、UTF-16 存储残留
  （Rust `String` 装不下孤立代理；彻底修要把 `Value::String` 换成 UTF-16 / WTF-8）。
- **明确不做 / 后置**：NaN-boxing（纯性能项，依赖堆与句柄）；并行跑 test262
  （实测更慢且护栏分类漂移）。
