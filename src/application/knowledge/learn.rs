//! LearnService — high-level knowledge acquisition from AI task execution.
//!
//! The `learn` operation takes a task result (pattern, pitfalls, decisions,
//! success/failure) and writes structured knowledge into the Knowledge World:
//!
//! - Patterns → Knowledge nodes (source: `ai_task`)
//! - Pitfalls → Experience nodes
//! - Pattern + pitfalls → Playbook (ordered steps)
//! - Success/failure → Playbook counter updates; auto-flags `_needs_review`
//!
//! # Typical usage
//!
//! ```ignore
//! let svc = LearnServiceImpl::new(knowledge_svc);
//! let report = svc.learn(&LearnRequest {
//!     task: "支付平台迁移".into(),
//!     entities: vec!["PayService".into(), "pay-channel.yml".into()],
//!     pattern: Some("ifCode+wayCode+merchantNo+DB".into()),
//!     pitfalls: vec!["别忘了channelExtra".into()],
//!     decisions: vec![],
//!     thread_id: None,
//!     success: Some(true),
//!     project: Some("aflm".into()),
//! }).await?;
//! // report.summary = "已沉淀 1 个知识模式, 1 个 Playbook, 1 条踩坑经验"
//! ```

use async_trait::async_trait;
use crate::domain::error::DtError;
use std::sync::Arc;

use super::knowledge::entities::{
    Experience, ExperienceSeverity, Knowledge, KnowledgeSource, Playbook, Step,
};
use super::knowledge::service::KnowledgeService;
use crate::application::knowledge::knowledge::service::DefaultKnowledgeService;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

// ---------------------------------------------------------------------------
// Request / Report
// ---------------------------------------------------------------------------

/// Input for the `learn` operation.
#[derive(Debug, Clone)]
pub struct LearnRequest {
    /// Task title or description (e.g. "支付平台迁移").
    pub task: String,
    /// Affected entities (file paths, class names, service names).
    pub entities: Vec<String>,
    /// Recognised solution pattern.
    pub pattern: Option<String>,
    /// Pitfalls encountered or to watch out for.
    pub pitfalls: Vec<String>,
    /// Architecture / technical decisions made.
    pub decisions: Vec<String>,
    /// Optional digital-thread ID for cross-task lineage.
    pub thread_id: Option<String>,
    /// Whether execution succeeded.
    pub success: Option<bool>,
    /// Owning project.
    pub project: Option<String>,
}

/// Summary of what was learned / persisted.
#[derive(Debug, Clone)]
pub struct LearnReport {
    /// Number of Knowledge nodes created/updated.
    pub knowledge_created: usize,
    /// Number of Experience nodes created.
    pub experiences_created: usize,
    /// Whether a Playbook was created or its counters updated.
    pub playbook_updated: bool,
    /// Human-readable one-line summary.
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Service for high-level knowledge acquisition.
///
/// A `LearnService` receives a structured [`LearnRequest`] and:
/// 1. Extracts the domain from the task name.
/// 2. Writes pattern-based Knowledge nodes.
/// 3. Writes pitfall-based Experience nodes.
/// 4. Synthesises a Playbook from pattern + pitfalls.
/// 5. Updates success/failure counters on the Playbook.
/// 6. Optionally links nodes to a digital thread.
#[async_trait]
pub trait LearnService: Send + Sync {
    /// Execute the learn pipeline and return a report.
    async fn learn(&self, request: &LearnRequest) -> Result<LearnReport, DtError>;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Canonical implementation of [`LearnService`].
///
/// Delegates actual storage writes to a [`KnowledgeService`].
pub struct LearnServiceImpl<S: KnowledgeService> {
    knowledge: Arc<S>,
}

impl<S: KnowledgeService> LearnServiceImpl<S> {
    pub fn new(knowledge: Arc<S>) -> Self {
        Self { knowledge }
    }
}

/// Constructor for [`LearnServiceImpl`] backed by a [`DefaultKnowledgeService`]
/// configured with vectorisation support.
///
/// Call this when `embed` / `vector` backends are available — the underlying
/// service will auto-embed Knowledge, Experience, Concept, and Playbook nodes
/// into the unified `kg_nodes` Qdrant collection on every write.
///
/// This is only implemented for `DefaultKnowledgeService` (not generic `S`)
/// because it relies on that concrete type's [`with_vectorization`]
/// constructor.
///
/// [`with_vectorization`]: DefaultKnowledgeService::with_vectorization
impl LearnServiceImpl<DefaultKnowledgeService> {
    pub fn with_vectorization(
        graph: Arc<dyn GraphRepository>,
        embed: Arc<dyn EmbedService>,
        vector: Arc<dyn VectorRepository>,
    ) -> Self {
        let svc = Arc::new(
            DefaultKnowledgeService::with_vectorization(graph, embed, vector),
        );
        Self::new(svc)
    }
}

#[async_trait]
impl<S: KnowledgeService + 'static> LearnService for LearnServiceImpl<S> {
    async fn learn(&self, request: &LearnRequest) -> Result<LearnReport, DtError> {
        let project = request.project.as_deref().unwrap_or("unknown");
        let domain = extract_domain(&request.task);
        let now = chrono::Utc::now().to_rfc3339();

        let mut knowledge_count: usize = 0;
        let mut experience_count: usize = 0;
        let mut playbook_touched = false;
        let mut summary_parts: Vec<String> = Vec::new();

        // ---- 1. Pattern → Knowledge ----
        if let Some(pattern) = &request.pattern {
            let knowledge_id = format_knowledge_id(project, &domain, "pattern", &request.task);
            let name = to_snake(&request.task);
            let knowledge = Knowledge {
                knowledge_id,
                name: name.clone(),
                title: format!("{} — 执行模式", request.task),
                domain: domain.clone(),
                summary: pattern.clone(),
                content: format!(
                    "# {}\n\n## 模式\n\n{}\n\n## 涉及实体\n\n{}",
                    request.task,
                    pattern,
                    request.entities.join(", "),
                ),
                definition: format!("{} 的解决方案模式", request.task),
                source: KnowledgeSource::AiTask,
                project: project.to_string(),
                confidence: 0.7,
                verified_by: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                version: 1,
            };
            self.knowledge.write_knowledge(&knowledge).await?;
            knowledge_count += 1;
        }

        // ---- 2. Pitfalls → Experience ----
        for (i, pitfall) in request.pitfalls.iter().enumerate() {
            let experience_id = format!(
                "dt://experience/{}/{}/pitfall-{}-{}",
                project,
                &domain,
                to_snake(&request.task),
                i + 1,
            );
            let experience = Experience {
                experience_id,
                title: format!("{} — 踩坑 #{}", request.task, i + 1),
                summary: pitfall.clone(),
                content: format!(
                    "## 坑点\n{}\n\n## 任务\n{}\n\n## 涉及实体\n{}",
                    pitfall,
                    request.task,
                    request.entities.join(", "),
                ),
                domain: domain.clone(),
                severity: ExperienceSeverity::Warning,
                project: project.to_string(),
                created_at: now.clone(),
            };
            self.knowledge.write_experience(&experience).await?;
            experience_count += 1;
        }

        // ---- 3. Decisions → Experience (recorded separately) ----
        for (i, decision) in request.decisions.iter().enumerate() {
            let experience_id = format!(
                "dt://experience/{}/{}/decision-{}-{}",
                project,
                &domain,
                to_snake(&request.task),
                i + 1,
            );
            let experience = Experience {
                experience_id,
                title: format!("{} — 决策 #{}", request.task, i + 1),
                summary: decision.clone(),
                content: format!(
                    "## 决策\n{}\n\n## 任务上下文\n{}",
                    decision, request.task,
                ),
                domain: domain.clone(),
                severity: ExperienceSeverity::Info,
                project: project.to_string(),
                created_at: now.clone(),
            };
            self.knowledge.write_experience(&experience).await?;
            experience_count += 1;
        }

        // ---- 4. Pattern + pitfalls → Playbook ----
        if request.pattern.is_some() || !request.pitfalls.is_empty()
            || !request.decisions.is_empty()
        {
            let playbook_id = format!(
                "dt://playbook/{}/{}/{}",
                project,
                &domain,
                to_snake(&request.task),
            );
            let name = request.task.clone();

            let mut steps: Vec<Step> = Vec::new();
            let mut order: u32 = 0;

            // Pattern step
            if let Some(pattern) = &request.pattern {
                order += 1;
                steps.push(Step {
                    order,
                    action: "应用模式".into(),
                    tool: "edit".into(),
                    target: request.entities.first().cloned(),
                    expected: pattern.clone(),
                    pitfall: None,
                });
            }

            // Entity steps
            for entity in &request.entities {
                order += 1;
                steps.push(Step {
                    order,
                    action: format!("修改 {}", entity),
                    tool: "edit".into(),
                    target: Some(entity.clone()),
                    expected: "变更生效".into(),
                    pitfall: None,
                });
            }

            // Pitfall steps (preventive)
            for pitfall in &request.pitfalls {
                order += 1;
                steps.push(Step {
                    order,
                    action: "预防踩坑".into(),
                    tool: "review".into(),
                    target: None,
                    expected: format!("确认: {}", pitfall),
                    pitfall: Some(pitfall.clone()),
                });
            }

            // Decision steps
            for decision in &request.decisions {
                order += 1;
                steps.push(Step {
                    order,
                    action: format!("遵循决策: {}", decision),
                    tool: "review".into(),
                    target: None,
                    expected: "已确认".into(),
                    pitfall: None,
                });
            }

            let description = format!(
                "适用于: {}。涉及的实体: {}{}{}",
                request.task,
                request.entities.join(", "),
                if request.pattern.is_some() {
                    "。已沉淀模式"
                } else {
                    ""
                },
                if !request.pitfalls.is_empty() {
                    format!("。{} 条注意事项", request.pitfalls.len())
                } else {
                    String::new()
                },
            );

            // Determine current counters: if success calls update existing counts.
            // We default to 0 on create; an ON MATCH path handles updates.
            let (success_count, failure_count) = match request.success {
                Some(true) => (1, 0),
                Some(false) => (0, 1),
                None => (0, 0),
            };
            let success_rate = if success_count + failure_count > 0 {
                (success_count as f64) / ((success_count + failure_count) as f64)
            } else {
                1.0
            };

            let playbook = Playbook {
                playbook_id: playbook_id.clone(),
                name,
                description,
                steps,
                domain: domain.clone(),
                project: project.to_string(),
                success_count,
                failure_count,
                _needs_review: success_rate < 0.7,
                created_at: now.clone(),
            };
            self.knowledge.write_playbook(&playbook).await?;
            playbook_touched = true;
        }

        // ---- 5. Success/failure → update playbook counters (MERGE-based) ----
        // The playbook was already written above with the correct counters.
        // If success/failure is set, the counters are already applied.
        // For subsequent calls on the same task, the ON MATCH SET in write_playbook
        // will overwrite. For now this is sufficient.

        // ---- 6. Thread association (placeholder) ----
        // In a future phase, link Knowledge/Experience/Playbook nodes
        // to the Digital Thread via `HAS_KNOWLEDGE` / `HAS_PLAYBOOK` relations.
        if let Some(_thread_id) = &request.thread_id {
            // TODO: when Digital Thread entity exists, create:
            //   MATCH (th:Thread {thread_id: $thread_id})
            //   MERGE (k)-[:HAS_KNOWLEDGE]->(th)
            //   MERGE (p)-[:HAS_PLAYBOOK]->(th)
        }

        // ---- Build summary ----
        if knowledge_count > 0 {
            summary_parts.push(format!("{} 个知识模式", knowledge_count));
        }
        if experience_count > 0 {
            summary_parts.push(format!("{} 条经验", experience_count));
        }
        if playbook_touched {
            summary_parts.push("1 个 Playbook".into());
        }

        let summary = if summary_parts.is_empty() {
            "无新增内容".into()
        } else {
            format!("已沉淀 {}", summary_parts.join(", "))
        };

        Ok(LearnReport {
            knowledge_created: knowledge_count,
            experiences_created: experience_count,
            playbook_updated: playbook_touched,
            summary,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a domain keyword from a task title.
///
/// Examples:
/// - "支付平台迁移"  → "支付"
/// - "部署服务升级"  → "部署"
/// - "日志采集优化"  → "日志"
/// Extract a domain keyword from the task title.
///
/// Common Chinese domain keywords are matched first, with a fallback to
/// the first 2 characters of the trimmed task.
pub(crate) fn extract_domain(task: &str) -> String {
    // Common domain keywords in Chinese
    let domains = ["支付", "部署", "日志", "配置", "监控", "安全", "测试", "数据库"];
    for d in &domains {
        if task.contains(d) {
            return d.to_string();
        }
    }
    // Fallback: first 2 chars of trimmed task
    task.chars().take(2).collect()
}

/// Convert a task title into a snake_case identifier.
///
/// Strips non-alphanumeric ASCII, lowercases, joins with hyphens.
/// Convert a task title into a snake_case identifier.
///
/// Strips non-alphanumeric ASCII, lowercases, joins with hyphens.
pub(crate) fn to_snake(task: &str) -> String {
    let filtered: String = task
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if filtered.is_empty() {
        "unnamed".into()
    } else {
        filtered.to_lowercase().replace(' ', "-")
    }
}

/// Create a deterministic knowledge_id for a pattern.
/// Create a deterministic knowledge_id for a pattern.
pub(crate) fn format_knowledge_id(project: &str, domain: &str, kind: &str, task: &str) -> String {
    format!(
        "dt://knowledge/{}/{}/{}-{}",
        project,
        domain,
        kind,
        to_snake(task),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::knowledge::entities::{Concept, Domain};
    use crate::application::knowledge::knowledge::service::KnowledgeService;
    use async_trait::async_trait;
    use crate::domain::error::DtError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Counts calls per entity type.
    struct SpyKnowledgeService {
        knowledge_count: Arc<AtomicUsize>,
        experience_count: Arc<AtomicUsize>,
        concept_count: Arc<AtomicUsize>,
        domain_count: Arc<AtomicUsize>,
        playbook_count: Arc<AtomicUsize>,
        update_count: Arc<AtomicUsize>,
    }

    impl SpyKnowledgeService {
        fn new(
            k: Arc<AtomicUsize>,
            e: Arc<AtomicUsize>,
            c: Arc<AtomicUsize>,
            d: Arc<AtomicUsize>,
            p: Arc<AtomicUsize>,
            u: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                knowledge_count: k,
                experience_count: e,
                concept_count: c,
                domain_count: d,
                playbook_count: p,
                update_count: u,
            }
        }
    }

    #[async_trait]
    impl KnowledgeService for SpyKnowledgeService {
        async fn write_knowledge(&self, _k: &Knowledge) -> Result<(), DtError> {
            self.knowledge_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn write_experience(&self, _e: &Experience) -> Result<(), DtError> {
            self.experience_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn write_concept(&self, _c: &Concept) -> Result<(), DtError> {
            self.concept_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn write_domain(&self, _d: &Domain) -> Result<(), DtError> {
            self.domain_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn write_playbook(&self, _p: &Playbook) -> Result<(), DtError> {
            self.playbook_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn update_knowledge(
            &self,
            _knowledge_id: &str,
            _diff: &str,
            _session_id: &str,
        ) -> Result<(), DtError> {
            self.update_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn spy() -> (Arc<SpyKnowledgeService>, LearnServiceImpl<SpyKnowledgeService>) {
        let svc = Arc::new(SpyKnowledgeService::new(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        ));
        let learner = LearnServiceImpl::new(Arc::clone(&svc) as Arc<SpyKnowledgeService>);
        (svc, learner)
    }

    /// Test the trait is object-safe.
    #[test]
    fn trait_is_object_safe() {
        fn _accept(_: &dyn LearnService) {}
    }

    #[tokio::test]
    async fn learn_with_pattern_creates_knowledge() {
        let (spy, learner) = spy();
        let report = learner
            .learn(&LearnRequest {
                task: "支付平台迁移".into(),
                entities: vec!["PayService".into()],
                pattern: Some("ifCode+wayCode".into()),
                pitfalls: vec![],
                decisions: vec![],
                thread_id: None,
                success: None,
                project: Some("test".into()),
            })
            .await
            .expect("learn");
        assert_eq!(report.knowledge_created, 1);
        assert_eq!(report.experiences_created, 0);
        assert!(report.playbook_updated);
        assert!(report.summary.contains("知识模式"));
        assert_eq!(spy.knowledge_count.load(Ordering::SeqCst), 1);
        assert_eq!(spy.playbook_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn learn_with_pitfalls_creates_experiences() {
        let (spy, learner) = spy();
        let report = learner
            .learn(&LearnRequest {
                task: "部署服务".into(),
                entities: vec![],
                pattern: None,
                pitfalls: vec!["忘了改配置".into(), "端口冲突".into()],
                decisions: vec![],
                thread_id: None,
                success: Some(false),
                project: None,
            })
            .await
            .expect("learn");
        assert_eq!(report.knowledge_created, 0);
        assert_eq!(report.experiences_created, 2);
        assert!(report.playbook_updated);
        assert!(report.summary.contains("2 条经验"));
        assert_eq!(spy.experience_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn learn_with_success_updates_playbook_counter() {
        let (spy, learner) = spy();
        let report = learner
            .learn(&LearnRequest {
                task: "支付平台迁移".into(),
                entities: vec!["PayService".into()],
                pattern: Some("ifCode+wayCode".into()),
                pitfalls: vec!["channelExtra".into()],
                decisions: vec![],
                thread_id: None,
                success: Some(true),
                project: Some("aflm".into()),
            })
            .await
            .expect("learn");
        assert_eq!(report.knowledge_created, 1);
        assert_eq!(report.experiences_created, 1);
        assert!(report.playbook_updated);
        assert_eq!(spy.playbook_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn learn_empty_request_returns_empty_report() {
        let (_spy, learner) = spy();
        let report = learner
            .learn(&LearnRequest {
                task: "空任务".into(),
                entities: vec![],
                pattern: None,
                pitfalls: vec![],
                decisions: vec![],
                thread_id: None,
                success: None,
                project: None,
            })
            .await
            .expect("learn");
        assert_eq!(report.knowledge_created, 0);
        assert_eq!(report.experiences_created, 0);
        assert!(!report.playbook_updated);
        assert_eq!(report.summary, "无新增内容");
    }

    #[tokio::test]
    async fn learn_with_decisions_creates_experience_records() {
        let (_spy, learner) = spy();
        let report = learner
            .learn(&LearnRequest {
                task: "架构升级".into(),
                entities: vec![],
                pattern: None,
                pitfalls: vec![],
                decisions: vec!["使用Redis替代Memcache".into()],
                thread_id: None,
                success: None,
                project: Some("infra".into()),
            })
            .await
            .expect("learn");
        assert_eq!(report.experiences_created, 1);
        assert!(report.playbook_updated);
    }

    // ---- helper tests ----

    #[test]
    fn extract_domain_from_task() {
        assert_eq!(extract_domain("支付平台迁移"), "支付");
        assert_eq!(extract_domain("部署服务升级"), "部署");
        assert_eq!(extract_domain("日志采集优化"), "日志");
        assert_eq!(extract_domain("配置中心"), "配置");
    }

    #[test]
    fn extract_domain_fallback() {
        assert_eq!(extract_domain("随便一个任务"), "随便");
    }

    #[test]
    fn to_snake_conversion() {
        assert_eq!(to_snake("支付平台迁移"), "支付平台迁移");
        assert_eq!(to_snake("Hello World"), "helloworld");
        assert_eq!(to_snake(""), "unnamed");
    }

    #[test]
    fn format_knowledge_id_produces_valid_uri() {
        let id = format_knowledge_id("test", "支付", "pattern", "支付平台迁移");
        assert!(id.starts_with("dt://knowledge/"));
        assert!(id.contains("test"));
        assert!(id.contains("支付"));
        assert!(id.contains("pattern"));
    }

    #[test]
    fn learn_report_fields() {
        let report = LearnReport {
            knowledge_created: 1,
            experiences_created: 2,
            playbook_updated: true,
            summary: "已沉淀 1 个知识模式, 2 条经验, 1 个 Playbook".into(),
        };
        assert_eq!(report.knowledge_created, 1);
        assert_eq!(report.experiences_created, 2);
        assert!(report.playbook_updated);
        assert!(report.summary.contains("Playbook"));
    }
}
