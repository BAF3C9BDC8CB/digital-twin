//! LearnService——从 AI 任务执行中进行高层知识沉淀。
//!
//! `learn` 操作接收任务结果（模式、踩坑、决策、成功/失败），
//! 并将结构化知识写入 Knowledge 世界：
//!
//! - 模式（Patterns）→ Knowledge 节点（来源：`ai_task`）
//! - 踩坑（Pitfalls）→ Experience 节点
//! - 模式 + 踩坑 → Playbook（有序步骤）
//! - 成功/失败 → Playbook 计数器更新；自动标记 `_needs_review`
//!
//! # 典型用法
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

use crate::domain::error::DtError;
use async_trait::async_trait;
use std::sync::Arc;

use super::knowledge::entities::{
    Experience, ExperienceSeverity, Knowledge, KnowledgeSource, Playbook, Step,
};
use super::knowledge::service::KnowledgeService;
use crate::application::knowledge::knowledge::service::DefaultKnowledgeService;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

// ---------------------------------------------------------------------------
// 请求 / 报告
// ---------------------------------------------------------------------------

/// `learn` 操作的输入。
#[derive(Debug, Clone)]
pub struct LearnRequest {
    /// 任务标题或描述（如 "支付平台迁移"）。
    pub task: String,
    /// 受影响的实体（文件路径、类名、服务名）。
    pub entities: Vec<String>,
    /// 识别出的解决方案模式。
    pub pattern: Option<String>,
    /// 遇到的或需要防范的坑。
    pub pitfalls: Vec<String>,
    /// 做出的架构 / 技术决策。
    pub decisions: Vec<String>,
    /// 可选的数字线程 ID，用于跨任务溯源。
    pub thread_id: Option<String>,
    /// 执行是否成功。
    pub success: Option<bool>,
    /// 所属项目。
    pub project: Option<String>,
}

/// 沉淀结果的摘要。
#[derive(Debug, Clone)]
pub struct LearnReport {
    /// 创建/更新的 Knowledge 节点数。
    pub knowledge_created: usize,
    /// 创建的 Experience 节点数。
    pub experiences_created: usize,
    /// 是否创建了 Playbook 或更新了其计数器。
    pub playbook_updated: bool,
    /// 人类可读的一行摘要。
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Trait（服务接口）
// ---------------------------------------------------------------------------

/// 高层知识沉淀服务。
///
/// 一个 `LearnService` 接收结构化的 [`LearnRequest`] 并：
/// 1. 从任务名提取领域。
/// 2. 写入基于模式（pattern）的 Knowledge 节点。
/// 3. 写入基于踩坑（pitfall）的 Experience 节点。
/// 4. 由模式 + 踩坑综合生成 Playbook。
/// 5. 更新 Playbook 上的成功/失败计数器。
/// 6. 可选地将节点关联到数字线程。
#[async_trait]
pub trait LearnService: Send + Sync {
    /// 执行学习管线并返回报告。
    async fn learn(&self, request: &LearnRequest) -> Result<LearnReport, DtError>;
}

// ---------------------------------------------------------------------------
// 实现
// ---------------------------------------------------------------------------

/// [`LearnService`] 的规范实现。
///
/// 实际存储写入委托给 [`KnowledgeService`]。
pub struct LearnServiceImpl<S: KnowledgeService> {
    knowledge: Arc<S>,
}

impl<S: KnowledgeService> LearnServiceImpl<S> {
    pub fn new(knowledge: Arc<S>) -> Self {
        Self { knowledge }
    }
}

/// 由支持向量化的 [`DefaultKnowledgeService`] 支撑的
/// [`LearnServiceImpl`] 构造函数。
///
/// 当 `embed` / `vector` 后端可用时调用此构造函数——底层服务会在
/// 每次写入时自动将 Knowledge、Experience、Concept 与 Playbook 节点
/// 向量化到统一的 `kg_nodes` Qdrant 集合。
///
/// 仅对 `DefaultKnowledgeService`（而非泛型 `S`）实现，因为它依赖
/// 该具体类型的 [`with_vectorization`] 构造函数。
///
/// [`with_vectorization`]: DefaultKnowledgeService::with_vectorization
impl LearnServiceImpl<DefaultKnowledgeService> {
    pub fn with_vectorization(
        graph: Arc<dyn GraphRepository>,
        embed: Arc<dyn EmbedService>,
        vector: Arc<dyn VectorRepository>,
    ) -> Self {
        let svc = Arc::new(DefaultKnowledgeService::with_vectorization(
            graph, embed, vector,
        ));
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
                summary: with_keywords(pattern.clone(), &request.task),
                content: format!(
                    "# {}\n\n## 模式\n\n{}\n\n## 涉及实体\n\n{}",
                    request.task,
                    pattern,
                    request.entities.join(", "),
                ),
                definition: format!("{} 的解决方案模式", request.task),
                source: KnowledgeSource::AiTask,
                project: project.to_string(),
                scope: "project".to_string(),
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
                summary: with_keywords(pitfall.clone(), &request.task),
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
                content: format!("## 决策\n{}\n\n## 任务上下文\n{}", decision, request.task,),
                domain: domain.clone(),
                severity: ExperienceSeverity::Info,
                project: project.to_string(),
                created_at: now.clone(),
            };
            self.knowledge.write_experience(&experience).await?;
            experience_count += 1;
        }

        // ---- 4. Pattern + pitfalls → Playbook ----
        if request.pattern.is_some()
            || !request.pitfalls.is_empty()
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

            // 模式步骤
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

            // 实体步骤
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

            // 踩坑步骤（预防性）
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

            // 决策步骤
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

            // 确定当前计数器：若提供 success 则更新已有计数。
            // 创建时默认 0；ON MATCH 路径负责更新。
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
        // Playbook 已在上方以正确的计数器写入。
        // 若设置了 success/failure，计数器已一并应用。
        // 后续对同一任务的调用由 write_playbook 中的 ON MATCH SET
        // 覆盖。目前这样已足够。

        // ---- 6. Thread association (placeholder) ----
        // 未来阶段：通过 `HAS_KNOWLEDGE` / `HAS_PLAYBOOK` 关系
        // 将 Knowledge/Experience/Playbook 节点关联到数字线程。
        if let Some(_thread_id) = &request.thread_id {
            // TODO: 数字线程实体存在后，创建：
            //   MATCH (th:Thread {thread_id: $thread_id})
            //   MERGE (k)-[:HAS_KNOWLEDGE]->(th)
            //   MERGE (p)-[:HAS_PLAYBOOK]->(th)
        }

        // ---- 构建摘要 ----
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
// 辅助函数
// ---------------------------------------------------------------------------

/// 从任务标题提取领域关键词。
///
/// 示例：
/// - "支付平台迁移"  → "支付"
/// - "部署服务升级"  → "部署"
/// - "日志采集优化"  → "日志"
/// 从任务标题提取领域关键词。
///
/// 为知识/经验 summary 追加中英文检索关键词，提升跨语言向量命中。
///
/// 中文任务（如"消息撤回链路"）常被英文查询词（recall/withdraw）检索不到——
/// 在 summary 尾部追加"（keywords: recall, withdraw, ...）"让向量空间同时
/// 覆盖中英文语义。内置常见业务关键词映射，映射不到的保留原文。
fn with_keywords(text: String, task: &str) -> String {
    const MAP: &[(&str, &[&str])] = &[
        ("撤回", &["recall", "withdraw", "revoke"]),
        ("消息", &["message", "msg"]),
        ("发送", &["send", "push"]),
        ("群组", &["group"]),
        ("单聊", &["c2c", "single"]),
        ("签名", &["sign", "signature"]),
        ("账号", &["account", "import"]),
        ("成员", &["member"]),
        ("回调", &["callback"]),
        ("历史", &["history", "roam"]),
        ("未读", &["unread"]),
        ("部署", &["deploy", "release"]),
        ("配置", &["config"]),
        ("登录", &["login", "auth"]),
        ("支付", &["pay", "payment"]),
        ("订单", &["order"]),
        ("库存", &["inventory", "stock"]),
        ("导入", &["import"]),
    ];
    let mut kws: Vec<&str> = Vec::new();
    for (zh, en) in MAP {
        if task.contains(zh) {
            kws.extend_from_slice(en);
        }
    }
    if kws.is_empty() {
        text
    } else {
        format!("{}（keywords: {}）", text.trim_end(), kws.join(", "))
    }
}

/// 先匹配常见中文领域关键词，匹配不到则回退到
/// 去除首尾空白后任务的前 2 个字符。
pub(crate) fn extract_domain(task: &str) -> String {
    // 常见中文领域关键词
    let domains = [
        "支付",
        "部署",
        "日志",
        "配置",
        "监控",
        "安全",
        "测试",
        "数据库",
    ];
    for d in &domains {
        if task.contains(d) {
            return d.to_string();
        }
    }
    // 回退：去除空白后任务的前 2 个字符
    task.chars().take(2).collect()
}

/// 将任务标题转换为 snake_case 标识符。
///
/// 去除非字母数字 ASCII 字符，转小写，以连字符连接。
/// 将任务标题转换为 snake_case 标识符。
///
/// 去除非字母数字 ASCII 字符，转小写，以连字符连接。
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

/// 为模式创建确定性的 knowledge_id。
/// 为模式创建确定性的 knowledge_id。
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::knowledge::entities::{Concept, Domain};
    use crate::application::knowledge::knowledge::service::KnowledgeService;
    use crate::domain::error::DtError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 按实体类型统计调用次数。
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
        async fn delete_knowledge(&self, _entity_id: &str) -> Result<(), DtError> {
            Ok(())
        }
    }

    fn spy() -> (
        Arc<SpyKnowledgeService>,
        LearnServiceImpl<SpyKnowledgeService>,
    ) {
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

    /// 验证 trait 是对象安全的。
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
            .expect("learn 应成功");
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
            .expect("learn 应成功");
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
            .expect("learn 应成功");
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
            .expect("learn 应成功");
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
            .expect("learn 应成功");
        assert_eq!(report.experiences_created, 1);
        assert!(report.playbook_updated);
    }

    // ---- 辅助测试 ----

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
