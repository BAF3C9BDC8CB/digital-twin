//! CLI handler for `dt learn` — learn from AI task execution.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::knowledge::knowledge::service::DefaultKnowledgeService;
use crate::application::knowledge::learn::{LearnRequest, LearnService, LearnServiceImpl};
use crate::domain::traits::GraphRepository;

/// Handle `dt learn` — synthesise Knowledge, Experience, and Playbook nodes from a task result.
///
/// `graph` must be pre-connected by the caller.
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
        }
        Err(e) => {
            eprintln!("learn failed: {e}");
            return Err(e.into());
        }
    }

    Ok(())
}
