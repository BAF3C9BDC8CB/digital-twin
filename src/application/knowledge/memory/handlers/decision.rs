//! [`DecisionHandler`] — creates `(:Decision)` nodes in Neo4j.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `title`        — decision title
//! - `context`      — background and problem
//! - `alternatives` — comma-separated candidate solutions
//! - `evidence`     — supporting evidence
//! - `choice`       — final choice
//! - `rationale`    — why this choice
//! - `consequences` — impact
//! - `confidence`   — confidence score (0.0–1.0)
//! - `verified`     — "true" | "false"

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::{make_event_id, parse_key_values};

/// Handler for architecture decision events.
///
/// Produces Cypher:
/// ```cypher
/// MERGE (e:Decision {decision_id: $decision_id})
/// SET e.title = $title, e.context = $context, ...
/// // Optional: link to Knowledge node
/// MERGE (k:Knowledge {knowledge_id: $knowledge_id})
/// MERGE (e)-[:BASED_ON]->(k)
/// ```
pub struct DecisionHandler;

#[async_trait]
impl EventHandler for DecisionHandler {
    fn event_type(&self) -> EventType {
        EventType::Decision
    }

    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        let props = parse_key_values(&event.details);

        let title = props
            .get("title")
            .cloned()
            .unwrap_or_else(|| event.entity_id.clone());
        let context = props
            .get("context")
            .cloned()
            .unwrap_or_else(|| event.details.clone());
        let alternatives = props
            .get("alternatives")
            .cloned()
            .unwrap_or_default();
        let evidence = props
            .get("evidence")
            .cloned()
            .unwrap_or_default();
        let choice = props
            .get("choice")
            .cloned()
            .unwrap_or_else(|| "no specific choice recorded".to_string());
        let rationale = props
            .get("rationale")
            .cloned()
            .unwrap_or_default();
        let consequences = props
            .get("consequences")
            .cloned()
            .unwrap_or_default();
        let confidence: f64 = props
            .get("confidence")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.5);
        let verified: bool = props
            .get("verified")
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false);
        let decision_id =
            make_event_id("decision", &title, &event.details);

        // Build knowledge_id from entity if present.
        let knowledge_id = props
            .get("knowledge_id")
            .cloned()
            .unwrap_or(format!("dt://knowledge/{}/decision_basis", event.project));

        // Build thread_id from session or entity.
        let thread_id = props
            .get("thread_id")
            .cloned()
            .unwrap_or_else(|| format!("dt://thread/{}/default", event.project));

        let cypher = r#"
            MERGE (e:Decision {decision_id: $decision_id})
            SET e.title = $title,
                e.context = $context,
                e.alternatives = $alternatives,
                e.evidence = $evidence,
                e.choice = $choice,
                e.rationale = $rationale,
                e.consequences = $consequences,
                e.confidence = $confidence,
                e.verified = $verified,
                e.session_id = $session_id,
                e.timestamp = $timestamp,
                e.entity_id = $entity_id,
                e.event_type = $event_type,
                e.details = $details
            WITH e
            MERGE (k:Knowledge {knowledge_id: $knowledge_id})
            MERGE (e)-[:BASED_ON]->(k)
            WITH e
            MERGE (t:Thread {thread_id: $thread_id})
            MERGE (e)-[:BELONGS_TO]->(t)
            WITH e
            MERGE (prj:Project {name: $project})
            MERGE (e)-[:BELONGS_TO]->(prj)
            "#.to_string();

        let mut params = std::collections::HashMap::new();
        params.insert(
            "decision_id".into(),
            serde_json::Value::String(decision_id),
        );
        params.insert(
            "title".into(),
            serde_json::Value::String(title),
        );
        params.insert(
            "context".into(),
            serde_json::Value::String(context),
        );
        params.insert(
            "alternatives".into(),
            serde_json::Value::String(alternatives),
        );
        params.insert(
            "evidence".into(),
            serde_json::Value::String(evidence),
        );
        params.insert(
            "choice".into(),
            serde_json::Value::String(choice),
        );
        params.insert(
            "rationale".into(),
            serde_json::Value::String(rationale),
        );
        params.insert(
            "consequences".into(),
            serde_json::Value::String(consequences),
        );
        let conf_num = serde_json::Number::from_f64(confidence)
            .unwrap_or_else(|| serde_json::Number::from(0u64));
        params.insert(
            "confidence".into(),
            serde_json::Value::Number(conf_num),
        );
        params.insert(
            "verified".into(),
            serde_json::Value::Bool(verified),
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
            "knowledge_id".into(),
            serde_json::Value::String(knowledge_id),
        );
        params.insert(
            "thread_id".into(),
            serde_json::Value::String(thread_id),
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
            assert!(query.contains("MERGE (e:Decision"));
            assert!(query.contains("e)-[:BASED_ON]->(k)"));
            assert!(query.contains("e)-[:BELONGS_TO]->(t)"));
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
            event_type: EventType::Decision,
            entity_id: "dt://decision/test/001".into(),
            entity_type: "ArchitectureDecision".into(),
            project: "test".into(),
            details: details.into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn decision_handler_writes_correct_query() {
        let handler = DecisionHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "title: Use Redis; context: caching needed; \
             alternatives: Redis,Memcached; evidence: team expertise; \
             choice: Redis; rationale: better persistence; \
             consequences: needs cluster; confidence: 0.9; verified: true",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn decision_handler_defaults() {
        let handler = DecisionHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("title: Simple choice");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn decision_handler_confidence_defaults() {
        let handler = DecisionHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("title: no-confidence; verified: false");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
