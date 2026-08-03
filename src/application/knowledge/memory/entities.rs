//! Memory 世界实体：Day、Session、MemoryEvent。
//!
//! 这些构成知识图谱的时间维度：
//! ```text
//! (:Day {day_id: "2026-07-09"})
//!   └─[:HAS_SESSION]-> (:Session {session_id: "2026-07-09-001"})
//!        ├─[:HAS_EVENT]-> (:Modification)
//!        ├─[:HAS_EVENT]-> (:ConfigChange)
//!        └─[:HAS_EVENT]-> (:Deployment)
//! ```

use serde::{Deserialize, Serialize};

/// Day 节点——时间维度上的一个日历日。
///
/// 当某天的首个事件被记录时惰性创建。
/// 每天仅一个节点（通过 `day_id` 唯一性保证幂等）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Day {
    /// 唯一日标识，格式 "YYYY-MM-DD"。
    pub day_id: String,
    /// 人类可读的日期字符串，格式 "YYYY-MM-DD"。
    pub date: String,
}

/// Session 节点——一天内的工作会话。
///
/// 一天可有多个会话，按顺序编号。
/// 由 Day 的恰好一条 [:HAS_SESSION] 关系拥有。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// 唯一会话标识，如 "2026-07-09-001" 或 UUID。
    pub session_id: String,
    /// 会话的人类可读摘要。
    pub summary: String,
    /// 会话期间做出的关键架构 / 技术决策。
    pub key_decisions: Vec<String>,
    /// 可选的线程/会话标识，用于分组。
    pub thread_id: Option<String>,
    /// 会话开始时间（ISO 8601）。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 会话结束时间（仍活跃时为 None）。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// memory 系统支持的所有事件类型。
///
/// 每种事件类型映射到各自的图标签
/// （如 `EventType::Modification` → `:Modification` 标签）。
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
    /// 返回用于 Cypher 标签的字符串表示。
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

/// 记忆事件——会话期间发生的某件事。
///
/// 每个事件通过 [:HAS_EVENT] 关联到其父 Session。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    /// 事件类型。
    pub event_type: EventType,
    /// 外部实体标识（如 Jenkins job 名、Nacos data_id）。
    pub entity_id: String,
    /// 外部实体类型标签（如 "JenkinsJob"、"NacosConfig"）。
    pub entity_type: String,
    /// 所属项目名。
    pub project: String,
    /// 人类可读的详情 / 描述。
    pub details: String,
    /// 该事件所属的会话。
    pub session_id: String,
    /// 事件发生时间。
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// 测试
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
        let json = serde_json::to_string(&evt).expect("序列化应成功");
        let back: MemoryEvent = serde_json::from_str(&json).expect("反序列化应成功");
        assert_eq!(back.event_type, EventType::Deployment);
        assert_eq!(back.entity_id, "my-job");
        assert_eq!(back.project, "digital-twin");
    }
}
