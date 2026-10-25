#!/usr/bin/env bash
# 文档数字的单一数据源。
#
# README.md 与 docs/PROJECT.md 里的通过率一律由本脚本从 docs/phase2-status.tsv
# 计算并写回，**不手改**。`scripts/phase2-status.sh --update` 会自动调用它，
# 所以基线一刷新，文档数字跟着变，不会再出现"README 一个数、PROJECT 另一个数、
# PLAN 第三个数"的分裂（ARCH-REVIEW-2026-10-24 §1.2）。
#
# 数字里同时带上护栏预算：同一份代码在不同 `TEST262_MEMORY_MB` 下通过数会差几条，
# 不带预算的数字不可复现。
#
# 被改写的段落必须用标记包起来：
#   <!-- doc-numbers --> ... <!-- /doc-numbers -->
# 没有标记的行脚本不会碰（`docs/PLAN.md` 里的历史验收数字属于日志，故意不在管辖范围）。

set -euo pipefail
cd "$(dirname "$0")/.."

tsv=docs/phase2-status.tsv
[[ -f "$tsv" ]] || { echo "缺少 $tsv" >&2; exit 1; }

# 逐套件求和（跳过 # 注释行）。
read -r passed skipped failed < <(
    awk -F'\t' '!/^#/ && NF >= 4 { p += $2; s += $3; f += $4 } END { print p + 0, s + 0, f + 0 }' "$tsv"
)
executed=$((passed + failed))
if (( executed == 0 )); then
    echo "$tsv 里没有数据" >&2
    exit 1
fi
rate=$(awk -v p="$passed" -v e="$executed" 'BEGIN { printf "%.2f", 100 * p / e }')
updated=$(grep -m1 '^# 最近更新：' "$tsv" | sed 's/^# 最近更新： *//')
budget=$(grep -m1 '^# guards:' "$tsv" | sed -n 's/.*TEST262_MEMORY_MB=\([0-9][0-9]*\).*/TEST262_MEMORY_MB=\1/p')
[[ -n "$budget" ]] || budget="护栏预算未记录"

line="**${rate}%**（${passed} / 执行 ${executed}；跳过 ${skipped}；${updated} 基线，${budget}）"

rewrite() {
    local file="$1"
    grep -q '<!-- doc-numbers -->' "$file" || { echo "$file 缺少 doc-numbers 标记" >&2; exit 1; }
    grep -q '<!-- /doc-numbers -->' "$file" || { echo "$file 缺少 /doc-numbers 标记" >&2; exit 1; }
    # 整文件匹配，两个标记同行也能处理（README 里就是同一行）。
    DOC_NUMBERS="$line" perl -0pi -e '
        s{<!-- doc-numbers -->.*?<!-- /doc-numbers -->}
         {"<!-- doc-numbers -->" . $ENV{DOC_NUMBERS} . "<!-- /doc-numbers -->"}se
    ' "$file"
}

for file in README.md docs/PROJECT.md; do
    rewrite "$file"
    echo "  已更新 $file"
done
echo "  test262 通过率：$line"
