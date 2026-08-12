# dt memorize 正确用法 + 记忆迁移到 KG 配方（2026-08-12 实测）

## 核心坑：details 必须是标准 key:value 格式

`dt memorize` 的 DETAILS 参数由 `parse_details` 解析，**只认标准键**：
`name / title / domain / summary / content / definition / source / confidence / verified_by`

❌ 错误示范（本会话实测踩坑）：自由文本 `"项目: warehouse(...); 后端: 8095..."` →
parse_details 匹配不到任何标准键 → **content 为空** → 向量化空文本 → knowledge 世界搜不到。

✅ 正确格式：
```
dt memorize KnowledgeAdded warehouse-deployment \
  "name: warehouse 项目部署; title: warehouse 项目部署; \
   summary: warehouse(/data/aflmProjects/warehouse): uvp-warehouse-center 8095 + api 8094; \
   content: 完整细节正文...; domain: 项目" \
  --entity-type Project --project warehouse
```

命令签名：`dt memorize <KnowledgeAdded|Decision|Environment|Dependencies> <entity-id> "<details>" [--entity-type <标签>] [--project <name>]`
（Experience/Concept/Domain/Playbook 也可，各自字段不同）

## 向量化是异步的（非 bug）

- 写入流程：写 Memgraph（MERGE）→ auto_sync_kg 入队 SyncAccumulator → 后台 VectorQueue embed + upsert 到 Qdrant `kg_nodes`
- flush 只等 500ms + 后台 worker——**写入后等 3-5 秒**再验证召回
- 若 embed/vector 连接失败（connect_embed 读 pipeline.yaml providers），sync_acc=None → 静默只写图不向量化，无报错

## kg_nodes payload 键名（Qdrant 侧验证用）

`type`（小写：'knowledge'/'config'/'concept'…，**不是** entity_type）、`business_id`（=knowledge_id）、
`name`、`summary`、`description`、`project`、`labels`、`origin`（'learned'）。

⚠️ Qdrant 点 ID 必须是 uint/UUID——**不能**用 `retrieve(col, ['warehouse-deployment'])`（报 400）；
用 filter 按 payload 字段查：`scroll_filter=Filter(must=[FieldCondition(key='name', match=MatchValue(value='...'))])`

## 验证配方（写入 → 召回闭环）

```bash
# 1. 写后验证 Qdrant 点存在
python -c "from qdrant_client import QdrantClient; from qdrant_client.http import models as qm; \
qc=QdrantClient(url='http://127.0.0.1:6333'); \
res,_=qc.scroll('kg_nodes', scroll_filter=qm.Filter(must=[qm.FieldCondition(key='name', match=qm.MatchValue(value='<name>'))]), limit=3, with_payload=True); print(len(res))"
# 2. knowledge 世界召回验证
dt search "<查询词>" --world knowledge --limit 3 --json
```

## 记忆迁移配方（Hermes MEMORY.md → KG）

用户偏好场景：MEMORY.md 满（96%）时，把低频项目细节迁到 KG，靠 digital-twin memory provider 按需召回。

1. **盘点分类**：MEMORY.md 条目分三类
   - 保留（每轮高价值）：工具坑、环境事实、Hermes 自身部署、KG 准则
   - 迁移（低频/项目特定）：项目端口/库/登录/部署细节（技能已覆盖的优先迁）
   - 精简：技能已完整覆盖的内容可直接删
2. **批量迁移**：每个项目一条 dt memorize（正确 key:value 格式），project 参数限定
3. **瘦身**：memory 工具批量 remove 已迁移条目（batch operations 一次完成，检查最终字符数）
4. **闭环验证**：模拟 provider prefetch（`load_memory_provider('digital-twin')` + `prefetch(查询)`）确认新记忆可被召回

实测效果：MEMORY.md 2191→829 chars（96%→37%），warehouse/any-auto-register 记忆被 prefetch 首条命中。
