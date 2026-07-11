//! [`ConversationHandler`] — creates `(:Conversation)` nodes in Neo4j.
//!
//! Conversation events are session-scoped and do not link to a specific
//! external entity. They record the session summary and key findings.
//!
//! Produces Cypher:
//! ```cypher
//! MERGE (e:Conversation {conv_id: $conv_id})
//! SET e.summary = $summary, e.session_id = $session_id, ...
//! MERGE (prj:Project {name: $project})
//! MERGE (e)-[:BELONGS_TO]->(prj)
//! ```

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::{make_event_id, parse_key_values};

/// Handler for conversation / session-end events.
///
/// Stores the session summary and attaches to the project.
/// Unlike entity-specific handlers, this does not link to a Method/Deployment/etc.
pub struct ConversationHandler;

#[async_trait]
impl EventHandler for ConversationHandler {
    fn event_type(&self) -> EventType {
        EventType::Conversation
    }

    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        let props = parse_key_values(&event.details);

        let summary = props
            .get("summary")
            .cloned()
            .unwrap_or_else(|| event.details.clone());

        let conv_id =
            make_event_id("conv", &event.entity_id, &event.details);

        let cypher = r#"
            MERGE (e:Conversation {conv_id: $conv_id})
            SET e.summary = $summary,
                e.session_id = $session_id,
                e.timestamp = $timestamp,
                e.entity_id = $entity_id,
                e.event_type = $event_type,
                e.details = $details
            WITH e
            MERGE (prj:Project {name: $project})
            MERGE (e)-[:BELONGS_TO]->(prj)
            "#.to_string();

        let mut params = std::collections::HashMap::new();
        params.insert(
            "conv_id".into(),
            serde_json::Value::String(conv_id),
        );
        params.insert(
            "summary".into(),
            serde_json::Value::String(summary),
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
            serde_json::Value::String(event.entity_id.clone()),
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
            assert!(query.contains("MERGE (e:Conversation"));
            assert!(query.contains("e)-[:BELONGS_TO]->(prj)"));
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
            event_type: EventType::Conversation,
            entity_id: "2026-07-10".into(),
            entity_type: "Session".into(),
            project: "test".into(),
            details: details.into(),
            session_id: "2026-07-10-cli".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn conversation_handler_writes_correct_query() {
        let handler = ConversationHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "summary: 测试会话摘要; 完成P1测试; 关键发现: 需要修复Conversation事件",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn conversation_handler_defaults_to_details() {
        let handler = ConversationHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        // No structured details — falls back to raw details string
        let evt = make_event("Just a plain text summary");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
