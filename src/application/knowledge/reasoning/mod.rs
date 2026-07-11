// Reasoning World entities: Observation, Analysis, Decision.
//
// These form the reasoning dimension of the knowledge graph:
// ```text
// (:Observation)-[:ABOUT]->(:Method|:Class|:Service)
// (:Observation)-[:UPGRADES_TO]->(:Knowledge)
//
// (:Analysis)-[:TRIGGERED_BY]->(:Session)
// (:Analysis)-[:EXAMINED]->(:Method|:Class)
// (:Analysis)-[:PRODUCED]->(:Observation|:Decision)
//
// (:Decision)-[:BASED_ON]->(:Knowledge)
// (:Decision)-[:BELONGS_TO]->(:Thread)
// ```
//
// Three-tier progression:
//   Observation(发现) → Analysis(分析) → Decision(决策)

pub mod lifecycle;
pub mod service;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Observation — a factual observation or evidence about code/system
// ---------------------------------------------------------------------------

/// An observation represents a factual discovery about the codebase or system
/// behaviour. Observations are the first tier in the reasoning pipeline and
/// form the evidence base for subsequent analysis.
///
/// Label: `:Observation`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// Unique observation identifier.
    pub observation_id: String,
    /// Human-readable description of what was observed.
    pub description: String,
    /// Supporting evidence (e.g. log excerpts, code snippets, URLs).
    pub evidence: String,
    /// List of entity identifiers this observation is about.
    pub entities: Vec<String>,
    /// Observed pattern or category (e.g. "null-pointer", "timeout").
    pub pattern: Option<String>,
    /// Confidence in this observation (0.0–1.0).
    pub confidence: f64,
    /// The session in which this observation was made.
    pub session_id: String,
    /// When the observation was recorded (ISO 8601).
    pub timestamp: String,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            observation_id: String::new(),
            description: String::new(),
            evidence: String::new(),
            entities: Vec::new(),
            pattern: None,
            confidence: 0.5,
            session_id: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// Analysis — the reasoning process that connects observations to decisions
// ---------------------------------------------------------------------------

/// An analysis represents a structured investigation triggered by a session.
/// It examines observations (and code entities) and may produce new
/// observations or decisions.
///
/// Label: `:Analysis`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    /// Unique analysis identifier.
    pub analysis_id: String,
    /// The question or problem being investigated.
    pub question: String,
    /// Initial hypothesis about the answer.
    pub hypothesis: Option<String>,
    /// Method or approach used for analysis (e.g. "root-cause", "comparison").
    pub method: String,
    /// Intermediate reasoning steps.
    pub intermediate: Vec<Step>,
    /// Final conclusion drawn from the analysis.
    pub conclusion: String,
    /// Confidence in the conclusion (0.0–1.0).
    pub confidence: f64,
    /// Total cost in milliseconds (API calls, processing time).
    pub total_cost_ms: Option<u64>,
    /// The session that triggered this analysis.
    pub session_id: String,
    /// When the analysis was recorded (ISO 8601).
    pub timestamp: String,
}

impl Default for Analysis {
    fn default() -> Self {
        Self {
            analysis_id: String::new(),
            question: String::new(),
            hypothesis: None,
            method: String::new(),
            intermediate: Vec::new(),
            conclusion: String::new(),
            confidence: 0.5,
            total_cost_ms: None,
            session_id: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// A single intermediate reasoning step within an analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Execution order (1-indexed).
    pub order: u32,
    /// The thought or action taken.
    pub thought: String,
    /// Tool used (e.g. "search", "read", "grep").
    pub tool: Option<String>,
    /// Target entity or file examined.
    pub target: Option<String>,
    /// What was discovered at this step.
    pub finding: String,
}

// ---------------------------------------------------------------------------
// Decision — a reasoned decision (may not yet be confirmed)
// ---------------------------------------------------------------------------

/// A decision represents a reasoned choice made during analysis. Unlike
/// Memory World Decisions (which are confirmed/archived), Reasoning World
/// Decisions may still be tentative (verified = false).
///
/// Once confirmed via [`ReasoningService::confirm_decision`], the decision
/// is considered final (verified = true).
///
/// Label: `:Decision`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Unique decision identifier.
    pub decision_id: String,
    /// Short title describing the decision.
    pub title: String,
    /// Background context — why this decision was needed.
    pub context: String,
    /// Comma-separated list of alternatives considered.
    pub alternatives: String,
    /// Supporting evidence for the decision.
    pub evidence: String,
    /// The chosen option.
    pub choice: String,
    /// Rationale explaining why this choice was made.
    pub rationale: String,
    /// Expected consequences of the decision.
    pub consequences: String,
    /// Confidence in this decision (0.0–1.0).
    pub confidence: f64,
    /// Whether this decision has been confirmed (verified) as final.
    pub verified: bool,
    /// The session in which this decision was made.
    pub session_id: String,
    /// Knowledge node this decision is based on.
    pub knowledge_id: Option<String>,
    /// Thread this decision belongs to.
    pub thread_id: Option<String>,
    /// When the decision was made (ISO 8601).
    pub timestamp: String,
}

impl Default for Decision {
    fn default() -> Self {
        Self {
            decision_id: String::new(),
            title: String::new(),
            context: String::new(),
            alternatives: String::new(),
            evidence: String::new(),
            choice: String::new(),
            rationale: String::new(),
            consequences: String::new(),
            confidence: 0.5,
            verified: false,
            session_id: String::new(),
            knowledge_id: None,
            thread_id: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// ReasoningChain — the complete reasoning trace for a session
// ---------------------------------------------------------------------------

/// A reasoning chain bundles all three tiers (observations, analyses,
/// decisions) for a given session, forming a complete reasoning trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningChain {
    /// All observations recorded during the session.
    pub observations: Vec<Observation>,
    /// All analyses performed during the session.
    pub analyses: Vec<Analysis>,
    /// All decisions made during the session.
    pub decisions: Vec<Decision>,
}

impl ReasoningChain {
    /// Create an empty reasoning chain.
    pub fn empty() -> Self {
        Self {
            observations: Vec::new(),
            analyses: Vec::new(),
            decisions: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_defaults() {
        let obs = Observation::default();
        assert!(obs.observation_id.is_empty());
        assert!(obs.entities.is_empty());
        assert!(obs.pattern.is_none());
        assert!((obs.confidence - 0.5).abs() < f64::EPSILON);
        assert!(!obs.timestamp.is_empty());
    }

    #[test]
    fn observation_entity_fields() {
        let obs = Observation {
            observation_id: "obs://session-001/001".into(),
            description: "Null pointer in PaymentService".into(),
            evidence: "Stack trace: NPE at PayService.java:142".into(),
            entities: vec!["dt://entity/test/class/PayService".into()],
            pattern: Some("null-pointer".into()),
            confidence: 0.95,
            session_id: "2026-07-09-001".into(),
            timestamp: "2026-07-09T10:00:00Z".into(),
        };
        assert_eq!(obs.observation_id, "obs://session-001/001");
        assert_eq!(obs.entities.len(), 1);
        assert_eq!(obs.pattern.as_deref(), Some("null-pointer"));
        assert!((obs.confidence - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn analysis_defaults() {
        let a = Analysis::default();
        assert!(a.analysis_id.is_empty());
        assert!(a.hypothesis.is_none());
        assert!(a.intermediate.is_empty());
        assert!(a.total_cost_ms.is_none());
        assert!((a.confidence - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn analysis_with_steps() {
        let step = Step {
            order: 1,
            thought: "Check if NPE is from null channelExtra".into(),
            tool: Some("grep".into()),
            target: Some("PayService.java".into()),
            finding: "channelExtra is set in constructor".into(),
        };

        let analysis = Analysis {
            analysis_id: "an://session-001/001".into(),
            question: "Why is PayService throwing NPE?".into(),
            hypothesis: Some("Uninitialized channelExtra field".into()),
            method: "root-cause".into(),
            intermediate: vec![step],
            conclusion: "channelExtra is initialized correctly; NPE elsewhere".into(),
            confidence: 0.7,
            total_cost_ms: Some(1500),
            session_id: "2026-07-09-001".into(),
            timestamp: "2026-07-09T10:30:00Z".into(),
        };
        assert_eq!(analysis.question, "Why is PayService throwing NPE?");
        assert_eq!(analysis.intermediate.len(), 1);
        assert_eq!(analysis.intermediate[0].order, 1);
        assert!(analysis.total_cost_ms.is_some());
    }

    #[test]
    fn step_fields() {
        let s = Step {
            order: 1,
            thought: "Look at the stack trace".into(),
            tool: Some("read".into()),
            target: Some("src/main.rs".into()),
            finding: "Found the bug source".into(),
        };
        assert_eq!(s.order, 1);
        assert_eq!(s.tool.as_deref(), Some("read"));
        assert!(s.target.is_some());
    }

    #[test]
    fn decision_defaults() {
        let d = Decision::default();
        assert!(d.decision_id.is_empty());
        assert!(!d.verified);
        assert!(d.knowledge_id.is_none());
        assert!(d.thread_id.is_none());
        assert!((d.confidence - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn decision_entity_fields() {
        let d = Decision {
            decision_id: "dec://session-001/001".into(),
            title: "Use Redis for caching".into(),
            context: "Need fast caching layer".into(),
            alternatives: "Redis, Memcached, LocalCache".into(),
            evidence: "Redis has better persistence and team expertise".into(),
            choice: "Redis".into(),
            rationale: "Better persistence, built-in clustering, team knows it".into(),
            consequences: "Need Redis cluster, adds ops overhead".into(),
            confidence: 0.9,
            verified: false,
            session_id: "2026-07-09-001".into(),
            knowledge_id: Some("dt://knowledge/test/caching-strategy".into()),
            thread_id: Some("dt://thread/test/default".into()),
            timestamp: "2026-07-09T11:00:00Z".into(),
        };
        assert_eq!(d.title, "Use Redis for caching");
        assert!(!d.verified);
        assert!(d.knowledge_id.is_some());
        assert!(d.thread_id.is_some());
    }

    #[test]
    fn reasoning_chain_empty() {
        let chain = ReasoningChain::empty();
        assert!(chain.observations.is_empty());
        assert!(chain.analyses.is_empty());
        assert!(chain.decisions.is_empty());
    }

    #[test]
    fn reasoning_chain_with_data() {
        let chain = ReasoningChain {
            observations: vec![Observation::default()],
            analyses: vec![Analysis::default()],
            decisions: vec![Decision::default()],
        };
        assert_eq!(chain.observations.len(), 1);
        assert_eq!(chain.analyses.len(), 1);
        assert_eq!(chain.decisions.len(), 1);
    }

    #[test]
    fn observation_serialization_roundtrip() {
        let obs = Observation {
            observation_id: "obs://test/001".into(),
            description: "A test observation".into(),
            evidence: "Some evidence".into(),
            entities: vec!["entity-1".into()],
            pattern: Some("test-pattern".into()),
            confidence: 0.8,
            session_id: "s001".into(),
            timestamp: "2026-07-09T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&obs).expect("serialize");
        let back: Observation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.observation_id, obs.observation_id);
        assert_eq!(back.confidence, 0.8);
        assert_eq!(back.entities.len(), 1);
    }

    #[test]
    fn analysis_serialization_roundtrip() {
        let analysis = Analysis {
            analysis_id: "an://test/001".into(),
            question: "Why?".into(),
            hypothesis: Some("Maybe".into()),
            method: "root-cause".into(),
            intermediate: vec![Step {
                order: 1,
                thought: "Check".into(),
                tool: None,
                target: None,
                finding: "Found".into(),
            }],
            conclusion: "Because".into(),
            confidence: 0.9,
            total_cost_ms: Some(500),
            session_id: "s001".into(),
            timestamp: "2026-07-09T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&analysis).expect("serialize");
        let back: Analysis = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.analysis_id, analysis.analysis_id);
        assert_eq!(back.intermediate.len(), 1);
    }

    #[test]
    fn decision_serialization_roundtrip() {
        let d = Decision {
            decision_id: "dec://test/001".into(),
            title: "Use X".into(),
            context: "Need X".into(),
            alternatives: "X,Y".into(),
            evidence: "Team uses X".into(),
            choice: "X".into(),
            rationale: "Best fit".into(),
            consequences: "Need ops".into(),
            confidence: 0.85,
            verified: true,
            session_id: "s001".into(),
            knowledge_id: Some("dt://knowledge/test/x".into()),
            thread_id: Some("dt://thread/test/default".into()),
            timestamp: "2026-07-09T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&d).expect("serialize");
        let back: Decision = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.decision_id, d.decision_id);
        assert!(back.verified);
    }
}
