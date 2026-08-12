# KG 查询：一步到位

**规则：所有 KG 查询优先用 MCP Tool `dt_search_kg`，MCP 不可用时降级为 CLI `dt search --world knowledge`。**

---

## 推荐方式（覆盖 90% 场景）

```
dt_search_kg(query="<自然语言关键词>", limit=10)
```

GraphRAG 混合检索（向量召回 + 图扩展 + rerank），返回 JSON，含 `summary` / 来源文档 / `hop` / `score_breakdown`。

返回结果含 `elementId` 时，用精确查询取完整属性（经 memgraph MCP `run_cypher_query`）：

```cypher
MATCH (n) WHERE elementId(n) = "<返回的 elementId>"
RETURN n
```

---

## 回退方式

### 方式 A-降级：CLI（MCP 不可用时）

```bash
dt search "<关键词>" --world knowledge --limit 10
```

> `dt search-kg` 子命令已移除，KG 语义搜索走统一检索的 knowledge 世界。

### 方式 B：精确命名匹配（代码实体必须 world=code）

适用于明确的服务名/配置名/类名/方法名精确匹配：

```bash
# 代码实体（类/方法）— knowledge 世界不索引代码实体，必须 code
dt search "<关键词>" --world code --project <项目名> --limit 10

# 服务/配置实体
dt search "<关键词>" --world knowledge --project <项目名> --limit 10
```

Cypher 兜底（本环境 Memgraph 不支持 Neo4j 全文索引语法 `db.index.fulltext`，用属性匹配）：

```cypher
MATCH (n) WHERE n.project = '<项目名>' AND n.name CONTAINS '<关键词>'
RETURN n.name, labels(n)[0] AS type, n.hostname, n.url
LIMIT 10
```

### 方式 C：传统 Cypher

适用于探索性查询、统计聚合：

```cypher
MATCH (n)
WHERE n.name CONTAINS $keyword OR n.service_type CONTAINS $keyword
   OR n.auth_user CONTAINS $keyword OR n.hostname CONTAINS $keyword
   OR n.description CONTAINS $keyword OR n.url CONTAINS $keyword
   OR n.source_file CONTAINS $keyword
   OR ANY(lbl IN labels(n) WHERE toLower(lbl) CONTAINS toLower($keyword))
AND NONE(lbl IN labels(n) WHERE lbl IN ['Method','Class','Interface','Enum','Package','Module'])
RETURN labels(n)[0] AS type, n.name, n.auth_user, n.hostname, n.description
LIMIT 20
```

---

## KG 同步

```
dt_kg_sync()                    # MCP Tool（首选）
```

CLI 降级：

```bash
dt kg-sync                 # 全量同步（推荐首次）
dt kg-sync   # 增量同步（默认，日常维护）；--full 全量重建
```
