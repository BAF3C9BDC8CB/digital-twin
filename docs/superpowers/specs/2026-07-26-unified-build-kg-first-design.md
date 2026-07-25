# 统一构建 + KG 优先架构设计

**日期**: 2026-07-26
**状态**: Draft
**作者**: Loom (coordinator) + 用户协作设计

## 1. 背景与问题诊断

### 1.1 当前知识图谱利用率低下的根因

通过对 `dt build`、`dt kg-sync`、`dt search` 三个核心命令的源码追踪，定位到三个结构性问题：

**问题 1：写入路径割裂（build vs kg-sync）**

```
dt build 的写入路径：
  代码 → KG(Method/Class) + Qdrant({project}_methods)     ✅ 有向量
  文档 → KG(Document) + Qdrant({project}_semantic)        ✅ 有向量
  @knowledge注释 → KG(Concept/Knowledge/Experience)        ❌ 无向量
  learn → KG(Knowledge/Experience/Playbook)                 ❌ 无向量

dt kg-sync 的写入路径：
  KG业务节点 → 读出来 → 嵌入 → Qdrant(kg_nodes)            ✅ 补向量
```

`build` 写的知识节点不带向量，必须再跑一次 `kg-sync` 才能被语义搜索。实际几乎没人记得跑第二步，于是知识节点成了"KG 里有、向量里没有"的孤岛。

**问题 2：搜索路径向量优先（KG 退化为元数据存储）**

`handle_search`（build.rs L450-1037）的逻辑：先走 Qdrant 向量搜索（主路径），向量没结果才 fallback 到 KG Cypher。`expand_nodes()`（search.rs L68-75）是空实现 `Ok(vec![])`，图遍历能力完全废弃。KG 的核心价值——关系遍历（Method→CALLS→Method、Concept→IMPLEMENTED_BY→Method、Decision→BASED_ON→Knowledge）——被彻底埋没。

**问题 3：project 级别隔离 vs 通用知识**

Qdrant 集合按 `{project}_xxx` 组织，KG 节点都带 `project` 属性，搜索时 `WHERE n.project = 'xxx'`。但很多知识本质是跨项目的：架构决策影响多个项目、踩坑经验可复用、基础设施是环境级、通用文档描述整个系统。强行绑定 project 导致这些知识无法被跨项目检索。

### 1.2 实测数据

KG 节点分布（2026-07-26 实测）：

| 标签 | 数量 | 有 embedding | 可被向量搜索 |
|------|------|-------------|------------|
| DocumentChunk | 543 | ✅ | ✅（但都是代码/yaml 块） |
| Method | 90 | ✅ | ✅ |
| Entity | 70 | ❌ | ❌ |
| Document | 26 | ❌ | ❌（19 个是 yaml 配置） |
| Class | 26 | ❌ | ❌ |
| Concept | 4 | ❌ | ❌ |
| Domain | 4 | ❌ | ❌ |
| Experience | 2 | ❌ | ❌ |
| Knowledge | 2 | ❌ | ❌ |

完全缺失的节点类型（`event-hooks.yaml` 定义了但 KG 中 0 个）：`Modification` / `Deployment` / `JenkinsJob` / `JenkinsBuild` / `ServiceInstance` / `ConfigChange` / `BugFix` / `Decision` / `Conversation` / `PodEvent` / `K8sSyncEvent` / `NacosConfig` / `NacosService` / `Infrastructure` / `Server` / `Database`。

`search-kg` 返回分数全是 `0.0164`，因为向量库里只有代码块和方法，Knowledge/Experience/Concept/Decision 这些真正有价值的知识节点根本没被嵌入向量。

## 2. 设计目标

1. **统一命令入口**：合并 `dt build` 和 `dt kg-sync` 为单一 `dt build` 命令，按源类型分发，知识节点写入即时嵌入，不再需要二次同步。
2. **KG 为真相之源**：写入先 KG 后向量，搜索先 KG 图遍历后向量召回。向量是索引加速层，不是主存储。
3. **去 project 中心化**：`project` 是节点的来源标签，不是隔离边界。知识按领域/环境/集群组织，支持跨项目关系。
4. **增量默认**：所有构建默认增量，`--full` 才是全量重建。
5. **build --test 同路径**：测试命令与生产 build 走完全相同的代码路径，唯一区别是处理 `test/` 目录。
6. **LLM 后置异步**：LLM 分析不阻塞 build 主流程，提交到后台队列处理。
7. **多厂商 Provider**：硅基流动作为一个厂商实现，同时支持本地 xInference，LLM 可降级跳过。

## 3. 统一命令架构

### 3.1 命令接口

```bash
# 默认：增量构建，自动识别源类型
dt build                              # 索引当前目录（自动识别）
dt build --path ./myproject           # 索引指定项目代码
dt build --source doc --path ./docs   # 只索引文档
dt build --source infra               # 同步基础设施（Nacos + K8s）
dt build --source knowledge            # 补嵌入所有 _kg_synced_at IS NULL 的节点

# 批量与全量
dt build --all                        # 索引 config.yaml 中所有项目
dt build --full                        # 全量重建（绕过增量快照）

# 测试（与生产同路径）
dt build --test                        # 增量构建 test/ 目录 + verify
dt build --test --full                 # 全量重建测试数据
dt clean --test                        # 清空 test-pipeline 的所有数据

# LLM 状态
dt llm-status                         # 查看各项目 LLM 分析进度
dt build --llm-sync                    # 显式阻塞处理所有 pending LLM 任务
```

### 3.2 增量默认原则

所有构建默认增量，具体机制因源类型而异：

| 源类型 | 增量机制 | 全量触发 |
|--------|---------|---------|
| code | SQLite snapshot 的 mtime + sha1 对比，跳过未变更文件 | `--full` |
| doc | 同 code，基于文件 mtime | `--full` |
| infra | Nacos/K8s 同步本身是 upsert 语义，天然增量 | `--full` 强制重新拉取 |
| knowledge | 只处理 `_kg_synced_at IS NULL` 的节点 | `--full` 重新嵌入所有知识节点 |

### 3.3 内部架构：SourceRouter + 统一管道

```
dt build
  │
  ├─ SourceRouter::route(path, source_flag)
  │   ├─ 识别源类型：检查路径内容 / 显式 --source
  │   └─ 返回 Vec<Box<dyn SourceExtractor>>
  │
  ├─ SourceExtractor trait（每种源类型实现一个）
  │   fn extract(&self) -> Vec<KgNodePayload>
  │   fn label(&self) -> &str
  │   fn incremental_key(&self) -> IncrementalKey   // 增量判定依据
  │
  │   ├─ CodeExtractor     → 扫描 .java/.py/.rs → Method/Class/Module + 关系
  │   ├─ DocExtractor      → 扫描 .md/.txt/.yaml → Document/DocumentChunk
  │   ├─ InfraExtractor    → Nacos API + K8s API → NacosConfig/Server/ServiceInstance
  │   └─ KnowledgeExtractor → 扫描 KG 中 _kg_synced_at IS NULL 的节点 → 补向量
  │
  ├─ UnifiedWritePipeline（每种源类型共享）
  │   for each extractor:
  │   │   1. write_to_graph(nodes, relationships)     ← 先写 KG（真相之源）
  │   │   2. embed_and_index(nodes)                    ← 再嵌入向量（加速层）
  │   │   │   ├─ build_search_text(node) → 语义文本
  │   │   │   ├─ embed_provider.embed_batch(texts) → vectors
  │   │   │   └─ vector_repo.upsert(collection, points)
  │   │   3. mark_synced(node_ids)                    ← SET _kg_synced_at = now()
  │   │   4. submit_llm_jobs(nodes)                   ← LLM 任务提交到后台队列（非阻塞）
  │   │
  │   └─ BuildReport 汇总各源类型写入数量 + LLM pending 数量
  │
  └─ 保留现有 BuildStrategy（full/incremental）用于代码文件的 mtime 增量
```

### 3.4 保留的独立命令

| 命令 | 保留原因 | 变化 |
|------|---------|------|
| `dt nacos-sync` | 从 Nacos API 拉取数据，是"数据获取"而非"索引" | 内部复用 InfraExtractor，写入后自动嵌入向量 |
| `dt k8s-sync` | 从 K8s API 拉取数据，同上 | 同上 |
| `dt event` | 写事件节点（Modification/Deployment 等） | 写入后自动嵌入向量 |
| `dt memorize` | 写决策/知识节点 | 写入后自动嵌入向量 |
| `dt learn` | 写经验/Playbook | 写入后自动嵌入向量 |

**合并的命令**：`dt kg-sync` 合并到 `dt build --source knowledge`。`dt kg-sync` 保留但标记 deprecated，内部转发到 `dt build --source knowledge`。

## 4. 通用 KG 节点模型（去 project 中心化）

### 4.1 节点分类与归属维度

按"归属维度"而非"project"组织节点：

| 节点类型 | 归属维度 | 隔离属性 |
|---------|---------|---------|
| Method/Class | project | `n.project = "offen-pay"` |
| Document（项目文档） | project | `n.project = "offen-pay"` |
| Document（通用文档） | global | `n.project = null` |
| Decision | global | `n.project = null`, `n.scope` |
| Experience | domain | `n.domain = "支付"` |
| Knowledge | domain | `n.domain = "支付"` |
| Concept | domain | `n.domain = "支付"` |
| Playbook | domain | `n.domain = "支付"` |
| NacosConfig | env | `n.env = "prod"` |
| NacosService | env | `n.env = "prod"` |
| Server | cluster | `n.cluster = "prod"` |
| ServiceInstance | env | `n.env = "prod"` |
| Deployment | env + job | `n.env`, `n.job` |
| Modification | project | `n.project = "offen-pay"` |
| BugFix | project | `n.project = "offen-pay"` |

核心原则：`project` 只是节点的"来源标签"之一，不是隔离边界。搜索时 project 是可选过滤条件，不是强制边界。

### 4.2 跨项目关系

通用知识节点可以关联到多个项目的代码：

```cypher
(:Decision {title: "支付平台用 ifCode+wayCode 拆分"})
  ├─[:AFFECTS]->(:Method {project: "offen-pay", name: "createPay"})
  └─[:AFFECTS]->(:Method {project: "doctor-center", name: "payCallback"})

(:Experience {domain: "支付", summary: "Docker MySQL 时区坑"})
  └─[:APPLIES_TO]->(:NacosConfig {env: "prod", data_id: "mysql.yml"})
```

这些跨项目关系是"通用知识图谱"的核心价值——知识不再被困在单个项目里。

## 5. Qdrant 集合重组

### 5.1 集合映射

| 当前（project 隔离） | 目标（全局 + payload 标签） |
|---------------------|---------------------------|
| `{project}_methods` | `code_methods`（全局，payload 带 project） |
| `{project}_semantic` | `doc_chunks`（全局，payload 带 project） |
| `{project}_knowledge` | 并入 `kg_nodes` |
| `kg_nodes` | `kg_nodes`（保留，全局业务节点） |
| `config_chunks` | 并入 `kg_nodes` |

搜索时通过 payload 过滤 project（可选），而非遍历多个集合。

### 5.2 搜索时的 project 过滤

```rust
// project 是可选过滤，不是集合选择
fn search_code(query_vec, project: Option<&str>) {
    let filter = project.map(|p| json!({"project": p}));
    vec_repo.search_with_filter("code_methods", query_vec, filter)
}
```

### 5.3 迁移策略

旧集合保留一段时间，新写入走新集合，搜索同时查新旧集合并融合，验证稳定后删除旧集合。具体迁移步骤见第 11 节。

## 6. 搜索路径重构：KG 优先 + 向量加速

### 6.1 新搜索流程

```
dt search "支付回调逻辑" --world all
  │
  ├─ 1. KG 图搜索（主路径，并行启动）
  │   ├─ Cypher 精确匹配
  │   │   MATCH (n) WHERE (n:Method OR n:Knowledge OR n:Decision)
  │   │   AND (n.name CONTAINS $q OR n.summary CONTAINS $q)
  │   │   RETURN n, labels(n)
  │   │
  │   └─ expand_nodes 图扩展（补全实现后）
  │       从命中节点出发，沿关系遍历 1-2 跳：
  │       (:Method)-[:CALLS]->(:Method)
  │       (:Concept)-[:IMPLEMENTED_BY]->(:Method)
  │       (:Decision)-[:BASED_ON]->(:Knowledge)
  │       (:Experience)-[:APPLIES_TO]->(:NacosConfig)
  │       → 返回扩展节点列表
  │
  ├─ 2. 向量召回（加速路径，并行启动）
  │   └─ Qdrant 语义搜索 → 返回 elementId + score
  │       → 拿 elementId 回 KG 取完整节点 + 关系
  │
  ├─ 3. RRF 融合两路结果
  │   └─ reciprocal_rank_fusion(kg_results, vector_results)
  │
  └─ 4. 返回融合后的结果（带 KG 关系上下文）
```

### 6.2 expand_nodes 补全实现

当前 `search.rs::expand_nodes`（L68-75）是空实现 `Ok(vec![])`。补全为真实图遍历：

```rust
pub async fn expand_nodes(
    graph: &dyn GraphRepository,
    element_ids: &Vec<String>,
    depth: usize,      // 默认 2 跳
    limit: usize,      // 默认 50
) -> anyhow::Result<Vec<ExpandedNode>> {
    // depth=2 → 用 *1..2 变长路径语法（Memgraph 支持）
    let cypher = r#"
        MATCH (n) WHERE elementId(n) IN $ids
        OPTIONAL MATCH (n)-[r*1..2]-(related)
        WITH n, r AS path
        UNWIND path AS rel
        WITH n, rel, startNode(rel) AS sn, endNode(rel) AS en
        WITH n,
             CASE WHEN sn = n THEN endNode(rel) ELSE startNode(rel) END AS related,
             type(rel) AS rel_type,
             CASE WHEN sn = n THEN 'out' ELSE 'in' END AS dir
        RETURN DISTINCT elementId(related) AS eid, labels(related) AS labels,
               coalesce(related.name, related.title, '') AS name,
               collect(DISTINCT rel_type)[0] AS rel_type, dir
        LIMIT $limit
    "#;
    // 执行查询，返回扩展节点
}
```

这是"重心回到 KG"的关键——搜索结果不再只是向量匹配的孤立节点，而是带关系上下文的子图。

### 6.3 search-kg 重构

当前 `search-kg`：向量优先，关键词兜底。改为：KG Cypher + 图扩展优先，向量召回加速，RRF 融合。

## 7. 即时嵌入（解决知识节点孤岛）

### 7.1 write_knowledge_annotations 改造

当前 `pipeline.rs::write_knowledge_annotations`（L728-935）只写 KG，不嵌入。改造为写 KG 后即时嵌入：

```rust
async fn write_knowledge_annotations(&self, graph, project, annotations) {
    for ann in annotations {
        // 写 Concept/Knowledge/Experience 到 KG（现有逻辑）
        graph.write_query(...);

        // 新增：即时嵌入向量
        if let (Some(embed_svc), Some(vector_repo)) = (&self.embed, &self.vector) {
            let search_text = build_search_text(&node);  // 拼接 name+summary+definition
            let vec = embed_svc.embed_batch(&[search_text]).await?;
            let point = json!({
                "id": hash(&node_id),
                "vector": vec[0],
                "payload": {
                    "labels": ["Knowledge"],
                    "name": node.name,
                    "summary": node.summary,
                    "domain": node.domain,
                    "project": project,
                    "element_id": node_id,
                }
            });
            vector_repo.upsert("kg_nodes", vec![point]).await?;
            // 标记已同步
            graph.write_query(
                "MATCH (n) WHERE elementId(n) = $eid SET n._kg_synced_at = $now",
                ...
            ).await?;
        }
    }
}
```

### 7.2 learn() 改造

`learn.rs::LearnServiceImpl::learn()` 写 Knowledge/Experience/Playbook 后，同样调用 `embed_and_index`，确保所有新写入的节点即时有向量。

### 7.3 event/memorize 改造

`dt event` 和 `dt memorize` 写节点后，同样调用 `embed_and_index`。

## 8. build --test 与生产 build 完全同路径

### 8.1 设计原则

`build --test` 必须走与生产 `build` **完全相同的代码路径**（SourceRouter → UnifiedWritePipeline），唯一区别是：
- `path` 指向 `test/` 目录
- `project` 名固定为 `"test-pipeline"`
- 执行后自动运行 `verify_test_data` 对比 `expected.json`

### 8.2 当前问题

当前 `build --test`（main.rs L1325-1396）设 `pipeline=false`，跳过了 LLM 分析和完整管道，与生产 build 路径有差异。这导致测试无法覆盖生产环境的完整流程。

### 8.3 改造

```
dt build --test 的执行流程：
  1. 连接真实后端（Memgraph/Qdrant/SQLite/embed）—— fail fast，不降级
  2. 调用 handle_build(
       path = "test/",
       name = "test-pipeline",
       full = false,        // 增量（首次无 snapshot → 处理所有文件）
       pipeline = true,     // ← 关键：启用完整管道，包括 LLM 后置
     )
  3. verify_test_data() 对比 expected.json
  4. 打印 TestReport，失败则 exit(1)
```

关键变化：当前 `build --test` 设 `pipeline=false`（跳过 LLM），改为 `pipeline=true`，走完整管道。LLM 分析改为后置异步（第 9 节），不阻塞 verify。

### 8.4 清空命令

```bash
dt clean --test
```

清空 `test-pipeline` 的所有数据：
- 删除 KG 中 `project="test-pipeline"` 的所有节点
- 删除 Qdrant 中 `test-pipeline` 相关集合
- 删除 SQLite 中 `test-pipeline` 的 snapshots 和 LLM 进度

当前 `cleanup.rs` 已有部分实现，需对齐新的集合结构。

### 8.5 expected.json 维护

`test/expected.json` 是 ground truth。当 `test/` 目录内容变化时需要更新：

```bash
dt build --test --update-expected   # 重新生成 expected.json（谨慎使用）
```

## 9. LLM 分析后置异步处理

### 9.1 问题

当前 Phase 2 LLM 分析（pipeline.rs L289-438）同步阻塞在 `execute()` 中，`buffer_unordered(5)` 并发但整体等待所有方法分析完才返回。大项目可能有数百个方法，每个方法一次 LLM 调用，总耗时可能几十分钟，用户被迫等待，build 命令看起来"卡住"。

### 9.2 设计：LLM 分析从同步改为异步后置

```
build 主流程（同步，快速返回）：
  1. 扫描文件 → 提取实体
  2. write_to_graph()          ← KG 写入（快）
  3. embed_and_index()         ← 向量嵌入（快，BGE-M3 批量）
  4. mark_synced()             ← 标记完成
  5. submit_llm_jobs()         ← 提交 LLM 分析任务到后台队列（非阻塞）
  6. 返回 BuildReport          ← 立即返回，不等待 LLM

LLM 后置处理（异步，后台执行）：
  - LlmAnalysisWorker 从队列取任务
  - 每个方法：调用 LLM → 嵌入 LLM 结果 → 更新 Qdrant 的 llm_analysis 字段
  - 进度持久化到 SQLite（is_llm_analyzed / mark_llm_analyzed）
  - 失败重试 3 次，最终失败记录到日志
  - 下次 build 时跳过已分析且 hash 未变的方法
```

### 9.3 任务队列设计

```rust
// 新增：LLM 分析任务队列（SQLite 持久化，防止进程退出丢失）
pub struct LlmAnalysisQueue {
    // 字段：method_id, project, source_hash, status(pending/done/failed), attempts, created_at
}

// build 主流程提交任务后立即返回
pipeline.submit_llm_jobs(&methods).await?;  // 非阻塞

// 后台 worker（独立线程或 tokio task）
LlmAnalysisWorker::run(queue, llm_provider, embed_provider, vector_repo)
    .await;  // 持续运行，处理队列中的任务
```

### 9.4 运行模式

**模式 1：build 内置 worker（默认，CLI 场景）**
- build 命令启动一个后台 tokio task 处理 LLM 队列
- build 命令的 `handle_build` 函数返回（打印 BuildReport），但 CLI 进程不立即退出，后台 task 继续处理 LLM 队列直到完成或用户 Ctrl+C
- BuildReport 中包含 `llm_pending` 数量，用户可看到"42 methods submitted for background LLM analysis"
- 适合 CLI 一次性使用：用户看到 build 完成后可以继续其他操作，LLM 在后台静默处理

**模式 2：daemon 模式（长期运行）**
- dt-daemon 后台常驻，持续处理 LLM 队列
- build 命令只提交任务，立即返回
- 适合服务化部署

**模式 3：显式处理（手动）**
- `dt build --llm-sync`：显式阻塞处理所有 pending 的 LLM 任务
- 适合需要立即完成 LLM 分析的场景

### 9.5 进度可见性

```bash
dt llm-status
# 输出：
# LLM Analysis Status:
#   test-pipeline: 42 total, 15 done, 27 pending, 0 failed
#   offen-pay: 128 total, 128 done, 0 pending
```

### 9.6 与 build --test 的协同

`build --test` 走完整管道（`pipeline=true`），LLM 任务提交到后台队列。`verify_test_data` 不等待 LLM 完成，只验证 KG/Qdrant 的结构正确性。LLM 分析的 `llm_analysis` 字段在 `expected.json` 中标记为可选（`has_llm_analysis_on_methods` 可为 false），测试不因 LLM 未完成而失败。

## 10. 多厂商 Provider 支持

### 10.1 问题

当前 `SiliconFlowClient` 是唯一实现，硬编码了 SiliconFlow 的 URL 和模型。需要：
- 硅基流动作为一个厂商实现
- 同时支持本地 xInference 接口
- 本地接口可能无法支持所有三个模型（Qwen3-14B LLM、BAAI/bge-reranker-v2-m3 重排、BAAI/bge-m3 嵌入）
- 能力降级：嵌入和重排必须（搜索依赖），LLM 可选（后置处理，不阻塞 build）

### 10.2 Provider 抽象设计

```rust
// 新增 trait：统一 LLM 服务接口
#[async_trait]
pub trait LlmService: Send + Sync + 'static {
    async fn chat(&self, system_prompt: &str, user_input: &str, temperature: f32, max_tokens: u32) -> Result<String, DtError>;
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
    fn capabilities(&self) -> LlmCapabilities;
}

// 新增 trait：重排服务接口
#[async_trait]
pub trait RerankService: Send + Sync + 'static {
    async fn rerank(&self, query: &str, documents: &[String], top_k: usize) -> Result<Vec<RerankResult>, DtError>;
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

// 能力声明
pub struct LlmCapabilities {
    pub embed: bool,      // 支持嵌入
    pub rerank: bool,     // 支持重排
    pub chat: bool,       // 支持 LLM 对话
    pub max_tokens: u32,  // 最大 token 数
}

// Provider 枚举
pub enum Provider {
    SiliconFlow(SiliconFlowProvider),
    XInference(XInferenceProvider),
    // 未来可扩展：Ollama, OpenAI, etc.
}
```

### 10.3 两个 Provider 实现

**SiliconFlowProvider（云端，全功能）**
- URL: `https://api.siliconflow.cn/v1`
- 嵌入: BAAI/bge-m3 ✅
- 重排: BAAI/bge-reranker-v2-m3 ✅
- LLM: Qwen3-14B 或配置的模型 ✅
- 能力: `{ embed: true, rerank: true, chat: true }`

**XInferenceProvider（本地，部分功能）**
- URL: `http://localhost:9997/v1`（OpenAI 兼容接口）
- 嵌入: BAAI/bge-m3 ✅（本地部署）
- 重排: BAAI/bge-reranker-v2-m3 ✅（本地部署）
- LLM: Qwen3-14B ❌（本地可能未部署，或显式禁用）
- 能力: `{ embed: true, rerank: true, chat: false }`
- 降级：LLM 分析跳过，不影响 build 主流程

### 10.4 配置驱动

```yaml
# config.yaml
services:
  # 默认 provider（嵌入 + 重排必须，LLM 可选）
  default:
    embed_provider: xinterence      # 嵌入用本地
    rerank_provider: xinterence     # 重排用本地
    llm_provider: siliconflow       # LLM 用云端（或 null 表示禁用）

  # 或者统一用一个 provider
  # provider: xinterence            # 全部用本地（LLM 会降级跳过）

  siliconflow:
    url: "https://api.siliconflow.cn/v1"
    api_key: "sk-xxx"
    model_embed: "BAAI/bge-m3"
    model_reranker: "BAAI/bge-reranker-v2-m3"
    model_llm: "Qwen/Qwen3-14B"

  xinterence:
    url: "http://localhost:9997/v1"
    api_key: ""                     # 本地通常不需要
    model_embed: "BAAI/bge-m3"
    model_reranker: "BAAI/bge-reranker-v2-m3"
    model_llm: null                  # 显式禁用 LLM
```

### 10.5 能力降级策略

```
build 流程中的能力检查：

  1. embed_provider 必须可用 → 否则 build 失败（向量是核心索引）
  2. rerank_provider 可选 → 搜索时降级为无重排
  3. llm_provider 可选：
     - 有 LLM 能力 → 提交 LLM 分析任务到后台队列（第 9 节）
     - 无 LLM 能力 → 跳过 LLM 分析，build 正常完成
     - 方法节点的 llm_analysis 字段为空，不影响搜索

  能力检查在启动时执行：
    dt health → 显示各 provider 的能力状态
    dt build  → 启动时检查 embed 能力，LLM 能力仅警告不阻塞
```

### 10.6 与 LLM 后置处理的协同

```
LLM 后置处理 + 多厂商支持：

  build 主流程：
    1. 用 embed_provider 嵌入向量（必须）
    2. 检查 llm_provider.capabilities().chat
       - true  → 提交 LLM 任务到后台队列
       - false → 跳过，日志记录 "LLM analysis skipped (no LLM provider)"
    3. 返回 BuildReport（含 llm_pending 数量）

  后台 LLM worker：
    - 从队列取任务
    - 调用 llm_provider.chat()
    - 如果 llm_provider 为 null 或无 chat 能力 → 标记任务为 skipped
    - 否则执行分析 → 更新 Qdrant llm_analysis 字段
```

## 11. 迁移策略（分阶段，降低风险）

### 阶段 1：最小改动，立即见效

- 补全 `expand_nodes` 实现为真实图遍历
- `write_knowledge_annotations` / `learn` 加即时嵌入
- 搜索路径加 KG 图扩展（不改集合结构）

**效果**：知识节点不再孤岛，搜索有图扩展，重心开始回 KG。

### 阶段 2：合并命令

- 实现 `SourceRouter` + `SourceExtractor` trait
- `KnowledgeExtractor` 吸收 `KgBridge::sync_incremental` 逻辑
- `dt build --source knowledge` 替代 `dt kg-sync`
- `dt kg-sync` 标记 deprecated（保留但提示用 `build --source knowledge`）

**效果**：单一命令入口，不再遗漏同步。

### 阶段 3：LLM 后置 + 多厂商

- 实现 `LlmAnalysisQueue`（SQLite 持久化）
- LLM 分析从同步改为异步后置
- 实现 `LlmService` / `RerankService` trait
- 实现 `XInferenceProvider`
- 配置驱动 provider 选择

**效果**：build 不再阻塞，支持本地 xInference。

### 阶段 4：build --test 同路径

- `build --test` 改为 `pipeline=true`，走完整管道
- `dt clean --test` 对齐新集合结构
- `expected.json` 维护命令

**效果**：测试覆盖完整生产流程。

### 阶段 5：集合重组

- 新写入走全局集合（`code_methods` / `doc_chunks` / `kg_nodes`）
- 搜索同时查新旧集合，RRF 融合
- 验证稳定后，迁移旧 `{project}_xxx` 集合数据到全局集合
- 删除旧集合

**效果**：去 project 隔离，跨项目搜索。

### 阶段 6：去 project 中心化

- Decision/Experience/Knowledge 节点去掉 project 强绑定
- 按领域(domain)/环境(env)/集群(cluster)组织
- 跨项目关系（AFFECTS 等）建立

**效果**：通用知识图谱。

### 向后兼容

- `dt kg-sync` 保留但标记 deprecated，内部转发到 `dt build --source knowledge`
- 旧 Qdrant 集合在迁移期保留，搜索同时查新旧
- KG 节点的 `project` 属性保留，只是不再作为强制隔离边界

## 12. 测试策略

| 层级 | 测试内容 |
|------|---------|
| 单元 | SourceExtractor 各实现的 `extract()` 输出、`build_search_text` 拼接、`expand_nodes` Cypher 正确性、Provider 能力声明 |
| 集成 | 统一管道 `write_to_graph → embed_and_index → mark_synced` 全流程、即时嵌入验证、LLM 队列提交与后台处理 |
| 搜索 | KG 优先 + 向量加速的 RRF 融合结果、图扩展深度 2 跳正确性、跨项目搜索 |
| 迁移 | 新旧集合双写期数据一致性、旧集合数据迁移完整性 |
| build --test | 与生产 build 同路径验证、verify_test_data 对比、clean --test 清空完整性 |
| Provider | SiliconFlow 全功能、XInference 嵌入+重排、XInference LLM 降级跳过 |
| LLM 后置 | 队列持久化、worker 处理、失败重试、进度查询 |

## 13. 架构总览

```
                    ┌─────────────────────────────────────────────────┐
                    │       dt build (统一入口，默认增量)                │
                    ├─────────────────────────────────────────────────┤
                    │  SourceRouter → UnifiedWritePipeline             │
                    │  1. write_to_graph (KG, 真相之源)                 │
                    │  2. embed_and_index (向量, 加速层, embed_provider) │ ← 第10节
                    │  3. submit_llm_jobs (后置, 可选, llm_provider)     │ ← 第9节
                    │  4. 返回 BuildReport (不阻塞)                      │
                    └─────────────────────────────────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    │                               │
              build --test                    build (生产)
            (第8节: 同路径)               (默认增量)
            path=test/                    path=项目目录
            + verify_test_data
                                    │
                        ┌───────────┴───────────┐
                        │  LLM 后置 worker       │
                        │  (第9节: 异步)          │
                        │  从队列取任务           │
                        │  调用 llm_provider     │ ← 第10节
                        │  更新 Qdrant            │
                        └───────────────────────┘

搜索路径（第6节）：
  KG 图遍历（主） + 向量召回（加速） → RRF 融合 → 带关系上下文的结果
```

## 14. 设计决策记录

| 决策 | 选择 | 理由 |
|------|------|------|
| 命令合并 | 合并 build + kg-sync | 消除割裂，知识节点即时嵌入 |
| KG vs 向量重心 | KG 为真相之源，向量为索引层 | 关系遍历是 KG 核心价值，向量只是快速召回 |
| project 隔离 | 去中心化，project 变为标签 | 通用知识本质跨项目 |
| 增量策略 | 默认增量 | 大项目全量重建成本高，增量是日常需求 |
| build --test | 与生产同路径 | 测试必须覆盖完整生产流程 |
| LLM 分析 | 后置异步 | LLM 调用慢，不应阻塞 build 主流程 |
| Provider 抽象 | 多厂商，能力降级 | 支持本地 xInference，LLM 可选 |
| 集合重组 | 全局集合 + payload 标签 | 去隔离，跨项目搜索 |
| 迁移方式 | 分阶段 | 降低风险，每阶段可独立验证 |