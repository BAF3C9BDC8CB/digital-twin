//! Memory (RecordEvent) gRPC handler for the DtCore service.
//!
//! Delegates to [`DefaultMemoryService`] to create Day → Session → Event
//! chains in the knowledge graph.

use crate::application::hooks::HookEngine;
use crate::domain::traits::GraphRepository;
use crate::proto::dt::core::*;
use crate::proto::dt::common;
use crate::application::knowledge::memory::service::{
    DefaultMemoryService, MemoryService,
};
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use std::sync::Arc;
use tonic::Status;

/// Handler for `RecordEvent` RPC — persist an event into the time dimension.
pub async fn handle_record_event(
    req: EventRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    hook_engine: Option<Arc<HookEngine>>,
) -> Result<common::Empty, Status> {
    let graph = graph.ok_or_else(|| {
        Status::unavailable("Graph backend not available")
    })?;

    let project = if req.project.is_empty() {
        "unknown"
    } else {
        &req.project
    };

    let parsed_type = parse_event_type(&req.r#type).ok_or_else(|| {
        Status::invalid_argument(format!(
            "unknown event type: {}. \
             Supported: Modification, Deployment, ConfigChange, BugFix, Decision, Conversation",
            req.r#type
        ))
    })?;

    let session_id = format!("{}-grpc", chrono::Utc::now().format("%Y-%m-%d"));

    let event = MemoryEvent {
        event_type: parsed_type,
        entity_id: req.entity_id.clone(),
        entity_type: if req.entity_type.is_empty() {
            "Unknown".to_string()
        } else {
            req.entity_type.clone()
        },
        project: project.to_string(),
        details: req.details.clone(),
        session_id,
        timestamp: chrono::Utc::now(),
    };

    let memory_svc = DefaultMemoryService::new(graph, hook_engine);
    memory_svc
        .record_event(&event)
        .await
        .map_err(|e| Status::internal(format!("record_event failed: {e}")))?;

    Ok(common::Empty {})
}

/// Parse an EventType from a string (case-insensitive).
///
/// Matches the logic in `main.rs::parse_event_type`.
fn parse_event_type(s: &str) -> Option<EventType> {
    match s.to_lowercase().as_str() {
        "modification" => Some(EventType::Modification),
        "deployment" => Some(EventType::Deployment),
        "configchange" => Some(EventType::ConfigChange),
        "bugfix" => Some(EventType::BugFix),
        "decision" => Some(EventType::Decision),
        "conversation" => Some(EventType::Conversation),
        _ => None,
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

    struct MinimalGraphRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl GraphRepository for MinimalGraphRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn record_event_requires_graph() {
        let req = EventRequest {
            r#type: "Deployment".into(),
            entity_id: "test-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "test".into(),
            details: "job: test; branch: main".into(),
        };
        let result = handle_record_event(req, None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn record_event_invalid_type_errors() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let graph: Arc<dyn GraphRepository> = Arc::new(MinimalGraphRepo {
            write_count: write.clone(),
            read_count: read.clone(),
        });

        let req = EventRequest {
            r#type: "bogus".into(),
            entity_id: "test-1".into(),
            entity_type: "Bogus".into(),
            project: "test".into(),
            details: "".into(),
        };
        let result = handle_record_event(req, Some(graph), None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn record_event_deployment_succeeds() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let graph: Arc<dyn GraphRepository> = Arc::new(MinimalGraphRepo {
            write_count: write.clone(),
            read_count: read.clone(),
        });

        let req = EventRequest {
            r#type: "Deployment".into(),
            entity_id: "my-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "test".into(),
            details: "job: my-job; env: test; branch: main".into(),
        };
        let result = handle_record_event(req, Some(graph), None).await;
        assert!(result.is_ok());
        // Without hook_engine, the event is silently dropped (logged as warning).
        // The hook system handles all event types now; old handlers are removed.
        assert_eq!(write.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn parse_event_type_mapping() {
        assert_eq!(parse_event_type("Deployment"), Some(EventType::Deployment));
        assert_eq!(parse_event_type("DEPLOYMENT"), Some(EventType::Deployment));
        assert_eq!(parse_event_type("deployment"), Some(EventType::Deployment));
        assert_eq!(
            parse_event_type("Conversation"),
            Some(EventType::Conversation)
        );
        assert_eq!(parse_event_type("unknown"), None);
    }
}
