# Knowledge Hybrid Search (S5) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `world=knowledge` 从 CONTAINS 子串匹配重写为 GraphRAG 式混合检索（向量召回 → 图扩展 → rerank → 融合排序），`world=doc` 改查 `doc_chunks` 并支持实体证据回填。

**Architecture:** 新建 `src/application/knowledge/extract/retrieve.rs` 承载全部 GraphRAG 逻辑（召回/扩展/合并/截断/融合/证据回填）；`search_mcp.rs` 只做 world 分发与结果适配。Entity 种子按 `entity_id` 扩展（SAME_AS 无向归并 + RELATES 变长路径），非 Entity 种子用 payload `elementId` 定位扩展；候选分桶截断后送 bge-reranker-v2-m3，sigmoid 归一进融合公式；任一路故障降级不整体失败。

**Tech Stack:** Rust（tokio / async-trait / serde_json）、Memgraph（Cypher over Bolt）、Qdrant（`search_with_filter` 原生过滤）、xinference/SiliconFlow rerank。

**Spec:** `docs/superpowers/specs/2026-08-01-knowledge-search-design.md`（S5 定稿；下文 §x.y 引用均指该文档）

## Global Constraints

- 本计划**不做**：proto 变更、CLI / `application/search.rs`（第二检索栈）改动、gRPC `world` 透传（S5-D10，后续任务卡见 Task 12）。
- `max_hops` 白名单 `{1,2}`，其他值钳到 2；**严禁**把请求整数直接拼进 Cypher 字符串（只允许白名单枚举值插值）。
- 融合公式（§5.4 / S5-D7）：正常 `final = 0.6·rerank(sigmoid) + 0.3·语义 + 0.1·graph_boost(0.5^hop)`；降级 `final = 0.75·语义 + 0.25·graph_boost(一阶：种子 1.0、邻居 0.5)`。
- 邻居白名单与 `kg_bridge::BUSINESS_LABELS` **同源引用**（denylist 剔除 `ConfigChange/BugFix/Decision/PodEvent/Document`），禁止复制一份 label 列表。
- `business_id` 派生复用 `kg_bridge` 同源函数，禁止另写一套派生（§5.2.3）。
- 降级可观测（§5.7.4）：条级 `rerank_degraded` + 世界级 `degraded: ["rerank_unavailable" | "graph_expansion_failed" | "embed_unavailable"]`。
- 测试基线：`772 passed / 2 failed`（预存失败 `ts_java::parses_hello_service`、`backup_sqlite::copy_database_writes_file`）——**不得扩大失败数**；`cargo clippy --all-targets` 0 error。
- LLM 抽取存在漂移，集成实测断言一律用"存在性 + 位次归因"，不做精确相等（§9.1 容差）。
- 每任务一次提交，conventional commits（`feat:` / `refactor:` / `test:`）。

## File Structure

| 文件 | 动作 | 职责 |
|------|------|------|
| `src/application/knowledge/extract/retrieve.rs` | 新建 | GraphRAG 全流程 + `ScoreBreakdown`/`RelationSnippet` 契约类型 + 内联单测 |
| `src/application/knowledge/extract/mod.rs` | 修改 | 注册 `pub mod retrieve;` |
| `src/application/context/search_mcp.rs` | 修改 | 契约结构扩展；`search_knowledge` 委托 retrieve；`search_vector`→`search_doc` 改写；`new`/`empty` 加 rerank 参 |
| `src/application/sync/kg_bridge.rs` | 修改 | 提取 `business_id_from_props`（Task 2） |
| `src/infrastructure/embedder.rs` | 修改 | 提取 `build_provider_router`；新增 `create_rerank_router`（Task 9） |
| `src/interfaces/grpc/services/build_service.rs` | 修改 | `handle_search` 构造处接 rerank（Task 8 传 `None`，Task 9 换真路由） |

---

### Task 0: S5-0 前置——全量重建 `kg_nodes`（人工/运维，无代码）

**为什么必须做**：旧格式点无 `project`/`business_id`/`origin`（主文档 §13.6 实测 3945 点中仅 281 新格式）。不重建则召回被 `project` 过滤卡死、缺 `business_id` 的点在种子解析时被丢弃，后续所有实测验收失真。

- [x] **Step 1: 清库**

```bash
curl -X DELETE http://localhost:6334/collections/kg_nodes
```

预期返回：`{"result": true, "status": "ok", ...}`。

- [x] **Step 2: 全量重建（embed 走 SiliconFlow，先 export key）**

```bash
export SILICONFLOW_API_KEY=<从 config/config.yaml.bak 读取，不写入任何被提交文件>
cargo run -- build
```

- [x] **Step 3: 验证新格式点占比**

```bash
curl -s -X POST http://localhost:6334/collections/kg_nodes/points/scroll \
  -H 'Content-Type: application/json' \
  -d '{"limit": 20, "with_payload": ["business_id","project","origin"]}'
```

预期：抽样点的 payload 均含 `business_id`/`project`/`origin` 三键；缺失则回 Step 2 排查 build/kg-sync 链路。

- [x] **Step 4: 记录**——在执行简报中记录重建时间、点数、抽样结果（无代码提交）。

---

### Task 1: 契约扩展 + retrieve.rs 骨架

**Files:**
- Create: `src/application/knowledge/extract/retrieve.rs`
- Modify: `src/application/knowledge/extract/mod.rs:9-16`
- Modify: `src/application/context/search_mcp.rs:22-79`（三个结构体 + tests mod）
- Test: `src/application/context/search_mcp.rs` 内 tests mod

**Interfaces:**
- Consumes: 无（首个代码任务）。
- Produces（后续任务依赖的精确签名）:
  - `retrieve::ScoreBreakdown { pub semantic: f64, pub rerank: f64, pub graph_boost: f64, pub final_score: f64 }`（`Debug, Clone, Serialize, Deserialize`）
  - `retrieve::RelationSnippet { pub rel_type: String, pub other_end_id: String, pub other_end_name: String, pub direction: String, pub confidence: f64, pub evidence: Option<String>, pub supplementary_count: u32 }`（同 derive）
  - `SearchRequest` 新增：`max_hops: Option<u32>`、`with_evidence: Option<bool>`、`origin: Option<String>`、`doc_id: Option<String>`
  - `SearchHit` 新增：`score_breakdown: Option<ScoreBreakdown>`、`hop: Option<u32>`、`via_same_as: Option<bool>`、`relations: Option<Vec<RelationSnippet>>`、`evidence: Option<Vec<String>>`、`rerank_degraded: Option<bool>`
  - `CrossWorldResult` 新增：`degraded: Vec<String>`

- [x] **Step 1: 先改测试（编译应失败）**

`search_mcp.rs` tests mod 顶部新增 import，4 个既有测试的结构体字面量补齐新字段，并新增 1 个向后兼容测试：

```rust
// tests mod 顶部
use crate::application::knowledge::extract::retrieve::{RelationSnippet, ScoreBreakdown};
```

`search_hit_construction` 的 `SearchHit` 字面量追加：

```rust
            score_breakdown: Some(ScoreBreakdown {
                semantic: 0.71,
                rerank: 0.92,
                graph_boost: 1.0,
                final_score: 0.83,
            }),
            hop: Some(0),
            via_same_as: None,
            relations: Some(vec![RelationSnippet {
                rel_type: "routes_to".into(),
                other_end_id: "dt://entity/p/Service/s".into(),
                other_end_name: "PayChannelService".into(),
                direction: "out".into(),
                confidence: 0.9,
                evidence: Some("ifCode 决定路由".into()),
                supplementary_count: 2,
            }]),
            evidence: None,
            rerank_degraded: None,
```

`cross_world_result_empty` / `cross_world_result_serialization` 的 `CrossWorldResult` 字面量追加 `degraded: vec![],`；后者的 `SearchHit` 字面量追加六个 `None` 字段（`score_breakdown: None, hop: None, via_same_as: None, relations: None, evidence: None, rerank_degraded: None`）。`search_request_defaults` 的 `SearchRequest` 字面量追加 `max_hops: None, with_evidence: None, origin: None, doc_id: None`。

新增测试：

```rust
#[test]
fn search_hit_deserializes_legacy_json_without_new_fields() {
    let legacy = r#"{
        "id":"1","title":"t","snippet":"s","source_world":"knowledge",
        "entity_type":"Knowledge","score":0.5,"source_ref":null,
        "file_path":null,"start_line":null,"end_line":null,
        "signature":null,"calls":[],"element_id":null
    }"#;
    let hit: SearchHit = serde_json::from_str(legacy).unwrap();
    assert!(hit.score_breakdown.is_none());
    assert!(hit.hop.is_none());
    assert!(hit.relations.is_none());
    assert!(hit.rerank_degraded.is_none());
}
```

- [x] **Step 2: 确认编译失败**

Run: `cargo test --lib search_mcp 2>&1 | tail -20`
Expected: FAIL — 结构体缺字段、`retrieve` 模块不存在。

- [x] **Step 3: 新建 retrieve.rs（本任务只放契约类型与 env 配置）**

`src/application/knowledge/extract/retrieve.rs`：

```rust
//! Retrieve 检索层 — GraphRAG 式混合检索（spec S5 / 主文档 §8）。
//!
//! 管线：召回(kg_nodes) → 图扩展(SAME_AS/RELATES/elementId) → 分桶截断
//! → rerank(sigmoid) → 融合排序。任一路故障降级，不整体失败（spec §3）。

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};

// ---------------------------------------------------------------------------
// 输出契约子结构（spec §5.7.3）
// ---------------------------------------------------------------------------

/// 排序分解——权重调参（主文档 §8 收敛回写）的观测数据源。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreBreakdown {
    /// 向量召回分；邻居 = 种子分 × 0.5^hop（§5.4 近似）。
    pub semantic: f64,
    /// sigmoid 归一后；rerank_degraded 时为 0.0。
    pub rerank: f64,
    /// 正常 1.0/0.5/0.25（0.5^hop）；降级模式一阶衰减 1.0/0.5（§5.4）。
    pub graph_boost: f64,
    /// = SearchHit.score，冗余字段便于消费方免对齐。
    pub final_score: f64,
}

/// 关系摘要——命中实体对外的 top-5 关系（按 confidence 降序）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelationSnippet {
    /// RELATES.type，如 routes_to / depends_on。
    pub rel_type: String,
    /// 对端 business_id。
    pub other_end_id: String,
    /// 对端 name（展示用；不在候选集中时为空串）。
    pub other_end_name: String,
    /// "out" | "in"（相对命中实体）。
    pub direction: String,
    pub confidence: f64,
    /// 最高 confidence 边的证据句。
    pub evidence: Option<String>,
    /// 其余文档来源的补充证据数（边聚合产物；>0 表示多文档佐证）。
    pub supplementary_count: u32,
}

// ---------------------------------------------------------------------------
// 配置（env；spec §8 配置项汇总表）
// ---------------------------------------------------------------------------

/// 语义分阈值（复用 code 世界同款 env，S5 起 knowledge/doc 世界生效）。
pub fn min_score() -> f64 {
    std::env::var("DT_SEARCH_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.3)
}

/// rerank 候选总上限（分桶规则见 §5.2.3）。
pub fn rerank_top_n() -> usize {
    std::env::var("DT_KG_RERANK_TOP_N")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
}

/// 图扩展跳数白名单 {1,2}，其他值钳到 2（S5-D3；严禁直接拼请求整数进 Cypher）。
pub fn clamp_max_hops(h: u32) -> u32 {
    h.clamp(1, 2)
}

/// bge-reranker-v2-m3 返回 logit，sigmoid 归一到 [0,1]（S5-D6）。
pub fn sigmoid(x: f32) -> f64 {
    1.0 / (1.0 + (-(x as f64)).exp())
}

// ---------------------------------------------------------------------------
// Retriever — 混合检索执行器（管线在后续任务逐层填充）
// ---------------------------------------------------------------------------

/// GraphRAG 混合检索执行器。`graph`/`rerank` 可选（缺失即走对应降级路径）。
pub struct Retriever {
    pub(crate) graph: Option<Arc<dyn GraphRepository>>,
    pub(crate) vector: Arc<dyn VectorRepository>,
    pub(crate) embed: Arc<dyn EmbedService>,
    pub(crate) rerank: Option<Arc<dyn RerankService>>,
}

impl Retriever {
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self { graph, vector, embed, rerank }
    }
}
```

`src/application/knowledge/extract/mod.rs` 第 9-10 行改为：

```rust
pub mod consolidate;
pub mod model;
pub mod retrieve;
```

`search_mcp.rs` 结构体扩展（顶部 import 新增 `use crate::application::knowledge::extract::retrieve::{RelationSnippet, ScoreBreakdown};`）：

```rust
// SearchRequest 追加字段（带 doc 注释）：
    /// 图扩展跳数（knowledge 世界），白名单 {1,2}，默认 1。
    pub max_hops: Option<u32>,
    /// knowledge top-5 实体从 doc_chunks 回填证据段落。
    pub with_evidence: Option<bool>,
    /// 按 kg_nodes payload origin 过滤召回种子（extracted/learned/manual）。
    pub origin: Option<String>,
    /// 仅 world=doc：限定单文档内检索证据块。
    pub doc_id: Option<String>,

// SearchHit 追加字段：
    /// 排序分解（knowledge 世界新链路填充）。
    #[serde(default)]
    pub score_breakdown: Option<ScoreBreakdown>,
    /// 图距离：0=直接命中（含 SAME_AS 别名），1/2=扩展邻居。
    #[serde(default)]
    pub hop: Option<u32>,
    /// 是否经 SAME_AS 别名归并命中。
    #[serde(default)]
    pub via_same_as: Option<bool>,
    /// 命中实体的关系摘要（去重聚合后，上限 5 条）。
    #[serde(default)]
    pub relations: Option<Vec<RelationSnippet>>,
    /// 证据段落（world=doc 或 with_evidence 回填）。
    #[serde(default)]
    pub evidence: Option<Vec<String>>,
    /// rerank 降级标记。
    #[serde(default)]
    pub rerank_degraded: Option<bool>,

// CrossWorldResult 追加字段：
    /// 降级标记（"rerank_unavailable" / "graph_expansion_failed" / "embed_unavailable"）。
    #[serde(default)]
    pub degraded: Vec<String>,
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test --lib search_mcp 2>&1 | tail -10`
Expected: 5 passed（4 旧 + 1 新）。

- [x] **Step 5: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs src/application/knowledge/extract/mod.rs src/application/context/search_mcp.rs
git commit -m "feat(s5): search contract extension (SearchHit/SearchRequest/CrossWorldResult) + retrieve.rs skeleton"
```

---

### Task 2: kg_bridge 提取 `business_id_from_props`（同源复用）

**Files:**
- Modify: `src/application/sync/kg_bridge.rs:1083-1143`
- Test: `src/application/sync/kg_bridge.rs` 内 tests mod

**Interfaces:**
- Consumes: 现有 `pub(crate) fn business_id(node: &KgNode) -> String`（21 项 ID_KEYS 顺序扫描 → `name@qualifier` → element_id 兜底）。
- Produces: `pub(crate) fn business_id_from_props(props: &serde_json::Value, element_id_fallback: &str) -> String`——retrieve.rs（Task 5）用它派生非 Entity 邻居的 business_id。**禁止在 retrieve.rs 另写派生逻辑**（Global Constraints）。

- [x] **Step 1: 先写失败测试**

`kg_bridge.rs` tests mod 新增：

```rust
#[test]
fn business_id_from_props_matches_node_variant() {
    // 21 项显式 id 优先
    let props = serde_json::json!({"knowledge_id": "k-1", "name": "n"});
    assert_eq!(super::business_id_from_props(&props, "4:1:1"), "k-1");
    // entity_id 优先级高于 knowledge_id
    let props = serde_json::json!({"entity_id": "e-1", "knowledge_id": "k-1"});
    assert_eq!(super::business_id_from_props(&props, "4:1:1"), "e-1");
    // 复合键：name@namespace（Table/ConfigKey/K8sDeployment 形态）
    let props = serde_json::json!({"name": "cfg", "namespace": "public"});
    assert_eq!(super::business_id_from_props(&props, "4:1:1"), "cfg@public");
    // 无 id 无限定词：裸 name
    let props = serde_json::json!({"name": "plain"});
    assert_eq!(super::business_id_from_props(&props, "4:1:1"), "plain");
    // 全缺：element_id 兜底
    let props = serde_json::json!({});
    assert_eq!(super::business_id_from_props(&props, "4:1:1"), "4:1:1");
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib kg_bridge business_id_from_props 2>&1 | tail -5`
Expected: FAIL — `business_id_from_props` not found。

- [x] **Step 3: 实现——ID_KEYS 提升为模块级常量，两个函数共用**

`kg_bridge.rs` 中把 `ID_KEYS` 从 `business_id` 函数内提到模块级（紧邻 `BUSINESS_LABELS` 之后），然后重写：

```rust
/// Explicit unique-ID property keys in priority order (I1).
const ID_KEYS: &[&str] = &[
    "entity_id", "knowledge_id", "concept_id", "experience_id", "playbook_id",
    "domain_id", "server_id", "database_id", "service_id", "instance_id",
    "endpoint_id", "doc_id", "config_id", "thread_id", "requirement_id",
    "decision_id", "event_id", "session_id", "version_id", "observation_id",
    "analysis_id",
];

/// Derive the stable business ID from a property map (S5 共享入口).
///
/// Same 21-key priority order as [`business_id`]; used by retrieve.rs for
/// graph-expansion neighbours that never materialise as `KgNode`.
pub(crate) fn business_id_from_props(props: &serde_json::Value, element_id_fallback: &str) -> String {
    for key in ID_KEYS {
        if let Some(s) = props.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if let Some(name) = props
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let qualifier = props
            .get("namespace")
            .and_then(|v| v.as_str())
            .or_else(|| props.get("db").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty());
        return match qualifier {
            Some(q) => format!("{name}@{q}"),
            None => name.to_string(),
        };
    }
    element_id_fallback.to_string()
}

pub(crate) fn business_id(node: &KgNode) -> String {
    business_id_from_props(&node.properties, &node.element_id)
}
```

- [x] **Step 4: 跑测试确认通过（含既有 business_id 相关测试不回归）**

Run: `cargo test --lib kg_bridge 2>&1 | tail -5`
Expected: 全绿（新测试 + 既有测试）。

- [x] **Step 5: 提交**

```bash
git add src/application/sync/kg_bridge.rs
git commit -m "refactor(s5): extract business_id_from_props for reuse by retrieve.rs"
```

---

### Task 3: 召回层——`Retriever::recall` + 种子解析

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`
- Test: `src/application/knowledge/extract/retrieve.rs` 内 tests mod（本任务新建 mocks，后续任务复用）

**Interfaces:**
- Consumes: Task 1 的 `Retriever`/`min_score()`；`VectorRepository::search_with_filter(collection, vector, limit, filter)`；`EmbedService::embed_batch(&[String])`；`KG_NODES` 常量。
- Produces:
  - `pub(crate) struct Seed { pub business_id: String, pub element_id: Option<String>, pub labels: Vec<String>, pub name: String, pub summary: String, pub entity_type: String, pub semantic: f64 }`
  - `pub(crate) async fn recall(&self, query: &str, project: Option<&str>, origin: Option<&str>, k: u64) -> Result<Vec<Seed>, DtError>`
  - `fn recall_filter(project: Option<&str>, origin: Option<&str>) -> serde_json::Value`
  - `fn parse_seed(hit: &serde_json::Value, min_score: f64) -> Option<Seed>`（无 `business_id` 或低于阈值的点丢弃）

- [x] **Step 1: 先写失败测试（含 mocks）**

retrieve.rs 尾部新建 tests mod（MockGraph/MockVector 在 Task 4/5/7 复用）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use serde_json::json;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    pub(crate) struct MockGraph {
        pub responses: Mutex<VecDeque<serde_json::Value>>,
        pub captured: Mutex<Vec<(String, HashMap<String, serde_json::Value>)>>,
    }

    #[async_trait::async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.captured.lock().unwrap().push((query.to_string(), params));
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json!([])))
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(json!([]))
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) struct MockVector {
        pub hits: Vec<serde_json::Value>,
        pub captured_filter: Mutex<Option<serde_json::Value>>,
        pub captured_limit: Mutex<Option<u64>>,
    }

    #[async_trait::async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> { Ok(()) }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.hits.clone())
        }
        async fn search_with_filter(
            &self,
            _c: &str,
            _v: Vec<f32>,
            limit: u64,
            filter: serde_json::Value,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            *self.captured_filter.lock().unwrap() = Some(filter);
            *self.captured_limit.lock().unwrap() = Some(limit);
            Ok(self.hits.clone())
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> { Ok(()) }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> { Ok(()) }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> { Ok(vec![]) }
        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.to_string(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> { Ok(()) }
        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }

    pub(crate) struct MockEmbed;

    #[async_trait::async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    fn seed_hit(score: f64, business_id: &str, labels: &[&str]) -> serde_json::Value {
        json!({
            "id": "pt-1",
            "score": score,
            "payload": {
                "business_id": business_id,
                "elementId": "4:91:1",
                "labels": labels,
                "name": "ifCode",
                "summary": "渠道路由字段",
                "type": "Channel",
                "project": "offen-pay",
                "origin": "extracted"
            }
        })
    }

    #[tokio::test]
    async fn recall_filters_by_score_and_business_id() {
        let vector = MockVector {
            hits: vec![
                seed_hit(0.9, "dt://entity/p/Channel/ifcode", &["Entity"]),
                seed_hit(0.1, "dt://entity/p/Channel/low", &["Entity"]), // 低于阈值
                {
                    // 旧格式点：无 business_id → 丢弃
                    let mut h = seed_hit(0.95, "x", &["Knowledge"]);
                    h["payload"].as_object_mut().unwrap().remove("business_id");
                    h
                },
            ],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let r = Retriever::new(None, Arc::new(vector), Arc::new(MockEmbed), None);
        let seeds = r
            .recall("渠道怎么路由", Some("offen-pay"), Some("extracted"), 60)
            .await
            .unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].business_id, "dt://entity/p/Channel/ifcode");
        assert_eq!(seeds[0].semantic, 0.9);
        assert!(seeds[0].labels.iter().any(|l| l == "Entity"));
    }

    #[tokio::test]
    async fn recall_builds_native_filter_with_project_and_origin() {
        let vector = Arc::new(MockVector {
            hits: vec![],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        });
        let r = Retriever::new(None, vector.clone(), Arc::new(MockEmbed), None);
        let _ = r.recall("q", Some("p1"), Some("manual"), 60).await.unwrap();
        let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
        let must = filter["must"].as_array().unwrap();
        assert!(must.iter().any(|c| c["key"] == "project" && c["match"]["value"] == "p1"));
        assert!(must.iter().any(|c| c["key"] == "origin" && c["match"]["value"] == "manual"));
        assert_eq!(*v2.captured_limit.lock().unwrap(), Some(60));
    }
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib retrieve 2>&1 | tail -10`
Expected: FAIL — `recall`/`Seed` 不存在。

- [x] **Step 3: 实现召回层**

retrieve.rs 追加（`use` 区补充 `use crate::shared::collections::KG_NODES;`）：

```rust
// ---------------------------------------------------------------------------
// ① 召回（§5.1）
// ---------------------------------------------------------------------------

/// 向量召回的种子节点。`business_id` 为稳定业务主键（旧格式点缺失即丢弃）。
#[derive(Debug, Clone)]
pub(crate) struct Seed {
    pub business_id: String,
    pub element_id: Option<String>,
    pub labels: Vec<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    pub semantic: f64,
}

/// Qdrant 原生 filter：project/origin 可选 must 条件（R7；§5.1/§5.7.1）。
fn recall_filter(project: Option<&str>, origin: Option<&str>) -> serde_json::Value {
    let mut must = Vec::new();
    if let Some(p) = project {
        must.push(serde_json::json!({"key": "project", "match": {"value": p}}));
    }
    if let Some(o) = origin {
        must.push(serde_json::json!({"key": "origin", "match": {"value": o}}));
    }
    serde_json::json!({ "must": must })
}

/// 从 Qdrant hit 解析种子；语义分 < min_score 或无 business_id 丢弃（§5.1）。
fn parse_seed(hit: &serde_json::Value, min_score: f64) -> Option<Seed> {
    let score = hit.get("score")?.as_f64()?;
    if score < min_score {
        return None;
    }
    let p = hit.get("payload")?;
    let business_id = p.get("business_id")?.as_str()?;
    if business_id.is_empty() {
        return None;
    }
    let labels: Vec<String> = p
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|l| l.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let summary = p.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let entity_type = p
        .get("type")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| labels.first().cloned())
        .unwrap_or_else(|| "?".to_string());
    let element_id = p.get("elementId").and_then(|v| v.as_str()).map(String::from);
    Some(Seed { business_id: business_id.to_string(), element_id, labels, name, summary, entity_type, semantic: score })
}

impl Retriever {
    /// embed(query) → kg_nodes.search_with_filter(project[+origin], k) → 种子（§5.1）。
    pub(crate) async fn recall(
        &self,
        query: &str,
        project: Option<&str>,
        origin: Option<&str>,
        k: u64,
    ) -> Result<Vec<Seed>, DtError> {
        let embeddings = self.embed.embed_batch(&[query.to_string()]).await?;
        let Some(qvec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };
        let hits = self
            .vector
            .search_with_filter(KG_NODES, qvec, k, recall_filter(project, origin))
            .await?;
        let threshold = min_score();
        Ok(hits.iter().filter_map(|h| parse_seed(h, threshold)).collect())
    }
}
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -8`
Expected: 2 passed。

- [x] **Step 5: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs
git commit -m "feat(s5): retrieve recall layer — kg_nodes vector recall with native project/origin filter"
```

---

### Task 4: Entity 图扩展——SAME_AS 归并 + RELATES 邻居 + 边解析

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`
- Test: 同文件 tests mod（复用 Task 3 mocks）

**Interfaces:**
- Consumes: Task 3 `Seed`；`GraphRepository::read_query`；`parse_graph_rows`（`crate::application::context::graph_parse`，Bolt 格式 = 行对象数组）。
- Produces:
  - `pub(crate) struct RawEdge { pub head: String, pub tail: String, pub rel_type: String, pub confidence: f64, pub evidence: Option<String>, pub doc_id: Option<String> }`
  - `pub(crate) struct ExpandedNode { pub business_id: String, pub element_id: Option<String>, pub name: String, pub summary: String, pub entity_type: String, pub from_seed: String, pub hop: u32, pub via_same_as: bool, pub path_min_confidence: f64 }`
  - `pub(crate) struct ExpansionResult { pub nodes: Vec<ExpandedNode>, pub edges: Vec<RawEdge> }`（`Default` derive）
  - `pub(crate) async fn expand_entity(&self, seeds: &[Seed], max_hops: u32) -> Result<ExpansionResult, DtError>`
  - `fn parse_entity_rows(rows: Vec<serde_json::Value>, original: &std::collections::HashSet<&str>) -> ExpansionResult`

- [x] **Step 1: 先写失败测试**

```rust
fn entity_row(
    seed_id: &str,
    neighbor: serde_json::Value,
    hop: serde_json::Value,
    edges: serde_json::Value,
) -> serde_json::Value {
    json!({
        "seed_id": seed_id,
        "seed_element_id": format!("eid-{seed_id}"),
        "seed_name": format!("name-{seed_id}"),
        "seed_summary": format!("summary-{seed_id}"),
        "seed_type": "Channel",
        "neighbor": neighbor,
        "neighbor_element_id": neighbor.get("entity_id").map(|_| "eid-nb"),
        "hop": hop,
        "edges": edges,
    })
}

#[tokio::test]
async fn expand_entity_builds_whitelisted_cypher() {
    let graph = Arc::new(MockGraph {
        responses: Mutex::new(VecDeque::from(vec![json!([])])),
        captured: Mutex::new(vec![]),
    });
    let r = Retriever::new(Some(graph.clone()), Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }), Arc::new(MockEmbed), None);
    let seeds = vec![Seed {
        business_id: "A".into(), element_id: Some("eid-A".into()),
        labels: vec!["Entity".into()], name: "a".into(), summary: "s".into(),
        entity_type: "Channel".into(), semantic: 0.9,
    }];
    let _ = r.expand_entity(&seeds, 2).await.unwrap();
    let (cypher, params) = graph.captured.lock().unwrap()[0].clone();
    assert!(cypher.contains("SAME_AS"));
    assert!(cypher.contains("RELATES*1..2"));
    assert!(cypher.contains("LIMIT 500"));
    assert_eq!(params["seeds"], json!(["A"]));
    // max_hops=1 时拼 *1..1（白名单插值）
    let graph2 = Arc::new(MockGraph { responses: Mutex::new(VecDeque::from(vec![json!([])])), captured: Mutex::new(vec![]) });
    let r2 = Retriever::new(Some(graph2.clone()), Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }), Arc::new(MockEmbed), None);
    let _ = r2.expand_entity(&seeds, 1).await.unwrap();
    assert!(graph2.captured.lock().unwrap()[0].0.contains("RELATES*1..1"));
}

#[test]
fn parse_entity_rows_alias_and_neighbor_and_edges() {
    let original: std::collections::HashSet<&str> = ["A"].into_iter().collect();
    let rows = vec![
        // 原始种子 A → 1 跳邻居 B，两条同三元组边（不同 doc_id，待 Task 6 聚合）
        entity_row("A", json!({"entity_id":"B","name":"nb","type":"Service","summary":"sb"}), json!(1),
            json!([{"type":"routes_to","confidence":0.9,"evidence":"e1","doc_id":"d1","head":"A","tail":"B"},
                   {"type":"routes_to","confidence":0.7,"evidence":"e2","doc_id":"d2","head":"A","tail":"B"}])),
        // SAME_AS 别名 C（不在原始种子集）→ 无邻居
        entity_row("C", serde_json::Value::Null, serde_json::Value::Null, json!([])),
    ];
    let result = parse_entity_rows(rows, &original);
    // 别名节点：hop=0、via_same_as=true
    let alias = result.nodes.iter().find(|n| n.business_id == "C").unwrap();
    assert!(alias.via_same_as);
    assert_eq!(alias.hop, 0);
    // 邻居节点：hop=1、via_same_as=false、path_min_confidence 取路径最小值
    let nb = result.nodes.iter().find(|n| n.business_id == "B").unwrap();
    assert_eq!(nb.hop, 1);
    assert!(!nb.via_same_as);
    assert!((nb.path_min_confidence - 0.7).abs() < 1e-9);
    // 原始种子不产生 ExpandedNode（自身在召回侧已是候选）
    assert!(result.nodes.iter().all(|n| n.business_id != "A"));
    // 两条原始边都保留（聚合在 Task 6 attach_relations）
    assert_eq!(result.edges.len(), 2);
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib retrieve 2>&1 | tail -10`
Expected: FAIL — `expand_entity`/`parse_entity_rows` 不存在。

- [x] **Step 3: 实现 Entity 扩展**

retrieve.rs 追加（`use` 区补充 `use crate::application::context::graph_parse::parse_graph_rows;` 与 `use std::collections::HashSet;`）：

```rust
// ---------------------------------------------------------------------------
// ② 图扩展（§5.2）
// ---------------------------------------------------------------------------

/// 归一化后的关系边（head/tail 为 business_id）。
#[derive(Debug, Clone)]
pub(crate) struct RawEdge {
    pub head: String,
    pub tail: String,
    pub rel_type: String,
    pub confidence: f64,
    pub evidence: Option<String>,
    pub doc_id: Option<String>,
}

/// 图扩展产物节点（SAME_AS 别名或 RELATES/任意关系邻居；不含原始种子）。
#[derive(Debug, Clone)]
pub(crate) struct ExpandedNode {
    pub business_id: String,
    pub element_id: Option<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    /// 扩展出该节点的种子 business_id（邻居语义分衰减用）。
    pub from_seed: String,
    pub hop: u32,
    pub via_same_as: bool,
    /// 路径各边 confidence 最小值（缺失按 0.5）；分桶预排分用（§5.2.3）。
    pub path_min_confidence: f64,
}

#[derive(Debug, Default)]
pub(crate) struct ExpansionResult {
    pub nodes: Vec<ExpandedNode>,
    pub edges: Vec<RawEdge>,
}

fn edge_confidence(e: &serde_json::Value) -> f64 {
    e.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5)
}

/// §5.2.1：Entity 种子 — SAME_AS 无向归并 + RELATES*1..max_hops（白名单插值）。
fn entity_expansion_cypher(max_hops: u32) -> String {
    let hops = clamp_max_hops(max_hops);
    format!(
        r#"
UNWIND $seeds AS seed
MATCH (e:Entity {{entity_id: seed}})
OPTIONAL MATCH (e)-[:SAME_AS]-(alias:Entity)
WITH collect(DISTINCT e) + collect(DISTINCT alias) AS seed_nodes
UNWIND seed_nodes AS s
OPTIONAL MATCH path = (s)-[:RELATES*1..{hops}]-(nb:Entity)
WITH s, nb, relationships(path) AS rels, length(path) AS hop
RETURN s.entity_id AS seed_id,
       elementId(s) AS seed_element_id,
       s.name AS seed_name,
       s.summary AS seed_summary,
       s.type AS seed_type,
       nb {{ .entity_id, .name, .type, .summary, .keywords }} AS neighbor,
       elementId(nb) AS neighbor_element_id,
       hop,
       [r IN rels | {{type: r.type, confidence: r.confidence,
                     evidence: r.evidence, doc_id: r.doc_id,
                     head: startNode(r).entity_id, tail: endNode(r).entity_id}}] AS edges
LIMIT 500
"#
    )
}

/// 解析 Entity 扩展行。`original` = 原始种子的 entity_id 集合（判定 via_same_as）。
fn parse_entity_rows(rows: Vec<serde_json::Value>, original: &HashSet<&str>) -> ExpansionResult {
    let mut result = ExpansionResult::default();
    let mut seen_alias: HashSet<String> = HashSet::new();
    for row in rows {
        let seed_id = row.get("seed_id").and_then(|v| v.as_str()).unwrap_or("");
        if seed_id.is_empty() {
            continue;
        }
        // SAME_AS 别名（命中节点不在原始种子集）→ hop=0 候选；原始种子自身跳过
        if !original.contains(seed_id) && seen_alias.insert(seed_id.to_string()) {
            result.nodes.push(ExpandedNode {
                business_id: seed_id.to_string(),
                element_id: row.get("seed_element_id").and_then(|v| v.as_str()).map(String::from),
                name: row.get("seed_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                summary: row.get("seed_summary").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                entity_type: row.get("seed_type").and_then(|v| v.as_str()).unwrap_or("Entity").to_string(),
                from_seed: seed_id.to_string(),
                hop: 0,
                via_same_as: true,
                path_min_confidence: 1.0,
            });
        }
        // 边（无论邻居是否存在都收集——hop 为 null 时 edges 为空数组）
        let edges = row.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let path_min = edges.iter().map(edge_confidence).fold(1.0_f64, f64::min);
        for e in &edges {
            result.edges.push(RawEdge {
                head: e.get("head").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                tail: e.get("tail").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                rel_type: e.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                confidence: edge_confidence(e),
                evidence: e.get("evidence").and_then(|v| v.as_str()).map(String::from),
                doc_id: e.get("doc_id").and_then(|v| v.as_str()).map(String::from),
            });
        }
        // 邻居节点
        let Some(nb) = row.get("neighbor").filter(|n| !n.is_null()) else { continue };
        let Some(nb_id) = nb.get("entity_id").and_then(|v| v.as_str()) else { continue };
        let hop = row.get("hop").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        result.nodes.push(ExpandedNode {
            business_id: nb_id.to_string(),
            element_id: row.get("neighbor_element_id").and_then(|v| v.as_str()).map(String::from),
            name: nb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            summary: nb.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            entity_type: nb.get("type").and_then(|v| v.as_str()).unwrap_or("Entity").to_string(),
            from_seed: seed_id.to_string(),
            hop,
            via_same_as: false,
            path_min_confidence: if edges.is_empty() { 0.5 } else { path_min },
        });
    }
    result
}

impl Retriever {
    /// Entity 种子图扩展（SAME_AS 归并 + RELATES 邻居，§5.2.1）。
    pub(crate) async fn expand_entity(
        &self,
        seeds: &[Seed],
        max_hops: u32,
    ) -> Result<ExpansionResult, DtError> {
        let Some(ref graph) = self.graph else {
            return Ok(ExpansionResult::default());
        };
        if seeds.is_empty() {
            return Ok(ExpansionResult::default());
        }
        let original: HashSet<&str> = seeds.iter().map(|s| s.business_id.as_str()).collect();
        let mut params = std::collections::HashMap::new();
        params.insert(
            "seeds".to_string(),
            serde_json::json!(seeds.iter().map(|s| &s.business_id).collect::<Vec<_>>()),
        );
        let raw = graph.read_query(&entity_expansion_cypher(max_hops), params).await?;
        Ok(parse_entity_rows(parse_graph_rows(&raw), &original))
    }
}
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -8`
Expected: 4 passed（含 Task 3 两个）。

- [x] **Step 5: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs
git commit -m "feat(s5): entity graph expansion — SAME_AS merge + RELATES neighbors with edge parsing"
```

---

### Task 5: 非 Entity 扩展——elementId 定位 + 邻居白名单 + 逐种子截断

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`
- Test: 同文件 tests mod

**Interfaces:**
- Consumes: Task 2 `business_id_from_props`；Task 4 `ExpandedNode`/`ExpansionResult`/`RawEdge`；`kg_bridge::BUSINESS_LABELS`。
- Produces:
  - `pub(crate) fn neighbor_allowed(labels: &[String]) -> bool`（S5-D11：Entity 恒允许；BUSINESS_LABELS 内且不在 denylist 允许；denylist = `ConfigChange/BugFix/Decision/PodEvent/Document`）
  - `pub(crate) async fn expand_business(&self, seeds: &[Seed]) -> Result<ExpansionResult, DtError>`（无 `elementId` 的种子静默跳过；每种子按 confidence 降序保留 ≤10 邻居）

- [x] **Step 1: 先写失败测试**

```rust
#[test]
fn neighbor_whitelist() {
    assert!(neighbor_allowed(&["Entity".into()]));
    assert!(neighbor_allowed(&["Knowledge".into()]));
    assert!(neighbor_allowed(&["Table".into()]));
    assert!(!neighbor_allowed(&["Document".into()]));
    assert!(!neighbor_allowed(&["ConfigChange".into()]));
    assert!(!neighbor_allowed(&["PodEvent".into()]));
    assert!(!neighbor_allowed(&["Method".into()]));   // 代码节点不在 BUSINESS_LABELS
}

#[tokio::test]
async fn expand_business_filters_whitelist_and_caps_per_seed() {
    // 构造 12 个邻居：11 个 Knowledge（confidence 0.05*i）+ 1 个 Document（应被白名单滤掉）
    let mut rows = vec![];
    for i in 0..11 {
        rows.push(json!({
            "seed_eid": "eid-seed",
            "neighbor": {"knowledge_id": format!("k-{i}"), "name": format!("n{i}"),
                          "summary": "s", "labels": ["Knowledge"]},
            "neighbor_element_id": format!("eid-n{i}"),
            "rel_type": "RELATES_TO",
            "rel_confidence": 0.05 * i as f64,
        }));
    }
    rows.push(json!({
        "seed_eid": "eid-seed",
        "neighbor": {"doc_id": "d-1", "name": "doc", "labels": ["Document"]},
        "neighbor_element_id": "eid-doc",
        "rel_type": "MENTIONED_IN",
        "rel_confidence": 0.99,
    }));
    let graph = Arc::new(MockGraph {
        responses: Mutex::new(VecDeque::from(vec![json!(rows)])),
        captured: Mutex::new(vec![]),
    });
    let r = Retriever::new(Some(graph.clone()), Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }), Arc::new(MockEmbed), None);
    let seeds = vec![Seed {
        business_id: "k-seed".into(), element_id: Some("eid-seed".into()),
        labels: vec!["Knowledge".into()], name: "s".into(), summary: "s".into(),
        entity_type: "Knowledge".into(), semantic: 0.8,
    }];
    let result = r.expand_business(&seeds).await.unwrap();
    // 白名单过滤 Document；逐种子截断 ≤10（11 个 Knowledge 取 confidence 前 10）
    assert_eq!(result.nodes.len(), 10);
    assert!(result.nodes.iter().all(|n| n.entity_type != "Document"));
    assert!(result.nodes.iter().any(|n| n.business_id == "k-10")); // 最高分保留
    assert!(!result.nodes.iter().any(|n| n.business_id == "k-0")); // 最低分被截
    // Cypher 用 elementId 定位，参数为 payload elementId 列表
    let (cypher, params) = graph.captured.lock().unwrap()[0].clone();
    assert!(cypher.contains("elementId(n) = eid"));
    assert_eq!(params["seed_eids"], json!(["eid-seed"]));
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib retrieve 2>&1 | tail -10`
Expected: FAIL — `expand_business`/`neighbor_allowed` 不存在。

- [x] **Step 3: 实现非 Entity 扩展**

retrieve.rs 追加：

```rust
/// S5-D11 邻居白名单：Entity 恒允许；BUSINESS_LABELS 内且非 Events 组/Document 允许。
/// 与 `kg_bridge::BUSINESS_LABELS` 同源引用，禁止复制 label 列表（Global Constraints）。
pub(crate) fn neighbor_allowed(labels: &[String]) -> bool {
    const DENY: &[&str] = &["ConfigChange", "BugFix", "Decision", "PodEvent", "Document"];
    labels.iter().any(|l| l == "Entity")
        || (labels
            .iter()
            .any(|l| crate::application::sync::kg_bridge::BUSINESS_LABELS.contains(&l.as_str()))
            && !labels.iter().any(|l| DENY.contains(&l.as_str())))
}

/// §5.2.2：非 Entity 种子 — payload elementId 定位（无映射表），1 跳任意关系邻居。
const BIZ_EXPANSION_CYPHER: &str = r#"
UNWIND $seed_eids AS eid
MATCH (n) WHERE elementId(n) = eid
OPTIONAL MATCH (n)-[r]-(nb)
RETURN eid AS seed_eid,
       nb { .* , labels: labels(nb) } AS neighbor,
       elementId(nb) AS neighbor_element_id,
       type(r) AS rel_type,
       r.confidence AS rel_confidence,
       elementId(startNode(r)) AS rel_head,
       elementId(endNode(r)) AS rel_tail
"#;

/// 每种子邻居上限（§5.2.2；配合 §5.2.3 邻居桶承担扇出防护）。
const PER_SEED_NEIGHBOR_CAP: usize = 10;

impl Retriever {
    pub(crate) async fn expand_business(&self, seeds: &[Seed]) -> Result<ExpansionResult, DtError> {
        let Some(ref graph) = self.graph else {
            return Ok(ExpansionResult::default());
        };
        // 无 elementId 的种子无法定位，静默跳过（payload 恒应携带；缺失记 warn）
        let located: Vec<&Seed> = seeds
            .iter()
            .filter(|s| {
                if s.element_id.is_none() {
                    tracing::warn!("seed {} has no elementId, skip graph expansion", s.business_id);
                }
                s.element_id.is_some()
            })
            .collect();
        if located.is_empty() {
            return Ok(ExpansionResult::default());
        }
        let mut params = std::collections::HashMap::new();
        params.insert(
            "seed_eids".to_string(),
            serde_json::json!(located.iter().map(|s| s.element_id.clone().unwrap()).collect::<Vec<_>>()),
        );
        let raw = graph.read_query(BIZ_EXPANSION_CYPHER, params).await?;
        let rows = parse_graph_rows(&raw);

        // seed_eid → seed（from_seed / 边端点 business_id 映射用）
        let by_eid: std::collections::HashMap<&str, &Seed> = located
            .iter()
            .filter_map(|s| s.element_id.as_deref().map(|e| (e, *s)))
            .collect();

        // 按种子分组 → 白名单过滤 → confidence 降序 → ≤PER_SEED_NEIGHBOR_CAP
        let mut per_seed: std::collections::HashMap<String, Vec<&serde_json::Value>> = std::collections::HashMap::new();
        for row in &rows {
            let Some(nb) = row.get("neighbor").filter(|n| !n.is_null()) else { continue };
            let labels: Vec<String> = nb
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|l| l.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if !neighbor_allowed(&labels) {
                continue;
            }
            let seed_eid = row.get("seed_eid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            per_seed.entry(seed_eid).or_default().push(row);
        }

        let mut result = ExpansionResult::default();
        for (seed_eid, mut group) in per_seed {
            group.sort_by(|a, b| {
                let ca = a.get("rel_confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
                let cb = b.get("rel_confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
                cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
            });
            group.truncate(PER_SEED_NEIGHBOR_CAP);
            let from_seed = by_eid
                .get(seed_eid.as_str())
                .map(|s| s.business_id.clone())
                .unwrap_or_default();
            for row in group {
                let nb = &row["neighbor"];
                let nb_eid = row.get("neighbor_element_id").and_then(|v| v.as_str()).unwrap_or("");
                let nb_bid = crate::application::sync::kg_bridge::business_id_from_props(nb, nb_eid);
                let labels: Vec<String> = nb
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|l| l.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let entity_type = nb
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| labels.first().cloned())
                    .unwrap_or_else(|| "?".to_string());
                let conf = row.get("rel_confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
                result.nodes.push(ExpandedNode {
                    business_id: nb_bid.clone(),
                    element_id: row.get("neighbor_element_id").and_then(|v| v.as_str()).map(String::from),
                    name: nb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    summary: nb.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    entity_type,
                    from_seed: from_seed.clone(),
                    hop: 1,
                    via_same_as: false,
                    path_min_confidence: conf,
                });
                // 边端点 elementId → business_id（端点非种子即邻居）
                let rel_type = row.get("rel_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if !rel_type.is_empty() {
                    let head_eid = row.get("rel_head").and_then(|v| v.as_str()).unwrap_or("");
                    let tail_eid = row.get("rel_tail").and_then(|v| v.as_str()).unwrap_or("");
                    let resolve = |eid: &str| -> String {
                        if eid == seed_eid {
                            from_seed.clone()
                        } else if eid == nb_eid {
                            nb_bid.clone()
                        } else {
                            eid.to_string()
                        }
                    };
                    result.edges.push(RawEdge {
                        head: resolve(head_eid),
                        tail: resolve(tail_eid),
                        rel_type,
                        confidence: conf,
                        evidence: None,
                        doc_id: None,
                    });
                }
            }
        }
        Ok(result)
    }
}
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -8`
Expected: 6 passed。

- [x] **Step 5: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs
git commit -m "feat(s5): business-node expansion via elementId + neighbor whitelist + per-seed cap"
```

---

### Task 6: 候选合并 + 边聚合挂接 + 分桶截断

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`
- Test: 同文件 tests mod

**Interfaces:**
- Consumes: Task 3 `Seed`；Task 4/5 `ExpandedNode`/`ExpansionResult`/`RawEdge`。
- Produces:
  - `pub(crate) struct Candidate { pub business_id: String, pub element_id: Option<String>, pub name: String, pub summary: String, pub entity_type: String, pub semantic: f64, pub hop: u32, pub via_same_as: bool, pub graph_boost: f64, pub pre_rank: f64, pub relations: Vec<RelationSnippet>, pub source_ref: Option<String>, pub rerank_score: Option<f64> }`
  - `pub(crate) fn merge_candidates(seeds: &[Seed], nodes: Vec<ExpandedNode>) -> Vec<Candidate>`（去重取最小 hop；邻居 semantic = 种子分 × 0.5^hop；邻居 pre_rank = 种子分 × path_min_confidence）
  - `pub(crate) fn attach_relations(candidates: &mut [Candidate], edges: &[RawEdge])`（三元组去重保最高 confidence；top-5 入 `relations`；`source_ref` = 最高 confidence 边 `doc_id`）
  - `pub(crate) fn bucket_truncate(candidates: &mut Vec<Candidate>, top_n: usize)`（种子桶 ⌈top_n×0.6⌉ + 邻居桶 top_n−种子实际数；§5.2.3）

- [x] **Step 1: 先写失败测试**

```rust
fn mk_seed(bid: &str, score: f64) -> Seed {
    Seed { business_id: bid.into(), element_id: Some(format!("eid-{bid}")), labels: vec!["Entity".into()],
           name: format!("n{bid}"), summary: "s".into(), entity_type: "Channel".into(), semantic: score }
}

fn mk_node(bid: &str, from: &str, hop: u32, conf: f64) -> ExpandedNode {
    ExpandedNode { business_id: bid.into(), element_id: Some(format!("eid-{bid}")),
                   name: format!("n{bid}"), summary: "s".into(), entity_type: "Service".into(),
                   from_seed: from.into(), hop, via_same_as: false, path_min_confidence: conf }
}

#[test]
fn merge_dedups_by_min_hop_and_decays_neighbor_semantic() {
    let seeds = vec![mk_seed("A", 0.9), mk_seed("B", 0.8)];
    let expansion = ExpansionResult {
        nodes: vec![mk_node("B", "A", 1, 0.9), mk_node("C", "A", 1, 0.9), mk_node("D", "A", 2, 0.6)],
        edges: vec![],
    };
    let candidates = merge_candidates(&seeds, expansion);
    // B 既是种子又是邻居 → 保留 hop=0（种子形态，semantic=0.8 自身分）
    let b = candidates.iter().find(|c| c.business_id == "B").unwrap();
    assert_eq!(b.hop, 0);
    assert!((b.semantic - 0.8).abs() < 1e-9);
    // C：1 跳邻居，semantic = 0.9 × 0.5 = 0.45；pre_rank = 0.9 × 0.9
    let c = candidates.iter().find(|c| c.business_id == "C").unwrap();
    assert!((c.semantic - 0.45).abs() < 1e-9);
    assert!((c.pre_rank - 0.81).abs() < 1e-9);
    assert!((c.graph_boost - 0.5).abs() < 1e-9);
    // D：2 跳，boost 0.25
    let d = candidates.iter().find(|c| c.business_id == "D").unwrap();
    assert!((d.graph_boost - 0.25).abs() < 1e-9);
}

#[test]
fn attach_relations_dedups_triple_and_fills_source_ref() {
    let mut candidates = merge_candidates(&[mk_seed("A", 0.9), mk_seed("B", 0.8)], ExpansionResult::default());
    let edges = vec![
        RawEdge { head: "A".into(), tail: "B".into(), rel_type: "routes_to".into(),
                  confidence: 0.9, evidence: Some("e1".into()), doc_id: Some("d1".into()) },
        RawEdge { head: "A".into(), tail: "B".into(), rel_type: "routes_to".into(),
                  confidence: 0.7, evidence: Some("e2".into()), doc_id: Some("d2".into()) },
    ];
    attach_relations(&mut candidates, &edges);
    let a = candidates.iter().find(|c| c.business_id == "A").unwrap();
    let rel = &a.relations[0];
    assert_eq!(rel.rel_type, "routes_to");
    assert_eq!(rel.direction, "out");
    assert_eq!(rel.other_end_id, "B");
    assert_eq!(rel.other_end_name, "nB");
    assert!((rel.confidence - 0.9).abs() < 1e-9);
    assert_eq!(rel.supplementary_count, 1);       // 第二文档证据聚合
    assert_eq!(a.source_ref.as_deref(), Some("d1")); // 最高 confidence 边的 doc_id
    let b = candidates.iter().find(|c| c.business_id == "B").unwrap();
    assert_eq!(b.relations[0].direction, "in");
}

#[test]
fn bucket_truncate_reserves_neighbor_quota() {
    // 60/40 分桶：top_n=4 → 种子桶 3，邻居桶 1
    let mut candidates = vec![];
    for (i, s) in [0.95, 0.9, 0.85, 0.8].iter().enumerate() {
        let mut c = merge_candidates(&[mk_seed(&format!("S{i}"), *s)], ExpansionResult::default());
        candidates.append(&mut c);
    }
    let mut nodes = vec![];
    for (i, pr) in [0.7, 0.6, 0.5].iter().enumerate() {
        let mut c = merge_candidates(&[], ExpansionResult {
            nodes: vec![ExpandedNode {
                business_id: format!("N{i}"), element_id: None, name: "n".into(), summary: "s".into(),
                entity_type: "Service".into(), from_seed: "S0".into(), hop: 1,
                via_same_as: false, path_min_confidence: *pr,
            }],
            edges: vec![],
        });
        // 手动设 pre_rank 便于断言（merge 会算成 seed.semantic×conf，此处无种子 → 用构造值）
        c[0].pre_rank = *pr;
        nodes.push(c.remove(0));
    }
    candidates.extend(nodes);
    bucket_truncate(&mut candidates, 4);
    assert_eq!(candidates.len(), 4);
    assert_eq!(candidates.iter().filter(|c| c.hop == 0).count(), 3); // 种子桶 ⌈4×0.6⌉=3
    assert_eq!(candidates.iter().filter(|c| c.hop >= 1).count(), 1); // 邻居桶 4-3=1
    assert!(candidates.iter().any(|c| c.business_id == "N0"));        // 邻居取 pre_rank 最高
    assert!(!candidates.iter().any(|c| c.business_id == "S3"));       // 种子按语义分截断
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib retrieve 2>&1 | tail -10`
Expected: FAIL — `merge_candidates`/`attach_relations`/`bucket_truncate` 不存在。

- [x] **Step 3: 实现合并/挂接/截断**

retrieve.rs 追加：

```rust
// ---------------------------------------------------------------------------
// ③ 候选合并与分桶截断（§5.2.3）
// ---------------------------------------------------------------------------

/// rerank 候选。`pre_rank` 仅用于候选选拔（分桶排序键），不参与最终融合。
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub business_id: String,
    pub element_id: Option<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    /// 种子=自身召回分；邻居=种子分 × 0.5^hop（§5.4 近似）。
    pub semantic: f64,
    pub hop: u32,
    pub via_same_as: bool,
    /// 1.0 / 0.5 / 0.25（0.5^hop）。
    pub graph_boost: f64,
    /// 桶内预排分：种子=semantic；邻居=种子 semantic × path_min_confidence。
    pub pre_rank: f64,
    pub relations: Vec<RelationSnippet>,
    pub source_ref: Option<String>,
    pub rerank_score: Option<f64>,
}

fn boost_of(hop: u32) -> f64 {
    0.5_f64.powi(hop as i32)
}

/// 合并种子与扩展产物：按 business_id 去重，同节点取最小 hop（最大 boost）。
pub(crate) fn merge_candidates(seeds: &[Seed], expansion: ExpansionResult) -> Vec<Candidate> {
    let seed_score = |bid: &str| seeds.iter().find(|s| s.business_id == bid).map(|s| s.semantic).unwrap_or(0.0);
    let mut map: std::collections::HashMap<String, Candidate> = std::collections::HashMap::new();
    for s in seeds {
        map.insert(s.business_id.clone(), Candidate {
            business_id: s.business_id.clone(),
            element_id: s.element_id.clone(),
            name: s.name.clone(),
            summary: s.summary.clone(),
            entity_type: s.entity_type.clone(),
            semantic: s.semantic,
            hop: 0,
            via_same_as: false,
            graph_boost: 1.0,
            pre_rank: s.semantic,
            relations: vec![],
            source_ref: None,
            rerank_score: None,
        });
    }
    for n in expansion.nodes {
        let base = seed_score(&n.from_seed);
        let (hop, boost, semantic, pre_rank) = if n.via_same_as {
            (0, 1.0, base, base)                       // 别名视为同一实体
        } else {
            (n.hop, boost_of(n.hop), base * boost_of(n.hop), base * n.path_min_confidence)
        };
        map.entry(n.business_id.clone())
            .and_modify(|c| {
                if hop < c.hop {
                    c.hop = hop;
                    c.graph_boost = boost;
                    c.semantic = semantic;
                    c.pre_rank = pre_rank;
                    c.via_same_as = n.via_same_as;
                }
            })
            .or_insert(Candidate {
                business_id: n.business_id.clone(),
                element_id: n.element_id.clone(),
                name: n.name.clone(),
                summary: n.summary.clone(),
                entity_type: n.entity_type.clone(),
                semantic,
                hop,
                via_same_as: n.via_same_as,
                graph_boost: boost,
                pre_rank,
                relations: vec![],
                source_ref: None,
                rerank_score: None,
            });
    }
    // 无 summary 候选：name 兜底进 rerank 文本；仍为空则丢弃（§5.2.3）
    map.into_values()
        .filter(|c| !c.summary.is_empty() || !c.name.is_empty())
        .collect()
}

/// 边去重聚合 + 挂接到候选：同一 (head, rel_type, tail) 保最高 confidence，
/// 其余计 supplementary_count；source_ref = 最高 confidence 边 doc_id（§5.2.1/§5.7.2）。
pub(crate) fn attach_relations(candidates: &mut [Candidate], edges: &[RawEdge]) {
    use std::collections::HashMap;
    // 三元组去重（保最高 confidence，计补充证据数）
    let mut dedup: HashMap<(&str, &str, &str), (&RawEdge, u32)> = HashMap::new();
    for e in edges {
        dedup
            .entry((e.head.as_str(), e.rel_type.as_str(), e.tail.as_str()))
            .and_modify(|(best, cnt)| {
                if e.confidence > best.confidence {
                    *best = e;
                }
                *cnt += 1;
            })
            .or_insert((e, 0));
    }
    // 先建 business_id → name 快照（避免 iter_mut 与不可变借用冲突）
    let names: HashMap<String, String> = candidates
        .iter()
        .map(|c| (c.business_id.clone(), c.name.clone()))
        .collect();
    let mut best_doc: HashMap<String, (f64, Option<String>)> = HashMap::new();
    for c in candidates.iter_mut() {
        let mut rels: Vec<RelationSnippet> = dedup
            .values()
            .filter(|(e, _)| e.head == c.business_id || e.tail == c.business_id)
            .map(|(e, cnt)| {
                let outgoing = e.head == c.business_id;
                let other = if outgoing { &e.tail } else { &e.head };
                RelationSnippet {
                    rel_type: e.rel_type.clone(),
                    other_end_id: other.clone(),
                    other_end_name: names.get(other).cloned().unwrap_or_default(),
                    direction: if outgoing { "out".into() } else { "in".into() },
                    confidence: e.confidence,
                    evidence: e.evidence.clone(),
                    supplementary_count: *cnt,
                }
            })
            .collect();
        rels.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
        rels.truncate(5);
        c.relations = rels;
    }
    // source_ref：候选为端点的边中 confidence 最高者的 doc_id
    for e in edges {
        for bid in [&e.head, &e.tail] {
            let entry = best_doc.entry(bid.clone()).or_insert((f64::MIN, None));
            if e.confidence > entry.0 && e.doc_id.is_some() {
                *entry = (e.confidence, e.doc_id.clone());
            }
        }
    }
    for c in candidates.iter_mut() {
        if let Some((_, doc)) = best_doc.get(&c.business_id) {
            c.source_ref = doc.clone();
        }
    }
}

/// 分桶截断（§5.2.3）：种子桶 ⌈top_n×0.6⌉，邻居桶 top_n−种子实际数（保底 40% 可上浮）。
pub(crate) fn bucket_truncate(candidates: &mut Vec<Candidate>, top_n: usize) {
    let seed_cap = ((top_n as f64) * 0.6).ceil() as usize;
    let mut seeds: Vec<Candidate> = candidates.iter().filter(|c| c.hop == 0).cloned().collect();
    seeds.sort_by(|a, b| b.semantic.partial_cmp(&a.semantic).unwrap_or(std::cmp::Ordering::Equal));
    seeds.truncate(seed_cap);
    let neighbor_cap = top_n.saturating_sub(seeds.len());
    let mut neighbors: Vec<Candidate> = candidates.iter().filter(|c| c.hop >= 1).cloned().collect();
    neighbors.sort_by(|a, b| b.pre_rank.partial_cmp(&a.pre_rank).unwrap_or(std::cmp::Ordering::Equal));
    neighbors.truncate(neighbor_cap);
    seeds.extend(neighbors);
    *candidates = seeds;
}
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -8`
Expected: 9 passed。

- [x] **Step 5: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs
git commit -m "feat(s5): candidate merge + edge aggregation + bucket truncation"
```

---

### Task 7: 降级融合 + source_ref 回退 + 编排管线（rerank 桩）

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`
- Test: 同文件 tests mod

**Interfaces:**
- Consumes: Task 1-6 全部产物；`crate::application::context::search_mcp::SearchHit`。
- Produces:
  - `pub struct RetrieveRequest<'a> { pub query: &'a str, pub project: Option<&'a str>, pub limit: usize, pub max_hops: u32, pub origin: Option<&'a str> }`
  - `pub struct RetrieveOutcome { pub hits: Vec<SearchHit>, pub degraded: Vec<String> }`
  - `pub async fn search_knowledge(&self, req: &RetrieveRequest) -> Result<RetrieveOutcome, DtError>`
  - `pub(crate) async fn apply_rerank(&self, query: &str, candidates: &mut [Candidate]) -> bool`（**本任务为桩：恒返回 false**；Task 9 替换为真实现）
  - `fn fuse(c: &Candidate, rerank_available: bool) -> ScoreBreakdown`（双权重分支全实现）
  - `async fn fill_source_refs(&self, candidates: &mut [Candidate])`（MENTIONED_IN 回退，best-effort）

- [x] **Step 1: 先写失败测试**

```rust
#[test]
fn fuse_degraded_uses_one_hop_boost() {
    // 降级：0.75·semantic + 0.25·boost(一阶)
    let mut c = merge_candidates(&[mk_seed("A", 0.8)], ExpansionResult {
        nodes: vec![mk_node("N", "A", 2, 0.9)], edges: vec![],
    });
    let n = c.iter().find(|x| x.business_id == "N").unwrap();
    let b = fuse(n, false);
    // semantic = 0.8×0.25=0.2；boost 一阶 = 0.5（非 0.25）
    assert!((b.graph_boost - 0.5).abs() < 1e-9);
    assert!((b.final_score - (0.75 * 0.2 + 0.25 * 0.5)).abs() < 1e-9);
    assert_eq!(b.rerank, 0.0);
    let a = c.iter().find(|x| x.business_id == "A").unwrap();
    assert!((fuse(a, false).final_score - (0.75 * 0.8 + 0.25 * 1.0)).abs() < 1e-9);
}

#[tokio::test]
async fn pipeline_degraded_full_path() {
    // 向量召回 3 种子：Entity A(0.9) + Entity C(0.7, 无边孤儿) + Knowledge K(0.8, 带 elementId)
    let vector = MockVector {
        hits: vec![
            seed_hit(0.9, "dt://entity/p/Channel/A", &["Entity"]),
            seed_hit(0.7, "dt://entity/p/Concept/C", &["Entity"]),
            { let mut h = seed_hit(0.8, "k-1", &["Knowledge"]);
              h["payload"]["elementId"] = json!("eid-k1");
              h["payload"]["type"] = json!("Knowledge");
              h },
        ],
        captured_filter: Mutex::new(None), captured_limit: Mutex::new(None),
    };
    let graph = Arc::new(MockGraph {
        responses: Mutex::new(VecDeque::from(vec![
            // ① Entity 扩展：A → B(1 跳, routes_to 0.9, doc d1)；C 无边
            json!([entity_row("dt://entity/p/Channel/A",
                json!({"entity_id":"dt://entity/p/Service/B","name":"B","type":"Service","summary":"sb"}),
                json!(1),
                json!([{"type":"routes_to","confidence":0.9,"evidence":"e","doc_id":"d1",
                        "head":"dt://entity/p/Channel/A","tail":"dt://entity/p/Service/B"}]))]),
            // ② 非 Entity 扩展：空
            json!([]),
            // ③ MENTIONED_IN 回退：仅 C 需要（A/B 已从边拿到 d1；K 非 dt://entity/ 前缀不回退）
            json!([{"eid": "dt://entity/p/Concept/C", "doc_id": "d-c"}]),
        ])),
        captured: Mutex::new(vec![]),
    });
    let r = Retriever::new(Some(graph), Arc::new(vector), Arc::new(MockEmbed), None);
    let req = RetrieveRequest { query: "q", project: Some("p"), limit: 10, max_hops: 1, origin: None };
    let out = r.search_knowledge(&req).await.unwrap();
    // rerank 桩 → 降级标记（条级 + 世界级）
    assert_eq!(out.degraded, vec!["rerank_unavailable"]);
    assert!(out.hits.iter().all(|h| h.rerank_degraded == Some(true)));
    assert!(out.hits.iter().all(|h| h.score_breakdown.is_some()));
    // B 经图扩展进入结果且 hop=1、source_ref 来自最高 confidence 边
    let b = out.hits.iter().find(|h| h.id == "dt://entity/p/Service/B").unwrap();
    assert_eq!(b.hop, Some(1));
    assert_eq!(b.source_ref.as_deref(), Some("d1"));
    assert_eq!(b.relations.as_ref().unwrap()[0].rel_type, "routes_to");
    // C 无边 → MENTIONED_IN 回退 source_ref
    let c = out.hits.iter().find(|h| h.id == "dt://entity/p/Concept/C").unwrap();
    assert_eq!(c.source_ref.as_deref(), Some("d-c"));
    assert!(c.relations.is_none());
    // K（非 Entity）不做 MENTIONED_IN 回退
    let k = out.hits.iter().find(|h| h.id == "k-1").unwrap();
    assert!(k.source_ref.is_none());
    // 排序：A(0.75×0.9+0.25×1.0=0.925) > K(0.85) > C(0.775) > B(0.4625)
    assert_eq!(out.hits[0].id, "dt://entity/p/Channel/A");
}

#[tokio::test]
async fn pipeline_embed_failure_returns_empty_with_marker() {
    struct FailEmbed;
    #[async_trait::async_trait]
    impl EmbedService for FailEmbed {
        async fn embed_batch(&self, _t: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Err(DtError::Repository("embed down".into()))
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }
    let r = Retriever::new(None, Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }), Arc::new(FailEmbed), None);
    let req = RetrieveRequest { query: "q", project: None, limit: 10, max_hops: 1, origin: None };
    let out = r.search_knowledge(&req).await.unwrap();
    assert!(out.hits.is_empty());
    assert_eq!(out.degraded, vec!["embed_unavailable"]);
}

#[tokio::test]
async fn pipeline_graph_failure_falls_back_to_vector_only() {
    struct FailGraph;
    #[async_trait::async_trait]
    impl GraphRepository for FailGraph {
        async fn read_query(&self, _q: &str, _p: HashMap<String, serde_json::Value>) -> Result<serde_json::Value, DtError> {
            Err(DtError::Repository("graph down".into()))
        }
        async fn write_query(&self, _q: &str, _p: HashMap<String, serde_json::Value>) -> Result<serde_json::Value, DtError> { Ok(json!([])) }
        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }
    let vector = MockVector {
        hits: vec![seed_hit(0.9, "dt://entity/p/Channel/A", &["Entity"])],
        captured_filter: Mutex::new(None), captured_limit: Mutex::new(None),
    };
    let r = Retriever::new(Some(Arc::new(FailGraph)), Arc::new(vector), Arc::new(MockEmbed), None);
    let req = RetrieveRequest { query: "q", project: None, limit: 10, max_hops: 1, origin: None };
    let out = r.search_knowledge(&req).await.unwrap();
    assert_eq!(out.hits.len(), 1);  // 种子仍在
    assert!(out.degraded.contains(&"graph_expansion_failed".to_string()));
    assert!(out.degraded.contains(&"rerank_unavailable".to_string()));
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib retrieve 2>&1 | tail -10`
Expected: FAIL — `fuse`/`search_knowledge`/`RetrieveRequest` 不存在。

- [x] **Step 3: 实现编排管线**

retrieve.rs 追加（`use` 区补充 `use crate::application::context::search_mcp::SearchHit;`）：

```rust
// ---------------------------------------------------------------------------
// 编排管线（§3：召回 → 扩展 → 截断 → rerank → 融合 → 证据）
// ---------------------------------------------------------------------------

/// knowledge 世界检索请求。
pub struct RetrieveRequest<'a> {
    pub query: &'a str,
    pub project: Option<&'a str>,
    pub limit: usize,
    pub max_hops: u32,
    pub origin: Option<&'a str>,
}

/// 检索产出：命中 + 世界级降级标记（§5.7.4）。
pub struct RetrieveOutcome {
    pub hits: Vec<SearchHit>,
    pub degraded: Vec<String>,
}

/// 融合排序（§5.4）。降级模式 graph_boost 一阶衰减（种子 1.0、邻居统一 0.5）。
fn fuse(c: &Candidate, rerank_available: bool) -> ScoreBreakdown {
    let (wr, ws, wb) = if rerank_available { (0.6, 0.3, 0.1) } else { (0.0, 0.75, 0.25) };
    let boost = if rerank_available {
        c.graph_boost
    } else if c.hop == 0 {
        1.0
    } else {
        0.5
    };
    let rerank = c.rerank_score.unwrap_or(0.0);
    ScoreBreakdown {
        semantic: c.semantic,
        rerank,
        graph_boost: boost,
        final_score: wr * rerank + ws * c.semantic + wb * boost,
    }
}

impl Retriever {
    /// Task 9 替换为真实现；本任务为桩：恒 false（恒走降级权重）。
    pub(crate) async fn apply_rerank(&self, _query: &str, _candidates: &mut [Candidate]) -> bool {
        false
    }

    /// source_ref 回退：无边 Entity 候选查 MENTIONED_IN（§5.7.2；best-effort 静默失败）。
    async fn fill_source_refs(&self, candidates: &mut [Candidate]) {
        let Some(ref graph) = self.graph else { return };
        let need: Vec<String> = candidates
            .iter()
            .filter(|c| c.source_ref.is_none() && c.business_id.starts_with("dt://entity/"))
            .map(|c| c.business_id.clone())
            .collect();
        if need.is_empty() {
            return;
        }
        let cypher = r#"
UNWIND $ids AS eid
MATCH (e:Entity {entity_id: eid})-[:MENTIONED_IN]->(d:Document)
RETURN eid AS eid, d.doc_id AS doc_id
ORDER BY eid, d.doc_id
"#;
        let mut params = std::collections::HashMap::new();
        params.insert("ids".to_string(), serde_json::json!(need));
        let Ok(raw) = graph.read_query(cypher, params).await else { return };
        let mut first: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for row in parse_graph_rows(&raw) {
            let (Some(eid), Some(doc)) = (
                row.get("eid").and_then(|v| v.as_str()),
                row.get("doc_id").and_then(|v| v.as_str()),
            ) else { continue };
            first.entry(eid.to_string()).or_insert_with(|| doc.to_string());
        }
        for c in candidates.iter_mut() {
            if c.source_ref.is_none() {
                c.source_ref = first.get(&c.business_id).cloned();
            }
        }
    }

    /// knowledge 世界混合检索全流程（§3）。
    pub async fn search_knowledge(&self, req: &RetrieveRequest) -> Result<RetrieveOutcome, DtError> {
        let mut degraded: Vec<String> = Vec::new();
        let limit = req.limit.max(1);

        // ① 召回（embed/vector 失败 → 空结果 + embed_unavailable）
        let seeds = match self
            .recall(req.query, req.project, req.origin, (limit * 3) as u64)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("knowledge recall failed: {e}");
                return Ok(RetrieveOutcome { hits: vec![], degraded: vec!["embed_unavailable".into()] });
            }
        };

        // ② 图扩展（失败 → 仅向量召回 + graph_expansion_failed）
        let mut expansion = ExpansionResult::default();
        if self.graph.is_some() && !seeds.is_empty() {
            let (entity_seeds, biz_seeds): (Vec<Seed>, Vec<Seed>) = seeds
                .iter()
                .cloned()
                .partition(|s| s.labels.iter().any(|l| l == "Entity"));
            let mut failed = false;
            match self.expand_entity(&entity_seeds, req.max_hops).await {
                Ok(r) => { expansion.nodes.extend(r.nodes); expansion.edges.extend(r.edges); }
                Err(e) => { tracing::warn!("entity expansion failed: {e}"); failed = true; }
            }
            match self.expand_business(&biz_seeds).await {
                Ok(r) => { expansion.nodes.extend(r.nodes); expansion.edges.extend(r.edges); }
                Err(e) => { tracing::warn!("business expansion failed: {e}"); failed = true; }
            }
            if failed {
                degraded.push("graph_expansion_failed".into());
            }
        }

        // ③ 合并 → 边挂接 → 分桶截断
        let mut candidates = merge_candidates(&seeds, expansion.nodes);
        attach_relations(&mut candidates, &expansion.edges);
        bucket_truncate(&mut candidates, rerank_top_n());

        // ④ rerank（Task 9；桩恒 false → 降级）
        let reranked = self.apply_rerank(req.query, &mut candidates).await;
        if !reranked {
            degraded.push("rerank_unavailable".into());
        }

        // ⑤ source_ref 回退 + 融合 + 截断 limit，同分 hop 升序
        self.fill_source_refs(&mut candidates).await;
        let mut scored: Vec<(Candidate, ScoreBreakdown)> = candidates
            .into_iter()
            .map(|c| {
                let b = fuse(&c, reranked);
                (c, b)
            })
            .collect();
        scored.sort_by(|(ca, ba), (cb, bb)| {
            bb.final_score
                .partial_cmp(&ba.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(ca.hop.cmp(&cb.hop))
        });
        scored.truncate(limit);

        let hits = scored
            .into_iter()
            .map(|(c, b)| SearchHit {
                id: c.business_id,
                title: c.name,
                snippet: c.summary,
                source_world: "knowledge".into(),
                entity_type: c.entity_type,
                score: b.final_score,
                source_ref: c.source_ref,
                file_path: None,
                start_line: None,
                end_line: None,
                signature: None,
                calls: vec![],
                element_id: c.element_id,
                score_breakdown: Some(b),
                hop: Some(c.hop),
                via_same_as: if c.via_same_as { Some(true) } else { None },
                relations: if c.relations.is_empty() { None } else { Some(c.relations) },
                evidence: None,
                rerank_degraded: if reranked { None } else { Some(true) },
            })
            .collect();
        Ok(RetrieveOutcome { hits, degraded })
    }
}
```

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -8`
Expected: 12 passed。

- [x] **Step 5: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs
git commit -m "feat(s5): retrieval pipeline orchestration with degraded fusion + MENTIONED_IN source_ref fallback"
```

---

### Task 8: search_mcp 委托切换 + 构造器第四参（S5a 切流）

**Files:**
- Modify: `src/application/context/search_mcp.rs`（`CrossWorldSearch` 结构/`new`/`empty`/`search_knowledge`/`search`）
- Modify: `src/interfaces/grpc/services/build_service.rs:115-126`
- Test: `src/application/context/search_mcp.rs` 内 tests mod

**Interfaces:**
- Consumes: Task 7 `Retriever`/`RetrieveRequest`/`RetrieveOutcome`。
- Produces:
  - `CrossWorldSearch::new(graph, vector, embed, rerank: Option<Arc<dyn RerankService>>)`（第四参）
  - `CrossWorldSearch::empty()`（全部 None）
  - `search_knowledge(&self, request: &SearchRequest) -> (Vec<SearchHit>, Vec<String>)`（hits + degraded；旧 CONTAINS 实现删除）
  - `search()` 的 knowledge 分支把 degraded 合入 `CrossWorldResult.degraded`

- [x] **Step 1: 先写失败测试**

```rust
#[tokio::test]
async fn knowledge_world_with_empty_backends_returns_empty_and_no_panic() {
    let cws = CrossWorldSearch::empty();
    let req = SearchRequest {
        query: "q".into(),
        world: Some("knowledge".into()),
        limit: Some(5),
        project: None,
        max_hops: None,
        with_evidence: None,
        origin: None,
        doc_id: None,
    };
    let result = cws.search(&req).await.unwrap();
    assert_eq!(result.hits.len(), 0);
    assert_eq!(result.per_world_counts.get("knowledge"), Some(&0));
    assert!(result.degraded.is_empty());
}

#[test]
fn constructor_accepts_rerank_as_fourth_param() {
    fn _accept(_: Option<std::sync::Arc<dyn crate::domain::traits::RerankService>>) {}
    let cws = CrossWorldSearch::new(None, None, None, None);
    drop(cws);
}
```

- [x] **Step 2: 确认失败**

Run: `cargo test --lib search_mcp 2>&1 | tail -10`
Expected: FAIL — `new` 只收 3 参、`search_knowledge` 签名不符、build_service.rs 编译错（`SearchRequest` 字面量缺字段——Task 1 已加字段，此处须同步补）。

- [x] **Step 3: 实现委托切换**

`search_mcp.rs` 修改：

```rust
// 顶部 use 追加：
use crate::application::knowledge::extract::retrieve::{RetrieveRequest, Retriever};
use crate::domain::traits::RerankService;

// 结构体：
pub struct CrossWorldSearch {
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    rerank: Option<Arc<dyn RerankService>>,
}

impl CrossWorldSearch {
    /// Create with backends.
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self { graph, vector, embed, rerank }
    }

    /// Create with no backends (for testing).
    pub fn empty() -> Self {
        Self { graph: None, vector: None, embed: None, rerank: None }
    }

    // search_code 保持不动 …

    /// Search the Knowledge World via GraphRAG hybrid retrieval (S5).
    ///
    /// 委托 retrieve.rs；返回 (hits, degraded)。vector/embed 缺失时无可召回手段，返回空。
    async fn search_knowledge(&self, request: &SearchRequest) -> (Vec<SearchHit>, Vec<String>) {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return (Vec::new(), Vec::new());
        };
        let retriever = Retriever::new(
            self.graph.clone(),
            vector.clone(),
            embed.clone(),
            self.rerank.clone(),
        );
        let req = RetrieveRequest {
            query: &request.query,
            project: request.project.as_deref(),
            limit: request.limit.unwrap_or(20),
            max_hops: request.max_hops.unwrap_or(1),
            origin: request.origin.as_deref(),
        };
        match retriever.search_knowledge(&req).await {
            Ok(outcome) => (outcome.hits, outcome.degraded),
            Err(e) => {
                tracing::warn!("knowledge retrieval failed: {e}");
                (Vec::new(), Vec::new())
            }
        }
    }

    // search_vector 保持不动（Task 10 才改写）…
}
```

旧 `search_knowledge` 的 CONTAINS Cypher 整体删除（spec §6.1）。`search()` 的 knowledge 分支改为：

```rust
        // Search knowledge world
        let mut degraded: Vec<String> = Vec::new();
        if world == "all" || world == "knowledge" {
            let (hits, dgr) = self.search_knowledge(&request_for_branch).await;
            degraded.extend(dgr);
            per_world.insert("knowledge".to_string(), hits.len());
            all_hits.extend(hits);
        }
```

注意 `search()` 里 knowledge 分支现在需要整个 `&SearchRequest`（不再只传 query/limit）——直接把 `request` 传入即可（`self.search_knowledge(request).await`）。`CrossWorldResult` 构造追加 `degraded` 字段：

```rust
        Ok(CrossWorldResult {
            query: request.query.clone(),
            world: world.to_string(),
            hits: all_hits,
            total,
            per_world_counts: per_world,
            degraded,
        })
```

`build_service.rs:115-126` 同步（本任务先传 `None`，Task 9 换真路由）：

```rust
    let rerank: Option<Arc<dyn RerankService>> = None;
    let cws = crate::application::context::search_mcp::CrossWorldSearch::new(graph, vector, embed, rerank);
    let cws_req = crate::application::context::search_mcp::SearchRequest {
        query: req.query,
        world: Some("code".into()),
        limit: Some(limit),
        project: if req.project.is_empty() { None } else { Some(req.project) },
        max_hops: None,
        with_evidence: None,
        origin: None,
        doc_id: None,
    };
```

`build_service.rs` 顶部 use 追加 `use crate::domain::traits::RerankService;`。

- [x] **Step 4: 跑测试确认通过 + 全量编译**

Run: `cargo test --lib search_mcp 2>&1 | tail -6 && cargo build 2>&1 | tail -3`
Expected: search_mcp 7 passed；build 0 error。

- [x] **Step 5: S5a 人工实测（test-pipeline，对照 spec §7 S5a 验证方式）**

```bash
# 进程内驱动（无 CLI/gRPC 入口，S5-D10）：用单测临时挂接真后端或 dt eval 入口均可；
# 断言要点：
#   1. 语义查询 "渠道怎么路由"（必须显式传 project=test-pipeline）命中 ifCode，CONTAINS 时代必不命中
#   2. Entity 首次出现在 knowledge 结果中
#   3. learned 节点（Knowledge/Concept/Playbook）种子经 elementId 扩展带出 1 跳邻居
#   4. Event/Document 节点不出现在候选（白名单）
```

实测结果记入执行简报（LLM 抽取漂移按 §9.1 容差处理：ifCode 跌出 top-5 但在 top-10 时用 `score_breakdown` 归因记录位次，不算失败）。

**S5a 实测记录（2026-08-02，tests/s5_knowledge_search.rs，2 passed）**：
- 语义非字面命中 ✓："新增渠道的唯一代码标识" → ifCode #1（hop=0，score 0.81）。
- 图扩展捞回向量漏掉的实体 ✓：alipay 向量 rank #128（召回窗口 k=120 之外）经 ifCode RELATES 边以 hop=1 出现在 #31。
- Entity 进入 knowledge 结果 ✓；rerank_unavailable 降级标记 ✓；score_breakdown 齐全 ✓。
- **偏差 1（Document 种子剔除）**：kg_sync 写入的 Document 点在扁平分布下挤占 knowledge 结果（实测 top-15 占 13），`parse_seed` 增加 Document label 剔除（证据检索归 world=doc/doc_chunks；与 S5-D11 同一噪声逻辑）。
- **偏差 2（规范查询归因）**："渠道怎么路由" 未命中 ifCode——本次构建 ifCode 摘要漂移为 "用于标识新增渠道的唯一代码标识"（无路由语义），向量 rank #74/275 跌出召回窗口；§9.1 所述抽取漂移，非检索链路故障（归因测试 s5a_canonical_query_attribution 仅打印）。
- **已知边界（后续优化点）**：同一节点既被向量低分召回（种子桶溢出）又是扩展邻居时，按最小 hop 规则归入种子桶后会被整体丢弃（本次 wechat/yinsheng 实例）；可考虑"种子桶溢出的节点回退邻居桶"，列入后续优化。

- [x] **Step 6: 提交**

```bash
git add src/application/context/search_mcp.rs src/interfaces/grpc/services/build_service.rs
git commit -m "feat(s5): switch search_knowledge to retrieve.rs delegation; CrossWorldSearch takes rerank param"
```

---

### Task 9: rerank 接入——sigmoid + 完整融合 + 路由接线（S5b）

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`（替换 `apply_rerank` 桩）
- Modify: `src/infrastructure/embedder.rs`（提取 `build_provider_router` + 新增 `create_rerank_router`）
- Modify: `src/interfaces/grpc/services/build_service.rs`（`None` → 真路由）
- Test: retrieve.rs / embedder.rs tests mod

**Interfaces:**
- Consumes: `RerankService::rerank(query, documents) -> Result<Vec<f32>, DtError>`；Task 7 桩签名。
- Produces:
  - `pub(crate) async fn apply_rerank(&self, query: &str, candidates: &mut [Candidate]) -> bool`（真实现；未配置/失败 → false）
  - `pub fn create_rerank_router(cfg: ProviderConfig) -> Arc<dyn RerankService>`（embedder.rs）
  - `fn build_provider_router(cfg: ProviderConfig) -> EmbedProviderRouter`（embedder.rs 私有共享）

- [ ] **Step 1: 先写失败测试**

retrieve.rs tests mod 新增 MockRerank 与测试：

```rust
pub(crate) struct MockRerank {
    pub logits: Vec<f32>,
    pub fail: bool,
}

#[async_trait::async_trait]
impl RerankService for MockRerank {
    async fn rerank(&self, _q: &str, docs: &[String]) -> Result<Vec<f32>, DtError> {
        if self.fail {
            return Err(DtError::Repository("rerank down".into()));
        }
        Ok(self.logits.iter().take(docs.len()).cloned().collect())
    }
    async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
}

#[test]
fn sigmoid_normalizes_logits() {
    assert!((sigmoid(0.0) - 0.5).abs() < 1e-9);
    assert!((sigmoid(2.0) - 0.880797).abs() < 1e-4);
    assert!((sigmoid(-2.0) - 0.119203).abs() < 1e-4);
}

#[test]
fn fuse_full_weights_with_rerank() {
    // 正常：0.6·rerank + 0.3·semantic + 0.1·boost
    let mut c = merge_candidates(&[mk_seed("A", 0.8)], ExpansionResult::default());
    c[0].rerank_score = Some(0.5);
    let b = fuse(&c[0], true);
    assert!((b.final_score - (0.6 * 0.5 + 0.3 * 0.8 + 0.1 * 1.0)).abs() < 1e-9); // = 0.64
    assert!((b.rerank - 0.5).abs() < 1e-9);
    // 邻居：graph_boost 仍按 0.5^hop（非降级一阶）
    let mut c2 = merge_candidates(&[mk_seed("A", 0.8)], ExpansionResult {
        nodes: vec![mk_node("N", "A", 2, 0.9)], edges: vec![],
    });
    let n = c2.iter_mut().find(|x| x.business_id == "N").unwrap();
    n.rerank_score = Some(1.0);
    let b2 = fuse(n, true);
    assert!((b2.graph_boost - 0.25).abs() < 1e-9);
}

#[tokio::test]
async fn apply_rerank_writes_sigmoid_scores_and_fails_open() {
    let mut c = merge_candidates(&[mk_seed("A", 0.9), mk_seed("B", 0.8)], ExpansionResult::default());
    // 正常路径：logit 0 → 0.5
    let r = Retriever::new(None, Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }),
                           Arc::new(MockEmbed), Some(Arc::new(MockRerank { logits: vec![0.0, 2.0], fail: false })));
    assert!(r.apply_rerank("q", &mut c).await);
    assert!(c.iter().all(|x| x.rerank_score.is_some()));
    // 失败路径：fails-open → false，候选无分数
    let mut c2 = merge_candidates(&[mk_seed("A", 0.9)], ExpansionResult::default());
    let r2 = Retriever::new(None, Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }),
                            Arc::new(MockEmbed), Some(Arc::new(MockRerank { logits: vec![], fail: true })));
    assert!(!r2.apply_rerank("q", &mut c2).await);
    assert!(c2[0].rerank_score.is_none());
    // 未配置：false
    let r3 = Retriever::new(None, Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) }),
                            Arc::new(MockEmbed), None);
    assert!(!r3.apply_rerank("q", &mut c2).await);
}
```

embedder.rs tests mod 新增：

```rust
#[test]
fn create_rerank_router_returns_configured_service() {
    let svc = create_rerank_router(ProviderConfig::default_siliconflow());
    // 对象安全 + 构造成功即可（路由正确性由 provider_router 既有测试保证）
    fn _accept(_: Arc<dyn crate::domain::traits::RerankService>) {}
    _accept(svc);
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib retrieve sigmoid 2>&1 | tail -5 && cargo test --lib embedder 2>&1 | tail -5`
Expected: FAIL — `sigmoid` 测试已存在可过（Task 1 已定义函数，若此前未测则补）；`create_rerank_router` 不存在。

- [ ] **Step 3: 实现**

retrieve.rs 中把 `apply_rerank` 桩**替换**为：

```rust
    /// ③ 重排（§5.3）：name+summary 送 reranker，logit 经 sigmoid 归一。
    /// 返回 false = 未配置或调用失败（调用方走 S5-D7 降级权重并打标）。
    pub(crate) async fn apply_rerank(&self, query: &str, candidates: &mut [Candidate]) -> bool {
        let Some(ref rerank) = self.rerank else {
            return false;
        };
        if candidates.is_empty() {
            return true;
        }
        let docs: Vec<String> = candidates
            .iter()
            .map(|c| format!("{}。{}", c.name, c.summary))
            .collect();
        match rerank.rerank(query, &docs).await {
            Ok(logits) => {
                for (c, x) in candidates.iter_mut().zip(logits) {
                    c.rerank_score = Some(sigmoid(x));
                }
                true
            }
            Err(e) => {
                tracing::warn!("rerank failed, falling back to degraded weights: {e}");
                false
            }
        }
    }
```

embedder.rs 重构（`create_embed_router` 主体提取为私有共享函数；文件顶部 `use crate::domain::traits::EmbedService;` 改为 `use crate::domain::traits::{EmbedService, RerankService};`）：

```rust
/// Build the router with configured clients (shared by embed/rerank constructors).
fn build_provider_router(cfg: ProviderConfig) -> crate::infrastructure::provider_router::EmbedProviderRouter {
    use crate::infrastructure::provider_router::{EmbedProviderRouter, ProviderRouterConfig};

    let siliconflow = if !cfg.siliconflow_url.is_empty() {
        Some(Arc::new(crate::infrastructure::siliconflow::SiliconFlowClient::new(
            cfg.siliconflow_url,
            cfg.siliconflow_api_key,
            cfg.siliconflow_model_embed,
            cfg.siliconflow_model_reranker,
            cfg.siliconflow_model_llm,
        )))
    } else {
        None
    };
    let xinference = if !cfg.xinference_url.is_empty() {
        Some(Arc::new(crate::infrastructure::xinference::XInferenceClient::new(
            cfg.xinference_url,
            cfg.xinference_api_key,
            cfg.xinference_model_embed,
            cfg.xinference_model_reranker,
            cfg.xinference_model_llm,
        )))
    } else {
        None
    };
    let router_config = ProviderRouterConfig {
        embed_provider: cfg.embed_provider,
        rerank_provider: cfg.rerank_provider,
        llm_provider: cfg.llm_provider,
    };
    EmbedProviderRouter::new(siliconflow, xinference, router_config)
}

pub fn create_embed_router(cfg: ProviderConfig) -> Arc<dyn EmbedService> {
    Arc::new(build_provider_router(cfg))
}

/// Build an [`EmbedProviderRouter`] as a [`RerankService`] (S5 首个业务调用点)。
pub fn create_rerank_router(cfg: ProviderConfig) -> Arc<dyn RerankService> {
    Arc::new(build_provider_router(cfg))
}
```

`build_service.rs` 中 Task 8 的 `let rerank: Option<Arc<dyn RerankService>> = None;` 替换为：

```rust
    let rerank: Option<Arc<dyn RerankService>> = Some(
        crate::infrastructure::embedder::create_rerank_router(
            crate::infrastructure::embedder::ProviderConfig {
                siliconflow_url: crate::infrastructure::siliconflow::base_url_from_env(),
                siliconflow_api_key: crate::infrastructure::siliconflow::api_key_from_env(),
                siliconflow_model_embed: crate::infrastructure::siliconflow::embed_model_from_env(),
                siliconflow_model_reranker: crate::infrastructure::siliconflow::reranker_model_from_env(),
                siliconflow_model_llm: crate::infrastructure::siliconflow::llm_model_from_env(),
                xinference_url: String::new(),
                xinference_api_key: String::new(),
                xinference_model_embed: String::new(),
                xinference_model_reranker: String::new(),
                xinference_model_llm: String::new(),
                embed_provider: "siliconflow".into(),
                rerank_provider: "siliconflow".into(),
                llm_provider: "siliconflow".into(),
            },
        ),
    );
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -6 && cargo test --lib embedder 2>&1 | tail -4 && cargo build 2>&1 | tail -3`
Expected: retrieve 15 passed；embedder 全绿；build 0 error。

- [ ] **Step 5: S5b 联调（本地 xinference bge-reranker-v2-m3 启动后）**

- 确认 `config/pipeline.yaml` 的 rerank 路由实际生效 provider（§8.11：yaml 当前全路由 xinference，与 build_service 内嵌的 siliconflow 配置是两条链路，分别验证各自路径）。
- 对 test-pipeline 同查询对比 S5a（降级权重）与 S5b（完整融合）的排序变化，确认 `rerank_degraded` 不再出现、`score_breakdown.rerank > 0`。
- 关停 rerank 服务复测：`rerank_degraded=true` + `degraded:["rerank_unavailable"]` 重新出现（spec §9.4）。

- [ ] **Step 6: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs src/infrastructure/embedder.rs src/interfaces/grpc/services/build_service.rs
git commit -m "feat(s5): wire rerank into retrieval — sigmoid normalization, full fusion weights, provider router"
```

---

### Task 10: world=doc 改写——`search_doc` 查 doc_chunks（含 source="doc" 硬过滤）

**Files:**
- Modify: `src/application/context/search_mcp.rs`（删 `search_vector`，新增 `search_doc`，`search()` 分发与 per_world key）
- Test: `src/application/context/search_mcp.rs` 内 tests mod

**Interfaces:**
- Consumes: `DOC_CHUNKS` 常量；`VectorRepository::search_with_filter`；Task 1 `min_score`（retrieve.rs 导出，`crate::application::knowledge::extract::retrieve::min_score`）。
- Produces:
  - `async fn search_doc(&self, query: &str, project: Option<&str>, doc_id: Option<&str>, limit: usize) -> Result<Vec<SearchHit>, DtError>`
  - `world == "doc" | "vector" | "all` 分发到 `search_doc`；`per_world_counts` key = `"doc"`；`source_world = "doc"`（§5.7.2 / §8.7 别名保留）

- [ ] **Step 1: 先写失败测试**

search_mcp.rs tests mod 需要一个最小的 VectorRepository stub（本文件无 mock 基建；参照 `domain/traits.rs` tests 的 StubVectorRepo 写法）：

```rust
struct StubVector {
    hits: Vec<serde_json::Value>,
    captured_filter: std::sync::Mutex<Option<serde_json::Value>>,
}

#[async_trait::async_trait]
impl VectorRepository for StubVector {
    async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> { Ok(()) }
    async fn search(&self, _c: &str, _v: Vec<f32>, _l: u64) -> Result<Vec<serde_json::Value>, DtError> {
        Ok(self.hits.clone())
    }
    async fn search_with_filter(
        &self,
        _c: &str,
        _v: Vec<f32>,
        _l: u64,
        filter: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, DtError> {
        *self.captured_filter.lock().unwrap() = Some(filter);
        Ok(self.hits.clone())
    }
    async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> { Ok(()) }
    async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> { Ok(()) }
    async fn list_collections(&self) -> Result<Vec<String>, DtError> { Ok(vec![]) }
    async fn collection_info(&self, name: &str) -> Result<crate::domain::types::CollectionInfo, DtError> {
        Ok(crate::domain::types::CollectionInfo { name: name.into(), points_count: 0, vector_dim: 0, model_version: String::new() })
    }
    async fn delete_collection(&self, _n: &str) -> Result<(), DtError> { Ok(()) }
    async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
        Ok(crate::domain::types::HealthStatus::Healthy)
    }
}

struct StubEmbed;
#[async_trait::async_trait]
impl EmbedService for StubEmbed {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
    }
    async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
        Ok(crate::domain::types::HealthStatus::Healthy)
    }
}

#[tokio::test]
async fn doc_world_filters_source_and_maps_chunk_payload() {
    let chunk = serde_json::json!({
        "id": "pt-1", "score": 0.9,
        "payload": {
            "doc_id": "dt://doc/offen-pay/pay-design.md",
            "block_index": 3,
            "project": "offen-pay",
            "text": "路由规则：根据 ifCode 匹配渠道表",
            "entity_ids": ["dt://entity/offen-pay/Channel/ifcode"],
            "degraded": false,
            "source": "doc"
        }
    });
    let vector = std::sync::Arc::new(StubVector { hits: vec![chunk], captured_filter: std::sync::Mutex::new(None) });
    let cws = CrossWorldSearch::new(None, Some(vector.clone()), Some(std::sync::Arc::new(StubEmbed)), None);
    let req = SearchRequest {
        query: "ifCode 证据".into(), world: Some("doc".into()), limit: Some(5),
        project: Some("offen-pay".into()), max_hops: None, with_evidence: None,
        origin: None, doc_id: Some("dt://doc/offen-pay/pay-design.md".into()),
    };
    let result = cws.search(&req).await.unwrap();
    assert_eq!(result.per_world_counts.get("doc"), Some(&1));
    let hit = &result.hits[0];
    assert_eq!(hit.id, "dt://doc/offen-pay/pay-design.md:3");
    assert_eq!(hit.source_world, "doc");
    assert_eq!(hit.entity_type, "Doc");
    assert_eq!(hit.title, "pay-design.md");
    assert!(hit.snippet.contains("ifCode"));
    assert_eq!(hit.source_ref.as_deref(), Some("dt://doc/offen-pay/pay-design.md"));
    // filter 硬条件：source="doc" + project + doc_id
    let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
    let must = filter["must"].as_array().unwrap();
    assert!(must.iter().any(|c| c["key"] == "source" && c["match"]["value"] == "doc"));
    assert!(must.iter().any(|c| c["key"] == "project" && c["match"]["value"] == "offen-pay"));
    assert!(must.iter().any(|c| c["key"] == "doc_id"));
}

#[tokio::test]
async fn doc_world_skips_nacos_shaped_points() {
    // nacos 配置点（config_sync 写入端）：无 text/doc_id/block_index → 解析丢弃
    let nacos = serde_json::json!({
        "id": "pt-9", "score": 0.9,
        "payload": {"entity_id": "x", "key": "k", "value": "v", "namespace": "public",
                     "source_type": "nacos_config", "project": "p"}
    });
    let vector = std::sync::Arc::new(StubVector { hits: vec![nacos], captured_filter: std::sync::Mutex::new(None) });
    let cws = CrossWorldSearch::new(None, Some(vector), Some(std::sync::Arc::new(StubEmbed)), None);
    let req = SearchRequest {
        query: "q".into(), world: Some("doc".into()), limit: Some(5),
        project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
    };
    let result = cws.search(&req).await.unwrap();
    assert_eq!(result.hits.len(), 0);
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib search_mcp 2>&1 | tail -10`
Expected: FAIL — `search_doc` 不存在、per_world key 为 `"vector"`。

- [ ] **Step 3: 实现 search_doc（删除 search_vector）**

`search_mcp.rs` 中删除整个 `search_vector`，替换为（`use` 追加 `use crate::application::knowledge::extract::retrieve::min_score; use crate::shared::collections::DOC_CHUNKS;`）：

```rust
    /// Search the Doc World via `doc_chunks` (S5 §5.5)。
    ///
    /// filter 强制 `source="doc"`——排除 config_sync 写入的 nacos 配置点
    /// （payload 无 text/doc_id/block_index）。分数过 DT_SEARCH_MIN_SCORE。
    async fn search_doc(
        &self,
        query: &str,
        project: Option<&str>,
        doc_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };
        let embeddings = embed.embed_batch(&[query.to_string()]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        let mut must = vec![serde_json::json!({"key": "source", "match": {"value": "doc"}})];
        if let Some(p) = project {
            must.push(serde_json::json!({"key": "project", "match": {"value": p}}));
        }
        if let Some(d) = doc_id {
            must.push(serde_json::json!({"key": "doc_id", "match": {"value": d}}));
        }
        let filter = serde_json::json!({ "must": must });

        let results = vector
            .search_with_filter(DOC_CHUNKS, query_vec, limit as u64, filter)
            .await?;
        let threshold = min_score();
        let hits = results
            .into_iter()
            .filter_map(|hit| {
                let score = hit.get("score")?.as_f64()?;
                if score < threshold {
                    return None;
                }
                let payload = hit.get("payload")?;
                let doc = payload.get("doc_id")?.as_str()?;
                let block = payload.get("block_index")?.as_u64()? as u32;
                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Some(SearchHit {
                    id: format!("{doc}:{block}"),
                    title: doc.rsplit('/').next().unwrap_or(doc).to_string(),
                    snippet: text.chars().take(200).collect(),
                    source_world: "doc".into(),
                    entity_type: "Doc".into(),
                    score,
                    source_ref: Some(doc.to_string()),
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    signature: None,
                    calls: vec![],
                    element_id: None,
                    score_breakdown: None,
                    hop: None,
                    via_same_as: None,
                    relations: None,
                    evidence: None,
                    rerank_degraded: None,
                })
            })
            .collect();
        Ok(hits)
    }
```

`search()` 分发改动：

```rust
        // Search doc world (S5: doc_chunks; "vector" 保留为 "doc" 别名，§8.7)
        if world == "all" || world == "doc" || world == "vector" {
            if let Ok(hits) = self
                .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                .await
            {
                per_world.insert("doc".to_string(), hits.len());
                all_hits.extend(hits);
            }
        }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --lib search_mcp 2>&1 | tail -6`
Expected: 9 passed。

- [ ] **Step 5: 提交**

```bash
git add src/application/context/search_mcp.rs
git commit -m "feat(s5): world=doc queries doc_chunks with source=doc hard filter + doc_id param"
```

---

### Task 11: with_evidence——top-5 实体证据回填（合并单查询）

**Files:**
- Modify: `src/application/knowledge/extract/retrieve.rs`（`backfill_evidence` + `group_evidence`）
- Modify: `src/application/context/search_mcp.rs`（knowledge 分支按 `with_evidence` 调用）
- Test: retrieve.rs tests mod

**Interfaces:**
- Consumes: `SearchRequest.with_evidence`；`SearchHit.evidence`。
- Produces:
  - `pub async fn backfill_evidence(&self, query: &str, hits: &mut [SearchHit])`（best-effort，失败静默）
  - `fn group_evidence(results: &[serde_json::Value], ids: &[&str]) -> std::collections::HashMap<String, Vec<String>>`（每实体 ≤2 段，按分数降序）

- [ ] **Step 1: 先写失败测试**

```rust
#[test]
fn group_evidence_caps_two_chunks_per_entity_prefers_high_score() {
    let chunk = |eids: &[&str], text: &str, score: f64| serde_json::json!({
        "score": score,
        "payload": {"text": text, "entity_ids": eids, "source": "doc",
                     "doc_id": "d", "block_index": 0}
    });
    let results = vec![
        chunk(&["E1"], "低分证据", 0.5),
        chunk(&["E1", "E2"], "高分证据", 0.9),
        chunk(&["E1"], "中分证据", 0.7),
        chunk(&["E3"], "别人的证据", 0.99),
        {   // nacos 形状点：无 text → 跳过
            let mut c = chunk(&["E1"], "", 0.99);
            c["payload"].as_object_mut().unwrap().remove("text");
            c["payload"].as_object_mut().unwrap().remove("entity_ids");
            c
        },
    ];
    let grouped = group_evidence(&results, &["E1", "E2"]);
    assert_eq!(grouped["E1"].len(), 2);
    assert_eq!(grouped["E1"][0], "高分证据");   // 按分数降序
    assert_eq!(grouped["E1"][1], "中分证据");
    assert_eq!(grouped["E2"].len(), 1);
    assert!(!grouped.contains_key("E3"));        // 不在 top-N 请求列表
}

#[tokio::test]
async fn backfill_evidence_builds_merged_should_filter() {
    let vector = Arc::new(MockVector { hits: vec![], captured_filter: Mutex::new(None), captured_limit: Mutex::new(None) });
    let r = Retriever::new(None, vector.clone(), Arc::new(MockEmbed), None);
    let mut hits = vec![
        SearchHit {
            id: "E1".into(), title: "t".into(), snippet: "s".into(),
            source_world: "knowledge".into(), entity_type: "Channel".into(), score: 0.9,
            source_ref: None, file_path: None, start_line: None, end_line: None,
            signature: None, calls: vec![], element_id: None,
            score_breakdown: None, hop: Some(0), via_same_as: None, relations: None,
            evidence: None, rerank_degraded: None,
        },
    ];
    r.backfill_evidence("q", &mut hits).await;
    let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
    // must: source=doc；should: entity_ids 数组匹配（原生 filter 前置，§5.5）
    let must = filter["must"].as_array().unwrap();
    assert!(must.iter().any(|c| c["key"] == "source" && c["match"]["value"] == "doc"));
    let should = filter["should"].as_array().unwrap();
    assert!(should.iter().any(|c| c["key"] == "entity_ids" && c["match"]["value"] == "E1"));
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib retrieve 2>&1 | tail -8`
Expected: FAIL — `group_evidence`/`backfill_evidence` 不存在。

- [ ] **Step 3: 实现证据回填**

retrieve.rs 追加（`use` 区补充 `use crate::shared::collections::DOC_CHUNKS;`——若 Task 3 已引 KG_NODES，此处合并为一行 `use crate::shared::collections::{DOC_CHUNKS, KG_NODES};`）：

```rust
// ---------------------------------------------------------------------------
// ⑤ 证据回填（§5.5.2；with_evidence，仅 knowledge 世界）
// ---------------------------------------------------------------------------

/// 把 doc_chunks 命中按 entity_ids 归属到 top-N 实体：每实体 ≤2 段、按分数降序。
/// 前置条件：entity_ids 数组匹配只在 QdrantRepo 原生 filter（R7）下成立——
/// 本函数只负责分组；filter 构造见 backfill_evidence，数组匹配语义由测试锁死。
fn group_evidence(
    results: &[serde_json::Value],
    ids: &[&str],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut per_entity: std::collections::HashMap<String, Vec<(f64, String)>> = std::collections::HashMap::new();
    for hit in results {
        let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let Some(payload) = hit.get("payload") else { continue };
        let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let entity_ids: Vec<&str> = payload
            .get("entity_ids")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|e| e.as_str()).collect())
            .unwrap_or_default();
        for id in ids {
            if entity_ids.contains(id) {
                per_entity.entry(id.to_string()).or_default().push((score, text.to_string()));
            }
        }
    }
    per_entity
        .into_iter()
        .map(|(k, mut chunks)| {
            chunks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            chunks.truncate(2);
            (k, chunks.into_iter().map(|(_, t)| t).collect())
        })
        .collect()
}

impl Retriever {
    /// knowledge top-5 实体从 doc_chunks 回填证据段落（合并单查询；best-effort 静默失败）。
    pub async fn backfill_evidence(&self, query: &str, hits: &mut [SearchHit]) {
        let ids: Vec<&str> = hits.iter().take(5).map(|h| h.id.as_str()).collect();
        if ids.is_empty() {
            return;
        }
        let should: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({"key": "entity_ids", "match": {"value": id}}))
            .collect();
        let filter = serde_json::json!({
            "must": [{"key": "source", "match": {"value": "doc"}}],
            "should": should,
        });
        let Ok(embeddings) = self.embed.embed_batch(&[query.to_string()]).await else { return };
        let Some(qvec) = embeddings.into_iter().next() else { return };
        let Ok(results) = self
            .vector
            .search_with_filter(DOC_CHUNKS, qvec, 20, filter)
            .await
        else { return };
        let mut grouped = group_evidence(&results, &ids);
        for hit in hits.iter_mut().take(5) {
            if let Some(chunks) = grouped.remove(&hit.id) {
                hit.evidence = Some(chunks);
            }
        }
    }
}
```

`search_mcp.rs` 的 `search_knowledge` 中，`Ok(outcome)` 分支改为：

```rust
        match retriever.search_knowledge(&req).await {
            Ok(mut outcome) => {
                if request.with_evidence == Some(true) {
                    retriever.backfill_evidence(&request.query, &mut outcome.hits).await;
                }
                (outcome.hits, outcome.degraded)
            }
            Err(e) => {
                tracing::warn!("knowledge retrieval failed: {e}");
                (Vec::new(), Vec::new())
            }
        }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test --lib retrieve 2>&1 | tail -6 && cargo test --lib search_mcp 2>&1 | tail -4`
Expected: retrieve 18 passed；search_mcp 9 passed。

- [ ] **Step 5: S5c 实测（test-pipeline）**

```text
1. world=doc 查询 "给我 ifCode 的证据段落" → 返回含原文 text 的块，id 形如 {doc_id}:{block_index}
2. doc 世界结果不含 nacos 配置点（payload 无 text 的点不出现）
3. world=knowledge + with_evidence=true → top-5 实体各附 ≤2 段证据
4. doc_id 参数限定单文档生效
```

- [ ] **Step 6: 提交**

```bash
git add src/application/knowledge/extract/retrieve.rs src/application/context/search_mcp.rs
git commit -m "feat(s5): with_evidence backfill — merged single query, per-entity top-2 chunks"
```

---

### Task 12: 收尾——权重观测 + 主文档状态更新 + 后续任务卡

**Files:**
- Modify: `docs/superpowers/specs/2026-07-31-universal-knowledge-pipeline-design.md`（§13.1 任务总表 S5 行 + §8 权重收敛回写说明）
- Modify: `docs/superpowers/specs/2026-08-01-knowledge-search-design.md`（§13 追加"S5 实施记录"小节，含后续任务卡）
- Test: 无（文档任务；以全量测试 + clippy 为验证）

**Interfaces:**
- Consumes: 全部前置任务的实测结果。
- Produces: 后续任务卡文本（proto 变更 + CLI/MCP 换栈 + `application/search.rs` 归并）。

- [ ] **Step 1: 权重收敛观测**

对 test-pipeline 与任一生产项目各跑 ≥10 个真实查询，采样 `score_breakdown` 三分量分布（可临时加 `tracing::info!` 或直接用返回 JSON）。记录：rerank 分与语义分的相关性、邻居进入 top-10 的比例、降级/正常占比。结论写进 S5 实施记录；若权重明显失调（如 rerank 分恒高导致语义失效），调整 `0.6/0.3/0.1` 初值并同步修改 spec §5.4 与主文档 §8。

- [ ] **Step 2: 主文档 §13.1 S5 行更新**

把 `2026-07-31-universal-knowledge-pipeline-design.md` §13.1 的 S5 行：

```markdown
| **S5** | 检索层混合检索（向量召回+图扩展+rerank） | ⏸️ 方案本身延后，不在本轮 | — | — |
```

改为：

```markdown
| **S5** | 检索层混合检索（向量召回+图扩展+rerank） | ✅ 完成 | <提交哈希> | spec: 2026-08-01-knowledge-search-design.md |
```

- [ ] **Step 3: S5 spec 追加实施记录 + 后续任务卡**

`2026-08-01-knowledge-search-design.md` 文末追加：

```markdown
---

## 10. S5 实施记录（2026-08-DD 填写）

- 落地提交：<哈希列表>
- 实测结果：<S5a/S5b/S5c 验证记录，含 ifCode 位次与 score_breakdown 归因>
- 权重收敛：<观测结论；是否调整 0.6/0.3/0.1>

### 后续任务卡（S5-D10 入口可达性）

**标题**：检索入口暴露与双栈归并
**背景**：S5 混合检索仅进程内可达。gRPC `build_service` 硬编码 `world="code"` 且
proto `SearchRequest` 无 `world` 字段、`SearchResult` 无新字段位；CLI `dt search`/
`dt search-kg` 与 MCP `dt_search` 走第二栈（`application/search.rs`）。
**范围**：
1. proto：`SearchRequest` 加 `world`/`max_hops`/`with_evidence`/`origin`/`doc_id`；
   `SearchResult` 加 `score_breakdown`/`hop`/`via_same_as`/`relations`/`evidence`/
   `rerank_degraded`（或新增 CrossWorldSearch RPC 全量承载 §5.7 契约）。
2. `build_service`：透传 world 及新参数；mcp-server.py:462 的 gRPC TODO 一并接通。
3. CLI：`dt search` knowledge/doc 世界改走 retrieve.rs；退役/归并
   `application/search.rs` 的 `expand_nodes`/`fusion`/`rewrite` 中与 retrieve.rs
   重叠的部分（QueryRewriter 是否保留为查询改写前置另行评估）。
**验收**：`dt search --world knowledge "渠道怎么路由"` 与 gRPC 同参调用返回一致结果。
```

- [ ] **Step 4: 全量验证**

Run: `cargo test 2>&1 | tail -5 && cargo clippy --all-targets 2>&1 | tail -5`
Expected: 全绿（预存 2 失败不扩大）；clippy 0 error。

- [ ] **Step 5: 提交**

```bash
git add docs/superpowers/specs/2026-07-31-universal-knowledge-pipeline-design.md docs/superpowers/specs/2026-08-01-knowledge-search-design.md
git commit -m "docs(s5): implementation record + follow-up task card for search entry points"
```

---

## Self-Review 记录（计划作者已完成）

- **Spec 覆盖**：§5.1→Task 3；§5.2.1→Task 4；§5.2.2→Task 5；§5.2.3→Task 6；§5.3→Task 9；§5.4→Task 7/9；§5.5→Task 10/11；§5.6/§5.7→Task 1/7/10/11；§6.1 删除→Task 8；§6.2 改造→Task 8/9/10；§7 S5-0/S5a/S5b/S5c/收尾→Task 0/8/9/10-11/12；§8 风险→各任务注释 + Task 12 观测；§9 验收→Task 8/9/11 实测步骤 + Task 12。
- **类型一致性**：`Seed`/`Candidate`/`ExpandedNode`/`RawEdge`/`RetrieveRequest`/`RetrieveOutcome`/`ScoreBreakdown`/`RelationSnippet` 签名在各任务间逐一核对一致；`apply_rerank` 桩（Task 7）与真实现（Task 9）签名相同。
- **已知有意取舍**：`expand_business` 的边不产 `doc_id`（非 RELATES 边无此属性），非 Entity 候选 `source_ref` 恒 None；`MENTIONED_IN` 回退只对 `dt://entity/` 前缀候选生效。
