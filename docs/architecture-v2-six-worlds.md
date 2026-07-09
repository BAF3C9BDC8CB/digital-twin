# Digital Twin v2 架构设计：六世界模型

> 状态：设计阶段 | 日期：2026-07-09

## 一、整体架构

```
                         ┌─────────────────────────────────┐
                         │         Digital Twin v2          │
                         │      (Agent-Native Platform)     │
                         └──────────────┬──────────────────┘
                                        │
              ┌─────────────────────────┼─────────────────────────┐
              │                         │                         │
     ┌────────▼────────┐     ┌─────────▼──────────┐     ┌───────▼────────┐
     │  Reality World  │     │  Knowledge World   │     │  Memory World  │
     │  (事实/资源)     │     │  (知识/概念)        │     │  (历史/经验)    │
     └────────┬────────┘     └─────────┬──────────┘     └───────┬────────┘
              │                         │                         │
     ┌────────▼────────┐     ┌─────────▼──────────┐     ┌───────▼────────┐
     │  Runtime World  │     │  Semantic World    │     │ Reasoning World│
     │  (实时状态)      │     │  (向量/相似度)      │     │  (AI推理缓存)   │
     └────────┬────────┘     └─────────┬──────────┘     └───────┬────────┘
              │                         │                         │
              └─────────────────────────┼─────────────────────────┘
                                        │
                         ┌──────────────▼──────────────┐
                         │      Context Builder        │
                         │   (世界切片 → 任务上下文)      │
                         └──────────────┬──────────────┘
                                        │
                         ┌──────────────▼──────────────┐
                         │      MCP Interface          │
                         │  dt_context / dt_plan / ... │
                         └──────────────┬──────────────┘
                                        │
                         ┌──────────────▼──────────────┐
                         │           LLM               │
                         └─────────────────────────────┘
```

## 二、六个世界

| 世界 | 存储 | 内容 | 特征 |
|------|------|------|------|
| **Reality** | Neo4j | 代码、服务、数据库、服务器、K8s、配置、API | 客观存在，可被自动发现 |
| **Knowledge** | Neo4j | 领域概念、业务术语、技术知识、架构模式 | 人类整理或 AI 自动沉淀 |
| **Memory** | Neo4j | 修改记录、部署事件、Bug修复、会话摘要、经验 | 时间线驱动，只增不删 |
| **Runtime** | 缓存/K8s API | CPU、内存、Pod状态、连接数、JVM指标 | 实时查询，不入库 |
| **Semantic** | Qdrant | 所有文本的向量（代码、文档、日志、Issue、Chat） | 相似度检索 |
| **Reasoning** | Neo4j（会话级） | Decision Graph：假设→证据→结论→置信度。AI 推理过程、模式发现、影响分析、决策链路 | AI 生成，验证后可升级为 Knowledge；未验证的会话结束后降级 |

### Reasoning World 深入：Decision Graph

Reasoning World 不仅是"AI 推理缓存"，更是一个 **Decision Graph（决策图谱）**，记录 AI 的每一次分析和决策链路：

```
(:Decision)           ← AI 做出的选择（如"为什么用 Redis 而不是本地缓存？"）
  ├── context         ← 当时面临的问题
  ├── alternatives    ← 考虑过的候选方案
  ├── evidence        ← 支撑决策的证据（文档、代码、历史）
  ├── choice          ← 最终选择
  ├── confidence      ← 置信度 (0.0~1.0)
  └── verified        ← 是否已被验证为正确

(:Observation)        ← AI 发现但尚未得出结论的模式
  示例："Module A 和 Module B 结构高度相似"
  示例："Payment 和 Refund 共用同一套 RedisLock 模式"
  这类观察不是 Knowledge（未被验证），也不是 Memory（不是事实事件）

验证 → 升级为 (:Knowledge)
未验证 → 会话结束降级或丢弃
```

**生命周期：**
```
Observation（AI 发现模式/异常）
    ↓ 分析验证
Decision（AI 做出选择/判断）
    ↓ 执行确认
Knowledge（验证正确，永久保留）
```

## 三、核心原则

### World 关系
- **Reality + Knowledge + Memory** 是 Neo4j 中三类 Entity，通过统一的关系模型连接
- **Semantic** 是 Qdrant 中的向量，通过 `entity_id` 反查 Neo4j
- **Runtime** 不入库，由 MCP 实时查询后注入 Context
- **Reasoning** 是 AI 推理痕迹，会话内有效；AI 确认结论正确后升级为永久 Knowledge

### 代码定位
- 代码不是架构的中心，而是 Knowledge 的附件
- 查询路径：任务 → 匹配 Knowledge/Playbook → 定位相关 Entity → 找到 Code
- 不再是：搜索代码 → 找到 Method → 结束

### 关系价值
- Graph 的价值不在存储节点，而在存储**世界之间的联系**
- 统一关系模型（CALLS、DEPENDS_ON、BELONGS_TO、CONFIGURES 等）
- 关系是跨世界的：一个 Knowledge 节点可以通过 `IMPLEMENTED_BY` 关联到 Code Entity

## 四、核心组件：Context Builder

Context Builder 是整个平台的大脑，负责从六世界中切出当前任务需要的"世界切片"。

```
输入：用户任务描述
  ↓
Context Builder:
  1. 解析意图 → 确定涉及哪些 World
  2. 查询 Reality → 相关代码、服务、配置
  3. 查询 Knowledge → 领域概念、业务术语
  4. 查询 Memory → 历史相似任务、踩坑记录
  5. 查询 Semantic → 相关文档、设计记录
  6. 查询 Runtime → 当前服务状态
  7. 组装 Reasoning → AI 之前对此类任务的分析
  ↓
组装完成后进入压缩管道（避免 Context 爆炸）：
  8. Ranker    → 按相关度排序，过滤噪声
  9. Dedup     → 合并重复信息，保留来源
  10. Resolver  → 检测冲突信息（如两处记录互斥），标记或消解
  11. Summarize → 对大规模结果做摘要压缩，保留关键证据
  ↓
输出：聚合 Task Context → 注入 LLM
```

## 五、MCP 接口

最终 LLM 只看到 8 个高层 MCP（替代现有底层工具）：

| MCP | 功能 |
|-----|------|
| `dt_context` | 聚合返回任务所需全部上下文（六世界切片） |
| `dt_plan` | 根据任务自动生成执行计划（匹配 Playbook） |
| `dt_domain` | 返回某一业务领域的知识模型 |
| `dt_history` | 检索历史相似任务与修改记录 |
| `dt_dependency` | 返回调用链、依赖关系、影响范围 |
| `dt_verify` | 修改完成后验证受影响配置/数据库/接口一致性 |
| `dt_learn` | 将本次修改/经验/决策写回知识图谱 |
| `dt_search` | 语义搜索代码（保留，但不再是第一步） |

## 六、数字主线（Digital Thread）

Digital Thread 是跨六世界的横切层，将**同一条业务主线**上分散在各世界的事件串联起来，形成完整的演化链。

### 问题

一次业务需求（如"支付平台从通联迁移到银盛"）会散落在不同世界：

- Reality：代码改了 5 个文件、Nacos 配置变了
- Memory：记录了修改事件、部署记录
- Knowledge：沉淀了迁移经验和 Playbook
- Semantic：保存了相关设计文档的向量
- Reasoning：保存了迁移分析过程的决策链路

但它们彼此孤立，缺少一条主线将它们关联在一起。

### 设计

```
(:Thread)                          ← 业务主线
  ├── name:        "支付平台迁移：通联 → 银盛"
  ├── created_at:  2026-07-09
  ├── status:      进行中 / 已完成 / 已归档
  │
  ├── [:HAS_REQUIREMENT] → (:Requirement)
  ├── [:HAS_SESSION]     → (:Session)      ← 多个会话
  ├── [:HAS_DECISION]    → (:Decision)     ← 架构决策
  ├── [:HAS_MODIFICATION]→ (:Modification) ← 代码变更
  ├── [:HAS_DEPLOYMENT]  → (:Deployment)   ← 部署记录
  ├── [:HAS_KNOWLEDGE]   → (:Knowledge)    ← 沉淀的经验
  └── [:HAS_PLAYBOOK]    → (:Playbook)     ← 生成的执行手册
```

### 价值

有了 Thread，AI 不仅能回答：

> **"支付平台怎么迁移？"**

还能回答：

> **"这次迁移为什么这样设计？经历了哪些讨论？哪些代码、配置、部署和经验属于同一个任务？"**

Thread 把六个世界从**"六张独立的快照"**变成**"跨时间、跨系统、跨知识的完整演化链"**。
