# Digital Twin v3.1 激进重构 - 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 20 个已识别缺陷 + 模块拆分重构 + 集成测试覆盖

**Architecture:** 将 `engine-rust/src/` 从 18 个平铺文件重组为 5 个子模块 (client, index, sync, common + 根级)，提取公共 trait 消除重复代码，建立 `OnceCell<reqwest::Client>` 全局连接池，CALLS 增量构建，统一错误处理。

**Tech Stack:** Rust 2021, tokio, reqwest, rusqlite, tree-sitter (7 languages), Neo4j HTTP API, Qdrant HTTP API

---

## 文件结构总览

```
engine-rust/src/
├── main.rs              # CLI 入口（修改：更新 use 路径）
├── config.rs            # 配置（修改：H3 移除默认密码）
├── models.rs            # 数据模型（不变）
├── scanner.rs           # 文件扫描（不变）
├── parser.rs            # 解析器（修改：H5/H6/H15）
├── search.rs            # 搜索（修改：L17 call chain）
├── health.rs            # 健康检查（修改：use 路径）
├── validate.rs          # 验证（修改：use 路径）
├── event.rs             # 事件（修改：use 路径）
├── knowledge.rs         # 知识（修改：use 路径）
│
├── client/              # 新建：后端客户端层
│   ├── mod.rs           # 全局 OnceCell<reqwest::Client>
│   ├── neo4j.rs         # 从 neo4j.rs 移动（修改：H8/H10/M9）
│   ├── qdrant.rs        # 从 qdrant.rs 移动（修改：M9）
│   └── embed.rs         # 从 embed.rs 移动（修改：M9）
│
├── index/               # 新建：索引编排层
│   ├── mod.rs
│   ├── convert.rs       # 新建：From trait（H7）
│   ├── build.rs         # 从 build.rs 提取增量构建（修改：H4/H7/M10）
│   ├── full.rs          # 从 build.rs 提取全量索引（修改：H7/L18）
│   ├── update.rs        # 从 update.rs 移动（修改：H7）
│   ├── remove.rs        # 从 remove.rs 移动（不变）
│   └── callgraph.rs     # 新建：CALLS 增量构建（H4）
│
├── sync/                # 新建：外部同步
│   ├── mod.rs
│   ├── nacos.rs         # 从 nacos_sync.rs 移动（不变）
│   └── k8s.rs           # 从 k8s_sync.rs 移动（不变）
│
└── common/              # 新建：公共工具
    ├── mod.rs
    ├── hash.rs           # SHA1/SHA256 辅助
    └── error.rs          # 统一错误处理
```

---

### Task 0: 准备工作 — 创建目录结构和公共模块

**Files:**
- Create: `engine-rust/src/client/mod.rs`
- Create: `engine-rust/src/client/neo4j.rs`
- Create: `engine-rust/src/client/qdrant.rs`
- Create: `engine-rust/src/client/embed.rs`
- Create: `engine-rust/src/index/mod.rs`
- Create: `engine-rust/src/index/convert.rs`
- Create: `engine-rust/src/index/build.rs`
- Create: `engine-rust/src/index/full.rs`
- Create: `engine-rust/src/index/update.rs`
- Create: `engine-rust/src/index/remove.rs`
- Create: `engine-rust/src/index/callgraph.rs`
- Create: `engine-rust/src/sync/mod.rs`
- Create: `engine-rust/src/sync/nacos.rs`
- Create: `engine-rust/src/sync/k8s.rs`
- Create: `engine-rust/src/common/mod.rs`
- Create: `engine-rust/src/common/hash.rs`
- Create: `engine-rust/src/common/error.rs`
- Delete: `engine-rust/src/neo4j.rs`
- Delete: `engine-rust/src/qdrant.rs`
- Delete: `engine-rust/src/embed.rs`
- Delete: `engine-rust/src/build.rs`
- Delete: `engine-rust/src/update.rs`
- Delete: `engine-rust/src/remove.rs`
- Delete: `engine-rust/src/nacos_sync.rs`
- Delete: `engine-rust/src/k8s_sync.rs`
- Create: `services/search-web/lazy_consistency.py`
- Create: `engine-rust/tests/parser_test.rs`
- Create: `engine-rust/tests/convert_test.rs`
- Create: `engine-rust/tests/build_test.rs`
- Create: `engine-rust/tests/neo4j_test.rs`
- Modify: `engine-rust/Cargo.toml`
- Modify: `engine-rust/src/main.rs`
- Modify: `engine-rust/src/config.rs`
- Modify: `engine-rust/src/parser.rs`
- Modify: `engine-rust/src/search.rs`
- Modify: `engine-rust/src/health.rs`
- Modify: `engine-rust/src/validate.rs`
- Modify: `engine-rust/src/event.rs`
- Modify: `engine-rust/src/knowledge.rs`
- Modify: `engine-rust/src/scanner.rs` (add re-export)
- Modify: `services/search-web/app.py`
- Modify: `services/embed-server/requirements.txt`
- Modify: `dt-sync`
- Modify: `config.yaml.example`

- [ ] **Step 0.0: 创建新目录**

```bash
mkdir -p engine-rust/src/client
mkdir -p engine-rust/src/index
mkdir -p engine-rust/src/sync
mkdir -p engine-rust/src/common
mkdir -p engine-rust/tests
```

---

### Task 1: 公共模块 — common/hash.rs 和 common/error.rs

**Files:**
- Create: `engine-rust/src/common/mod.rs`
- Create: `engine-rust/src/common/hash.rs`
- Create: `engine-rust/src/common/error.rs`

- [ ] **Step 1.1: 写入 common/mod.rs**

```rust
pub mod hash;
pub mod error;
```

- [ ] **Step 1.2: 写入 common/hash.rs**

```rust
use sha1::{Digest, Sha1};
use sha2::Sha256;

pub fn sha1_hex(data: &str) -> String {
    let mut h = Sha1::new();
    h.update(data.as_bytes());
    hex::encode(h.finalize())
}

pub fn sha256_hex(data: &str) -> String {
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    hex::encode(h.finalize())
}

pub fn sha256_truncated_hex(data: &str) -> String {
    hex::encode(&Sha256::digest(data.as_bytes())[..20])
}

pub fn method_id_to_u64(method_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(method_id.as_bytes());
    let result = hasher.finalize();
    u64::from_be_bytes(result[..8].try_into().unwrap())
}
```

- [ ] **Step 1.3: 写入 common/error.rs**

```rust
use std::fmt;

#[derive(Debug)]
pub enum DtError {
    Neo4j(String),
    Qdrant(String),
    Embed(String),
    Sqlite(String),
    Parse(String),
    Config(String),
}

impl fmt::Display for DtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DtError::Neo4j(m) => write!(f, "Neo4j error: {}", m),
            DtError::Qdrant(m) => write!(f, "Qdrant error: {}", m),
            DtError::Embed(m) => write!(f, "Embed error: {}", m),
            DtError::Sqlite(m) => write!(f, "SQLite error: {}", m),
            DtError::Parse(m) => write!(f, "Parse error: {}", m),
            DtError::Config(m) => write!(f, "Config error: {}", m),
        }
    }
}

impl std::error::Error for DtError {}

#[macro_export]
macro_rules! warn_on_err {
    ($expr:expr, $ctx:expr) => {
        if let Err(e) = $expr {
            eprintln!("[warn] {}: {}", $ctx, e);
        }
    };
}
```

- [ ] **Step 1.4: 编译验证**

```bash
cd engine-rust && cargo check 2>&1 | head -20
```

Expected: 编译通过（common 模块无外部依赖问题）

---

### Task 2: 版本号统一 + Cargo.toml 依赖清理 (H1, M14)

**Files:**
- Modify: `engine-rust/Cargo.toml`

- [ ] **Step 2.1: 更新 Cargo.toml**

Read current `Cargo.toml`，做以下修改：
- `version = "3.0.0"` → `version = "3.1.0"`
- 删除 `rayon = "1"` 行
- 删除 `indicatif = "0.17"` 行
- 添加 `tempfile = "3"` (测试用)
- 添加 `lazy_static = "1"` (连接池用)

```toml
[package]
name = "dt"
version = "3.1.0"
edition = "2021"

[dependencies]
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "blocking", "rustls-tls"], default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
sha1 = "0.10"
sha2 = "0.10"
hex = "0.4"
base64 = "0.22"
anyhow = "1"
chrono = { version = "0.4", features = ["serde"] }
rusqlite = { version = "0.31", features = ["bundled"] }
walkdir = "2"
regex = "1"
lazy_static = "1"
tree-sitter = "0.24"
tree-sitter-java = "0.23"
tree-sitter-typescript = "0.23"
tree-sitter-python = "0.23"
tree-sitter-go = "0.23"
tree-sitter-rust = "0.23"
tree-sitter-php = "0.23"
tree-sitter-javascript = "0.23"

[dev-dependencies]
tempfile = "3"

[package.metadata.rust-analyzer]
# workaround for $ in string literals
```

- [ ] **Step 2.2: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

Expected: 可能有未使用导入的警告（后续 Task 修复），但无编译错误

---

### Task 3: client/ 模块 — 连接池 + 客户端层 (M9, H8, M10)

**Files:**
- Create: `engine-rust/src/client/mod.rs`
- Create: `engine-rust/src/client/neo4j.rs`
- Create: `engine-rust/src/client/qdrant.rs`
- Create: `engine-rust/src/client/embed.rs`
- Delete: `engine-rust/src/neo4j.rs`
- Delete: `engine-rust/src/qdrant.rs`
- Delete: `engine-rust/src/embed.rs`

- [ ] **Step 3.1: 写入 client/mod.rs**

```rust
pub mod neo4j;
pub mod qdrant;
pub mod embed;

use lazy_static::lazy_static;
use std::time::Duration;

lazy_static! {
    static ref HTTP_CLIENT: reqwest::Client = reqwest::Client::builder()
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("Failed to create HTTP client");
}

pub fn get_client() -> &'static reqwest::Client {
    &HTTP_CLIENT
}
```

- [ ] **Step 3.2: 写入 client/neo4j.rs (完整内容)**

从原 `neo4j.rs` 复制所有内容，然后做以下修改：

1. 所有 `client()` 函数调用 → `crate::client::get_client()`
2. `delete_all_methods` 从字符串拼接改为参数化：

```rust
pub async fn delete_all_methods(project: &str) -> Result<()> {
    run_cypher_raw(
        "MATCH (m:Method {project: $project}) DETACH DELETE m",
        json!({"project": project}),
    ).await?;
    run_cypher_raw(
        "MATCH (c:Class) WHERE NOT (c)-[:CONTAINS]->() DELETE c",
        json!({}),
    ).await?;
    Ok(())
}
```

3. 所有 `let _ =` 错误吞没 → 使用 `warn_on_err!` 宏

完整文件内容（含所有修改）：

```rust
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use base64::Engine;

use crate::config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodNode {
    pub method_id: String,
    pub project: String,
    pub file_path: String,
    pub language: String,
    pub package_or_module: String,
    pub class_name: String,
    pub name: String,
    pub signature: String,
    pub params: String,
    pub return_type: String,
    pub start_line: i64,
    pub end_line: i64,
    pub calls: Vec<String>,
}

fn neo4j_url() -> String {
    let cfg = config::load();
    let base = cfg.services.neo4j.url.trim_end_matches('/');
    format!("{}/db/neo4j/tx/commit", base)
}

fn auth_header() -> String {
    let cfg = config::load();
    let cred = format!("{}:{}", cfg.services.neo4j.user, cfg.services.neo4j.password);
    format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, cred.as_bytes()))
}

pub async fn run_cypher_raw(statement: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let body = json!({
        "statements": [{
            "statement": statement,
            "parameters": params
        }]
    });
    let client = crate::client::get_client();
    let resp = client
        .post(neo4j_url())
        .header("Content-Type", "application/json")
        .header("Authorization", auth_header())
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("Neo4j request failed ({}): {}", status, text));
    }
    let data: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("JSON parse failed (status {}): {} | body: {}", status, e, &text[..200.min(text.len())]))?;
    Ok(data)
}

pub async fn ensure_schema() -> Result<()> {
    let constraints = [
        "CREATE CONSTRAINT IF NOT EXISTS FOR (m:Method) REQUIRE m.method_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (c:Class) REQUIRE c.class_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (e:Event) REQUIRE e.event_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (k:Knowledge) REQUIRE k.id IS UNIQUE",
        "CREATE INDEX IF NOT EXISTS FOR (m:Method) ON (m.project)",
        "CREATE INDEX IF NOT EXISTS FOR (m:Method) ON (m.name)",
        "CREATE INDEX IF NOT EXISTS FOR (m:Method) ON (m.file_path)",
        "CREATE INDEX IF NOT EXISTS FOR (e:Event) ON (e.type)",
        "CREATE INDEX IF NOT EXISTS FOR (e:Event) ON (e.timestamp)",
        "CREATE INDEX IF NOT EXISTS FOR (k:Knowledge) ON (k.project)",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosConfig) REQUIRE n.config_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosService) REQUIRE n.service_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosInstance) REQUIRE n.instance_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:Environment) REQUIRE n.name IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosNamespace) REQUIRE n.namespace_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:KubernetesCluster) REQUIRE n.name IS UNIQUE",
        "CREATE INDEX IF NOT EXISTS FOR (n:K8sPod) ON (n.ip)",
        "CREATE INDEX IF NOT EXISTS FOR (n:K8sPod) ON (n.name)",
        "CREATE INDEX IF NOT EXISTS FOR (n:Deployment) ON (n.name)",
        "CREATE INDEX IF NOT EXISTS FOR (n:K8sService) ON (n.name)",
        "CREATE INDEX IF NOT EXISTS FOR (n:NacosConfig) ON (n.data_id)",
        "CREATE INDEX IF NOT EXISTS FOR (n:NacosService) ON (n.name)",
    ];
    for cypher in &constraints {
        run_cypher_raw(cypher, json!({})).await?;
    }
    Ok(())
}

pub async fn write_methods_batch(methods: &[MethodNode]) -> Result<()> {
    if methods.is_empty() { return Ok(()); }
    let stmt = "\
UNWIND $methods AS m
MERGE (n:Method {method_id: m.method_id})
SET n.project = m.project,
    n.file_path = m.file_path,
    n.language = m.language,
    n.package_or_module = m.package_or_module,
    n.class_name = m.class_name,
    n.name = m.name,
    n.signature = m.signature,
    n.params = m.params,
    n.return_type = m.return_type,
    n.start_line = m.start_line,
    n.end_line = m.end_line,
    n.calls = m.calls";
    let methods_val: Vec<serde_json::Value> = methods.iter().map(|m| {
        json!({
            "method_id": m.method_id, "project": m.project, "file_path": m.file_path,
            "language": m.language, "package_or_module": m.package_or_module,
            "class_name": m.class_name, "name": m.name, "signature": m.signature,
            "params": m.params, "return_type": m.return_type,
            "start_line": m.start_line, "end_line": m.end_line, "calls": m.calls,
        })
    }).collect();
    run_cypher_raw(stmt, json!({"methods": methods_val})).await?;
    Ok(())
}

pub async fn write_classes_batch(classes: &[ClassBatchEntry]) -> Result<()> {
    if classes.is_empty() { return Ok(()); }
    let stmt = "\
UNWIND $classes AS c
MERGE (n:Class {class_id: c.class_id})
SET n.name = c.name,
    n.project = c.project,
    n.file_path = c.file_path,
    n.package_or_module = c.package_or_module
WITH n, c
UNWIND c.method_ids AS mid
MATCH (m:Method {method_id: mid})
MERGE (n)-[:CONTAINS]->(m)";
    let classes_val: Vec<serde_json::Value> = classes.iter().map(|c| {
        json!({
            "class_id": c.class_id, "name": c.name, "project": c.project,
            "file_path": c.file_path, "package_or_module": c.package_or_module,
            "method_ids": c.method_ids,
        })
    }).collect();
    run_cypher_raw(stmt, json!({"classes": classes_val})).await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassBatchEntry {
    pub class_id: String,
    pub name: String,
    pub project: String,
    pub file_path: String,
    pub package_or_module: String,
    pub method_ids: Vec<String>,
}

pub async fn delete_methods_by_file(project: &str, file_path: &str) -> Result<()> {
    let stmt = "\
MATCH (m:Method {project: $project, file_path: $file_path})
OPTIONAL MATCH (c:Class)-[r:CONTAINS]->(m)
DELETE r
WITH m
DETACH DELETE m";
    run_cypher_raw(stmt, json!({"project": project, "file_path": file_path})).await?;
    Ok(())
}

pub async fn delete_all_methods(project: &str) -> Result<()> {
    run_cypher_raw(
        "MATCH (m:Method {project: $project}) DETACH DELETE m",
        json!({"project": project}),
    ).await?;
    run_cypher_raw(
        "MATCH (c:Class) WHERE NOT (c)-[:CONTAINS]->() DELETE c",
        json!({}),
    ).await?;
    Ok(())
}

pub async fn create_call_relationships(project: &str) -> Result<u64> {
    let stmt = "\
MATCH (caller:Method {project: $project})
UNWIND caller.calls AS called_name
WITH caller, called_name
WHERE size(called_name) >= 3 AND called_name <> caller.name
MATCH (callee:Method {project: $project, name: called_name})
WHERE callee.method_id <> caller.method_id
MERGE (caller)-[:CALLS]->(callee)
RETURN count(*) AS created";
    let resp = run_cypher_raw(stmt, json!({"project": project})).await?;
    let count = resp["results"][0]["data"][0]["row"][0].as_i64().unwrap_or(0);
    Ok(count as u64)
}

pub async fn create_call_relationships_incremental(
    project: &str,
    file_paths: &[String],
) -> Result<u64> {
    if file_paths.is_empty() {
        return Ok(0);
    }
    let stmt = "\
MATCH (caller:Method {project: $project})
WHERE caller.file_path IN $file_paths
UNWIND caller.calls AS called_name
WITH caller, called_name
WHERE size(called_name) >= 3 AND called_name <> caller.name
MATCH (callee:Method {project: $project, name: called_name})
WHERE callee.method_id <> caller.method_id
MERGE (caller)-[:CALLS]->(callee)
RETURN count(*) AS created";
    let resp = run_cypher_raw(
        stmt,
        json!({"project": project, "file_paths": file_paths}),
    ).await?;
    let count = resp["results"][0]["data"][0]["row"][0].as_i64().unwrap_or(0);
    Ok(count as u64)
}

pub async fn health() -> Result<()> {
    let body = json!({"statements": [{"statement": "RETURN 1", "parameters": {}}]});
    let client = crate::client::get_client();
    let resp = client
        .post(neo4j_url())
        .header("Content-Type", "application/json")
        .header("Authorization", auth_header())
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Neo4j unavailable: {}", text));
    }
    Ok(())
}
```

- [ ] **Step 3.3: 写入 client/qdrant.rs**

从原 `qdrant.rs` 复制，全部 `client_short()`/`client_long()` → `crate::client::get_client()`：

```rust
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

use crate::config;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f64,
    pub payload: HashMap<String, serde_json::Value>,
}

fn qdrant_url() -> String {
    config::load().services.qdrant.url.trim_end_matches('/').to_string()
}

pub async fn ensure_collection(name: &str, dim: usize) -> Result<()> {
    let url = format!("{}/collections/{}", qdrant_url(), name);
    let client = crate::client::get_client();
    if client.get(&url).send().await?.status().is_success() {
        return Ok(());
    }
    let body = json!({
        "name": name,
        "vectors": { "size": dim, "distance": "Cosine" },
        "hnsw_config": { "m": 16, "ef_construct": 100 },
        "optimizers_config": { "indexing_threshold": 10000 }
    });
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        if !text.contains("already exists") {
            return Err(anyhow!("Failed to create collection: {}", text));
        }
    }
    Ok(())
}

pub async fn delete_collection(name: &str) -> Result<()> {
    let url = format!("{}/collections/{}", qdrant_url(), name);
    let client = crate::client::get_client();
    let resp = client.delete(&url).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Failed to delete collection: {}", text));
    }
    Ok(())
}

pub async fn upsert_points(
    collection: &str,
    points: Vec<(serde_json::Value, Vec<f32>, HashMap<String, serde_json::Value>)>,
) -> Result<()> {
    let qdrant_points: Vec<serde_json::Value> = points.into_iter()
        .map(|(id, vector, payload)| json!({"id": id, "vector": vector, "payload": payload}))
        .collect();
    let body = json!({"points": qdrant_points});
    let url = format!("{}/collections/{}/points", qdrant_url(), collection);
    let client = crate::client::get_client();
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Upsert failed: {}", text));
    }
    Ok(())
}

pub async fn delete_points_by_filter(collection: &str, file_path: &str) -> Result<()> {
    let body = json!({
        "filter": {
            "must": [{ "key": "file_path", "match": { "value": file_path } }]
        }
    });
    let url = format!("{}/collections/{}/points/delete", qdrant_url(), collection);
    let client = crate::client::get_client();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Failed to delete points: {}", text));
    }
    Ok(())
}

pub async fn search(collection: &str, vector: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>> {
    let body = json!({"vector": vector, "limit": limit, "with_payload": true});
    let url = format!("{}/collections/{}/points/search", qdrant_url(), collection);
    let client = crate::client::get_client();

    #[derive(Deserialize)]
    struct QdrantResp { result: Vec<QdrantPoint> }
    #[derive(Deserialize)]
    struct QdrantPoint { id: serde_json::Value, score: f64, payload: HashMap<String, serde_json::Value> }

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    let data: QdrantResp = resp.json().await?;
    Ok(data.result.into_iter().map(|p| SearchResult {
        id: format!("{}", p.id),
        score: p.score,
        payload: p.payload,
    }).collect())
}
```

- [ ] **Step 3.4: 写入 client/embed.rs**

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Serialize)]
struct EmbedBatchReq { texts: Vec<String> }

#[derive(Deserialize)]
struct EmbedBatchResp { vectors: Vec<Vec<f32>>, dim: usize }

pub async fn health() -> Result<(String, usize)> {
    let cfg = config::load();
    #[derive(Deserialize)]
    struct HealthResp { status: String, model: String, dim: usize }
    let client = crate::client::get_client();
    let resp: HealthResp = client
        .get(format!("{}/health", cfg.services.embed_server.url))
        .send().await?.json().await?;
    Ok((resp.model, resp.dim))
}

pub async fn embed_batch(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let cfg = config::load();
    let client = crate::client::get_client();
    let resp: EmbedBatchResp = client
        .post(format!("{}/embed-batch", cfg.services.embed_server.url))
        .json(&EmbedBatchReq { texts })
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.vectors)
}
```

- [ ] **Step 3.5: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

Expected: 编译通过（main.rs 中的旧 use 路径会导致错误，Task 7 修复）

---

### Task 4: config.rs — 凭证安全 (H3)

**Files:**
- Modify: `engine-rust/src/config.rs`

- [ ] **Step 4.1: 修改 config.rs 默认密码**

找到第 95 行附近的 `"neo4j".into()` 默认密码，改为空字符串：

```rust
neo4j: Neo4jConfig {
    url: "http://localhost:7474".into(),
    user: "neo4j".into(),
    password: String::new(),  // 原为 "neo4j".into() — 强制用户配置
},
```

- [ ] **Step 4.2: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

---

### Task 5: parser.rs — 行号修复 + class_name 填充 + 非支持语言 (H5, H6, H15)

**Files:**
- Modify: `engine-rust/src/parser.rs`

- [ ] **Step 5.1: 修复行号计算 (H5)**

原代码 `parser.rs:69-70`:
```rust
let start = content[..node.start_byte()].chars().filter(|c| *c == '\n').count() + 1;
let end = content[..node.end_byte()].chars().filter(|c| *c == '\n').count() + 1;
```

替换为：
```rust
let start = node.start_position().row + 1;
let end = node.end_position().row + 1;
```

同样修复第 98-99 行的 class 行号计算。

- [ ] **Step 5.2: 填充 class_name (H6)**

在 `walk_node` 函数中，遍历时追踪当前类名，填充到方法的 `class_name` 字段：

修改 `walk_node` 签名，增加 `current_class: &str` 参数：
```rust
fn walk_node(node: &tree_sitter::Node, content: &str, project: &str, path: &str, lang: &str,
    methods: &mut Vec<MethodBlock>, classes: &mut Vec<ClassBlock>, current_class: &str) {
```

在遍历子节点时，如果当前节点是 class，子节点用类名递归：
```rust
let class_name_for_children = if is_class {
    name_node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(content.as_bytes()).ok())
        .unwrap_or("")
} else {
    current_class
};

for i in 0..node.child_count() {
    if let Some(child) = node.child(i) {
        walk_node(&child, content, project, path, lang, methods, classes, class_name_for_children);
    }
}
```

创建方法时使用 `current_class`:
```rust
methods.push(MethodBlock {
    // ...
    class_name: current_class.to_string(),
    // ...
});
```

**注意**：`parse_file` 中初始调用传 `""`。

同时修复 `make_method_id`，使用 class_name 参与 hash 以避免不同类同方法名的碰撞。

- [ ] **Step 5.3: 非支持语言返回空 (H15)**

原代码第 32 行：
```rust
let idx = match ext { "java" => 0, "ts"|"tsx" => 1, "py" => 2, "go" => 3, "rs" => 4, "php" => 5, _ => 6 };
if idx >= self.parsers.len() { return Ok(ParsedFile::default()); }
```

修改为：当扩展名不在已知列表中且 idx >= 7 时直接返回空，而不是用 JS parser 乱解析：
```rust
let (idx, lang_name) = match ext {
    "java" => (0, "java"),
    "ts" | "tsx" => (1, "typescript"),
    "py" => (2, "python"),
    "go" => (3, "go"),
    "rs" => (4, "rust"),
    "php" => (5, "php"),
    "js" | "jsx" | "mjs" | "cjs" => (6, "javascript"),
    _ => return Ok(ParsedFile::default()),
};
let parser = &mut self.parsers[idx];
```

- [ ] **Step 5.4: 使用 common/hash.rs**

将 `make_method_id` 和 `make_class_id` 替换为使用 `crate::common::hash::sha1_hex`：

```rust
fn make_method_id(project: &str, path: &str, class_name: &str, name: &str) -> String {
    crate::common::hash::sha1_hex(&format!("{}::{}::{}::{}", project, path, class_name, name))
}
fn make_class_id(project: &str, path: &str, name: &str) -> String {
    crate::common::hash::sha1_hex(&format!("{}::{}::class::{}", project, path, name))
}
```

- [ ] **Step 5.5: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

---

### Task 6: index/convert.rs — 消除重复转换代码 (H7)

**Files:**
- Create: `engine-rust/src/index/convert.rs`
- Create: `engine-rust/src/index/mod.rs`

- [ ] **Step 6.1: 写入 index/mod.rs**

```rust
pub mod convert;
pub mod build;
pub mod full;
pub mod update;
pub mod remove;
pub mod callgraph;

pub use build::init_sqlite;
```

- [ ] **Step 6.2: 写入 index/convert.rs**

```rust
use std::collections::HashMap;
use serde_json::json;

use crate::models::MethodBlock;
use crate::client::neo4j::MethodNode;
use crate::common::hash::method_id_to_u64;

impl From<&MethodBlock> for MethodNode {
    fn from(m: &MethodBlock) -> Self {
        MethodNode {
            method_id: m.method_id.clone(),
            project: m.project.clone(),
            file_path: m.file_path.clone(),
            language: m.language.clone(),
            package_or_module: m.package_or_module.clone(),
            class_name: m.class_name.clone(),
            name: m.name.clone(),
            signature: m.signature.clone(),
            params: m.params.join(", "),
            return_type: m.return_type.clone(),
            start_line: m.start_line as i64,
            end_line: m.end_line as i64,
            calls: m.calls.clone(),
        }
    }
}

pub fn build_payload(m: &MethodBlock) -> HashMap<String, serde_json::Value> {
    let mut p = HashMap::new();
    p.insert("method_id".into(), json!(m.method_id));
    p.insert("project".into(), json!(m.project));
    p.insert("file_path".into(), json!(m.file_path));
    p.insert("language".into(), json!(m.language));
    p.insert("package_or_module".into(), json!(m.package_or_module));
    p.insert("class_name".into(), json!(m.class_name));
    p.insert("name".into(), json!(m.name));
    p.insert("signature".into(), json!(m.signature));
    p.insert("params".into(), json!(m.params.join(", ")));
    p.insert("return_type".into(), json!(m.return_type));
    p.insert("start_line".into(), json!(m.start_line));
    p.insert("end_line".into(), json!(m.end_line));
    p.insert("comment".into(), json!(m.comment));
    p.insert("search_text".into(), json!(m.search_text));
    p.insert("source_code".into(), json!(m.source_code));
    p.insert("calls".into(), json!(m.calls));
    p
}

pub fn build_qdrant_point(m: &MethodBlock, vector: &[f32]) -> (serde_json::Value, Vec<f32>, HashMap<String, serde_json::Value>) {
    let point_id = json!(method_id_to_u64(&m.method_id));
    (point_id, vector.to_vec(), build_payload(m))
}

pub fn split_class_path(class_name: &str, file_path: &str) -> (String, String) {
    let pkg = std::path::Path::new(file_path).parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace('/', ".")
        .to_string();
    (pkg, class_name.to_string())
}
```

- [ ] **Step 6.3: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

Expected: 编译通过

---

### Task 7: index/ — 索引层重构 (H4 callgraph, 拆分 build.rs)

**Files:**
- Create: `engine-rust/src/index/build.rs`
- Create: `engine-rust/src/index/full.rs`
- Create: `engine-rust/src/index/update.rs`
- Create: `engine-rust/src/index/remove.rs`
- Create: `engine-rust/src/index/callgraph.rs`

- [ ] **Step 7.1: 写入 index/callgraph.rs (H4 — 增量 CALLS)**

```rust
use anyhow::Result;

use crate::client::neo4j;

pub async fn rebuild_calls_for_project(project: &str) -> Result<u64> {
    neo4j::create_call_relationships(project).await
}

pub async fn rebuild_calls_for_files(project: &str, file_paths: &[String]) -> Result<u64> {
    neo4j::create_call_relationships_incremental(project, file_paths).await
}
```

- [ ] **Step 7.2: 写入 index/build.rs (增量构建)**

从原 `build.rs` 复制 `run_build` 函数及辅助函数 (`init_sqlite`, `build_payload`, `method_id_to_u64`, `split_class_path`)，然后做以下修改：

1. `use` 路径全部更新为新的模块路径：
```rust
use crate::client::{neo4j, qdrant, embed};
use crate::index::convert::{build_payload, build_qdrant_point, split_class_path};
use crate::index::callgraph;
use crate::common::hash::method_id_to_u64;
use crate::config;
use crate::scanner;
use crate::parser::Parser;
```

2. 构建 Neo4j nodes 使用 `From` trait：
```rust
let neo4j_nodes: Vec<neo4j::MethodNode> = methods.iter().map(|m| m.into()).collect();
```

3. 构建 Qdrant points 使用辅助函数：
```rust
let qdrant_points: Vec<_> = methods.iter().enumerate()
    .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
    .collect();
```

4. 删除旧数据使用 `warn_on_err!`：
```rust
warn_on_err!(qdrant::delete_points_by_filter(&collection, rel).await, format!("qdrant delete {}", rel));
warn_on_err!(neo4j::delete_methods_by_file(project, rel).await, format!("neo4j delete {}", rel));
```

5. 调用增量 CALLS：
```rust
let rels = callgraph::rebuild_calls_for_files(project, &changed).await?;
```

6. 删除 `method_snapshots` 相关代码（M11）：
- `init_sqlite` 中删除 `CREATE TABLE method_snapshots` 语句

完整 `init_sqlite`:
```rust
pub fn init_sqlite() -> Result<rusqlite::Connection> {
    let path = config::SQLITE_PATH;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let db = rusqlite::Connection::open(path)?;
    db.execute_batch("
        CREATE TABLE IF NOT EXISTS file_snapshots (
            file_path TEXT NOT NULL,
            project TEXT NOT NULL,
            file_sha1 TEXT NOT NULL,
            file_mtime REAL NOT NULL DEFAULT 0,
            method_count INTEGER DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (file_path, project)
        );
    ")?;
    Ok(db)
}
```

- [ ] **Step 7.3: 写入 index/full.rs (全量索引, L18 流式)**

从原 `build.rs` 复制 `run_index` 函数，修改：

1. 改为流式处理：parse → embed → write 交替，删除 `all_methods` 全量收集
2. 使用 `From` trait 和 `build_qdrant_point`
3. 使用 `crate::index::callgraph::rebuild_calls_for_project`

```rust
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::Result;

use crate::config;
use crate::client::{neo4j, qdrant, embed};
use crate::index::convert::{build_payload, build_qdrant_point, split_class_path};
use crate::index::callgraph;
use crate::scanner;
use crate::parser::Parser;

pub async fn run_index(root: &str, project: &str) -> Result<()> {
    let collection = format!("{}_methods", project);

    embed::health().await?;
    neo4j::health().await?;

    println!("[reindex] clearing old data...");
    let _ = qdrant::delete_collection(&collection).await;
    neo4j::delete_all_methods(project).await?;

    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    let files = scanner::collect_files(root);
    println!("[scan] found {} files", files.len());

    let mut p = Parser::new()?;
    let mut total_methods = 0usize;
    let mut all_classes = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let t0 = Instant::now();

    let batch_size = 128usize;
    let mut batch_methods: Vec<crate::models::MethodBlock> = Vec::with_capacity(batch_size);

    for f in &files {
        let fpath = f.to_string_lossy();
        let _content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parsed = match p.parse_file(&fpath, project, root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        all_classes.extend(parsed.classes);

        for m in parsed.methods {
            if seen.insert(m.method_id.clone()) {
                batch_methods.push(m);
                if batch_methods.len() >= batch_size {
                    process_batch(project, &collection, &batch_methods).await?;
                    total_methods += batch_methods.len();
                    batch_methods.clear();
                }
            }
        }
    }
    // Process remaining
    if !batch_methods.is_empty() {
        process_batch(project, &collection, &batch_methods).await?;
        total_methods += batch_methods.len();
    }

    // Class relationships
    if !all_classes.is_empty() {
        let mut class_map: HashMap<String, neo4j::ClassBatchEntry> = HashMap::new();
        for c in &all_classes {
            class_map.entry(c.class_id.clone()).or_insert_with(|| {
                let (pkg, _) = split_class_path(&c.name, &c.file_path);
                neo4j::ClassBatchEntry {
                    class_id: c.class_id.clone(),
                    name: c.name.clone(),
                    project: project.to_string(),
                    file_path: c.file_path.clone(),
                    package_or_module: pkg,
                    method_ids: vec![],
                }
            });
        }
        let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
        neo4j::write_classes_batch(&class_vec).await?;
    }

    let rels = callgraph::rebuild_calls_for_project(project).await?;
    println!("[rels] created {} CALLS relationships", rels);

    // Init SQLite cache
    let db = crate::index::build::init_sqlite()?;
    for f in &files {
        let rel = scanner::rel_path(root, f);
        let content = std::fs::read_to_string(f).unwrap_or_default();
        let hash = crate::common::hash::sha1_hex(&content);
        db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64, 0usize],
        )?;
    }

    println!("[done] {} methods indexed in {:.1}s", total_methods, t0.elapsed().as_secs_f64());
    Ok(())
}

async fn process_batch(
    project: &str,
    collection: &str,
    methods: &[crate::models::MethodBlock],
) -> Result<()> {
    let texts: Vec<String> = methods.iter().map(|m| m.search_text.clone()).collect();
    let vectors = embed::embed_batch(texts).await?;

    let qdrant_points: Vec<_> = methods.iter().enumerate()
        .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
        .collect();

    let neo4j_nodes: Vec<neo4j::MethodNode> = methods.iter().map(|m| m.into()).collect();

    if !qdrant_points.is_empty() {
        qdrant::upsert_points(collection, qdrant_points).await?;
    }
    if !neo4j_nodes.is_empty() {
        neo4j::write_methods_batch(&neo4j_nodes).await?;
    }
    Ok(())
}
```

- [ ] **Step 7.4: 写入 index/update.rs**

从原 `update.rs` 复制，修改为使用 convert trait 和增量 CALLS：

```rust
use std::path::Path;
use anyhow::Result;

use crate::config;
use crate::client::{neo4j, qdrant, embed};
use crate::index::convert::{build_payload, build_qdrant_point, split_class_path};
use crate::index::callgraph;
use crate::scanner;
use crate::parser::Parser;

pub async fn run_update(root: &str, project: &str, file: &str) -> Result<()> {
    let fpath = Path::new(root).join(file);
    if !fpath.exists() {
        eprintln!("[error] file not found: {}", fpath.display());
        return Ok(());
    }
    let content = std::fs::read_to_string(&fpath)?;
    let rel = scanner::rel_path(root, &fpath);
    let collection = format!("{}_methods", project);

    embed::health().await?;
    neo4j::health().await?;
    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    println!("[update] {}: removing old data...", rel);
    warn_on_err!(qdrant::delete_points_by_filter(&collection, &rel).await, format!("qdrant delete {}", rel));
    warn_on_err!(neo4j::delete_methods_by_file(project, &rel).await, format!("neo4j delete {}", rel));

    let mut p = Parser::new()?;
    let parsed = p.parse_file(&fpath.to_string_lossy(), project, root)?;

    if parsed.methods.is_empty() {
        println!("[update] {}: no methods to index", rel);
        let db = crate::index::build::init_sqlite()?;
        let hash = crate::common::hash::sha1_hex(&content);
        db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64],
        )?;
        return Ok(());
    }

    let texts: Vec<String> = parsed.methods.iter().map(|m| m.search_text.clone()).collect();
    println!("[update] {}: embedding {} methods...", rel, texts.len());
    let vectors = embed::embed_batch(texts).await?;

    let qdrant_points: Vec<_> = parsed.methods.iter().enumerate()
        .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
        .collect();
    let neo4j_nodes: Vec<neo4j::MethodNode> = parsed.methods.iter().map(|m| m.into()).collect();

    qdrant::upsert_points(&collection, qdrant_points).await?;
    neo4j::write_methods_batch(&neo4j_nodes).await?;

    // Class relationships
    if !parsed.classes.is_empty() {
        let mut class_map: std::collections::HashMap<String, neo4j::ClassBatchEntry> = std::collections::HashMap::new();
        for c in &parsed.classes {
            class_map.entry(c.class_id.clone()).or_insert_with(|| {
                let (pkg, _) = split_class_path(&c.name, &c.file_path);
                neo4j::ClassBatchEntry {
                    class_id: c.class_id.clone(),
                    name: c.name.clone(),
                    project: project.to_string(),
                    file_path: c.file_path.clone(),
                    package_or_module: pkg,
                    method_ids: parsed.methods.iter()
                        .filter(|m| m.class_name == c.name)
                        .map(|m| m.method_id.clone())
                        .collect(),
                }
            });
        }
        let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
        neo4j::write_classes_batch(&class_vec).await?;
    }

    // Incremental CALLS
    let rels = callgraph::rebuild_calls_for_files(project, &[rel.clone()]).await?;
    println!("[rels] created {} CALLS relationships", rels);

    // Update SQLite
    let db = crate::index::build::init_sqlite()?;
    let hash = crate::common::hash::sha1_hex(&content);
    db.execute(
        "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64, parsed.methods.len()],
    )?;

    println!("[done] {} updated ({} methods)", rel, parsed.methods.len());
    Ok(())
}
```

- [ ] **Step 7.5: 写入 index/remove.rs**

从原 `remove.rs` 复制，修改 use 路径：

```rust
use anyhow::Result;
use crate::client::{neo4j, qdrant};

pub async fn run_remove(project: &str, file: Option<&str>, all: bool) -> Result<()> {
    let collection = format!("{}_methods", project);

    if all {
        println!("[remove] clearing all data for project {}...", project);
        warn_on_err!(qdrant::delete_collection(&collection).await, "qdrant delete_collection");
        neo4j::delete_all_methods(project).await?;
        println!("[done] project {} data removed", project);
        return Ok(());
    }

    if let Some(file_path) = file {
        println!("[remove] removing file {}...", file_path);
        warn_on_err!(qdrant::delete_points_by_filter(&collection, file_path).await, format!("qdrant delete {}", file_path));
        neo4j::delete_methods_by_file(project, file_path).await?;
        println!("[done] removed methods for file {}", file_path);
        return Ok(());
    }

    eprintln!("[error] specify --file <path> or --all");
    Ok(())
}
```

- [ ] **Step 7.6: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

Expected: 编译通过（main.rs 还需要修复 use 路径，下个 Task）

---

### Task 8: main.rs — 更新 CLI 入口 + 所有根模块 use 路径

**Files:**
- Modify: `engine-rust/src/main.rs`
- Modify: `engine-rust/src/search.rs`
- Modify: `engine-rust/src/health.rs`
- Modify: `engine-rust/src/validate.rs`
- Modify: `engine-rust/src/event.rs`
- Modify: `engine-rust/src/knowledge.rs`

- [ ] **Step 8.1: 更新 main.rs 模块声明**

```rust
mod config;
mod models;
mod parser;
mod scanner;
mod event;
mod knowledge;
mod search;
mod health;
mod validate;
mod common;
mod client;
mod index;
mod sync;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dt", version = "3.1.0", about = "Digital Twin CLI - knowledge graph & vector index management")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Incremental build: scan project, hash-compare files, index changes only
    Build {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
    },
    /// Index a single file (for instant incremental update)
    Update {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        file: String,
    },
    /// Full rebuild of a project
    Index {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
    },
    /// Remove methods for a file or entire project from Neo4j + Qdrant
    Remove {
        #[arg(long)]
        project: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = "false")]
        all: bool,
    },
    /// Write an Event node to Neo4j
    Event {
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        entity_id: String,
        #[arg(long)]
        entity_type: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        details: Option<String>,
    },
    /// Write a Knowledge node to Neo4j
    Memorize {
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        entity_id: String,
        #[arg(long)]
        entity_type: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        details: Option<String>,
    },
    /// Semantic code search
    Search {
        query: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long, default_value = "false")]
        all: bool,
        #[arg(long, default_value = "false")]
        json: bool,
    },
    /// Check connectivity of all backend services
    Health,
    /// Build Neo4j CALLS relationships
    BuildCallGraph {
        #[arg(long)]
        name: String,
    },
    /// Validate extraction quality (dry-run)
    Validate {
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: String,
    },
    /// Sync Nacos configurations into the knowledge graph
    NacosSync {
        #[arg(long, default_value = "all")]
        env: String,
    },
    /// Sync Kubernetes resources into the knowledge graph
    K8sSync {
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Parse a single file to JSON (no DB writes)
    Parse {
        #[arg(long)]
        file: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        root: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { path, name } => {
            index::build::run_build(&path, &name).await?;
        }
        Commands::Update { path, name, file } => {
            index::update::run_update(&path, &name, &file).await?;
        }
        Commands::Index { path, name } => {
            index::full::run_index(&path, &name).await?;
        }
        Commands::Remove { project, file, all } => {
            index::remove::run_remove(&project, file.as_deref(), all).await?;
        }
        Commands::Event { r#type, entity_id, entity_type, project, details } => {
            event::write_event(&r#type, &entity_id, entity_type.as_deref(), project.as_deref(), details.as_deref()).await?;
        }
        Commands::Memorize { r#type, entity_id, entity_type, project, details } => {
            knowledge::write_knowledge(&r#type, &entity_id, entity_type.as_deref(), project.as_deref(), details.as_deref()).await?;
        }
        Commands::Search { query, project, limit, all, json } => {
            search::run_search(&query, project.as_deref(), limit, all, json).await?;
        }
        Commands::NacosSync { env } => {
            sync::nacos::run_sync(&env).await?;
        }
        Commands::K8sSync { limit } => {
            sync::k8s::run_sync(limit).await?;
        }
        Commands::Health => {
            health::run_health().await?;
        }
        Commands::BuildCallGraph { name } => {
            client::neo4j::ensure_schema().await?;
            let count = client::neo4j::create_call_relationships(&name).await?;
            println!("[done] created {} CALLS relationships for project {}", count, name);
        }
        Commands::Validate { path, name } => {
            validate::run_validate(&path, &name).await?;
        }
        Commands::Parse { file, project, root } => {
            let mut p = parser::Parser::new()?;
            let parsed = p.parse_file(&file, &project, &root)?;
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "methods": parsed.methods.iter().map(|m| serde_json::json!({
                    "method_id": m.method_id, "name": m.name, "class_name": m.class_name,
                    "signature": m.signature, "start_line": m.start_line,
                    "end_line": m.end_line, "language": m.language, "calls": m.calls,
                })).collect::<Vec<_>>(),
                "classes": parsed.classes.iter().map(|c| serde_json::json!({
                    "class_id": c.class_id, "name": c.name,
                    "kind": format!("{:?}", c.kind),
                })).collect::<Vec<_>>(),
            }))?);
        }
    }
    Ok(())
}
```

- [ ] **Step 8.2: 更新 search.rs use 路径**

```rust
use crate::client::{embed, qdrant, neo4j};
use crate::config;
```

增加 L17 — call chain 展开：

在 `enrich_results` 后增加 callers/callees 查询（仅当搜索结果是单个 method 且有 method_id 时）：

```rust
async fn enrich_results(results: Vec<qdrant::SearchResult>) -> Result<Vec<SearchResultItem>> {
    let cfg = config::load();
    let neo4j_available = neo4j::health().await.is_ok();

    let items: Vec<SearchResultItem> = results.into_iter().map(|r| {
        let p = &r.payload;
        SearchResultItem {
            method_id: p.get("method_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            signature: p.get("signature").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            file_path: p.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            start_line: p.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0),
            end_line: p.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0),
            language: p.get("language").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            project: p.get("project").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            score: r.score,
            source_code: p.get("source_code").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            comment: p.get("comment").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            class_name: p.get("class_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            calls: p.get("calls").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }
    }).collect();
    Ok(items)
}
```

`print_results` 中增加 call chain 输出：
```rust
if !r.calls.is_empty() {
    println!("    calls: {}", r.calls.join(", "));
}
```

- [ ] **Step 8.3: 更新 health.rs**

```rust
use anyhow::Result;
use crate::client::{neo4j, embed};
use crate::config;
// check_qdrant 函数中 client 用 crate::client::get_client()
```

- [ ] **Step 8.4: 更新 event.rs / knowledge.rs / validate.rs**

只需修改 `use crate::neo4j` → `use crate::client::neo4j`

- [ ] **Step 8.5: 写入 sync/mod.rs, sync/nacos.rs, sync/k8s.rs**

sync/mod.rs:
```rust
pub mod nacos;
pub mod k8s;
```

sync/nacos.rs: 从原 `nacos_sync.rs` 复制，修改 `use crate::neo4j` → `use crate::client::neo4j`

sync/k8s.rs: 从原 `k8s_sync.rs` 复制，修改 `use crate::neo4j` → `use crate::client::neo4j`

- [ ] **Step 8.6: 删除旧文件**

```bash
rm engine-rust/src/neo4j.rs
rm engine-rust/src/qdrant.rs
rm engine-rust/src/embed.rs
rm engine-rust/src/build.rs
rm engine-rust/src/update.rs
rm engine-rust/src/remove.rs
rm engine-rust/src/nacos_sync.rs
rm engine-rust/src/k8s_sync.rs
```

- [ ] **Step 8.7: 编译验证**

```bash
cd engine-rust && cargo check 2>&1
```

Expected: 编译通过，0 errors

---

### Task 9: 集成测试 (M13)

**Files:**
- Create: `engine-rust/tests/parser_test.rs`
- Create: `engine-rust/tests/convert_test.rs`

- [ ] **Step 9.1: 写入 tests/parser_test.rs**

测试 parser 的：行号修复(H5)、class_name 填充(H6)、语言检测(H15)

```rust
use std::fs;
use tempfile::TempDir;

// We use dt's parser module directly
// Note: since dt is a binary crate, we need to add a lib.rs to access internal modules
// For now, test via the Parse CLI subcommand
```

由于 dt 是 binary crate（没有 `lib.rs`），测试需要通过 CLI 集成测试。先创建 `engine-rust/src/lib.rs` 使内部模块可被测试访问：

```rust
// lib.rs — re-exports for integration tests
pub mod config;
pub mod models;
pub mod parser;
pub mod scanner;
pub mod common;
pub mod client;
pub mod index;
pub mod sync;
pub mod event;
pub mod knowledge;
pub mod search;
pub mod health;
pub mod validate;
```

然后写入 parser 测试：

```rust
use dt::parser::Parser;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_parse_rust_function_line_numbers() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(&file_path, "\n\nfn hello() {\n    println!(\"hi\");\n}\n").unwrap();

    let mut p = Parser::new().unwrap();
    let result = p.parse_file(
        &file_path.to_string_lossy(),
        "testproj",
        &dir.path().to_string_lossy(),
    ).unwrap();

    assert_eq!(result.methods.len(), 1);
    let m = &result.methods[0];
    assert_eq!(m.start_line, 3, "start_line should be 3 (0-indexed row + 1)");
    assert_eq!(m.end_line, 5, "end_line should be 5");
    assert_eq!(m.name, "hello");
}

#[test]
fn test_unsupported_language_returns_empty() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.c");
    fs::write(&file_path, "int main() { return 0; }").unwrap();

    let mut p = Parser::new().unwrap();
    let result = p.parse_file(
        &file_path.to_string_lossy(),
        "testproj",
        &dir.path().to_string_lossy(),
    ).unwrap();

    assert!(result.methods.is_empty(), "unsupported language should return empty");
    assert!(result.classes.is_empty());
}

#[test]
fn test_class_name_is_populated() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, "class MyClass:\n    def my_method(self):\n        pass\n").unwrap();

    let mut p = Parser::new().unwrap();
    let result = p.parse_file(
        &file_path.to_string_lossy(),
        "testproj",
        &dir.path().to_string_lossy(),
    ).unwrap();

    for m in &result.methods {
        if m.name == "my_method" {
            assert_eq!(m.class_name, "MyClass", "class_name should be populated");
        }
    }
}

#[test]
fn test_tempdir_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let file_path = path.join("test.rs");
    fs::write(&file_path, "fn foo() {}").unwrap();

    let mut p = Parser::new().unwrap();
    let _ = p.parse_file(&file_path.to_string_lossy(), "test", &path.to_string_lossy());

    drop(dir);
    // TempDir::drop 自动清理
    assert!(!path.exists(), "temp dir should be cleaned up after drop");
}
```

- [ ] **Step 9.2: 写入 tests/convert_test.rs**

```rust
use dt::models::{MethodBlock, EntityKind, ClassBlock};
use dt::client::neo4j::MethodNode;
use dt::index::convert::build_payload;

#[test]
fn test_methodblock_to_methodnode() {
    let mb = MethodBlock {
        method_id: "abc123".into(),
        project: "test".into(),
        file_path: "src/main.rs".into(),
        language: "rust".into(),
        package_or_module: "".into(),
        class_name: "".into(),
        name: "main".into(),
        signature: "fn main()".into(),
        params: vec![],
        return_type: "".into(),
        source_code: "fn main() {}".into(),
        search_text: "search".into(),
        summary: "".into(),
        start_line: 1,
        end_line: 1,
        comment: "".into(),
        calls: vec!["println".into()],
    };

    let node: MethodNode = (&mb).into();
    assert_eq!(node.method_id, "abc123");
    assert_eq!(node.name, "main");
    assert_eq!(node.project, "test");
    assert_eq!(node.start_line, 1);
    assert_eq!(node.calls, vec!["println"]);
    assert_eq!(node.params, "");
}

#[test]
fn test_build_payload_has_all_keys() {
    let mb = MethodBlock {
        method_id: "abc123".into(),
        project: "test".into(),
        file_path: "src/main.rs".into(),
        language: "rust".into(),
        package_or_module: "".into(),
        class_name: "".into(),
        name: "main".into(),
        signature: "fn main()".into(),
        params: vec![],
        return_type: "".into(),
        source_code: "fn main() {}".into(),
        search_text: "search text".into(),
        summary: "".into(),
        start_line: 1,
        end_line: 3,
        comment: "// comment".into(),
        calls: vec!["println".into()],
    };

    let payload = build_payload(&mb);
    assert_eq!(payload.get("method_id").unwrap().as_str().unwrap(), "abc123");
    assert_eq!(payload.get("project").unwrap().as_str().unwrap(), "test");
    assert_eq!(payload.get("file_path").unwrap().as_str().unwrap(), "src/main.rs");
    assert_eq!(payload.get("start_line").unwrap().as_u64().unwrap(), 1);
    assert!(payload.get("search_text").unwrap().as_str().unwrap().contains("search"));
}
```

- [ ] **Step 9.3: 运行测试**

```bash
cd engine-rust && cargo test 2>&1
```

Expected: 全部测试通过。tempfile 自动清理。

---

### Task 10: Python 服务修复 (H2, H3, M12, L19)

**Files:**
- Create: `services/search-web/lazy_consistency.py`
- Modify: `services/search-web/app.py`
- Modify: `services/embed-server/requirements.txt`

- [ ] **Step 10.1: 写入 lazy_consistency.py (H2)**

```python
"""
Lazy Consistency Checker for Digital Twin search-web.
Performs best-effort consistency verification between Neo4j and Qdrant,
and discovers unindexed files.
"""

import os
import json
import subprocess
from typing import List, Tuple, Optional

import requests

NEO4J_URL = os.environ.get("NEO4J_URL", "http://localhost:7474/db/neo4j/tx/commit")
NEO4J_AUTH = os.environ.get("NEO4J_AUTH", "")


def _neo4j_query(cypher: str, params: dict = None) -> list:
    """Run a Cypher query and return rows."""
    if not NEO4J_AUTH:
        return []
    try:
        r = requests.post(
            NEO4J_URL,
            json={"statements": [{"statement": cypher, "parameters": params or {}}]},
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Basic {NEO4J_AUTH}",
            },
            timeout=10,
        )
        if r.status_code != 200:
            return []
        data = r.json()
        results = []
        for row in data.get("results", []):
            for entry in row.get("data", []):
                results.append(entry["row"])
        return results
    except Exception:
        return []


class ConsistencyChecker:
    """Check consistency between Neo4j and Qdrant, discover new files."""

    def _resolve_project_root(self, project: str) -> Optional[str]:
        """Get project root path from Neo4j."""
        rows = _neo4j_query(
            "MATCH (p:Project {name: $name}) RETURN p.path",
            {"name": project},
        )
        if rows and rows[0]:
            return rows[0][0]
        return None

    def verify_and_repair(self, vector: list, results: list) -> Tuple[list, dict]:
        """Verify results and return (results, stats).
        
        Checks if returned methods exist in Neo4j and marks stale entries.
        """
        stats = {"dirty_files": 0, "verified": 0, "stale": 0}
        if not results or not NEO4J_AUTH:
            return results, stats

        method_ids = [
            r.get("payload", {}).get("method_id", "").strip('"')
            for r in results
        ]
        method_ids = [m for m in method_ids if m]

        if not method_ids:
            return results, stats

        # Verify methods exist in Neo4j
        verified_rows = _neo4j_query(
            "MATCH (m:Method) WHERE m.method_id IN $ids RETURN m.method_id, m.file_path",
            {"ids": method_ids},
        )
        verified_ids = {row[0] for row in verified_rows}

        for r in results:
            mid = r.get("payload", {}).get("method_id", "").strip('"')
            if mid and mid in verified_ids:
                stats["verified"] += 1
            elif mid:
                stats["stale"] += 1

        return results, stats

    def discover_new_files(self, project: str) -> List[Tuple[str, str, str]]:
        """Discover files that exist on disk but are not indexed.
        
        Returns list of (project_name, file_path, reason).
        """
        root = self._resolve_project_root(project)
        if not root or not os.path.isdir(root):
            return []

        # Use dt validate as dry-run to discover unindexed files
        new_files = []
        try:
            result = subprocess.run(
                ["dt", "validate", "--path", root, "--name", project],
                capture_output=True, text=True, timeout=60,
            )
            # parse output for files with errors
            for line in result.stdout.splitlines():
                if "skip" in line.lower() or "error" in line.lower():
                    parts = line.split()
                    for part in parts:
                        if os.path.exists(os.path.join(root, part)):
                            new_files.append((project, part, "unindexed"))
        except Exception:
            pass

        return new_files
```

- [ ] **Step 10.2: 修复 search-web/app.py (H3, M12)**

1. 删除第 33 和 117 行的硬编码 fallback：
```python
# 第 33 行，原为：
auth = os.environ.get("NEO4J_AUTH", "bmVvNGo6bmVvNGo=")
# 改为：
auth = os.environ.get("NEO4J_AUTH", "")
if not auth:
    return []  # no auth configured, skip Neo4j queries
```

2. 修复域搜索 (M12)：
```python
# 第 153-161 行 search_all 函数，删除硬编码集合名
def search_all(vector: list, limit: int, domain: str = "code"):
    """跨项目搜索指定域"""
    if domain == "code":
        return search_code_all(vector, limit)
    # document and environment collections not yet implemented
    print(f"[warn] domain '{domain}' not yet implemented, returning empty")
    return []
```

- [ ] **Step 10.3: 锁定 Python 依赖 (L19)**

```txt
fastapi==0.115.6
uvicorn[standard]==0.34.0
sentence-transformers==3.3.1
```

- [ ] **Step 10.4: 验证 Python import**

```bash
cd services/search-web && python3 -c "from lazy_consistency import ConsistencyChecker; print('OK')" 2>&1
```

Expected: `OK`

---

### Task 11: dt-sync 凭证安全 (H3, M16) + config.yaml.example (L20)

**Files:**
- Modify: `dt-sync`
- Modify: `config.yaml.example`

- [ ] **Step 11.1: 修复 dt-sync 凭证**

删除硬编码的 `bmVvNGo6bmVvNGo=`，改为从 config.yaml 读取：

```bash
#!/bin/bash
# digital-twin hook — updated to use dt CLI
# Usage: dt-sync [--path <path> --name <name>]
set -e
GREEN='\033[0;32m'; NC='\033[0m'
log() { echo -e "${GREEN}[dt]${NC} $1"; }

# Read Neo4j auth from config.yaml
CONFIG_FILE="${DT_CONFIG:-./config.yaml}"
NEO4J_USER=$(python3 -c "import yaml; c=yaml.safe_load(open('$CONFIG_FILE')); print(c.get('services',{}).get('neo4j',{}).get('user','neo4j'))" 2>/dev/null || echo "neo4j")
NEO4J_PASS=$(python3 -c "import yaml; c=yaml.safe_load(open('$CONFIG_FILE')); print(c.get('services',{}).get('neo4j',{}).get('password',''))" 2>/dev/null || echo "")
NEO4J_URL=$(python3 -c "import yaml; c=yaml.safe_load(open('$CONFIG_FILE')); print(c.get('services',{}).get('neo4j',{}).get('url','http://localhost:7474'))" 2>/dev/null || echo "http://localhost:7474")
NEO4J_AUTH=$(echo -n "${NEO4J_USER}:${NEO4J_PASS}" | base64 -w0)

MINUTES=30
FILE=""
PROJECT=""
ALL=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --all) ALL=true; shift ;;
    --file) FILE="$2"; shift 2 ;;
    --project) PROJECT="$2"; shift 2 ;;
    --minutes) MINUTES="$2"; shift 2 ;;
    *) shift ;;
  esac
done

if [ -n "$FILE" ] && [ -n "$PROJECT" ]; then
  log "Update: $PROJECT :: $FILE"
  dt update --path "$(pwd)" --name "$PROJECT" --file "$FILE"
  exit 0
fi

if $ALL; then
  log "Full rebuild all projects..."
  python3 -c "
import json, urllib.request
r = urllib.request.urlopen('${NEO4J_URL}/db/neo4j/tx/commit',
    data=json.dumps({'statements':[{'statement':'MATCH (p:Project) RETURN p.name, p.path ORDER BY p.name'}]}).encode(),
    headers={'Content-Type':'application/json','Authorization':'Basic ${NEO4J_AUTH}'})
for row in json.loads(r.read())['results'][0]['data']:
    print(f\"{row['row'][0]}|{row['row'][1]}\")
" | while IFS='|' read -r pname ppath; do
    log "  Building $pname..."
    dt build --path "$ppath" --name "$pname"
  done
  exit 0
fi

# Incremental mode
log "Scanning modified files in the last ${MINUTES} minutes..."
python3 -c "
import json, urllib.request
r = urllib.request.urlopen('${NEO4J_URL}/db/neo4j/tx/commit',
    data=json.dumps({'statements':[{'statement':'MATCH (p:Project) RETURN p.name, p.path'}]}).encode(),
    headers={'Content-Type':'application/json','Authorization':'Basic ${NEO4J_AUTH}'})
rows = json.loads(r.read())['results'][0]['data']
for row in rows:
    print(f\"{row['row'][0]}|{row['row'][1]}\")
" | while IFS='|' read -r pname ppath; do
  [ -z "$ppath" ] && continue
  [ ! -d "$ppath" ] && continue
  log "  Building $pname..."
  dt build --path "$ppath" --name "$pname"
done

echo $(date -u +%Y-%m-%dT%H:%M:%SZ) > /var/lib/digital-twin/last-sync 2>/dev/null || true
log "Done."
```

- [ ] **Step 11.2: 更新 config.yaml.example (L20)**

```yaml
# Digital Twin Configuration
# Copy this to config.yaml and customize

server:
  hostname: my-server

services:
  embed_server:
    url: http://localhost:8001
    dim: 768
    model: BAAI/bge-base-zh-v1.5
  neo4j:
    url: http://localhost:7474
    user: neo4j
    password: changeme
  qdrant:
    url: http://localhost:6333
  # Optional: Kubernetes sync via Kuboard API
  # k8s:
  #   server: http://10.10.2.100:20080
  #   username: admin
  #   password: changeme
  #   cluster_id: cluster-id
  #   skip_tls_verify: false
  # Optional: Nacos config sync
  # nacos:
  #   test: http://nacos-test.example.com
  #   prod: http://nacos.example.com

snapshot_dir: /var/lib/digital-twin/snapshots

# Optional: register projects for dt-sync incremental mode
# projects:
#   - name: my-project
#     path: /path/to/project
#     stack: Python/TypeScript

# Optional: document directories to index
# document_dirs:
#   - /path/to/docs

# Optional: watcher configuration
# watcher:
#   debounce_seconds: 30
#   watch_extensions:
#     - .py
#     - .ts
#     - .tsx
#     - .js
#     - .jsx
#     - .java
#     - .go
#     - .rs
#     - .php
#   ignore_dirs:
#     - node_modules
#     - .git
#     - target
#     - __pycache__
```

---

### Task 12: 编译 + 全量测试 + 清理

- [ ] **Step 12.1: 编译 release**

```bash
cd engine-rust && cargo build --release 2>&1
```

Expected: 0 errors, 0 warnings

- [ ] **Step 12.2: 运行全部测试**

```bash
cd engine-rust && cargo test 2>&1
```

Expected: 全部测试通过

- [ ] **Step 12.3: 编译检查（clippy 无严重警告）**

```bash
cd engine-rust && cargo clippy -- -D warnings 2>&1 || true
```

- [ ] **Step 12.4: 冒烟验证**

```bash
dt health 2>&1
```

Expected: 三个服务状态输出

- [ ] **Step 12.5: 确认无残留测试数据**

```bash
# 测试用 tempfile 自动清理，手动确认
ls /tmp/.tmp* 2>/dev/null || echo "no temp files left"
```

---

## 依赖关系总览

```
Task 0 (准备)
  ├── Task 1 (common)
  │     ├── Task 2 (Cargo.toml)
  │     ├── Task 3 (client/)
  │     │     ├── Task 4 (config)
  │     │     ├── Task 5 (parser)
  │     │     │     └── Task 6 (index/convert)
  │     │     │           └── Task 7 (index/*)
  │     │     │                 └── Task 8 (main.rs + all use paths)
  │     │     │                       ├── Task 9 (tests)
  │     │     │                       ├── Task 10 (Python)
  │     │     │                       ├── Task 11 (shell + config.example)
  │     │     │                       │     └── Task 12 (compile + verify)
```

可并行的任务组：
- Group A: Task 1 → Task 2, Task 3, Task 4 (可并行)
- Group B: Task 5 → Task 6 → Task 7 → Task 8 (串行依赖)
- Group C: Task 9, Task 10, Task 11 (可并行，依赖 Task 8)
- Group D: Task 12 (最终验证，依赖所有)

---

## 执行顺序建议

```
Phase 1: 基础设施  (Task 0, 1)       — 目录 + common 模块
Phase 2: 客户端层  (Task 2, 3, 4)    — Cargo + client + config (可并行)
Phase 3: 核心逻辑  (Task 5, 6, 7, 8) — parser → convert → index → main
Phase 4: 测试+服务 (Task 9, 10, 11)  — tests + Python + shell (可并行)
Phase 5: 验证     (Task 12)          — compile + test + smoke
```
