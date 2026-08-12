# dt memorize 写入指南：格式、异步同步、验证（2026-08-12 实测）

## 正确格式（最重要！）

`dt memorize <TYPE> <ENTITY_ID> "<details>" [--entity-type <标签>] [--project <name>]`

details **必须用标准 key: value; 分号分隔**，只认这些键：
`name / title / domain / summary / content / definition / source / confidence / verified_by`

```bash
# ✅ 正确（content/summary 有值 → 向量化有效）
dt memorize KnowledgeAdded warehouse-deployment \
  "name: warehouse 项目部署; title: warehouse 项目部署; \
   summary: uvp-warehouse-center 8095 + uvp-warehouse-api 8094; \
   content: warehouse(/data/aflmProjects/warehouse) 部署: 后端 8095/8094, 前端 yyc-caigou/yyc-yaochang-gongsi...; \
   domain: 项目" \
  --entity-type Project --project warehouse

# ❌ 错误（自定义键如 "项目: xxx" 解析不出标准字段 → name/title/summary/content 全空）
#    → Memgraph 节点存在但 content 为空 → 向量化无效 → knowledge 世界搜不到！
```

parse_details 只解析上述标准键，其他键（如"项目:"/"后端:"）被丢弃，字段落空。

## 异步向量化同步

- 写入路径：`write_knowledge`（MERGE 写 Memgraph）→ `auto_sync_kg`（enqueue + flush 500ms）→ 后台 VectorQueue embed + upsert 到 kg_nodes
- **写入后需等 3-4 秒**才进 Qdrant（CLI 进程退出前 flush 未必完成）
- 批量写入时每条之间 sleep 1-2s，最后再统一 sleep 3s 验证

## 验证方法

```bash
# 1. Memgraph 节点（确认 content 有值）
python -c "
from neo4j import GraphDatabase
drv = GraphDatabase.driver('bolt://127.0.0.1:7688')
with drv.session() as s:
    r = s.run(\"MATCH (n) WHERE n.knowledge_id = '<id>' RETURN n\").data()[0]
    print(r['n'].get('content'), r['n'].get('summary'))"

# 2. Qdrant kg_nodes（确认向量点存在）——payload 键是 type='knowledge'（小写！），business_id=entity_id
python -c "
from qdrant_client import QdrantClient
from qdrant_client.http import models as qm
qc = QdrantClient(url='http://127.0.0.1:6333')
res, _ = qc.scroll('kg_nodes', scroll_filter=qm.Filter(must=[
    qm.FieldCondition(key='business_id', match=qm.MatchValue(value='<id>'))]), limit=3, with_payload=True)
print(res[0].payload if res else '❌ 不在 kg_nodes')"

# 3. knowledge 世界召回
dt search "<查询词>" --world knowledge --limit 3 --json
```

## kg_nodes payload 键（勿混淆）

- 键名是 `type`（'knowledge' 小写）、`business_id`、`name`、`summary`、`project`、`labels: ['Knowledge']`
- **不是** `entity_type`（那是 search 返回的显示字段）——按 entity_type='Knowledge' 查 Qdrant 会得到 0

## MEMORY.md → KG 迁移工作流（实测 2191→1186 字符）

1. 盘点 MEMORY.md/USER.md 条目，分类：高频准则（保留）/ 项目细节（迁移）
2. 用上述正确格式批量 dt memorize（每条独立进程，sleep 1s）
3. dt search --world knowledge 验证每条可召回（同查询词）
4. memory 工具批量 remove 已迁移条目（operations 数组一次完成）
5. 验证 provider prefetch 对新记忆的召回（load_memory_provider + prefetch）

## 坑

- 迁移后召回验证时：新记忆会被语义更强的旧实体抢占（如 Product opencode/Feishu/Redis）——属正常排序，用具体查询词验证
- dt memorize 的向量化同步不依赖 daemon（connect_embed 直连 HTTP provider），CLI 模式可用
