# KG CALLS 图遍历实战配方 + 价值审计事实（2026-08-12 实测验证）

来源：KG 价值发挥评估审计（im-center"消息撤回"三会话，Memgraph bolt://127.0.0.1:7688，纯只读查询全部跑通）。
Method 节点属性：name, class_name, file_path, project, calls(列表), signature, start_line/end_line, comment。

## 已验证查询（可直接复制）

1. 确认 CALLS 关系存在：
```cypher
MATCH (a)-[:CALLS]->(b) RETURN labels(a) AS src_label, labels(b) AS dst_label, a.name AS src, b.name AS dst LIMIT 10
```

2. 精确业务调用边（同名方法用 class_name 消歧，别只按 name 查）：
```cypher
MATCH (a:Method)-[:CALLS]->(b:Method)
WHERE a.project='im-center' AND a.name='groupMsgRecall' AND a.class_name='GroupService'
  AND b.name='groupMsgRecallUpdate'
RETURN a.signature, b.class_name, b.signature LIMIT 5
```

3. 完整调用链一条查询（1-2 hop 图遍历，核心配方）：
```cypher
MATCH p = (a:Method)-[:CALLS*1..2]->(b:Method)
WHERE a.project='im-center' AND a.class_name='GroupController' AND a.name='groupMsgRecall'
RETURN [n IN nodes(p) | n.class_name + '.' + n.name] AS chain LIMIT 20
```
→ 实测直接给出 `GroupController.groupMsgRecall → GroupService.groupMsgRecall → MessageRecordMongoService.groupMsgRecallUpdate`，还带出 CommonResult.success/fail、ImClient.getGroup 等分支——一条查询复现 agent 花 8-10 次 read_file 才梳理出的 controller→service→mongo 调用链。

4. 跨类业务调用（过滤 getter/公共方法噪音）：
```cypher
MATCH (a:Method)-[:CALLS]->(b:Method)
WHERE a.project='im-center' AND a.class_name <> b.class_name
  AND NOT (b.name IN ['toString','success','fail','error','info'])
RETURN a.class_name, a.name, b.class_name, b.name LIMIT 40
```

5. 统计与结构速查：
```cypher
MATCH (a:Method)-[:CALLS]->(b:Method) WHERE a.project='im-center'
RETURN count(*) AS total_edges, count(DISTINCT a.name) AS distinct_callers
MATCH (n) RETURN labels(n) AS l, count(*) AS c ORDER BY c DESC LIMIT 20
```

## Memgraph 语法坑（踩过）
- `b.name NOT IN [...]` 解析失败（`mismatched input 'NOT'`）→ 必须写 `NOT (b.name IN [...])`。
- 按 id 前缀查知识节点 `n.id STARTS WITH 'dt://knowledge'` 返回空——Knowledge/Playbook/Experience 节点的 id 不是该格式；改用 `WHERE n.entity_type IN ['pattern','playbook','experience']` 或直接 dt_search_kg(world=knowledge) 检索。

## 噪音与质量事实（2026-08-12 实测）
- im-center 有 8903 条 CALLS 边 / 189 个 caller，但大量是 toString/success/fail/getUrl 等 getter 与公共方法——LIMIT 60 会被 toString 淹没，**图遍历必须过滤**（配方 4 的 NOT IN 列表 + 只看跨类）。
- `dt_search_kg(world=code)` 返回的 `calls` 字段 = 方法体内所有方法名（含 JDK 库方法），但**业务链关键 hop 就在里面**：GroupService.groupMsgRecall 的 calls 含 groupMsgRecallUpdate（mongo 层）。L1 命中后可用 calls 推断下一跳，不必先读文件。
- llm_analysis 对 Builder 构造器/内部类方法有语义偏差且构成 0.28 分噪声（limit=8 时 3/7 命中是构造器/重复项）——检索结果需甄别，别轻信低分项摘要。
- knowledge 世界 im-center 仅 8 个知识节点（Knowledge×2 + Playbook×2 + Experience×4，内容全是发送链路+群组管理）——业务概念（如"撤回"）可能没有语义入口，搜不到不代表不存在，先查 entity_type 分布。
- memory 世界实测 0 命中——会话结论不会自动进 KG，需主动沉淀。
- 沉淀工具：`dt_learn(task, project, pattern, pitfalls, decisions)` 批量写 Knowledge World（pattern/踩坑/决策）；`dt_memorize(type, entity_id, details)` 写单条 Decision/KnowledgeAdded。标题/pattern 里带业务关键词（如"撤回 recall withdraw"）可提升后续向量命中。

## 审计方法论（KG 是否被充分利用的检查清单）
评估一次代码问答会话的 KG 价值发挥时，逐项核对：
1. L1 命中里的 calls 列表是否被用来推断调用链（还是只当位置线索）；
2. 是否用过 L2 CALLS 图遍历拿全链（配方 3 一条查询可替代 3-5 次 read_file）；
3. 分析结论（pattern/踩坑/决策）是否 dt_learn 沉淀回 KG 供后续会话复用；
4. 三会话重复问同一问题 → 检查 memory 世界/session_search 是否有复用路径；
5. 检索噪声占比（构造器/内部类/getter 命中数 vs limit）。
