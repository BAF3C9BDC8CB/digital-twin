---
name: digital-twin
description: |
  知识图谱优先查询规则：在每次响应前先查询 Neo4j 知识图谱，获取环境上下文。
  当用户提到项目名/服务名/API/配置/Bug/部署时自动触发。
  询问用户问题前必先查 KG，已有数据则不重复询问。
---

# Personal Digital Twin

<EXTREMELY-IMPORTANT>
## 第一规则：先查知识图谱，再做任何其他事

在当前会话中，你的**第一个动作**必须是查询 Neo4j 知识图谱。
- ❌ 禁止先读文件
- ❌ 禁止先看目录结构
- ❌ 禁止先运行任何 shell 命令
- ✅ 必须先查询 Neo4j，从图的结果决定下一步做什么
</EXTREMELY-IMPORTANT>

---

## 🔄 执行流程

### 1. 关键词匹配查询（每次用户消息后必做）

从用户消息提取关键词（项目名、服务名、文件路径、API 名、Bug 描述），
查询知识图谱中所有匹配的实体，结果作为后续分析的基线上下文。

```cypher
MATCH (n)
WHERE n.name CONTAINS $keyword OR n.title CONTAINS $keyword
   OR n.file_path CONTAINS $keyword
RETURN labels(n)[0] AS type, n.name, n.description, n.file_path, n.root_cause, n.fix_summary
LIMIT 10
```

### 2. 上下文感知探针（按需执行）

根据用户消息的上下文类型，执行对应的扩展查询：

| 上下文 | 判断关键词 | 探针查询 |
|--------|-----------|---------|
| 项目/代码 | 项目名、文件路径、类名、方法名 | `MATCH (p:Project {name: $kw}) RETURN p` |
| Bug/报错 | exception、error、bug、异常、报错 | `MATCH (k:Knowledge) WHERE k.root_cause CONTAINS $kw RETURN k` |
| 部署/发布 | deploy、发布、上线、Jenkins | `MATCH (e:Event {type: "Deploy"}) RETURN e ORDER BY e.timestamp DESC LIMIT 5` |
| 配置变更 | Nacos、配置、改配置、Apollo | `MATCH (e:Event {type: "ConfigChange"}) RETURN e ORDER BY e.timestamp DESC LIMIT 5` |
| 无明确上下文 | 上述均不匹配 | 不执行扩展查询 |

### 3. 回复后判断是否写入

写入时使用 `dt` CLI（见 AGENTS.md 中的触发规则）：

| 操作 | 命令 |
|------|------|
| 记录事件 | `dt event --type ... --entity-id ...` |
| 记录知识 | `dt memorize --type ... --entity-id ...` |
| 索引代码 | `dt update --path ... --name ... --file ...` |
| 批量构建 | `dt build --path ... --name ...` |

关联规则：
- ✅ **优先关联**到已有实体
- ⚠️ **所有节点必须有标签**，不允许无标签的裸节点

写入后回复：`📝 已将 [标题] 记录到知识图谱`

---

## 禁止

- ❌ 不要问用户"要不要查知识图谱"——静默查询
- ❌ 不要在回答中长篇展示 Neo4j 结果
- ❌ 不要每次对话全量扫代码
- ❌ 不要询问用户已知存在于知识图谱中的信息
