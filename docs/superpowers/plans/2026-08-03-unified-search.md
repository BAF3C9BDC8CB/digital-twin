# 统一检索（Unified Search）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 三套割裂检索栈（CLI legacy / gRPC S5 / MCP subprocess）统一为 CrossWorldSearch 单栈单契约，所有入口（CLI/MCP/gRPC）委托，结果自带 LLM 分析+定位信息（最小上下文可读）。

**Architecture:** `search_mcp.rs` 的 CrossWorldSearch 成为唯一检索服务：code（code_methods 向量）、knowledge（retrieve.rs GraphRAG）、doc（doc_chunks）、config（新迁入）、memory（新迁入）、all（三世界 RRF 融合）。CLI 变纯渲染壳（人类格式/JSON），MCP subprocess 传 JSON，gRPC proto 透传全量字段。legacy `application/search.rs` 整体退役。

**Tech Stack:** Rust（tokio/async-trait/serde/clap/tonic）、Memgraph（Cypher）、Qdrant、xinference（bge-m3 + bge-reranker-v2-m3）、Python MCP（subprocess）。

**Spec:** `docs/superpowers/specs/2026-08-03-unified-search-design.md`（U-D1..U-D7 已定稿）

## Global Constraints

- 测试基线：**773 passed / 2 failed 预存**（`infrastructure::parser::ts_java::tests::parses_hello_service`、`interfaces::cli::backup_sqlite::tests::copy_database_writes_file`）——任何任务结束失败数不得增加；`cargo clippy --all-targets` 必须 0 error
- **不得触碰**用户未提交文件：`config/pipeline.yaml`、`test/expected.json`
- live 测试一律 `#[ignore]`，需 Memgraph `bolt://localhost:7688` + Qdrant `:6333/:6334` + xinference `:9997` 在线；离线 `cargo test` 不受影响
- 每任务一次提交：`feat(unified-search): ...` / `refactor(unified-search): ...` / `docs(unified-search): ...`
- TDD：行为变更先写失败测试再实现
- `SearchHit` 无 Default——新增字段后 `rg -n "SearchHit {" src/ tests/` 列出的构造点必须全部补齐

---

### Task 1: 契约扩展 — SearchHit.llm_analysis + search_code 改造

**Files:**
- Modify: `src/application/context/search_mcp.rs`（SearchHit :48-93；search_code :164-298；tests :470-783）
- Modify: `src/application/knowledge/extract/retrieve.rs`（SearchHit 构造点 :963、:1826 及测试内其余构造点）

**Interfaces:**
- Consumes: code_methods payload 已有 `llm_analysis`/`project` 字段（实测确认）
- Produces: `SearchHit.llm_analysis: Option<String>`（serde default）；`search_code` 只查 `CODE_METHODS` 单集合、project 过滤下沉 payload 级

- [x] **Step 1: 写失败测试 — search_code 提取 llm_analysis 且只查单集合**

在 `search_mcp.rs` tests 模块追加（StubVector/StubEmbed 已存在于 :626-692，复用）：

```rust
#[tokio::test]
async fn code_world_extracts_llm_analysis_and_uses_single_collection() {
    let method = serde_json::json!({
        "id": "pt-m1", "score": 0.9,
        "payload": {
            "name": "createApp", "file_path": "test/project/app.js",
            "start_line": 32, "end_line": 36,
            "signature": "function createApp(port)",
            "llm_analysis": "用途：创建服务器实例。\n逻辑：实例化服务器对象。",
            "project": "test-pipeline", "calls": []
        }
    });
    let vector = std::sync::Arc::new(StubVector {
        hits: vec![method],
        captured_filter: std::sync::Mutex::new(None),
    });
    let cws = CrossWorldSearch::new(None, Some(vector), Some(std::sync::Arc::new(StubEmbed)), None);
    let req = SearchRequest {
        query: "createApp".into(), world: Some("code".into()), limit: Some(5),
        project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
    };
    let result = cws.search(&req).await.unwrap();
    assert_eq!(result.hits.len(), 1);
    let hit = &result.hits[0];
    assert_eq!(
        hit.llm_analysis.as_deref(),
        Some("用途：创建服务器实例。\n逻辑：实例化服务器对象。")
    );
    assert_eq!(hit.file_path.as_deref(), Some("test/project/app.js"));
    assert_eq!(hit.start_line, Some(32));
}
```

注意：StubVector 的 `list_collections()` 返回 `Ok(vec![])`——改造后 search_code 不再调 list_collections；若仍走旧逻辑则 hits 为空、测试失败，恰好双重验证。

- [x] **Step 2: 运行测试确认失败**

Run: `cargo test --lib application::context::search_mcp 2>&1 | tail -5`
Expected: FAIL（`llm_analysis` 字段不存在编译错误，或旧逻辑 hits 为空断言失败）

- [x] **Step 3: SearchHit 增加 llm_analysis 字段**

`search_mcp.rs:70`（`signature` 字段后）插入：

```rust
    /// 方法级 LLM 分析（code world，payload 直取；"用途：…\n逻辑：…"）。
    #[serde(default)]
    pub llm_analysis: Option<String>,
```

补齐全部构造点（编译器会逐一点名）：
- `search_mcp.rs`：search_code（Step 4 处理）、search_doc（:379-399）加 `llm_analysis: None`
- `retrieve.rs` :963、:1826 及 `rg -n "SearchHit {" src/ tests/` 列出的其余全部构造点，一律 `llm_analysis: None`
- `search_mcp.rs` 既有测试的 SearchHit 字面量（:476-515、:554-574）同样补字段

- [x] **Step 4: search_code 改造 — 单集合 + payload 提取**

`search_mcp.rs:179-193` 删除 collection 发现逻辑，替换为：

```rust
        // U-D6：只查全局 code_methods；project 过滤下沉 payload 级
        let method_cols = vec![crate::shared::collections::CODE_METHODS.to_string()];
```

`search_mcp.rs:211-214` 的 name 过滤之后增加 project payload 过滤：

```rust
                        if let Some(p) = project {
                            let pp = payload.get("project").and_then(|v| v.as_str()).unwrap_or("");
                            if pp != p {
                                continue;
                            }
                        }
```

同函数 SearchHit 构造（:226-283）中增加：

```rust
                            llm_analysis: payload
                                .get("llm_analysis")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
```

- [x] **Step 5: 运行测试确认通过 + 全量回归**

Run: `cargo test 2>&1 | rg "test result" | tail -3`
Expected: 新测试 PASS；773+N passed / 2 failed（预存不变）

- [x] **Step 6: Commit**

```bash
git add src/application/context/search_mcp.rs src/application/knowledge/extract/retrieve.rs
git commit -m "feat(unified-search): SearchHit.llm_analysis + search_code single-collection with payload-level project filter (U-D6)"
```

---

### Task 2: fusion 迁移 + all 世界 RRF 融合

**Files:**
- Create: `src/application/context/fusion.rs`
- Modify: `src/application/context/mod.rs`（加 `pub mod fusion;`，按字母序在 `domain_query` 后）
- Modify: `src/application/context/search_mcp.rs`（search() dispatch :408-463 重写）

**Interfaces:**
- Consumes: `search.rs:3-53` 的 RankedItem/reciprocal_rank_fusion（**逐字搬移**，Task 3 复用）
- Produces: `fusion::rrf_hits(world_lists: Vec<Vec<SearchHit>>, k: f64, limit: usize) -> Vec<SearchHit>`；world="all" 时 hits 为 RRF 融合序（score=RRF 分），单世界路径保持原生分数

- [x] **Step 1: 写失败测试 — rrf_hits 融合语义**

新建 `src/application/context/fusion.rs`，先只写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::SearchHit;

    fn hit(world: &str, id: &str, score: f64) -> SearchHit {
        SearchHit {
            id: id.into(), title: id.into(), snippet: String::new(),
            source_world: world.into(), entity_type: "X".into(), score,
            source_ref: None, file_path: None, start_line: None, end_line: None,
            signature: None, calls: vec![], element_id: None,
            llm_analysis: None, score_breakdown: None, hop: None,
            via_same_as: None, relations: None, evidence: None, rerank_degraded: None,
        }
    }

    #[test]
    fn rrf_hits_keys_by_world_and_id_and_sets_rrf_score() {
        let code = vec![hit("code", "a", 0.9), hit("code", "b", 0.8)];
        let kn = vec![hit("knowledge", "b", 0.7), hit("knowledge", "c", 0.6)];
        let fused = rrf_hits(vec![code, kn], 60.0, 10);
        // 键为 world:id —— code:b 与 knowledge:b 是不同条目，不去重
        assert_eq!(fused.len(), 4);
        // rank1 的 RRF 分 = 1/(60+1)
        assert!((fused[0].score - 1.0 / 61.0).abs() < 1e-9);
    }

    #[test]
    fn rrf_hits_respects_limit() {
        let l1 = (0..5).map(|i| hit("code", &format!("x{i}"), 0.9)).collect::<Vec<_>>();
        assert_eq!(rrf_hits(vec![l1], 60.0, 3).len(), 3);
    }
}
```

- [x] **Step 2: 运行测试确认失败**

Run: `cargo test --lib application::context::fusion 2>&1 | tail -3`
Expected: FAIL（模块/函数不存在）

- [x] **Step 3: 实现 fusion.rs**

文件主体 = `search.rs:6-52` 的 `RankedItem` + `reciprocal_rank_fusion` **逐字搬移**（去掉外层 `pub mod fusion {}` 包裹，置于文件顶层），头部加模块文档，末尾追加：

```rust
//! 跨世界融合 — RRF（自 application/search.rs 迁入）+ SearchHit 级融合。

use std::collections::HashMap;
use crate::application::context::search_mcp::SearchHit;

// ……RankedItem + reciprocal_rank_fusion 逐字搬移（search.rs:6-52）……

/// SearchHit 级 RRF：以 `{source_world}:{id}` 为键跨列表去重累分，
/// 最终 score 为 RRF 分（量级 1/(k+rank)，仅用于排序与展示）。
pub fn rrf_hits(world_lists: Vec<Vec<SearchHit>>, k: f64, limit: usize) -> Vec<SearchHit> {
    let mut score_map: HashMap<String, (f64, SearchHit)> = HashMap::new();
    for list in &world_lists {
        for (rank, item) in list.iter().enumerate() {
            let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
            let key = format!("{}:{}", item.source_world, item.id);
            score_map
                .entry(key)
                .and_modify(|(score, _)| *score += rrf_score)
                .or_insert_with(|| (rrf_score, item.clone()));
        }
    }
    let mut fused: Vec<_> = score_map.into_values().collect();
    fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    fused
        .into_iter()
        .take(limit)
        .map(|(score, mut item)| {
            item.score = score;
            item
        })
        .collect()
}
```

`context/mod.rs` 插入 `pub mod fusion;`。

- [x] **Step 4: search() dispatch 重写（all→RRF；单世界保持）**

`search_mcp.rs:408-463` 的 `search` 函数体重写为：

```rust
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError> {
        let world = request.world.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(20);
        let project = request.project.as_deref();

        let mut per_world = std::collections::HashMap::new();
        let mut degraded: Vec<String> = Vec::new();

        let all_hits = if world == "all" {
            // U-D2/U-D3：all = code+knowledge+doc，跨世界 RRF
            let mut lists: Vec<Vec<SearchHit>> = Vec::new();

            let code_hits = self
                .search_code(&request.query, project, limit)
                .await
                .unwrap_or_default();
            per_world.insert("code".to_string(), code_hits.len());
            lists.push(code_hits);

            let (kn_hits, dgr) = self.search_knowledge(request).await;
            degraded.extend(dgr);
            per_world.insert("knowledge".to_string(), kn_hits.len());
            lists.push(kn_hits);

            let doc_hits = self
                .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                .await
                .unwrap_or_default();
            per_world.insert("doc".to_string(), doc_hits.len());
            lists.push(doc_hits);

            crate::application::context::fusion::rrf_hits(lists, 60.0, limit)
        } else {
            let mut hits = match world {
                "code" => self
                    .search_code(&request.query, project, limit)
                    .await
                    .unwrap_or_default(),
                "knowledge" => {
                    let (h, dgr) = self.search_knowledge(request).await;
                    degraded.extend(dgr);
                    h
                }
                // "vector" 保留为 "doc" 别名（S5 §8.7）
                "doc" | "vector" => self
                    .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                    .await
                    .unwrap_or_default(),
                // config / memory 分支由 Task 3 / Task 4 加入
                _ => Vec::new(),
            };
            per_world.insert(world.to_string(), hits.len());
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit);
            hits
        };

        let total = all_hits.len();
        Ok(CrossWorldResult {
            query: request.query.clone(),
            world: world.to_string(),
            hits: all_hits,
            total,
            per_world_counts: per_world,
            degraded,
        })
    }
```

既有测试兼容性：`knowledge_world_with_empty_backends_returns_empty_and_no_panic`（:600）断言 `per_world_counts["knowledge"]==Some(0)`——单世界分支仍 insert ✓；doc 两个 stub 测试（:694/:752）走单世界分支，`per_world_counts["doc"]==1` ✓。

- [x] **Step 5: 运行测试确认通过 + 全量回归**

Run: `cargo test 2>&1 | rg "test result" | tail -3`
Expected: 基线不扩大，fusion 2 个新测试 PASS

- [x] **Step 6: Commit**

```bash
git add src/application/context/fusion.rs src/application/context/mod.rs src/application/context/search_mcp.rs
git commit -m "feat(unified-search): fusion.rs migration + all-world RRF over SearchHit (U-D2/U-D3)"
```

---

### Task 3: config 世界 — search_config + QueryRewriter 迁入

**Files:**
- Create: `src/application/context/search_config.rs`
- Modify: `src/shared/collections.rs`（加 CONFIG_CHUNKS 常量）
- Modify: `src/application/context/mod.rs`（加 `pub mod search_config;`）
- Modify: `src/application/context/search_mcp.rs`（dispatch 加 config 分支 + `graph_ref()` 辅助方法）
- 源位置 `cli/build.rs:819-1100` 由 Task 5 删除（本任务不动）

**Interfaces:**
- Consumes: `fusion::{RankedItem, reciprocal_rank_fusion}`（Task 2）；`build.rs:750-765` `extract_ascii_words`（**逐字搬移**）；`search.rs:253-325` QueryRewriter（**逐字搬移**为私有模块）
- Produces: `impl CrossWorldSearch { pub(crate) async fn search_config(&self, query: &str, project: Option<&str>, limit: usize) -> (Vec<SearchHit>, Vec<String>) }`；degraded 取值 `"embed_unavailable"` / `"graph_unavailable"`；`pub(crate) fn graph_ref(&self) -> &Option<Arc<dyn GraphRepository>>`（Task 4 复用）

- [x] **Step 1: collections.rs 加常量 + search_mcp 加 graph_ref**

`collections.rs:14` 后：

```rust
/// Global collection for config chunk vectors (dt sync --config-chunks 写入).
pub const CONFIG_CHUNKS: &str = "config_chunks";
```

`search_mcp.rs` 的 `impl CrossWorldSearch`（:132-157 区域内）加：

```rust
    /// 供同 crate 扩展方法（search_config/search_memory）访问图后端。
    pub(crate) fn graph_ref(&self) -> &Option<Arc<dyn GraphRepository>> {
        &self.graph
    }
```

- [x] **Step 2: 写失败测试**

新建 `search_config.rs`，`#[cfg(test)]`（stub 从 search_mcp.rs tests :626-692 复制精简版，测试不跨文件共享 stub）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct StubVector { hits: Vec<serde_json::Value> }
    #[async_trait::async_trait]
    impl VectorRepository for StubVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> { Ok(()) }
        async fn search(&self, _c: &str, _v: Vec<f32>, _l: u64)
            -> Result<Vec<serde_json::Value>, DtError> { Ok(self.hits.clone()) }
        async fn search_with_filter(&self, _c: &str, _v: Vec<f32>, _l: u64, _f: serde_json::Value)
            -> Result<Vec<serde_json::Value>, DtError> { Ok(self.hits.clone()) }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> { Ok(()) }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> { Ok(()) }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> { Ok(vec![]) }
        async fn collection_info(&self, name: &str)
            -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.into(), points_count: 0, vector_dim: 0, model_version: String::new() })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> { Ok(()) }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy) }
    }
    struct StubEmbed;
    #[async_trait::async_trait]
    impl EmbedService for StubEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect()) }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy) }
    }

    #[tokio::test]
    async fn config_world_maps_config_chunks_payload() {
        let chunk = serde_json::json!({
            "id": "pt-c1", "score": 0.9,
            "payload": { "section_name": "spring", "data_id": "app.yaml",
                         "text": "redis:\n  host: 127.0.0.1", "key_count": 3 }
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(Arc::new(StubVector { hits: vec![chunk] })),
            Some(Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "redis 配置".into(), world: Some("config".into()), limit: Some(5),
            project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.per_world_counts.get("config"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.entity_type, "ConfigChunk");
        assert!(hit.title.contains("app.yaml"));
        assert!(hit.snippet.contains("redis"));
    }

    #[tokio::test]
    async fn config_world_empty_backends_degrades_gracefully() {
        let cws = CrossWorldSearch::empty();
        let req = SearchRequest {
            query: "q".into(), world: Some("config".into()), limit: Some(5),
            project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 0);
        assert!(result.degraded.contains(&"embed_unavailable".to_string())
            || result.degraded.contains(&"graph_unavailable".to_string()));
    }

    #[test]
    fn query_rewriter_expands_chinese_terms() {
        let rw = rewrite::QueryRewriter::with_defaults();
        let out = rw.rewrite("数据库配置");
        assert!(out.iter().any(|s| s.contains("database")));
        assert_eq!(out[0], "数据库配置");
    }
}
```

- [x] **Step 3: 运行测试确认失败**

Run: `cargo test --lib application::context::search_config 2>&1 | tail -3`
Expected: FAIL（模块不存在）

- [x] **Step 4: 实现 search_config.rs**

文件结构：

```rust
//! config 世界 — 自 cli/build.rs:819-1100 迁入。
//! 多查询变体 embed → config_chunks+doc_chunks 向量 → RRF → ASCII 关键词过滤；
//! 向量不可用/无结果 → Cypher 关键词回退（ConfigKey/Server/Database/NacosConfig/NacosService）。

use std::collections::HashMap;
use std::sync::Arc;

use crate::application::context::fusion::{reciprocal_rank_fusion, RankedItem};
use crate::application::context::search_mcp::{CrossWorldSearch, SearchHit, SearchRequest};
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::shared::collections::{CONFIG_CHUNKS, DOC_CHUNKS};

/// 逐字搬移自 cli/build.rs:750-765（原样复制函数体）。
pub(crate) fn extract_ascii_words(text: &str) -> Vec<String> {
    // ……搬移原实现……
}

/// 逐字搬移自 application/search.rs:253-325（rewrite 仅 config 世界使用）。
pub(crate) mod rewrite {
    // ……QueryRewriter 原样（use std::collections::HashMap 在模块内）……
}
```

`search_config` 函数体（**行为与 build.rs:819-1100 一致**，println 全部移除、改为返回数据）：

1. **多查询变体**（build.rs:824-837）：`qs = [query] + extract_ascii_words(query) 去重 + "{query} config"（query 不含 "config" 时）`，`qs.truncate(3)`
2. vector 或 embed 缺失，或 embed_batch 失败/空 → `degraded.push("embed_unavailable".into())`，跳第 5 步
3. 对 `CONFIG_CHUNKS`、`DOC_CHUNKS` × 每个 query 向量 `vector.search(col, qvec, (limit*2) as u64)`，映射 RankedItem（**逐字沿用 build.rs:854-941**）：
   - config_chunks → `entity_type: "ConfigChunk"`，title `format!("[{}:{}] ({} keys)", data_id, section, key_count)`，snippet=text
   - doc_chunks → 仅收 `doc_id` 含 `#section-` 的点 → `entity_type: "Config"`，title=section 名，snippet=`format!("{full_text}\n{display_line}")`（display_line = `"{file_path}  {首行80字符}"`）
4. `reciprocal_rank_fusion(rank_lists, 60.0, limit)` → ASCII 关键词过滤（build.rs:951-964：query 中 ≥3 字符 ascii 词小写化，title+snippet 小写含任一才保留）→ 按 title 去重 → 映射 SearchHit：`source_world: "config"`，`source_ref: None`，其余 None/空（含 `llm_analysis: None`）；非空则返回 `(hits, degraded)`
5. **Cypher 回退**（build.rs:1010-1099）：graph 缺失 → `degraded.push("graph_unavailable".into())` 返回空。keywords = `extract_ascii_words(query)` + QueryRewriter 展开（≥3 字符、去重、truncate(5)，同 build.rs:790-813）；orig_ascii 非空则 must_have 只用 orig_ascii（build.rs:1028-1050）；Cypher **逐字沿用 build.rs:1056-1066**（含 `project_filter` 与 `display_limit = if limit > 200 { limit } else { limit.max(50) }`）；行映射 SearchHit：`entity_type=type, title=name, snippet=snippet, source_ref=Some(source), source_world: "config", score: 0.0`，按 name 去重

- [x] **Step 5: dispatch 加 config 分支**

`search_mcp.rs` Task 2 重写的 match 中，`"doc" | "vector"` 分支后插入：

```rust
                "config" => {
                    let (h, dgr) = self.search_config(&request.query, project, limit).await;
                    degraded.extend(dgr);
                    h
                }
```

`context/mod.rs` 加 `pub mod search_config;`。

- [x] **Step 6: 运行测试确认通过 + 全量回归 + clippy**

Run: `cargo test 2>&1 | rg "test result" | tail -3 && cargo clippy --all-targets 2>&1 | rg "^error" | head -3`
Expected: 测试 PASS（基线不扩大）；clippy 无 error 输出

- [x] **Step 7: Commit**

```bash
git add src/application/context/search_config.rs src/application/context/mod.rs src/application/context/search_mcp.rs src/shared/collections.rs
git commit -m "feat(unified-search): config world migrated into CrossWorldSearch (search_config + QueryRewriter + CONFIG_CHUNKS)"
```

---

### Task 4: memory 世界 — search_memory

**Files:**
- Create: `src/application/context/search_memory.rs`
- Modify: `src/application/context/mod.rs`（加 `pub mod search_memory;`）
- Modify: `src/application/context/search_mcp.rs`（dispatch 加 memory 分支）

**Interfaces:**
- Consumes: `CrossWorldSearch::graph_ref()`（Task 3）、`GraphRepository::read_query`
- Produces: `impl CrossWorldSearch { pub(crate) async fn search_memory(&self, query: &str, limit: usize) -> Vec<SearchHit> }`——graph 缺失返回空

- [x] **Step 1: 写失败测试**

`search_memory.rs` `#[cfg(test)]`（MockGraph 捕获查询 + 返回固定行，模式同 search.rs:168-211，复制精简版并实现 GraphRepository 全部方法——read_query/write_query/health_check）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::{CrossWorldSearch, SearchRequest};
    use std::sync::Arc;

    struct MockGraph { captured_query: std::sync::Mutex<String> }
    impl MockGraph {
        fn new() -> Self { Self { captured_query: std::sync::Mutex::new(String::new()) } }
    }
    #[async_trait::async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(&self, query: &str, _params: HashMap<String, serde_json::Value>)
            -> Result<serde_json::Value, crate::domain::error::DtError> {
            *self.captured_query.lock().unwrap() = query.to_string();
            Ok(serde_json::json!([{
                "type": "Decision", "name": "s5-d6-clamp",
                "desc": "rerank 分数 clamp 归一", "eid": "4:0:99"
            }]))
        }
        async fn write_query(&self, _q: &str, _p: HashMap<String, serde_json::Value>)
            -> Result<serde_json::Value, crate::domain::error::DtError> { Ok(serde_json::json!([])) }
        async fn health_check(&self)
            -> Result<crate::domain::types::HealthStatus, crate::domain::error::DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy) }
    }

    #[tokio::test]
    async fn memory_world_queries_event_labels_and_maps_rows() {
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "S5".into(), world: Some("memory".into()), limit: Some(5),
            project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.per_world_counts.get("memory"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.entity_type, "Decision");
        assert_eq!(hit.title, "s5-d6-clamp");
        assert_eq!(hit.source_world, "memory");
        assert_eq!(hit.element_id.as_deref(), Some("4:0:99"));
        let captured = graph.captured_query.lock().unwrap().clone();
        assert!(captured.contains("n:Modification"));
        assert!(captured.contains("n:Decision"));
        assert!(captured.contains("elementId(n) AS eid"));
    }
}
```

- [x] **Step 2: 运行测试确认失败**

Run: `cargo test --lib application::context::search_memory 2>&1 | tail -3`
Expected: FAIL（模块不存在）

- [x] **Step 3: 实现 search_memory.rs**

```rust
//! memory 世界 — 事件节点关键词检索（自 cli/build.rs:1509-1516 迁入，补 elementId 定位）。

use std::collections::HashMap;

use crate::application::context::search_mcp::{CrossWorldSearch, SearchHit};

impl CrossWorldSearch {
    pub(crate) async fn search_memory(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let Some(ref graph) = self.graph_ref() else {
            return Vec::new();
        };
        let cypher = format!(
            "MATCH (n) WHERE (n:Modification OR n:Deployment OR n:ConfigChange \
             OR n:BugFix OR n:Decision OR n:Conversation OR n:Session) \
             AND (n.details CONTAINS $q OR coalesce(n.summary, '') CONTAINS $q) \
             RETURN labels(n)[0] AS type, coalesce(n.name, n.entity_id, n.session_id, '') AS name, \
                    coalesce(n.details, n.summary, '') AS desc, elementId(n) AS eid \
             LIMIT {limit}"
        );
        let mut params = HashMap::new();
        params.insert("q".into(), serde_json::Value::String(query.to_string()));
        let Ok(result) = graph.read_query(&cypher, params).await else {
            return Vec::new();
        };
        result
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| SearchHit {
                        id: row.get("eid").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        title: row.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        snippet: row.get("desc").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        source_world: "memory".into(),
                        entity_type: row.get("type").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                        score: 0.0,
                        source_ref: None,
                        file_path: None,
                        start_line: None,
                        end_line: None,
                        signature: None,
                        calls: vec![],
                        element_id: row.get("eid").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        llm_analysis: None,
                        score_breakdown: None,
                        hop: None,
                        via_same_as: None,
                        relations: None,
                        evidence: None,
                        rerank_degraded: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
```

- [x] **Step 4: dispatch 加 memory 分支**

config 分支后插入：

```rust
                "memory" => self.search_memory(&request.query, limit).await,
```

`context/mod.rs` 加 `pub mod search_memory;`。

- [x] **Step 5: 运行测试确认通过 + 全量回归**

Run: `cargo test 2>&1 | rg "test result" | tail -3`
Expected: 基线不扩大，新测试 PASS

- [x] **Step 6: Commit**

```bash
git add src/application/context/search_memory.rs src/application/context/mod.rs src/application/context/search_mcp.rs
git commit -m "feat(unified-search): memory world (event labels) migrated into CrossWorldSearch"
```

---

### Task 5: CLI 渲染壳 — search_render + handle_search 重写 + main.rs

**Files:**
- Create: `src/interfaces/cli/search_render.rs`
- Modify: `src/interfaces/cli/mod.rs`（加 `pub mod search_render;`——先读该文件确认现有 mod 列表与插入位置）
- Modify: `src/interfaces/cli/build.rs`（handle_search 整体重写替换 :767-1561；create_search_embed_client :234-290 重构提取 provider 配置；删除 `extract_ascii_words` 定义——已迁 search_config.rs）
- Modify: `src/main.rs`（Search 命令 :243-262、dispatch :1575-1586）

**Interfaces:**
- Consumes: `CrossWorldSearch::new/search`（Task 1-4）
- Produces:
  - `pub fn render_human(result: &CrossWorldResult) -> String`
  - `pub fn render_json(result: &CrossWorldResult) -> String`
  - `pub async fn handle_search(query: String, world: String, limit: usize, json: bool, project: Option<String>, graph: Option<Arc<dyn GraphRepository>>, vector: Option<Arc<dyn VectorRepository>>) -> anyhow::Result<()>` —— **path 参数删除**（现行 path 仅打印不参与检索；spec §10 收尾补记此溢出修正）

- [x] **Step 1: 写失败测试 — 三行制渲染 + JSON**

`search_render.rs` `#[cfg(test)]`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::{CrossWorldResult, SearchHit};

    fn base_hit() -> SearchHit {
        SearchHit {
            id: "1".into(), title: "createApp".into(), snippet: String::new(),
            source_world: "code".into(), entity_type: "Method".into(), score: 0.9412,
            source_ref: None, file_path: Some("test/project/app.js".into()),
            start_line: Some(32), end_line: Some(36),
            signature: Some("function createApp(port)".into()), calls: vec![],
            element_id: None,
            llm_analysis: Some("用途：创建服务器实例。\n逻辑：实例化服务器对象。".into()),
            score_breakdown: None, hop: None, via_same_as: None,
            relations: None, evidence: None, rerank_degraded: None,
        }
    }

    fn result_with(hits: Vec<SearchHit>, degraded: Vec<String>) -> CrossWorldResult {
        CrossWorldResult {
            query: "q".into(), world: "all".into(),
            total: hits.len(), hits,
            per_world_counts: std::collections::HashMap::new(),
            degraded,
        }
    }

    #[test]
    fn human_render_method_three_lines() {
        let out = render_human(&result_with(vec![base_hit()], vec![]));
        assert!(out.contains("[0.9412] [Method] createApp"));
        assert!(out.contains("分析: 用途：创建服务器实例。"));
        assert!(out.contains("位置: test/project/app.js:L32-36"));
        assert!(out.contains("signature: function createApp(port)"));
    }

    #[test]
    fn human_render_entity_shows_summary_source_and_hop() {
        let mut h = base_hit();
        h.entity_type = "Entity".into();
        h.source_world = "knowledge".into();
        h.title = "ifCode".into();
        h.snippet = "支付渠道编码，决定路由".into();
        h.llm_analysis = None;
        h.file_path = None; h.start_line = None; h.end_line = None; h.signature = None;
        h.source_ref = Some("dt://doc/支付架构决策.md".into());
        h.hop = Some(0);
        let out = render_human(&result_with(vec![h], vec![]));
        assert!(out.contains("摘要: 支付渠道编码，决定路由"));
        assert!(out.contains("来源: dt://doc/支付架构决策.md"));
        assert!(out.contains("[hop=0]"));
    }

    #[test]
    fn human_render_degraded_footer() {
        let out = render_human(&result_with(vec![base_hit()], vec!["rerank_unavailable".into()]));
        assert!(out.contains("降级") && out.contains("rerank_unavailable"));
    }

    #[test]
    fn json_render_is_pure_parseable_json() {
        let out = render_json(&result_with(vec![base_hit()], vec![]));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hits"][0]["llm_analysis"], "用途：创建服务器实例。\n逻辑：实例化服务器对象。");
        assert_eq!(v["hits"][0]["start_line"], 32);
    }
}
```

- [x] **Step 2: 运行测试确认失败**

Run: `cargo test --lib interfaces::cli::search_render 2>&1 | tail -3`
Expected: FAIL（模块不存在）

- [x] **Step 3: 实现 search_render.rs**

```rust
//! 检索结果渲染 — 人类格式（类型感知三行制）与 JSON（MCP 消费）。

use crate::application::context::search_mcp::{CrossWorldResult, SearchHit};

const MAX_CHARS: usize = 200;

fn truncate(s: &str) -> String {
    if s.chars().count() > MAX_CHARS {
        format!("{}…", s.chars().take(MAX_CHARS).collect::<String>())
    } else {
        s.to_string()
    }
}

fn render_hit(h: &SearchHit) -> String {
    let mut out = format!("[{:.4}] [{}] {}\n", h.score, h.entity_type, h.title);
    let (label, body) = match h.entity_type.as_str() {
        "Method" => ("分析", h.llm_analysis.clone().unwrap_or_else(|| h.snippet.clone())),
        "Doc" => ("原文", h.snippet.clone()),
        _ => ("摘要", h.snippet.clone()),
    };
    if !body.is_empty() {
        let one_line = body.lines().collect::<Vec<_>>().join("；");
        out.push_str(&format!("  {label}: {}\n", truncate(&one_line)));
    }
    let mut loc = String::new();
    if let Some(fp) = &h.file_path {
        loc = match (h.start_line, h.end_line) {
            (Some(s), Some(e)) => format!("位置: {fp}:L{s}-{e}"),
            _ => format!("位置: {fp}"),
        };
        if let Some(sig) = &h.signature {
            loc.push_str(&format!("  signature: {sig}"));
        }
    } else if let Some(sr) = &h.source_ref {
        loc = format!("来源: {sr}");
        if let Some(hop) = h.hop {
            loc.push_str(&format!("  [hop={hop}]"));
        }
    }
    if !loc.is_empty() {
        out.push_str(&format!("  {loc}\n"));
    }
    out
}

pub fn render_human(result: &CrossWorldResult) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("  (no results)\n");
    }
    for h in &result.hits {
        out.push_str(&render_hit(h));
    }
    if !result.degraded.is_empty() {
        out.push_str(&format!("  ⚠️ 降级: {}\n", result.degraded.join(", ")));
    }
    out
}

pub fn render_json(result: &CrossWorldResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".into())
}
```

`interfaces/cli/mod.rs` 加 `pub mod search_render;`。

- [x] **Step 4: create_search_embed_client 重构提取 provider 配置**

`build.rs:234-290`：将 ProviderConfig 组装逻辑提取为 `fn provider_config_from_pipeline() -> ProviderConfig`（含原有的 pipeline.yaml 加载失败回退 `ProviderConfig::default_siliconflow()` 逻辑），然后：

```rust
fn create_search_embed_client() -> Arc<dyn EmbedService> {
    use crate::infrastructure::embedder::create_embed_router;
    create_embed_router(provider_config_from_pipeline())
}

fn create_search_rerank_client() -> Arc<dyn crate::domain::traits::RerankService> {
    use crate::infrastructure::embedder::create_rerank_router;
    create_rerank_router(provider_config_from_pipeline())
}
```

- [x] **Step 5: handle_search 整体重写**

`build.rs:767-1561` 全部替换为：

```rust
/// Handle `dt search` — 统一检索渲染壳（U-D3：默认 world=all；--json 输出纯 JSON）。
pub async fn handle_search(
    query: String,
    world: String,
    limit: usize,
    json: bool,
    project: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> anyhow::Result<()> {
    tracing::info!("搜索: query={query} world={world} limit={limit} json={json} project={project:?}");

    if !json {
        println!("Search: query=\"{query}\" world={world} limit={limit}");
    }

    let embed: Option<Arc<dyn EmbedService>> = Some(create_search_embed_client());
    let rerank = Some(create_search_rerank_client());
    let cws = crate::application::context::search_mcp::CrossWorldSearch::new(
        graph, vector, embed, rerank,
    );
    let req = crate::application::context::search_mcp::SearchRequest {
        query: query.clone(),
        world: Some(world),
        limit: Some(limit),
        project,
        max_hops: None,
        with_evidence: None,
        origin: None,
        doc_id: None,
    };
    let result = cws.search(&req).await?;

    if json {
        // U-D4：--json 时 stdout 仅含 JSON（header 行已抑制，日志走 stderr）
        println!("{}", crate::interfaces::cli::search_render::render_json(&result));
    } else {
        print!("{}", crate::interfaces::cli::search_render::render_human(&result));
    }
    Ok(())
}
```

同时删除 `build.rs` 中已无引用的 `extract_ascii_words` 定义（:750-765）与 `get_keywords` 闭包、config 段、Cypher 段等全部死代码；`rg -n "extract_ascii_words|reciprocal_rank_fusion|RankedItem" src/interfaces/cli/build.rs` 确认清零。

- [x] **Step 6: main.rs Search 命令与 dispatch**

命令定义（:243-262）替换为：

```rust
    /// Unified search across worlds.
    ///
    /// Usage: dt search <query> [--world all|code|knowledge|doc|config|memory] [--limit 10] [--json]
    Search {
        /// Search query string (positional).
        query: String,

        /// Which world to search: all, code, knowledge, doc, config, memory.
        #[arg(long = "world", default_value = "all")]
        world: String,

        /// Limit results.
        #[arg(long = "limit", default_value = "10")]
        limit: usize,

        /// Output pure JSON to stdout (for MCP / scripting).
        #[arg(long = "json")]
        json: bool,

        /// Scope to a project name.
        #[arg(long = "project", short = 'p')]
        project: Option<String>,
    },
```

dispatch（:1575-1586）替换为：

```rust
        Some(Commands::Search {
            query,
            world,
            limit,
            json,
            project,
        }) => {
            let graph = connect_graph().await;
            let vector = connect_vector().await;
            dt_daemon::interfaces::cli::build::handle_search(
                query, world, limit, json, project, graph, vector,
            )
            .await?;
            return Ok(());
        }
```

- [x] **Step 7: 运行测试 + 构建 + 人工冒烟**

Run: `cargo test 2>&1 | rg "test result" | tail -3 && cargo build 2>&1 | tail -2`
Expected: 测试基线不扩大；构建成功
冒烟（需服务在线）：`./target/debug/dt search "createApp" --world code` 应输出三行制 Method 结果；`./target/debug/dt search "createApp" --json | python3 -m json.tool` 应为合法 JSON

- [x] **Step 8: Commit**

```bash
git add src/interfaces/cli/search_render.rs src/interfaces/cli/mod.rs src/interfaces/cli/build.rs src/main.rs
git commit -m "feat(unified-search): CLI render shell — type-aware 3-line format, --json pure stdout, default world=all"
```

---

### Task 6: search-kg 完全移除（U-D5）

**Files:**
- Modify: `src/main.rs`（删除 SearchKg 变体 :264-274、dispatch 分支 :1588-1596）
- Modify: `src/interfaces/cli/build.rs`（删除 handle_search_kg :1583-1904 与死代码 print_config_chunk_results :1564 附近）

**Interfaces:**
- Consumes: Task 5 的 handle_search
- Produces: `dt search-kg` 命令不存在——clap 原生报 `unrecognized subcommand`（exit=2）；无 stub、无引导

- [x] **Step 1: main.rs 整体删除 SearchKg**

删除 `SearchKg` 命令变体（:264-274 整个，含 doc comment）与对应 dispatch 分支（:1588-1596 整个 `Some(Commands::SearchKg { .. })` 臂）。**不留 hidden stub、不留任何兼容代码。**

- [x] **Step 2: build.rs 删除 handle_search_kg 与死代码**

删除 `handle_search_kg` 全函数（:1583-1904）；删除 `print_config_chunk_results`（:1564 附近，已无调用方）；`rg -n "handle_search_kg|print_config_chunk_results|SearchKg" src/` 确认零引用。

- [x] **Step 3: 构建 + 行为验证**

Run: `cargo build 2>&1 | tail -2 && ./target/debug/dt search-kg "foo" 2>&1; echo "exit=$?"; ./target/debug/dt search --help 2>&1 | rg -c "search-kg" || true`
Expected: 构建成功；stderr 含 `unrecognized subcommand` 且 `exit=2`；help 中 0 次出现 search-kg

- [x] **Step 4: 全量回归 + Commit**

Run: `cargo test 2>&1 | rg "test result" | tail -3 && cargo clippy --all-targets 2>&1 | rg "^error" | head -3`
Expected: 基线不扩大；clippy 0 error

```bash
git add src/main.rs src/interfaces/cli/build.rs
git commit -m "refactor(unified-search): remove dt search-kg entirely (U-D5, no compat shim)"
```

---

### Task 7: MCP 工具修复 — JSON 透传 + dt_search_expand 删除

**Files:**
- Modify: `mcp/mcp-server.py`（工具注册 :266-302；执行分发 :731-756；头部工具列表注释 :92）

**Interfaces:**
- Consumes: `dt search <query> --world W --json`（Task 5/6，query 为位置参数——修复 `--query` bug）
- Produces: `dt_search`/`dt_search_kg` 输出为 CLI 透传的 JSON 文本；`dt_search_expand` 从注册与分发中移除

- [x] **Step 1: 执行分发改写（:731-756）**

```python
    # ===== 搜索 =====
    if name == "dt_search_kg":
        query = arguments.get("query", "")
        limit = arguments.get("limit", 10)
        text = run_cmd([DT_BIN, "search", query, "--world", "knowledge",
                        "--limit", str(limit), "--json"])

    elif name == "dt_search":
        query = arguments.get("query", "")
        world = arguments.get("world", "all")
        limit = arguments.get("limit", 10)
        project = arguments.get("project", "")
        cmd = [DT_BIN, "search", query, "--world", world,
               "--limit", str(limit), "--json"]
        if project:
            cmd += ["--project", project]
        text = run_cmd(cmd)
```

（`dt_search_expand` 分支整段删除；`--path` 传参随 CLI 删除一并移除）

- [x] **Step 2: 注册块改写（:266-302）**

- 删除 `dt_search_expand` 的 Tool 注册（:277-289）
- `dt_search_kg` 描述改为 `"搜索知识图谱（GraphRAG 混合检索：向量召回+图扩展+rerank），返回 JSON（含 summary/来源/hop）"`
- `dt_search` 描述改为 `"统一检索（world: all|code|knowledge|doc|config|memory，默认 all），返回 JSON（Method 含 llm_analysis/file_path/start_line/end_line）"`，inputSchema 的 `world` default 改 `"all"`，新增可选 `project` 属性
- 文件头工具列表注释（:92）移除 `dt_search_expand`

- [x] **Step 3: 验证**

Run: `python3 -c "import ast; ast.parse(open('mcp/mcp-server.py').read()); print('syntax ok')" && rg -c "dt_search_expand" mcp/mcp-server.py || true`
Expected: syntax ok；`dt_search_expand` 0 次出现
人工冒烟（服务在线时）：`./target/debug/dt search "ifCode" --world knowledge --json | python3 -m json.tool | head -20`

- [x] **Step 4: Commit**

```bash
git add mcp/mcp-server.py
git commit -m "fix(unified-search): MCP tools positional query + --json passthrough; drop dt_search_expand (U-D7)"
```

---

### Task 8: gRPC proto 全量字段 + 透传 + deprecated 删除

**Files:**
- Modify: `proto/dt_core.proto`（SearchRequest :45-51、SearchResult :53-59）
- Modify: `src/interfaces/grpc/services/build_service.rs`（handle_search :81-179；删除 search_via_vector :186-292 与 search_via_graph :296-350）

**Interfaces:**
- Consumes: `CrossWorldSearch`（Task 1-4 全世界）；tonic-build 随 `cargo build` 自动再生成（build.rs:21）
- Produces: proto `SearchRequest{world,max_hops,with_evidence,origin,doc_id}`；`SearchResult{entity_type,snippet,llm_analysis,end_line,hop,rerank_degraded,evidence,score_breakdown,relations}`；`fn hit_to_proto(hit: SearchHit) -> SearchResult`（纯函数，可单测）

- [x] **Step 1: 写失败测试 — hit_to_proto 映射**

`build_service.rs` `#[cfg(test)]`（若无 tests 模块则新建）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::SearchHit;
    use crate::application::knowledge::extract::retrieve::{RelationSnippet, ScoreBreakdown};

    #[test]
    fn hit_to_proto_maps_all_new_fields() {
        let hit = SearchHit {
            id: "1".into(), title: "ifCode".into(), snippet: "支付渠道编码".into(),
            source_world: "knowledge".into(), entity_type: "Entity".into(), score: 0.94,
            source_ref: Some("dt://doc/pay.md".into()),
            file_path: None, start_line: None, end_line: None,
            signature: None, calls: vec![],
            element_id: Some("4:0:1".into()),
            llm_analysis: None,
            score_breakdown: Some(ScoreBreakdown {
                semantic: 0.71, rerank: 0.92, graph_boost: 1.0, final_score: 0.83,
            }),
            hop: Some(1),
            via_same_as: None,
            relations: Some(vec![RelationSnippet {
                rel_type: "relates".into(), other_end_id: "dt://entity/p/Config/waycode".into(),
                other_end_name: "wayCode".into(), direction: "out".into(),
                confidence: 0.9, evidence: None, supplementary_count: 0,
            }]),
            evidence: Some(vec!["证据段A".into()]),
            rerank_degraded: Some(false),
        };
        let p = hit_to_proto(hit);
        assert_eq!(p.entity_type, "Entity");
        assert_eq!(p.snippet, "支付渠道编码");
        assert_eq!(p.hop, 1);
        assert_eq!(p.evidence, vec!["证据段A".to_string()]);
        let sb = p.score_breakdown.expect("score_breakdown");
        assert!((sb.final_score - 0.83).abs() < 1e-6);
        assert_eq!(p.relations.len(), 1);
        assert_eq!(p.relations[0].other_end_name, "wayCode");
    }
}
```

- [x] **Step 2: 运行测试确认失败**

Run: `cargo test --lib interfaces::grpc::services::build_service 2>&1 | tail -3`
Expected: FAIL（hit_to_proto / proto 字段不存在）

- [x] **Step 3: proto 变更**

`SearchRequest` 与 `SearchResult` 替换为（`expand`/`path` 保留字段位但标注 ignored，避免破坏 wire 兼容）：

```proto
message SearchRequest {
  string query = 1;
  int32 limit = 2;
  bool expand = 3;   // ignored (legacy)
  string path = 4;   // ignored (legacy)
  string project = 5;
  string world = 6;        // all|code|knowledge|doc|config|memory；空=all
  uint32 max_hops = 7;     // knowledge 世界图扩展跳数；0=默认1
  bool with_evidence = 8;  // knowledge top-5 证据回填
  string origin = 9;       // knowledge 召回 origin 过滤
  string doc_id = 10;      // doc 世界单文档限定
}

message ScoreBreakdown {
  double semantic = 1;
  double rerank = 2;
  double graph_boost = 3;
  double final_score = 4;
}

message RelationSnippet {
  string rel_type = 1;
  string other_end_id = 2;
  string other_end_name = 3;
  string direction = 4;
  double confidence = 5;
  string evidence = 6;
  int32 supplementary_count = 7;
}

message SearchResult {
  float score = 1;
  string name = 2;
  string file_path = 3;
  int32 start_line = 4;
  string signature = 5;
  string entity_type = 6;
  string snippet = 7;
  string llm_analysis = 8;
  int32 end_line = 9;
  uint32 hop = 10;
  bool rerank_degraded = 11;
  repeated string evidence = 12;
  ScoreBreakdown score_breakdown = 13;
  repeated RelationSnippet relations = 14;
}
```

- [x] **Step 4: build_service 透传 + hit_to_proto**

handle_search 中 `cws_req` 构造改为：

```rust
    let cws_req = crate::application::context::search_mcp::SearchRequest {
        query: req.query,
        world: if req.world.is_empty() { None } else { Some(req.world) },
        limit: Some(limit),
        project: if req.project.is_empty() { None } else { Some(req.project) },
        max_hops: if req.max_hops == 0 { None } else { Some(req.max_hops) },
        with_evidence: Some(req.with_evidence),
        origin: if req.origin.is_empty() { None } else { Some(req.origin) },
        doc_id: if req.doc_id.is_empty() { None } else { Some(req.doc_id) },
    };
```

新增纯函数并替换 results 映射（:159-169）：

```rust
fn hit_to_proto(hit: crate::application::context::search_mcp::SearchHit) -> SearchResult {
    SearchResult {
        score: hit.score as f32,
        name: hit.title,
        file_path: hit.file_path.unwrap_or_default(),
        start_line: hit.start_line.map(|l| l as i32).unwrap_or(0),
        signature: hit.signature.unwrap_or_default(),
        entity_type: hit.entity_type,
        snippet: hit.snippet,
        llm_analysis: hit.llm_analysis.unwrap_or_default(),
        end_line: hit.end_line.map(|l| l as i32).unwrap_or(0),
        hop: hit.hop.unwrap_or(0),
        rerank_degraded: hit.rerank_degraded.unwrap_or(false),
        evidence: hit.evidence.unwrap_or_default(),
        score_breakdown: hit.score_breakdown.map(|sb| ScoreBreakdown {
            semantic: sb.semantic,
            rerank: sb.rerank,
            graph_boost: sb.graph_boost,
            final_score: sb.final_score,
        }),
        relations: hit
            .relations
            .unwrap_or_default()
            .into_iter()
            .map(|r| RelationSnippet {
                rel_type: r.rel_type,
                other_end_id: r.other_end_id,
                other_end_name: r.other_end_name,
                direction: r.direction,
                confidence: r.confidence,
                evidence: r.evidence.unwrap_or_default(),
                supplementary_count: r.supplementary_count as i32,
            })
            .collect(),
    }
}
```

（`use` 相应 proto 类型名若与生成的 `ScoreBreakdown`/`RelationSnippet` 冲突，用 `crate::interfaces::grpc::pb::` 前缀或重命名导入区分 retrieve.rs 同名结构——以编译器提示为准统一加前缀。）

同时**删除** `search_via_vector`（:186-292）与 `search_via_graph`（:296-350）两个 deprecated 函数；`rg -n "search_via_vector|search_via_graph" src/` 确认零引用。

- [x] **Step 5: 构建（tonic 再生成）+ 测试 + clippy**

Run: `cargo build 2>&1 | tail -2 && cargo test 2>&1 | rg "test result" | tail -3 && cargo clippy --all-targets 2>&1 | rg "^error" | head -3`
Expected: proto 编译通过；hit_to_proto 测试 PASS；基线不扩大；clippy 0 error

- [x] **Step 6: Commit**

```bash
git add proto/dt_core.proto src/interfaces/grpc/services/build_service.rs
git commit -m "feat(unified-search): gRPC Search world passthrough + full SearchHit fields in proto; drop deprecated search fns"
```

---

### Task 9: 退役清理 — search.rs / CollectionKind / 全量验证

**Files:**
- Delete: `src/application/search.rs`
- Modify: `src/application/mod.rs`（移除 `pub mod search;` :7）
- Modify: `src/infrastructure/qdrant/collection.rs`（CollectionKind 命名体系——**先确认引用再删**）

**Interfaces:**
- Consumes: Task 2-8 已完成全部迁移
- Produces: `rg -n "application::search|crate::application::search" src/ tests/` 零结果

- [x] **Step 1: 引用清零检查**

Run: `rg -n "application::search|application:search" src/ tests/ | rg -v "search_mcp|search_config|search_memory|search_render"; rg -n "CollectionKind|collection_name" src/ tests/ | rg -v "shared::collections"`
Expected: 第一组零结果（若有残留——如 context/stages/retriever.rs 引用 expand_nodes——先改引用方再走下一步）；第二组确认 CollectionKind 仅自引用+测试

- [x] **Step 2: 删除 search.rs + mod 声明**

`git rm src/application/search.rs`；`application/mod.rs` 移除 `pub mod search;` 行。
若 Step 1 发现残留引用（如 `context/stages/retriever.rs` 使用 expansion/fusion），改引用方使用 `application::context::fusion` 同名函数（签名兼容，RankedItem 字段一致）。

- [x] **Step 3: CollectionKind 处理**

若 Step 1 第二组确认仅 `infrastructure/qdrant/collection.rs` 自引用+其内测试：删除 `CollectionKind` 枚举、`collection_name` 函数及其测试；若存在外部引用（写入侧命名），保留并在文件头标注 `// legacy naming, 仅写入侧兼容` 不删。**以实际引用为准，不强行删。**

- [x] **Step 4: 全量验证**

Run: `cargo test 2>&1 | rg "test result" | tail -3 && cargo clippy --all-targets 2>&1 | rg "^error" | head -3`
Expected: 773+N passed / 2 failed（预存不变）；clippy 0 error

- [x] **Step 5: Commit**

```bash
git add -A src/application src/infrastructure/qdrant
git commit -m "refactor(unified-search): retire legacy application/search.rs stack (fusion/expansion/rewrite migrated)"
```

---

### Task 10: live 实测验收 + 文档收尾

**Files:**
- Create: `tests/unified_search.rs`
- Modify: `docs/superpowers/specs/2026-08-03-unified-search-design.md`（追加 §12 实施记录）
- Modify: `docs/superpowers/specs/2026-07-31-universal-knowledge-pipeline-design.md`（§13.1 加统一检索行）
- Modify: 本计划文件（勾选 + 实测记录）

**Interfaces:**
- Consumes: Task 1-9 全部；`CARGO_BIN_EXE_dt`（cargo 集成测试自动注入 bin 路径，bin 名 `dt`——Cargo.toml:7 已确认）
- Produces: 6 个 #[ignore] live 测试全部通过；spec §12 实施记录

- [x] **Step 1: 写 live 测试**

`tests/unified_search.rs`（全部 #[ignore]，需三服务在线）：

```rust
//! 统一检索 live 验收（需 Memgraph+Qdrant+xinference 在线）：
//! cargo test --test unified_search -- --ignored --nocapture

use std::process::Command;

fn dt(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_dt"))
        .args(args)
        .output()
        .expect("dt binary");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn search_json(args: &[&str]) -> serde_json::Value {
    let (stdout, stderr, code) = dt(args);
    assert_eq!(code, 0, "dt search failed: {stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not pure JSON: {e}\n--- stdout ---\n{stdout}"))
}

#[test]
#[ignore]
fn u_all_world_finds_createapp_with_analysis_and_location() {
    let v = search_json(&["search", "createApp", "--json"]);
    let hits = v["hits"].as_array().unwrap();
    let m = hits
        .iter()
        .find(|h| h["entity_type"] == "Method")
        .unwrap_or_else(|| panic!("no Method hit in all-world results: {hits:?}"));
    assert!(m["file_path"].as_str().unwrap().contains("app.js"));
    assert_eq!(m["start_line"], 32);
    assert!(!m["llm_analysis"].as_str().unwrap_or("").is_empty());
}

#[test]
#[ignore]
fn u_knowledge_ifcode_semantic_hit_via_cli() {
    let v = search_json(&["search", "新增渠道的唯一代码标识", "--world", "knowledge", "--json"]);
    let hits = v["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "knowledge world empty");
    let top3: Vec<String> = hits
        .iter()
        .take(3)
        .map(|h| h["title"].as_str().unwrap_or("").to_lowercase())
        .collect();
    assert!(top3.iter().any(|t| t.contains("ifcode")), "ifCode not in top3: {top3:?}");
}

#[test]
#[ignore]
fn u_memory_world_finds_s5_events() {
    let v = search_json(&["search", "S5", "--world", "memory", "--json"]);
    let total = v["total"].as_u64().unwrap();
    assert!(total >= 1, "memory world should find S5-era events, got 0");
}

#[test]
#[ignore]
fn u_config_world_returns_valid_json() {
    // 本地无 config_chunks 数据——只验证管线可用与 JSON 结构（不断言具体结果）
    let v = search_json(&["search", "nacos", "--world", "config", "--json"]);
    assert!(v["hits"].is_array());
    assert!(v["total"].is_u64());
}

#[test]
#[ignore]
fn u_search_kg_removed_clap_error() {
    let (_stdout, stderr, code) = dt(&["search-kg", "foo"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
}

#[test]
#[ignore]
fn u_human_format_three_lines_for_method() {
    let (stdout, _stderr, code) = dt(&["search", "createApp", "--world", "code"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("[Method] createApp"), "{stdout}");
    assert!(stdout.contains("分析:"), "{stdout}");
    assert!(stdout.contains("app.js:L32-36"), "{stdout}");
}
```

- [x] **Step 2: 运行 live 测试**

Run: `cargo test --test unified_search -- --ignored --nocapture 2>&1 | tail -12`
Expected: 6 passed / 0 failed。失败时按 systematic-debugging 归因（服务在线？数据在位？JSON 污染？），修复后重跑，**不得放宽断言**

- [x] **Step 3: spec 追加 §12 实施记录**

`2026-08-03-unified-search-design.md` 末尾追加：

```markdown
---

## 12. 实施记录（2026-08-03）

### 12.1 落地提交
（逐任务 commit hash + 一句话，从 git log 摘录）

### 12.2 实测结果
（6 个 live 测试的实际输出摘要：all 世界 createApp 命中形态、knowledge ifCode 排名、memory 事件数、人类格式样例行）

### 12.3 实施期偏差
- `dt search` 的 `--path` 参数删除（原实现中仅打印不参与检索，MCP 传参同步移除）
- （执行中发现的其他偏差逐条记录）
```

- [x] **Step 4: 主文档 §13.1 加行**

`2026-07-31-universal-knowledge-pipeline-design.md` §13.1 表格 S5 行后加：

```markdown
| **统一检索** | 检索面单栈化：CrossWorldSearch 全世界 + CLI/MCP/gRPC 全入口委托 + llm_analysis/定位字段 | ✅ 完成（2026-08-03） | 见 spec 2026-08-03 §12 | spec: 2026-08-03-unified-search-design.md |
```

- [x] **Step 5: 计划勾选 + 最终全量验证 + Commit**

本计划全部 `- [ ]` 勾选为 `- [x]`；Run: `cargo test 2>&1 | rg "test result" | tail -3 && cargo clippy --all-targets 2>&1 | rg -c "^error" || echo "clippy clean"`

```bash
git add tests/unified_search.rs docs/superpowers/
git commit -m "docs(unified-search): implementation record + live verification (6 live tests green)"
```

---

## 验收清单（spec §11 对照）

| # | 验收项 | 验证方式 |
|---|--------|---------|
| 1 | `dt search "createApp"`（默认 all）Method 命中+分析+`app.js:32-36` | Task 10 u_all_world / u_human_format |
| 2 | `dt search "云仓 支付" --world knowledge` Entity 命中+摘要+来源 | Task 10 u_knowledge（同语义） |
| 3 | `dt search-kg` → clap `unrecognized subcommand` | Task 10 u_search_kg_removed |
| 4 | MCP dt_search 返回合法 JSON 含 llm_analysis/路径/行号 | Task 7 + Task 10 JSON 纯净断言 |
| 5 | gRPC world="knowledge" 语义命中 | Task 8 proto/透传 + 人工 grpcurl 冒烟（可选） |
| 6 | cargo test / clippy 基线 | 每任务 Step + Task 9/10 全量 |
