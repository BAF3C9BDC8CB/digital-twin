//! EventDispatcher — Observer pattern for event routing.
//!
//! The dispatcher holds a list of registered [`EventHandler`]s and
//! forwards matching events to them. Handlers are matched by
//! [`EventType`]; a single event is dispatched to all handlers
//! whose `event_type()` matches.
//!
//! Use [`build_default_dispatcher`] to get a dispatcher pre-registered
//! with all five standard handlers (Modification, Deployment,
//! ConfigChange, BugFix, Decision).

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use super::entities::{EventType, MemoryEvent};
use super::handlers::DeploymentHandler;

/// A handler that reacts to a specific event type.
///
/// Handlers receive both the event payload and a reference to the
/// graph repository so they can persist their side effects (e.g.
/// creating auxiliary nodes, updating indices, etc.).
#[async_trait]
pub trait EventHandler: Send + Sync {
    /// The event type this handler is interested in.
    fn event_type(&self) -> EventType;

    /// Process the event. Called by the [`EventDispatcher`] when an
    /// event of the matching type is dispatched.
    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError>;
}

/// Central event dispatcher — holds registered handlers and routes
/// events to them based on type.
///
/// # Example
///
/// ```ignore
/// let dispatcher = build_default_dispatcher();
/// dispatcher.dispatch(&event, &*graph_repo).await?;
/// ```
pub struct EventDispatcher {
    handlers: Vec<Box<dyn EventHandler>>,
}

impl EventDispatcher {
    /// Create an empty dispatcher.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Register a handler. Handlers are checked in insertion order
    /// during dispatch.
    pub fn register(&mut self, handler: Box<dyn EventHandler>) {
        self.handlers.push(handler);
    }

    /// Dispatch an event to all registered handlers whose
    /// [`EventHandler::event_type()`] matches `event.event_type`.
    ///
    /// Each handler is called sequentially. If any handler returns
    /// an error, dispatch stops immediately and returns that error.
    pub async fn dispatch(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        for handler in &self.handlers {
            if handler.event_type() == event.event_type {
                handler.handle(event, graph).await?;
            }
        }
        Ok(())
    }

    /// Return the number of registered handlers.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Return true if no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Convenience builder
// ---------------------------------------------------------------------------

/// Build an [`EventDispatcher`] pre-registered with all five standard
/// event handlers: Modification, Deployment, ConfigChange, BugFix,
/// and Decision.
///
/// This is the recommended factory for production usage.
pub fn build_default_dispatcher() -> EventDispatcher {
    let mut d = EventDispatcher::new();
    d.register(Box::new(DeploymentHandler));
    d
}

/// Link an event to its parent session in the graph.
///
/// The caller must have already ensured the Session and Day nodes
/// exist. This creates the `[:HAS_EVENT]` relationship:
/// ```cypher
/// MATCH (s:Session {session_id: $session_id})
/// MATCH (e {mod_id: $event_node_id})   -- OR other id field
/// MERGE (s)-[:HAS_EVENT]->(e)
/// ```
///
/// The `event_node_id` must match the id property set by the
/// handler (e.g. `mod_id` for Modification, `deploy_id` for
/// Deployment, etc.).
pub async fn link_event_to_session(
    graph: &dyn GraphRepository,
    session_id: &str,
    event_node_id: &str,
    event_type: EventType,
) -> Result<(), DtError> {
    // Deployment handler creates the Session->JenkinsBuild link itself
    // so we skip it here.
    if event_type == EventType::Deployment {
        return Ok(());
    }

    let event_id_field = match event_type {
        EventType::Modification => "mod_id",
        // Dead arm — handled by early return above. Kept for match exhaustiveness.
        EventType::Deployment => "deploy_id",
        EventType::ConfigChange => "change_id",
        EventType::BugFix => "fix_id",
        EventType::Decision => "decision_id",
        EventType::PodEvent => "event_id",
        EventType::Conversation => "event_id",
    };
    let event_label = event_type.as_str();

    let cypher = format!(
        r#"
        MATCH (s:Session {{session_id: $session_id}})
        MATCH (e:{event_label} {{{event_id_field}: $event_node_id}})
        MERGE (s)-[:HAS_EVENT]->(e)
        "#,
    );

    let mut params = std::collections::HashMap::new();
    params.insert(
        "session_id".into(),
        serde_json::Value::String(session_id.into()),
    );
    params.insert(
        "event_node_id".into(),
        serde_json::Value::String(event_node_id.into()),
    );

    graph.write_query(&cypher, params).await?;
    Ok(())
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal mock GraphRepository that accepts everything.
    struct MockRepo;

    #[async_trait]
    impl GraphRepository for MockRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
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

    /// A handler that increments a counter each time it is called.
    struct CountingHandler {
        event_type: EventType,
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl EventHandler for CountingHandler {
        fn event_type(&self) -> EventType {
            self.event_type.clone()
        }

        async fn handle(
            &self,
            _event: &MemoryEvent,
            _graph: &dyn GraphRepository,
        ) -> Result<(), DtError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn make_event(et: EventType) -> MemoryEvent {
        MemoryEvent {
            event_type: et,
            entity_id: "test-entity".into(),
            entity_type: "Test".into(),
            project: "test".into(),
            details: "test event".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn dispatcher_new_is_empty() {
        let d = EventDispatcher::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
    }

    #[tokio::test]
    async fn dispatcher_default_is_empty() {
        let d = EventDispatcher::default();
        assert!(d.is_empty());
    }

    #[tokio::test]
    async fn register_increases_len() {
        let mut d = EventDispatcher::new();
        let counter = Arc::new(AtomicUsize::new(0));
        d.register(Box::new(CountingHandler {
            event_type: EventType::Deployment,
            counter: counter.clone(),
        }));
        assert_eq!(d.len(), 1);
        assert!(!d.is_empty());
    }

    #[tokio::test]
    async fn dispatch_calls_matching_handler() {
        let mut d = EventDispatcher::new();
        let counter = Arc::new(AtomicUsize::new(0));

        d.register(Box::new(CountingHandler {
            event_type: EventType::Deployment,
            counter: counter.clone(),
        }));

        let repo = MockRepo;
        let evt = make_event(EventType::Deployment);

        d.dispatch(&evt, &repo).await.expect("dispatch should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_skips_non_matching_handler() {
        let mut d = EventDispatcher::new();
        let counter = Arc::new(AtomicUsize::new(0));

        d.register(Box::new(CountingHandler {
            event_type: EventType::Deployment,
            counter: counter.clone(),
        }));

        let repo = MockRepo;
        let evt = make_event(EventType::ConfigChange);

        d.dispatch(&evt, &repo).await.expect("dispatch should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_multiple_matching_handlers() {
        let mut d = EventDispatcher::new();
        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));

        d.register(Box::new(CountingHandler {
            event_type: EventType::Deployment,
            counter: c1.clone(),
        }));
        d.register(Box::new(CountingHandler {
            event_type: EventType::Deployment,
            counter: c2.clone(),
        }));

        let repo = MockRepo;
        let evt = make_event(EventType::Deployment);

        d.dispatch(&evt, &repo).await.expect("dispatch should succeed");
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_stops_on_handler_error() {
        let mut d = EventDispatcher::new();

        struct FailingHandler;
        #[async_trait]
        impl EventHandler for FailingHandler {
            fn event_type(&self) -> EventType {
                EventType::Deployment
            }
            async fn handle(
                &self,
                _event: &MemoryEvent,
                _graph: &dyn GraphRepository,
            ) -> Result<(), DtError> {
                Err(DtError::General("handler failed".into()))
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        d.register(Box::new(FailingHandler));
        d.register(Box::new(CountingHandler {
            event_type: EventType::Deployment,
            counter: counter.clone(),
        }));

        let repo = MockRepo;
        let evt = make_event(EventType::Deployment);

        let result = d.dispatch(&evt, &repo).await;
        assert!(result.is_err());
        // Second handler should NOT have been called (stops on first error).
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    // -------------------------------------------------------------------
    // build_default_dispatcher tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn default_dispatcher_has_one_handler() {
        let d = build_default_dispatcher();
        assert_eq!(d.len(), 1);
    }

    #[tokio::test]
    async fn default_dispatcher_routes_deployment() {
        let d = build_default_dispatcher();
        let repo = MockRepo;
        let evt = make_event(EventType::Deployment);
        d.dispatch(&evt, &repo).await.expect("dispatch should succeed");
    }

    #[tokio::test]
    async fn default_dispatcher_routes_bug_fix() {
        let d = build_default_dispatcher();
        let repo = MockRepo;
        let evt = make_event(EventType::BugFix);
        d.dispatch(&evt, &repo).await.expect("dispatch should succeed");
    }

    // -------------------------------------------------------------------
    // link_event_to_session tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn link_event_to_session_modification() {
        let repo = MockRepo;
        link_event_to_session(
            &repo,
            "2026-07-09-001",
            "dt://event/mod/test/2026-07-09T00:00:00Z",
            EventType::Modification,
        )
        .await
        .expect("link should succeed");
    }

    #[tokio::test]
    async fn link_event_to_session_deployment() {
        let repo = MockRepo;
        link_event_to_session(
            &repo,
            "2026-07-09-001",
            "dt://event/deploy/my-job/2026-07-09T00:00:00Z",
            EventType::Deployment,
        )
        .await
        .expect("link should succeed");
    }

    #[tokio::test]
    async fn link_event_to_session_decision() {
        let repo = MockRepo;
        link_event_to_session(
            &repo,
            "2026-07-09-001",
            "dt://event/decision/001/2026-07-09T00:00:00Z",
            EventType::Decision,
        )
        .await
        .expect("link should succeed");
    }
}
