# Knowledge Graph Behavior

This project uses a Memgraph knowledge graph for persistent memory.

## ⚠️ [新增] 必须先加载 digital-twin 技能

执行任何任务前，先调用 `skill` 工具加载 **digital-twin** 技能，获取完整指令后再按流程执行。仅靠本文件不够——skill 文件中包含最新的详细工作流。

## ⚠️ 执行顺序：先感知环境，再查知识图谱

**执行流程：**
1. **环境感知（第一个动作）** — 调 MCP `dt_sense()`（CLI 降级 `dt sense --json`）：一条命令完成项目定位、注册匹配、索引状态检查；已索引返回项目简报（统计/目录画像/语言/关键实体），未注册返回候选项目发现报告。**不再手工读目录猜关键词。**
2. **按场景搜索** — 具体任务需求走 `dt_search` / `dt_search_kg`（见下方查询策略）
3. **深入探索** — 感知与搜索完成后，再深入代码/目录

> dt_sense 不可用时（MCP 掉线且 dt 不在 PATH）才回退旧流程：读根目录 → 提取关键词 → 查 KG。

> 不强求"第一个动作就是 KG"——但要求**在深度探索代码之前完成 KG 查询**。

---

### 查询策略：按场景选择（优先级从高到低）

#### 🥇 场景 A：查找基础设施/服务/凭证/配置信息

**最先尝试 MCP Tool `dt_search_kg`**（GraphRAG 混合检索，无需写 Cypher）：

```
dt_search_kg(query="<关键词>", limit=10)
```

拿到 `elementId` 后，用精确查询取完整属性（经 memgraph MCP `run_cypher_query`）：

```cypher
MATCH (n) WHERE elementId(n) = "4:xxx..."
RETURN n.auth_user, n.auth_password, n.hostname, n.port, n.url, n.service_type
```

> MCP 不可用时降级为 CLI：`dt search "<关键词>" --world knowledge --limit 10`
> （`dt search-kg` 子命令已移除，KG 搜索走统一检索的 knowledge 世界。）

#### 🥈 场景 B：精确命名关键词匹配

明确的命名关键词（如服务名、配置名、类名、方法名）优先走统一检索，`world` 选对层、`project` 限定消除跨项目噪音：

```
dt_search_kg(query="<关键词>", world="code|knowledge|config", project="<项目名>", limit=10)
```

代码实体（类/方法）推荐用 world=code（knowledge 世界不索引代码实体）；服务/配置在 knowledge/config 世界。**代码逻辑任务推荐先 dt_search_kg(world=code) 定位再读源码验证**；若上下文（[DT-SENSE] 简报 project 字段 / 用户消息）已明确目标项目名，推荐同时带 project=<项目名> 过滤跨项目噪音（KG 命中=事实；仅当 dt_search_kg 不可用/超时才纯读源码并标注 ⚠）。
Cypher 兜底用属性匹配（本环境 Memgraph 不支持 Neo4j 全文索引语法）：

```cypher
MATCH (n) WHERE n.project = '<项目名>' AND n.name CONTAINS '<关键词>'
RETURN n.name, labels(n)[0] AS type, n.hostname, n.url
LIMIT 10
```

> 覆盖实体：Class/Method/Entity/Service/Server/Project 等全部标签，按 project 过滤跨项目噪音。

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

> ⚠️ 场景 C 是兜底方案，优先用场景 A 或 B。场景 A (`dt_search_kg`) 是推荐首选。

> ⚠️ **查询词建议**（复测验证）：中文查询尽量带具体动词（"添加群成员"而非"群成员管理"）；功能名/标识符类（accountImport、addGroupMember）用英文或中英混搭召回率更高（100% vs 80%）。

#### 场景 D：代码调用链问题（入口→链路追踪）

L1 定位入口方法后，**推荐用 L2 遍历 CALLS 关系**直接拿调用链，省去逐个读文件：

```cypher
MATCH p=(a:Method)-[:CALLS*1..2]->(b)
WHERE a.project = '<项目名>' AND a.name = '<入口方法名>'
RETURN a.name AS caller, b.name AS callee
LIMIT 20
```

> 注意过滤噪音：`toString/success/fail/error/getUrl/getXxx` 等公共方法/getter 会淹没业务链，优先看跨类调用（`b.class_name <> a.class_name`）。

**唯一不查的情况：** 当前环境无任何项目上下文（刚启动、无目录、无打开的文件）且用户消息中也无任何关键词。除此以外都必须查。

---

## Active Trigger: "记忆" keyword

用户说 "记忆" / "记一下" / "记住这个" / "记下来" / "记住" 时：

必须立即写入 KG。这是命令，不是建议。

写入方式（首选 MCP Tool `dt_memorize`）：
```
dt_memorize(type="KnowledgeAdded",
            entity_id="<唯一标识>",
            entity_type="<实体类型>",
            details="<要记住的内容>",
            project="<项目名>")
```

MCP 不可用时降级为 CLI：
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
