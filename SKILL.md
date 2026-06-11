---
name: digital-twin
description: |
  知识图谱优先查询规则 + Qdrant 语义代码搜索。
  每次响应前先查 Neo4j KG 获取环境上下文。
  当用户提到项目名/服务名/API/配置/Bug/部署/代码搜索时自动触发。
---

# Digital Twin 工作流

<EXTREMELY-IMPORTANT>
## 第一规则：先查知识图谱，再做任何其他事

在当前会话中，你的**第一个动作**必须是查询 Neo4j 知识图谱。
- ❌ 禁止先读文件、先看目录、先 grep
- ✅ 先查 Neo4j，从结果决定下一步做什么
</EXTREMELY-IMPORTANT>

---

## 代码搜索优先使用 `dt search`（Qdrant 向量语义搜索）

涉及代码搜索时，不要直接 grep 或 glob 读文件，按以下流程执行：

**Step 1：确定项目名**
从以下来源获取项目名（按优先级）：
- 用户直接提到（如"uvp-oauth-center 里的登录逻辑"）
- 从环境提取（git remote、工作目录名）
- 查知识图谱确认：`MATCH (p:Project) WHERE p.name CONTAINS $keyword RETURN p.name`

**Step 2：执行语义搜索**
```bash
dt search "<关键词>" --project "<项目名>" --limit 10
```
返回结果包含：`method_id`、`name`、`file_path`、`start_line`、`end_line`、`signature`、`source_code`、`calls` 等。

**Step 3：按需查看完整上下文**
`dt search` 已返回方法签名和代码片段，需要完整上下文时再通过 Read 工具读取对应文件的指定行范围。

**为什么这样设计：**
- Qdrant 存储了代码向量，语义搜索比关键字 grep 更准确
- 分页（`--limit`）控制结果量
- 项目级过滤避免跨项目噪声
- 避免了"查 KG → 读 method_id → 读文件"的绕路流程

---

## KG 查询：三步递进策略

不是固定流程，而是建议路径。关键词明确时可直接跳 Step 3，不确定时从 Step 1 开始。

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

> 三步不是强制流程——**关键是让查询适应问题，而不是反过来。**

---

## 事件与知识写入规则

### 触发写入

| 触发操作 | 条件 | 命令 |
|---------|------|------|
| 用户说"记忆/记一下/记住这个/记下来/记住" | 总是 | `dt memorize --type KnowledgeAdded --entity-id "<标识>" --entity-type "<实体类型>" --details "<内容>" --project "<项目>"` |
| 安装软件（apt/pip/npm/brew 等） | 总是 | `dt event --type SoftwareInstalled --entity-id "<包名>" --entity-type Software --details "version: <版本>, method: <安装方式>" --project "<项目>"` |
| 修改 Nacos/Apollo/Consul 等外部配置 | 总是 | `dt event --type ConfigChange --entity-id "<data_id>" --entity-type NacosConfig --details "<改动摘要>" --project "<项目>"` |
| 同步 Nacos 配置 | AI 判断必要时 | `dt nacos-sync --env test` 或 `dt nacos-sync --env prod` |
| 做出架构/技术决策 | 总是 | `dt memorize --type Decision --entity-id "<决策标识>" --entity-type ArchitectureDecision --details "decision: <决策>; reason: <原因>" --project "<项目>"` |
| Jenkins 部署 | 仅生产/stable 环境 | `dt event --type Deploy --entity-id "<job_name>" --entity-type JenkinsJob --details "branch: <分支>, env: <环境>" --project "<项目>"` |

### 不写 Event/Knowledge 但同步代码实体

`dt update` / `dt build` 同步 Method/Class/CALLS 到 Neo4j + Qdrant。

| 触发操作 | 条件 | 命令 |
|---------|------|------|
| 源码修改（创建/编辑 .py/.java/.ts 等） | 文件维度 | `dt update --path <路径> --name <项目名> --file <相对路径>` |
| 批量同步 / 项目首次索引 | 项目维度 | `dt build --path <路径> --name <项目名>` |
| 删除文件 | 文件已删除 | `dt remove --project <项目名> --file <原相对路径>` |

### 完全不操作

| 操作 | 原因 |
|------|------|
| Bug 修复 | 信息已 inline 在代码中 |
| 开发/测试环境的临时部署构建 | 非生产发布，无回溯价值 |
| 一般的 API 请求（查询类 GET） | 读操作不产生变更 |
| 一次性对话、临时调试、常规编辑 | 无长期价值 |

### 执行规则
- 写入后回复：`📝 已将 [XXX] 记录到知识图谱`
- 写入时优先关联已有实体，禁止创建孤立节点

---

## Session-end Protocol

用户说 "done" / "结束" 时：
1. 列出关键发现
2. 执行：`dt event --type Conversation --entity-id "<会话日期>" --entity-type Session --project "<项目>" --details "<关键发现摘要>"`
3. 回复：`📝 已将 [本次会话摘要] 记录到知识图谱`

---

## Event 架构说明

```
(:Event)-[:INDEXED_METHOD]->(:Method)       # 方法被索引
(:Event)-[:INDEXED_PROJECT]->(:Project)     # 项目被索引
(:Event)-[:INSTALLED_SOFTWARE]->(:Software)  # 软件被安装
(:Event)-[:INDEXED_DOC]->(:Document)        # 文档被索引
(:Event)-[:SYNCED_NAMESPACE]->(:NacosNamespace) # Nacos 同步
(:Event)-[:SCANNED_SERVER]->(:Server)       # 服务器扫描
(:Event)-[:DEPLOYED_JOB]->(:JenkinsJob)     # Jenkins 部署
```

Event 通过 `event_id` 唯一约束去重（SHA256 哈希）。

---

## 禁止

- ❌ 不要问用户"要不要查知识图谱"——静默查询
- ❌ 不要在回答中长篇展示 Neo4j 结果
- ❌ 不要每次对话全量扫代码
- ❌ 不要询问用户已知存在于知识图谱中的信息
