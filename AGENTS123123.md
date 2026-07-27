# Knowledge Graph Behavior

This project uses a Memgraph knowledge graph for persistent memory.

## ⚠️ [新增] 必须先加载 digital-twin 技能

执行任何任务前，先调用 `skill` 工具加载 **digital-twin** 技能，获取完整指令后再按流程执行。仅靠本文件不够——skill 文件中包含最新的详细工作流。

## ⚠️ 执行顺序：先感知环境，再查知识图谱

**执行流程：**
1. **最小环境感知** — 读一次当前工作目录（`read` 根目录），获取项目名/目录名
2. **提取关键词** — 从用户消息 + 当前环境（项目名、工作目录、文件名、git remote）提取关键词
3. **查询 KG** — 有关键词则立即查 KG；无关键词则继续分析
4. **深入探索** — KG 查询完成后，再深入代码/目录

> 不强求"第一个动作就是 KG"——但要求**在深度探索代码之前完成 KG 查询**。

---

### 查询策略：按场景选择（优先级从高到低）

#### 🥇 场景 A：查找基础设施/服务/凭证/配置信息

**最先尝试** `dt search-kg`（向量语义搜索，无需写 Cypher）：

```bash
dt search-kg "<关键词>" --limit 10
```

拿到 `elementId` 后，用精确查询取完整属性：

```cypher
MATCH (n) WHERE elementId(n) = "4:xxx..."
RETURN n.auth_user, n.auth_password, n.hostname, n.port, n.url, n.service_type
```

#### 🥈 场景 B：全文关键词精确匹配

如果是明确的命名关键词（如服务名、配置名），用全文索引兜底：

```cypher
CALL db.index.fulltext.queryNodes("infra_search", "<关键词>")
YIELD node, score
RETURN node.name, labels(node)[0] AS type, node.auth_user, node.hostname, node.url, score
ORDER BY score DESC LIMIT 10
```

> 全文索引覆盖标签：Infrastructure, Server, Database, Project, Environment, Software, Knowledge, Configuration, NacosConfig, NacosService

#### 🥉 场景 C：探索性查询（不确定目标类型时）

```cypher
MATCH (n)
WHERE (
  n.name CONTAINS $keyword
  OR n.auth_user CONTAINS $keyword
  OR n.hostname CONTAINS $keyword
  OR n.service_type CONTAINS $keyword
  OR n.description CONTAINS $keyword
  OR n.url CONTAINS $keyword
  OR n.source_file CONTAINS $keyword
  OR ANY(lbl IN labels(n) WHERE toLower(lbl) CONTAINS toLower($keyword))
)
AND NONE(lbl IN labels(n) WHERE lbl IN ['Method','Class','Interface','Enum','Package','Module'])
RETURN labels(n)[0] AS type, n.name, n.auth_user, n.hostname, n.description
LIMIT 20
```

> ⚠️ 场景 C 是兜底方案，优先用场景 A 或 B。场景 A (`dt search-kg`) 是推荐首选。

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

## ⚠️ 触发规则（已自动化）

事件写入已由 Hook 系统自动处理，AI 不再需要手动调用 `dt event`：

| 操作 | 自动触发的 Hook | 写入标签 |
|------|----------------|---------|
| 代码修改 | `code_modified`（dt build 插件） | `:Modification` |
| Jenkins 部署 | `jenkins_deploy_completed`（jcli_build） | `:Deployment` + 更新 JenkinsJob/Build/ServiceInstance |
| Nacos 配置变更 | `config_changed` | `:ConfigChange` |
| 架构决策 | `decision_made`（dt memorize） | `:Decision` |
| Bug 修复 | `bug_fix_recorded` | `:BugFix` |
| 会话结束 | `session_ended` | `:Conversation` |
| K8s Pod 异常 | `pod_event_occurred` | `:PodEvent` |
| K8s 同步完成 | `k8s_synced` | `:K8sSyncEvent` |

AI 只需要：
- 执行正常的操作（修改代码、部署、变更配置等），Hook 会自动完成事件记录
- 无需手动调用 `dt event` 或记忆命令

## Session-end Protocol

用户说 "done" / "结束" 时：
1. 列出关键发现
2. `session_ended` Hook 会自动记录会话到知识图谱
3. 无需手动调用 `dt event`

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
