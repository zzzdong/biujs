#!/usr/bin/env bash
#
# 阶段二进度跟踪 / 回退检测。
#
#   ./scripts/phase2-status.sh             跑三级验证 + 全量回归，与基线快照逐套件对比
#   ./scripts/phase2-status.sh --update    同上，并把新表写回 docs/phase2-status.tsv
#   ./scripts/phase2-status.sh --quick     只跑单元测试与 feature 测试（跳过 3 分钟的全量）
#
# 退出码：0 = 没有回退；1 = 有回退或验证失败；2 = 环境问题（缺基线快照等）。
#
# 为什么需要它：`cargo test` 只告诉你总数。而"通过数不低于上一提交"是必要不充分条件 ——
# 一批语义改动可能同时修好 A 与弄坏 B，总数持平甚至上升。逐套件对比是本项目的纪律
# （见 docs/es6-conformance-phase2.md §7.1）。
set -uo pipefail

cd "$(dirname "$0")/.."

SNAP="docs/phase2-status.tsv"
# §6.5：全量回归一律带内存上限（OS 侧的硬兜底）。
# 逐用例泄漏已修（B37）—— `VM::run` 现在重置了一次运行触及的**全部**状态
# （迭代器/生成器注册表、委托栈、前导标志），并且 realm 的 `Rc` 环会在 `run` 与析构时被断开，
# 于是"每个程序一个 VM"不再漏掉整套 `Builtins` 图。
#
# 2500000（2.5GB）**不够**（2026-10-09 实测，deep-arch-log §43.3）：分片跑到
# `built-ins/Array/prototype/map/15.4.4.19-3-28.js`（`length: 4294967296` 的类数组）时，
# 那条 384MiB 的分配会失败 → 整片 SIGABRT → 脚本判定"没有跑完"、丢掉整轮结果。
# 与本仓库的改动无关：在 `1dbee97`（本批之前）上同样复现，4 片与 8 片都复现。
# 提到 4GB 后 HEAD 与改动后都能跑完，且都精确复现已提交基线（含 `built-ins/Array`
# 的 2113/113/855）。实测该套件峰值 RSS 1.91GB，而片是**串行**跑的 ⇒ 不会招致 OOM killer。
# 可用 MEM_LIMIT_KB 覆盖。
: "${MEM_LIMIT_KB:=4000000}"
# 片数：每片一个进程。4 片时单片峰值约为整轮的 1/3，上面的上限有充足余量。
: "${TEST262_CHUNKS:=4}"
# 三层护栏（见 tests/test262_runner.rs 顶部）：每条都由 runner 变成一条**普通失败**，
# 而不是让进程挂死或被杀（后者会丢掉整轮结果）。
#   TEST262_TIMEOUT_MS  单条用例的墙钟预算（0 = 关）
#   TEST262_STEP_LIMIT  单条用例的指令预算（0 = 关）
#   TEST262_MEMORY_MB   单条用例的增量上限（0 = 关）
: "${TEST262_TIMEOUT_MS:=15000}"   # 实测第 10 慢的用例 2.2s（更慢的 9 条基线里本已失败）
: "${TEST262_STEP_LIMIT:=0}"        # 0 交给 runner 用 DEFAULT_STEP_LIMIT
: "${TEST262_MEMORY_MB:=256}"        # 实测 64MiB 阈值下只有 9 条越线，且都本已失败
# 整轮再套一层硬超时：护栏本身出问题时，至少不让脚本无限等下去。
: "${TEST262_HARD_TIMEOUT:=1200}"
export TEST262_TIMEOUT_MS TEST262_STEP_LIMIT TEST262_MEMORY_MB
UPDATE=0
QUICK=0
for arg in "$@"; do
  case "$arg" in
    --update) UPDATE=1 ;;
    --quick)  QUICK=1 ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 快照里的 `#` 注释行不能参与 join（否则会被当成"消失的套件"）
strip_comments() { grep -v '^[[:space:]]*#' "$1"; }

hr() { printf '%s\n' "────────────────────────────────────────────────────────"; }

# ── 1. 构建 ────────────────────────────────────────────────────────────────
echo "== 构建 =="
if ! cargo build --release 2>&1 | tail -3; then
  echo "构建失败，停止。" >&2
  exit 1
fi

# ── 2. 单元测试 ────────────────────────────────────────────────────────────
echo
echo "== 单元测试 =="
if ! cargo test --release --lib 2>&1 | tee "$TMP/lib.txt" | grep -E "^test result"; then
  echo "单元测试失败，停止。" >&2
  exit 1
fi
grep -q "test result: ok" "$TMP/lib.txt" || { echo "单元测试有失败。" >&2; exit 1; }

# ── 3. feature 集成测试 ────────────────────────────────────────────────────
echo
echo "== feature 集成测试 =="
if ! cargo test --release --test features 2>&1 | tee "$TMP/features.txt" | grep -E "^test result"; then
  echo "feature 测试失败，停止。" >&2
  exit 1
fi
grep -q "test result: ok" "$TMP/features.txt" || { echo "feature 测试有失败。" >&2; exit 1; }

# ── 3b. 护栏测试（§7.2：超时 / 步数 / 堆预算）────────────────────────────────
echo
echo "== 护栏测试（超时 / 步数 / 堆预算）=="
if ! cargo test --release --test guards 2>&1 | tee "$TMP/guards.txt" | grep -E "^test result"; then
  echo "护栏测试失败，停止。" >&2
  exit 1
fi
grep -q "test result: ok" "$TMP/guards.txt" || { echo "护栏测试有失败。" >&2; exit 1; }

if [ "$QUICK" = "1" ]; then
  echo
  echo "--quick：跳过全量回归。"
  exit 0
fi
# ── 4. 全量 test262 ────────────────────────────────────────────────────────
# 分片跑：每片一个进程（`TEST262_CHUNKS` / `TEST262_CHUNK_INDEX`），片结束进程退出、
# 内存随之释放。为什么：整轮（101 个套件）在一个地址空间里会把峰值推到 OOM killer 或
# 撞上 ulimit；分片后每片都远低于上限，于是上限本身也能调小。
echo
echo "== 全量 test262（分 ${TEST262_CHUNKS} 片跑；每片内存上限 ${MEM_LIMIT_KB}KB）=="
echo "   护栏：单例 ${TEST262_TIMEOUT_MS}ms / ${TEST262_STEP_LIMIT:-默认} 步 / 堆 ${TEST262_MEMORY_MB}MB，每片硬超时 ${TEST262_HARD_TIMEOUT}s"

CHUNK_FILES=""
RUN_STATUS=0
INDEX=0
# 并行跑各片（并发度 `TEST262_JOBS`，默认核数）。
#
# 每片仍然是**独立进程** —— 这一点是刻意保留的，不是偷懒：内存护栏是一个**进程级
# 全局分配器**（`BUDGET_BYTES` 按用例装填，`ALLOCATOR.live` 全进程计数，预算是"当前
# 活跃字节 + 每用例额度"的增量）。改成多线程 worker 会让所有 worker 共用同一个预算和
# 同一个活跃计数，于是 A 用例的分配被算到 B 用例头上 —— 护栏要么误报要么漏报。
# 进程隔离本来就是这套护栏成立的前提，所以并行只能加在进程这一层。
# 默认 **串行**（`TEST262_JOBS=1`）：实测 4 片并发（4 核）**更慢** —— 512s vs 串行
# 452s，而且护栏计数漂了（timeout 7→11、memory 22→18；头条数字 16567/7843/3141
# 两次一致）。两个原因：
#   1. 分片是按"套件在 `SUITES` 里的位置"切的，而套件大小极不均衡（`built-ins/Array`
#      一家三千多条），所以并发的墙钟≈最大的那一片，大部分核在等这一片；
#   2. 4 个进程抢 CPU，靠墙钟判定的 timeout 护栏开始误伤 —— 于是"内存护栏"里的
#      用例改在"超时护栏"里失败。头条不变，但护栏分类不再可比。
# 想真加速，得先把工作量切匀（按用例数而不是按套件位置切），再谈并发。
JOBS="${TEST262_JOBS:-1}"
# 直接跑编译好的二进制，而不是 `cargo test`：并发调用 cargo 会抢 target 目录的
# 构建锁，实测会退化成串行 —— 并行白做。
cargo test --release --test test262_runner --no-run >/dev/null 2>&1
RUNNER_BIN="$(ls -t target/release/deps/test262_runner-* 2>/dev/null | grep -v '\.d$' | head -1)"
if [ -z "$RUNNER_BIN" ]; then
  echo "找不到 test262_runner 二进制（先 cargo test --release --test test262_runner --no-run）。" >&2
  exit 1
fi
while [ "$INDEX" -lt "$TEST262_CHUNKS" ]; do
  OUT="$TMP/full-chunk-$INDEX.txt"
  CHUNK_FILES="$CHUNK_FILES $OUT"
  ( ulimit -v "$MEM_LIMIT_KB"
    timeout --signal=TERM "$TEST262_HARD_TIMEOUT" \
      env TEST262_FAILURES=0 TEST262_CHUNKS="$TEST262_CHUNKS" TEST262_CHUNK_INDEX="$INDEX" \
        "$RUNNER_BIN" --nocapture
    # 后台作业的退出码只能这样回收：`wait` 拿到的是汇总值。
    echo $? > "$OUT.status" ) > "$OUT" 2>&1 &
  INDEX=$((INDEX + 1))
  while [ "$(jobs -rp | wc -l)" -ge "$JOBS" ]; do
    wait -n 2>/dev/null || true
  done
done
wait

# 收尾检查：并发时退出码统一在这里判读（与串行时逐片判断的行为一致）。
INDEX=0
while [ "$INDEX" -lt "$TEST262_CHUNKS" ]; do
  OUT="$TMP/full-chunk-$INDEX.txt"
  STATUS="$(cat "$OUT.status" 2>/dev/null || echo 1)"
  if [ "$STATUS" = "124" ] || [ "$STATUS" = "143" ]; then
    echo "第 $INDEX 片超过硬超时 ${TEST262_HARD_TIMEOUT}s 被中止。" >&2
    echo "先看卡在哪个用例：TEST262_TIMINGS=1 复跑看最慢的十条，或 BIUJS_TEST262_TRACE=1 看最后一行。" >&2
    tail -20 "$OUT" >&2
    exit 1
  fi
  if ! grep -q "^TOTAL" "$OUT"; then
    echo "第 $INDEX 片没有跑完（进程可能被内存上限打断或崩溃）。" >&2
    echo "最后 20 行：" >&2
    tail -20 "$OUT" >&2
    exit 1
  fi
  if [ "$STATUS" != "0" ]; then
    echo "第 $INDEX 片的 test262_runner 退出码 $STATUS，但摘要已生成 —— 请检查是否有套件报告失败以外的异常。" >&2
    RUN_STATUS="$STATUS"
  fi
  INDEX=$((INDEX + 1))
done
# 后面的 grep 仍按整轮输出处理：把各片拼起来。
cat $CHUNK_FILES > "$TMP/full.txt"

# 摘要表 → TSV（表头下面到分隔线之间的行）。各片的行直接合起来即为整轮。
awk '/^suite[[:space:]]+passed/{f=1; next} /^-{10,}/{f=0} f && NF>=4 {print $1"\t"$2"\t"$3"\t"$4}' \
  $CHUNK_FILES > "$TMP/new.tsv"

# 头条数字：整轮 = 各片之和（每片的 TOTAL 只统计自己那部分套件）。
SUMS="$(awk '/^[[:space:]]*TOTAL[[:space:]]/{p+=$2; s+=$3; f+=$4} END{print p+0, s+0, f+0}' \
  $CHUNK_FILES)"
PASSED="$(echo "$SUMS" | awk '{print $1}')"
SKIPPED="$(echo "$SUMS" | awk '{print $2}')"
FAILED="$(echo "$SUMS" | awk '{print $3}')"
EXECUTED=$((PASSED + FAILED))
RATE="$(awk -v p="$PASSED" -v e="$EXECUTED" 'BEGIN{printf "%.2f", (e ? p * 100 / e : 0)}')"
GUARD_SUMS="$(awk '/^[[:space:]]*guards:/{gsub(/,/, ""); for (i = 1; i <= NF; i++) {
                     if ($i == "timeout") t += $(i+1);
                     else if ($i == "step-limit") s += $(i+1);
                     else if ($i == "memory") m += $(i+1);
                   }} END{print t+0, s+0, m+0}' \
  $CHUNK_FILES)"
echo
hr
printf '  TOTAL                                              %s    %s    %s\n' "$PASSED" "$SKIPPED" "$FAILED"
echo "  pass rate over executed tests: ${RATE}% (${PASSED} / ${EXECUTED})"
printf '  guards: timeout %s, step-limit %s, memory %s   (TEST262_TIMEOUT_MS / TEST262_STEP_LIMIT / TEST262_MEMORY_MB, 0 = off)\n' \
  $(echo "$GUARD_SUMS" | awk '{print $1}') $(echo "$GUARD_SUMS" | awk '{print $2}') $(echo "$GUARD_SUMS" | awk '{print $3}')
echo "  执行 $EXECUTED = 通过 $PASSED + 失败 $FAILED；跳过 $SKIPPED"

# ── 5. 头条数字 ────────────────────────────────────────────────────────────
# 跳过数是个"不变量"：表里只剩范围外/G3 排除项时它应当稳定。
# 它变了意味着有人动了跳过表 —— 那必须是一次有意的、写进 §6.2 的决定。
# 8109 → 8143（B2a）：`built-ins/Map` 入册，套件自带的 42 条 feature 门控用例
# 进入跳过表；同时 `Map` 从 IN_SCOPE_PENDING 摘掉，另有 8 条本来被它"顺带跳过"
# 的用例转为执行（见 §6.1 里 `Array.prototype.flatMap` 那条事故记录）。
# 8143 → 8149（B2b）：`built-ins/Set` 入册，自带 26 条门控用例进表；`Set` 解锁
# 又让 20 条本来被"Set"这个子串顺带跳过的用例（`WeakSet` 之外的）转为执行 ——
# 其中 Map 套件 +12 条，其余在 language/*。
# 8149 → 8133（B3）：`built-ins/WeakMap`/`WeakSet` 入册（各带 9 + 5 条门控），
# 两个特性名从 IN_SCOPE_PENDING 摘掉后，先前被它们顺带跳过、分散在已入册套件里的
# 30 条用例转为执行。
# 8133 → 8173（B35）：`built-ins/Promise` 入册，套件自带的 40 条门控用例进跳过表；同时
# `Promise` 从 IN_SCOPE_PENDING 摘掉、`Promise.allSettled` / `Promise.any` /
# `Promise.prototype.finally` / `AggregateError` 等 ES2018+ 特性进 OUT_OF_SCOPE。
# 8173 → 8166（B45）：`Proxy` 从 IN_SCOPE_PENDING 摘掉、`built-ins/Proxy` 入册。
# 先前被 `features: [Proxy]` 顺带跳过、散落在 Object / JSON / Array / Symbol 等套件里的
# 7 条用例转为执行（它们的通过数就是本次那几个套件的小幅提升）。
# 8166 → 7843（B46）：`Reflect` 从 IN_SCOPE_PENDING 摘掉、`built-ins/Reflect` 入册。
# 这次是 -323：test262 里带 `features: [Reflect]` 的用例远比 Reflect 套件本身多
# （`Proxy` / `Math` / `Object` / `Promise` 里都有），它们此前被顺带跳过。
EXPECTED_SKIPPED=7843
if [ "$SKIPPED" != "$EXPECTED_SKIPPED" ]; then
  echo "  !! 跳过数从 $EXPECTED_SKIPPED 变为 $SKIPPED —— 跳过表被改动了。"
  echo "     请确认这是有意的（计划书 §2.2：交付特性时必须同批解锁），并更新本脚本的 EXPECTED_SKIPPED。"
fi
hr

# ── 6. 与基线逐套件对比 ────────────────────────────────────────────────────
if [ ! -f "$SNAP" ]; then
  echo
  if [ "$UPDATE" != "1" ]; then
    echo "没有基线快照 $SNAP。"
    echo "若这是第一次运行，用 --update 建立基线（并提交它）。"
    exit 2
  fi
  echo "没有基线快照，本次作为首次建立（跳过对比）。"
  mkdir -p "$(dirname "$SNAP")"
  {
    echo "# 阶段二基线快照 —— 由 scripts/phase2-status.sh --update 生成"
    echo "# 格式：suite<TAB>passed<TAB>skipped<TAB>failed"
    echo "# 最近更新：$(date +%Y-%m-%d)"
    cat "$TMP/new.tsv"
  } > "$SNAP"
  echo "已写入 $SNAP。"
  exit 0
fi

echo
echo "== 与基线 $SNAP 对比 =="
strip_comments "$SNAP" | sort > "$TMP/old.tsv"
join -t$'\t' -j 1 "$TMP/old.tsv" <(sort "$TMP/new.tsv") > "$TMP/joined.tsv"

REGRESSED=0
: > "$TMP/regress.txt"
: > "$TMP/gain.txt"
while IFS=$'\t' read -r suite old_p old_s old_f new_p new_s new_f; do
  if [ "$new_p" -lt "$old_p" ]; then
    printf '  回退  %-48s %s -> %s （失败 %s -> %s）\n' "$suite" "$old_p" "$new_p" "$old_f" "$new_f" >> "$TMP/regress.txt"
    REGRESSED=1
  elif [ "$new_p" -gt "$old_p" ]; then
    printf '  提升  %-48s %s -> %s\n' "$suite" "$old_p" "$new_p" >> "$TMP/gain.txt"
  fi
done < "$TMP/joined.tsv"

# 基线里有、新表里没有的套件（套件被移除或改名）
join -t$'\t' -j 1 -v 1 "$TMP/old.tsv" <(sort "$TMP/new.tsv") > "$TMP/missing.txt"
# 新表里有、基线里没有的（新增套件）
join -t$'\t' -j 1 -v 2 "$TMP/old.tsv" <(sort "$TMP/new.tsv") > "$TMP/added.txt"

if [ -s "$TMP/gain.txt" ]; then
  echo
  echo "提升的套件："
  sort -k3 -t' ' -r "$TMP/gain.txt" | head -20
fi

if [ -s "$TMP/missing.txt" ]; then
  echo
  echo "基线里有、本次没有的套件（被移除或改名？）："
  cut -f1 "$TMP/missing.txt" | sed 's/^/  /'
fi
if [ -s "$TMP/added.txt" ]; then
  echo
  echo "本次新出现的套件："
  cut -f1 "$TMP/added.txt" | sed 's/^/  /'
fi

if [ "$REGRESSED" = "1" ]; then
  echo
  echo "回退的套件（§7.1 要求：解释或回退）："
  cat "$TMP/regress.txt"
fi

# ── 7. 写回快照 ────────────────────────────────────────────────────────────
if [ "$UPDATE" = "1" ]; then
  {
    echo "# 阶段二基线快照 —— 由 scripts/phase2-status.sh --update 生成"
    echo "# 格式：suite<TAB>passed<TAB>skipped<TAB>failed"
    echo "# 最近更新：$(date +%Y-%m-%d)"
    cat "$TMP/new.tsv"
  } > "$SNAP"
  echo
  echo "已写入 $SNAP —— 记得把它和本批改动一起提交（它记录了本批的基线）。"
fi

echo
if [ "$REGRESSED" = "1" ]; then
  echo "结论：有逐套件回退。"
  exit 1
fi
echo "结论：无逐套件回退。"
