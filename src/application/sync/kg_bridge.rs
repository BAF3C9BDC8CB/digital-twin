//! KG → Qdrant bridge — syncs V2 business-label nodes from Neo4j into
//! the Qdrant vector store for semantic search.
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

/// Batch size for embedding + upsert — balances throughput with memory.
const BATCH_SIZE: usize = 64;

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
// KgNode — raw row from Neo4j
// ---------------------------------------------------------------------------

/// A single node row returned by the fetch Cypher query.
///
/// Each row contains: `[node_properties, elementId, labels]`.
#[derive(Debug, Clone)]
pub(crate) struct KgNode {
    /// Neo4j elementId (used as the source for point-id hashing).
    element_id: String,
    /// All labels on this node (e.g. `["Server", "Infrastructure"]`).
    labels: Vec<String>,
    /// Full property map from the node.
    properties: serde_json::Value,
}

// ---------------------------------------------------------------------------
// KgBridge
// ---------------------------------------------------------------------------

/// Bridges the Neo4j knowledge graph to Qdrant by embedding business-label
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
        }
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

    // ------------------------------------------------------------------
    // Implementation
    // ------------------------------------------------------------------

    /// Shared sync logic — `incremental` controls the Cypher WHERE clause.
    async fn sync_impl(&self, incremental: bool) -> Result<SyncReport, DtError> {
        let start = Instant::now();
        let mode = if incremental { "incremental" } else { "full" };

        tracing::info!("[kg-sync] starting {} sync", mode);

        // 1.  Ensure the Qdrant collection exists.
        self.vector
            .ensure_collection(KG_COLLECTION, VECTOR_DIM)
            .await?;

        // 2.  Fetch nodes from Neo4j.
        let nodes = self.fetch_nodes(incremental).await?;

        if nodes.is_empty() {
            tracing::info!("[kg-sync] no nodes to sync");
            return Ok(SyncReport {
                source: format!("kg-sync/{mode}"),
                ..SyncReport::default()
            });
        }

        let total = nodes.len();
        tracing::info!("[kg-sync] fetched {total} nodes to sync");

        let mut synced: usize = 0;
        let mut failed: usize = 0;
        let mut errors: Vec<String> = Vec::new();

        // 3.  Process in batches.
        for chunk in nodes.chunks(BATCH_SIZE) {
            match self.process_batch(chunk).await {
                Ok(count) => synced += count,
                Err(e) => {
                    failed += chunk.len();
                    errors.push(format!("batch error: {e}"));
                    tracing::warn!("[kg-sync] batch failed ({} nodes): {e}", chunk.len());
                }
            }
        }

        let elapsed = start.elapsed().as_millis() as u64;

        tracing::info!(
            "[kg-sync] complete — {synced}/{total} synced, {failed} failed ({elapsed}ms)"
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
    async fn process_batch(&self, chunk: &[KgNode]) -> Result<usize, DtError> {
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

        // (e) Mark nodes as synced in Neo4j.
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

    /// Fetch business-label nodes from Neo4j.
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

        let nodes = parse_neo4j_rows(&result)?;
        Ok(nodes)
    }
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
        "ConfigKey" => concat_props(props, &["key", "value", "data_id", "namespace", "description"]),
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

/// Generate a deterministic UUID v4 from a Neo4j elementId via SHA-256.
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
// Neo4j result parsing
// ---------------------------------------------------------------------------

/// Parse the raw Neo4j response JSON into a `Vec<KgNode>`.
///
/// Handles two response formats:
/// 1. **neo4rs driver** — `Value::Array` of row objects:
///    ```json
///    [{"n": {...}, "eid": "4:...", "lbls": ["Server"]}]
///    ```
/// 2. **Neo4j HTTP API** (legacy fallback):
///    ```json
///    {"results":[{"columns":["n","eid","lbls"],"data":[{"row":[{...},"4:...",["Server"]]}]}]}
///    ```
fn parse_neo4j_rows(raw: &serde_json::Value) -> Result<Vec<KgNode>, DtError> {
    // Try neo4rs driver format first (Array of row objects).
    if let Some(rows) = raw.as_array() {
        return parse_neo4rs_rows(rows);
    }

    // Fall back to Neo4j HTTP API format.
    let rows = raw
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|results| results.first())
        .and_then(|first| first.get("data"))
        .and_then(|data| data.as_array())
        .ok_or_else(|| DtError::Repository("missing 'results[0].data' in Neo4j response".into()))?;

    let mut nodes: Vec<KgNode> = Vec::with_capacity(rows.len());

    for row_val in rows {
        let row = row_val
            .get("row")
            .and_then(|r| r.as_array())
            .ok_or_else(|| DtError::Repository("missing 'row' in Neo4j data item".into()))?;

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

/// Parse rows from the neo4rs driver format (Array of JSON objects).
///
/// Each object has keys `n` (node properties), `eid` (elementId string),
/// and `lbls` (labels array).
fn parse_neo4rs_rows(rows: &[serde_json::Value]) -> Result<Vec<KgNode>, DtError> {
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
    // parse_neo4j_rows
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
        let nodes = parse_neo4j_rows(&raw).expect("should parse");
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
        let nodes = parse_neo4j_rows(&raw).expect("should parse empty");
        assert!(nodes.is_empty());
    }

    #[test]
    fn parse_missing_results_returns_error() {
        let raw = serde_json::json!({"unexpected": "format"});
        let result = parse_neo4j_rows(&raw);
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
}
