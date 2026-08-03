// Reasoning 世界实体：Observation、Analysis、Decision。
//
// 这些构成知识图谱的推理维度：
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
// 三级递进：
//   Observation(发现) → Analysis(分析) → Decision(决策)

pub mod lifecycle;
pub mod service;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Observation — 关于代码/系统的事实性观察或证据
// ---------------------------------------------------------------------------

/// 观察（observation）表示对代码库或系统行为的事实性发现。
/// 观察是推理管线的第一级，构成后续分析的证据基础。
///
/// 标签：`:Observation`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// 唯一观察标识。
    pub observation_id: String,
    /// 对所观察内容的人类可读描述。
    pub description: String,
    /// 支撑证据（如日志摘录、代码片段、URL）。
    pub evidence: String,
    /// 该观察涉及的实体标识列表。
    pub entities: Vec<String>,
    /// 观察到的模式或类别（如 "null-pointer"、"timeout"）。
    pub pattern: Option<String>,
    /// 该观察的置信度（0.0–1.0）。
    pub confidence: f64,
    /// 做出该观察的会话。
    pub session_id: String,
    /// 观察记录时间（ISO 8601）。
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
// Analysis — 将观察连接到决策的推理过程
// ---------------------------------------------------------------------------

/// 分析（analysis）表示由会话触发的结构化调查。
/// 它审视观察（以及代码实体），并可能产出新的观察或决策。
///
/// 标签：`:Analysis`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    /// 唯一分析标识。
    pub analysis_id: String,
    /// 正在调查的问题。
    pub question: String,
    /// 关于答案的初始假设。
    pub hypothesis: Option<String>,
    /// 分析方法或途径（如 "root-cause"、"comparison"）。
    pub method: String,
    /// 中间推理步骤。
    pub intermediate: Vec<Step>,
    /// 从分析得出的最终结论。
    pub conclusion: String,
    /// 结论的置信度（0.0–1.0）。
    pub confidence: f64,
    /// 总耗时毫秒数（API 调用、处理时间）。
    pub total_cost_ms: Option<u64>,
    /// 触发本次分析的会话。
    pub session_id: String,
    /// 分析记录时间（ISO 8601）。
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

/// 分析中的单个中间推理步骤。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// 执行顺序（从 1 开始）。
    pub order: u32,
    /// 思路或采取的动作。
    pub thought: String,
    /// 使用的工具（如 "search"、"read"、"grep"）。
    pub tool: Option<String>,
    /// 被检查的目标实体或文件。
    pub target: Option<String>,
    /// 该步骤中发现了什么。
    pub finding: String,
}

// ---------------------------------------------------------------------------
// Decision — 经过推理的决策（可能尚未确认）
// ---------------------------------------------------------------------------

/// 决策（decision）表示分析期间做出的经过推理的选择。与
/// Memory 世界的决策（已确认/已归档）不同，Reasoning 世界的
/// 决策可能仍是 tentative（verified = false）。
///
/// 一旦经 [`ReasoningService::confirm_decision`] 确认，
/// 该决策即被视为最终（verified = true）。
///
/// 标签：`:Decision`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// 唯一决策标识。
    pub decision_id: String,
    /// 描述决策的短标题。
    pub title: String,
    /// 背景上下文——为何需要此决策。
    pub context: String,
    /// 考虑过的备选方案列表（逗号分隔）。
    pub alternatives: String,
    /// 支持该决策的证据。
    pub evidence: String,
    /// 被选中的方案。
    pub choice: String,
    /// 解释为何做出此选择的理由。
    pub rationale: String,
    /// 决策的预期后果。
    pub consequences: String,
    /// 该决策的置信度（0.0–1.0）。
    pub confidence: f64,
    /// 该决策是否已被确认（verified）为最终。
    pub verified: bool,
    /// 做出该决策的会话。
    pub session_id: String,
    /// 该决策所依据的知识节点。
    pub knowledge_id: Option<String>,
    /// 该决策所属的线程。
    pub thread_id: Option<String>,
    /// 决策做出时间（ISO 8601）。
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
// ReasoningChain — 会话的完整推理轨迹
// ---------------------------------------------------------------------------

/// 推理链将一个会话的全部三级（观察、分析、决策）打包在一起，
/// 构成完整的推理轨迹。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningChain {
    /// 会话期间记录的所有观察。
    pub observations: Vec<Observation>,
    /// 会话期间执行的所有分析。
    pub analyses: Vec<Analysis>,
    /// 会话期间做出的所有决策。
    pub decisions: Vec<Decision>,
}

impl ReasoningChain {
    /// 创建空推理链。
    pub fn empty() -> Self {
        Self {
            observations: Vec::new(),
            analyses: Vec::new(),
            decisions: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
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
        let json = serde_json::to_string(&obs).expect("序列化应成功");
        let back: Observation = serde_json::from_str(&json).expect("反序列化应成功");
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
        let json = serde_json::to_string(&analysis).expect("序列化应成功");
        let back: Analysis = serde_json::from_str(&json).expect("反序列化应成功");
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
        let json = serde_json::to_string(&d).expect("序列化应成功");
        let back: Decision = serde_json::from_str(&json).expect("反序列化应成功");
        assert_eq!(back.decision_id, d.decision_id);
        assert!(back.verified);
    }
}
