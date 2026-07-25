# 搜索与依赖分析修复设计

## 背景

当前搜索功能存在 10 个问题：search_code 返回空(#1)、行号丢失(#2)、--path 未生效(#3)、max_depth 被忽略(#4)、两套搜索实现分裂(#5)、兜底搜索没用全文索引(#6)、Bolt 格式不兼容(#7)、collection 列表无缓存(#8)、代码搜索与 KG 关系割裂(#9)、score 阈值硬编码(#10)。

## 决策

- 范围：全部 10 个问题
- 架构统一：修好 CrossWorldSearch 为唯一搜索入口，build_service::handle_search 委托给它
- max_depth：Cypher 可变长路径 `*1..N`，上限 5
- --path：按 collection 名过滤（{project}_methods）
- 搜索与 KG 关系：SearchHit 附带 calls + element_id，不做自动展开

## 修复后数据流

### 构建（dt build）
- 源码 → Qdrant `{project}_methods`（payload: method_id, name, signature, file_path, project, start_line, end_line, calls, source_text, comment, class_name, llm_analysis）
- 源码 → Memgraph `:Method` 节点 + `:CALLS` 关系（基于静态 calls 字段）
- 业务事件/知识 → Memgraph 业务节点 → dt kg-sync → Qdrant `kg_nodes`

### 搜索
- 代码搜索（dt_search/dt_search_expand）：CrossWorldSearch.search_code → 按 project 过滤 {project}_methods collection → 读完整 payload（含 start_line/end_line/calls/method_id）→ 返回 SearchHit（含 calls + element_id）
- KG 搜索（dt_search_kg）：搜 kg_nodes collection → 返回 elementId
- 兜底：vector 不可用时用全文索引 db.index.fulltext.queryNodes("infra_search", ...)

### 依赖分析（dt_dependency）
- Cypher 可变长路径 `*1..N`（N=min(max_depth,5)）
- 统一 parse 函数支持 Bolt + HTTP 两种格式
- 返回 DependencyGraph（entities 带 distance）

### 关联机制
- 代码搜索返回的 SearchHit 包含 calls（静态调用列表）和 element_id（KG 节点 ID）
- 用户拿 element_id 调 dt_dependency 深查调用链

## 统一 SearchHit 字段

```rust
pub struct SearchHit {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub source_world: String,
    pub entity_type: String,
    pub score: f64,
    pub source_ref: Option<String>,
    pub file_path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub signature: Option<String>,
    pub calls: Vec<String>,
    pub element_id: Option<String>,
}
```

## 废弃
- build_service.rs::search_via_vector 和 search_via_graph 标记 #[deprecated]，逻辑迁入 CrossWorldSearch

## 配置
- DT_SEARCH_MIN_SCORE 环境变量，默认 0.3
- collection 列表进程内缓存，TTL 60s