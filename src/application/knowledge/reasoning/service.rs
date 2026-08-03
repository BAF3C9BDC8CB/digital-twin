//! ReasoningService trait——推理维度的契约。
//!
//! 管理三级推理管线：
//!   Observation(发现) → Analysis(分析) → Decision(决策)
//!
//! trait 通过 [`GraphRepository`] 抽象与任何具体存储后端解耦。
//! 生命周期管理委托给 [`LifecycleManager`]。
//!
//! [`DefaultReasoningService`] 是规范的生产实现。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use async_trait::async_trait;
use std::sync::Arc;

use super::lifecycle::{DefaultLifecycleManager, LifecycleManager};
use super::{Analysis, Decision, Observation, ReasoningChain};

/// 管理推理维度的服务。
///
/// # 典型用法
///
/// ```ignore
/// let svc = DefaultReasoningService::new(graph_repo);
///
/// // 记录一条观察
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
/// // 记录分析结果
/// svc.record_analysis(&Analysis {
///     analysis_id: "an://session-001/001".into(),
///     question: "Why is PayService throwing NPE?".into(),
///     conclusion: "Uninitialized channelExtra field".into(),
///     session_id: "2026-07-09-001".into(),
///     ..
/// }).await?;
///
/// // 记录一条决策
/// svc.record_decision(&Decision {
///     decision_id: "dec://session-001/001".into(),
///     title: "Initialize channelExtra in constructor".into(),
///     choice: "Add null-guard in getChannelExtra()".into(),
///     session_id: "2026-07-09-001".into(),
///     ..
/// }).await?;
///
/// // 确认决策
/// svc.confirm_decision("dec://session-001/001").await?;
///
/// // 会话结束：将所有推理节点标记为过期
/// svc.mark_stale("2026-07-09-001").await?;
///
/// // 获取完整推理链以供回顾
/// let chain = svc.get_reasoning_chain("2026-07-09-001").await?;
/// ```
#[async_trait]
pub trait ReasoningService: Send + Sync {
    /// 在图中的观察节点记录。
    ///
    /// 用给定属性创建 `(:Observation)` 节点。
    async fn record_observation(&self, obs: &Observation) -> Result<(), DtError>;

    /// 在图中的分析节点记录。
    ///
    /// 用给定属性创建 `(:Analysis)` 节点。
    /// 中间步骤序列化为 JSON 字符串。
    async fn record_analysis(&self, analysis: &Analysis) -> Result<(), DtError>;

    /// 在图中的决策节点记录。
    ///
    /// 用给定属性创建 `(:Decision)` 节点，
    /// 可选地关联到 Knowledge 节点与 Thread 节点。
    async fn record_decision(&self, decision: &Decision) -> Result<(), DtError>;

    /// 通过设置 `verified = true` 确认决策。
    ///
    /// 委托给底层 [`LifecycleManager::confirm_decision`]。
    async fn confirm_decision(&self, decision_id: &str) -> Result<(), DtError>;

    /// 将会话的所有推理节点标记为过期。
    ///
    /// 为给定会话的每个 `:Observation`、`:Analysis` 与 `:Decision`
    /// 设置 `_stale_at = timestamp()`。
    /// 过期节点在 Context Builder 查询中被排除。
    ///
    /// 返回被标记的节点数。
    async fn mark_stale(&self, session_id: &str) -> Result<usize, DtError>;

    /// 检索会话的完整推理链。
    ///
    /// 查询属于给定会话的所有 `:Observation`、`:Analysis` 与 `:Decision`
    /// 节点，并打包成 [`ReasoningChain`]。
    async fn get_reasoning_chain(&self, session_id: &str) -> Result<ReasoningChain, DtError>;
}

// ---------------------------------------------------------------------------
// DefaultReasoningService — 规范实现
// ---------------------------------------------------------------------------

/// 由 [`GraphRepository`] 支撑的 [`ReasoningService`] 规范实现。
///
/// # 生命周期
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
    /// 创建由给定图仓库支撑的 [`DefaultReasoningService`]。
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
        // 构建 Decision 节点的基础 Cypher。
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
        // 查询该会话的观察。
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

        // 查询该会话的分析。
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

        // 查询该会话的决策。
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

        // 解析观察
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

        // 解析分析
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

        // 解析决策
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
                        // 上面的决策查询不返回 knowledge_id 和 thread_id
                        //（它们存在于关系上）；此处保持 None。
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
// 测试
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
            .expect("record_observation 应成功");
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
            .expect("record_analysis 应成功");
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
            .expect("record_decision 应成功");
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
            .expect("record_decision 应成功");
        assert!(write.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn confirm_decision_delegates_to_lifecycle() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        // confirm_decision 返回 NotFound，因为 CountingRepo 不返回行；
        // 这是预期行为，验证了委托路径。
        let result = svc.confirm_decision("dec://test/001").await;
        // mock 返回 [{"marked":1}]，但 confirm 需要 [{"decision_id":...}]，
        // 因此会得到 NotFound——测试中可接受——我们只
        // 验证它经由生命周期路径调用了 write_query。
        assert!(write.load(Ordering::SeqCst) >= 1);
        let _ = result; // may be Err(NotFound)
    }

    #[tokio::test]
    async fn mark_stale_returns_count() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let repo = Arc::new(CountingRepo::new(write.clone(), read.clone()));
        let svc = DefaultReasoningService::new(repo);

        let count = svc.mark_stale("2026-07-09-001").await.expect("mark_stale 应成功");
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
            .expect("get_reasoning_chain 应成功");

        // 应已触发 3 次读查询（观察、分析、决策）
        assert!(read.load(Ordering::SeqCst) >= 3);
        // 全部为空，因为 mock 返回 []。
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

        // 记录全部三级
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
        .expect("observation 应成功");

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
        .expect("analysis 应成功");

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
        .expect("decision 应成功");

        // 至少 3 次写（每类一个）
        assert!(write.load(Ordering::SeqCst) >= 3);

        // 获取推理链
        let _chain = svc
            .get_reasoning_chain("pipeline-session")
            .await
            .expect("chain 应成功");
        // mock 对全部返回 []，因此链为空——但读调用确实发生了。
        assert!(read.load(Ordering::SeqCst) >= 3);

        // 将会话标记为过期
        let count = svc.mark_stale("pipeline-session").await.expect("stale 应成功");
        assert!(count >= 1);
    }
}
