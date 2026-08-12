#!/usr/bin/env bash
# 验证 Qdrant 缺口过滤（is_empty vs is_null）与 set_payload 保留向量的探针。
# 用法: scripts/verify-qdrant-gap-filter.sh [project] [collection]
# 依赖: curl + python3；Qdrant 需在本机 6333 运行（QDRANT_URL 可覆盖）。
# 输出: 集合列表、is_empty 缺口数、is_null 对比数、set_payload 后向量维度与探针字段。
set -euo pipefail
QDRANT_URL="${QDRANT_URL:-http://127.0.0.1:6333}"
PROJECT="${1:-message-center}"
COLLECTION="${2:-code_methods}"

echo "== 集合状态 =="
curl -sf -m 5 "$QDRANT_URL/collections" | python3 -c "import json,sys; d=json.load(sys.stdin); print([c['name'] for c in d['result']['collections']])"

echo "== is_empty 缺口计数 ($PROJECT / $COLLECTION) =="
curl -sf -m 10 -X POST "$QDRANT_URL/collections/$COLLECTION/points/count" -H 'Content-Type: application/json' \
  -d "{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"project\",\"match\":{\"value\":\"$PROJECT\"}},{\"key\":\"llm_analysis\",\"is_empty\":true}]}}"

echo; echo "== is_null 对比（预期 <= is_empty，键缺失时通常为 0）=="
curl -sf -m 10 -X POST "$QDRANT_URL/collections/$COLLECTION/points/count" -H 'Content-Type: application/json' \
  -d "{\"exact\":true,\"filter\":{\"must\":[{\"key\":\"project\",\"match\":{\"value\":\"$PROJECT\"}},{\"key\":\"llm_analysis\",\"is_null\":true}]}}"

echo; echo "== set_payload 探针（写入 -> 验证向量保留 -> 清理）=="
PID=$(curl -sf -m 5 -X POST "$QDRANT_URL/collections/$COLLECTION/points/scroll" -H 'Content-Type: application/json' \
  -d '{"limit":1,"with_payload":false,"with_vector":false}' \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print(d['result']['points'][0]['id'] if d['result']['points'] else '')")
if [ -n "$PID" ]; then
  curl -sf -m 5 -X POST "$QDRANT_URL/collections/$COLLECTION/points/payload" -H 'Content-Type: application/json' \
    -d "{\"payload\":{\"__probe\":\"ok\"},\"points\":[$PID]}" >/dev/null
  curl -sf -m 5 -X POST "$QDRANT_URL/collections/$COLLECTION/points" -H 'Content-Type: application/json' \
    -d "{\"ids\":[$PID],\"with_payload\":true,\"with_vector\":true}" \
    | python3 -c "import json,sys; p=json.load(sys.stdin)['result'][0]; print('point', p['id'], '| vector_len:', len(p.get('vector',[])), '| probe:', p['payload'].get('__probe'))"
  curl -sf -m 5 -X POST "$QDRANT_URL/collections/$COLLECTION/points/payload/delete" -H 'Content-Type: application/json' \
    -d "{\"keys\":[\"__probe\"],\"points\":[$PID]}" >/dev/null
  echo "（探针字段已清理）"
else
  echo "集合为空，跳过 set_payload 探针"
fi
