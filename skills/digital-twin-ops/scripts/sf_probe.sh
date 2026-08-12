#!/bin/bash
# SiliconFlow 云 API 连通性/余额探针 —— 从 pipeline.yaml 读真实 key(不打印 key 本体)
# 用途: dt build --source nacos 全量前预检, 或排查 402/401 余额问题
# 用法: bash sf_probe.sh [pipeline.yaml 路径, 默认 ~/.config/digital-twin/pipeline.yaml]
CFG="${1:-$HOME/.config/digital-twin/pipeline.yaml}"

KEY=$(python3 -c "
import re, sys
with open(sys.argv[1], encoding='utf-8') as f:
    content = f.read()
m = re.search(r'api_key:\s*[\"\x27]?([^\"\x27\n]*)', content)
print(m.group(1).strip() if m else '')
" "$CFG")

if [ -z "$KEY" ]; then
    echo "ERROR: 未在 $CFG 中找到 api_key"
    exit 1
fi
echo "key 前缀: ${KEY:0:6} 长度: ${#KEY} (不打印 key 本体)"

echo "--- 余额 (GET /v1/user/info) ---"
curl -s --max-time 15 https://api.siliconflow.cn/v1/user/info \
  -H "Authorization: Bearer $KEY" | python3 -c "
import json, sys
d = json.load(sys.stdin)
dd = d.get('data') or {}
if dd:
    print(f\"balance={dd.get('balance')} chargeBalance={dd.get('chargeBalance')} totalBalance={dd.get('totalBalance')} status={dd.get('status')}\")
else:
    print(d)
"

echo "--- chat 测试 (Qwen/Qwen3.5-9B, 10 token) ---"
curl -s --max-time 20 -X POST https://api.siliconflow.cn/v1/chat/completions \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"model":"Qwen/Qwen3.5-9B","messages":[{"role":"user","content":"ping"}],"max_tokens":10}' | head -c 300
echo

echo "--- embed 测试 (BAAI/bge-m3) ---"
curl -s --max-time 20 -X POST https://api.siliconflow.cn/v1/embeddings \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"model":"BAAI/bge-m3","input":"ping"}' | head -c 200
echo
echo
echo "解读: 401=key 无效; 402 code 30001=key 有效但余额不足; balance=0 → 先充值/领额度再跑构建"
