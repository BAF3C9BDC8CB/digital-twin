//! KG → Qdrant bridge — syncs V2 business-label nodes from the graph database
//! into the Qdrant vector store for semantic search.
//!
//! ## V2 Design
//!
//! Unlike V1 which synced a limited set of Infrastructure-labelled nodes,
//! V2 syncs **all business-label nodes** according to the V2 data schema:
//!
//! - **Infrastructure nodes**:  Server, Database, K8sDeployment, K8sService
//! - **Service nodes**:         Service, ServiceInstance
//! - **Nacos nodes**:           NacosConfig, NacosService, NacosNamespace,
//!   NacosGroup, NacosInstance
//! - **Knowledge nodes**:       Knowledge, Concept, Playbook, Experience, Domain
//! - **Document nodes**:        Document, Endpoint, ConfigKey, Table
//! - **Event nodes**:           Deployment, ConfigChange, BugFix, Decision, PodEvent
//! - **Cross-cutting**:         Thread, Requirement
//!
//! ## Sync modes
//!
//! - **Full sync** (`sync_all`):  re-syncs every node regardless of timestamp.
//! - **Incremental sync** (`sync_incremental`): only syncs nodes where
//!   `_kg_synced_at IS NULL` (i.e. newly created or mutated since last sync).
//!
//! ## Search text construction
//!
//! Each node type has its own `build_search_text` logic that concatenates
//! the most semantically meaningful properties for embedding. This ensures
//! that vector search returns relevant results per entity type.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

use super::traits::SyncReport;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Qdrant collection name for KG node vectors.
const KG_COLLECTION: &str = "kg_nodes";

/// Default vector dimension (BGE-M3 = 1024).
const VECTOR_DIM: u32 = 1024;

/// Batch size for embedding + upsert — balances throughput with GPU memory.
/// fp16 BGE-M3 ~1.2 GB + 128×512×1024 activations ~128 MB/batch.
const BATCH_SIZE: usize = 128;

/// Number of concurrent batches for pipelined embed→upsert.
/// 2 concurrent on 8GB GPU is safe with fp16.
const CONCURRENCY: usize = 2;

/// V2 business labels that are synced to Qdrant for semantic search.
///
/// These cover all entity types defined in the V2 data schema that carry
/// semantically searchable content.  Ephemeral / structural labels
/// (e.g. `Project`, `Module`, `Method`, `Class`) are deliberately excluded
/// because they are handled by the code-index pipeline.
pub const BUSINESS_LABELS: &[&str] = &[
    // -- Infrastructure --
    "Server",
    "Database",
    "K8sDeployment",
    "K8sService",
    // -- Service registry --
    "Service",
    "ServiceInstance",
    // -- Nacos --
    "NacosConfig",
    "NacosService",
    "NacosNamespace",
    "NacosGroup",
    "NacosInstance",
    // -- Knowledge --
    "Knowledge",
    "Concept",
    "Playbook",
    "Experience",
    "Domain",
    // -- Documents & data --
    "Document",
    "Endpoint",
    "ConfigKey",
    "ConfigSection",
    "Table",
    // -- Events --
    "ConfigChange",
    "BugFix",
    "Decision",
    "PodEvent",
    // -- Cross-cutting --
    "Thread",
    "Requirement",
];

// ---------------------------------------------------------------------------
// KgNode — raw row from the graph database
// ---------------------------------------------------------------------------

/// A single node row returned by the fetch Cypher query.
///
/// Each row contains: `[node_properties, elementId, labels]`.
#[derive(Debug, Clone)]
pub(crate) struct KgNode {
    /// Graph element ID (Memgraph node ID) (used as the source for point-id hashing).
    element_id: String,
    /// All labels on this node (e.g. `["Server", "Infrastructure"]`).
    labels: Vec<String>,
    /// Full property map from the node.
    properties: serde_json::Value,
}

// ---------------------------------------------------------------------------
// KgBridge
// ---------------------------------------------------------------------------

/// Bridges the graph database to Qdrant by embedding business-label
/// nodes and upserting them as vectors for semantic search.
///
/// # Example
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
    /// Optional global queue for priority-aware embedding.
    /// When present, `process_batch` routes through the queue (LOW lane).
    queue: Option<Arc<super::queue::VectorQueue>>,
}

impl KgBridge {
    /// Create a new bridge wired to the three backend services.
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

    /// Attach a global VectorQueue for priority-aware embedding.
    ///
    /// When set, `process_batch` routes embed calls through the queue's
    /// LOW-priority lane so background sync yields to user searches.
    pub fn with_queue(mut self, queue: Arc<super::queue::VectorQueue>) -> Self {
        self.queue = Some(queue);
        self
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Full sync: re-embeds and upserts **every** business-label node.
    ///
    /// This is a potentially expensive operation — prefer
    /// [`sync_incremental`](Self::sync_incremental) for routine use.
    pub async fn sync_all(&self) -> Result<SyncReport, DtError> {
        self.sync_impl(false).await
    }

    /// Incremental sync: only processes nodes whose `_kg_synced_at`
    /// property is `NULL` — i.e. nodes that were created or had their
    /// sync marker reset since the last successful sync.
    pub async fn sync_incremental(&self) -> Result<SyncReport, DtError> {
        self.sync_impl(true).await
    }

    /// Detect whether config content is YAML by filename extension or content.
    fn detect_is_yaml(data_id: &str, config_type: &str, content: &str) -> bool {
        if config_type == "yaml" || config_type == "yml" {
            return true;
        }
        if config_type == "properties" || config_type == "json" || config_type == "xml" {
            return false;
        }
        // Detect from filename
        let lower = data_id.to_lowercase();
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            return true;
        }
        if lower.ends_with(".properties") || lower.ends_with(".json") || lower.ends_with(".xml") {
            return false;
        }
        // Content-based detection: YAML has top-level "key:" lines
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') { continue; }
            if trimmed.contains(':') && !trimmed.contains('=') {
                return true;
            }
            break;
        }
        false
    }

    /// Reconstruct config text in original format.
    fn reconstruct_text(name: &str, pairs: &[(String, String)], is_yaml: bool) -> String {
        if !is_yaml {
            let mut t = name.to_string();
            for (k, v) in pairs {
                t.push('\n');
                t.push_str(&format!("{}={}", k, v));
            }
            return t;
        }
        // YAML: reconstruct indented tree from dotted keys
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
            if full_parts.len() <= prefix_depth { continue; }
            let rel_parts = &full_parts[prefix_depth..];
            // Output intermediate keys (track to avoid duplicates)
            let mut cur_path = String::new();
            for (i, part) in rel_parts.iter().enumerate() {
                if !cur_path.is_empty() { cur_path.push('.'); }
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

    /// Sync adaptive config chunks from ConfigSection + ConfigKey
    /// nodes into the Qdrant `config_chunks` collection.
    ///
    /// Uses `chunk_config_adaptive` from the Nacos config content stored in
    /// NacosConfig nodes, embeds each chunk via BGE-M3, and upserts into Qdrant.
    pub async fn sync_config_chunks(&self) -> Result<SyncReport, DtError> {
        let start = Instant::now();

        // Fetch NacosConfig nodes with their content
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

        tracing::info!("[config_chunks] chunking and vectorising {} configs", total);

        let mut chunk_count = 0usize;
        let collection = "config_chunks";
        self.vector.ensure_collection(collection, 1024).await?;

        // Use chunk_config_adaptive from our chunker module
        use crate::shared::chunker::chunk_config_adaptive;

        for cfg in &configs {
            let content = cfg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let config_id = cfg.get("config_id").and_then(|v| v.as_str()).unwrap_or("");
            let data_id = cfg.get("data_id").and_then(|v| v.as_str()).unwrap_or("");
            let group = cfg.get("group").and_then(|v| v.as_str()).unwrap_or("DEFAULT_GROUP");
            let config_type = cfg.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let namespace = cfg.get("namespace").and_then(|v| v.as_str()).unwrap_or("");

            if content.is_empty() { continue; }

            let is_yaml = Self::detect_is_yaml(data_id, config_type, content);
            let sections = chunk_config_adaptive(content, is_yaml);
            if sections.is_empty() { continue; }

            // Build chunk texts and embed
            let texts: Vec<String> = sections.iter().map(|(name, pairs)| {
                Self::reconstruct_text(name, pairs, is_yaml)
            }).collect();

            let vectors = match self.embed.embed_batch(&texts).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("[config_chunks] embed failed for {}: {}", data_id, e);
                    continue;
                }
            };

            // Build Qdrant points
            let points: Vec<serde_json::Value> = sections.iter().zip(vectors.iter())
                .map(|((section_name, pairs), vec)| {
                    let text = Self::reconstruct_text(section_name, pairs, is_yaml);
                    serde_json::json!({
                        "id": format!("{}#{}", config_id, section_name),
                        "vector": vec,
                        "payload": {
                            "section_name": section_name,
                            "namespace": namespace,
                            "data_id": data_id,
                            "group": group,
                            "config_type": config_type,
                            "text": text,
                            "source_type": "config_chunk",
                            "key_count": pairs.len(),
                        }
                    })
                })
                .collect();

            if let Err(e) = self.vector.upsert(collection, points.clone()).await {
                tracing::warn!("[config_chunks] upsert failed for {}: {}", data_id, e);
            } else {
                chunk_count += points.len();
            }
        }

        let elapsed = start.elapsed();
        tracing::info!(
            "[config_chunks] done: {} chunks from {} configs ({:.1}s)",
            chunk_count, total, elapsed.as_secs_f64()
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
    // Single-node sync — for auto-sync after write
    // ------------------------------------------------------------------

    /// Sync a single KG node to Qdrant, looked up by (label, property_key, value).
    ///
    /// This is used for **auto-sync after write**: after a node is created or
    /// mutated via `dt memorize` / `dt learn` / `dt event`, this method fetches
    /// the node from the graph database and syncs it into Qdrant immediately — so the vector
    /// index is always current without needing a manual `dt kg-sync`.
    ///
    /// If the node isn't found (e.g. the label isn't in [`BUSINESS_LABELS`]),
    /// it silently succeeds — not every graph node needs to be in Qdrant.
    ///
    /// # Arguments
    /// - `label` — graph label (e.g. `"Knowledge"`, `"Experience"`, `"Decision"`).
    /// - `prop_key` — the property used to look up the node (e.g. `"knowledge_id"`).
    /// - `prop_value` — the property value.
    pub async fn sync_node_by_property(
        &self,
        label: &str,
        prop_key: &str,
        prop_value: &str,
    ) -> Result<(), DtError> {
        // Only sync business-label nodes.
        if !BUSINESS_LABELS.contains(&label) {
            tracing::debug!("[kg-sync] skip non-business label: {label}");
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
            tracing::debug!(
                "[kg-sync] node not found: label={label} {prop_key}={prop_value}"
            );
            return Ok(());
        }

        self.process_batch(&nodes).await?;

        tracing::debug!(
            "[kg-sync] auto-synced 1 node: label={label} {prop_key}={prop_value}",
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal (exposed for BatchAccumulator)
    // ------------------------------------------------------------------

    /// Fetch a single business-label node from Memgraph by (label, key, value).
    ///
    /// Returns `None` if the node isn't found or isn't a business label.
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
    // Implementation
    // ------------------------------------------------------------------

    /// Shared sync logic — `incremental` controls the Cypher WHERE clause.
    async fn sync_impl(&self, incremental: bool) -> Result<SyncReport, DtError> {
        let start = Instant::now();
        let mode = if incremental { "incremental" } else { "full" };

        tracing::info!("[kg-sync] starting {} sync (BATCH_SIZE={BATCH_SIZE}, CONCURRENCY={CONCURRENCY})", mode);

        // 1.  Ensure the Qdrant collection exists.
        self.vector
            .ensure_collection(KG_COLLECTION, VECTOR_DIM)
            .await?;

        // 2.  Fetch nodes from the graph database.
        let nodes = self.fetch_nodes(incremental).await?;

        if nodes.is_empty() {
            tracing::info!("[kg-sync] no nodes to sync");
            return Ok(SyncReport {
                source: format!("kg-sync/{mode}"),
                ..SyncReport::default()
            });
        }

        let total = nodes.len();
        tracing::info!("[kg-sync] fetched {total} nodes");

        let mut synced: usize = 0;
        let mut failed: usize = 0;
        let mut errors: Vec<String> = Vec::new();

        // 3.  Process in batches with concurrent pipelining (if >1 batch).
        if total <= BATCH_SIZE || CONCURRENCY < 2 {
            // Single batch or no concurrency — sequential.
            for chunk in nodes.chunks(BATCH_SIZE) {
                match self.process_batch(chunk).await {
                    Ok(count) => synced += count,
                    Err(e) => {
                        failed += chunk.len();
                        errors.push(format!("batch error: {e}"));
                    }
                }
            }
        } else {
            // Multiple batches — pipelined concurrency.
            use futures::stream::{self, StreamExt};

            let embed = self.embed.clone();
            let vector = self.vector.clone();
            let graph = self.graph.clone();

            let chunks: Vec<Vec<KgNode>> = nodes
                .chunks(BATCH_SIZE)
                .map(|c| c.to_vec())
                .collect();

            tracing::info!(
                "[kg-sync] pipelining {} batches x {} nodes, {} concurrent",
                chunks.len(), BATCH_SIZE, CONCURRENCY,
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
                        errors.push(format!("batch error: {e}"));
                    }
                }
            }
        }

        let elapsed = start.elapsed().as_millis() as u64;

        tracing::info!(
            "[kg-sync] complete — {synced}/{total} synced, {failed} failed ({elapsed}ms)",
        );

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

    /// Process a single batch: embed → upsert → mark synced.
    pub(crate) async fn process_batch(&self, chunk: &[KgNode]) -> Result<usize, DtError> {
        // (a) Build search texts from node properties.
        let texts: Vec<String> = chunk.iter().map(build_search_text).collect();

        // (b) Generate embeddings.
        let vectors = self.embed.embed_batch(&texts).await?;

        // (c) Build Qdrant points.
        let points: Vec<serde_json::Value> = chunk
            .iter()
            .zip(vectors.iter())
            .map(|(node, vec)| build_qdrant_point(node, vec))
            .collect();

        // (d) Upsert to Qdrant.
        self.vector.upsert(KG_COLLECTION, points).await?;

        // (e) Mark nodes as synced in Memgraph.
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

    /// Fetch business-label nodes from Memgraph.
    ///
    /// When `incremental` is `true`, only nodes without `_kg_synced_at`
    /// are returned.
    async fn fetch_nodes(&self, incremental: bool) -> Result<Vec<KgNode>, DtError> {
        // Build label OR-clause:  n:Server OR n:Database OR n:K8sDeployment OR ...
        let label_conds: Vec<String> = BUSINESS_LABELS
            .iter()
            .map(|l| format!("n:{l}"))
            .collect();
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

/// Owned version of process_batch for use in concurrent streams.
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
// Search text construction — per label type
// ---------------------------------------------------------------------------

/// Build a free-text search string from a KG node's properties.
///
/// The choice of properties is label-aware so that the resulting
/// embedding captures the most discriminative information for each
/// entity type.
pub(crate) fn build_search_text(node: &KgNode) -> String {
    let props = &node.properties;
    let primary_label = node
        .labels
        .iter()
        .find(|l| BUSINESS_LABELS.contains(&l.as_str()))
        .map(|s| s.as_str())
        .unwrap_or("Unknown");

    match primary_label {
        // ── Infrastructure ──────────────────────────────────────────
        "Server" => concat_props(props, &["name", "service_type", "hostname", "description", "environment"]),
        "Database" => concat_props(props, &["name", "db_type", "host", "description", "environment"]),
        "K8sDeployment" => concat_props(props, &["name", "namespace", "image", "description", "environment"]),
        "K8sService" => concat_props(props, &["name", "namespace", "cluster_ip", "description", "environment"]),

        // ── Service registry ───────────────────────────────────────
        "Service" => concat_props(props, &["name", "service_name", "hostname", "port", "description", "environment"]),
        "ServiceInstance" => concat_props(props, &["instance_id", "service_name", "host", "port", "environment"]),

        // ── Nacos ──────────────────────────────────────────────────
        "NacosConfig" => concat_props(props, &["data_id", "group", "namespace", "content"]),
        "NacosService" => concat_props(props, &["service_name", "group_name", "namespace", "description"]),
        "NacosNamespace" => concat_props(props, &["namespace", "description"]),
        "NacosGroup" => concat_props(props, &["group_name", "namespace", "description"]),
        "NacosInstance" => concat_props(props, &["instance_id", "service_name", "ip", "port", "namespace"]),

        // ── Knowledge ──────────────────────────────────────────────
        "Knowledge" => concat_props(props, &["name", "title", "domain", "summary", "content"]),
        "Concept" => concat_props(props, &["name", "definition", "domain", "description"]),
        "Playbook" => concat_props(props, &["name", "title", "description", "domain"]),
        "Experience" => concat_props(props, &["name", "title", "description", "domain"]),
        "Domain" => concat_props(props, &["name", "description"]),

        // ── Documents & data ───────────────────────────────────────
        "Document" => concat_props(props, &["title", "content", "source_file", "description"]),
        "Endpoint" => concat_props(props, &["path", "method", "controller", "description", "project"]),
        "ConfigKey" => concat_props(props, &["name", "value", "data_id", "namespace", "description"]),
        "ConfigSection" => concat_props(props, &["section_id", "name", "summary", "namespace", "data_id", "config_type"]),
        "Table" => concat_props(props, &["table_name", "db_type", "description", "columns"]),

        // ── Events ─────────────────────────────────────────────────
        "Deployment" => concat_props(props, &["name", "env", "branch", "description"]),
        "ConfigChange" => concat_props(props, &["name", "data_id", "description", "summary"]),
        "BugFix" => concat_props(props, &["title", "file", "description", "summary"]),
        "Decision" => concat_props(props, &["title", "decision", "reason", "scope", "description"]),
        "PodEvent" => concat_props(props, &["pod_name", "namespace", "reason", "message", "description"]),

        // ── Cross-cutting ──────────────────────────────────────────
        "Thread" => concat_props(props, &["title", "description", "domain", "tags"]),
        "Requirement" => concat_props(props, &["title", "description", "status", "domain"]),

        // ── Fallback ───────────────────────────────────────────────
        _ => concat_props(props, &["name", "title", "description", "summary", "content"]),
    }
}

/// Concatenate non-empty property values (in order) into a space-separated
/// search text string.
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
                _ => { /* skip nulls, bools, arrays, objects */ }
            }
        }
    }

    parts.join(" ")
}

// ---------------------------------------------------------------------------
// Qdrant point construction
// ---------------------------------------------------------------------------

/// Build a Qdrant point JSON value from a KG node and its embedding vector.
fn build_qdrant_point(node: &KgNode, vector: &[f32]) -> serde_json::Value {
    let point_id = make_point_id(&node.element_id);
    let payload = build_payload(node);

    serde_json::json!({
        "id": point_id,
        "vector": vector,
        "payload": payload,
    })
}

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

/// Build the Qdrant payload from node properties.
///
/// The payload contains lightweight metadata for filtering and display.
fn build_payload(node: &KgNode) -> serde_json::Value {
    let props = &node.properties;

    serde_json::json!({
        "elementId": node.element_id,
        "name": props.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "labels": node.labels,
        "service_type": props.get("service_type").cloned().unwrap_or(serde_json::Value::Null),
        "environment": props.get("environment").cloned().unwrap_or(serde_json::Value::Null),
        "description": props.get("description")
            .and_then(|v| v.as_str())
            .map(|s| &s[..s.len().min(200)])
            .unwrap_or(""),
        "source": "kg",
    })
}

/// Generate a deterministic UUID v4 from a graph element ID via SHA-256.
///
/// This ensures the same elementId always maps to the same Qdrant point
/// ID across sync runs, allowing idempotent upserts.
fn make_point_id(element_id: &str) -> String {
    let hash = Sha256::digest(element_id.as_bytes());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]),
        u16::from_be_bytes([hash[4], hash[5]]),
        u16::from_be_bytes([hash[6], hash[7]]) & 0x0fff,
        u16::from_be_bytes([hash[8], hash[9]]) & 0x3fff | 0x8000,
        u64::from_be_bytes([
            hash[10], hash[11], hash[12], hash[13],
            hash[14], hash[15], 0, 0,
        ]) >> 16,
    )
}

// ---------------------------------------------------------------------------
// Graph result parsing
// ---------------------------------------------------------------------------

/// Parse the raw graph response JSON into a `Vec<KgNode>`.
///
/// Handles two response formats:
    /// 1. **Bolt driver** — `Value::Array` of row objects:
///    ```json
///    [{"n": {...}, "eid": "4:...", "lbls": ["Server"]}]
///    ```
/// 2. **HTTP API** (legacy fallback):
///    ```json
///    {"results":[{"columns":["n","eid","lbls"],"data":[{"row":[{...},"4:...",["Server"]]}]}]}
///    ```
fn parse_graph_rows(raw: &serde_json::Value) -> Result<Vec<KgNode>, DtError> {
    // Try Bolt driver format first (Array of row objects).
    if let Some(rows) = raw.as_array() {
        return parse_bolt_rows(rows);
    }

    // Fall back to HTTP API format.
    let rows = raw
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|results| results.first())
        .and_then(|first| first.get("data"))
        .and_then(|data| data.as_array())
        .ok_or_else(|| DtError::Repository("missing 'results[0].data' in graph response".into()))?;

    let mut nodes: Vec<KgNode> = Vec::with_capacity(rows.len());

    for row_val in rows {
        let row = row_val
            .get("row")
            .and_then(|r| r.as_array())
            .ok_or_else(|| DtError::Repository("missing 'row' in graph data item".into()))?;

        if row.len() < 3 {
            continue;
        }

        let properties = row[0].clone();
        let element_id = row[1]
            .as_str()
            .unwrap_or("")
            .to_string();

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

/// Parse rows from the Bolt driver format (Array of JSON objects).
///
/// Each object has keys `n` (node properties), `eid` (elementId string),
/// and `lbls` (labels array).
fn parse_bolt_rows(rows: &[serde_json::Value]) -> Result<Vec<KgNode>, DtError> {
    let mut nodes: Vec<KgNode> = Vec::with_capacity(rows.len());

    for row in rows {
        let properties = row
            .get("n")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let element_id = row
            .get("eid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if element_id.is_empty() {
            // Try legacy row array format inside each row object
            if let Some(row_arr) = row.get("row").and_then(|r| r.as_array()) {
                if row_arr.len() >= 3 {
                    let props = row_arr[0].clone();
                    let eid = row_arr[1].as_str().unwrap_or("").to_string();
                    let lbls: Vec<String> = row_arr[2]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // make_point_id
    // ------------------------------------------------------------------

    #[test]
    fn make_point_id_is_deterministic() {
        let eid = "4:test-element-id-123";
        let id1 = make_point_id(eid);
        let id2 = make_point_id(eid);
        assert_eq!(id1, id2, "same elementId must produce same UUID");
    }

    #[test]
    fn make_point_id_different_per_input() {
        let id_a = make_point_id("4:aaa");
        let id_b = make_point_id("4:bbb");
        assert_ne!(id_a, id_b, "different elementIds must produce different UUIDs");
    }

    #[test]
    fn make_point_id_is_valid_uuid() {
        let id = make_point_id("4:some-element");
        // Should match UUID v4 pattern: xxxxxxxx-xxxx-4xxx-[89ab]xxx-xxxxxxxxxxxx
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5, "must have 5 dash-separated parts");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert!(parts[2].starts_with('4'), "version nibble must be 4");
        assert_eq!(parts[3].len(), 4);
        assert!(parts[3].starts_with('8') || parts[3].starts_with('9') || parts[3].starts_with('a') || parts[3].starts_with('b') || parts[3].starts_with('A') || parts[3].starts_with('B'),
            "variant bits must be 10xx");
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
    fn payload_truncates_long_description() {
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
        assert!(desc.len() <= 200);
    }

    #[test]
    fn payload_includes_all_expected_fields() {
        let node = KgNode {
            element_id: "4:abc".into(),
            labels: vec!["Database".into()],
            properties: serde_json::json!({
                "name": "mydb",
                "environment": "prod",
            }),
        };
        let payload = build_payload(&node);
        assert_eq!(payload["elementId"], "4:abc");
        assert_eq!(payload["name"], "mydb");
        assert_eq!(payload["source"], "kg");
        assert_eq!(payload["environment"], "prod");
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
        let nodes = parse_graph_rows(&raw).expect("should parse");
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
        let nodes = parse_graph_rows(&raw).expect("should parse empty");
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
            properties: serde_json::json!({"name": "api", "description": "desc"}),
        };
        let vector = vec![0.1_f32, 0.2_f32, 0.3_f32];
        let point = build_qdrant_point(&node, &vector);

        assert!(point["id"].is_string());
        assert_eq!(point["vector"].as_array().unwrap().len(), 3);
        assert_eq!(point["payload"]["name"], "api");
        assert_eq!(point["payload"]["source"], "kg");
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
        assert_eq!(labels.len(), orig_len, "BUSINESS_LABELS must have no duplicates");
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
            assert!(
                !text.is_empty(),
                "search text empty for label '{label}'"
            );
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
            &'a str,                // label
            &'a str,                // id_field
            &'a str,                // id_value
            &'a serde_json::Value,   // properties
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), DtError>> + Send + 'a>,
        > = |g, e, v, lbl, fid, vid, p| Box::pin(embed_kg_node(g, e, v, lbl, fid, vid, p));
        let _ = wrapper;
    }

    // ------------------------------------------------------------------
    // build_search_text — regression coverage for the immediate-embedding
    // labels used by `embed_kg_node` (Knowledge / Concept / Experience).
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
