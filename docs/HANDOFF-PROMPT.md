# 给后来者的提示词

下面这段可以直接复制粘贴给下一个接手的人（或新会话）。它把"从哪读起、现在在哪、
下一步做什么、有哪些坑"一次性说清，省去重新摸索。

---

你接手的是一个用 Rust 写的 JavaScript 引擎 `biujs`（仓库根目录 `/home/alex/code/rust/biujs`），
目标是严格模式 ES6，明确不做 `eval` / 类 eval 特性 / `with`。当前 test262 通过率
**84.06%**（16567 / 执行 19708）。

**第一步：先读这三份文档，再动手。**

1. `docs/PROJECT.md` —— 项目说明、当前数字、怎么跑、**五条必须知道的纪律**
2. `docs/ARCHITECTURE.md` —— 当前架构（照实现写的，不是照理想写的）**与六条已知结构性弱点**
3. `docs/PLAN.md` —— 下一阶段任务计划（P0 收尾减负 / P1 堆与句柄 / P2 收集器 /
   P3 语义线 / P4 对象形状），每条都带了验收标准

`docs/archive/` 里有 11 份历史文档（操作日志与旧方案），**只用来溯源，不要照着施工** ——
它们描述的状态已经过时。源码注释里若引用了 `docs/archive/xxx.md §N`，那是历史出处，
不是当前计划。

**当前最该做的（按优先级）：**

- P0-4：拆分 `src/vm/mod.rs`（原 11269 行，已拆到 `mod.rs` 6288 + `exec.rs` 3404 + `proxy.rs` 748
  + `builtins_dispatch.rs` 1083）。切分方案与拆分现状见 `docs/PLAN.md` §3。纪律：**纯移动不改逻辑，
  每移动一个模块跑一次全量确认零回退**（更细的 `frame.rs` / `call.rs` 等切分属收尾，可顺手做）。
- P1：把 `Rc<RefCell<dyn JSObject>>` 换成 `Heap` + `Gc<T>` 句柄。动机很实在：现在没有 GC，
  环要靠 `Builtins::teardown()` 里一条**写死的 23 项清单**手工断，漏一项就会每条 test262
  用例漏一整个 realm，最终撞穿 4GB `ulimit` 让进程 SIGABRT（真实发生过）。
  **先在一个 builtins 文件上验证再铺开**（建议 `src/builtins/array.rs`）。
- P3 语义线：`DataView` 收尾（现 40.43%，未入册）与 `%TypedArray%.prototype` 方法族。

**几条硬约束（都是事故换来的，务必遵守）：**

1. **崩溃优先于失败**。一条用例把进程带崩会带走整个分片的结果，比它单纯失败严重得多。
   任何"无界尺寸 → 大分配"的路径必须先变成 `RangeError`。
   ⚠️ `[profile.release]` 是 `panic = "abort"`：release 下 panic 直接中止进程，
   所以引擎里"能返回 `Err` 就不要 panic"。
2. **每步路径上的东西只读一次**。曾经为了调试在 `step()` 里每步读一次环境变量，结果是
   29% 墙钟回归、护栏从 7/0/22 漂到 11/0/18 —— **而通过数完全没变**。
3. **验证必须同时看三个数**：头条（passed/skipped/failed）、护栏三计数、墙钟。
   只看头条会漏掉上面那类回归。
4. **入册纪律**：先量（加进 `SUITES`、摘掉特性名、跑定向回归），通过率 ≥50% 才留下，
   并同批更新快照与 `EXPECTED_SKIPPED`。实现 + 解锁 + 入册要同批做。
5. **改动核心类型不要分批**（编译器会列全位点，半迁移成本更高）。
6. **结构变化与语义变化不要放在同一个提交里**。

**怎么验证每一步：**

```sh
cargo test --release --lib            # 单元 204
cargo test --release --test features  # feature 断言 538
cargo test --release --test guards    # 护栏 7
./scripts/phase2-status.sh            # 全量 + 与基线逐套件对比（约 450s）
./scripts/phase2-status.sh --update   # 完成一批后刷新基线快照
```

全量跑完必须看到"无逐套件回退"，且护栏计数不增（基线：timeout 7 / step-limit 0 / memory 22）。

**已经判定不做的**：NaN-boxing（8B 值表示）是纯性能项，不是正确性所需，且依赖"先有堆与
句柄"；并行跑测试实测更慢（512s vs 串行 452s）且会让护栏分类漂移。理由都写在 `docs/PLAN.md` §5。
