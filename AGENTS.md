# Knowledge Graph Behavior

This project uses a Neo4j knowledge graph for persistent memory.

## ⚠️ 第一动作：每次工作必须先查询知识图谱

执行任何任务的**第一个动作**必须是查知识图谱。不允许直接读文件或探索目录而不先查 KG。

**执行流程：**
1. 从用户消息提取关键词（服务名、文件路径、API、Bug、术语）
2. 从当前环境提取关键词（项目名、工作目录、当前文件、git remote、README 中的项目名）
3. 如果第 1+2 步有任何关键词 → 立即用 `neo4j_read_cypher` 查询 KG
4. 有匹配结果 → 取出作为上下文使用
5. 无匹配 → 继续正常分析

```cypher
MATCH (n)
WHERE n.name CONTAINS $keyword OR n.title CONTAINS $keyword
   OR n.file_path CONTAINS $keyword
RETURN labels(n)[0] AS type, n.name, n.description, n.file_path, n.root_cause, n.fix_summary
LIMIT 10
```

**唯一不查的情况：** 当前环境无任何项目上下文（刚启动、无目录、无打开的文件）且用户消息中也无任何关键词。除此以外都必须查。

---

## Active Trigger: "记忆" keyword

用户说 "记忆" / "记一下" / "记住这个" / "记下来" / "记住" 时：

必须立即写入 KG。这是命令，不是建议。

写入方式（优先使用系统命令，不依赖编辑器 API）：
```bash
dt memorize --type KnowledgeAdded \
  --entity-id "<唯一标识>" \
  --entity-type "<实体类型>" \
  --details "<要记住的内容>" \
  --project "<项目名>"
```

写入后必须回复：`📝 已将 [XXX] 记录到知识图谱`

---

## ⚠️ 触发规则：AI 操作后写入知识图谱

以下 AI 操作完成后，必须**无条件**执行对应的写入命令。不可省略，不可跳过。

### 写 KG

| # | 触发操作 | 条件 | 命令 |
|---|---------|------|------|
| 1 | 用户说"记忆/记一下/记住这个/记下来/记住" | 总是 | `dt memorize --type KnowledgeAdded --entity-id "<标识>" --entity-type "<实体类型>" --details "<内容>" --project "<项目>"` |
| 2 | 安装软件（apt/pip/npm/brew 等） | 总是 | `dt event --type SoftwareInstalled --entity-id "<包名>" --entity-type Software --details "version: <版本>, method: <安装方式>" --project "<项目>"` |
| 3 | 修改 Nacos/Apollo/Consul 等外部配置 | 总是 | `dt event --type ConfigChange --entity-id "<data_id>" --entity-type NacosConfig --details "<改动摘要>" --project "<项目>"` |
| 4 | 做出架构/技术决策（选型、迁移、方案设计） | 总是 | `dt memorize --type Decision --entity-id "<决策标识>" --entity-type ArchitectureDecision --details "decision: <决策>; reason: <原因>; scope: <影响范围>" --project "<项目>"` |
| 5 | Jenkins 部署（`jenkins_build_job` MCP） | **仅生产/stable 环境** | `dt event --type Deploy --entity-id "<job_name>" --entity-type JenkinsJob --details "branch: <分支>, env: <环境>, params: <参数>" --project "<项目>"` |

### 不写 KG 但更新向量库

以下操作不写 KG，但需要更新 Qdrant 向量库（代码搜索依赖）：

| 触发操作 | 条件 | 命令 |
|---------|------|------|
| 源码修改（创建/编辑 .py/.java/.ts 等） | 文件维度 | `dt update --path <项目路径> --name <项目名> --file <相对路径>` |
| 批量同步 / 项目首次索引 | 项目维度 | `dt build --path <项目路径> --name <项目名>` |
| 删除文件 | 文件已删除 | `dt remove --project <项目名> --file <原相对路径>` |

新增的文件会自动被 `dt build` 发现并索引。`dt build` 通过 SQLite 记录文件哈希，只处理有改动的文件，不重新索引整个项目。

### 完全不操作

| 操作 | 原因 |
|------|------|
| Bug 修复 | 信息已 inline 在代码中 |
| 开发/测试环境的临时部署构建 | 非生产发布，无回溯价值 |
| 一般的 API 请求（查询类 GET） | 读操作不产生变更 |
| 一次性对话、临时调试、常规编辑 | 无长期价值 |

### 执行规则

- 写入后必须回复：`📝 已将 [XXX] 记录到知识图谱`
- 写入时优先关联已有实体，禁止创建孤立节点
- 不执行的后果：Event 节点会变为孤立节点，后续时间线查询全部失效

## Session-end Protocol

用户说 "done" / "结束" 时：
1. 列出关键发现
2. 写入 Conversation 节点，关联发现的实体
3. 回复：`📝 已将 [本次会话摘要] 记录到知识图谱`

---

## Event 知识图谱架构说明

当前知识图谱中的 Event 节点已通过以下关系关联到对应实体：

```
(:Event)-[:INDEXED_METHOD]->(:Method)       # 方法/文件被索引
(:Event)-[:INDEXED_PROJECT]->(:Project)     # 项目被索引
(:Event)-[:INSTALLED_SOFTWARE]->(:Software)  # 软件被安装
(:Event)-[:INDEXED_DOC]->(:Document)        # 文档被索引
(:Event)-[:SYNCED_NAMESPACE]->(:NacosNamespace) # Nacos 配置同步
(:Event)-[:SCANNED_SERVER]->(:Server)       # 服务器被扫描
(:Event)-[:DEPLOYED_JOB]->(:JenkinsJob)     # Jenkins 部署
```

Event 节点通过 `event_id` 唯一约束去重（SHA256 哈希），重复操作不会创建重复节点。
