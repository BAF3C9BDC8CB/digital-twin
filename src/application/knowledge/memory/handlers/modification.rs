//! [`ModificationHandler`] — creates `(:Modification)` nodes in Neo4j.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `file`         — file path
//! - `entity_type`  — "Method" | "Class" | "NacosConfig"
//! - `entity_id`    — target entity id
//! - `change_type`  — "create" | "modify" | "delete"
//! - `diff_summary` — AI-generated change summary
//! - `reason`       — why this change was made

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::{make_event_id, parse_key_values};

/// Handler for code modification events.
///
/// Produces Cypher:
/// ```cypher
/// MERGE (e:Modification {mod_id: $mod_id})
/// SET e.file = $file, e.entity_type = $entity_type, ...
/// MERGE (target:Method {method_id: $entity_id})
/// MERGE (e)-[:AFFECTS]->(target)
/// ```
pub struct ModificationHandler;

#[async_trait]
impl EventHandler for ModificationHandler {
    fn event_type(&self) -> EventType {
        EventType::Modification
    }

    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        let props = parse_key_values(&event.details);

        let file = props
            .get("file")
            .cloned()
            .unwrap_or_default();
        let entity_type = props
            .get("entity_type")
            .cloned()
            .unwrap_or_else(|| event.entity_type.clone());
        let entity_id = props
            .get("entity_id")
            .cloned()
            .unwrap_or_else(|| event.entity_id.clone());
        let change_type = props
            .get("change_type")
            .cloned()
            .unwrap_or_else(|| "modify".to_string());
        let diff_summary = props
            .get("diff_summary")
            .cloned()
            .unwrap_or_else(|| event.details.clone());
        let reason = props
            .get("reason")
            .cloned()
            .unwrap_or_default();
        let mod_id =
            make_event_id("mod", &entity_id, &event.details);

        // Determine the target label from entity_type.
        // We support Method, Class, and NacosConfig.
        let target_label = match entity_type.to_lowercase().as_str() {
            "class" => "Class",
            "nacosconfig" => "NacosConfig",
            _ => "Method", // default to Method
        };
        let target_id_field = match entity_type.to_lowercase().as_str() {
            "class" => "class_id",
            "nacosconfig" => "config_id",
            _ => "method_id",
        };

        let cypher = format!(
            r#"
            MERGE (e:Modification {{mod_id: $mod_id}})
            SET e.file = $file,
                e.entity_type = $entity_type,
                e.entity_id = $entity_id,
                e.change_type = $change_type,
                e.diff_summary = $diff_summary,
                e.reason = $reason,
                e.session_id = $session_id,
                e.timestamp = $timestamp,
                e.event_type = $event_type,
                e.details = $details
            WITH e
            MERGE (target:{target_label} {{{target_id_field}: $entity_id}})
            MERGE (e)-[:AFFECTS]->(target)
            WITH e
            MERGE (prj:Project {{name: $project}})
            MERGE (e)-[:BELONGS_TO]->(prj)
            "#,
        );

        let mut params = std::collections::HashMap::new();
        params.insert("mod_id".into(), serde_json::Value::String(mod_id));
        params.insert("file".into(), serde_json::Value::String(file));
        params.insert(
            "entity_type".into(),
            serde_json::Value::String(entity_type),
        );
        params.insert(
            "entity_id".into(),
            serde_json::Value::String(entity_id),
        );
        params.insert(
            "change_type".into(),
            serde_json::Value::String(change_type),
        );
        params.insert(
            "diff_summary".into(),
            serde_json::Value::String(diff_summary),
        );
        params.insert("reason".into(), serde_json::Value::String(reason));
        params.insert(
            "session_id".into(),
            serde_json::Value::String(event.session_id.clone()),
        );
        params.insert(
            "timestamp".into(),
            serde_json::Value::String(event.timestamp.to_rfc3339()),
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

    /// A mock graph repository that counts write_query calls.
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
            // Assert key cypher fragments are present.
            assert!(query.contains("MERGE (e:Modification"));
            assert!(query.contains("e)-[:AFFECTS]->(target)"));
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
            event_type: EventType::Modification,
            entity_id: "dt://entity/test/class/Foo/method/bar@10".into(),
            entity_type: "Method".into(),
            project: "test".into(),
            details: details.into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn modification_handler_writes_correct_query() {
        let handler = ModificationHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "file: src/main.rs; entity_type: Method; change_type: modify; \
             diff_summary: added logging; reason: debugging",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn modification_handler_defaults() {
        let handler = ModificationHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        // Event with minimal details — should still produce valid Cypher.
        let evt = make_event("entity_id: dt://entity/test/class/Foo");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn class_target_label() {
        let handler = ModificationHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = MemoryEvent {
            event_type: EventType::Modification,
            entity_id: "dt://entity/test/class/Foo".into(),
            entity_type: "Class".into(),
            project: "test".into(),
            details: "file: src/main.rs; entity_type: Class".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        };

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
