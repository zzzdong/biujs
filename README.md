biujs
=======

A JavaScript engine implemented in Rust. Targeted to support **strict mode ES6** features but without `eval` or eval-like features or `with` statement.

## Supported Features

- Function declarations and expressions
- Class declarations and expressions with constructors
- `new` operator with constructor calls
- `this` binding (strict mode only)
- Prototype chain and method inheritance
- Closures: a captured binding is a **shared cell**, so `counter()` really counts
- Tagged templates (`` tag`…` ``) with the per-site frozen template object (so `String.raw` works)
- let variables, control flow, operators
- Objects and Arrays with prototype methods
- `JSON.parse` / `JSON.stringify` (includes `toJSON`, `replacer`, `space`, reviver)
- Generators / async, `Promise`, `Proxy` / `Reflect`, `Map` / `Set`, `WeakMap` / `WeakSet`
- `ArrayBuffer` / TypedArray views / `DataView`

## 接手 / 继续开发

**从这里开始，按顺序读三份：**

| 文档 | 内容 |
|---|---|
| [`docs/PROJECT.md`](docs/PROJECT.md) | 项目说明、当前数字、怎么跑、几条必须知道的纪律 |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | 当前架构（照实现写）与已知结构性弱点 |
| [`docs/PLAN.md`](docs/PLAN.md) | 下一阶段的任务计划与优先级 |

test262 口径 / 批次表 / 快照纪律在
[`docs/es6-conformance-phase2.md`](docs/es6-conformance-phase2.md)。
历史决策与操作日志已归档到 `docs/archive/`（11 份，仅供溯源，不再照着施工）。

## Conformance

<!-- doc-numbers -->**84.39%**（17304 / 执行 20504；跳过 8577；2026-10-26 基线，TEST262_MEMORY_MB=384）<!-- /doc-numbers -->。

一条命令跑齐三级验证并与基线逐套件对比：

```sh
./scripts/phase2-status.sh            # 检查是否有回退
./scripts/phase2-status.sh --update   # 完成一批后刷新基线快照
```

快验三档：

```sh
cargo test --release --lib            # 单元 238
cargo test --release --test features  # feature 断言 616
cargo test --release --test guards    # 护栏 7
```

Conformance is tracked against a **pinned** revision of
[tc39/test262](https://github.com/tc39/test262), recorded in `tests/test262.pin`
(currently `7e115f4`, 2026-05-21) — every pass rate in
`docs/es6-conformance-phase2.md` is measured on that revision, and the runner prints
the revision it used:

```sh
git submodule update --init                     # fetch the pinned revision
cargo test --release --test test262_runner      # full run (see docs §6 for the memory guard)
cargo test --release --test test262_runner test262_at_pinned_revision   # pin drift check
```

Bumping the pin is a deliberate change; the procedure is documented in
`docs/es6-conformance-phase2.md` §6.7.

> ⚠️ `[profile.release]` 是 `panic = "abort"`。release 下 panic 会直接中止进程，
> 所以引擎里"能返回 `Err` 就不要 panic" —— 详见 `docs/PROJECT.md` §5。
