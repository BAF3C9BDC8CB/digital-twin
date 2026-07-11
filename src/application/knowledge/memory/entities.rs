//! Memory world entities: Day, Session, MemoryEvent.
//!
//! These form the time dimension of the knowledge graph:
//! ```text
//! (:Day {day_id: "2026-07-09"})
//!   └─[:HAS_SESSION]-> (:Session {session_id: "2026-07-09-001"})
//!        ├─[:HAS_EVENT]-> (:Modification)
//!        ├─[:HAS_EVENT]-> (:ConfigChange)
//!        └─[:HAS_EVENT]-> (:Deployment)
//! ```

use serde::{Deserialize, Serialize};

/// Day node — represents a calendar day in the time dimension.
///
/// Created lazily when the first event of a day is recorded.
/// Only one node per date (idempotent via `day_id` uniqueness).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Day {
    /// Unique day identifier, format "YYYY-MM-DD".
    pub day_id: String,
    /// Human-readable date string, format "YYYY-MM-DD".
    pub date: String,
}

/// Session node — a work session within a day.
///
/// A day can have multiple sessions, numbered sequentially.
/// Owned by exactly one [:HAS_SESSION] relationship from a Day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier, e.g. "2026-07-09-001" or UUID.
    pub session_id: String,
    /// Human-readable summary of the session.
    pub summary: String,
    /// Key architectural / technical decisions made during the session.
    pub key_decisions: Vec<String>,
    /// Optional thread/conversation identifier for grouping.
    pub thread_id: Option<String>,
    /// Session start time (ISO 8601).
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Session end time (None if still active).
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// All event types supported by the memory system.
///
/// Each event type maps to its own Neo4j label
/// (e.g. `EventType::Modification` → `:Modification` label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    Modification,
    Deployment,
    ConfigChange,
    BugFix,
    Decision,
    PodEvent,
    Conversation,
}

impl EventType {
    /// Return the string representation used in Cypher labels.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Modification => "Modification",
            EventType::Deployment => "Deployment",
            EventType::ConfigChange => "ConfigChange",
            EventType::BugFix => "BugFix",
            EventType::Decision => "Decision",
            EventType::PodEvent => "PodEvent",
            EventType::Conversation => "Conversation",
        }
    }
}

/// A memory event — something that happened during a session.
///
/// Each event is linked to its parent Session via [:HAS_EVENT].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    /// The kind of event.
    pub event_type: EventType,
    /// External entity identifier (e.g. Jenkins job name, Nacos data_id).
    pub entity_id: String,
    /// External entity type label (e.g. "JenkinsJob", "NacosConfig").
    pub entity_type: String,
    /// Owning project name.
    pub project: String,
    /// Human-readable details / description.
    pub details: String,
    /// The session this event belongs to.
    pub session_id: String,
    /// When the event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_as_str_matches_label() {
        assert_eq!(EventType::Modification.as_str(), "Modification");
        assert_eq!(EventType::Deployment.as_str(), "Deployment");
        assert_eq!(EventType::ConfigChange.as_str(), "ConfigChange");
        assert_eq!(EventType::BugFix.as_str(), "BugFix");
        assert_eq!(EventType::Decision.as_str(), "Decision");
        assert_eq!(EventType::PodEvent.as_str(), "PodEvent");
        assert_eq!(EventType::Conversation.as_str(), "Conversation");
    }

    #[test]
    fn day_entity_fields() {
        let day = Day {
            day_id: "2026-07-09".into(),
            date: "2026-07-09".into(),
        };
        assert_eq!(day.day_id, "2026-07-09");
        assert_eq!(day.date, day.day_id);
    }

    #[test]
    fn session_entity_optional_fields() {
        let t = chrono::Utc::now();
        let session = Session {
            session_id: "2026-07-09-001".into(),
            summary: "Test session".into(),
            key_decisions: vec!["use async-trait".into()],
            thread_id: Some("thread-1".into()),
            started_at: t,
            ended_at: None,
        };
        assert_eq!(session.session_id, "2026-07-09-001");
        assert!(session.thread_id.is_some());
        assert!(session.ended_at.is_none());
        assert_eq!(session.key_decisions.len(), 1);
    }

    #[test]
    fn memory_event_serialization_roundtrip() {
        let t = chrono::Utc::now();
        let evt = MemoryEvent {
            event_type: EventType::Deployment,
            entity_id: "my-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "digital-twin".into(),
            details: "Deploy to production".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: t,
        };
        let json = serde_json::to_string(&evt).expect("serialize");
        let back: MemoryEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.event_type, EventType::Deployment);
        assert_eq!(back.entity_id, "my-job");
        assert_eq!(back.project, "digital-twin");
    }
}
