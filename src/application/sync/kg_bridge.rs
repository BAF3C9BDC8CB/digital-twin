//! KG → Qdrant 桥接——将图数据库中 V2 业务标签节点同步到
//! Qdrant 向量库，用于语义搜索。
//!
//! ## V2 设计
//!
//! 与仅同步有限 Infrastructure 标签节点的 V1 不同，
//! V2 按照 V2 数据模式同步**全部业务标签节点**：
//!
//! - **基础设施节点**：  Server、Database、K8sDeployment、K8sService
//! - **服务节点**：         Service、ServiceInstance
//! - **Nacos 节点**：           NacosConfig、NacosService、NacosNamespace、
//!   NacosGroup、NacosInstance
//! - **知识节点**：       Knowledge、Concept、Playbook、Experience、Domain
//! - **文档节点**：        Document、Endpoint、ConfigKey、Table
//! - **事件节点**：           Deployment、ConfigChange、BugFix、Decision、PodEvent
//! - **横切节点**：         Thread、Requirement
//!
//! ## 同步模式
//!
//! - **全量同步**（`sync_all`）：无论时间戳如何，重新同步所有节点。
//! - **增量同步**（`sync_incremental`）：仅同步 `_kg_synced_at IS NULL`
//!   的节点（即上次同步后新建或变更的节点）。
//!
//! ## 搜索文本构造
//!
//! 每种节点类型都有自己的 `build_search_text` 逻辑，用于拼接
//! 最具语义意义的属性进行向量化。这确保向量搜索能按实体类型
//! 返回相关结果。

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

use super::traits::SyncReport;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// KG 节点向量在 Qdrant 中的集合名。
const KG_COLLECTION: &str = "kg_nodes";

/// 默认向量维度（BGE-M3 = 1024）。
const VECTOR_DIM: u32 = 1024;

/// 向量化 + upsert 的批量大小——平衡吞吐量与 GPU 内存。
/// fp16 BGE-M3 约 1.2 GB + 128×512×1024 激活约 128 MB/批。
const BATCH_SIZE: usize = 128;

/// 流水线向量化→upsert 的并发批次数。
/// 8GB GPU 上 fp16 时 2 路并发是安全的。
const CONCURRENCY: usize = 2;

/// 同步到 Qdrant 以用于语义搜索的 V2 业务标签。
///
/// 这些覆盖 V2 数据模式中定义的所有携带可语义搜索内容的实体类型。
/// 瞬态 / 结构性标签（如 `Project`、`Module`、`Method`、`Class`）
/// 被有意排除，因为它们由代码索引流水线处理。
pub const BUSINESS_LABELS: &[&str] = &[
    // -- 基础设施 --
    "Server",
    "Database",
    "K8sDeployment",
    "K8sService",
    // -- 服务注册 --
    "Service",
    "ServiceInstance",
    // -- Nacos --
    "NacosConfig",
    "NacosService",
    "NacosNamespace",
    "NacosGroup",
    "NacosInstance",
    // -- 知识 --
    "Knowledge",
    "Concept",
    "Playbook",
    "Experience",
    "Domain",
    // -- 文档与数据 --
    "Document",
    "Endpoint",
    "ConfigKey",
    "ConfigSection",
    "Table",
    // -- 事件 --
    "ConfigChange",
    "BugFix",
    "Decision",
    "PodEvent",
    // -- 横切 --
    "Thread",
    "Requirement",
];

// ---------------------------------------------------------------------------
// KgNode——图数据库的原始行
// ---------------------------------------------------------------------------

/// 获取 Cypher 查询返回的单行节点。
///
/// 每行包含：`[node_properties, elementId, labels]`。
#[derive(Debug, Clone)]
pub(crate) struct KgNode {
    /// 图元素 ID（Memgraph 节点 ID）（用作 point-id 哈希的输入）。
    element_id: String,
    /// 该节点上的所有标签（例如 `["Server", "Infrastructure"]`）。
    labels: Vec<String>,
    /// 节点的完整属性映射。
    properties: serde_json::Value,
}

// ---------------------------------------------------------------------------
// KgBridge
// ---------------------------------------------------------------------------

/// 通过向量化业务标签节点并作为向量 upsert 到 Qdrant，
/// 将图数据库桥接到 Qdrant 以实现语义搜索。
///
/// # 示例
///
/// ```ignore
/// let bridge = KgBridge::new(graph, embed, vector);
/// let report = bridge.sync_all().await?;
/// println!("Synced {} nodes", report.items_created);
/// ```
pub struct KgBridge {
    graph: Arc<dyn GraphRepository>,
    embed: Arc<dyn EmbedService>,
    vector: Arc<dyn VectorRepository>,
    /// 可选：用于优先级感知向量化的全局队列。
    /// 存在时，`process_batch` 会经由队列（LOW 通道）处理。
    queue: Option<Arc<super::queue::VectorQueue>>,
}

impl KgBridge {
    /// 创建连接到三个后端服务的新桥接。
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        embed: Arc<dyn EmbedService>,
        vector: Arc<dyn VectorRepository>,
    ) -> Self {
        Self {
            graph,
            embed,
            vector,
            queue: None,
        }
    }

    /// 挂载全局 VectorQueue 以实现优先级感知的向量化。
    ///
    /// 设置后，`process_batch` 会将向量化调用路由到队列的
    /// LOW 优先级通道，使后台同步让位于用户搜索。
    pub fn with_queue(mut self, queue: Arc<super::queue::VectorQueue>) -> Self {
        self.queue = Some(queue);
        self
    }

    // ------------------------------------------------------------------
    // 公共 API
    // ------------------------------------------------------------------

    /// 全量同步：重新向量化并 upsert **每个**业务标签节点。
    ///
    /// 这可能是代价较高的操作——日常使用请优先
    /// 选择 [`sync_incremental`](Self::sync_incremental)。
    pub async fn sync_all(&self) -> Result<SyncReport, DtError> {
        self.sync_impl(false).await
    }

    /// 增量同步：仅处理 `_kg_synced_at` 属性为 `NULL` 的节点——
    /// 即上次成功同步后被创建或同步标记被重置的节点。
    pub async fn sync_incremental(&self) -> Result<SyncReport, DtError> {
        self.sync_impl(true).await
    }

    /// 通过文件扩展名或内容判断配置内容是否为 YAML。
    fn detect_is_yaml(data_id: &str, config_type: &str, content: &str) -> bool {
        if config_type == "yaml" || config_type == "yml" {
            return true;
        }
        if config_type == "properties" || config_type == "json" || config_type == "xml" {
            return false;
        }
        // 从文件名判断
        let lower = data_id.to_lowercase();
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            return true;
        }
        if lower.ends_with(".properties") || lower.ends_with(".json") || lower.ends_with(".xml") {
            return false;
        }
        // 基于内容的判断：YAML 有顶层 "key:" 行
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.contains(':') && !trimmed.contains('=') {
                return true;
            }
            break;
        }
        false
    }

    /// 按原始格式重建配置文本。
    fn reconstruct_text(name: &str, pairs: &[(String, String)], is_yaml: bool) -> String {
        if !is_yaml {
            let mut t = name.to_string();
            for (k, v) in pairs {
                t.push('\n');
                t.push_str(&format!("{}={}", k, v));
            }
            return t;
        }
        // YAML：从点号分隔的键重建缩进树
        let prefix = name.to_string();
        let prefix_parts: Vec<&str> = prefix.split('.').collect();
        let prefix_depth = prefix_parts.len();

        let mut text = String::new();
        for (i, part) in prefix_parts.iter().enumerate() {
            let indent = "  ".repeat(i);
            text.push_str(&format!("{}{}:\n", indent, part));
        }

        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (k, v) in pairs {
            let full_parts: Vec<&str> = k.split('.').collect();
            if full_parts.len() <= prefix_depth {
                continue;
            }
            let rel_parts = &full_parts[prefix_depth..];
            // 输出中间键（跟踪以避免重复）
            let mut cur_path = String::new();
            for (i, part) in rel_parts.iter().enumerate() {
                if !cur_path.is_empty() {
                    cur_path.push('.');
                }
                cur_path.push_str(part);
                let depth = prefix_depth + i;
                let indent = "  ".repeat(depth);
                let is_last = i == rel_parts.len() - 1;
                if is_last {
                    text.push_str(&format!("{}{}: {}\n", indent, part, v));
                } else if !seen_paths.contains(&cur_path) {
                    seen_paths.insert(cur_path.clone());
                    text.push_str(&format!("{}{}:\n", indent, part));
                }
            }
        }
        text.trim_end().to_string()
    }

    /// 将 ConfigSection + ConfigKey 节点的自适应配置分块
    /// 同步到 Qdrant 的 `config_chunks` 集合。
    ///
    /// 对 NacosConfig 节点中存储的 Nacos 配置内容使用 `chunk_config_adaptive`，
    /// 通过 BGE-M3 向量化每个分块，并 upsert 到 Qdrant。
    pub async fn sync_config_chunks(&self) -> Result<SyncReport, DtError> {
        let start = Instant::now();

        // 获取 NacosConfig 节点及其内容
        let cypher = r#"
            MATCH (c:NacosConfig)
            WHERE c.content IS NOT NULL AND c.content <> ''
            RETURN c.config_id AS config_id, c.data_id AS data_id,
                   c.group AS group, c.type AS type,
                   c.content AS content, c.namespace AS namespace
        "#;
        let result = self.graph.read_query(cypher, HashMap::new()).await?;
        let configs = result.as_array().cloned().unwrap_or_default();

        let total = configs.len();
        if total == 0 {
            return Ok(SyncReport::skipped("config_chunks"));
        }

        tracing::info!("[config_chunks] 正在分块并向量化 {} 个配置", total);

        let mut chunk_count = 0usize;
        let collection = "config_chunks";
        self.vector.ensure_collection(collection, 1024).await?;

        // 使用我们 chunker 模块中的 chunk_config_adaptive
        use crate::shared::chunker::chunk_config_adaptive;

        for cfg in &configs {
            let content = cfg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let data_id = cfg.get("data_id").and_then(|v| v.as_str()).unwrap_or("");
            let group = cfg
                .get("group")
                .and_then(|v| v.as_str())
                .unwrap_or("DEFAULT_GROUP");
            let config_type = cfg.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let namespace = cfg.get("namespace").and_then(|v| v.as_str()).unwrap_or("");

            if content.is_empty() {
                continue;
            }

            let is_yaml = Self::detect_is_yaml(data_id, config_type, content);
            let sections = chunk_config_adaptive(content, is_yaml);
            if sections.is_empty() {
                continue;
            }

            // 构建分块文本并向量化
            let texts: Vec<String> = sections
                .iter()
                .map(|(name, pairs)| Self::reconstruct_text(name, pairs, is_yaml))
                .collect();

            let vectors = match self.embed.embed_batch(&texts).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("[config_chunks] {} 向量化失败: {}", data_id, e);
                    continue;
                }
            };

            // 构建 Qdrant points
            let points: Vec<serde_json::Value> = sections
                .iter()
                .zip(vectors.iter())
                .map(|((section_name, pairs), vec)| {
                    let text = Self::reconstruct_text(section_name, pairs, is_yaml);
                    let (doc_id, source_ref) =
                        crate::application::sync::nacos::config_sync::nacos_chunk_source(
                            namespace,
                            group,
                            data_id,
                            section_name,
                        );
                    serde_json::json!({
                        "id": source_ref,
                        "vector": vec,
                        "payload": {
                            // ---- 区块 ----
                            "section_name": section_name,
                            "config_type": config_type,
                            // ---- 来源 ----
                            "namespace": namespace,
                            "data_id": data_id,
                            "group": group,
                            "source": "nacos",
                            "doc_id": doc_id,
                            "source_ref": source_ref,
                            "environment": "",  // F6: 与 ConfigChunkVectorizer schema 对齐
                            // ---- 内容 ----
                            "text": text,
                            "key_count": pairs.len(),
                            // ---- 元数据 ----
                            "source_type": "config_chunk",
                        }
                    })
                })
                .collect();

            // F6: 写入前按 namespace/data_id purge 旧点（与 ConfigChunkVectorizer 行为一致）
            let _ = self
                .vector
                .delete_by_filter(
                    collection,
                    serde_json::json!({
                        "must": [
                            {"key": "namespace", "match": {"value": namespace}},
                            {"key": "data_id", "match": {"value": data_id}},
                        ]
                    }),
                )
                .await;

            if let Err(e) = self.vector.upsert(collection, points.clone()).await {
                tracing::warn!("[config_chunks] {} upsert 失败: {}", data_id, e);
            } else {
                chunk_count += points.len();
            }
        }

        let elapsed = start.elapsed();
        tracing::info!(
            "[config_chunks] 完成: {} 个分块来自 {} 个配置（{:.1}s）",
            chunk_count,
            total,
            elapsed.as_secs_f64()
        );

        Ok(SyncReport {
            source: "config_chunks".into(),
            items_fetched: total,
            items_created: chunk_count,
            elapsed_ms: elapsed.as_millis() as u64,
            ..Default::default()
        })
    }

    // ------------------------------------------------------------------
    // 单节点同步——用于写入后的自动同步
    // ------------------------------------------------------------------

    /// 将单个 KG 节点同步到 Qdrant，按 (label, property_key, value) 查找。
    ///
    /// 用于**写入后自动同步**：节点通过 `dt memorize` / `dt learn` / `dt event`
    /// 创建或变更后，本方法立即从图数据库获取该节点并同步到 Qdrant——
    /// 这样无需手动执行 `dt kg-sync`，向量索引也始终是最新的。
    ///
    /// 若未找到节点（例如标签不在 [`BUSINESS_LABELS`] 中），
    /// 则静默成功——并非每个图节点都需要进入 Qdrant。
    ///
    /// # 参数
    /// - `label` — 图标签（例如 `"Knowledge"`、`"Experience"`、`"Decision"`）。
    /// - `prop_key` — 用于查找节点的属性（例如 `"knowledge_id"`）。
    /// - `prop_value` — 属性值。
    pub async fn sync_node_by_property(
        &self,
        label: &str,
        prop_key: &str,
        prop_value: &str,
    ) -> Result<(), DtError> {
        // 仅同步业务标签节点。
        if !BUSINESS_LABELS.contains(&label) {
            tracing::debug!("[kg-sync] 跳过非业务标签: {label}");
            return Ok(());
        }

        let cypher = format!(
            "MATCH (n:{label} {{{prop_key}: $value}}) \
             RETURN n, elementId(n) AS eid, labels(n) AS lbls"
        );
        let mut params = std::collections::HashMap::new();
        params.insert(
            "value".to_string(),
            serde_json::Value::String(prop_value.to_string()),
        );

        let result = self.graph.read_query(&cypher, params).await?;
        let nodes = parse_graph_rows(&result)?;

        if nodes.is_empty() {
            tracing::debug!("[kg-sync] 未找到节点: label={label} {prop_key}={prop_value}");
            return Ok(());
        }

        self.process_batch(&nodes).await?;

        tracing::debug!("[kg-sync] 已自动同步 1 个节点: label={label} {prop_key}={prop_value}",);
        Ok(())
    }

    // ------------------------------------------------------------------
    // 内部（暴露给 BatchAccumulator）
    // ------------------------------------------------------------------

    /// 按 (label, key, value) 从 Memgraph 获取单个业务标签节点。
    ///
    /// 若未找到节点或节点不是业务标签，则返回 `None`。
    pub(crate) async fn fetch_node(
        &self,
        label: &str,
        prop_key: &str,
        prop_value: &str,
    ) -> Result<Option<KgNode>, DtError> {
        if !BUSINESS_LABELS.contains(&label) {
            return Ok(None);
        }

        let cypher = format!(
            "MATCH (n:{label} {{{prop_key}: $value}}) \
             RETURN n, elementId(n) AS eid, labels(n) AS lbls"
        );
        let mut params = std::collections::HashMap::new();
        params.insert(
            "value".to_string(),
            serde_json::Value::String(prop_value.to_string()),
        );

        let result = self.graph.read_query(&cypher, params).await?;
        let mut nodes = parse_graph_rows(&result)?;
        Ok(nodes.pop())
    }

    // ------------------------------------------------------------------
    // 实现
    // ------------------------------------------------------------------

    /// 共享的同步逻辑——`incremental` 控制 Cypher 的 WHERE 子句。
    async fn sync_impl(&self, incremental: bool) -> Result<SyncReport, DtError> {
        let start = Instant::now();
        let mode = if incremental { "incremental" } else { "full" };

        tracing::info!(
            "[kg-sync] 开始 {} 同步（BATCH_SIZE={BATCH_SIZE}, CONCURRENCY={CONCURRENCY}）",
            mode
        );

        // 1.  确保 Qdrant 集合存在。
        self.vector
            .ensure_collection(KG_COLLECTION, VECTOR_DIM)
            .await?;

        // 2.  从图数据库获取节点。
        let nodes = self.fetch_nodes(incremental).await?;

        if nodes.is_empty() {
            tracing::info!("[kg-sync] 没有需要同步的节点");
            return Ok(SyncReport {
                source: format!("kg-sync/{mode}"),
                ..SyncReport::default()
            });
        }

        let total = nodes.len();
        tracing::info!("[kg-sync] 已获取 {total} 个节点");

        let mut synced: usize = 0;
        let mut failed: usize = 0;
        let mut errors: Vec<String> = Vec::new();

        // 3.  分批处理，支持并发流水线（若多于 1 批）。
        if total <= BATCH_SIZE || CONCURRENCY < 2 {
            // 单批或无并发——顺序处理。
            for chunk in nodes.chunks(BATCH_SIZE) {
                match self.process_batch(chunk).await {
                    Ok(count) => synced += count,
                    Err(e) => {
                        failed += chunk.len();
                        errors.push(format!("批次错误: {e}"));
                    }
                }
            }
        } else {
            // 多批——流水线并发。
            use futures::stream::{self, StreamExt};

            let embed = self.embed.clone();
            let vector = self.vector.clone();
            let graph = self.graph.clone();

            let chunks: Vec<Vec<KgNode>> = nodes.chunks(BATCH_SIZE).map(|c| c.to_vec()).collect();

            tracing::info!(
                "[kg-sync] 流水线处理 {} 批 x {} 个节点，{} 路并发",
                chunks.len(),
                BATCH_SIZE,
                CONCURRENCY,
            );

            let results: Vec<Result<usize, DtError>> = stream::iter(chunks)
                .map(move |chunk| {
                    let e = embed.clone();
                    let v = vector.clone();
                    let g = graph.clone();
                    async move { process_batch_owned(e, v, g, chunk).await }
                })
                .buffer_unordered(CONCURRENCY)
                .collect()
                .await;

            for result in results {
                match result {
                    Ok(count) => synced += count,
                    Err(e) => {
                        failed += BATCH_SIZE;
                        errors.push(format!("批次错误: {e}"));
                    }
                }
            }
        }

        let elapsed = start.elapsed().as_millis() as u64;

        tracing::info!("[kg-sync] 完成——{synced}/{total} 已同步，{failed} 失败（{elapsed}ms）",);

        Ok(SyncReport {
            source: format!("kg-sync/{mode}"),
            items_fetched: total,
            items_created: synced,
            items_updated: 0,
            items_skipped: 0,
            items_failed: failed,
            errors,
            elapsed_ms: elapsed,
            skipped: false,
            ..SyncReport::default()
        })
    }

    /// 处理单个批次：向量化 → upsert → 标记已同步。
    pub(crate) async fn process_batch(&self, chunk: &[KgNode]) -> Result<usize, DtError> {
        // (a) 根据节点属性构建搜索文本。
        let texts: Vec<String> = chunk.iter().map(build_search_text).collect();

        // (b) 生成向量。
        let vectors = self.embed.embed_batch(&texts).await?;

        // (c) 构建 Qdrant points。
        let points: Vec<serde_json::Value> = chunk
            .iter()
            .zip(vectors.iter())
            .map(|(node, vec)| build_qdrant_point(node, vec))
            .collect();

        // (d) upsert 到 Qdrant。
        self.vector.upsert(KG_COLLECTION, points).await?;

        // (e) 在 Memgraph 中标记节点为已同步。
        let eids: Vec<&str> = chunk.iter().map(|n| n.element_id.as_str()).collect();
        let mut params = HashMap::new();
        params.insert("eids".to_string(), serde_json::json!(eids));

        self.graph
            .write_query(
                "UNWIND $eids AS eid \
                 MATCH (n) WHERE elementId(n) = eid \
                 SET n._kg_synced_at = datetime()",
                params,
            )
            .await?;

        Ok(chunk.len())
    }

    /// 从 Memgraph 获取业务标签节点。
    ///
    /// 当 `incremental` 为 `true` 时，仅返回没有 `_kg_synced_at`
    /// 的节点。
    async fn fetch_nodes(&self, incremental: bool) -> Result<Vec<KgNode>, DtError> {
        // 构建标签 OR 子句：n:Server OR n:Database OR n:K8sDeployment OR ...
        let label_conds: Vec<String> = BUSINESS_LABELS.iter().map(|l| format!("n:{l}")).collect();
        let label_clause = label_conds.join(" OR ");

        let cypher = if incremental {
            format!(
                "MATCH (n) \
                 WHERE ({label_clause}) AND (n._kg_synced_at IS NULL) \
                 RETURN n, elementId(n) AS eid, labels(n) AS lbls"
            )
        } else {
            format!(
                "MATCH (n) \
                 WHERE ({label_clause}) \
                 RETURN n, elementId(n) AS eid, labels(n) AS lbls"
            )
        };

        let params = HashMap::new();
        let result = self.graph.read_query(&cypher, params).await?;

        let nodes = parse_graph_rows(&result)?;
        Ok(nodes)
    }
}

/// process_batch 的所有权版本，用于并发流处理。
pub(crate) async fn process_batch_owned(
    embed: Arc<dyn EmbedService>,
    vector: Arc<dyn VectorRepository>,
    graph: Arc<dyn GraphRepository>,
    chunk: Vec<KgNode>,
) -> Result<usize, DtError> {
    let texts: Vec<String> = chunk.iter().map(build_search_text).collect();
    let vectors = embed.embed_batch(&texts).await?;
    let points: Vec<serde_json::Value> = chunk
        .iter()
        .zip(vectors.iter())
        .map(|(node, vec)| build_qdrant_point(node, vec))
        .collect();
    vector.upsert(KG_COLLECTION, points).await?;
    let eids: Vec<&str> = chunk.iter().map(|n| n.element_id.as_str()).collect();
    let mut params = HashMap::new();
    params.insert("eids".to_string(), serde_json::json!(eids));
    graph
        .write_query(
            "UNWIND $eids AS eid \
             MATCH (n) WHERE elementId(n) = eid \
             SET n._kg_synced_at = datetime()",
            params,
        )
        .await?;
    Ok(chunk.len())
}

// ---------------------------------------------------------------------------
// 搜索文本构造——按标签类型
// ---------------------------------------------------------------------------

/// 根据 KG 节点的属性构建自由文本搜索字符串。
///
/// 属性选择按标签感知，使生成的向量能捕获每种实体类型
/// 最具区分度的信息。
pub(crate) fn build_search_text(node: &KgNode) -> String {
    let props = &node.properties;
    let primary_label = node
        .labels
        .iter()
        .find(|l| BUSINESS_LABELS.contains(&l.as_str()))
        .map(|s| s.as_str())
        .unwrap_or("Unknown");

    match primary_label {
        // ── 基础设施 ──────────────────────────────────────────
        "Server" => concat_props(
            props,
            &[
                "name",
                "service_type",
                "hostname",
                "description",
                "environment",
            ],
        ),
        "Database" => concat_props(
            props,
            &["name", "db_type", "host", "description", "environment"],
        ),
        "K8sDeployment" => concat_props(
            props,
            &["name", "namespace", "image", "description", "environment"],
        ),
        "K8sService" => concat_props(
            props,
            &[
                "name",
                "namespace",
                "cluster_ip",
                "description",
                "environment",
            ],
        ),

        // ── 服务注册 ───────────────────────────────────────
        "Service" => concat_props(
            props,
            &[
                "name",
                "service_name",
                "hostname",
                "port",
                "description",
                "environment",
            ],
        ),
        "ServiceInstance" => concat_props(
            props,
            &["instance_id", "service_name", "host", "port", "environment"],
        ),

        // ── Nacos ──────────────────────────────────────────────────
        "NacosConfig" => concat_props(props, &["data_id", "group", "namespace", "content"]),
        "NacosService" => concat_props(
            props,
            &["service_name", "group_name", "namespace", "description"],
        ),
        "NacosNamespace" => concat_props(props, &["namespace", "description"]),
        "NacosGroup" => concat_props(props, &["group_name", "namespace", "description"]),
        "NacosInstance" => concat_props(
            props,
            &["instance_id", "service_name", "ip", "port", "namespace"],
        ),

        // ── 知识 ──────────────────────────────────────────────
        // 增强：包含所有语义丰富的字段以获得更好的向量质量。
        // summary/content 承载踩坑文本；definition 承载概念定义。
        "Knowledge" => concat_props(
            props,
            &[
                "name",
                "title",
                "domain",
                "summary",
                "content",
                "definition",
                "description",
            ],
        ),
        "Concept" => concat_props(
            props,
            &[
                "name",
                "definition",
                "domain",
                "summary",
                "description",
                "content",
            ],
        ),
        "Playbook" => concat_props(
            props,
            &["name", "title", "description", "domain", "content"],
        ),
        "Experience" => concat_props(
            props,
            &[
                "name",
                "title",
                "description",
                "domain",
                "content",
                "summary",
            ],
        ),
        "Domain" => concat_props(props, &["name", "description", "summary"]),

        // ── 文档与数据 ───────────────────────────────────────
        "Document" => concat_props(props, &["title", "content", "source_file", "description"]),
        "Endpoint" => concat_props(
            props,
            &["path", "method", "controller", "description", "project"],
        ),
        "ConfigKey" => concat_props(
            props,
            &["name", "value", "data_id", "namespace", "description"],
        ),
        "ConfigSection" => concat_props(
            props,
            &[
                "section_id",
                "name",
                "summary",
                "namespace",
                "data_id",
                "config_type",
            ],
        ),
        "Table" => concat_props(props, &["table_name", "db_type", "description", "columns"]),

        // ── 事件 ─────────────────────────────────────────────────
        "Deployment" => concat_props(props, &["name", "env", "branch", "description"]),
        "ConfigChange" => concat_props(props, &["name", "data_id", "description", "summary"]),
        "BugFix" => concat_props(props, &["title", "file", "description", "summary"]),
        "Decision" => concat_props(
            props,
            &["title", "decision", "reason", "scope", "description"],
        ),
        "PodEvent" => concat_props(
            props,
            &["pod_name", "namespace", "reason", "message", "description"],
        ),

        // ── Cross-cutting ──────────────────────────────────────────
        "Thread" => concat_props(props, &["title", "description", "domain", "tags"]),
        "Requirement" => concat_props(props, &["title", "description", "status", "domain"]),

        // ── Fallback ───────────────────────────────────────────────
        _ => concat_props(
            props,
            &["name", "title", "description", "summary", "content"],
        ),
    }
}

/// 将非空属性值（按顺序）拼接成以空格分隔的搜索文本字符串。
fn concat_props(props: &serde_json::Value, keys: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(keys.len());

    for key in keys {
        if let Some(val) = props.get(key) {
            match val {
                serde_json::Value::String(s) if !s.is_empty() => {
                    parts.push(s.clone());
                }
                serde_json::Value::Number(n) => {
                    parts.push(n.to_string());
                }
                // I3: 字符串数组贡献每个元素（例如 keywords）
                serde_json::Value::Array(arr) => {
                    for item in arr {
                        match item {
                            serde_json::Value::String(s) if !s.is_empty() => {
                                parts.push(s.clone());
                            }
                            serde_json::Value::Number(n) => {
                                parts.push(n.to_string());
                            }
                            _ => { /* 跳过 null、布尔值、嵌套数组/对象 */ }
                        }
                    }
                }
                _ => { /* 跳过 null、布尔值、对象 */ }
            }
        }
    }

    parts.join(" ")
}

// ---------------------------------------------------------------------------
// Qdrant point 构造
// ---------------------------------------------------------------------------

/// 根据 KG 节点及其向量构建 Qdrant point 的 JSON 值。
///
/// I1: point ID 派生自节点的稳定 **business ID**（而非易变的图
/// elementId），因此重建后的图节点仍映射到同一个向量 point，
/// 跨重建的 upsert 保持幂等。
fn build_qdrant_point(node: &KgNode, vector: &[f32]) -> serde_json::Value {
    let point_id = make_point_id(&business_id(node));
    let payload = build_payload(node);

    serde_json::json!({
        "id": point_id,
        "vector": vector,
        "payload": payload,
    })
}

/// 向量化单个 KG 节点并 upsert 到 Qdrant 的 `kg_nodes` 集合。
///
/// 这是**即时向量化**入口——知识/概念/经验节点写入图数据库后立即调用，
/// 这样无需单独执行 `dt kg-sync`，向量索引也始终是最新的。
///
/// # 参数
/// - `graph` — 图仓库（用于标记 `_kg_synced_at`）
/// - `embed` — 向量化服务（BGE-M3）
/// - `vector` — 向量仓库（Qdrant）
/// - `label` — 主要业务标签（例如 "Knowledge"、"Concept"、"Experience"）
/// - `id_field` — 节点的唯一 ID 属性名（例如 "knowledge_id"）
/// - `id_value` — 节点的唯一 ID 值
/// - `properties` — 节点的完整属性映射（用于构建搜索文本）
///
/// # 流程
/// 1. 根据给定属性构造临时 `KgNode`
/// 2. 通过 `build_search_text` 构建搜索文本
/// 3. 通过 `embed.embed_batch` 向量化文本
/// 4. 通过 `build_qdrant_point` 构建 Qdrant point
/// 5. upsert 到 `kg_nodes` 集合
/// 6. 在图数据库中标记节点 `_kg_synced_at = datetime()`
pub async fn embed_kg_node(
    graph: &dyn GraphRepository,
    embed: &dyn EmbedService,
    vector: &dyn VectorRepository,
    label: &str,
    id_field: &str,
    id_value: &str,
    properties: &serde_json::Value,
) -> Result<(), DtError> {
    // 仅向量化业务标签节点
    if !BUSINESS_LABELS.contains(&label) {
        tracing::debug!("[embed_kg_node] 跳过非业务标签: {label}");
        return Ok(());
    }

    // 1. 获取该节点真实的 Memgraph elementId。Qdrant payload 中的
    //    "elementId" 字段必须是真实的图元素 ID（格式 "4:xxx:yyy"），
    //    这样 `expand_nodes`（使用 `WHERE elementId(n) IN $ids`）才能
    //    匹配到它。构造合成 id 会破坏图展开。
    let fetch_cypher =
        format!("MATCH (n:{label} {{{id_field}: $value}}) RETURN elementId(n) AS eid");
    let mut fetch_params = HashMap::new();
    fetch_params.insert(
        "value".to_string(),
        serde_json::Value::String(id_value.to_string()),
    );
    let fetch_result = graph.read_query(&fetch_cypher, fetch_params).await?;
    let real_element_id = fetch_result
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("eid"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| {
            DtError::Repository(format!(
                "embed_kg_node: 未找到节点 {label} {id_field}={id_value}"
            ))
        })?;

    // 2. 使用真实的 elementId 构造 KgNode
    let node = KgNode {
        element_id: real_element_id.clone(),
        labels: vec![label.to_string()],
        properties: properties.clone(),
    };

    // 3. 构建搜索文本
    let text = build_search_text(&node);

    // 4. 向量化
    let vectors = embed.embed_batch(std::slice::from_ref(&text)).await?;
    let vec = match vectors.into_iter().next() {
        Some(v) => v,
        None => return Ok(()),
    };

    // 5. 构建 Qdrant point（build_payload 将 node.element_id 写入 "elementId"）
    let point = build_qdrant_point(&node, &vec);

    // 6. upsert 到 Qdrant
    vector.ensure_collection(KG_COLLECTION, VECTOR_DIM).await?;
    vector.upsert(KG_COLLECTION, vec![point]).await?;

    // 7. 在图数据库中标记已同步
    let mark_cypher =
        format!("MATCH (n:{label} {{{id_field}: $value}}) SET n._kg_synced_at = datetime()");
    let mut mark_params = HashMap::new();
    mark_params.insert(
        "value".to_string(),
        serde_json::Value::String(id_value.to_string()),
    );
    graph.write_query(&mark_cypher, mark_params).await?;

    tracing::debug!(
        "[embed_kg_node] 已向量化 {label} {id_field}={id_value}（eid={}）",
        real_element_id
    );
    Ok(())
}

/// 根据节点属性构建 Qdrant payload。
///
/// I2/I4: 与提取实体 point 共享的统一核心模式（§7.2）——
/// `{elementId, business_id, name, type, summary, keywords, project, labels,
/// doc_id?, origin, source}`。`summary` 为**完整**文本（不截断）。
/// `description` 作为 `summary` 的遗留别名保留，因为现有消费方
/// （retriever.rs、search_mcp.rs）读取它。
fn build_payload(node: &KgNode) -> serde_json::Value {
    let props = &node.properties;
    let bid = business_id(node);
    let primary_label = node
        .labels
        .iter()
        .find(|l| BUSINESS_LABELS.contains(&l.as_str()))
        .map(|l| l.to_lowercase())
        .unwrap_or_default();
    // 完整代表文本：summary/description/content 中第一个非空值。
    let summary = ["summary", "description", "content"]
        .iter()
        .find_map(|k| props.get(k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let keywords: Vec<serde_json::Value> = props
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| serde_json::Value::String(s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let origin = props
        .get("origin")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("learned");

    // 显示标题：name → title → file_path 文件名 → business_id 最后一段。
    // Document 节点没有 `name` 属性（consolidate 仅设置
    // doc_id/project/file_path/doc_type）——没有该兜底时其 payload
    // name 为 null，会破坏 §7.2 结构断言与知识检索。
    let name = props
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            props
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .or_else(|| {
            props
                .get("file_path")
                .and_then(|v| v.as_str())
                .and_then(|p| p.rsplit(['/', '\\']).next())
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| bid.rsplit('/').next().unwrap_or(&bid).to_string());

    let mut payload = serde_json::json!({
        // ---- 标识 ----
        "elementId": node.element_id,
        "business_id": bid,
        "name": name,
        "type": primary_label,
        "labels": node.labels,
        // ---- 内容（I4：完整、不截断） ----
        "summary": summary,
        "description": summary,
        "keywords": keywords,
        // ---- 范围 ----
        "project": props.get("project").cloned().unwrap_or(serde_json::Value::Null),
        // ---- 来源 ----
        "origin": origin,
        "source": "kg",
        // ---- 标签专属扩展（展示用） ----
        "service_type": props.get("service_type").cloned().unwrap_or(serde_json::Value::Null),
        "environment": props.get("environment").cloned().unwrap_or(serde_json::Value::Null),
    });
    // 仅在存在时写入 doc_id（提取/业务文档关联的节点）
    if let Some(doc_id) = props.get("doc_id").and_then(|v| v.as_str()) {
        if !doc_id.is_empty() {
            payload["doc_id"] = serde_json::Value::String(doc_id.to_string());
        }
    }
    payload
}

/// 通过 SHA-256 从稳定的 business ID 生成确定性的 UUID v4。
///
/// 这确保同一 business ID 在多次同步运行中始终映射到同一个 Qdrant
/// point ID，从而支持幂等 upsert。设为 `pub(crate)` 以便 Consolidate
/// 层（任务 2、§7.4）能从 business ID 派生 point ID。
pub(crate) fn make_point_id(business_id: &str) -> String {
    let hash = Sha256::digest(business_id.as_bytes());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]),
        u16::from_be_bytes([hash[4], hash[5]]),
        u16::from_be_bytes([hash[6], hash[7]]) & 0x0fff,
        u16::from_be_bytes([hash[8], hash[9]]) & 0x3fff | 0x8000,
        u64::from_be_bytes([hash[10], hash[11], hash[12], hash[13], hash[14], hash[15], 0, 0,])
            >> 16,
    )
}

/// 派生 KG 节点的稳定 business ID（I1）。
///
/// 优先级顺序：
/// 1. 显式唯一 ID 属性（`knowledge_id`、`concept_id`、…）——
///    这些是节点的真实业务标识，图重建后依然有效。
/// 2. `name`（对于 K8sDeployment/Table/ConfigKey 等复合键节点，
///    可由 `namespace`/`db` 限定）。
/// 3. `element_id` 作为最后兜底（遗留行为）。
pub(crate) fn business_id(node: &KgNode) -> String {
    business_id_from_props(&node.properties, &node.element_id)
}

/// 按优先级顺序排列的显式唯一 ID 属性键（I1）。
const ID_KEYS: &[&str] = &[
    "entity_id",
    "knowledge_id",
    "concept_id",
    "experience_id",
    "playbook_id",
    "domain_id",
    "server_id",
    "database_id",
    "service_id",
    "instance_id",
    "endpoint_id",
    "doc_id",
    "config_id",
    "thread_id",
    "requirement_id",
    "decision_id",
    "event_id",
    "session_id",
    "version_id",
    "observation_id",
    "analysis_id",
];

/// 从属性映射派生稳定 business ID（S5 共享入口）。
///
/// 与 [`business_id`] 相同的 21 键优先级顺序；retrieve.rs 用它为
/// 从不物化为 `KgNode` 的图展开邻居派生 ID。
pub(crate) fn business_id_from_props(
    props: &serde_json::Value,
    element_id_fallback: &str,
) -> String {
    for key in ID_KEYS {
        if let Some(s) = props.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }

    // 复合标识节点：由 namespace/db 限定的 name。
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

/// 删除属于某个业务节点的 `kg_nodes` 向量 point（I5）。
///
/// 补全 §7.5：图节点被删除时，其向量 point 也必须一并删除。
/// 通过 payload 的 `business_id` 匹配删除——确定性源于
/// I1 使 business_id ↔ point 按构造为 1:1。
///
/// 注意：I2 之前写入的遗留 point 缺少 `business_id` payload 键，
/// 不会被匹配；它们由 §12 风险项 6 中记录的一次性 `kg_nodes`
/// 清空处理。
pub async fn delete_kg_vector(
    vector: &dyn VectorRepository,
    business_id: &str,
) -> Result<(), DtError> {
    vector
        .delete_by_filter(
            KG_COLLECTION,
            serde_json::json!({
                "must": [{"key": "business_id", "match": {"value": business_id}}],
            }),
        )
        .await
}

// ---------------------------------------------------------------------------
// 图结果解析
// ---------------------------------------------------------------------------

/// 将原始图响应 JSON 解析为 `Vec<KgNode>`。
///
/// 处理两种响应格式：
/// 1. **Bolt driver**——行对象的 `Value::Array`：
///    ```json
///    [{"n": {...}, "eid": "4:...", "lbls": ["Server"]}]
///    ```
/// 2. **HTTP API**（遗留兜底）：
///    ```json
///    {"results":[{"columns":["n","eid","lbls"],"data":[{"row":[{...},"4:...",["Server"]]}]}]}
///    ```
fn parse_graph_rows(raw: &serde_json::Value) -> Result<Vec<KgNode>, DtError> {
    // 先尝试 Bolt driver 格式（行对象的数组）。
    if let Some(rows) = raw.as_array() {
        return parse_bolt_rows(rows);
    }

    // 回退到 HTTP API 格式。
    let rows = raw
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|results| results.first())
        .and_then(|first| first.get("data"))
        .and_then(|data| data.as_array())
        .ok_or_else(|| DtError::Repository("图响应中缺少 'results[0].data'".into()))?;

    let mut nodes: Vec<KgNode> = Vec::with_capacity(rows.len());

    for row_val in rows {
        let row = row_val
            .get("row")
            .and_then(|r| r.as_array())
            .ok_or_else(|| DtError::Repository("图数据条目中缺少 'row'".into()))?;

        if row.len() < 3 {
            continue;
        }

        let properties = row[0].clone();
        let element_id = row[1].as_str().unwrap_or("").to_string();

        let labels: Vec<String> = row[2]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        nodes.push(KgNode {
            element_id,
            labels,
            properties,
        });
    }

    Ok(nodes)
}

/// 解析 Bolt driver 格式的行（JSON 对象数组）。
///
/// 每个对象包含键 `n`（节点属性）、`eid`（elementId 字符串）和
/// `lbls`（标签数组）。
fn parse_bolt_rows(rows: &[serde_json::Value]) -> Result<Vec<KgNode>, DtError> {
    let mut nodes: Vec<KgNode> = Vec::with_capacity(rows.len());

    for row in rows {
        let properties = row.get("n").cloned().unwrap_or(serde_json::Value::Null);

        let element_id = row
            .get("eid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if element_id.is_empty() {
            // 尝试每个行对象内部的遗留行数组格式
            if let Some(row_arr) = row.get("row").and_then(|r| r.as_array()) {
                if row_arr.len() >= 3 {
                    let props = row_arr[0].clone();
                    let eid = row_arr[1].as_str().unwrap_or("").to_string();
                    let lbls: Vec<String> = row_arr[2]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    nodes.push(KgNode {
                        element_id: eid,
                        labels: lbls,
                        properties: props,
                    });
                    continue;
                }
            }
            continue;
        }

        let labels: Vec<String> = row
            .get("lbls")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        nodes.push(KgNode {
            element_id,
            labels,
            properties,
        });
    }

    Ok(nodes)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::{CollectionInfo, HealthStatus};
    use async_trait::async_trait;

    // ------------------------------------------------------------------
    // business_id_from_props
    // ------------------------------------------------------------------

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

    // ------------------------------------------------------------------
    // make_point_id
    // ------------------------------------------------------------------

    #[test]
    fn make_point_id_is_deterministic() {
        let eid = "4:test-element-id-123";
        let id1 = make_point_id(eid);
        let id2 = make_point_id(eid);
        assert_eq!(id1, id2, "相同的 elementId 必须生成相同的 UUID");
    }

    #[test]
    fn make_point_id_different_per_input() {
        let id_a = make_point_id("4:aaa");
        let id_b = make_point_id("4:bbb");
        assert_ne!(id_a, id_b, "不同的 elementIds 必须生成不同的 UUID");
    }

    #[test]
    fn make_point_id_is_valid_uuid() {
        let id = make_point_id("4:some-element");
        // 应匹配 UUID v4 模式：xxxxxxxx-xxxx-4xxx-[89ab]xxx-xxxxxxxxxxxx
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5, "必须包含 5 个以破折号分隔的部分");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert!(parts[2].starts_with('4'), "版本半字节必须为 4");
        assert_eq!(parts[3].len(), 4);
        assert!(
            parts[3].starts_with('8')
                || parts[3].starts_with('9')
                || parts[3].starts_with('a')
                || parts[3].starts_with('b')
                || parts[3].starts_with('A')
                || parts[3].starts_with('B'),
            "变体位必须为 10xx"
        );
        assert_eq!(parts[4].len(), 12);
    }

    // ------------------------------------------------------------------
    // build_search_text
    // ------------------------------------------------------------------

    fn make_node(labels: Vec<&str>, props: serde_json::Value) -> KgNode {
        KgNode {
            element_id: "4:test-eid".into(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            properties: props,
        }
    }

    #[test]
    fn search_text_for_server() {
        let node = make_node(
            vec!["Server"],
            serde_json::json!({
                "name": "payment-svc",
                "service_type": "spring-boot",
                "hostname": "10.0.1.5",
                "description": "Payment processing service",
                "environment": "prod"
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("payment-svc"));
        assert!(text.contains("spring-boot"));
        assert!(text.contains("10.0.1.5"));
        assert!(text.contains("Payment processing service"));
        assert!(text.contains("prod"));
    }

    #[test]
    fn search_text_for_database() {
        let node = make_node(
            vec!["Database"],
            serde_json::json!({
                "name": "user-db",
                "db_type": "mysql",
                "host": "10.0.2.10",
                "description": "User database",
                "environment": "staging"
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("user-db"));
        assert!(text.contains("mysql"));
        assert!(text.contains("10.0.2.10"));
    }

    #[test]
    fn search_text_for_nacos_config() {
        let node = make_node(
            vec!["NacosConfig"],
            serde_json::json!({
                "data_id": "application.yml",
                "group": "DEFAULT_GROUP",
                "namespace": "newoffen-test",
                "content": "spring.datasource.url=jdbc:mysql://..."
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("application.yml"));
        assert!(text.contains("DEFAULT_GROUP"));
        assert!(text.contains("newoffen-test"));
        assert!(text.contains("jdbc:mysql"));
    }

    #[test]
    fn search_text_for_knowledge() {
        let node = make_node(
            vec!["Knowledge"],
            serde_json::json!({
                "name": "deploy-process",
                "title": "Deployment Process",
                "domain": "devops",
                "summary": "How to deploy services",
                "content": "Step 1: build. Step 2: push. Step 3: deploy."
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("deploy-process"));
        assert!(text.contains("Deployment Process"));
        assert!(text.contains("devops"));
        assert!(text.contains("How to deploy services"));
        assert!(text.contains("Step 1: build"));
    }

    #[test]
    fn search_text_for_concept() {
        let node = make_node(
            vec!["Concept"],
            serde_json::json!({
                "name": "CircuitBreaker",
                "definition": "A design pattern for fault tolerance",
                "domain": "architecture"
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("CircuitBreaker"));
        assert!(text.contains("design pattern"));
        assert!(text.contains("architecture"));
    }

    #[test]
    fn search_text_fallback_for_unknown_label() {
        let node = make_node(
            vec!["SomeUnknownLabel"],
            serde_json::json!({
                "name": "mystery",
                "title": "The Mystery",
                "description": "Something unknown"
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("mystery"));
        assert!(text.contains("The Mystery"));
        assert!(text.contains("Something unknown"));
    }

    #[test]
    fn search_text_skips_empty_and_null() {
        let node = make_node(
            vec!["Server"],
            serde_json::json!({
                "name": "srv",
                "service_type": null,
                "hostname": "",
                "description": "desc"
            }),
        );
        let text = build_search_text(&node);
        assert_eq!(text, "srv desc");
    }

    // ------------------------------------------------------------------
    // build_payload
    // ------------------------------------------------------------------

    #[test]
    fn payload_preserves_full_description() {
        // I4: summary/description must NOT be truncated (was 200 chars).
        let node = KgNode {
            element_id: "4:test".into(),
            labels: vec!["Server".into()],
            properties: serde_json::json!({
                "name": "test-svc",
                "description": "a".repeat(500),
                "service_type": "http",
                "environment": "dev",
            }),
        };
        let payload = build_payload(&node);
        let desc = payload["description"].as_str().unwrap();
        assert_eq!(desc.len(), 500, "description 必须完整保留");
        assert_eq!(
            payload["summary"].as_str().unwrap().len(),
            500,
            "summary 完整镜像 description"
        );
    }

    #[test]
    fn payload_includes_all_expected_fields() {
        let node = KgNode {
            element_id: "4:abc".into(),
            labels: vec!["Database".into()],
            properties: serde_json::json!({
                "name": "mydb",
                "database_id": "dt://db/proj/mydb",
                "environment": "prod",
                "project": "proj",
                "keywords": ["mysql", "核心库"],
            }),
        };
        let payload = build_payload(&node);
        // I2 unified core schema (§7.2)
        assert_eq!(payload["elementId"], "4:abc");
        assert_eq!(payload["business_id"], "dt://db/proj/mydb");
        assert_eq!(payload["name"], "mydb");
        assert_eq!(payload["type"], "database");
        assert_eq!(payload["labels"], serde_json::json!(["Database"]));
        assert_eq!(payload["project"], "proj");
        assert_eq!(payload["keywords"], serde_json::json!(["mysql", "核心库"]));
        assert_eq!(payload["origin"], "learned");
        assert_eq!(payload["source"], "kg");
        // label-specific extensions retained
        assert_eq!(payload["environment"], "prod");
        // doc_id absent when not set
        assert!(payload.get("doc_id").is_none());
    }

    #[test]
    fn payload_doc_id_present_when_set() {
        let node = KgNode {
            element_id: "4:abc".into(),
            labels: vec!["Document".into()],
            properties: serde_json::json!({
                "name": "guide",
                "doc_id": "dt://doc/proj/guide.md",
            }),
        };
        let payload = build_payload(&node);
        assert_eq!(payload["doc_id"], "dt://doc/proj/guide.md");
    }

    // ------------------------------------------------------------------
    // business_id (I1)
    // ------------------------------------------------------------------

    #[test]
    fn business_id_prefers_explicit_id_property() {
        let node = KgNode {
            element_id: "4:xyz".into(),
            labels: vec!["Knowledge".into()],
            properties: serde_json::json!({
                "name": "some-name",
                "knowledge_id": "dt://knowledge/proj/pattern/foo",
            }),
        };
        assert_eq!(business_id(&node), "dt://knowledge/proj/pattern/foo");
    }

    #[test]
    fn business_id_falls_back_to_qualified_name() {
        // 复合键节点（K8sDeployment/Table/ConfigKey）：name@namespace
        let node = KgNode {
            element_id: "4:xyz".into(),
            labels: vec!["K8sDeployment".into()],
            properties: serde_json::json!({
                "name": "pay-svc",
                "namespace": "prod",
            }),
        };
        assert_eq!(business_id(&node), "pay-svc@prod");

        let bare = KgNode {
            element_id: "4:xyz".into(),
            labels: vec!["Server".into()],
            properties: serde_json::json!({"name": "api"}),
        };
        assert_eq!(business_id(&bare), "api");
    }

    #[test]
    fn business_id_last_resort_element_id() {
        let node = KgNode {
            element_id: "4:fallback".into(),
            labels: vec!["PodEvent".into()],
            properties: serde_json::json!({}),
        };
        assert_eq!(business_id(&node), "4:fallback");
    }

    // ------------------------------------------------------------------
    // concat_props — I3 string arrays
    // ------------------------------------------------------------------

    #[test]
    fn concat_props_includes_string_arrays() {
        let props = serde_json::json!({
            "name": "foo",
            "keywords": ["bar", "baz"],
            "mixed": ["qux", 7, null, true],
            "empty_arr": [],
        });
        let text = concat_props(&props, &["name", "keywords", "mixed", "empty_arr"]);
        assert_eq!(text, "foo bar baz qux 7");
    }

    // ------------------------------------------------------------------
    // parse_graph_rows
    // ------------------------------------------------------------------

    #[test]
    fn parse_valid_rows() {
        let raw = serde_json::json!({
            "results": [{
                "columns": ["n", "eid", "lbls"],
                "data": [
                    {"row": [{"name": "test-svc", "service_type": "http"}, "4:eid-1", ["Server"]]},
                    {"row": [{"data_id": "app.yml", "group": "DEFAULT"}, "4:eid-2", ["NacosConfig"]]},
                ]
            }]
        });
        let nodes = parse_graph_rows(&raw).expect("应能解析");
        assert_eq!(nodes.len(), 2);

        assert_eq!(nodes[0].element_id, "4:eid-1");
        assert_eq!(nodes[0].labels, vec!["Server"]);
        assert_eq!(nodes[0].properties["name"], "test-svc");

        assert_eq!(nodes[1].element_id, "4:eid-2");
        assert_eq!(nodes[1].labels, vec!["NacosConfig"]);
        assert_eq!(nodes[1].properties["data_id"], "app.yml");
    }

    #[test]
    fn parse_empty_response() {
        let raw = serde_json::json!({
            "results": [{
                "columns": ["n", "eid", "lbls"],
                "data": []
            }]
        });
        let nodes = parse_graph_rows(&raw).expect("应能解析空响应");
        assert!(nodes.is_empty());
    }

    #[test]
    fn parse_missing_results_returns_error() {
        let raw = serde_json::json!({"unexpected": "format"});
        let result = parse_graph_rows(&raw);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // build_qdrant_point
    // ------------------------------------------------------------------

    #[test]
    fn point_structure_is_correct() {
        let node = KgNode {
            element_id: "4:test-point".into(),
            labels: vec!["Server".into()],
            properties: serde_json::json!({
                "name": "api",
                "server_id": "dt://server/proj/api",
                "description": "desc",
            }),
        };
        let vector = vec![0.1_f32, 0.2_f32, 0.3_f32];
        let point = build_qdrant_point(&node, &vector);

        assert!(point["id"].is_string());
        // I1: point id derives from the business id, NOT the elementId
        assert_eq!(
            point["id"].as_str().unwrap(),
            make_point_id("dt://server/proj/api")
        );
        assert_eq!(point["vector"].as_array().unwrap().len(), 3);
        assert_eq!(point["payload"]["name"], "api");
        assert_eq!(point["payload"]["source"], "kg");
    }

    #[test]
    fn point_id_stable_across_element_id_change() {
        // I1 核心属性：重建后的图节点（新的 elementId、相同的
        // 业务标识）映射到同一个 point → 幂等 upsert。
        let props = serde_json::json!({
            "name": "api",
            "server_id": "dt://server/proj/api",
        });
        let old_node = KgNode {
            element_id: "4:old".into(),
            labels: vec!["Server".into()],
            properties: props.clone(),
        };
        let new_node = KgNode {
            element_id: "4:new".into(),
            labels: vec!["Server".into()],
            properties: props,
        };
        let v = vec![0.1_f32];
        assert_eq!(
            build_qdrant_point(&old_node, &v)["id"],
            build_qdrant_point(&new_node, &v)["id"]
        );
    }

    // ------------------------------------------------------------------
    // delete_kg_vector (I5)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_kg_vector_filters_on_business_id() {
        struct CaptureVector {
            captured: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
        }

        #[async_trait]
        impl VectorRepository for CaptureVector {
            async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
                Ok(())
            }
            async fn search(
                &self,
                _c: &str,
                _v: Vec<f32>,
                _l: u64,
            ) -> Result<Vec<serde_json::Value>, DtError> {
                Ok(vec![])
            }
            async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
                Ok(())
            }
            async fn delete_by_filter(
                &self,
                collection: &str,
                filter: serde_json::Value,
            ) -> Result<(), DtError> {
                self.captured
                    .lock()
                    .unwrap()
                    .push((collection.to_string(), filter));
                Ok(())
            }
            async fn list_collections(&self) -> Result<Vec<String>, DtError> {
                Ok(vec![])
            }
            async fn collection_info(&self, _n: &str) -> Result<CollectionInfo, DtError> {
                Ok(CollectionInfo {
                    name: "kg_nodes".to_string(),
                    points_count: 0,
                    vector_dim: 1024,
                    model_version: "bge-m3".to_string(),
                })
            }
            async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
                Ok(())
            }
            async fn health_check(&self) -> Result<HealthStatus, DtError> {
                Ok(HealthStatus::Healthy)
            }
        }

        let vector = CaptureVector {
            captured: std::sync::Mutex::new(vec![]),
        };
        delete_kg_vector(&vector, "dt://knowledge/proj/pattern/foo")
            .await
            .expect("删除应成功");

        let captured = vector.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, KG_COLLECTION);
        assert_eq!(
            captured[0].1,
            serde_json::json!({
                "must": [{"key": "business_id", "match": {"value": "dt://knowledge/proj/pattern/foo"}}],
            })
        );
    }

    // ------------------------------------------------------------------
    // BUSINESS_LABELS integrity
    // ------------------------------------------------------------------

    #[test]
    fn business_labels_no_duplicates() {
        let mut labels: Vec<&str> = BUSINESS_LABELS.to_vec();
        labels.sort_unstable();
        let orig_len = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), orig_len, "BUSINESS_LABELS 不得有重复");
    }

    #[test]
    fn business_labels_covers_all_search_text_branches() {
        // Every label in BUSINESS_LABELS should produce a non-empty search text
        // when given a generous set of properties covering all known keys.
        for &label in BUSINESS_LABELS {
            let node = KgNode {
                element_id: "4:test".into(),
                labels: vec![label.to_string()],
                properties: serde_json::json!({
                    "name": "x",
                    "title": "x",
                    "description": "y",
                    "domain": "test",
                    "summary": "z",
                    "content": "z",
                    "definition": "z",
                    "instance_id": "x",
                    "service_name": "x",
                    "service_type": "x",
                    "hostname": "x",
                    "host": "x",
                    "port": 8080,
                    "data_id": "x",
                    "group": "x",
                    "namespace": "x",
                    "db_type": "x",
                    "image": "x",
                    "cluster_ip": "x",
                    "path": "/x",
                    "method": "GET",
                    "controller": "X",
                    "key": "x",
                    "value": "x",
                    "table_name": "x",
                    "env": "test",
                    "branch": "main",
                    "decision": "x",
                    "reason": "x",
                    "scope": "x",
                    "pod_name": "x",
                    "message": "x",
                    "tags": "x",
                    "status": "x",
                    "environment": "test",
                    "project": "x",
                }),
            };
            let text = build_search_text(&node);
            assert!(!text.is_empty(), "标签 '{label}' 的搜索文本为空");
        }
    }

    #[test]
    fn search_text_for_k8s_deployment() {
        let node = make_node(
            vec!["K8sDeployment"],
            serde_json::json!({
                "name": "payment-api",
                "namespace": "newoffen",
                "image": "registry/payment:v2",
                "description": "Payment API deployment",
                "environment": "prod"
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("payment-api"));
        assert!(text.contains("newoffen"));
        assert!(text.contains("registry/payment:v2"));
    }

    #[test]
    fn search_text_for_endpoint() {
        let node = make_node(
            vec!["Endpoint"],
            serde_json::json!({
                "path": "/api/v1/users",
                "method": "GET",
                "controller": "UserController",
                "description": "List users",
                "project": "user-svc"
            }),
        );
        let text = build_search_text(&node);
        assert!(text.contains("/api/v1/users"));
        assert!(text.contains("GET"));
        assert!(text.contains("UserController"));
    }

    // ------------------------------------------------------------------
    // embed_kg_node
    // ------------------------------------------------------------------

    #[test]
    fn embed_kg_node_function_exists() {
        // 验证函数签名存在（编译时检查）。
        //
        // `pub async fn` 被脱糖为 `fn(...) -> impl Future<Output=...>`，
        // 无法直接 `as` cast 到 `fn(...) -> Pin<Box<dyn Future + Send>>`
        // (E0605: `impl Future` ≠ `Pin<Box<dyn Future + Send>>`)。如实
        // 写一份 function-fit 检查时 HRTB 与 `impl Future` 也无法精确
        // 匹配（E0308, "one type is more general than the other"）。
        //
        // 因此改用「无捕获闭包包裹 + Box::pin」的方式：闭包形式上写明
        // brief 中要求的 7 个参数类型与返回 `Pin<Box<dyn Future<Output=
        // Result<(), DtError>> + Send>>` 的目标形状；闭包体内调用
        // `embed_kg_node(...)` 并 `Box::pin` 装入 HRTB 的 `Send + '_`
        // 容器。该闭包可隐式 coerce 为 `for<...> fn(...) -> Pin<Box...>`
        // 形式的 fn 指针，赋给显式 fn 指针类型变量即可在编译期同时验证：
        //   1. `embed_kg_node` 函数符号存在
        //   2. 7 个参数类型与 brief 完全一致
        //   3. 返回类型为 `Future<Output = Result<(), DtError>> + Send`
        //      （通过 `Pin<Box<... + Send + '_>>` 表达）
        let wrapper: for<'a> fn(
            &'a (dyn GraphRepository + 'a),
            &'a (dyn EmbedService + 'a),
            &'a (dyn VectorRepository + 'a),
            &'a str,               // label
            &'a str,               // id_field
            &'a str,               // id_value
            &'a serde_json::Value, // properties
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), DtError>> + Send + 'a>,
        > = |g, e, v, lbl, fid, vid, p| Box::pin(embed_kg_node(g, e, v, lbl, fid, vid, p));
        let _ = wrapper;
    }

    // ------------------------------------------------------------------
    // embed_kg_node——行为测试（C1 回归）
    //
    // 验证 embed_kg_node：
    //   1. 发起 read_query 获取真实的 Memgraph elementId
    //   2. 调用 embed_batch
    //   3. upsert 的 Qdrant point 的 payload "elementId" 是真实的
    //      Memgraph 元素 id（格式 "4:xxx:yyy"）——而非先前破坏
    //      `expand_nodes` 查找的合成 "<label>/<id_field>=<id_value>" 字符串。
    //   4. 发起 write_query 标记 _kg_synced_at。
    // ------------------------------------------------------------------

    /// 模拟图：返回固定的真实 Memgraph elementId，并统计写入次数。
    struct MockGraph {
        write_count: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            // 模拟 Memgraph Bolt 响应：行对象数组，在 "eid" 键下携带
            // 真实 elementId（与获取 Cypher 匹配：
            //   MATCH (n:Knowledge {knowledge_id: $value}) RETURN elementId(n) AS eid
            Ok(serde_json::json!([{"eid": "4:1:abc123"}]))
        }
        async fn write_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            *self.write_count.lock().unwrap() += 1;
            Ok(serde_json::json!([]))
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// 模拟向量化：返回单个 1024 维向量。
    struct MockEmbed;

    #[async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(vec![vec![0.1_f32; 1024]])
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// 模拟向量库：捕获 upsert 的 Qdrant points 供检查。
    struct MockVector {
        upserted: std::sync::Mutex<Vec<serde_json::Value>>,
    }

    #[async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(vec![])
        }
        async fn upsert(&self, _c: &str, points: Vec<serde_json::Value>) -> Result<(), DtError> {
            self.upserted.lock().unwrap().extend(points);
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(&self, _n: &str) -> Result<CollectionInfo, DtError> {
            Ok(CollectionInfo {
                name: "kg_nodes".to_string(),
                points_count: 0,
                vector_dim: 1024,
                model_version: "bge-m3".to_string(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn embed_kg_node_fetches_real_element_id_and_embeds() {
        let graph = MockGraph {
            write_count: std::sync::Mutex::new(0),
        };
        let embed = MockEmbed;
        let vector = MockVector {
            upserted: std::sync::Mutex::new(vec![]),
        };

        let props = serde_json::json!({
            "name": "test",
            "summary": "test summary",
            "title": "Test Knowledge"
        });
        let result = embed_kg_node(
            &graph as &dyn GraphRepository,
            &embed as &dyn EmbedService,
            &vector as &dyn VectorRepository,
            "Knowledge",
            "knowledge_id",
            "dt://knowledge/test/test/test",
            &props,
        )
        .await;

        assert!(result.is_ok(), "embed_kg_node 应成功: {:?}", result.err());

        // 1. write_query 必须被调用一次（用于标记 _kg_synced_at）。
        assert_eq!(
            *graph.write_count.lock().unwrap(),
            1,
            "标记查询应恰好被调用一次"
        );

        // 2. 应恰好 upsert 一个 Qdrant point。
        let upserted = vector.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 1, "应 upsert 一个 point");

        // 3. payload 的 "elementId" 字段必须是模拟 read query 返回的真实
        //    Memgraph 元素 id——而非先前实现写入的合成
        //    `Knowledge/knowledge_id=dt://knowledge/test/test/test` 字符串
        //    （该字符串曾破坏 `expand_nodes` 查找）。
        let payload = upserted[0].get("payload").expect("point 必须包含 payload");
        let element_id = payload
            .get("elementId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            element_id, "4:1:abc123",
            "应使用真实的 Memgraph elementId；实际为: {element_id}"
        );
    }

    // ------------------------------------------------------------------
    // build_search_text——为 `embed_kg_node`（Knowledge / Concept /
    // Experience）使用的即时向量化标签提供回归覆盖。
    // `build_search_text` 已有实现，这里加测验证其正确性。ref: brief Step 5/6
    // ------------------------------------------------------------------

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
}
