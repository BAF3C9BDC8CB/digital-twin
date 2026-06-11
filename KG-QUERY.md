# KG 查询：三步递进策略

不是固定流程，而是建议路径。关键词明确时可直接跳 Step 3，不确定时从 Step 1 开始。

---

**Step 1：发现基础类型目录**
先看 KG 中有哪些大类，不查具体数据：
```cypher
MATCH (n)
RETURN distinct labels(n)[0] AS type
ORDER BY type
```

**Step 2：确定范围 + 关键词，定位具体节点类型**
根据关键词 + 排除/包含某些基础类型，找到命中的节点类型：
```cypher
MATCH (n)
WHERE (
  n.name CONTAINS $keyword OR n.service_name CONTAINS $keyword
  OR n.data_id CONTAINS $keyword OR n.ip CONTAINS $keyword
  OR n.description CONTAINS $keyword
  OR ANY(lbl IN labels(n) WHERE toLower(lbl) CONTAINS toLower($keyword))
)
AND NONE(lbl IN labels(n) WHERE lbl IN [
  'Method','Class','Interface','Enum','Package','Module'
])
RETURN labels(n)[0] AS type,
       coalesce(n.name, n.service_name, n.data_id, n.ip) AS name
LIMIT 20
```
排除的代码类型可根据需要增减。这一步不追求精确答案，只看命中什么类型。

**Step 3：按节点类型精准查询**
根据 Step 2 发现的类型，定向查询该类型的特定字段：
```cypher
// 示例：查询 NacosInstance 的具体 IP:Port
MATCH (i:NacosInstance)
WHERE i.service_name CONTAINS $keyword
RETURN i.service_name, i.ip, i.port, i.namespace, i.healthy
LIMIT 20
```

> 三步不是强制流程——**关键是让查询适应问题，而不是反过来。** 如果关键词已经很明确知道要查什么类型，直接 Step 3 即可。
