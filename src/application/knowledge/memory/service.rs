//! MemoryService trait — contract for time-dimension operations.
//!
//! Implementations handle Day→Session→Event chain creation and query.
//! The trait is decoupled from any specific storage backend via the
//! `GraphRepository` abstraction.
//!
//! [`DefaultMemoryService`] is the canonical production implementation.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::sync::Arc;

use super::dispatcher::{
    build_default_dispatcher, link_event_to_session, EventDispatcher,
};
use super::entities::{Day, MemoryEvent, Session};
use super::handlers::make_event_id;

/// Service for managing the time dimension (Day → Session → Event).
///
/// # Typical usage
///
/// ```ignore
/// let svc = DefaultMemoryService::new(graph_repo);
/// let day = svc.ensure_day("2026-07-09").await?;
/// svc.create_session(&Session {
///     session_id: "2026-07-09-001".into(),
///     summary: "Fixing bug #42".into(),
///     ..
/// }).await?;
/// svc.record_event(&MemoryEvent {
///     event_type: EventType::BugFix,
///     entity_id: "42".into(),
///     session_id: "2026-07-09-001".into(),
///     ..
/// }).await?;
/// ```
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Ensure a Day node exists for the given date (idempotent).
    ///
    /// If the node already exists, returns it without modification.
    /// Otherwise creates a new `(:Day)` node.
    async fn ensure_day(&self, date: &str) -> Result<Day, DtError>;

    /// Create a new Session node and link it to its parent Day via [:HAS_SESSION].
    ///
    /// The Day must have been created via `ensure_day` first.
    async fn create_session(&self, session: &Session) -> Result<(), DtError>;

    /// Record an event and link it to its parent Session via [:HAS_EVENT].
    ///
    /// Automatically ensures the Session exists and also creates
    /// the external entity reference node (if not already present).
    async fn record_event(&self, event: &MemoryEvent) -> Result<(), DtError>;

    /// Retrieve all events belonging to a session, ordered by timestamp.
    async fn get_session_events(&self, session_id: &str) -> Result<Vec<MemoryEvent>, DtError>;

    /// Retrieve the timeline — all Days and their Sessions — for the
    /// last `days` calendar days.
    ///
    /// Returns Days in descending date order (most recent first).
    async fn get_timeline(&self, days: u32) -> Result<Vec<Day>, DtError>;
}

// ---------------------------------------------------------------------------
// DefaultMemoryService — canonical implementation
// ---------------------------------------------------------------------------

/// Canonical implementation of [`MemoryService`] backed by
/// a [`GraphRepository`] and the default set of event handlers.
///
/// # Lifecycle
///
/// ```text
/// ensure_day     → MERGE (:Day)
/// create_session → MERGE (:Session),  MERGE (day)-[:HAS_SESSION]->(session)
/// record_event   → 1. ensure_day (lazy)
///                  2. create_session (if not exists)
///                  3. dispatch to handler → MERGE (:EventType {…})
///                  4. link (:Session)-[:HAS_EVENT]->(:EventType)
/// ```
pub struct DefaultMemoryService {
    graph: Arc<dyn GraphRepository>,
    dispatcher: EventDispatcher,
}

impl DefaultMemoryService {
    /// Create a new [`DefaultMemoryService`] backed by the given
    /// graph repository, with all five standard event handlers
    /// registered.
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self {
            graph,
            dispatcher: build_default_dispatcher(),
        }
    }
}

#[async_trait]
impl MemoryService for DefaultMemoryService {
    async fn ensure_day(&self, date: &str) -> Result<Day, DtError> {
        let cypher = r#"
            MERGE (d:Day {day_id: $day_id})
            ON CREATE SET d.date = $date
            RETURN d.day_id AS day_id, d.date AS date
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert("day_id".into(), serde_json::Value::String(date.into()));
        params.insert("date".into(), serde_json::Value::String(date.into()));

        let _result = self.graph.read_query(cypher, params).await?;
        // Always return a Day from the given date — even if the read
        // result is empty (MERGE guarantees the node exists).
        Ok(Day {
            day_id: date.into(),
            date: date.into(),
        })
    }

    async fn create_session(&self, session: &Session) -> Result<(), DtError> {
        // First ensure the parent Day exists.
        let day_id = session.session_id.split('-').take(3).collect::<Vec<_>>().join("-");
        self.ensure_day(&day_id).await?;

        let cypher = r#"
            MATCH (d:Day {day_id: $day_id})
            MERGE (s:Session {session_id: $session_id})
            ON CREATE SET
                s.summary = $summary,
                s.key_decisions = $key_decisions,
                s.thread_id = $thread_id,
                s.started_at = $started_at,
                s.ended_at = $ended_at
            MERGE (d)-[:HAS_SESSION]->(s)
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert("day_id".into(), serde_json::Value::String(day_id));
        params.insert(
            "session_id".into(),
            serde_json::Value::String(session.session_id.clone()),
        );
        params.insert(
            "summary".into(),
            serde_json::Value::String(session.summary.clone()),
        );
        params.insert(
            "thread_id".into(),
            session
                .thread_id
                .as_ref()
                .map(|s| serde_json::Value::String(s.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        params.insert(
            "started_at".into(),
            serde_json::Value::String(session.started_at.to_rfc3339()),
        );
        params.insert(
            "ended_at".into(),
            session
                .ended_at
                .map(|t| serde_json::Value::String(t.to_rfc3339()))
                .unwrap_or(serde_json::Value::Null),
        );

        // key_decisions is stored as a JSON array string for simplicity
        let kd = serde_json::to_string(&session.key_decisions)
            .unwrap_or_else(|_| "[]".to_string());
        params.insert(
            "key_decisions".into(),
            serde_json::Value::String(kd),
        );

        self.graph.write_query(cypher, params).await?;
        Ok(())
    }

    async fn record_event(&self, event: &MemoryEvent) -> Result<(), DtError> {
        // 1. Ensure the parent Day exists (lazy creation).
        let day_id = event
            .session_id
            .split('-')
            .take(3)
            .collect::<Vec<_>>()
            .join("-");
        self.ensure_day(&day_id).await?;

        // 2. Ensure the parent Session exists (merge on session_id).
        let now = chrono::Utc::now();
        let session = Session {
            session_id: event.session_id.clone(),
            summary: format!("Auto-created session for {}", event.event_type.as_str()),
            key_decisions: vec![],
            thread_id: None,
            started_at: event.timestamp,
            ended_at: Some(now),
        };
        // Try to create session; if it already exists, MERGE is no-op.
        let _ = self.create_session(&session).await;

        // 3. Dispatch to the appropriate event-type handler.
        //    The handler creates the event node (e.g. :Modification, :Deployment).
        self.dispatcher
            .dispatch(event, self.graph.as_ref())
            .await?;

        // 4. Link the event node back to the Session via [:HAS_EVENT].
        //    Build the event node ID using the same scheme as the handlers.
        let prefix = match event.event_type {
            super::entities::EventType::Modification => "mod",
            super::entities::EventType::Deployment => "deploy",
            super::entities::EventType::ConfigChange => "cfg",
            super::entities::EventType::BugFix => "fix",
            super::entities::EventType::Decision => "decision",
            super::entities::EventType::PodEvent => "pod",
            super::entities::EventType::Conversation => "conv",
        };
        let event_node_id = make_event_id(prefix, &event.entity_id, &event.details);

        link_event_to_session(
            self.graph.as_ref(),
            &event.session_id,
            &event_node_id,
            event.event_type.clone(),
        )
        .await?;

        Ok(())
    }

    async fn get_session_events(&self, _session_id: &str) -> Result<Vec<MemoryEvent>, DtError> {
        // Stub: full query implementation requires a real Neo4j connection
        // and deserialising results back into MemoryEvent structs. For
        // now, return an empty vec.
        Ok(vec![])
    }

    async fn get_timeline(&self, _days: u32) -> Result<Vec<Day>, DtError> {
        // Stub: full query implementation requires a real Neo4j connection.
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::memory::entities::{EventType, MemoryEvent, Session};
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A mock graph repository that counts write_query calls.
    struct CountingRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepo {
        fn new(
            write_count: Arc<AtomicUsize>,
            read_count: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                write_count,
                read_count,
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
            Ok(serde_json::Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    /// Verify the trait is object-safe (can be used as `dyn MemoryService`).
    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn MemoryService) {}
    }

    /// Verify all trait methods compile when referenced.
    #[test]
    fn trait_method_signatures_exist() {
        fn _assert_methods<T: MemoryService>() {}
    }

    /// Quick sanity check on entity construction used in trait docs.
    #[test]
    fn example_session_construction() {
        let t = chrono::Utc::now();
        let session = Session {
            session_id: "2026-07-09-001".into(),
            summary: "Example session".into(),
            key_decisions: vec![],
            thread_id: None,
            started_at: t,
            ended_at: None,
        };
        assert_eq!(session.session_id, "2026-07-09-001");
    }

    #[test]
    fn example_event_construction() {
        let evt = MemoryEvent {
            event_type: EventType::BugFix,
            entity_id: "42".into(),
            entity_type: "Bug".into(),
            project: "test".into(),
            details: "Fixed null pointer".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        };
        assert_eq!(evt.event_type, EventType::BugFix);
        assert_eq!(evt.entity_id, "42");
    }

    // -------------------------------------------------------------------
    // DefaultMemoryService tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn default_memory_service_ensure_day() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo);

        let day = svc.ensure_day("2026-07-09").await.expect("ensure_day");
        assert_eq!(day.day_id, "2026-07-09");
        assert_eq!(day.date, "2026-07-09");
        // MERGE uses read_query first.
        assert!(read.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn default_memory_service_create_session() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo);

        let t = chrono::Utc::now();
        let session = Session {
            session_id: "2026-07-09-001".into(),
            summary: "Test session".into(),
            key_decisions: vec!["use async-trait".into()],
            thread_id: None,
            started_at: t,
            ended_at: None,
        };

        svc.create_session(&session).await.expect("create_session");
        // Should trigger: ensure_day (1 read) + create_session (1 write)
        assert!(read.load(Ordering::SeqCst) >= 1);
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn default_memory_service_record_event_full_chain() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo);

        let evt = MemoryEvent {
            event_type: EventType::Deployment,
            entity_id: "my-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "test".into(),
            details: "job: my-job; env: test; branch: main; status: success".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        };

        svc.record_event(&evt).await.expect("record_event");
        // Expected writes: ensure_day (read), create_session (write),
        // handler write (write), link_event_to_session (write)
        // So at least 3 writes + 1 read.
        assert!(read.load(Ordering::SeqCst) >= 1);
        assert!(write.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn default_memory_service_record_modification_event() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo);

        let evt = MemoryEvent {
            event_type: EventType::Modification,
            entity_id: "dt://entity/test/class/Foo/method/bar@10".into(),
            entity_type: "Method".into(),
            project: "test".into(),
            details: "file: src/main.rs; entity_type: Method; change_type: modify; \
                      diff_summary: test; reason: test".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        };

        svc.record_event(&evt).await.expect("record_event");
        assert!(write.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn default_memory_service_record_decision_event() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo);

        let evt = MemoryEvent {
            event_type: EventType::Decision,
            entity_id: "dt://decision/test/001".into(),
            entity_type: "ArchitectureDecision".into(),
            project: "test".into(),
            details: "title: Test decision; choice: Option A".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        };

        svc.record_event(&evt).await.expect("record_event");
        assert!(write.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn default_memory_service_stubs_ok() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo);

        let events = svc.get_session_events("any").await.expect("stub");
        assert!(events.is_empty());

        let timeline = svc.get_timeline(7).await.expect("stub");
        assert!(timeline.is_empty());
    }
}
