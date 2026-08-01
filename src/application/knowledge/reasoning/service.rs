//! ReasoningService trait — contract for the reasoning dimension.
//!
//! Manages the three-tier reasoning pipeline:
//!   Observation(发现) → Analysis(分析) → Decision(决策)
//!
//! The trait is decoupled from any specific storage backend via the
//! [`GraphRepository`] abstraction. Lifecycle management is delegated
//! to [`LifecycleManager`].
//!
//! [`DefaultReasoningService`] is the canonical production implementation.

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use async_trait::async_trait;
use std::sync::Arc;

use super::lifecycle::{DefaultLifecycleManager, LifecycleManager};
use super::{Analysis, Decision, Observation, ReasoningChain};

/// Service for managing the reasoning dimension.
///
/// # Typical usage
///
/// ```ignore
/// let svc = DefaultReasoningService::new(graph_repo);
///
/// // Record an observation
/// svc.record_observation(&Observation {
///     observation_id: "obs://session-001/001".into(),
///     description: "NullPointerException in PayService".into(),
///     evidence: "Stack trace line 142".into(),
///     entities: vec!["dt://entity/test/class/PayService".into()],
///     pattern: Some("null-pointer".into()),
///     confidence: 0.95,
///     session_id: "2026-07-09-001".into(),
///     ..
/// }).await?;
///
/// // Record analysis results
/// svc.record_analysis(&Analysis {
///     analysis_id: "an://session-001/001".into(),
///     question: "Why is PayService throwing NPE?".into(),
///     conclusion: "Uninitialized channelExtra field".into(),
///     session_id: "2026-07-09-001".into(),
///     ..
/// }).await?;
///
/// // Record a decision
/// svc.record_decision(&Decision {
///     decision_id: "dec://session-001/001".into(),
///     title: "Initialize channelExtra in constructor".into(),
///     choice: "Add null-guard in getChannelExtra()".into(),
///     session_id: "2026-07-09-001".into(),
///     ..
/// }).await?;
///
/// // Confirm a decision
/// svc.confirm_decision("dec://session-001/001").await?;
///
/// // End of session: mark all reasoning nodes stale
/// svc.mark_stale("2026-07-09-001").await?;
///
/// // Get the full reasoning chain for review
/// let chain = svc.get_reasoning_chain("2026-07-09-001").await?;
/// ```
#[async_trait]
pub trait ReasoningService: Send + Sync {
    /// Record an observation node in the graph.
    ///
    /// Creates a `(:Observation)` node with the given properties.
    async fn record_observation(&self, obs: &Observation) -> Result<(), DtError>;

    /// Record an analysis node in the graph.
    ///
    /// Creates an `(:Analysis)` node with the given properties.
    /// Intermediate steps are serialised as a JSON string.
    async fn record_analysis(&self, analysis: &Analysis) -> Result<(), DtError>;

    /// Record a decision node in the graph.
    ///
    /// Creates a `(:Decision)` node with the given properties,
    /// optionally linking it to a Knowledge node and a Thread node.
    async fn record_decision(&self, decision: &Decision) -> Result<(), DtError>;

    /// Confirm a decision by setting `verified = true`.
    ///
    /// Delegates to the underlying [`LifecycleManager::confirm_decision`].
    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError>;

    /// Mark all reasoning nodes for a session as stale.
    ///
    /// Sets `_stale_at = timestamp()` on every `:Observation`,
    /// `:Analysis`, and `:Decision` for the given session.
    /// Stale nodes are excluded from Context Builder queries.
    ///
    /// Returns the number of nodes marked.
    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError>;

    /// Retrieve the complete reasoning chain for a session.
    ///
    /// Queries all `:Observation`, `:Analysis`, and `:Decision` nodes
    /// belonging to the given session and bundles them into a
    /// [`ReasoningChain`].
    async fn get_reasoning_chain(&self, session_id: &str) -> Result<ReasoningChain, DtError>;
}

// ---------------------------------------------------------------------------
// DefaultReasoningService — canonical implementation
// ---------------------------------------------------------------------------

/// Canonical implementation of [`ReasoningService`] backed by a
/// [`GraphRepository`].
///
/// # Lifecycle
///
/// ```text
/// record_observation → MERGE (:Observation {observation_id}) SET ...
/// record_analysis    → MERGE (:Analysis {analysis_id}) SET ...
/// record_decision    → MERGE (:Decision {decision_id}) SET ...
///                       optional: MERGE (:Knowledge)-[:BASED_ON]-(:Decision)
///                       optional: MERGE (:Thread)-[:BELONGS_TO]-(:Decision)
/// confirm_decision   → MATCH (:Decision) SET verified = true
/// mark_stale         → MATCH (:Observation|:Analysis|:Decision) SET _stale_at
/// get_reasoning_chain → MATCH all three types, return bundles
/// ```
pub struct DefaultReasoningService {
    graph: Arc<dyn GraphRepository>,
    lifecycle: DefaultLifecycleManager,
}

impl DefaultReasoningService {
    /// Create a new [`DefaultReasoningService`] backed by the given
    /// graph repository.
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self {
            graph: Arc::clone(&graph),
            lifecycle: DefaultLifecycleManager::new(graph),
        }
    }
}

#[async_trait]
impl ReasoningService for DefaultReasoningService {
    async fn record_observation(&self, obs: &Observation) -> Result<(), DtError> {
        let cypher = r#"
            MERGE (o:Observation {observation_id: $observation_id})
            ON CREATE SET
                o.description = $description,
                o.evidence    = $evidence,
                o.entities    = $entities,
                o.pattern     = $pattern,
                o.confidence  = $confidence,
                o.session_id  = $session_id,
                o.timestamp   = $timestamp
            ON MATCH SET
                o.description = $description,
                o.evidence    = $evidence,
                o.entities    = $entities,
                o.pattern     = $pattern,
                o.confidence  = $confidence
        "#;

        let entities_json =
            serde_json::to_string(&obs.entities).unwrap_or_else(|_| "[]".to_string());

        let mut params = std::collections::HashMap::new();
        params.insert(
            "observation_id".into(),
            serde_json::Value::String(obs.observation_id.clone()),
        );
        params.insert(
            "description".into(),
            serde_json::Value::String(obs.description.clone()),
        );
        params.insert(
            "evidence".into(),
            serde_json::Value::String(obs.evidence.clone()),
        );
        params.insert("entities".into(), serde_json::Value::String(entities_json));
        params.insert(
            "pattern".into(),
            obs.pattern
                .as_ref()
                .map(|p| serde_json::Value::String(p.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        params.insert("confidence".into(), serde_json::json!(obs.confidence));
        params.insert(
            "session_id".into(),
            serde_json::Value::String(obs.session_id.clone()),
        );
        params.insert(
            "timestamp".into(),
            serde_json::Value::String(obs.timestamp.clone()),
        );

        self.graph.write_query(cypher, params).await?;
        Ok(())
    }

    async fn record_analysis(&self, analysis: &Analysis) -> Result<(), DtError> {
        let steps_json =
            serde_json::to_string(&analysis.intermediate).unwrap_or_else(|_| "[]".to_string());

        let cypher = r#"
            MERGE (a:Analysis {analysis_id: $analysis_id})
            ON CREATE SET
                a.question       = $question,
                a.hypothesis     = $hypothesis,
                a.method         = $method,
                a.intermediate   = $intermediate,
                a.conclusion     = $conclusion,
                a.confidence     = $confidence,
                a.total_cost_ms  = $total_cost_ms,
                a.session_id     = $session_id,
                a.timestamp      = $timestamp
            ON MATCH SET
                a.question       = $question,
                a.hypothesis     = $hypothesis,
                a.conclusion     = $conclusion,
                a.confidence     = $confidence,
                a.total_cost_ms  = $total_cost_ms
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "analysis_id".into(),
            serde_json::Value::String(analysis.analysis_id.clone()),
        );
        params.insert(
            "question".into(),
            serde_json::Value::String(analysis.question.clone()),
        );
        params.insert(
            "hypothesis".into(),
            analysis
                .hypothesis
                .as_ref()
                .map(|h| serde_json::Value::String(h.clone()))
                .unwrap_or(serde_json::Value::Null),
        );
        params.insert(
            "method".into(),
            serde_json::Value::String(analysis.method.clone()),
        );
        params.insert("intermediate".into(), serde_json::Value::String(steps_json));
        params.insert(
            "conclusion".into(),
            serde_json::Value::String(analysis.conclusion.clone()),
        );
        params.insert("confidence".into(), serde_json::json!(analysis.confidence));
        params.insert(
            "total_cost_ms".into(),
            analysis
                .total_cost_ms
                .map(|c| serde_json::json!(c))
                .unwrap_or(serde_json::Value::Null),
        );
        params.insert(
            "session_id".into(),
            serde_json::Value::String(analysis.session_id.clone()),
        );
        params.insert(
            "timestamp".into(),
            serde_json::Value::String(analysis.timestamp.clone()),
        );

        self.graph.write_query(cypher, params).await?;
        Ok(())
    }

    async fn record_decision(&self, decision: &Decision) -> Result<(), DtError> {
        // Build the base Cypher for the Decision node.
        let cypher = if decision.knowledge_id.is_some() && decision.thread_id.is_some() {
            r#"
                MERGE (d:Decision {decision_id: $decision_id})
                SET d.title        = $title,
                    d.context      = $context,
                    d.alternatives = $alternatives,
                    d.evidence     = $evidence,
                    d.choice       = $choice,
                    d.rationale    = $rationale,
                    d.consequences = $consequences,
                    d.confidence   = $confidence,
                    d.verified     = $verified,
                    d.session_id   = $session_id,
                    d.timestamp    = $timestamp
                WITH d
                MERGE (k:Knowledge {knowledge_id: $knowledge_id})
                MERGE (d)-[:BASED_ON]->(k)
                WITH d
                MERGE (t:Thread {thread_id: $thread_id})
                MERGE (d)-[:BELONGS_TO]->(t)
            "#
        } else if decision.knowledge_id.is_some() {
            r#"
                MERGE (d:Decision {decision_id: $decision_id})
                SET d.title        = $title,
                    d.context      = $context,
                    d.alternatives = $alternatives,
                    d.evidence     = $evidence,
                    d.choice       = $choice,
                    d.rationale    = $rationale,
                    d.consequences = $consequences,
                    d.confidence   = $confidence,
                    d.verified     = $verified,
                    d.session_id   = $session_id,
                    d.timestamp    = $timestamp
                WITH d
                MERGE (k:Knowledge {knowledge_id: $knowledge_id})
                MERGE (d)-[:BASED_ON]->(k)
            "#
        } else if decision.thread_id.is_some() {
            r#"
                MERGE (d:Decision {decision_id: $decision_id})
                SET d.title        = $title,
                    d.context      = $context,
                    d.alternatives = $alternatives,
                    d.evidence     = $evidence,
                    d.choice       = $choice,
                    d.rationale    = $rationale,
                    d.consequences = $consequences,
                    d.confidence   = $confidence,
                    d.verified     = $verified,
                    d.session_id   = $session_id,
                    d.timestamp    = $timestamp
                WITH d
                MERGE (t:Thread {thread_id: $thread_id})
                MERGE (d)-[:BELONGS_TO]->(t)
            "#
        } else {
            r#"
                MERGE (d:Decision {decision_id: $decision_id})
                SET d.title        = $title,
                    d.context      = $context,
                    d.alternatives = $alternatives,
                    d.evidence     = $evidence,
                    d.choice       = $choice,
                    d.rationale    = $rationale,
                    d.consequences = $consequences,
                    d.confidence   = $confidence,
                    d.verified     = $verified,
                    d.session_id   = $session_id,
                    d.timestamp    = $timestamp
            "#
        };

        let mut params = std::collections::HashMap::new();
        params.insert(
            "decision_id".into(),
            serde_json::Value::String(decision.decision_id.clone()),
        );
        params.insert(
            "title".into(),
            serde_json::Value::String(decision.title.clone()),
        );
        params.insert(
            "context".into(),
            serde_json::Value::String(decision.context.clone()),
        );
        params.insert(
            "alternatives".into(),
            serde_json::Value::String(decision.alternatives.clone()),
        );
        params.insert(
            "evidence".into(),
            serde_json::Value::String(decision.evidence.clone()),
        );
        params.insert(
            "choice".into(),
            serde_json::Value::String(decision.choice.clone()),
        );
        params.insert(
            "rationale".into(),
            serde_json::Value::String(decision.rationale.clone()),
        );
        params.insert(
            "consequences".into(),
            serde_json::Value::String(decision.consequences.clone()),
        );
        params.insert("confidence".into(), serde_json::json!(decision.confidence));
        params.insert(
            "verified".into(),
            serde_json::Value::Bool(decision.verified),
        );
        params.insert(
            "session_id".into(),
            serde_json::Value::String(decision.session_id.clone()),
        );
        params.insert(
            "timestamp".into(),
            serde_json::Value::String(decision.timestamp.clone()),
        );
        if let Some(ref kid) = decision.knowledge_id {
            params.insert(
                "knowledge_id".into(),
                serde_json::Value::String(kid.clone()),
            );
        }
        if let Some(ref tid) = decision.thread_id {
            params.insert("thread_id".into(), serde_json::Value::String(tid.clone()));
        }

        self.graph.write_query(cypher, params).await?;
        Ok(())
    }

    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError> {
        self.lifecycle.confirm_decision(decision_id).await
    }

    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError> {
        self.lifecycle.mark_stale(session_id).await
    }

    async fn get_reasoning_chain(&self, session_id: &str) -> Result<ReasoningChain, DtError> {
        // Query observations for this session.
        let obs_cypher = r#"
            MATCH (o:Observation {session_id: $session_id})
            RETURN o.observation_id AS observation_id,
                   o.description AS description,
                   o.evidence AS evidence,
                   o.entities AS entities,
                   o.pattern AS pattern,
                   o.confidence AS confidence,
                   o.session_id AS session_id,
                   o.timestamp AS timestamp
            ORDER BY o.timestamp
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "session_id".into(),
            serde_json::Value::String(session_id.to_string()),
        );

        let obs_result = self.graph.read_query(obs_cypher, params.clone()).await?;

        // Query analyses for this session.
        let ana_cypher = r#"
            MATCH (a:Analysis {session_id: $session_id})
            RETURN a.analysis_id AS analysis_id,
                   a.question AS question,
                   a.hypothesis AS hypothesis,
                   a.method AS method,
                   a.intermediate AS intermediate,
                   a.conclusion AS conclusion,
                   a.confidence AS confidence,
                   a.total_cost_ms AS total_cost_ms,
                   a.session_id AS session_id,
                   a.timestamp AS timestamp
            ORDER BY a.timestamp
        "#;

        let ana_result = self.graph.read_query(ana_cypher, params.clone()).await?;

        // Query decisions for this session.
        let dec_cypher = r#"
            MATCH (d:Decision {session_id: $session_id})
            RETURN d.decision_id AS decision_id,
                   d.title AS title,
                   d.context AS context,
                   d.alternatives AS alternatives,
                   d.evidence AS evidence,
                   d.choice AS choice,
                   d.rationale AS rationale,
                   d.consequences AS consequences,
                   d.confidence AS confidence,
                   d.verified AS verified,
                   d.session_id AS session_id,
                   d.timestamp AS timestamp
            ORDER BY d.timestamp
        "#;

        let dec_result = self.graph.read_query(dec_cypher, params).await?;

        // Parse observations
        let observations: Vec<Observation> = obs_result
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| Observation {
                        observation_id: row
                            .get("observation_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        description: row
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        evidence: row
                            .get("evidence")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        entities: row
                            .get("entities")
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default(),
                        pattern: row
                            .get("pattern")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        confidence: row
                            .get("confidence")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                        session_id: row
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        timestamp: row
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse analyses
        let analyses: Vec<Analysis> = ana_result
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| Analysis {
                        analysis_id: row
                            .get("analysis_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        question: row
                            .get("question")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        hypothesis: row
                            .get("hypothesis")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        method: row
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        intermediate: row
                            .get("intermediate")
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or_default(),
                        conclusion: row
                            .get("conclusion")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        confidence: row
                            .get("confidence")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                        total_cost_ms: row.get("total_cost_ms").and_then(|v| v.as_u64()),
                        session_id: row
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        timestamp: row
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse decisions
        let decisions: Vec<Decision> = dec_result
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| Decision {
                        decision_id: row
                            .get("decision_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        title: row
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        context: row
                            .get("context")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        alternatives: row
                            .get("alternatives")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        evidence: row
                            .get("evidence")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        choice: row
                            .get("choice")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        rationale: row
                            .get("rationale")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        consequences: row
                            .get("consequences")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        confidence: row
                            .get("confidence")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                        verified: row
                            .get("verified")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        session_id: row
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        // knowledge_id and thread_id are not returned by
                        // the decision query above (they live on relationships);
                        // we leave them None.
                        knowledge_id: None,
                        thread_id: None,
                        timestamp: row
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(ReasoningChain {
            observations,
            analyses,
            decisions,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::Observation;
    use super::*;
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    impl CountingRepo {
        fn new(write_count: Arc<AtomicUsize>, read_count: Arc<AtomicUsize>) -> Self {
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
            Ok(serde_json::json!([]))
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!([{"marked": 1u64}]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn ReasoningService) {}
    }

    #[test]
    fn trait_method_signatures_exist() {
        fn _assert_methods<T: ReasoningService>() {}
    }

    #[tokio::test]
    async fn record_observation_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let obs = Observation {
            observation_id: "obs://test/001".into(),
            description: "Test observation".into(),
            evidence: "Test evidence".into(),
            entities: vec!["e-1".into()],
            pattern: Some("test-pattern".into()),
            confidence: 0.9,
            session_id: "2026-07-09-001".into(),
            timestamp: "2026-07-09T10:00:00Z".into(),
        };

        svc.record_observation(&obs)
            .await
            .expect("record_observation");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn record_analysis_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let analysis = Analysis {
            analysis_id: "an://test/001".into(),
            question: "Why?".into(),
            hypothesis: Some("Maybe".into()),
            method: "root-cause".into(),
            intermediate: vec![],
            conclusion: "Because".into(),
            confidence: 0.8,
            total_cost_ms: Some(100),
            session_id: "2026-07-09-001".into(),
            timestamp: "2026-07-09T10:30:00Z".into(),
        };

        svc.record_analysis(&analysis)
            .await
            .expect("record_analysis");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn record_decision_calls_write_query() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let decision = Decision {
            decision_id: "dec://test/001".into(),
            title: "Use Redis".into(),
            context: "Need caching".into(),
            alternatives: "Redis,Memcached".into(),
            evidence: "Team uses Redis".into(),
            choice: "Redis".into(),
            rationale: "Best fit".into(),
            consequences: "Need cluster".into(),
            confidence: 0.85,
            verified: false,
            session_id: "2026-07-09-001".into(),
            knowledge_id: Some("dt://knowledge/test/reasoning".into()),
            thread_id: Some("dt://thread/test/default".into()),
            timestamp: "2026-07-09T11:00:00Z".into(),
        };

        svc.record_decision(&decision)
            .await
            .expect("record_decision");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn record_decision_no_relations() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let decision = Decision {
            decision_id: "dec://test/002".into(),
            title: "Simple decision".into(),
            context: "".into(),
            alternatives: "".into(),
            evidence: "".into(),
            choice: "Do it".into(),
            rationale: "".into(),
            consequences: "".into(),
            confidence: 0.5,
            verified: false,
            session_id: "2026-07-09-001".into(),
            knowledge_id: None,
            thread_id: None,
            timestamp: "2026-07-09T11:00:00Z".into(),
        };

        svc.record_decision(&decision)
            .await
            .expect("record_decision");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn confirm_decision_delegates_to_lifecycle() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        // confirm_decision returns NotFound because CountingRepo doesn't
        // return rows; that's expected and validates the delegation path.
        let result = svc.confirm_decision("dec://test/001").await;
        // The mock returns [{"marked":1}] but confirm needs [{"decision_id":...}]
        // So this will be NotFound, which is fine for the test — we just
        // verify it calls write_query via the lifecycle path.
        assert!(write.load(Ordering::SeqCst) >= 1);
        let _ = result; // may be Err(NotFound)
    }

    #[tokio::test]
    async fn mark_stale_returns_count() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let count = svc.mark_stale("2026-07-09-001").await.expect("mark_stale");
        assert!(write.load(Ordering::SeqCst) >= 1);
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn get_reasoning_chain_queries_all_three_types() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let chain = svc
            .get_reasoning_chain("2026-07-09-001")
            .await
            .expect("get_reasoning_chain");

        // Should have triggered 3 read queries (obs, analysis, decision)
        assert!(read.load(Ordering::SeqCst) >= 3);
        // All empty because the mock returns [].
        assert!(chain.observations.is_empty());
        assert!(chain.analyses.is_empty());
        assert!(chain.decisions.is_empty());
    }

    #[tokio::test]
    async fn full_reasoning_pipeline() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        // Record all three tiers
        svc.record_observation(&Observation {
            observation_id: "obs://pipeline/001".into(),
            description: "NPE in PayService".into(),
            evidence: "Stack line 142".into(),
            entities: vec!["PayService".into()],
            pattern: Some("null-pointer".into()),
            confidence: 0.95,
            session_id: "pipeline-session".into(),
            timestamp: "2026-07-09T10:00:00Z".into(),
        })
        .await
        .expect("observation");

        svc.record_analysis(&Analysis {
            analysis_id: "an://pipeline/001".into(),
            question: "Why NPE?".into(),
            hypothesis: Some("Uninitialized field".into()),
            method: "root-cause".into(),
            intermediate: vec![],
            conclusion: "Add null guard".into(),
            confidence: 0.8,
            total_cost_ms: Some(500),
            session_id: "pipeline-session".into(),
            timestamp: "2026-07-09T10:30:00Z".into(),
        })
        .await
        .expect("analysis");

        svc.record_decision(&Decision {
            decision_id: "dec://pipeline/001".into(),
            title: "Add null guard".into(),
            context: "NPE fix".into(),
            alternatives: "null-guard,Optional".into(),
            evidence: "Stack trace".into(),
            choice: "null-guard".into(),
            rationale: "Simpler".into(),
            consequences: "Minimal".into(),
            confidence: 0.9,
            verified: false,
            session_id: "pipeline-session".into(),
            knowledge_id: None,
            thread_id: None,
            timestamp: "2026-07-09T11:00:00Z".into(),
        })
        .await
        .expect("decision");

        // At least 3 writes (one per entity)
        assert!(write.load(Ordering::SeqCst) >= 3);

        // Get the reasoning chain
        let _chain = svc
            .get_reasoning_chain("pipeline-session")
            .await
            .expect("chain");
        // Mock returns [] for all, so chain is empty — but the read calls happened.
        assert!(read.load(Ordering::SeqCst) >= 3);

        // Mark session stale
        let count = svc.mark_stale("pipeline-session").await.expect("stale");
        assert!(count >= 1);
    }
}
