#!/bin/bash
# ============================================================
# 观测脚本：检查 Hermes 会话日志中 dt 工具调用记录
# 用法: bash scripts/check-dt-usage.sh [小时数] [agent.log 路径]
#   默认查最近 1 小时的 agent.log
# ============================================================
HOURS="${1:-1}"
LOG="${2:-$HOME/.hermes/logs/agent.log}"

echo "═══════════════════════════════════════════"
echo " dt 工具使用观测（最近 ${HOURS}h，${LOG}）"
echo "═══════════════════════════════════════════"

SINCE=$(date -d "-${HOURS} hours" +%Y-%m-%dT%H:%M 2>/dev/null || echo "")

echo ""
echo "--- 1. dt_sense 调用（L0 感知）---"
SENSE=$(grep -c "dt_sense\|dt sense" "$LOG" 2>/dev/null)
echo "  次数: ${SENSE:-0}"
grep -o "dt_sense([^)]*)" "$LOG" 2>/dev/null | tail -3

echo ""
echo "--- 2. dt_search_kg / dt search 调用（L1 检索）---"
SEARCH=$(grep -c "dt_search_kg\|dt_search\|dt search" "$LOG" 2>/dev/null)
echo "  次数: ${SEARCH:-0}"
grep -o "dt_search_kg([^)]*)" "$LOG" 2>/dev/null | tail -3
grep -o 'dt search "[^"]*"' "$LOG" 2>/dev/null | tail -3

echo ""
echo "--- 3. run_cypher_query 调用（L2 定向）---"
CYPHER=$(grep -c "run_cypher_query" "$LOG" 2>/dev/null)
echo "  次数: ${CYPHER:-0}"

echo ""
echo "--- 4. [DT-SENSE] 简报注入（L0 感知）---"
BRIEF=$(grep -c "DT-SENSE" "$LOG" 2>/dev/null)
echo "  注入次数: ${BRIEF:-0}"

echo ""
echo "--- 5. dt 相关工具总调用 ---"
TOTAL=$(( $(grep -c "dt_sense\|dt_search\|run_cypher\|dt search\|dt sense" "$LOG" 2>/dev/null) ))
echo "  合计: $TOTAL"

echo ""
echo "═══════════════════════════════════════════"
echo " 判定标准:"
echo "  ✅ 用了 dt 搜索: dt_sense ≥1 且 (dt_search_kg/dt search ≥1)"
echo "  ✅ 只用感知:     dt_sense ≥1 但无检索调用"
echo "  ❌ 完全没用:     全部为 0（需检查插件/AGENTS.md 是否生效）"
echo "═══════════════════════════════════════════"
