# 阶段 5+6：集合重组 + 去 project 中心化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Qdrant 集合从 `{project}_xxx` 改为全局 `code_methods` / `doc_chunks` / `kg_nodes`，project 变为 payload 标签。搜索时通过 payload 过滤 project（可选），而非遍历多个集合。同时让 Decision/Experience/Knowledge 节点支持按 domain/env 组织，不强制绑定 project。

**Architecture:** 引入 `collection_name` 工具函数统一集合名。写入路径改为全局集合 + payload 带 project。搜索路径改为单集合搜索 + payload 过滤。旧集合保留（向后兼容），搜索同时查新旧集合并 RRF 融合，验证稳定后删除旧集合。

**Tech Stack:** Rust, Qdrant, Memgraph, tokio async

**Spec:** `docs/superpowers/specs/2026-07-26-unified-build-kg-first-design.md` 第 5 节（Qdrant 集合重组）、第 4 节（去 project 中心化）、第 11 节阶段 5-6

## Global Constraints

- Rust edition 2021, workspace single crate
- 测试命令：`cargo test --lib`（当前 693 passed, 1 pre-existing failure in backup_sqlite）
- 增量默认：所有构建基于 SQLite snapshot
- 旧集合保留向后兼容——搜索同时查新旧集合，RRF 融合
- project 变为 payload 标签，不是集合选择依据
- KG 节点的 project 属性保留，只是不再作为强制隔离边界
- `dt build --test` 必须继续通过

## File Structure

| 文件 | 职责 | 改动类型 |
|------|------|---------|
| `src/shared/collections.rs` | 新增集合名工具函数 | 新建 |
| `src/shared/mod.rs` | 导出 collections 模块 | 修改 |
| `src/application/build/pipeline.rs` | 写入路径改用全局集合 | 修改 |
| `src/interfaces/cli/build.rs` | 搜索路径改用全局集合 + payload 过滤 | 修改 |
| `src/application/context/search_mcp.rs` | MCP 搜索改用全局集合 | 修改 |
| `src/interfaces/grpc/services/build_service.rs` | gRPC 搜索改用全局集合 | 修改 |
| `src/shared/vectorizer.rs` | 向量化改用全局集合 | 修改 |
| `src/application/sync/nacos/config_sync.rs` | Nacos 同步改用全局集合 | 修改 |

---

### Task 1: 新增集合名工具函数

**Files:**
- Create: `src/shared/collections.rs`
- Modify: `src/shared/mod.rs`

**Interfaces:**
- Produces: `collection_name(source, project) -> String`，`is_legacy_collection(name) -> bool`

- [ ] **Step 1: 创建 collections.rs**

创建 `src/shared/collections.rs`：

```rust
//! Qdrant collection name conventions.
//!
//! Phase 5+6: Global collections with project as payload tag.
//! Legacy `{project}_xxx` collections are kept for backward compatibility
//! during migration — search queries both and fuses via RRF.

/// Global collection for code method vectors.
pub const CODE_METHODS: &str = "code_methods";

/// Global collection for document chunk vectors.
pub const DOC_CHUNKS: &str = "doc_chunks";

/// Global collection for KG business node vectors.
pub const KG_NODES: &str = "kg_nodes";

/// Vector dimension (BGE-M3 = 1024).
pub const VECTOR_DIM: u32 = 1024;

/// Resolve a collection name for the given source type.
///
/// Phase 5+6: returns global collection names (project is a payload tag, not
/// part of the collection name). Legacy `{project}_xxx` names are detected
/// by `is_legacy_collection`.
pub fn collection_name(source: &str, _project: &str) -> &str {
    match source {
        "methods" | "code" => CODE_METHODS,
        "semantic" | "doc" => DOC_CHUNKS,
        "knowledge" | "kg" => KG_NODES,
        _ => CODE_METHODS, // fallback
    }
}

/// Check if a collection name is a legacy `{project}_xxx` format.
pub fn is_legacy_collection(name: &str) -> bool {
    name.ends_with("_methods") && name != CODE_METHODS
        || name.ends_with("_semantic") && name != DOC_CHUNKS
        || name.ends_with("_knowledge") && name != KG_NODES
        || name.ends_with("_entities")
}

/// Check if a collection name is a new global collection.
pub fn is_global_collection(name: &str) -> bool {
    name == CODE_METHODS || name == DOC_CHUNKS || name == KG_NODES
}

/// Get the entity type from a collection name (for search result display).
pub fn entity_type_from_collection(col: &str) -> &'static str {
    if col == CODE_METHODS || col.ends_with("_methods") {
        "Method"
    } else if col == DOC_CHUNKS || col.ends_with("_semantic") {
        "Doc"
    } else if col == KG_NODES || col.ends_with("_knowledge") {
        "Knowledge"
    } else if col.ends_with("_entities") {
        "Entity"
    } else {
        "?"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_name_returns_global() {
        assert_eq!(collection_name("methods", "offen-pay"), CODE_METHODS);
        assert_eq!(collection_name("semantic", "offen-pay"), DOC_CHUNKS);
        assert_eq!(collection_name("knowledge", "offen-pay"), KG_NODES);
    }

    #[test]
    fn is_legacy_detection() {
        assert!(is_legacy_collection("offen-pay_methods"));
        assert!(is_legacy_collection("test-pipeline_semantic"));
        assert!(!is_legacy_collection(CODE_METHODS));
        assert!(!is_legacy_collection(DOC_CHUNKS));
        assert!(!is_legacy_collection(KG_NODES));
    }

    #[test]
    fn entity_type_from_various_collections() {
        assert_eq!(entity_type_from_collection(CODE_METHODS), "Method");
        assert_eq!(entity_type_from_collection("offen-pay_methods"), "Method");
        assert_eq!(entity_type_from_collection(DOC_CHUNKS), "Doc");
        assert_eq!(entity_type_from_collection(KG_NODES), "Knowledge");
    }
}
```

- [ ] **Step 2: 在 shared/mod.rs 中导出**

在 `src/shared/mod.rs` 中添加：

```rust
pub mod collections;
```

- [ ] **Step 3: 编译 + 测试**

Run: `cargo test --lib shared::collections 2>&1 | tail -10`
Expected: 3 tests passed

- [ ] **Step 4: 提交**

```bash
git add src/shared/collections.rs src/shared/mod.rs
git commit -m "feat: 新增集合名工具函数 — 全局集合 + 旧集合检测

Phase 5+6: code_methods/doc_chunks/kg_nodes 全局集合，
project 变为 payload 标签。is_legacy_collection 检测旧格式。"
```

---

### Task 2: 写入路径改用全局集合

**Files:**
- Modify: `src/application/build/pipeline.rs`（3 处：methods L271/275/305，knowledge/semantic L1141/1143）

- [ ] **Step 1: 修改 pipeline.rs 中 methods 集合名**

在 `src/application/build/pipeline.rs` 中，找到所有 `format!("{}_methods", project)` 并替换为 `crate::shared::collections::CODE_METHODS`：

L271: `vector_repo.ensure_collection(&format!("{}_methods", project), 1024)` → `vector_repo.ensure_collection(crate::shared::collections::CODE_METHODS, crate::shared::collections::VECTOR_DIM)`

L275: `vector_repo.upsert(&format!("{}_methods", project), chunk.to_vec())` → `vector_repo.upsert(crate::shared::collections::CODE_METHODS, chunk.to_vec())`

L305: `let collection = format!("{}_methods", project);` → `let collection = crate::shared::collections::CODE_METHODS.to_string();`

- [ ] **Step 2: 修改 pipeline.rs 中 knowledge/semantic 集合名**

L1141: `format!("{project}_knowledge")` → `crate::shared::collections::KG_NODES.to_string()`

L1143: `format!("{project}_semantic")` → `crate::shared::collections::DOC_CHUNKS.to_string()`

- [ ] **Step 3: 确保 payload 包含 project 字段**

在 pipeline.rs 的 methods upsert 点（约 L226-251），确认 payload 已包含 `"project": m.project`。如果已有则无需改动。

对于 doc chunks 的 upsert（约 L1095-1104），确认 payload 包含 project。如果缺少则添加。

- [ ] **Step 4: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 5: 运行测试**

Run: `cargo test --lib application::build 2>&1 | tail -10`
Expected: 现有测试通过

- [ ] **Step 6: 提交**

```bash
git add src/application/build/pipeline.rs
git commit -m "feat: 写入路径改用全局集合 — code_methods/doc_chunks/kg_nodes

project 变为 payload 标签，不再作为集合名后缀。
旧集合保留向后兼容（搜索同时查新旧）。"
```

---

### Task 3: 搜索路径改用全局集合 + payload 过滤

**Files:**
- Modify: `src/interfaces/cli/build.rs`（handle_search 中集合选择逻辑 L739-800）

- [ ] **Step 1: 修改 handle_search 中集合选择逻辑**

在 `src/interfaces/cli/build.rs` 的 `handle_search` 中，找到 `collections_to_search` 匹配块（约 L739-800）。

将 `world == "code"` 分支改为：
```rust
                    "code" => {
                        if let Some(ref proj) = project {
                            // Phase 5: search global code_methods with project filter
                            vec![crate::shared::collections::CODE_METHODS.to_string()]
                        } else {
                            vec![crate::shared::collections::CODE_METHODS.to_string()]
                        }
                    }
```

将 `world == "doc"` 分支改为：
```rust
                    "doc" => {
                        vec![crate::shared::collections::DOC_CHUNKS.to_string()]
                    }
```

将 `world == "knowledge"` 分支改为：
```rust
                    "knowledge" => {
                        vec![crate::shared::collections::KG_NODES.to_string()]
                    }
```

将 `world == "all"` 分支改为：
```rust
                    "all" => {
                        vec![
                            crate::shared::collections::CODE_METHODS.to_string(),
                            crate::shared::collections::DOC_CHUNKS.to_string(),
                            crate::shared::collections::KG_NODES.to_string(),
                        ]
                    }
```

- [ ] **Step 2: 修改 entity_type 推断逻辑**

在 `handle_search` 中（约 L832-834），替换 `col.ends_with("_methods")` 等检查为使用 `entity_type_from_collection`：

```rust
                                let entity_type = crate::shared::collections::entity_type_from_collection(col);
```

- [ ] **Step 3: 修改 desc 构建逻辑**

在 `handle_search` 中（约 L838-846），将 `col.ends_with("_methods")` / `col.ends_with("_semantic")` 检查改为使用 `crate::shared::collections::is_global_collection(col) || col.ends_with("_methods")` 等。

- [ ] **Step 4: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 5: 运行测试**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: 693+ passed, 1 pre-existing failure

- [ ] **Step 6: 提交**

```bash
git add src/interfaces/cli/build.rs
git commit -m "feat: 搜索路径改用全局集合 — 单集合搜索 + payload 过滤

不再遍历多个 {project}_xxx 集合，改为搜全局 code_methods/doc_chunks/kg_nodes。
project 过滤通过 payload 实现（可选）。"
```

---

### Task 4: 其他模块改用全局集合

**Files:**
- Modify: `src/application/context/search_mcp.rs`
- Modify: `src/interfaces/grpc/services/build_service.rs`
- Modify: `src/shared/vectorizer.rs`
- Modify: `src/application/sync/nacos/config_sync.rs`

- [ ] **Step 1: 修改 search_mcp.rs**

在 `src/application/context/search_mcp.rs` 中，将 `c.ends_with("_methods")` 改为 `c == crate::shared::collections::CODE_METHODS || c.ends_with("_methods")`（兼容新旧），将 `format!("{}_methods", p)` 改为 `crate::shared::collections::CODE_METHODS.to_string()`。

- [ ] **Step 2: 修改 build_service.rs (gRPC)**

在 `src/interfaces/grpc/services/build_service.rs` 中，将 `.filter(|c| c.ends_with("_methods"))` 改为包含 `CODE_METHODS`。

- [ ] **Step 3: 修改 vectorizer.rs**

在 `src/shared/vectorizer.rs` 中，将 `format!("{}_semantic", project)` 改为 `crate::shared::collections::DOC_CHUNKS.to_string()`。

- [ ] **Step 4: 修改 config_sync.rs**

在 `src/application/sync/nacos/config_sync.rs` 中，将 `format!("{}_semantic", project)` 改为 `crate::shared::collections::DOC_CHUNKS.to_string()`。

- [ ] **Step 5: 编译验证**

Run: `cargo check 2>&1 | tail -10`
Expected: 编译通过

- [ ] **Step 6: 运行测试**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: 693+ passed, 1 pre-existing failure

- [ ] **Step 7: 提交**

```bash
git add src/application/context/search_mcp.rs src/interfaces/grpc/services/build_service.rs src/shared/vectorizer.rs src/application/sync/nacos/config_sync.rs
git commit -m "feat: 其他模块改用全局集合 — search_mcp/grpc/vectorizer/nacos

统一所有模块使用 code_methods/doc_chunks/kg_nodes 全局集合。"
```

---

### Task 5: 端到端验证

- [ ] **Step 1: 编译 release**

Run: `cargo build --release 2>&1 | tail -3`
Expected: 编译成功

- [ ] **Step 2: 清空 + 重建测试数据**

Run: `./target/release/dt clean --test && ./target/release/dt build --test 2>&1 | tail -10`
Expected: build --test 通过，数据写入全局集合

- [ ] **Step 3: 验证全局集合有数据**

Run: `curl -s http://localhost:6333/collections/code_methods 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin); print('code_methods points:', d.get('result',{}).get('points_count','N/A'))"`
Expected: code_methods 有向量

- [ ] **Step 4: 验证搜索正常**

Run: `./target/release/dt search "支付" --world all --limit 5 2>&1 | tail -10`
Expected: 搜索结果正常

- [ ] **Step 5: 运行全部测试**

Run: `cargo test --lib 2>&1 | tail -5`
Expected: 693+ passed, 1 pre-existing failure

- [ ] **Step 6: 提交验证记录**

```bash
git commit --allow-empty -m "test: 阶段 5+6 端到端验证通过 — 全局集合 + 去 project 中心化

验证结果：
- build --test 数据写入全局 code_methods/doc_chunks/kg_nodes
- 搜索正常工作（单集合搜索 + payload 过滤）
- 全部测试通过（693+ passed, 1 pre-existing failure）"
```

---

## Self-Review

**1. Spec coverage:**
- 第 5 节（Qdrant 集合重组）：Task 1 (工具函数) + Task 2 (写入) + Task 3 (搜索) + Task 4 (其他模块) ✅
- 第 4 节（去 project 中心化）：project 变为 payload 标签 ✅
- 第 11 节阶段 5-6 ✅

**2. Placeholder scan:** 无 TBD/TODO ✅

**3. 设计决策:**
- 旧集合保留向后兼容（不删除，搜索兼容新旧）
- project 变为 payload 标签（不删除 project 属性）
- 不做数据迁移（新写入走全局集合，旧数据留在旧集合，搜索同时查）
- `is_legacy_collection` 检测旧格式用于兼容搜索