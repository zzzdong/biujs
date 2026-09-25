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
MEM_LIMIT_KB=6000000          # §6.5：全量回归一律带内存上限
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

if [ "$QUICK" = "1" ]; then
  echo
  echo "--quick：跳过全量回归。"
  exit 0
fi

# ── 4. 全量 test262 ────────────────────────────────────────────────────────
echo
echo "== 全量 test262（约 3 分钟，内存上限 ${MEM_LIMIT_KB}KB）=="
( ulimit -v "$MEM_LIMIT_KB"
  TEST262_FAILURES=0 cargo test --release --test test262_runner -- --nocapture ) > "$TMP/full.txt" 2>&1
RUN_STATUS=$?

if ! grep -q "^TOTAL" "$TMP/full.txt"; then
  echo "全量回归没有跑完（进程可能被内存上限打断或崩溃）。" >&2
  echo "最后 20 行：" >&2
  tail -20 "$TMP/full.txt" >&2
  exit 1
fi
if [ "$RUN_STATUS" != "0" ]; then
  echo "test262_runner 退出码 $RUN_STATUS，但摘要已生成 —— 请检查是否有套件报告失败以外的异常。" >&2
fi

# 摘要表 → TSV（表头下面到分隔线之间的行）
awk '/^suite[[:space:]]+passed/{f=1; next} /^-{10,}/{f=0} f && NF>=4 {print $1"\t"$2"\t"$3"\t"$4}' \
  "$TMP/full.txt" > "$TMP/new.tsv"

# ── 5. 头条数字 ────────────────────────────────────────────────────────────
echo
hr
grep -E "^(TOTAL|pass rate)" "$TMP/full.txt" | sed 's/^/  /'
TOTAL_LINE="$(grep '^TOTAL' "$TMP/full.txt" | head -1)"
EXECUTED=$(echo "$TOTAL_LINE" | awk '{print $2+$4}')
PASSED=$(echo "$TOTAL_LINE" | awk '{print $2}')
SKIPPED=$(echo "$TOTAL_LINE" | awk '{print $3}')
FAILED=$(echo "$TOTAL_LINE" | awk '{print $4}')
echo "  执行 $EXECUTED = 通过 $PASSED + 失败 $FAILED；跳过 $SKIPPED"

# 跳过数是个"不变量"：表里只剩范围外/G3 排除项时它应当稳定。
# 它变了意味着有人动了跳过表 —— 那必须是一次有意的、写进 §6.2 的决定。
EXPECTED_SKIPPED=8109
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
