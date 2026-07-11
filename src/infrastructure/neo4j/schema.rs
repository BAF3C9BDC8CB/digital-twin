//! V2 Schema initialization — constraints, indexes, and data lifecycle.
//!
//! Provides:
//! - `initialize_schema()` — creates all uniqueness constraints + fulltext indexes.
//! - `clean_all_data()` — wipes all nodes and relationships (for dev/testing).
//!
//! # Data Retention Policy (documented here, enforced via `dt cleanup`)
//!
//! | Data | TTL | Action when exceeded |
//! |------|-----|---------------------|
//! | Memory.Event (Modification, Deployment, ConfigChange, BugFix, Decision, PodEvent) | 365 days | Archive to `/var/lib/dt/archive/` |
//! | Reasoning (unverified Observation, Analysis, Decision) | Session end | `SET _stale_at = timestamp()`; `dt cleanup` deletes after 30 days |
//! | SQLite snapshots old rows | Latest only | `dt build` auto-deletes `WHERE updated_at < latest_per_file` |
//! | Qdrant orphan points | Follows Neo4j | Entity deleted → corresponding point cleaned by `dt kg-sync` |

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Report types
// ---------------------------------------------------------------------------

/// Summary of schema initialization.
#[derive(Debug, Clone)]
pub struct SchemaInitReport {
    /// Number of uniqueness constraints created (or already present).
    pub constraints_created: usize,
    /// Number of indexes created (or already present).
    pub indexes_created: usize,
    /// Wall-clock time for the entire init.
    pub elapsed_ms: u64,
}

/// Summary of data cleanup.
#[derive(Debug, Clone)]
pub struct CleanReport {
    /// Number of nodes deleted.
    pub nodes_deleted: usize,
    /// Number of relationships deleted.
    pub relationships_deleted: usize,
    /// Number of Qdrant collections removed (0 when Neo4j-only).
    pub qdrant_collections_removed: usize,
    /// Whether SQLite snapshots were cleared (false when Neo4j-only).
    pub snapshots_cleared: bool,
    /// Number of stale Reasoning nodes deleted (Observation/Analysis/Decision).
    pub reasoning_stale_deleted: usize,
    /// Number of Memory World events archived (Modification, Deployment, etc.).
    pub memory_archived: usize,
    /// Number of orphaned SQLite snapshot rows deleted.
    pub snapshots_orphaned: usize,
    /// Wall-clock time.
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Constraint definitions (Cypher IF NOT EXISTS — idempotent)
// ---------------------------------------------------------------------------

/// All V2 entity uniqueness constraints.
///
/// Each entity type gets a unique ID constraint. Composite constraints use
/// `(propA, propB) IS UNIQUE` notation (Neo4j 5.x+).
const CONSTRAINT_STATEMENTS: &[&str] = &[
    // ── Reality World: Code entities ──
    "CREATE CONSTRAINT method_id_unique IF NOT EXISTS FOR (n:Method) REQUIRE n.method_id IS UNIQUE",
    "CREATE CONSTRAINT class_id_unique IF NOT EXISTS FOR (n:Class) REQUIRE n.class_id IS UNIQUE",
    "CREATE CONSTRAINT module_id_unique IF NOT EXISTS FOR (n:Module) REQUIRE n.module_id IS UNIQUE",
    // ── Reality World: Infrastructure ──
    "CREATE CONSTRAINT server_id_unique IF NOT EXISTS FOR (n:Server) REQUIRE n.server_id IS UNIQUE",
    "CREATE CONSTRAINT database_id_unique IF NOT EXISTS FOR (n:Database) REQUIRE n.database_id IS UNIQUE",
    "CREATE CONSTRAINT table_name_db_unique IF NOT EXISTS FOR (n:Table) REQUIRE (n.name, n.db) IS UNIQUE",
    // ── Reality World: Configuration ──
    "CREATE CONSTRAINT nacos_config_id_unique IF NOT EXISTS FOR (n:NacosConfig) REQUIRE n.config_id IS UNIQUE",
    "CREATE CONSTRAINT config_key_name_ns_unique IF NOT EXISTS FOR (n:ConfigKey) REQUIRE (n.name, n.namespace) IS UNIQUE",
    // ── Reality World: API ──
    "CREATE CONSTRAINT endpoint_id_unique IF NOT EXISTS FOR (n:Endpoint) REQUIRE n.endpoint_id IS UNIQUE",
    // ── Reality World: Document ──
    "CREATE CONSTRAINT doc_id_unique IF NOT EXISTS FOR (n:Document) REQUIRE n.doc_id IS UNIQUE",
    // ── Reality World: Service / K8s ──
    "CREATE CONSTRAINT service_id_unique IF NOT EXISTS FOR (n:Service) REQUIRE n.service_id IS UNIQUE",
    "CREATE CONSTRAINT service_instance_id_unique IF NOT EXISTS FOR (n:ServiceInstance) REQUIRE n.instance_id IS UNIQUE",
    "CREATE CONSTRAINT k8s_deployment_name_ns_unique IF NOT EXISTS FOR (n:K8sDeployment) REQUIRE (n.name, n.namespace) IS UNIQUE",
    "CREATE CONSTRAINT k8s_service_name_ns_unique IF NOT EXISTS FOR (n:K8sService) REQUIRE (n.name, n.namespace) IS UNIQUE",
    // ── Knowledge World ──
    "CREATE CONSTRAINT knowledge_id_unique IF NOT EXISTS FOR (n:Knowledge) REQUIRE n.knowledge_id IS UNIQUE",
    "CREATE CONSTRAINT knowledge_version_id_unique IF NOT EXISTS FOR (n:KnowledgeVersion) REQUIRE n.version_id IS UNIQUE",
    "CREATE CONSTRAINT playbook_id_unique IF NOT EXISTS FOR (n:Playbook) REQUIRE n.playbook_id IS UNIQUE",
    "CREATE CONSTRAINT experience_id_unique IF NOT EXISTS FOR (n:Experience) REQUIRE n.experience_id IS UNIQUE",
    "CREATE CONSTRAINT concept_id_unique IF NOT EXISTS FOR (n:Concept) REQUIRE n.concept_id IS UNIQUE",
    "CREATE CONSTRAINT domain_id_unique IF NOT EXISTS FOR (n:Domain) REQUIRE n.domain_id IS UNIQUE",
    // ── Memory World ──
    "CREATE CONSTRAINT day_id_unique IF NOT EXISTS FOR (n:Day) REQUIRE n.day_id IS UNIQUE",
    "CREATE CONSTRAINT session_id_unique IF NOT EXISTS FOR (n:Session) REQUIRE n.session_id IS UNIQUE",
    "CREATE CONSTRAINT pod_event_id_unique IF NOT EXISTS FOR (n:PodEvent) REQUIRE n.event_id IS UNIQUE",
    // ── Digital Thread ──
    "CREATE CONSTRAINT thread_id_unique IF NOT EXISTS FOR (n:Thread) REQUIRE n.thread_id IS UNIQUE",
    "CREATE CONSTRAINT requirement_id_unique IF NOT EXISTS FOR (n:Requirement) REQUIRE n.requirement_id IS UNIQUE",
    // ── Reasoning World ──
    "CREATE CONSTRAINT observation_id_unique IF NOT EXISTS FOR (n:Observation) REQUIRE n.observation_id IS UNIQUE",
    "CREATE CONSTRAINT analysis_id_unique IF NOT EXISTS FOR (n:Analysis) REQUIRE n.analysis_id IS UNIQUE",
];

/// Full-text index covering infrastructure and knowledge search across labels.
///
/// Covers: Server, Database, NacosConfig, NacosService, K8sDeployment, K8sService,
/// Service, ServiceInstance, Knowledge, Concept, Experience, Playbook, Document, Thread.
///
/// Indexed properties: name, description, hostname, url, auth_user.
const FULLTEXT_INDEX_STATEMENT: &str = r#"
CREATE FULLTEXT INDEX infra_search IF NOT EXISTS
FOR (n:Server|Database|NacosConfig|NacosService|K8sDeployment|K8sService|Service|ServiceInstance|Method|Class|Module|Knowledge|Concept|Experience|Playbook|Document|Thread|ConfigKey|Endpoint)
ON EACH [n.name, n.description, n.hostname, n.url, n.auth_user, n.signature, n.file_path, n.package_or_module, n.data_id, n.title, n.summary, n.definition]
"#;

// ---------------------------------------------------------------------------
// Schema initialization
// ---------------------------------------------------------------------------

/// Initialize the complete V2 schema.
///
/// Creates all uniqueness constraints and the full-text index. All statements
/// use `IF NOT EXISTS` so the function is safe to call repeatedly (idempotent).
///
/// # Arguments
/// * `graph` — any [`GraphRepository`] implementation (real Neo4j or noop mock).
///
/// # Returns
/// [`SchemaInitReport`] summarising what was created and how long it took.
pub async fn initialize_schema(graph: &dyn GraphRepository) -> Result<SchemaInitReport, DtError> {
    let start = std::time::Instant::now();
    let empty_params = HashMap::new();

    let mut constraints_created = 0usize;
    let mut indexes_created = 0usize;

    // --- uniqueness constraints ---
    for stmt in CONSTRAINT_STATEMENTS {
        graph.write_query(stmt, empty_params.clone()).await?;
        constraints_created += 1;
    }

    // --- full-text index ---
    graph
        .write_query(FULLTEXT_INDEX_STATEMENT, empty_params)
        .await?;
    indexes_created += 1;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    Ok(SchemaInitReport {
        constraints_created,
        indexes_created,
        elapsed_ms,
    })
}

// ---------------------------------------------------------------------------
// Data cleanup
// ---------------------------------------------------------------------------

/// Wipe **all** nodes and relationships from the graph.
///
/// # Safety
/// This is a destructive operation. Use with caution — typically only
/// in development/testing environments. Production use should go through
/// `dt cleanup --confirm`.
///
/// Before deleting, the current node and relationship counts are captured
/// so the caller can report what was removed.
pub async fn clean_all_data(graph: &dyn GraphRepository) -> Result<CleanReport, DtError> {
    let start = std::time::Instant::now();
    let empty_params = HashMap::new();

    // Count nodes before deletion
    let nodes_deleted = count_nodes(graph).await.unwrap_or(0);

    // Count relationships before deletion
    let relationships_deleted = count_relationships(graph).await.unwrap_or(0);

    // Delete everything
    graph
        .write_query("MATCH (n) DETACH DELETE n", empty_params)
        .await?;

    Ok(CleanReport {
        nodes_deleted,
        relationships_deleted,
        qdrant_collections_removed: 0,
        snapshots_cleared: false,
        reasoning_stale_deleted: 0,
        memory_archived: 0,
        snapshots_orphaned: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count all nodes in the graph.
async fn count_nodes(graph: &dyn GraphRepository) -> Result<usize, DtError> {
    let result = graph
        .read_query("MATCH (n) RETURN count(n) AS total", HashMap::new())
        .await?;
    Ok(extract_count(&result, "total"))
}

/// Count all relationships in the graph.
async fn count_relationships(graph: &dyn GraphRepository) -> Result<usize, DtError> {
    let result = graph
        .read_query(
            "MATCH ()-[r]->() RETURN count(r) AS total",
            HashMap::new(),
        )
        .await?;
    Ok(extract_count(&result, "total"))
}

/// Extract a `usize` from a JSON result — handles both array-of-rows and scalar.
fn extract_count(value: &serde_json::Value, field: &str) -> usize {
    value
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::traits::GraphRepository;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;

    /// Mock repository that records queries for assertion.
    struct MockGraphRepo {
        write_calls: std::sync::Mutex<Vec<String>>,
        read_calls: std::sync::Mutex<Vec<String>>,
        should_fail_after: Option<usize>,
    }

    impl MockGraphRepo {
        fn new() -> Self {
            Self {
                write_calls: std::sync::Mutex::new(Vec::new()),
                read_calls: std::sync::Mutex::new(Vec::new()),
                should_fail_after: None,
            }
        }
    }

    #[async_trait]
    impl GraphRepository for MockGraphRepo {
        async fn read_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.read_calls.lock().unwrap().push(query.to_string());
            // Return a COUNT result of 0 by default
            Ok(serde_json::json!([{"total": 0}]))
        }

        async fn write_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            let mut calls = self.write_calls.lock().unwrap();
            calls.push(query.to_string());

            if let Some(limit) = self.should_fail_after {
                if calls.len() > limit {
                    return Err(DtError::Repository("mock failure".into()));
                }
            }
            Ok(serde_json::json!({"ok": true}))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn init_schema_creates_all_constraints() {
        let mock = MockGraphRepo::new();
        let report = initialize_schema(&mock).await.expect("should succeed");

        // 27 constraints + 1 fulltext index
        assert_eq!(report.constraints_created, 27);
        assert_eq!(report.indexes_created, 1);
        assert!(report.elapsed_ms < 5_000);

        let write_calls = mock.write_calls.lock().unwrap();
        assert_eq!(write_calls.len(), 28); // 27 constraints + 1 index
        assert!(write_calls[0].contains("method_id_unique"));
        assert!(write_calls[26].contains("analysis_id_unique"));
        assert!(write_calls[27].contains("FULLTEXT INDEX"));
    }

    #[tokio::test]
    async fn init_schema_is_idempotent_via_if_not_exists() {
        let mock = MockGraphRepo::new();
        // First call
        initialize_schema(&mock).await.unwrap();
        // Second call — all statements have IF NOT EXISTS, so should succeed
        let report2 = initialize_schema(&mock).await.unwrap();
        assert_eq!(report2.constraints_created, 27);
        assert_eq!(report2.indexes_created, 1);
    }

    #[tokio::test]
    async fn clean_all_data_deletes_everything() {
        let mock = MockGraphRepo::new();
        let report = clean_all_data(&mock).await.expect("should succeed");

        // Nodes/relationships were 0 (mock returns 0)
        assert_eq!(report.nodes_deleted, 0);
        assert_eq!(report.relationships_deleted, 0);
        assert_eq!(report.qdrant_collections_removed, 0);
        assert!(!report.snapshots_cleared);
        assert!(report.elapsed_ms < 5_000);

        // Verify the DETACH DELETE was called
        let write_calls = mock.write_calls.lock().unwrap();
        assert!(write_calls.iter().any(|s| s.contains("DETACH DELETE n")));
    }

    #[test]
    fn extract_count_handles_empty() {
        assert_eq!(extract_count(&serde_json::json!([]), "total"), 0);
        assert_eq!(extract_count(&serde_json::Value::Null, "total"), 0);
    }

    #[test]
    fn extract_count_handles_array_of_rows() {
        assert_eq!(
            extract_count(&serde_json::json!([{"total": 42}]), "total"),
            42
        );
    }

    #[test]
    fn schema_init_report_debug() {
        let report = SchemaInitReport {
            constraints_created: 27,
            indexes_created: 1,
            elapsed_ms: 123,
        };
        let debug = format!("{report:?}");
        assert!(debug.contains("27"));
        assert!(debug.contains("1"));
        assert!(debug.contains("123"));
    }

    #[test]
    fn clean_report_debug() {
        let report = CleanReport {
            nodes_deleted: 100,
            relationships_deleted: 200,
            qdrant_collections_removed: 3,
            snapshots_cleared: true,
            reasoning_stale_deleted: 5,
            memory_archived: 50,
            snapshots_orphaned: 10,
            elapsed_ms: 456,
        };
        let debug = format!("{report:?}");
        assert!(debug.contains("100"));
        assert!(debug.contains("200"));
        assert!(debug.contains("3"));
        assert!(debug.contains("true"));
        assert!(debug.contains("5"));
        assert!(debug.contains("50"));
        assert!(debug.contains("10"));
        assert!(debug.contains("456"));
    }
}
