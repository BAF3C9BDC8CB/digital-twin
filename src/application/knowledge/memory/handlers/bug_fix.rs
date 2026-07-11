//! [`BugFixHandler`] — creates `(:BugFix)` nodes in Neo4j.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `issue`        — issue number / description
//! - `root_cause`   — root cause analysis
//! - `solution`     — fix description
//! - `files_changed`— comma-separated file list

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::{make_event_id, parse_key_values};

/// Handler for bug fix events.
///
/// Produces Cypher:
/// ```cypher
/// MERGE (e:BugFix {fix_id: $fix_id})
/// SET e.issue = $issue, e.root_cause = $root_cause, ...
/// MERGE (m:Method {method_id: $method_id})
/// MERGE (e)-[:FIXES]->(m)
/// ```
pub struct BugFixHandler;

#[async_trait]
impl EventHandler for BugFixHandler {
    fn event_type(&self) -> EventType {
        EventType::BugFix
    }

    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        let props = parse_key_values(&event.details);

        let issue = props
            .get("issue")
            .cloned()
            .unwrap_or_else(|| event.entity_id.clone());
        let root_cause = props
            .get("root_cause")
            .cloned()
            .unwrap_or_default();
        let solution = props
            .get("solution")
            .cloned()
            .unwrap_or_else(|| event.details.clone());
        let files_changed_raw = props
            .get("files_changed")
            .cloned()
            .unwrap_or_default();
        // Store as comma-separated string; Neo4j does not have list
        // params via our simple param interface, so we embed as JSON array.
        let fix_id =
            make_event_id("fix", &issue, &event.details);

        let target_entity_id = props
            .get("entity_id")
            .cloned()
            .unwrap_or_else(|| event.entity_id.clone());

        let cypher = r#"
            MERGE (e:BugFix {fix_id: $fix_id})
            SET e.issue = $issue,
                e.root_cause = $root_cause,
                e.solution = $solution,
                e.files_changed = $files_changed,
                e.session_id = $session_id,
                e.timestamp = $timestamp,
                e.entity_id = $entity_id,
                e.event_type = $event_type,
                e.details = $details
            WITH e
            MERGE (m:Method {method_id: $entity_id})
            MERGE (e)-[:FIXES]->(m)
            WITH e
            MERGE (prj:Project {name: $project})
            MERGE (e)-[:BELONGS_TO]->(prj)
            "#.to_string();

        let mut params = std::collections::HashMap::new();
        params.insert(
            "fix_id".into(),
            serde_json::Value::String(fix_id),
        );
        params.insert(
            "issue".into(),
            serde_json::Value::String(issue),
        );
        params.insert(
            "root_cause".into(),
            serde_json::Value::String(root_cause),
        );
        params.insert(
            "solution".into(),
            serde_json::Value::String(solution),
        );
        params.insert(
            "files_changed".into(),
            serde_json::Value::String(files_changed_raw),
        );
        params.insert(
            "session_id".into(),
            serde_json::Value::String(event.session_id.clone()),
        );
        params.insert(
            "timestamp".into(),
            serde_json::Value::String(event.timestamp.to_rfc3339()),
        );
        params.insert(
            "entity_id".into(),
            serde_json::Value::String(target_entity_id),
        );
        params.insert(
            "event_type".into(),
            serde_json::Value::String(event.event_type.as_str().into()),
        );
        params.insert(
            "details".into(),
            serde_json::Value::String(event.details.clone()),
        );
        params.insert(
            "project".into(),
            serde_json::Value::String(event.project.clone()),
        );

        graph.write_query(&cypher, params).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingRepo {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GraphRepository for CountingRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            assert!(query.contains("MERGE (e:BugFix"));
            assert!(query.contains("e)-[:FIXES]->(m)"));
            Ok(serde_json::Value::Null)
        }

        async fn health_check(
            &self,
        ) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    fn make_event(details: &str) -> MemoryEvent {
        MemoryEvent {
            event_type: EventType::BugFix,
            entity_id: "dt://entity/test/class/Foo/method/fixMe@42".into(),
            entity_type: "Method".into(),
            project: "test".into(),
            details: details.into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn bug_fix_handler_writes_correct_query() {
        let handler = BugFixHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "issue: #42 NullPointer; root_cause: missing null check; \
             solution: add guard; files_changed: src/main.rs,lib/util.rs; \
             entity_id: dt://entity/test/class/Foo/method/fixMe@42",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn bug_fix_handler_defaults() {
        let handler = BugFixHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("issue: bug-1");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
