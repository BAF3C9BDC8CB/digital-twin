# 知识图谱空标签分析

> 分析日期: 2026-07-18
> 数据来源: Memgraph `db.labels()` + 源码 `src/` 全量搜索

## 总览

共 **17 个标签** 在知识图谱中节点数为 0，分三类：

---

## 第一类：代码完整，缺触发入口

有 `CREATE`/`MERGE` 语句存在，但对应的触发器从未被调用。

| 标签 | 节点数 | 源码位置 | 创建语句 | 设计存储内容 | 缺的触发 |
|------|--------|---------|---------|-------------|---------|
| **Thread** | 0 | `thread/service.rs:219` | `CREATE (t:Thread {name, title, description, project, status, ...})` | 会话线程追踪。按 project 分组会话和决策 | `dt_thread` MCP 工具从未被调用（无人执行过 create/list/close 等操作） |
| **Deployment** | 0 | `handlers/deployment.rs:82` | `MERGE (e:Deployment {deploy_id, job, env, branch, version, status, ...})` | CI/CD 部署事件记录。通过 `[:DEPLOYS]->(:ServiceInstance)` 关联到服务实例 | `DeploymentHandler` 已注册在 `EventDispatcher`，但 `EventType::Deployment` 的事件从未被 `record_event` 触发 |
| **Server** | 0 | `k8s/resource_sync.rs:415` | `MERGE (s:Server {server_id, name, hostname, service_type, cpu, memory})` | K8s Node 节点信息，作为基础设施实体 | `dt k8s-sync` 从未在有 K8s API 认证的环境中运行过 |
| **ServiceInstance** | 0 | `handlers/deployment.rs:95` | `MERGE (si:ServiceInstance {instance_id})` | 服务具体实例（host:port 级别）。通过 `[:DEPLOYS]` 被 Deployment 反向关联 | 同 Deployment——由同一 handler 在 `record_event(Deployment)` 时连带创建，但从未触发 |
| **Observation** | 0 | `reasoning/mod.rs:34`（struct） `schema.rs:103`（constraint） | 通过 `ReasoningService` 间接写入 | 推理第一层：事实发现。记录关于代码/系统的客观观察（`description`, `evidence`, `pattern`, `confidence`） | 推理管道 `ReasoningService` 从未接入 AI 工作流 |
| **Analysis** | 0 | `reasoning/mod.rs:78`（struct） `schema.rs:104`（constraint） | 同上 | 推理第二层：结构化分析。连接 Observation → Decision（`question`, `hypothesis`, `method`, `intermediate steps`, `conclusion`） | 同上，推理管道未激活 |

---

## 第二类：无创建代码

标签已定义（在 `BUSINESS_LABELS` 或约束中），但源码中没有任何 `CREATE`/`MERGE` 语句写入。

| 标签 | 节点数 | 设计存储内容 | 代码情况 |
|------|--------|-------------|---------|
| **Environment** | 0 | 部署环境（prod/test）。预期通过 `[:DEPLOYS_TO]->(:Namespace)` 关联到 K8s 命名空间 | 无 CREATE。`resource_sync.rs` 中交叉链接代码只有 `MATCH (env:Environment {name: $env})`，期望节点已存在。半成品 |
| **NacosNamespace** | 0 | Nacos 命名空间。属性: `namespace`, `description`。预期关联 `[:CONTAINS]->(:NacosConfig)` | 全库搜不到 CREATE/MERGE。Nacos 同步只写 `NacosConfig` 和 `NacosService`，命名空间作为属性内联在节点中 |
| **NacosInstance** | 0 | Nacos 服务实例（IP/端口级）。属性: `instance_id`, `service_name`, `ip`, `port` | 无 CREATE。设计的细化粒度，从未实现 |
| **KubernetesCluster** | 0 | K8s 集群级聚合节点。预期连接多个 `[:Namespace]` + `[:Server]` | 仅在 `BUSINESS_LABELS`（`kg_bridge.rs:59`）中定义，用于 Qdrant 同步白名单。无任何源码实现 |
| **KnowledgeVersion** | 0 | 知识版本历史。属性: `version_id`, `knowledge_id`, `version`, `diff`, `session_id` | 实体 struct 完整（`knowledge/entities.rs:140`），但版本生命周期功能未实现 |
| **Playbook** | 0 | 可执行操作手册。属性: `name`, `steps[]`, `domain`, `project`, `success_count` | 实体 struct 完整（`knowledge/entities.rs:163`），上下文解析器引用它，但无写入路径 |
| **Experience** | 0 | 经验教训/踩坑记录。属性: `title`, `summary`, `content`, `severity`, `domain` | 实体 struct 完整（`knowledge/entities.rs:209`），被 context stages 引用，但 `dt learn` 从未写入此标签 |
| **Requirement** | 0 | 需求追踪。属性: `title`, `description`, `status`, `domain` | 仅在 `BUSINESS_LABELS` 中定义。占位符 |
| **Table** | 0 | 数据库表。属性: `table_name`, `db_type`, `description`, `columns` | 在 `BUSINESS_LABELS` + `build_search_text` 中有分支，但无写入代码。占位符 |
| **Endpoint** | 0 | API 端点。属性: `path`, `method`, `controller`, `description`, `project` | `vectorizer.rs` 中有 Rust 结构体，`BUSINESS_LABELS` 中有定义，但无任何写入逻辑。占位符 |

---

## 第三类：V1 遗留标签

| 标签 | 节点数 | 设计 | 说明 |
|------|--------|------|------|
| **Event** | 0 | 泛型事件节点，所有事件子类型的基类。预期通过 `[:INDEXED_METHOD]->(:Method)` 等关系连接 | V1 设计。V2 重构为按类型的独立标签（`:Modification`, `:Deployment`, `:ConfigChange` 等）。`EventDispatcher` 直接创建 `(:Modification {mod_id: ...})` 而非 `(:Event {event_type: "Modification"})`。此标签因向后兼容保留在 schema 中但从未被写入 |

---

## 对比：有数据的类似标签

避免混淆的参考点：

| 标签 | 节点数 | 说明 | 和谁易混淆 |
|------|--------|------|-----------|
| **Service** | 45 | 微服务定义（来自 Nacos 同步），区别于 ServiceInstance | 不要混为 ServiceInstance(0) |
| **K8sDeployment** | 111 | K8s Deployment 资源（来自 k8s-sync），区别于 Deployment(0) | 不要混为 Deployment(0) 事件标签 |
| **Modification** | 43 | 代码修改事件（由 dt_build 自动触发写入） | EventType::Modification 被正常触发 |
| **ConfigChange** | 4 | 配置变更事件 | 有数据说明有被触发过 |
