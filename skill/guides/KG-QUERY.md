# KG 查询：一步到位

**规则：所有 KG 查询优先用 MCP Tool `dt_search_kg`，MCP 不可用时降级为 `dt search-kg` CLI。**

---

## 推荐方式（覆盖 90% 场景）

```bash
dt search-kg "<自然语言关键词>" --limit 10
```

返回结果含 `elementId`，用精确查询取完整属性：

```cypher
MATCH (n) WHERE elementId(n) = "<返回的 elementId>"
RETURN n
```

---

## 回退方式

### 方式 B：全文索引

适用于明确的服务名/配置名精确匹配：

```cypher
CALL db.index.fulltext.queryNodes("infra_search", "<关键词>")
YIELD node, score
RETURN node.name, labels(node)[0] AS type, node.auth_user, node.hostname, node.url, score
ORDER BY score DESC LIMIT 10
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

```bash
dt kg-sync                 # 全量同步（推荐首次）
dt kg-sync --incremental   # 增量同步（日常维护）
```
