//! Neo4j graph repository — schema init entry point and future Neo4j client.
//!
//! Currently provides:
//! - Schema initialization (constraints, indexes) via `init_schema()`.
//! - Data cleanup via `clean_all()`.
//!
//! The real `Neo4jGraphRepo` (wrapping `neo4rs`) will be added in Phase 1.x.

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::infrastructure::neo4j::schema;
use crate::infrastructure::neo4j::schema::{CleanReport, SchemaInitReport};

/// Initialize the V2 Neo4j schema — all uniqueness constraints + full-text indexes.
///
/// This is the primary entry point for schema setup, called by `dt schema init`.
/// All statements use `IF NOT EXISTS`, so repeated calls are safe.
pub async fn init_schema(graph: &dyn GraphRepository) -> Result<SchemaInitReport, DtError> {
    schema::initialize_schema(graph).await
}

/// Wipe all nodes and relationships from the graph.
///
/// Called by `dt clean --confirm`. Returns a report of what was deleted.
pub async fn clean_all(graph: &dyn GraphRepository) -> Result<CleanReport, DtError> {
    schema::clean_all_data(graph).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::domain::traits::GraphRepository;
    use crate::domain::types::HealthStatus;
    use std::collections::HashMap;

    /// Minimal mock that accepts everything and returns empty results.
    struct AcceptAllRepo;

    #[async_trait]
    impl GraphRepository for AcceptAllRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::json!([{"total": 0}]))
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn init_schema_returns_report() {
        let repo = AcceptAllRepo;
        let report = init_schema(&repo).await.expect("init should succeed");
        assert_eq!(report.constraints_created, 30);
        assert_eq!(report.indexes_created, 2);
        assert!(report.elapsed_ms < 5_000);
    }

    #[tokio::test]
    async fn clean_all_returns_report() {
        let repo = AcceptAllRepo;
        let report = clean_all(&repo).await.expect("clean should succeed");
        assert_eq!(report.nodes_deleted, 0);
        assert_eq!(report.relationships_deleted, 0);
    }
}
