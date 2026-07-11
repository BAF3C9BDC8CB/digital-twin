//! Knowledge World entities: Knowledge, KnowledgeVersion, Playbook, Experience,
//! Concept, Domain.
//!
//! These form the knowledge dimension of the knowledge graph:
//! ```text
//! (:Domain)-[:CONTAINS]->(:Knowledge)
//! (:Domain)-[:CONTAINS]->(:Concept)
//! (:Knowledge)-[:EVOLVED_FROM]->(:Knowledge)
//! (:KnowledgeVersion)-[:RECORDS]->(:Knowledge)
//! (:Playbook)-[:USES_KNOWLEDGE]->(:Knowledge)
//! (:Experience)-[:RELATED_TO]->(:Knowledge)
//! ```

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Knowledge — the central knowledge entity
// ---------------------------------------------------------------------------

/// A knowledge entry representing a concept, pattern, or insight.
///
/// Knowledge can be sourced from AI sessions, tasks, documents, code comments,
/// user dictation, or execution results. AI-generated knowledge has lower
/// confidence; human-verified knowledge has confidence = 1.0.
///
/// Version management: updates do NOT modify existing nodes. Instead, a new
/// Knowledge node is created and linked to the old one via `[:EVOLVED_FROM]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Knowledge {
    /// Unique knowledge identifier (dt://knowledge/{project}/{domain}/{name}).
    pub knowledge_id: String,
    /// Short name for the knowledge entry.
    pub name: String,
    /// Human-readable title.
    pub title: String,
    /// Domain classification (e.g. "支付", "部署", "配置").
    pub domain: String,
    /// One-sentence summary.
    pub summary: String,
    /// Full markdown content.
    pub content: String,
    /// Formal definition (for concept-style knowledge).
    pub definition: String,
    /// Origin of this knowledge.
    pub source: KnowledgeSource,
    /// Owning project name.
    pub project: String,
    /// Confidence 0.0–1.0. AI-generated = low, human-verified = 1.0.
    pub confidence: f64,
    /// Who verified this ("human" or null-equivalent).
    pub verified_by: Option<String>,
    /// Creation timestamp (ISO 8601).
    pub created_at: String,
    /// Last update timestamp (ISO 8601).
    pub updated_at: String,
    /// Version number: 1 for newly created, increments on update.
    pub version: u32,
}

/// Where a piece of knowledge came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeSource {
    /// Derived from an AI conversation session.
    #[serde(rename = "ai_session")]
    AiSession,
    /// Produced as a result of an AI task execution.
    #[serde(rename = "ai_task")]
    AiTask,
    /// Extracted from project documentation.
    #[serde(rename = "document")]
    Document,
    /// Extracted from code comments / annotations.
    #[serde(rename = "code_comment")]
    CodeComment,
    /// Explicitly dictated by a human user.
    #[serde(rename = "user_dictation")]
    UserDictation,
    /// Derived from execution results / logs.
    #[serde(rename = "execution_result")]
    ExecutionResult,
}

impl KnowledgeSource {
    /// Return the string representation used in Cypher/N4j labels.
    pub fn as_str(&self) -> &'static str {
        match self {
            KnowledgeSource::AiSession => "ai_session",
            KnowledgeSource::AiTask => "ai_task",
            KnowledgeSource::Document => "document",
            KnowledgeSource::CodeComment => "code_comment",
            KnowledgeSource::UserDictation => "user_dictation",
            KnowledgeSource::ExecutionResult => "execution_result",
        }
    }

    /// Parse from a string, defaulting to AiSession for unknown values.
    pub fn parse(s: &str) -> Self {
        match s {
            "ai_session" => KnowledgeSource::AiSession,
            "ai_task" => KnowledgeSource::AiTask,
            "document" => KnowledgeSource::Document,
            "code_comment" => KnowledgeSource::CodeComment,
            "user_dictation" => KnowledgeSource::UserDictation,
            "execution_result" => KnowledgeSource::ExecutionResult,
            _ => KnowledgeSource::AiSession,
        }
    }
}

impl Default for Knowledge {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            knowledge_id: String::new(),
            name: String::new(),
            title: String::new(),
            domain: String::new(),
            summary: String::new(),
            content: String::new(),
            definition: String::new(),
            source: KnowledgeSource::AiSession,
            project: String::new(),
            confidence: 0.5,
            verified_by: None,
            created_at: now.clone(),
            updated_at: now,
            version: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// KnowledgeVersion — records each evolution of a knowledge entry
// ---------------------------------------------------------------------------

/// A version record capturing what changed between two versions of knowledge.
///
/// Linked to the NEW version via `[:RECORDS]->(:Knowledge)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeVersion {
    /// Unique version identifier (dt://knowledge-version/{knowledge_id}/v{version}).
    pub version_id: String,
    /// The knowledge node this version describes.
    pub knowledge_id: String,
    /// Version number (1, 2, 3, ...).
    pub version: u32,
    /// Human-readable diff / change summary.
    pub diff: String,
    /// The session in which this version was created.
    pub session_id: String,
    /// When this version was recorded (ISO 8601).
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// Playbook — executable how-to manual
// ---------------------------------------------------------------------------

/// A playbook is a structured, executable how-to guide.
///
/// Consists of ordered steps that can be followed by AI or humans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playbook {
    /// Unique playbook identifier (dt://playbook/{project}/{name}).
    pub playbook_id: String,
    /// Name of the playbook.
    pub name: String,
    /// When this playbook is applicable.
    pub description: String,
    /// Ordered execution steps.
    pub steps: Vec<Step>,
    /// Domain classification.
    pub domain: String,
    /// Owning project.
    pub project: String,
    /// How many times this playbook was used successfully.
    pub success_count: u64,
    /// How many times this playbook failed.
    pub failure_count: u64,
    /// Auto-flagged when success rate < 70%.
    pub _needs_review: bool,
    /// Creation time (ISO 8601).
    pub created_at: String,
}

/// A single step in a playbook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Execution order (1-indexed).
    pub order: u32,
    /// What to do.
    pub action: String,
    /// Which tool to use (e.g. "edit", "bash", "search").
    pub tool: String,
    /// What file or entity to target.
    pub target: Option<String>,
    /// What the expected outcome looks like.
    pub expected: String,
    /// Gotchas and pitfalls to watch out for.
    pub pitfall: Option<String>,
}

// ---------------------------------------------------------------------------
// Experience — lessons learned / war stories
// ---------------------------------------------------------------------------

/// An experience record capturing a lesson learned or pitfall encountered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experience {
    /// Unique experience identifier (dt://experience/{project}/{id}).
    pub experience_id: String,
    /// Short title describing the experience.
    pub title: String,
    /// One-sentence takeaway.
    pub summary: String,
    /// Detailed narrative.
    pub content: String,
    /// Domain classification.
    pub domain: String,
    /// Severity of the lesson.
    pub severity: ExperienceSeverity,
    /// Owning project.
    pub project: String,
    /// When this experience was recorded (ISO 8601).
    pub created_at: String,
}

/// Severity level for experiences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExperienceSeverity {
    /// Critical — caused an outage or data loss.
    #[serde(rename = "critical")]
    Critical,
    /// Warning — a near-miss or potential issue.
    #[serde(rename = "warning")]
    Warning,
    /// Informational — a general tip.
    #[serde(rename = "info")]
    Info,
}

impl ExperienceSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExperienceSeverity::Critical => "critical",
            ExperienceSeverity::Warning => "warning",
            ExperienceSeverity::Info => "info",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "critical" => ExperienceSeverity::Critical,
            "warning" => ExperienceSeverity::Warning,
            _ => ExperienceSeverity::Info,
        }
    }
}

// ---------------------------------------------------------------------------
// Concept — a domain term / definition
// ---------------------------------------------------------------------------

/// A concept is a defined term within a domain.
///
/// Concepts help the AI understand project-specific jargon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Concept {
    /// Unique concept identifier (dt://concept/{domain}/{name}).
    pub concept_id: String,
    /// The term or concept name.
    pub name: String,
    /// Formal definition.
    pub definition: String,
    /// Domain classification.
    pub domain: String,
    /// Extended explanation.
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Domain — a knowledge domain / category
// ---------------------------------------------------------------------------

/// A domain groups related knowledge, concepts, and playbooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domain {
    /// Unique domain identifier (dt://domain/{name}).
    pub domain_id: String,
    /// Domain name (e.g. "支付", "部署", "配置").
    pub name: String,
    /// Human-readable description.
    pub description: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_default_values() {
        let k = Knowledge::default();
        assert!(k.knowledge_id.is_empty());
        assert_eq!(k.version, 1);
        assert_eq!(k.source, KnowledgeSource::AiSession);
        assert!(k.confidence > 0.0);
        assert!(k.verified_by.is_none());
    }

    #[test]
    fn knowledge_source_parsing() {
        assert_eq!(KnowledgeSource::parse("ai_session"), KnowledgeSource::AiSession);
        assert_eq!(KnowledgeSource::parse("ai_task"), KnowledgeSource::AiTask);
        assert_eq!(KnowledgeSource::parse("document"), KnowledgeSource::Document);
        assert_eq!(KnowledgeSource::parse("code_comment"), KnowledgeSource::CodeComment);
        assert_eq!(KnowledgeSource::parse("user_dictation"), KnowledgeSource::UserDictation);
        assert_eq!(KnowledgeSource::parse("execution_result"), KnowledgeSource::ExecutionResult);
        // Unknown defaults to AiSession.
        assert_eq!(KnowledgeSource::parse("garbage"), KnowledgeSource::AiSession);
    }

    #[test]
    fn knowledge_source_as_str() {
        assert_eq!(KnowledgeSource::AiSession.as_str(), "ai_session");
        assert_eq!(KnowledgeSource::CodeComment.as_str(), "code_comment");
    }

    #[test]
    fn experience_severity_parsing() {
        assert_eq!(ExperienceSeverity::parse("critical"), ExperienceSeverity::Critical);
        assert_eq!(ExperienceSeverity::parse("CRITICAL"), ExperienceSeverity::Critical);
        assert_eq!(ExperienceSeverity::parse("warning"), ExperienceSeverity::Warning);
        assert_eq!(ExperienceSeverity::parse("info"), ExperienceSeverity::Info);
        // Unknown defaults to Info.
        assert_eq!(ExperienceSeverity::parse("unknown"), ExperienceSeverity::Info);
    }

    #[test]
    fn experience_severity_as_str() {
        assert_eq!(ExperienceSeverity::Critical.as_str(), "critical");
        assert_eq!(ExperienceSeverity::Warning.as_str(), "warning");
        assert_eq!(ExperienceSeverity::Info.as_str(), "info");
    }

    #[test]
    fn playbook_step_fields() {
        let step = Step {
            order: 1,
            action: "修改 ifCode".into(),
            tool: "edit".into(),
            target: Some("PayService.java".into()),
            expected: "ifCode 从 allinpay 改为 ysf".into(),
            pitfall: Some("别忘了同步改 channelExtra".into()),
        };
        assert_eq!(step.order, 1);
        assert_eq!(step.action, "修改 ifCode");
        assert!(step.target.is_some());
        assert!(step.pitfall.is_some());
    }

    #[test]
    fn concept_entity_fields() {
        let c = Concept {
            concept_id: "dt://concept/支付/ifCode".into(),
            name: "ifCode".into(),
            definition: "支付渠道编码".into(),
            domain: "支付".into(),
            summary: "用于标识不同支付渠道的编码".into(),
        };
        assert_eq!(c.name, "ifCode");
        assert_eq!(c.domain, "支付");
    }

    #[test]
    fn domain_entity_fields() {
        let d = Domain {
            domain_id: "dt://domain/支付".into(),
            name: "支付".into(),
            description: "支付相关知识和概念".into(),
        };
        assert_eq!(d.name, "支付");
        assert_eq!(d.description, "支付相关知识和概念");
    }

    #[test]
    fn knowledge_version_fields() {
        let kv = KnowledgeVersion {
            version_id: "dt://knowledge-version/dt://knowledge/test/支付/pay-platform/v2".into(),
            knowledge_id: "dt://knowledge/test/支付/pay-platform".into(),
            version: 2,
            diff: "新增 pitfall: pay-timeout.yml 容易遗漏".into(),
            session_id: "2026-07-09-001".into(),
            timestamp: "2026-07-09T10:00:00Z".into(),
        };
        assert_eq!(kv.version, 2);
        assert!(kv.diff.contains("pitfall"));
    }

    #[test]
    fn knowledge_serialization_roundtrip() {
        let k = Knowledge {
            knowledge_id: "dt://knowledge/test/支付/test".into(),
            name: "test-knowledge".into(),
            title: "Test Knowledge".into(),
            domain: "支付".into(),
            summary: "A test entry".into(),
            content: "Some content".into(),
            definition: "A definition".into(),
            source: KnowledgeSource::AiSession,
            project: "test".into(),
            confidence: 0.8,
            verified_by: Some("human".into()),
            created_at: "2026-07-09T00:00:00Z".into(),
            updated_at: "2026-07-09T00:00:00Z".into(),
            version: 1,
        };
        let json = serde_json::to_string(&k).expect("serialize");
        let back: Knowledge = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.knowledge_id, k.knowledge_id);
        assert_eq!(back.confidence, 0.8);
        assert_eq!(back.source, KnowledgeSource::AiSession);
    }

    #[test]
    fn playbook_serialization_roundtrip() {
        let p = Playbook {
            playbook_id: "dt://playbook/test/migrate-payment".into(),
            name: "支付平台迁移".into(),
            description: "适用于支付平台切换场景".into(),
            steps: vec![Step {
                order: 1,
                action: "修改 ifCode".into(),
                tool: "edit".into(),
                target: Some("PayService.java".into()),
                expected: "改了".into(),
                pitfall: None,
            }],
            domain: "支付".into(),
            project: "test".into(),
            success_count: 10,
            failure_count: 2,
            _needs_review: false,
            created_at: "2026-07-09T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&p).expect("serialize");
        let back: Playbook = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.name, "支付平台迁移");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.success_count, 10);
    }
}
