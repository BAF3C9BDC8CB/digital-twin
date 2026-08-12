#!/usr/bin/env bash
# =============================================================================
# digital-twin 记忆召回测试（四层验证）
#   L0 数据层:  dt search --world knowledge 直接查（KG 里有没有）
#   L1 插件层:  provider 加载 + is_available
#   L2 系统层:  MemoryManager.prefetch_all 模拟（注入文本是否生成）
#   L3 端到端:  hermes chat -q 真实会话（模型是否真看到 [KG 记忆]）
#
# 用法: bash test-recall.sh [查询词...]
# 默认查询: warehouse 端口 / any-auto-register 部署
# =============================================================================
set -u

DT_BIN="${DT_BIN:-/home/luis/.local/bin/dt}"
PY="${PY:-/home/luis/.hermes/hermes-agent/venv/bin/python}"
QUERIES=("${@:-warehouse 项目的端口和测试库 any-auto-register 怎么部署}")

PASS=0; FAIL=0
ok()   { echo "  ✅ $1"; PASS=$((PASS+1)); }
bad()  { echo "  ❌ $1"; FAIL=$((FAIL+1)); }

echo "═══ L0 数据层: dt search 直接查 KG ═══"
for q in "${QUERIES[@]}"; do
  echo "── 查询: $q"
  out=$("$DT_BIN" search "$q" --world knowledge --limit 3 --json 2>/dev/null)
  hits=$(echo "$out" | "$PY" -c "
import json,sys
d=json.load(sys.stdin)
hs=d.get('hits',[])
for h in hs[:3]:
    print(f\"  ({h.get('entity_type')}) {h.get('title','')[:40]} | {str(h.get('snippet',''))[:60]}\")
print(f'共 {len(hs)} 条命中')
")
  echo "$hits"
  if echo "$hits" | grep -q "共 0 条"; then bad "L0: $q 无召回"; else ok "L0: $q 有召回"; fi
done

echo ""
echo "═══ L1 插件层: provider 加载 ═══"
"$PY" -c "
import sys
sys.path.insert(0, '/home/luis/.hermes/hermes-agent')
from plugins.memory import load_memory_provider, discover_memory_providers
dt = [a for a in discover_memory_providers() if a[0]=='digital-twin']
print('  发现:', dt)
p = load_memory_provider('digital-twin')
print('  加载:', 'OK' if p else 'FAIL', '| available:', p.is_available() if p else '?')
" 
if [ $? -eq 0 ]; then ok "L1: provider 加载"; else bad "L1: provider 加载失败"; fi

echo ""
echo "═══ L2 系统层: MemoryManager.prefetch_all 注入 ═══"
for q in "${QUERIES[@]}"; do
  echo "── 查询: $q"
  "$PY" -c "
import sys
sys.path.insert(0, '/home/luis/.hermes/hermes-agent')
from agent.memory_manager import MemoryManager
from plugins.memory import load_memory_provider
p = load_memory_provider('digital-twin')
mm = MemoryManager(external_prefetch_timeout=10.0)
mm.add_provider(p)
p.initialize('test', agent_context='primary')
r = mm.prefetch_all('$q', session_id='test')
if r:
    for line in r.splitlines()[:6]:
        print(f'  {line[:100]}')
    print(f'  → 注入 {len(r)} chars')
else:
    print('  → 空注入')
"
done
echo "  （L2 注入非空即通过，上面每查询是否出现 [KG 记忆] 即验证）"

echo ""
echo "═══ L3 端到端: 真实会话（回答是否含 KG 关键信息）═══"
q="${QUERIES[0]}"
EXPECT="${EXPECT_WORD:-8095}"
echo "── 会话查询: $q（提示不要查工具）"
echo "── 期望回答包含: $EXPECT"
SESSION_OUT=$(timeout 90 "$PY" -m hermes_cli.main chat -q "$q，不要查任何工具，直接回答" 2>&1 | head -40)
echo "$SESSION_OUT" | grep -A2 "Reasoning" | head -3
ANSWER=$(echo "$SESSION_OUT" | grep -iE "^[-*•]? ?[0-9]|端口|8095|8094" | head -3)
echo "$ANSWER" | head -3
if echo "$SESSION_OUT" | grep -qi "$EXPECT"; then
  ok "L3: 回答包含预期信息 $EXPECT（模型0工具调用答出私有数据=记忆注入生效）"
else
  bad "L3: 回答未见 $EXPECT（人工确认上面输出）"
fi

echo ""
echo "════════ 汇总: $PASS 通过 / $FAIL 失败 ════════"
exit $FAIL
