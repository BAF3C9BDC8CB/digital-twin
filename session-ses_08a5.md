## 知识图谱标签分析结果

从你的查询和 schema 文档，以下是完整的分析：

---

### 一、空标签清单（count=0）共 16 个

| 标签 | 所属世界 | 设计用途 | 为空原因 |
|------|---------|---------|---------|
| **Event** | Memory | 通用事件节点，关联 INDEXED_METHOD / DEPLOYED_JOB 等关系 | 未启用。实际事件已走子类标签（Modification/ConfigChange） |
| **NacosInstance** | Reality | Nacos 注册的服务实例 IP:port 列表 | 未实现 Nacos 实例同步管道 |
| **Environment** | Reality | 生产/测试/开发环境实体 | 未从配置中提取环境实体 |
| **NacosNamespace** | Reality | Nacos 命名空间独立实体 | 未提取命名空间级别实体 |
| **Deployment** | Memory | 部署事件记录（deploy_id, job, env, version） | **已重命名为 `K8sDeployment`**（Reality 层），原 Deployment 标签废弃 |
| **Server** | Reality | 服务器/主机信息（IP, SSH 凭证） | 未实现服务器自动发现 |
| **Table** | Reality | 数据库表结构 | 未实现数据库 schema 同步 |
| **Endpoint** | Reality | HTTP API 端点 | 未从代码注解提取 API |
| **KnowledgeVersion** | Knowledge | 知识条目的版本历史 | 知识版本管理功能未上线 |
| **Playbook** | Knowledge | 可执行手册/操作指南 | 尚未创建任何 playbook |
| **Experience** | Knowledge | 踩坑/经验教训 | 尚未记录任何经验 |
| **Thread** | Digital Thread | 跨世界数字主线 | 尚未创建主线任务 |
| **Requirement** | Digital Thread | 需求/任务 | 尚未记录需求 |
| **Observation** | Reasoning | AI 模式发现/观察 | 推理管道未运行 |
| **Analysis** | Reasoning | AI 分析过程记录 | 推理管道未运行 |

另外接近空的：
- **ServiceInstance** (1) → 只有 1 个实例，应有多个环境多副本
- **NacosGroup** (1) → 通常应有 DEFAULT_GROUP + 自定义组
- **Concept** (2) → 只有 2 个概念/术语

---

### 二、设计原则

这套标签体系遵循 **六世界架构**（参见 [`docs/architecture-v2-data-schema.md`](./docs/architecture-v2-data-schema.md)）：

**1. Reality World（事实世界）** — Memgraph
- **特征**：客观存在，自动发现
- **标签**：`Method`, `Class`, `Module`, `Service`, `ServiceInstance`, `NacosConfig`, `ConfigKey`, `Server`, `Database`, `Table`, `Endpoint`, `K8sDeployment`, `NacosService`, `NacosGroup`, `NacosInstance`, `Environment`, `KubernetesCluster`
- **原则**：只存稳定的、可被工具自动扫描到的实体。K8s Pod 这种瞬态实体不入 Memgraph（属于 Runtime World）

**2. Knowledge World（知识世界）** — Memgraph
- **特征**：概念、模式、经验，人工整理或 AI 沉淀
- **标签**：`Knowledge`, `KnowledgeVersion`, `Playbook`, `Experience`, `Concept`, `Domain`
- **原则**：置信度体系（AI 生成 0.3~0.7，人工确认为 1.0），可版本追溯

**3. Memory World（记忆世界）** — Memgraph
- **特征**：时间线驱动，只增不删，完整审计日志
- **标签**：`Day`, `Session`, `Modification`, `Deployment`, `ConfigChange`, `BugFix`, `Decision`, `PodEvent`, `Event`
- **原则**：事件溯源，365 天 TTL 后归档到文件系统

**4. Semantic World（语义世界）** — Qdrant（不是 Memgraph 标签）
- 向量化文本，供相似度检索

**5. Runtime World（运行时世界）** — 不入 Memgraph
- Pod 列表、CPU/Mem、uptime 等实时数据，Context Builder 每次实时拉取后注入瞬态字段

**6. Reasoning World（推理世界）** — Memgraph
- **特征**：AI 生成，验证后可升级为 Knowledge
- **标签**：`Observation`, `Analysis`
- **原则**：推理结果可升级，避免污染正式知识

**7. Digital Thread（数字主线）** — Memgraph
- **标签**：`Thread`, `Requirement`
- **原则**：横切六世界，串联业务演化

---

### 三、核心设计决策

| 决策 | 说明 |
|------|------|
| **Memgraph schemaless** | 加属性不需要 migration，加标签不需要 schema change |
| **Service / ServiceInstance 分离** | Service 是跨环境稳定标识，ServiceInstance 承载每个环境的具体部署信息 |
| **K8sDeployment ≠ Deployment** | K8sDeployment = K8s 资源（Reality），Deployment = 部署事件记录（Memory）——后者的标签已废弃 |
| **Pod 不入 Memgraph** | Pod 是运行时概念，全部属于 Runtime World，实时拉取 |
| **瞬态字段机制** | ServiceInstance 的 cpu/mem/uptime 等标记为瞬态注入，Context Builder 每次请求实时填充 |
| **关系全大写动词** | `HAS_INSTANCE`, `DEPLOYED_AS`, `CONFIGURED_BY`, `BELONGS_TO` |

---

### 四、建议：哪些空标签值得优先填充

1. **`Server`** — 生产/测试机的自动发现。目前在配置管理时非常有用
2. **`Environment`** — 配合 ServiceInstance 建立环境层级
3. **`Endpoint`** — 从 Spring Controller 注解提取 API 列表
4. **`Playbook` / `Experience`** — 将 AI 沉淀的知识结构化
5. **`Thread`** — 管理长任务的多会话上下文
