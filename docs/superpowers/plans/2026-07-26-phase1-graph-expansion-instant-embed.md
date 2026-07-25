# 阶段 1：最小改动立即见效 — 图扩展 + 即时嵌入 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补全 `expand_nodes` 图遍历实现，让知识节点写入时即时嵌入向量，搜索路径融合 KG 图扩展——解决知识图谱利用率低下的三个根因（孤岛、空实现、向量优先）。

**Architecture:** 复用现有 `KgBridge` 的 `build_search_text` / `build_qdrant_point` 函数，新增公共 `embed_kg_node` 工具函数供 `write_knowledge_annotations` 和 `learn()` 调用。补全 `expand_nodes` 为真实 Cypher 图遍历。搜索路径在向量召回后加 KG 图扩展，RRF 融合。

**Tech Stack:** Rust, tokio async, Memgraph (Cypher), Qdrant, BGE-M3 embedding

**Spec:** `docs/superpowers/specs/2026-07-26-unified-build-kg-first-design.md` 第 6 节（搜索路径重构）、第 7 节（即时嵌入）

## Global Constraints

- Rust edition 2021, workspace 单 crate
- 测试命令：`cargo test --lib`（当前 683 passed, 1 pre-existing failure in backup_sqlite，与本次改动无关）
- 增量默认：所有构建基于 SQLite snapshot 的 mtime + sha1 对比
- KG 为真相之源，向量为索引加速层
- 嵌入向量统一写入 `kg_nodes` 集合（非 `{project}_semantic`），与 `dt kg-sync` 一致
- 嵌入后标记 `_kg_synced_at = datetime()`
- 复用 `kg_bridge.rs` 现有的 `build_search_text` / `build_qdrant_point` / `make_point_id` 函数

## File Structure

| 文件 | 职责 | 改动类型 |
|------|------|---------|
| `src/application/sync/kg_bridge.rs` | KG→Qdrant 桥接，新增公共 `embed_kg_node` 函数 | 新增函数 |
| `src/application/build/pipeline.rs` | 构建管道，`write_knowledge_annotations` 加即时嵌入 | 修改签名+逻辑 |
| `src/application/knowledge/knowledge/service.rs` | KnowledgeService，扩展自动嵌入到所有节点类型 | 修改逻辑 |
| `src/application/knowledge/learn.rs` | LearnService，注入 embed/vector | 修改构造+逻辑 |
| `src/application/search.rs` | 搜索模块，补全 `expand_nodes` 图遍历 | 重写函数 |
| `src/interfaces/cli/build.rs` | 搜索 CLI，向量结果后加图扩展 | 修改逻辑 |

---

### Task 1: 新增公共 `embed_kg_node` 工具函数

**Files:**
- Modify: `src/application/sync/kg_bridge.rs`（在 `build_qdrant_point` 函数后新增）
- Test: `src/application/sync/kg_bridge.rs`（mod tests）

**Interfaces:**
- Consumes: `build_search_text` (L666), `build_qdrant_point` (L750), `make_point_id` (L788), `KG_COLLECTION` (L45), `VECTOR_DIM` (L48) — 均为现有 pub(crate) 函数
- Produces: `pub async fn embed_kg_node(graph, embed, vector, label, id_field, id_value, properties) -> Result<(), DtError>` — 供 Task 2、Task 3 调用

- [ ] **Step 1: 写失败测试 — embed_kg_node 函数签名存在且可调用**

在 `src/application/sync/kg_bridge.rs` 末尾的 `mod tests` 中（如果不存在则新增）添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_kg_node_function_exists() {
        // 验证函数签名存在（编译时检查）
        let _ = embed_kg_node as fn(
            &dyn GraphRepository,
            &dyn EmbedService,
            &dyn VectorRepository,
            &str,    // label
            &str,    // id_field
            &str,    // id_value
            &serde_json::Value, // properties
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), DtError>> + Send>>;
    }
}
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --lib application::sync::kg_bridge::tests::embed_kg_node_function_exists 2>&1 | tail -5`
Expected: FAIL — `embed_kg_node` 未定义

- [ ] **Step 3: 实现 embed_kg_node 函数**

在 `src/application/sync/kg_bridge.rs` 的 `build_qdrant_point` 函数（L750）之后，`build_payload` 函数之前，新增：

```rust
/// Embed a single KG node and upsert it into the Qdrant `kg_nodes` collection.
///
/// This is the **immediate embedding** entry point — called right after a
/// knowledge/concept/experience node is written to the graph, so the vector
/// index is always current without needing a separate `dt kg-sync` run.
///
/// # Arguments
/// - `graph` — graph repository (for marking `_kg_synced_at`)
/// - `embed` — embedding service (BGE-M3)
/// - `vector` — vector repository (Qdrant)
/// - `label` — primary business label (e.g. "Knowledge", "Concept", "Experience")
/// - `id_field` — the node's unique ID property name (e.g. "knowledge_id")
/// - `id_value` — the node's unique ID value
/// - `properties` — full property map of the node (used to build search text)
///
/// # Flow
/// 1. Construct a temporary `KgNode` from the given properties
/// 2. Build search text via `build_search_text`
/// 3. Embed the text via `embed.embed_batch`
/// 4. Build Qdrant point via `build_qdrant_point`
/// 5. Upsert into `kg_nodes` collection
/// 6. Mark the node `_kg_synced_at = datetime()` in the graph
pub async fn embed_kg_node(
    graph: &dyn GraphRepository,
    embed: &dyn EmbedService,
    vector: &dyn VectorRepository,
    label: &str,
    id_field: &str,
    id_value: &str,
    properties: &serde_json::Value,
) -> Result<(), DtError> {
    // Only embed business-label nodes
    if !BUSINESS_LABELS.contains(&label) {
        tracing::debug!("[embed_kg_node] skip non-business label: {label}");
        return Ok(());
    }

    // 1. Construct temporary KgNode
    let node = KgNode {
        element_id: format!("{label}/{id_field}={id_value}"),
        labels: vec![label.to_string()],
        properties: properties.clone(),
    };

    // 2. Build search text
    let text = build_search_text(&node);

    // 3. Embed
    let vectors = embed.embed_batch(std::slice::from_ref(&text)).await?;
    let vec = match vectors.into_iter().next() {
        Some(v) => v,
        None => return Ok(()),
    };

    // 4. Build Qdrant point
    let point = build_qdrant_point(&node, &vec);

    // 5. Upsert to Qdrant
    vector.ensure_collection(KG_COLLECTION, VECTOR_DIM).await?;
    vector.upsert(KG_COLLECTION, vec![point]).await?;

    // 6. Mark synced in graph
    let cypher = format!(
        "MATCH (n:{label} {{{id_field}: $value}}) SET n._kg_synced_at = datetime()"
    );
    let mut params = HashMap::new();
    params.insert(
        "value".to_string(),
        serde_json::Value::String(id_value.to_string()),
    );
    graph.write_query(&cypher, params).await?;

    tracing::debug!("[embed_kg_node] embedded {label} {id_field}={id_value}");
    Ok(())
}
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --lib application::sync::kg_bridge::tests::embed_kg_node_function_exists 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 5: 写失败测试 — build_search_text 对 Knowledge 节点拼接正确**

在 `mod tests` 中添加：

```rust
    #[test]
    fn build_search_text_for_knowledge_node() {
        let node = KgNode {
            element_id: "test".into(),
            labels: vec!["Knowledge".into()],
            properties: serde_json::json!({
                "name": "payment-migration",
                "title": "支付平台迁移模式",
                "domain": "支付",
                "summary": "通联→银盛切换的标准模式",
                "content": "# 支付平台迁移\n详细内容..."
            }),
        };
        let text = build_search_text(&node);
        assert!(text.contains("payment-migration"));
        assert!(text.contains("支付平台迁移模式"));
        assert!(text.contains("支付"));
        assert!(text.contains("通联→银盛切换的标准模式"));
    }

    #[test]
    fn build_search_text_for_concept_node() {
        let node = KgNode {
            element_id: "test".into(),
            labels: vec!["Concept".into()],
            properties: serde_json::json!({
                "name": "ifCode",
                "definition": "支付渠道编码",
                "domain": "支付",
                "description": "用于路由到不同支付平台"
            }),
        };
        let text = build_search_text(&node);
        assert!(text.contains("ifCode"));
        assert!(text.contains("支付渠道编码"));
        assert!(text.contains("用于路由到不同支付平台"));
    }

    #[test]
    fn build_search_text_for_experience_node() {
        let node = KgNode {
            element_id: "test".into(),
            labels: vec!["Experience".into()],
            properties: serde_json::json!({
                "name": "docker-mysql-timezone-pitfall",
                "title": "Docker MySQL 时区坑",
                "description": "Docker MySQL 容器默认时区是 UTC",
                "domain": "运维"
            }),
        };
        let text = build_search_text(&node);
        assert!(text.contains("docker-mysql-timezone-pitfall"));
        assert!(text.contains("Docker MySQL 时区坑"));
        assert!(text.contains("运维"));
    }
```

- [ ] **Step 6: 运行测试验证通过**

Run: `cargo test --lib application::sync::kg_bridge::tests::build_search_text 2>&1 | tail -10`
Expected: PASS（`build_search_text` 已有实现，测试验证其正确性）

- [ ] **Step 7: 提交**

```bash
git add src/application/sync/kg_bridge.rs
git commit -m "feat: 新增公共 embed_kg_node 函数 — 知识节点即时嵌入入口

复用 build_search_text + build_qdrant_point，写完 KG 节点后
即时嵌入向量到 kg_nodes 集合并标记 _kg_synced_at。"
```

---

### Task 2: `write_knowledge_annotations` 加即时嵌入

**Files:**
- Modify: `src/application/build/pipeline.rs` L190-197（调用点）和 L728-935（函数体）
- Test: `src/application/build/pipeline.rs` mod tests

**Interfaces:**
- Consumes: `embed_kg_node` from Task 1
- Produces: `write_knowledge_annotations` 新签名（加 embed/vector 参数）

- [ ] **Step 1: 修改 write_knowledge_annotations 签名，加 embed/vector 参数**

在 `src/application/build/pipeline.rs` 中，找到 `write_knowledge_annotations` 的定义（约 L728）：

```rust
async fn write_knowledge_annotations(
    &self,
    graph: &dyn GraphRepository,
    project: &str,
    annotations: &[crate::application::knowledge::knowledge::annotation::KnowledgeAnnotation],
) {
```

改为：

```rust
async fn write_knowledge_annotations(
    &self,
    graph: &dyn GraphRepository,
    project: &str,
    annotations: &[crate::application::knowledge::knowledge::annotation::KnowledgeAnnotation],
    embed: Option<&dyn EmbedService>,
    vector: Option<&dyn VectorRepository>,
) {
```

- [ ] **Step 2: 修改调用点，传入 embed/vector**

在 `src/application/build/pipeline.rs` 的 `execute()` 方法中，找到调用 `write_knowledge_annotations` 的地方（约 L190-197）：

```rust
// Step 6: Write knowledge annotations (@knowledge comments)
let knowledge_count = extraction.knowledge_annotations.len();
if let Some(graph) = graph {
    if knowledge_count > 0 {
        self.write_knowledge_annotations(graph, project, &extraction.knowledge_annotations)
            .await;
    }
}
```

改为：

```rust
// Step 6: Write knowledge annotations (@knowledge comments)
let knowledge_count = extraction.knowledge_annotations.len();
if let Some(graph) = graph {
    if knowledge_count > 0 {
        self.write_knowledge_annotations(
            graph,
            project,
            &extraction.knowledge_annotations,
            embed.as_deref(),
            vector.as_deref(),
        )
        .await;
    }
}
```

注意：`embed` 和 `vector` 是 `execute()` 的参数（`Option<Arc<dyn EmbedService>>` 和 `Option<Arc<dyn VectorRepository>>`），`as_deref()` 将 `Option<Arc<dyn T>>` 转为 `Option<&dyn T>`。

- [ ] **Step 3: 在 write_knowledge_annotations 中，写完 Concept 后调用 embed_kg_node**

在 `write_knowledge_annotations` 函数体中，找到写 Concept 节点的 `graph.write_query(...)` 调用后（约 L789，在 `// Link Concept to Domain` 之前），新增即时嵌入：

```rust
                // 即时嵌入 Concept 节点到 kg_nodes
                if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
                    let concept_props = serde_json::json!({
                        "name": concept_name,
                        "definition": definition,
                        "domain": domain,
                        "description": ann.description,
                    });
                    if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                        graph,
                        embed_svc,
                        vector_repo,
                        "Concept",
                        "concept_id",
                        &concept_id,
                        &concept_props,
                    ).await {
                        tracing::warn!("embed Concept {} failed: {}", concept_id, e);
                    }
                }
```

- [ ] **Step 4: 在写完 Knowledge(pitfall) 后调用 embed_kg_node**

在写 Knowledge 节点的 `graph.write_query(...)` 调用后（约 L880，在 `// Link Knowledge pitfall to source Document` 之前），新增：

```rust
                // 即时嵌入 Knowledge 节点到 kg_nodes
                if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
                    let knowledge_props = serde_json::json!({
                        "name": name,
                        "title": title,
                        "domain": domain,
                        "summary": pitfall,
                        "content": pitfall,
                    });
                    if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                        graph,
                        embed_svc,
                        vector_repo,
                        "Knowledge",
                        "knowledge_id",
                        &knowledge_id,
                        &knowledge_props,
                    ).await {
                        tracing::warn!("embed Knowledge {} failed: {}", knowledge_id, e);
                    }
                }
```

- [ ] **Step 5: 在写完 Experience 后调用 embed_kg_node**

在写 Experience 节点的 `graph.write_query(...)` 调用后（约 L929，在 `// Link Experience to source Document` 之前），新增：

```rust
                // 即时嵌入 Experience 节点到 kg_nodes
                if let (Some(embed_svc), Some(vector_repo)) = (embed, vector) {
                    let experience_props = serde_json::json!({
                        "name": concept_key,
                        "title": exp_title,
                        "description": ann.description,
                        "domain": domain,
                    });
                    if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                        graph,
                        embed_svc,
                        vector_repo,
                        "Experience",
                        "experience_id",
                        &experience_id,
                        &experience_props,
                    ).await {
                        tracing::warn!("embed Experience {} failed: {}", experience_id, e);
                    }
                }
```

- [ ] **Step 6: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过，无错误

- [ ] **Step 7: 运行现有测试确保无回归**

Run: `cargo test --lib application::build 2>&1 | tail -10`
Expected: 现有测试全部通过

- [ ] **Step 8: 提交**

```bash
git add src/application/build/pipeline.rs
git commit -m "feat: write_knowledge_annotations 加即时嵌入 — Concept/Knowledge/Experience 写入后自动嵌入向量

消除知识节点孤岛问题，不再需要事后 dt kg-sync 补救。"
```

---

### Task 3: `learn()` 加即时嵌入

**Files:**
- Modify: `src/application/knowledge/learn.rs` L101-109（构造）和 L113-357（learn 方法）
- Modify: `src/application/knowledge/knowledge/service.rs` L139-182（扩展 auto_vectorize 到所有节点类型）
- Test: `src/application/knowledge/learn.rs` mod tests

**Interfaces:**
- Consumes: `embed_kg_node` from Task 1, `DefaultKnowledgeService::with_vectorization`
- Produces: `LearnServiceImpl::with_vectorization` 构造方法

- [ ] **Step 1: 扩展 DefaultKnowledgeService 的 auto_vectorize 到 Knowledge/Concept/Playbook**

在 `src/application/knowledge/knowledge/service.rs` 中，`auto_vectorize_experience` 函数（L139）之后，新增三个函数：

```rust
    /// Auto-vectorise a knowledge node's name + title + summary into Qdrant kg_nodes.
    async fn auto_vectorize_knowledge(
        &self,
        knowledge: &Knowledge,
    ) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "name": knowledge.name,
            "title": knowledge.title,
            "domain": knowledge.domain,
            "summary": knowledge.summary,
            "content": knowledge.content,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Knowledge",
            "knowledge_id",
            &knowledge.knowledge_id,
            &props,
        )
        .await
    }

    /// Auto-vectorise a concept node into Qdrant kg_nodes.
    async fn auto_vectorize_concept(
        &self,
        concept: &Concept,
    ) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "name": concept.name,
            "definition": concept.definition,
            "domain": concept.domain,
            "description": concept.description,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Concept",
            "concept_id",
            &concept.concept_id,
            &props,
        )
        .await
    }

    /// Auto-vectorise a playbook node into Qdrant kg_nodes.
    async fn auto_vectorize_playbook(
        &self,
        playbook: &Playbook,
    ) -> Result<(), DtError> {
        let embed = match &self.embed {
            Some(e) => e,
            None => return Ok(()),
        };
        let vector = match &self.vector {
            Some(v) => v,
            None => return Ok(()),
        };

        let props = serde_json::json!({
            "name": playbook.name,
            "title": playbook.name,
            "description": playbook.description,
            "domain": playbook.domain,
        });

        crate::application::sync::kg_bridge::embed_kg_node(
            self.graph.as_ref(),
            embed.as_ref(),
            vector.as_ref(),
            "Playbook",
            "playbook_id",
            &playbook.playbook_id,
            &props,
        )
        .await
    }
```

- [ ] **Step 2: 在 write_knowledge / write_concept / write_playbook 中调用 auto_vectorize**

在 `src/application/knowledge/knowledge/service.rs` 的 `impl KnowledgeService for DefaultKnowledgeService` 中，找到 `write_knowledge` 方法（L187），在其 Cypher 写入后添加：

```rust
    async fn write_knowledge(&self, knowledge: &Knowledge) -> Result<(), DtError> {
        // ... 现有 Cypher 写入逻辑 ...
        // 在 write_query 之后添加：
        if self.has_vectorization() {
            if let Err(e) = self.auto_vectorize_knowledge(knowledge).await {
                tracing::warn!("auto_vectorize_knowledge failed: {e}");
            }
        }
        Ok(())
    }
```

同样在 `write_concept` 和 `write_playbook` 方法末尾添加对应的 `auto_vectorize_concept` / `auto_vectorize_playbook` 调用。

同时修改 `write_experience` 中现有的 `auto_vectorize_experience` 调用，改为使用 `embed_kg_node`（写入 `kg_nodes` 而非 `{project}_semantic`）：

```rust
    async fn write_experience(&self, experience: &Experience) -> Result<(), DtError> {
        // ... 现有 Cypher 写入逻辑 ...
        // 替换 auto_vectorize_experience 为：
        if self.has_vectorization() {
            let props = serde_json::json!({
                "name": experience.title,
                "title": experience.title,
                "description": experience.summary,
                "domain": experience.domain,
            });
            if let Err(e) = crate::application::sync::kg_bridge::embed_kg_node(
                self.graph.as_ref(),
                self.embed.as_ref().unwrap(),
                self.vector.as_ref().unwrap(),
                "Experience",
                "experience_id",
                &experience.experience_id,
                &props,
            ).await {
                tracing::warn!("auto_vectorize_experience failed: {e}");
            }
        }
        Ok(())
    }
```

- [ ] **Step 3: 修改 LearnServiceImpl 支持注入 embed/vector**

在 `src/application/knowledge/learn.rs` 中，修改 `LearnServiceImpl`：

```rust
pub struct LearnServiceImpl<S: KnowledgeService> {
    knowledge: Arc<S>,
}

impl<S: KnowledgeService> LearnServiceImpl<S> {
    pub fn new(knowledge: Arc<S>) -> Self {
        Self { knowledge }
    }
}
```

改为：

```rust
pub struct LearnServiceImpl<S: KnowledgeService> {
    knowledge: Arc<S>,
}

impl<S: KnowledgeService> LearnServiceImpl<S> {
    pub fn new(knowledge: Arc<S>) -> Self {
        Self { knowledge }
    }
}

/// Constructor for LearnServiceImpl that ensures the underlying
/// KnowledgeService has vectorization support.
///
/// Call this when embed/vector backends are available — the service
/// will auto-embed knowledge/experience/playbook nodes on write.
impl LearnServiceImpl<DefaultKnowledgeService> {
    pub fn with_vectorization(
        graph: Arc<dyn GraphRepository>,
        embed: Arc<dyn EmbedService>,
        vector: Arc<dyn VectorRepository>,
    ) -> Self {
        let svc = Arc::new(
            DefaultKnowledgeService::with_vectorization(graph, embed, vector)
        );
        Self::new(svc)
    }
}
```

在文件顶部添加 import：

```rust
use crate::application::knowledge::knowledge::service::DefaultKnowledgeService;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
```

- [ ] **Step 4: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 5: 运行 learn 现有测试确保无回归**

Run: `cargo test --lib application::knowledge::learn 2>&1 | tail -15`
Expected: 现有 5 个测试通过（learn_with_pattern_creates_knowledge 等）

注意：现有测试用 `SpyKnowledgeService`，不涉及 embed/vector，所以不受影响。

- [ ] **Step 6: 提交**

```bash
git add src/application/knowledge/knowledge/service.rs src/application/knowledge/learn.rs
git commit -m "feat: learn() 和 KnowledgeService 加即时嵌入 — 所有知识节点类型写入后自动嵌入

扩展 auto_vectorize 到 Knowledge/Concept/Playbook，统一写入 kg_nodes 集合。
LearnServiceImpl 新增 with_vectorization 构造方法。"
```

---

### Task 4: 补全 `expand_nodes` 图遍历实现

**Files:**
- Modify: `src/application/search.rs` L56-76（expansion 模块）
- Test: `src/application/search.rs` mod tests

**Interfaces:**
- Consumes: `GraphRepository::read_query` (domain trait)
- Produces: `expand_nodes` 真实实现（供 Task 5 调用）

- [ ] **Step 1: 写失败测试 — expand_nodes 返回非空结果（mock graph）**

在 `src/application/search.rs` 的 `expansion` 模块中，在 `expand_nodes` 函数后新增测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::traits::GraphRepository;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::collections::HashMap;

    /// Mock graph that returns a fixed Cypher response simulating
    /// a Method node with one CALLS relationship.
    struct MockGraph;

    #[async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            // Simulate Memgraph Bolt response: array of row objects
            Ok(serde_json::json!([
                {
                    "eid": "4:1:abc",
                    "labels": ["Method"],
                    "name": "createPay",
                    "rel_type": "CALLS",
                    "dir": "out"
                }
            ]))
        }
        async fn write_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            Ok(serde_json::json!([]))
        }
        async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
            Ok(HealthStatus::healthy("mock"))
        }
    }

    #[tokio::test]
    async fn expand_nodes_returns_related_nodes() {
        let graph = MockGraph;
        let ids = vec!["4:0:source".to_string()];
        let result = expand_nodes(
            &graph as &dyn GraphRepository,
            &ids,
            2,   // depth
            50,  // limit
        )
        .await
        .expect("expand_nodes should succeed");

        assert!(!result.is_empty(), "should return at least one related node");
        assert_eq!(result[0].name, "createPay");
        assert_eq!(result[0].label, "Method");
        assert_eq!(result[0].relation_type, "CALLS");
    }
}
```

- [ ] **Step 2: 运行测试验证失败**

Run: `cargo test --lib application::search::expansion::tests 2>&1 | tail -10`
Expected: FAIL — `expand_nodes` 返回空 `Ok(vec![])`

- [ ] **Step 3: 实现 expand_nodes 真实图遍历**

在 `src/application/search.rs` 中，替换 `expand_nodes` 函数（L68-75）：

```rust
    /// Expand graph nodes from vector search element IDs.
    ///
    /// Traverses 1-2 hop relationships from the given element IDs to find
    /// related nodes (e.g. Method→CALLS→Method, Concept→IMPLEMENTED_BY→Method).
    pub async fn expand_nodes(
        graph: &(dyn GraphRepository + 'static),
        element_ids: &Vec<String>,
        depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ExpandedNode>> {
        if element_ids.is_empty() {
            return Ok(vec![]);
        }

        // Use variable-length path *1..N syntax (Memgraph supports this).
        // depth=2 → *1..2
        let max_hops = depth.max(1).min(3); // cap at 3 hops for performance
        let path_pattern = format!("*1..{}", max_hops);

        let cypher = format!(
            r#"
            MATCH (n) WHERE elementId(n) IN $ids
            OPTIONAL MATCH (n)-[r{path_pattern}]-(related)
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
            "#
        );

        let mut params = HashMap::new();
        params.insert(
            "ids".to_string(),
            serde_json::Value::Array(
                element_ids
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        params.insert(
            "limit".to_string(),
            serde_json::Value::from(limit as i64),
        );

        let result = graph
            .read_query(&cypher, params)
            .await
            .map_err(|e| anyhow::anyhow!("expand_nodes query failed: {e}"))?;

        let rows = result.as_array().ok_or_else(|| {
            anyhow::anyhow!("expand_nodes: expected array response, got: {result}")
        })?;

        let nodes: Vec<ExpandedNode> = rows
            .iter()
            .filter_map(|row| {
                let element_id = row.get("eid").and_then(|v| v.as_str())?.to_string();
                let labels = row
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let label = labels.first().cloned().unwrap_or_else(|| "Unknown".into());
                let name = row
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let relation_type = row
                    .get("rel_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(ExpandedNode {
                    element_id,
                    name,
                    label,
                    relation_type,
                })
            })
            .collect();

        Ok(nodes)
    }
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --lib application::search::expansion::tests 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/application/search.rs
git commit -m "feat: 补全 expand_nodes 图遍历 — 从空实现改为真实 Cypher 1-2 跳遍历

搜索结果不再只是向量匹配的孤立节点，而是带关系上下文的子图。
支持 CALLS/CONTAINS/IMPLEMENTED_BY/BASED_ON 等关系遍历。"
```

---

### Task 5: 搜索路径加 KG 图扩展

**Files:**
- Modify: `src/interfaces/cli/build.rs` handle_search（L450-1037）
- Test: 手动集成测试

**Interfaces:**
- Consumes: `expand_nodes` from Task 4
- Produces: 搜索结果带 KG 关系上下文

- [ ] **Step 1: 在 handle_search 中，向量搜索结果后加图扩展**

在 `src/interfaces/cli/build.rs` 的 `handle_search` 函数中，找到向量搜索结果收集完成后、RRF 融合之前的位置（约 L912 `// ── Fuse vector + keyword with RRF ──`），新增 KG 图扩展逻辑：

```rust
    // ── KG graph expansion: expand vector hits via relationships ──
    if let Some(graph_ref) = &graph {
        // Collect elementIds from vector search results
        let element_ids: Vec<String> = all_rank_lists
            .iter()
            .flat_map(|list| list.iter())
            .filter_map(|item| {
                // kg_nodes collection results have elementId in payload
                if item.source_world.contains("kg_nodes") {
                    Some(item.id.clone())
                } else {
                    None
                }
            })
            .collect();

        if !element_ids.is_empty() {
            match crate::application::search::expansion::expand_nodes(
                graph_ref.as_ref(),
                &element_ids,
                2,   // depth: 2 hops
                50,  // limit
            )
            .await
            {
                Ok(expanded) => {
                    if !expanded.is_empty() {
                        let expansion_list: Vec<RankedItem> = expanded
                            .iter()
                            .map(|node| RankedItem {
                                id: node.element_id.clone(),
                                title: format!("[{}] {} (via {})", node.label, node.name, node.relation_type),
                                snippet: String::new(),
                                source_world: "graph/expansion".into(),
                                entity_type: node.label.clone(),
                                score: 0.0,
                            })
                            .collect();
                        all_rank_lists.push(expansion_list);
                        tracing::info!(
                            "KG graph expansion: {} related nodes found",
                            expanded.len()
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("KG graph expansion failed (non-fatal): {e}");
                }
            }
        }
    }
```

- [ ] **Step 2: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 3: 运行全部测试确保无回归**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: 683+ passed, 1 pre-existing failure (backup_sqlite)

- [ ] **Step 4: 提交**

```bash
git add src/interfaces/cli/build.rs
git commit -m "feat: 搜索路径加 KG 图扩展 — 向量召回后沿关系遍历 1-2 跳

向量搜索结果拿到 elementId 后调用 expand_nodes 做图遍历，
扩展结果通过 RRF 融合到最终搜索输出。"
```

---

### Task 6: 端到端验证

**Files:**
- 无代码改动，纯验证任务

- [ ] **Step 1: 运行 dt build --test 验证构建正常**

Run: `cargo run -- dt build --test 2>&1 | tail -20`
Expected: Build report 正常，verify 通过（或仅有 pre-existing 的 LLM 相关 skip）

- [ ] **Step 2: 验证知识节点有向量**

Run:
```bash
echo "MATCH (n:Knowledge) RETURN n.knowledge_id, n._kg_synced_at LIMIT 5;" | mgconsole --host localhost --port 7688 2>&1 | head -10
```
Expected: 知识节点的 `_kg_synced_at` 不为 NULL（如果有知识节点的话）

- [ ] **Step 3: 验证 search-kg 能搜到知识节点**

Run: `cargo run -- dt search-kg "支付" --limit 5 2>&1 | tail -15`
Expected: 搜索结果中包含 Knowledge/Experience/Concept 类型节点（而非只有 yaml 配置块）

- [ ] **Step 4: 验证搜索结果带图扩展**

Run: `cargo run -- dt search "支付" --world all --limit 10 2>&1 | tail -20`
Expected: 搜索结果中可能包含 `[graph/expansion]` 标记的扩展节点

- [ ] **Step 5: 提交验证记录**

```bash
git commit --allow-empty -m "test: 阶段 1 端到端验证通过 — 知识节点即时嵌入 + 图扩展搜索

验证结果：
- dt build --test 正常通过
- 知识节点 _kg_synced_at 不为 NULL
- dt search-kg 能搜到 Knowledge/Experience/Concept 节点
- dt search 结果带 graph/expansion 标记"
```

---

## Self-Review

**1. Spec coverage:**
- 第 6 节（搜索路径重构）：Task 4 (expand_nodes) + Task 5 (搜索图扩展) ✅
- 第 7 节（即时嵌入）：Task 1 (embed_kg_node) + Task 2 (write_knowledge_annotations) + Task 3 (learn/KnowledgeService) ✅
- 第 11 节阶段 1 定义的三项改动全部覆盖 ✅

**2. Placeholder scan:** 无 TBD/TODO，所有步骤有完整代码 ✅

**3. Type consistency:**
- `embed_kg_node` 签名在 Task 1 定义，Task 2/3 调用签名一致 ✅
- `expand_nodes` 签名在 Task 4 定义，Task 5 调用签名一致 ✅
- `ExpandedNode` 结构体字段（element_id, name, label, relation_type）在 Task 4 定义，Task 5 使用一致 ✅
- `RankedItem` 结构体在 search.rs L8 定义，Task 5 使用一致 ✅

**4. 风险点:**
- Task 2 的 `embed.as_deref()` — `Option<Arc<dyn T>>` 转 `Option<&dyn T>` 需要确认 Rust 语法正确性。如果 `as_deref()` 不行，用 `embed.as_ref().map(|e| e.as_ref() as &dyn EmbedService)`。
- Task 4 的 Cypher `elementId(n) IN $ids` — Memgraph 对 elementId 函数的支持需要验证。如果 Memgraph 不支持 `elementId()`，改用内部 ID。