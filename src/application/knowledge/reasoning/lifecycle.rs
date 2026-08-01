// Lifecycle management for reasoning graph nodes.
//
// Two-level cleanup strategy:
//
// Phase 1 — Mark stale (deprecation):
// When a session ends, all reasoning nodes belonging to that session are
// marked with _stale_at = timestamp(). Stale nodes are excluded from
// Context Builder queries.
//
// Phase 2 — Delete (garbage collection):
// A nightly dt cleanup job deletes all reasoning nodes whose _stale_at
// is more than 30 days in the past.
//
// Affected labels: :Observation, :Analysis, :Decision.

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use async_trait::async_trait;
use std::sync::Arc;

/// Manages the lifecycle of reasoning graph nodes.
///
/// Provides methods for marking reasoning nodes as stale and
/// performing garbage-collection cleanups.
#[async_trait]
pub trait LifecycleManager: Send + Sync {
    /// Mark all reasoning nodes for a given session as stale.
    ///
    /// Sets _stale_at = timestamp() on every :Observation,
    /// :Analysis, and :Decision node whose session_id matches.
    ///
    /// Returns the number of nodes marked.
    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError>;

    /// Delete stale reasoning nodes older than retention_days.
    ///
    /// Removes every :Observation, :Analysis, and :Decision node
    /// whose _stale_at is more than retention_days in the past.
    ///
    /// Returns the number of nodes deleted.
    async fn cleanup_stale(&self, retention_days: u32) -> Result<usize, DtError>;

    /// Confirm a reasoning decision (set verified = true).
    ///
    /// This transitions a tentative :Decision into a confirmed
    /// decision suitable for archival / Memory World integration.
    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError>;
}

// ---------------------------------------------------------------------------
// DefaultLifecycleManager — canonical implementation
// ---------------------------------------------------------------------------

/// Canonical implementation of LifecycleManager backed by a GraphRepository.
pub struct DefaultLifecycleManager {
    graph: Arc<dyn GraphRepository>,
}

impl DefaultLifecycleManager {
    /// Create a new DefaultLifecycleManager backed by the given graph repository.
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl LifecycleManager for DefaultLifecycleManager {
    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError> {
        let now = chrono::Utc::now().to_rfc3339();

        let cypher = r#"
            MATCH (n)
            WHERE (n:Observation OR n:Analysis OR n:Decision)
              AND n.session_id = $session_id
              AND n._stale_at IS NULL
            SET n._stale_at = $now
            RETURN count(n) AS marked
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "session_id".into(),
            serde_json::Value::String(session_id.to_string()),
        );
        params.insert("now".into(), serde_json::Value::String(now));

        let result = self.graph.write_query(cypher, params).await?;
        let count = result
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("marked"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        Ok(count)
    }

    async fn cleanup_stale(&self, retention_days: u32) -> Result<usize, DtError> {
        let threshold = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
        let threshold_str = threshold.to_rfc3339();

        let cypher = r#"
            MATCH (n)
            WHERE (n:Observation OR n:Analysis OR n:Decision)
              AND n._stale_at IS NOT NULL
              AND n._stale_at < $threshold
            DETACH DELETE n
            RETURN count(n) AS deleted
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert("threshold".into(), serde_json::Value::String(threshold_str));

        let result = self.graph.write_query(cypher, params).await?;
        let count = result
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("deleted"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        Ok(count)
    }

    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError> {
        let cypher = r#"
            MATCH (d:Decision {decision_id: $decision_id})
            SET d.verified = true
            RETURN d.decision_id
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "decision_id".into(),
            serde_json::Value::String(decision_id.to_string()),
        );

        let result = self.graph.write_query(cypher, params).await?;

        let affected = result.as_array().map(|rows| rows.len()).unwrap_or(0);

        if affected == 0 {
            return Err(DtError::NotFound(format!(
                "Decision not found: {}",
                decision_id
            )));
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepo {
        fn new(write: Arc<AtomicUsize>, read: Arc<AtomicUsize>) -> Self {
            Self {
                write_count: write,
                read_count: read,
            }
        }
    }

    #[async_trait]
    impl GraphRepository for CountingRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!([{"marked": 3u64}]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    struct ConfirmRepo {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GraphRepository for ConfirmRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!([{"decision_id": "dec://test/001"}]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn LifecycleManager) {}
    }

    #[tokio::test]
    async fn mark_stale_writes_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let mgr = DefaultLifecycleManager::new(repo);

        let count = mgr.mark_stale("2026-07-09-001").await.expect("mark_stale");
        assert!(write.load(Ordering::SeqCst) >= 1);
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn cleanup_stale_writes_query() {
        // Use a repo that returns deleted count (different field name than mark_stale).
        struct CleanupRepo {
            counter: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl GraphRepository for CleanupRepo {
            async fn read_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Ok(serde_json::Value::Null)
            }
            async fn write_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                self.counter.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!([{"deleted": 5u64}]))
            }
            async fn health_check(&self) -> Result<HealthStatus, DtError> {
                Ok(HealthStatus::Healthy)
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CleanupRepo {
            counter: counter.clone(),
        });
        let mgr = DefaultLifecycleManager::new(repo);

        let count = mgr.cleanup_stale(30).await.expect("cleanup_stale");
        assert!(counter.load(Ordering::SeqCst) >= 1);
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn confirm_decision_sets_verified() {
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(ConfirmRepo {
            counter: counter.clone(),
        });
        let mgr = DefaultLifecycleManager::new(repo);

        mgr.confirm_decision("dec://test/001")
            .await
            .expect("confirm_decision");
        assert!(counter.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn mark_stale_returns_zero_for_no_matches() {
        // Use a different repo that returns 0 marked
        struct EmptyRepo;
        #[async_trait]
        impl GraphRepository for EmptyRepo {
            async fn read_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Ok(serde_json::Value::Null)
            }
            async fn write_query(
                &self,
                _query: &str,
                _params: std::collections::HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Ok(serde_json::json!([]))
            }
            async fn health_check(&self) -> Result<HealthStatus, DtError> {
                Ok(HealthStatus::Healthy)
            }
        }

        let mgr = DefaultLifecycleManager::new(Arc::new(EmptyRepo));
        let count = mgr.mark_stale("no-such-session").await.expect("mark_stale");
        assert_eq!(count, 0);
    }
}
