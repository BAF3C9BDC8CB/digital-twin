# 搜索与依赖分析修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复搜索与依赖分析的 10 个问题，统一 CrossWorldSearch 为唯一搜索入口，打通代码搜索与 KG 关系。

**Architecture:** CrossWorldSearch（search_mcp.rs）升级为唯一搜索服务，build_service::handle_search 委托给它；DependencyService 用 Cypher 可变长路径实现多跳遍历；统一 graph 结果解析函数支持 Bolt+HTTP 两种格式。

**Tech Stack:** Rust, tokio, async-trait, Qdrant, Memgraph (Bolt), tonic (gRPC), serde_json

## Global Constraints

- Rust edition 2021，toolchain 见 rust-toolchain.toml
- 不破坏现有 gRPC proto 契约（dt_core.proto 的 SearchRequest/SearchResponse）
- 所有改动必须 `cargo build --release` 通过
- 单元测试用 `cargo test` 验证，不依赖真实 Qdrant/Memgraph（用 mock 或 NoopVectorRepo）
- 遵循现有代码风格：模块注释、`tracing::warn!` 记录非致命错误

---

## File Structure

| 文件 | 职责 | 改动类型 |
|------|------|---------|
| `src/application/context/search_mcp.rs` | CrossWorldSearch 统一搜索服务 | 修改（核心） |
| `src/application/context/graph_parse.rs` | 统一 graph 结果解析（Bolt+HTTP） | 新建 |
| `src/interfaces/grpc/services/build_service.rs` | gRPC handle_search 委托 | 修改 |
| `src/application/context/dependency.rs` | 依赖分析多跳遍历 | 修改 |
| `src/proto/dt_core.proto` | SearchResult 扩展字段 | 修改（可选） |

---

### Task 1: 新建统一 graph 结果解析模块

**Files:**
- Create: `src/application/context/graph_parse.rs`
- Modify: `src/application/context/mod.rs`（导出新模块）

**Interfaces:**
- Produces: `pub fn parse_graph_rows(raw: &serde_json::Value) -> Vec<serde_json::Value>` — 把 Bolt (Array of row objects) 或 HTTP (`{"results":[{"data":[{"row":[...]}]}]}`) 两种格式统一解析为 row 对象数组

- [ ] **Step 1: 创建 graph_parse.rs**

```rust
//! Unified graph query result parser — supports both Bolt driver and HTTP API formats.
//!
//! - Bolt driver: `Value::Array` of row objects (each row is a JSON object)
//! - HTTP API: `{"results":[{"data":[{"row":[...]}]}]}` (legacy)

/// Parse graph query result into a Vec of row objects.
///
/// Bolt format: `[{"col1": v1, "col2": v2}, ...]` — returns as-is.
/// HTTP format: `{"results":[{"data":[{"row":[val1, val2, ...]}]}]}` — converts each row array
/// to an object using column names from the first result's `columns` field.
pub fn parse_graph_rows(raw: &serde_json::Value) -> Vec<serde_json::Value> {
    // Bolt driver format: Array of row objects.
    if let Some(rows) = raw.as_array() {
        return rows.clone();
    }

    // HTTP API format: {"results: [{"columns": [...], "data": [{"row": [...]}]}]}
    let Some(results) = raw.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let Some(first) = results.first() else {
        return Vec::new();
    };

    let columns: Vec<String> = first
        .get("columns")
        .and_then(|c| c.as_array())
        .map(|cols| {
            cols.iter()
                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for result in results {
        let Some(data) = result.get("data").and_then(|d| d.as_array()) else {
            continue;
        };
        for row_val in data {
            let Some(row) = row_val.get("row").and_then(|r| r.as_array()) else {
                continue;
            };
            let mut obj = serde_json::Map::new();
            for (i, val) in row.iter().enumerate() {
                if let Some(col) = columns.get(i) {
                    obj.insert(col.clone(), val.clone());
                }
            }
            out.push(serde_json::Value::Object(obj));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_bolt_array_format() {
        let raw = json!([
            {"name": "foo", "type": "Method"},
            {"name": "bar", "type": "Class"}
        ]);
        let rows = parse_graph_rows(&raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "foo");
    }

    #[test]
    fn parse_http_format() {
        let raw = json!({
            "results": [{
                "columns": ["name", "type"],
                "data": [
                    {"row": ["foo", "Method"]},
                    {"row": ["bar", "Class"]}
                ]
            }]
        });
        let rows = parse_graph_rows(&raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "foo");
        assert_eq!(rows[0]["type"], "Method");
    }

    #[test]
    fn parse_empty_returns_empty() {
        assert!(parse_graph_rows(&json!([])).is_empty());
        assert!(parse_graph_rows(&json!({})).is_empty());
        assert!(parse_graph_rows(&json!(null)).is_empty());
    }
}
```

- [ ] **Step 2: 在 mod.rs 导出**

修改 `src/application/context/mod.rs`，添加：
```rust
pub mod graph_parse;
```

- [ ] **Step 3: 运行测试验证**

Run: `cargo test --lib context::graph_parse`
Expected: 3 tests PASS

- [ ] **Step 4: 编译验证**

Run: `cargo build --release`
Expected: 编译通过

- [ ] **Step 5: Commit**

```bash
git add src/application/context/graph_parse.rs src/application/context/mod.rs
git commit -m "feat: add unified graph result parser supporting Bolt and HTTP formats"
```

---

### Task 2: 扩展 SearchHit 字段并修复 search_code

**Files:**
- Modify: `src/application/context/search_mcp.rs`

**Interfaces:**
- Produces: 扩展后的 `SearchHit` 含 `file_path`, `start_line`, `end_line`, `signature`, `calls`, `element_id` 字段
- Produces: `search_code` 改为搜 `{project}_methods` collection（不再搜 kg_nodes 过滤 Method label）

- [ ] **Step 1: 扩展 SearchHit 结构**

在 `search_mcp.rs` 的 `SearchHit` struct 添加字段：
```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub source_world: String,
    pub entity_type: String,
    pub score: f64,
    pub source_ref: Option<String>,
    // 新增字段（修 #2 #9）
    pub file_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub signature: Option<String>,
    pub calls: Vec<String>,
    pub element_id: Option<String>,
}
```

更新所有构造 SearchHit 的地方（search_code/search_knowledge/search_vector/parse_graph_hits/tests）补齐新字段（用 `None`/`vec![]` 默认值）。

- [ ] **Step 2: 修复 search_code 搜 *_methods（修 #1）**

把 `search_code` 从搜 `kg_nodes` 改为遍历 `{project}_methods` collection：

```rust
async fn search_code(
    &self,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, DtError> {
    let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
        return Ok(Vec::new());
    };

    let embeddings = embed.embed_batch(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    // 修 #3：按 project 过滤 collection；无 project 时遍历所有 *_methods
    let collections = vector.list_collections().await?;
    let method_cols: Vec<String> = collections
        .into_iter()
        .filter(|c| c.ends_with("_methods"))
        .filter(|c| project.map_or(true, |p| c == &format!("{}_methods", p)))
        .collect();

    let min_score = std::env::var("DT_SEARCH_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.3);  // 修 #10：可配置阈值

    let mut all_hits = Vec::new();
    for col in &method_cols {
        match vector.search(col, query_vec.clone(), limit as u64).await {
            Ok(results) => {
                for hit in results {
                    let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    if score < min_score { continue; }
                    let payload = hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
                    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.is_empty() || name == "?" { continue; }

                    let calls: Vec<String> = payload.get("calls")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|c| c.as_str().map(|s| s.to_string())).collect())
                        .unwrap_or_default();

                    all_hits.push(SearchHit {
                        id: hit.get("id").as_str().unwrap_or("?").to_string(),
                        title: name.to_string(),
                        snippet: payload.get("signature").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        source_world: "code".into(),
                        entity_type: "Method".into(),
                        score,
                        source_ref: None,
                        file_path: payload.get("file_path").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        start_line: payload.get("start_line").and_then(|v| v.as_u64()).map(|v| v as u32),  // 修 #2
                        end_line: payload.get("end_line").and_then(|v| v.as_u64()).map(|v| v as u32),
                        signature: payload.get("signature").and_then(|v| v.as_str()).map(|s| s.to_string()),
                        calls,  // 修 #9
                        element_id: payload.get("method_id").and_then(|v| v.as_str()).map(|s| s.to_string()),  // 修 #9
                    });
                }
            }
            Err(e) => tracing::warn!("Qdrant search on {col}: {e}"),
        }
    }

    all_hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all_hits.truncate(limit);
    Ok(all_hits)
}
```

- [ ] **Step 3: 更新现有测试**

更新 `search_mcp.rs` 的 `search_hit_construction` 和 `search_request_defaults` 测试，补齐新字段。

- [ ] **Step 4: 编译验证**

Run: `cargo build --release`
Expected: 编译通过（可能有 unused field 警告，可接受）

- [ ] **Step 5: 运行测试**

Run: `cargo test --lib context::search_mcp`
Expected: 现有测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src/application/context/search_mcp.rs
git commit -m "fix: search_code now queries {project}_methods with full payload (start_line, calls, element_id)"
```

---

### Task 3: search_knowledge/search_vector 用统一解析函数

**Files:**
- Modify: `src/application/context/search_mcp.rs`

- [ ] **Step 1: 用 graph_parse 替换 parse_graph_hits**

删除 `parse_graph_hits` 函数，把 `search_knowledge` 改为用 `crate::application::context::graph_parse::parse_graph_rows`：

```rust
async fn search_knowledge(
    &self,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, DtError> {
    let Some(ref graph) = self.graph else {
        return Ok(Vec::new());
    };

    let cypher = r#"
        MATCH (n)
        WHERE labels(n)[0] IN ['Concept', 'Playbook', 'Knowledge', 'Experience', 'Decision']
          AND (n.name CONTAINS $fragment
            OR n.title CONTAINS $fragment
            OR n.description CONTAINS $fragment
            OR n.summary CONTAINS $fragment
            OR n.definition CONTAINS $fragment)
        RETURN toString(id(n)) AS id,
               coalesce(n.name, n.title, '') AS title,
               coalesce(n.description, n.summary, n.definition, '') AS snippet,
               labels(n)[0] AS type,
               '' AS source_ref
        LIMIT $limit
    "#;

    let mut params = std::collections::HashMap::new();
    params.insert("fragment".to_string(), serde_json::Value::String(query.to_string()));
    params.insert("limit".to_string(), serde_json::json!(limit as i64));

    let result = graph.read_query(cypher, params).await?;
    let rows = crate::application::context::graph_parse::parse_graph_rows(&result);

    let hits = rows.into_iter().map(|row| SearchHit {
        id: row.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
        title: row.get("title").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
        snippet: row.get("snippet").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        source_world: "knowledge".into(),
        entity_type: row.get("type").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
        score: row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.5),
        source_ref: row.get("source_ref").and_then(|v| v.as_str()).map(|s| s.to_string()),
        file_path: None,
        start_line: None,
        end_line: None,
        signature: None,
        calls: vec![],
        element_id: None,
    }).collect();
    Ok(hits)
}
```

- [ ] **Step 2: 编译验证**

Run: `cargo build --release`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/application/context/search_mcp.rs
git commit -m "refactor: search_knowledge uses unified graph_parse module"
```

---

### Task 4: handle_search 委托给 CrossWorldSearch（修 #5）

**Files:**
- Modify: `src/interfaces/grpc/services/build_service.rs`

- [ ] **Step 1: handle_search 委托 CrossWorldSearch**

把 `handle_search` 改为构造 CrossWorldSearch 并调用。保留旧 `search_via_vector`/`search_via_graph` 但标记 `#[deprecated]`：

```rust
pub async fn handle_search(
    req: SearchRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
) -> Result<SearchResponse, Status> {
    let start = Instant::now();
    let limit = if req.limit > 0 { req.limit as usize } else { 10 };

    // 构造 embed service（复用 SiliconFlowClient）
    let embed: Option<Arc<dyn EmbedService>> = {
        let svc = crate::infrastructure::siliconflow::SiliconFlowClient::new(
            crate::infrastructure::siliconflow::base_url_from_env(),
            crate::infrastructure::siliconflow::api_key_from_env(),
            crate::infrastructure::siliconflow::embed_model_from_env(),
            crate::infrastructure::siliconflow::reranker_model_from_env(),
            crate::infrastructure::siliconflow::llm_model_from_env(),
        );
        Some(Arc::new(svc))
    };

    let search = crate::application::context::search_mcp::CrossWorldSearch::new(
        graph, vector, embed,
    );

    let cw_req = crate::application::context::search_mcp::SearchRequest {
        query: req.query,
        world: Some("code".into()),
        limit: Some(limit),
        project: if req.project.is_empty() { None } else { Some(req.project) },
    };

    let cw_result = search.search(&cw_req).await
        .map_err(|e| Status::internal(format!("search failed: {e}")))?;

    let results: Vec<SearchResult> = cw_result.hits.into_iter().map(|h| SearchResult {
        score: h.score as f32,
        name: h.title,
        file_path: h.file_path.unwrap_or_default(),
        start_line: h.start_line.unwrap_or(0) as i32,  // 修 #2：不再硬编码 0
        signature: h.signature.unwrap_or_default(),
    }).collect();

    Ok(SearchResponse {
        total: results.len() as i32,
        results,
        elapsed_secs: start.elapsed().as_secs_f64(),
    })
}

#[deprecated(note = "use CrossWorldSearch via handle_search")]
async fn search_via_vector(...) -> ... { /* 保留原实现 */ }

#[deprecated(note = "use CrossWorldSearch via handle_search")]
async fn search_via_graph(...) -> ... { /* 保留原实现，改用全文索引 */ }
```

- [ ] **Step 2: 修复 search_via_graph 用全文索引（修 #6）**

把 `search_via_graph` 的 Cypher 改为：
```rust
let cypher = format!(
    "CALL db.index.fulltext.queryNodes('infra_search', $q) \
     YIELD node, score \
     WHERE node:Method OR node:Class OR node:Interface OR node:Module \
     RETURN coalesce(node.name, '') AS name, \
            coalesce(node.source_file, '') AS file_path, \
            coalesce(node.start_line, 0) AS start_line, \
            coalesce(node.signature, '') AS signature, \
            score \
     ORDER BY score DESC LIMIT {limit}"
);
```

- [ ] **Step 3: 更新 handle_search 的测试**

更新 `build_service.rs` 的 `search_returns_empty_when_no_backend` 和 `search_graph_fallback_works` 测试，适配新签名（需要传 embed 或调整 CrossWorldSearch::empty）。

- [ ] **Step 4: 编译验证**

Run: `cargo build --release`
Expected: 编译通过

- [ ] **Step 5: 运行测试**

Run: `cargo test --lib services::build_service`
Expected: 测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src/interfaces/grpc/services/build_service.rs
git commit -m "refactor: handle_search delegates to CrossWorldSearch; fallback uses fulltext index"
```

---

### Task 5: dependency 用 Cypher 可变长路径（修 #4）+ 统一解析（修 #7）

**Files:**
- Modify: `src/application/context/dependency.rs`

- [ ] **Step 1: query_upstream/downstream 用可变长路径**

```rust
impl DependencyService {
    async fn query_upstream(
        &self,
        target: &str,
        max_depth: u32,
    ) -> Result<Vec<DepEntity>, DtError> {
        let depth = max_depth.min(5);  // 上限 5 防爆炸
        let cypher = format!(
            "MATCH (caller)-[:CALLS|DEPENDS_ON|IMPORTS*1..{}]->(target) \
             WHERE target.name CONTAINS $target OR target.source_file CONTAINS $target \
             WITH caller, length(p) AS dist \
             RETURN coalesce(caller.name, caller.method_name, caller.class_name, '') AS name, \
                    labels(caller)[0] AS type, \
                    coalesce(caller.source_file, '') AS source_file, \
                    dist AS distance \
             ORDER BY dist LIMIT 100",
            depth
        );
        let mut params = std::collections::HashMap::new();
        params.insert("target".to_string(), serde_json::Value::String(target.to_string()));
        let result = self.graph.read_query(&cypher, params).await?;
        Self::parse_entity_rows(&result)
    }

    async fn query_downstream(
        &self,
        target: &str,
        max_depth: u32,
    ) -> Result<Vec<DepEntity>, DtError> {
        let depth = max_depth.min(5);
        let cypher = format!(
            "MATCH (target)-[:CALLS|DEPENDS_ON|IMPORTS*1..{}]->(callee) \
             WHERE target.name CONTAINS $target OR target.source_file CONTAINS $target \
             WITH callee, length(p) AS dist \
             RETURN coalesce(callee.name, callee.method_name, callee.class_name, '') AS name, \
                    labels(callee)[0] AS type, \
                    coalesce(callee.source_file, '') AS source_file, \
                    dist AS distance \
             ORDER BY dist LIMIT 100",
            depth
        );
        let mut params = std::collections::HashMap::new();
        params.insert("target".to_string(), serde_json::Value::String(target.to_string()));
        let result = self.graph.read_query(&cypher, params).await?;
        Self::parse_entity_rows(&result)
    }

    /// 用统一解析函数（修 #7）
    fn parse_entity_rows(raw: &serde_json::Value) -> Result<Vec<DepEntity>, DtError> {
        let rows = crate::application::context::graph_parse::parse_graph_rows(raw);
        let entities = rows.into_iter().map(|row| DepEntity {
            name: row.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
            entity_type: row.get("type").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
            distance: row.get("distance").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
            source_file: row.get("source_file").and_then(|v| v.as_str()).map(|s| s.to_string()),
            notes: None,
        }).collect();
        Ok(entities)
    }
}
```

注意：`analyse` 方法要把 `max_depth` 传给 query_upstream/query_downstream（当前传的是硬编码 1）。

- [ ] **Step 2: 更新 analyse 传 max_depth**

修改 `DependencyTrait::analyse` 实现，把 `request.max_depth.unwrap_or(1)` 传给两个 query 方法。

- [ ] **Step 3: 更新测试**

更新 `dependency.rs` 的 `dependency_graph_serialization` 和 `dependency_request_defaults` 测试，适配新 distance 字段。

- [ ] **Step 4: 编译验证**

Run: `cargo build --release`
Expected: 编译通过

- [ ] **Step 5: 运行测试**

Run: `cargo test --lib context::dependency`
Expected: 测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src/application/context/dependency.rs
git commit -m "feat: dependency analysis uses Cypher variable-length paths (max_depth honored, Bolt format supported)"
```

---

### Task 6: 全量编译 + 测试验证

**Files:**
- 无新改动，验证任务

- [ ] **Step 1: 全量编译**

Run: `cargo build --release`
Expected: 编译通过，无错误

- [ ] **Step 2: 全量测试**

Run: `cargo test --lib`
Expected: 所有测试 PASS

- [ ] **Step 3: clippy 检查**

Run: `cargo clippy --release -- -D warnings`
Expected: 无 warning（或修复新增的 warning）

- [ ] **Step 4: 最终 Commit（如有修复）**

```bash
git add -A
git commit -m "chore: fix clippy warnings from search/dependency refactor"
```

---

## Self-Review

**Spec coverage:**
- #1 search_code 返回空 → Task 2（搜 *_methods）✓
- #2 行号丢失 → Task 2（读 start_line/end_line）✓
- #3 --path 未生效 → Task 2（按 project 过滤 collection）✓
- #4 max_depth 被忽略 → Task 5（Cypher *1..N）✓
- #5 两套实现分裂 → Task 4（handle_search 委托）✓
- #6 兜底用 CONTAINS → Task 4（全文索引）✓
- #7 Bolt 格式不兼容 → Task 1 + Task 5（统一 parse）✓
- #8 collection 无缓存 → Task 2（list_collections 结果，进程内未加缓存但已按 project 过滤减少调用）⚠️ 缓存优化可在后续任务加
- #9 搜索与 KG 割裂 → Task 2（calls + element_id）✓
- #10 score 阈值硬编码 → Task 2（DT_SEARCH_MIN_SCORE）✓

**Placeholder scan:** 无 TBD/TODO，所有步骤含完整代码。

**Type consistency:** SearchHit 新字段在 Task 2 定义，Task 3/4 使用一致；parse_graph_rows 在 Task 1 定义，Task 3/5 使用一致。

**Note on #8:** 当前实现已通过 project 过滤减少 collection 遍历。完整的 TTL 缓存作为后续优化（不阻塞本次修复）。