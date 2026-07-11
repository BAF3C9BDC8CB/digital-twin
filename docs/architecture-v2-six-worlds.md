# Digital Twin v2 架构设计：六世界模型

> ⚠️ **DEPRECATED**: 本文档已被 [V3 单 Crate 分层架构](./architecture-v3-single-crate-layered.md) 替代。
> V2 多 crate workspace 方案已废弃，实际实现采用单 crate 内部模块分层。
> 保留本文档仅供历史参考。

> 状态：设计阶段 | 日期：2026-07-09（已刷新：新增安全/指标/备份/并发控制/版本管理/反馈闭环） | 关联文档：[项目结构](./architecture-v2-project-structure.md) · [数据格式](./architecture-v2-data-schema.md) · [数据管线](./architecture-v2-data-pipeline.md) · [MCP API](./architecture-v2-mcp-api-spec.md) · [实施路线图](../.weave/plans/v2-implementation-roadmap.md)

---

## 一、整体架构

```
                         ┌──────────────────────────────────────────────┐
                         │           Digital Twin v2                     │
                         │        (Agent-Native Platform)                │
                         │                                              │
                         │  ┌────────────────────────────────────────┐  │
                         │  │         dt CLI daemon (Rust)            │  │
                         │  │         gRPC Server :50051              │  │
                         │  │                                        │  │
                         │  │  ┌──────────────────────────────────┐  │  │
                         │  │  │          Plugin Registry          │  │  │
                         │  │  │  plugin_k8s / plugin_svc /        │  │  │
                         │  │  │  plugin_jenkins                   │  │  │
                         │  │  └──────────────────────────────────┘  │  │
                         │  │                                        │  │
                         │  │  ┌──────────────────────────────────┐  │  │
                         │  │  │        LogService (gRPC)          │  │  │
                         │  │  │   统一日志 → /var/log/dt-daemon   │  │  │
                         │  │  └──────────────────────────────────┘  │  │
                         │  └────────────┬───────────────────────────┘  │
                         └───────────────┼──────────────────────────────┘
                                         │
         ┌───────────────────────────────┼───────────────────────────────┐
         │                               │                               │
         │   Six Worlds                  │                               │
         │                               │                               │
         │  ┌─────────────┐  ┌───────────▼────────┐  ┌────────────────┐  │
         │  │  Reality    │  │  Knowledge World   │  │  Memory World  │  │
         │  │  (事实/资源) │  │  (知识/概念)        │  │  (历史/经验)    │  │
         │  │             │  │                    │  │                │  │
         │  │ Service     │  │ Knowledge          │  │ Day→Session    │  │
         │  │  └Instance  │  │ Playbook           │  │  →Event        │  │
         │  │ Method/Class│  │ Experience/Concept │  │ Modification   │  │
         │  │ Server/DB   │  │ Domain             │  │ Deployment     │  │
         │  │ NacosConfig │  │                    │  │ Decision       │  │
         │  └──────┬──────┘  └──────────┬─────────┘  └───────┬────────┘  │
         │         │                    │                     │          │
         │  ┌──────▼──────┐  ┌──────────▼─────────┐  ┌───────▼────────┐  │
         │  │  Runtime    │  │  Semantic World    │  │  Reasoning     │  │
         │  │  (实时状态)  │  │  (向量/相似度)      │  │  World         │  │
         │  │             │  │                    │  │  (AI推理缓存)   │  │
         │  │ →ServiceIns │  │ Qdrant collections │  │ Observation    │  │
         │  │  tance缓存  │  │ BGE-M3 1024-dim    │  │ →Analysis     │  │
         │  │  字段       │  │ entity_id→Neo4j    │  │ →Decision     │  │
         │  │ 不入Neo4j   │  │                    │  │ →Knowledge    │  │
         │  └──────┬──────┘  └──────────┬─────────┘  └───────┬────────┘  │
         │         │                    │                     │          │
         └─────────┼────────────────────┼─────────────────────┼──────────┘
                   │                    │                     │
                   └────────────────────┼─────────────────────┘
                                        │
                         ┌──────────────▼──────────────────────┐
                         │         Context Builder              │
                         │  Retriever→Ranker→Dedup→Resolver    │
                         │  →Summarize (Chain of Responsibility)│
                         │                                      │
                         │  输出：六世界聚合上下文 (JSON)         │
                         └──────────────┬──────────────────────┘
                                        │ gRPC
                         ┌──────────────▼──────────────────────┐
                         │          MCP Server (Python)         │
                         │  gRPC client → dt daemon :50051     │
                         │  JSON-RPC → OpenCode / LLM           │
                         └──────────────┬──────────────────────┘
                                        │
                         ┌──────────────▼──────────────────────┐
                         │               LLM                    │
                         └─────────────────────────────────────┘
```

**基础设施层（支撑六世界运行）：**

| 组件 | 通信 | 说明 |
|------|------|------|
| dt CLI daemon | gRPC Server :50051 | 常驻进程，所有工具的统一入口 |
| Neo4j | Bolt :7687 | 图数据库，存储 Reality/Knowledge/Memory/Reasoning/Thread |
| Qdrant | gRPC :6334 | 向量数据库，存储 Semantic World |
| dt-embed | gRPC :50052 | BGE-M3 嵌入服务 (Python) |
| dt-log | gRPC LogService | 统一日志管道，聚合 4 个进程的日志 |
| Plugin Registry | 进程内 | 插件生命周期管理，6 条强制约束 |
| MCP Server | gRPC client | 协议适配，LLM ↔ dt daemon |
| Auth Interceptor | 进程内 | gRPC mTLS + Unix socket 鉴权，权限分级（Admin/ReadOnly） |
| MetricsService | gRPC (:50051) | 指标采集与查询，不暴露 HTTP 端口 |
| Backup Manager | 进程内 | Neo4j/Qdrant/SQLite 分层备份与恢复 |

---

## 二、六个世界

| 世界 | 存储 | 核心实体 | 特征 |
|------|------|----------|------|
| **Reality** | Neo4j (Bolt) | Method, Class, Module, Service, **ServiceInstance**, Server, Database, Table, NacosConfig, ConfigKey, Endpoint, Document, **K8sDeployment** | 客观存在，可被自动发现。实体有稳定性层级：Service(年)→ServiceInstance(周)→K8sDeployment(部署时) |
| **Knowledge** | Neo4j (Bolt) | Knowledge, Playbook, Experience, Concept, Domain | 人类整理或 AI 自动沉淀。@knowledge 注释→自动提取；dt_learn→任务完成后沉淀 |
| **Memory** | Neo4j (Bolt) | Day, Session, Modification, Deployment（→ServiceInstance）, ConfigChange, BugFix, Decision | 时间线驱动，只增不删。TTL 365天后归档。完整审计日志 |
| **Runtime** | 缓存（不入 Neo4j） | **Pod 信息** (name, ip, phase, restarts, node) + **Metrics** (cpu, memory, uptime, heap, thread) | 实时查询 K8s API/Actuator，注入到 **ServiceInstance 缓存字段**。每次 dt_context 重新拉取 |
| **Semantic** | Qdrant (gRPC) | Code/Doc/Config/API/Exp/Log 向量 | BGE-M3 1024 维，通过 entity_id 反查 Neo4j |
| **Reasoning** | Neo4j（会话级） | Observation, Analysis, Decision | AI 推理痕迹。验证后升级为 Knowledge；未验证的会话结束后降级 |

### Reality World 深入：多环境模型与稳定性层级

Reality World 中的实体有天然的**稳定性层级**：

```
稳定性从高到低：
──────────────────────────────────────────────▶

(:Service)           (:K8sDeployment)       Pods & Metrics
  service_id            name                  pod_name, pod_ip
  永不变化              部署策略              phase, restarts
                       image, replicas        cpu, memory
  Reality ✅          Reality ✅              Runtime ⚠️
  (Neo4j 持久化)      (Neo4j, k8s-sync)     (缓存，不入库)
```

**设计决策：Pod 全部属于 Runtime**

K8s 自己都不保证 Pod 永存——滚动更新、节点故障、HPA 伸缩都会销毁 Pod。把 Pod 写进 Neo4j 意味着需要管理生命周期（标记 Terminated、清理旧 Pod、处理快照不一致）。

**放在 Runtime 的好处**：
- 每次查询都是最新数据，不存在"Pod 已终止但 Neo4j 还显示 Running"的脏数据
- 无需生命周期管理代码（Terminated 标记、过期 Pod 清理）
- 减少 Neo4j 写入量（k8s-sync 不再写 Pod）
- Pod 的历史信息（哪天哪个 Pod 崩溃了）应该走 Memory World 事件，不污染 Reality

**Reality 中唯一的 K8s 实体是 K8sDeployment**——它足够稳定（name/image/replicas 仅部署时变），提供足够的追溯信息。

```
(:Service)                              ← 跨环境稳定标识
  name: "aflm-pay"
    │
    ├──[:HAS_INSTANCE]──▶ (:ServiceInstance {env: "prod"})
    │                        host: "10.0.1.50", port: 8080
    │                        status: "running", version: "v2.3.1"
    │                          │
    │                          ├──[:DEPLOYED_AS]──▶ (:K8sDeployment)
    │                          │     name: "aflm-pay"
    │                          │     image: "aflm-pay:v2.3.1"
    │                          │     replicas: 2
    │                          │
    │                          └──[:CONFIGURED_BY]──▶ (:NacosConfig)
    │
    │   // Runtime 缓存字段 ↓ (Context Builder 实时查询 K8s API 注入)
    │   pods: [
    │     {name: "aflm-pay-7d8f-abc", ip: "10.244.1.23", phase: "Running",
    │      restarts: 3, node: "node-01", cpu: "250m", memory: "512Mi"},
    │     {name: "aflm-pay-7d8f-def", ip: "10.244.2.45", phase: "Running",
    │      restarts: 0, node: "node-02", cpu: "180m", memory: "480Mi"}
    │   ]
    │   uptime: "7d 12h", heap_used: "256MB/512MB"
    │
    └──[:HAS_INSTANCE]──▶ (:ServiceInstance {env: "test"})
                             host: "10.0.2.50", port: 8080
                             version: "v2.4.0-rc1"
                               │
                                └──[:DEPLOYED_AS]──▶ (:K8sDeployment)
                             // Runtime 缓存 ↓
                             pods: [{name: "aflm-pay-3f2e-mno", phase: "Pending", ...}]
```

**数据流：**

```
k8s-sync (每小时)                    Context Builder (按需)
─────────────────                    ─────────────────────
写入 Neo4j:                          实时查询 K8s API:
  (:K8sDeployment)                     GET /pods → pods[]
    name, image, replicas              GET /metrics → cpu, memory
  (:ServiceInstance)                   GET actuator → heap, threads
    host, port, version
                                      注入 ServiceInstance:
                                        pods: [...]
                                        cpu, memory, uptime...
```

**设计原则：**
- Service = 稳定标识，service_id 不含环境
- ServiceInstance = 每个环境的部署快照，含 host/port/version
- K8sDeployment = K8s 稳定资源，含 image/replicas
- K8sPod 全部属于 Runtime（实时查询，不入 Neo4j），终止后的历史追溯走 Memory World 事件
- Runtime 指标（CPU/Mem/Uptime）和 Pod 信息全部作为缓存字段挂在 ServiceInstance 上，**不入 Neo4j**
- Context Builder 组装时实时查询 K8s API → 注入 ServiceInstance 缓存字段
- 想追溯"昨天 Pod 为什么 CrashLoop"→ 查 Memory World 的事件记录

### Knowledge World 深入：来源与沉淀

六个来源，按优先级：

| 优先级 | 来源 | 触发 | 示例 |
|--------|------|------|------|
| ⭐1 | AI 会话自动提取 | 会话结束 | 从 Session 提取关键发现 |
| ⭐2 | AI 任务主动沉淀 | `dt_learn` MCP | 记录模式、经验、踩坑 |
| ⭐3 | 文档自动解析 | `dt build` 扫描 | md/pdf → 术语、概念 |
| ⭐4 | 代码注释标记 | `dt build` AST | `@knowledge domain="支付"` |
| ⭐5 | 执行结果采集 | AI 执行工具后 | kubectl/mysql 返回值沉淀 |
| ⭐6 | 用户口述 | "记住" | `dt memorize` 兜底 |

### Reasoning World 深入：Decision Graph

```
(:Observation)        ← AI 发现但尚未得出结论的模式
    ↓ 分析验证
(:Decision)           ← AI 做出的选择/判断
  ├── context         ← 当时面临的问题
  ├── alternatives    ← 考虑过的候选方案
  ├── evidence        ← 支撑决策的证据
  ├── choice          ← 最终选择
  ├── confidence      ← 置信度 (0.0~1.0)
  └── verified        ← 是否已被验证为正确
    ↓ 执行确认
(:Knowledge)          ← 验证正确，永久保留
```

---

## 三、核心原则

### World 关系

- **Reality + Knowledge + Memory** 是 Neo4j 中三类 Entity，通过统一关系模型连接
- **Semantic** 是 Qdrant 中的向量，通过 `entity_id` 反查 Neo4j
- **Runtime** 不入 Neo4j，通过 ServiceInstance 缓存字段注入 Context：
  ```
  K8s API / Actuator ──实时查询──▶ Context Builder ──注入──▶ ServiceInstance.{cpu_usage, pod_phase, ...}
  ```
- **Reasoning** 会话内有效；AI 确认结论正确后升级为永久 Knowledge

### 通信统一

- 全链路使用 **gRPC + Bolt**，消除 HTTP REST / 自定义帧 / subprocess
- dt CLI daemon 是唯一 gRPC 入口，MCP Server 作为 gRPC client
- 插件通过 Plugin trait 注册到 dt daemon，遵守 6 条强制约束

### 代码定位

- 代码不是架构的中心，而是 Knowledge 的附件
- 查询路径：任务 → 匹配 Knowledge/Playbook → 定位相关 Entity → 找到 Code
- 不再是：搜索代码 → 找到 Method → 结束

### 关系价值

- Graph 的价值不在存储节点，而在存储**世界之间的联系**
- 统一关系模型：CALLS、DEPENDS_ON、HAS_INSTANCE、RUNS_AS、IMPLEMENTED_BY、AFFECTS 等
- 关系是跨世界的：一个 Knowledge 节点可以通过 `IMPLEMENTED_BY` 关联到 Code Entity

### 扩展性

- **Neo4j schemaless**：加属性不需要 migration，加标签不需要 schema change
- **trait 先行**：所有扩展通过实现已有 trait（GraphRepository、SyncSource、EventHandler、Plugin）完成
- **ServiceInstance 是扩展枢纽**：任何与环境相关的字段都挂在 ServiceInstance 上
- 详见 [数据格式文档第十节：扩展指南](./architecture-v2-data-schema.md#十扩展指南如何新增实体关系属性)

---

## 四、核心组件：Context Builder

Context Builder 是整个平台的大脑，负责从六世界中切出当前任务需要的"世界切片"。

```
输入：用户任务描述
  ↓
Context Builder (Chain of Responsibility 模式):
  ┌─────────────────────────────────────────────────────────┐
  │ Stage 1: Retriever — 并行查询六世界                       │
  │   Reality   → Neo4j: 相关代码、服务实例、配置              │
  │   Knowledge → Neo4j: 领域概念、业务术语                    │
  │   Memory    → Neo4j: 历史相似任务、踩坑记录                │
  │   Semantic  → Qdrant: 文档向量检索                        │
  │   Runtime   → K8s API/Actuator: 实时拉取                  │
  │              → 注入 ServiceInstance 缓存字段               │
  │   Reasoning → Neo4j: 之前对此类任务的分析                  │
  ├─────────────────────────────────────────────────────────┤
  │ Stage 2: Ranker  — 按语义相关度排序，过滤低分结果 (<0.5)    │
  │ Stage 3: Dedup   — 合并重复信息，保留来源引用               │
  │ Stage 4: Resolver— 检测冲突（如配置 vs 代码不一致），标记    │
  │ Stage 5: Summarizer — 超 token 预算时压缩，保留关键证据     │
  └─────────────────────────────────────────────────────────┘
  ↓
输出：聚合 Task Context (JSON) → 注入 LLM
```

---

## 五、MCP 接口

LLM 通过 OpenCode MCP Protocol 调用以下 12 个高层 MCP 工具（含 4 个运维工具），底层全部走 gRPC：

| MCP | 功能 | 底层 gRPC |
|-----|------|-----------|
| `dt_context` | 聚合返回任务所需全部上下文（六世界切片） | `DtCoreService.BuildContext` |
| `dt_plan` | 根据任务自动生成执行计划（匹配 Playbook） | `DtCoreService.GeneratePlan` |
| `dt_domain` | 返回某一业务领域的知识模型 | `DtCoreService.QueryDomain` |
| `dt_history` | 检索历史相似任务与修改记录 | `DtCoreService.QueryHistory` |
| `dt_dependency` | 返回调用链、依赖关系、影响范围 | `DtCoreService.QueryDependency` |
| `dt_verify` | 修改完成后验证受影响配置/数据库/接口一致性 | `DtCoreService.Verify` |
| `dt_learn` | 将本次修改/经验/决策写回知识图谱 | `DtCoreService.Learn` |
| `dt_search` | 跨世界语义搜索（代码/知识/文档） | `DtCoreService.Search` |

底层运维 + 系统工具由插件暴露，LLM 也可直接调用：

| 插件 | gRPC Service | 提供的 RPC |
|------|-------------|-----------|
| plugin_k8s | `K8sService` | GetPods, GetLogs (stream), DownloadLogs, GetStatus |
| plugin_svc | `SvcService` | ListServices, GetStatus, Start (stream), Stop, Restart, GetLogs (stream) |
| plugin_jenkins | `JenkinsService` | ListJobs, GetParams, GetHistory, Build (stream), GetBuildLog (stream) |

| 系统工具 | CLI | 功能 |
|----------|-----|------|
| dt_backup | `dt backup [--restore\|--list\|--verify]` | Neo4j/Qdrant/SQLite 分层备份与灾难恢复 |
| dt_archive | `dt archive [--before\|--dry-run\|--list]` | Memory World 超期数据归档（.json.gz） |
| dt_cleanup | `dt cleanup [--dry-run\|--execute]` | 按 TTL 策略自动清理过期数据 |
| dt_metrics | `dt metrics [--watch\|--interval]` | gRPC MetricsService 查询，不暴露 HTTP |

---

## 六、数字主线（Digital Thread）

Digital Thread 是跨六世界的横切层，将**同一条业务主线**上分散在各世界的事件串联起来，形成完整的演化链。

### 问题

一次业务需求（如"支付平台从通联迁移到银盛"）会散落在不同世界：

- **Reality**：代码改了 5 个文件、Nacos 配置变了、ServiceInstance 版本更新
- **Memory**：记录了 Modification + Deployment（→ServiceInstance）事件
- **Knowledge**：沉淀了迁移经验和 Playbook
- **Semantic**：保存了相关设计文档的向量
- **Reasoning**：保存了迁移分析过程的决策链路

但它们彼此孤立，缺少一条主线将它们关联在一起。

### 设计

```
(:Thread)                          ← 业务主线
  ├── name:        "支付平台迁移：通联 → 银盛"
  ├── created_at:  2026-07-09
  ├── status:      进行中 / 已完成 / 已归档
  │
  ├── [:HAS_REQUIREMENT] → (:Requirement)     ← 需求定义
  ├── [:HAS_SESSION]     → (:Session)         ← 多个会话
  ├── [:HAS_DECISION]    → (:Decision)        ← 架构决策
  ├── [:HAS_MODIFICATION]→ (:Modification)    ← 代码变更
  ├── [:HAS_DEPLOYMENT]  → (:Deployment)      ← 部署记录
  │                            └─[:DEPLOYS]→ (:ServiceInstance)
  ├── [:HAS_KNOWLEDGE]   → (:Knowledge)       ← 沉淀的经验
  └── [:HAS_PLAYBOOK]    → (:Playbook)        ← 生成的执行手册

回溯版本链：
(:Knowledge)-[:EVOLVED_FROM*]->(:Knowledge)        ← 知识版本演化
(:KnowledgeVersion)                                 ← 每次变更的 diff
```

### 价值

有了 Thread，AI 不仅能回答：

> **"支付平台怎么迁移？"**

还能回答：

> **"这次迁移为什么这样设计？经历了哪些讨论？哪些代码、配置、部署（哪个环境？哪个版本？）和经验属于同一个任务？"**

Thread 把六个世界从**"六张独立的快照"**变成**"跨时间、跨系统、跨知识的完整演化链"**。

---

## 七、技术分层总览

```
┌─────────────────────────────────────────────────────┐
│  Interface    gRPC services + MCP + CLI              │
├─────────────────────────────────────────────────────┤
│  Application  BuildService, SyncService,             │
│               ContextService, MemoryService, ...     │
├─────────────────────────────────────────────────────┤
│  Domain       types, traits, entities, rules         │
├─────────────────────────────────────────────────────┤
│  Infrastructure  Neo4jRepo, QdrantRepo,              │
│                  NacosClient, K8sClient, dt-log      │
├─────────────────────────────────────────────────────┤
│  Cross-cutting   Security (mTLS+SecretString),       │
│                  Metrics (gRPC, no HTTP),            │
│                  Backup (分层快照)                    │
└─────────────────────────────────────────────────────┘
```

详见 [项目结构设计文档](./architecture-v2-project-structure.md)。
