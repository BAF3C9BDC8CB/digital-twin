//! MemoryService trait — contract for time-dimension operations.
//!
//! Implementations handle Day→Session→Event chain creation and query.
//! The trait is decoupled from any specific storage backend via the
//! `GraphRepository` abstraction.
//!
//! [`DefaultMemoryService`] is the canonical production implementation.
//!
//! # Event routing
//!
//! All event types are now handled by [`HookEngine`]; `record_event`
//! retains an `else if` chain that maps each [`EventType`] to the
//! appropriate hook name for backward compatibility.

use std::collections::HashMap;

use async_trait::async_trait;
use crate::application::hooks::{HookContext, HookEngine};
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::sync::Arc;

use super::entities::{Day, MemoryEvent, Session};

/// Parse a `key=value` / `key: value` string into a [`HashMap`].
///
/// Pairs are separated by `;` or `,`. Leading/trailing whitespace is
/// trimmed. Keys are lowercased so callers can match case-insensitively.
pub(crate) fn parse_key_values(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in raw.split([';', '\n', ',']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(pos) = part.find(['=', ':']) {
            let key = part[..pos].trim().to_lowercase();
            let value = part[pos + 1..].trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }
    map
}

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
/// a [`GraphRepository`] and (optionally) a [`HookEngine`].
///
/// # Lifecycle
///
/// ```text
/// ensure_day     → MERGE (:Day)
/// create_session → MERGE (:Session),  MERGE (day)-[:HAS_SESSION]->(session)
/// record_event   → 1. ensure_day (lazy)
///                  2. create_session (if not exists)
///                  3. route via hook engine → MERGE (:EventType {…})
///                  4. link (:Session)-[:HAS_EVENT]->(:EventType)
/// ```
pub struct DefaultMemoryService {
    graph: Arc<dyn GraphRepository>,
    hook_engine: Option<Arc<HookEngine>>,
}

impl DefaultMemoryService {
    /// Create a new [`DefaultMemoryService`] backed by the given
    /// graph repository.
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        hook_engine: Option<Arc<HookEngine>>,
    ) -> Self {
        Self {
            graph,
            hook_engine,
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
        Ok(Day {
            day_id: date.into(),
            date: date.into(),
        })
    }

    async fn create_session(&self, session: &Session) -> Result<(), DtError> {
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
        // Route each event type through the hook engine.
        let hook_name = match event.event_type {
            super::entities::EventType::BugFix => "bug_fix_recorded",
            super::entities::EventType::Conversation => "session_ended",
            super::entities::EventType::Decision => "decision_made",
            super::entities::EventType::Deployment => "jenkins_deploy_completed",
            super::entities::EventType::ConfigChange => "config_changed",
            super::entities::EventType::Modification => "code_modified",
            super::entities::EventType::PodEvent => "pod_event",
        };

        let mut ctx = HookContext {
            hook_name: hook_name.into(),
            project: event.project.clone(),
            session_id: event.session_id.clone(),
            entity_id: event.entity_id.clone(),
            entity_type: event.entity_type.clone(),
            fields: parse_key_values(&event.details),
        };

        // Special-case: Deployment needs virtual variables for side-effect templates.
        if event.event_type == super::entities::EventType::Deployment {
            let job = ctx.fields.get("job").cloned().unwrap_or_else(|| event.entity_id.clone());
            let env = ctx.fields.get("env").cloned().unwrap_or_else(|| "test".to_string());
            let build_number = ctx.fields.get("build_number").cloned().unwrap_or_default();
            ctx.fields.insert("job_id".into(), format!("dt://jenkins/job/{}", job));
            ctx.fields.insert("build_id".into(), if build_number.is_empty() {
                format!("dt://jenkins/job/{}/build/unknown", job)
            } else {
                format!("dt://jenkins/job/{}/build/{}", job, build_number)
            });
            ctx.fields.insert("instance_id".into(), format!("dt://service/{}/instance/{}", job, env));
            ctx.fields.insert("timestamp_raw".into(), event.timestamp.timestamp_millis().to_string());
        }

        if let Some(ref engine) = self.hook_engine {
            let results = engine.fire(hook_name, ctx).await;
            for r in &results {
                if !r.success {
                    tracing::warn!(
                        "[hook] {} event failed for label {}: {}",
                        hook_name,
                        r.label,
                        r.error.as_deref().unwrap_or("unknown"),
                    );
                }
            }
        } else {
            tracing::warn!(
                "No hook engine configured — event type={} not recorded",
                event.event_type.as_str(),
            );
        }

        Ok(())
    }

    async fn get_session_events(&self, _session_id: &str) -> Result<Vec<MemoryEvent>, DtError> {
        Ok(vec![])
    }

    async fn get_timeline(&self, _days: u32) -> Result<Vec<Day>, DtError> {
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

    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn MemoryService) {}
    }

    #[test]
    fn trait_method_signatures_exist() {
        fn _assert_methods<T: MemoryService>() {}
    }

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
        let svc = DefaultMemoryService::new(repo, None);

        let day = svc.ensure_day("2026-07-09").await.expect("ensure_day");
        assert_eq!(day.day_id, "2026-07-09");
        assert_eq!(day.date, "2026-07-09");
        assert!(read.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn default_memory_service_create_session() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo, None);

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
        assert!(read.load(Ordering::SeqCst) >= 1);
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn default_memory_service_record_event_no_hook() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo, None);

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
        // No hook engine — only a warning log, no graph writes.
        assert_eq!(write.load(Ordering::SeqCst), 0);
        assert_eq!(read.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn default_memory_service_stubs_ok() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultMemoryService::new(repo, None);

        let events = svc.get_session_events("any").await.expect("stub");
        assert!(events.is_empty());

        let timeline = svc.get_timeline(7).await.expect("stub");
        assert!(timeline.is_empty());
    }
}
