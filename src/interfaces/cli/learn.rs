//! CLI handler for `dt learn` — learn from AI task execution.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::knowledge::knowledge::service::DefaultKnowledgeService;
use crate::application::knowledge::learn::{self, LearnRequest, LearnService, LearnServiceImpl};
use crate::application::sync::batch::SyncAccumulator;
use crate::domain::traits::GraphRepository;

/// Handle `dt learn` — synthesise Knowledge, Experience, and Playbook nodes from a task result.
///
/// `graph` must be pre-connected by the caller.
/// `sync_acc` enqueues nodes for background (non-blocking) sync to Qdrant.
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
        "dt-daemon CLI: learn --task {task} --pattern {:?} --pitfalls {:?}",
        pattern,
        pitfalls,
    );

    // Connect to Neo4j for real persistence (fallback to noop if unavailable).
    // Both branches produce Arc<dyn GraphRepository>, so DefaultKnowledgeService is concrete.
    let graph_for_knowledge: Arc<dyn GraphRepository> = match graph {
        Some(g) => g,
        None => {
            tracing::warn!("Neo4j unavailable — using noop for learn");
            Arc::new(crate::infrastructure::neo4j::NoopGraphRepo)
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
                "learn: k={} e={} pb={} summary={}",
                report.knowledge_created,
                report.experiences_created,
                report.playbook_updated,
                report.summary,
            );

            // ── Auto-sync to Qdrant ──────────────────────────────
            auto_sync_learn(&request, sync_acc).await;
        }
        Err(e) => {
            eprintln!("learn failed: {e}");
            return Err(e.into());
        }
    }

    Ok(())
}

/// Reconstruct the Knowledge / Experience IDs created by LearnServiceImpl
/// and enqueue them for background sync to Qdrant.
///
/// Flushes the queue before returning so the sync completes within the
/// CLI process lifetime.
async fn auto_sync_learn(request: &LearnRequest, acc: Option<Arc<SyncAccumulator>>) {
    let acc = match acc {
        Some(a) => a,
        None => return,
    };

    let project = request.project.as_deref().unwrap_or("unknown");
    let domain = learn::extract_domain(&request.task);

    // Enqueue Knowledge node (created when pattern is present).
    if request.pattern.is_some() {
        let kid = learn::format_knowledge_id(project, &domain, "pattern", &request.task);
        acc.enqueue("Knowledge", "knowledge_id", &kid);
    }

    // Enqueue Experience nodes (one per pitfall).
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
