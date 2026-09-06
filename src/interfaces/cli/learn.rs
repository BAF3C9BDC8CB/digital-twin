//! `dt learn` 的 CLI 处理器——从 AI 任务执行中学习。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::sync::Arc;

use crate::application::knowledge::knowledge::service::DefaultKnowledgeService;
use crate::application::knowledge::learn::{self, LearnRequest, LearnService, LearnServiceImpl};
use crate::application::sync::batch::SyncAccumulator;
use crate::domain::traits::GraphRepository;

/// 处理 `dt learn`——从任务结果中综合出 Knowledge、Experience、Playbook 节点。
///
/// `graph` 必须由调用方预先连接。
/// `sync_acc` 将节点入队，供后台（非阻塞）同步到 Qdrant。
pub async fn handle_learn(
    task: String,
    entities: Vec<String>,
    pattern: Option<String>,
    pitfalls: Vec<String>,
    decisions: Vec<String>,
    thread_id: Option<String>,
    success: Option<bool>,
    project: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    sync_acc: Option<Arc<SyncAccumulator>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt CLI: learn --task {task} --pattern {:?} --pitfalls {:?}",
        pattern,
        pitfalls,
    );
    tracing::info!(
        task = "learn",
        task_name = %task,
        entities = ?entities,
        pattern = ?pattern,
        pitfalls_count = pitfalls.len(),
        decisions_count = decisions.len(),
        project = ?project,
        stage = "learn_start",
        "learn 调用开始"
    );

    // 连接 Memgraph 实现真实持久化（不可用时回退到 noop）。
    // 两个分支都产生 Arc<dyn GraphRepository>，因此 DefaultKnowledgeService 是具体类型。
    let graph_for_knowledge: Arc<dyn GraphRepository> = match graph {
        Some(g) => g,
        None => {
            tracing::warn!("Memgraph 不可用——learn 使用 noop");
            Arc::new(crate::infrastructure::memgraph::NoopGraphRepo)
        }
    };
    let knowledge_svc = Arc::new(DefaultKnowledgeService::new(graph_for_knowledge));
    let learner = LearnServiceImpl::new(knowledge_svc);

    let request = LearnRequest {
        task,
        entities,
        pattern,
        pitfalls,
        decisions,
        thread_id,
        success,
        project,
    };

    match learner.learn(&request).await {
        Ok(report) => {
            println!("{}", report.summary);
            tracing::info!(
                "learn: 知识={} 经验={} 剧本={} 摘要={}",
                report.knowledge_created,
                report.experiences_created,
                report.playbook_updated,
                report.summary,
            );

            // ── 自动同步到 Qdrant ──────────────────────────────
            auto_sync_learn(&request, sync_acc).await;
        }
        Err(e) => {
            eprintln!("learn 失败: {e}");
            return Err(e.into());
        }
    }

    Ok(())
}

/// 重建 LearnServiceImpl 创建的 Knowledge / Experience ID，
/// 并排队等待后台同步到 Qdrant。
///
/// 返回前会 flush 队列，确保同步在
/// CLI 进程生命周期内完成。
async fn auto_sync_learn(request: &LearnRequest, acc: Option<Arc<SyncAccumulator>>) {
    let acc = match acc {
        Some(a) => a,
        None => return,
    };

    let project = request.project.as_deref().unwrap_or("unknown");
    let domain = learn::extract_domain(&request.task);

    // 将 Knowledge 节点入队（存在 pattern 时创建）。
    if request.pattern.is_some() {
        let kid = learn::format_knowledge_id(project, &domain, "pattern", &request.task);
        acc.enqueue("Knowledge", "knowledge_id", &kid);
    }

    // 将 Experience 节点入队（每个 pitfall 一个）。
    for (i, _) in request.pitfalls.iter().enumerate() {
        let eid = format!(
            "dt://experience/{}/{}/pitfall-{}-{}",
            project,
            &domain,
            learn::to_snake(&request.task),
            i + 1,
        );
        acc.enqueue("Experience", "experience_id", &eid);
    }

    acc.flush().await;
}
