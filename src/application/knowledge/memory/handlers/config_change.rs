//! [`ConfigChangeHandler`] — creates `(:ConfigChange)` nodes in Neo4j.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `data_id`   — Nacos dataId
//! - `key`       — changed config key
//! - `old_value` — previous value (sanitised)
//! - `new_value` — new value (sanitised)

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::{make_event_id, parse_key_values};

/// Handler for configuration change events.
///
/// Produces Cypher:
/// ```cypher
/// MERGE (e:ConfigChange {change_id: $change_id})
/// SET e.data_id = $data_id, e.key = $key, ...
/// MERGE (cfg:NacosConfig {config_id: $config_id})
/// MERGE (e)-[:AFFECTS]->(cfg)
/// ```
pub struct ConfigChangeHandler;

#[async_trait]
impl EventHandler for ConfigChangeHandler {
    fn event_type(&self) -> EventType {
        EventType::ConfigChange
    }

    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        let props = parse_key_values(&event.details);

        let data_id = props
            .get("data_id")
            .cloned()
            .unwrap_or_else(|| event.entity_id.clone());
        let key = props
            .get("key")
            .cloned()
            .unwrap_or_default();
        let old_value = props
            .get("old_value")
            .cloned()
            .unwrap_or_default();
        let new_value = props
            .get("new_value")
            .cloned()
            .unwrap_or_else(|| event.details.clone());
        let change_id =
            make_event_id("cfg", &data_id, &event.details);

        // Build config_id to match NacosConfig entity.
        // Format: dt://config/{data_id}
        let config_id = format!("dt://config/{}", data_id);

        let cypher = r#"
            MERGE (e:ConfigChange {change_id: $change_id})
            SET e.data_id = $data_id,
                e.key = $key,
                e.old_value = $old_value,
                e.new_value = $new_value,
                e.session_id = $session_id,
                e.timestamp = $timestamp,
                e.entity_id = $entity_id,
                e.event_type = $event_type,
                e.details = $details
            WITH e
            MERGE (cfg:NacosConfig {config_id: $config_id})
            MERGE (e)-[:AFFECTS]->(cfg)
            WITH e
            MERGE (prj:Project {name: $project})
            MERGE (e)-[:BELONGS_TO]->(prj)
            "#.to_string();

        let mut params = std::collections::HashMap::new();
        params.insert(
            "change_id".into(),
            serde_json::Value::String(change_id),
        );
        params.insert(
            "data_id".into(),
            serde_json::Value::String(data_id),
        );
        params.insert("key".into(), serde_json::Value::String(key));
        params.insert(
            "old_value".into(),
            serde_json::Value::String(old_value),
        );
        params.insert(
            "new_value".into(),
            serde_json::Value::String(new_value),
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
            "config_id".into(),
            serde_json::Value::String(config_id),
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
            assert!(query.contains("MERGE (e:ConfigChange"));
            assert!(query.contains("e)-[:AFFECTS]->(cfg)"));
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
            event_type: EventType::ConfigChange,
            entity_id: "application.yml".into(),
            entity_type: "NacosConfig".into(),
            project: "test".into(),
            details: details.into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn config_change_handler_writes_correct_query() {
        let handler = ConfigChangeHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "data_id: application.yml; key: db.url; \
             old_value: old-db; new_value: new-db",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn config_change_handler_defaults() {
        let handler = ConfigChangeHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("new_value: changed-to-prod-db");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
