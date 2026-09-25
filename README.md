biujs
=======

A JavaScript engine implemented in Rust. Targeted to support **strict mode ES6** features but without `eval` or eval-like features or `with` statement.

## Supported Features

- Function declarations and expressions
- Class declarations and expressions with constructors
- `new` operator with constructor calls
- `this` binding (strict mode only)
- Prototype chain and method inheritance
- let variables, control flow, operators
- Objects and Arrays with prototype methods
- `JSON.parse` / `JSON.stringify` (includes `toJSON`, `replacer`, `space`, reviver)

## Conformance

**接手 / 继续开发请从 [`docs/handover.md`](docs/handover.md) 开始**（怎么跑、当前在哪、有哪些纪律），
实现问题查 [`docs/architecture.md`](docs/architecture.md)，计划与批次表查
[`docs/es6-conformance-phase2.md`](docs/es6-conformance-phase2.md)。

一条命令跑齐三级验证并与基线逐套件对比：

```sh
./scripts/phase2-status.sh            # 检查是否有回退
./scripts/phase2-status.sh --update   # 完成一批后刷新基线快照
```

Conformance is tracked against a **pinned** revision of
[tc39/test262](https://github.com/tc39/test262), recorded in `tests/test262.pin`
(currently `7e115f4`, 2026-05-21) — every pass rate in
`docs/es6-conformance-plan.md` is measured on that revision, and the runner prints
the revision it used:

```sh
git submodule update --init                     # fetch the pinned revision
cargo test --release --test test262_runner      # full run (see docs §6 for the memory guard)
cargo test --release --test test262_runner test262_at_pinned_revision   # pin drift check
```

Bumping the pin is a deliberate change; the procedure is documented in
`docs/es6-conformance-plan.md` §6.7.
