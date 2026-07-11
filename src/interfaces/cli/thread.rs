//! CLI handler for `dt thread` — manage Digital Thread lifecycle.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::knowledge::thread::service::{
    ThreadRequest, ThreadService, ThreadTrait,
};
use crate::domain::traits::GraphRepository;

/// Handle `dt thread` — create, list, append to, or close Digital Threads.
///
/// `graph` must be pre-connected by the caller.
pub async fn handle_thread(
    action: String,
    name: Option<String>,
    description: Option<String>,
    thread_id: Option<String>,
    session_id: Option<String>,
    decision_id: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: thread --action {action} --name {:?} --description {:?} \
         --thread-id {:?} --session-id {:?} --decision-id {:?}",
        name,
        description,
        thread_id,
        session_id,
        decision_id,
    );

    let request = ThreadRequest {
        action: action.clone(),
        thread_id,
        title: name.clone(),
        description,
        session_id,
        summary: None,
        decision: None,
        reason: None,
        impact: None,
        outcome: None,
        project: None,
        limit: None,
    };

    match graph {
        Some(graph) => {
            let thread_svc = ThreadService::new(graph);
            match thread_svc.execute(&request).await {
                Ok(response) => {
                    println!("Thread: {} — {}", response.action, response.message);
                    if let Some(ref t) = response.thread {
                        println!("  ID:      {}", t.thread_id);
                        println!("  Title:   {}", t.title);
                        println!("  Status:  {}", t.status);
                        if t.event_count > 0 {
                            println!("  Events:  {}", t.event_count);
                            for s in &t.sessions {
                                println!("    Session: {} ({})", s.session_id, s.summary);
                            }
                            for d in &t.decisions {
                                println!("    Decision: {}", d.decision);
                            }
                        }
                    }
                    if let Some(ref list) = response.list {
                        println!("  Total threads: {}", list.total);
                        for t in &list.threads {
                            println!(
                                "    [{}] {} ({} events)",
                                t.status, t.title, t.event_count,
                            );
                        }
                    }
                }
                Err(e) => eprintln!("Thread operation failed: {e}"),
            }
        }
        None => {
            eprintln!("Neo4j unavailable — cannot manage threads");
            println!("Thread: action={action} (Neo4j unavailable)");
        }
    }

    Ok(())
}
