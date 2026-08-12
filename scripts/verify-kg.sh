#!/bin/bash
# ============================================================
# digital-twin KG 验证脚本（用户自测版）
# 覆盖：索引状态 / 检索正确率 / 注释回归 / 索引对账 / 新功能
# 用法: bash scripts/verify-kg.sh [--full]
#   --full 增加 cargo test 全量测试（较慢，约 1 分钟）
# ============================================================
PASS=0; FAIL=0; WARN=0
ok()   { PASS=$((PASS+1)); echo "  ✅ $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  ❌ $1"; }
warn() { WARN=$((WARN+1)); echo "  ⚠️  $1"; }

echo "═══════════════════════════════════════════"
echo " 1. 索引状态（dt sense）"
echo "═══════════════════════════════════════════"
S=$(dt sense /data/aflmProjects/aflm/uvp-im-center --json 2>/dev/null)
echo "$S" | python3 -c "
import json,sys
d=json.load(sys.stdin)
s=d.get('stats',{})
print(f\"  项目: {d.get('project', d.get('project_name','?'))}\")
print(f\"  方法: {s.get('methods','?')}  类: {s.get('classes','?')}  向量: {s.get('vectors','?')}\")
print(f\"  last_build: {s.get('last_build','?')}\")
" 2>/dev/null || warn "sense 解析失败（可忽略，看原始输出）"
echo "  预期: methods=2287 vectors=2287"

echo ""
echo "═══════════════════════════════════════════"
echo " 2. 检索正确率（中文自然语言，预期命中 im-center）"
echo "═══════════════════════════════════════════"
for q in "发送单聊消息" "创建群组" "撤回消息" "查询历史消息" "UserSig 签名"; do
  N=$(dt search "$q" --world code --project im-center --limit 3 --json 2>/dev/null | python3 -c "import json,sys; h=json.load(sys.stdin).get('hits',[]); print(sum(1 for x in h if x.get('project')=='im-center'))" 2>/dev/null)
  if [ "$N" -ge 1 ]; then ok "「$q」→ im-center 命中 $N 条"; else bad "「$q」→ 0 条（预期 ≥1）"; fi
done

echo ""
echo "═══════════════════════════════════════════"
echo " 3. 英文标识符检索（预期 100% 命中）"
echo "═══════════════════════════════════════════"
for q in "accountImport" "addGroupMember" "getUserSig" "sendC2CMsg"; do
  N=$(dt search "$q" --world code --project im-center --limit 3 --json 2>/dev/null | python3 -c "import json,sys; h=json.load(sys.stdin).get('hits',[]); print(sum(1 for x in h if x.get('project')=='im-center'))" 2>/dev/null)
  if [ "$N" -ge 1 ]; then ok "$q → im-center 命中 $N 条"; else bad "$q → 0 条"; fi
done

echo ""
echo "═══════════════════════════════════════════"
echo " 4. 注释错位回归（此前 bug：无注释方法偷上方法 javadoc）"
echo "═══════════════════════════════════════════"
C=$(dt search "groupMsgGetSimple" --world code --project im-center --limit 1 --json 2>/dev/null | python3 -c "import json,sys; h=json.load(sys.stdin).get('hits',[]); print(h[0].get('comment','') or h[0].get('llm_analysis','') if h else 'NO_HIT')" 2>/dev/null)
if echo "$C" | grep -q "删除群成员消息"; then bad "groupMsgGetSimple 注释仍被错位污染: $C"; else ok "groupMsgGetSimple 注释干净（$C）"; fi
C2=$(dt search "groupMsgRecall" --world code --project im-center --limit 1 --json 2>/dev/null | python3 -c "import json,sys; h=json.load(sys.stdin).get('hits',[]); print(h[0].get('comment','') or h[0].get('llm_analysis','') if h else 'NO_HIT')" 2>/dev/null)
if echo "$C2" | grep -q "撤回"; then ok "groupMsgRecall 正确注释保留（$C2）"; else warn "groupMsgRecall 注释: $C2"; fi

echo ""
echo "═══════════════════════════════════════════"
echo " 5. 索引对账（dt health）"
echo "═══════════════════════════════════════════"
dt health 2>&1 | grep -E "索引对账|❌" | sed 's/^/  /'

echo ""
echo "═══════════════════════════════════════════"
echo " 6. 低分降级提示（故意错 world 搜代码，预期出现提示）"
echo "═══════════════════════════════════════════"
L=$(dt search "groupMsgGetSimple" --world knowledge --limit 3 2>&1)
if echo "$L" | grep -q "结果可能不相关"; then ok "低分降级提示触发"; else bad "低分提示未触发"; fi

echo ""
echo "═══════════════════════════════════════════"
echo " 7. 跨项目分组展示（不带 --project，预期有分布行）"
echo "═══════════════════════════════════════════"
G=$(dt search "发送单聊消息" --limit 5 2>&1)
if echo "$G" | grep -q "命中项目分布"; then ok "分组展示生效"; else bad "分组展示缺失"; fi

echo ""
echo "═══════════════════════════════════════════"
echo " 8. knowledge 世界（dt learn 补的知识层）"
echo "═══════════════════════════════════════════"
K=$(dt search "im-center 消息发送链路" --world knowledge --project im-center --limit 3 --json 2>/dev/null | python3 -c "import json,sys; h=json.load(sys.stdin).get('hits',[]); print(len(h))" 2>/dev/null)
if [ "${K:-0}" -ge 1 ]; then ok "knowledge 世界命中 $K 条（0.92 分知识层）"; else bad "knowledge 检索 0 条"; fi

if [ "$1" = "--full" ]; then
echo ""
echo "═══════════════════════════════════════════"
echo " 9. cargo test 全量测试（约 1 分钟）"
echo "═══════════════════════════════════════════"
cd /data/myProject/digital-twin-v2
T=$(cargo test --release --lib 2>&1 | tail -1)
echo "  $T"
echo "$T" | grep -q "0 failed" && ok "全量测试通过" || bad "测试有失败"
fi

echo ""
echo "═══════════════════════════════════════════"
echo " 结果: PASS=$PASS FAIL=$FAIL WARN=$WARN"
[ "$FAIL" -eq 0 ] && echo " ✅ 全部通过" || echo " ❌ 有失败项，见上方"
echo "═══════════════════════════════════════════"
