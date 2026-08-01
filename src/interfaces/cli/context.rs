//! CLI handlers for context-related commands:
//! `dt context`, `dt plan`, `dt domain`, `dt history`, `dt dependency`, `dt verify`.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::context::dependency::{
    DependencyRequest, DependencyService, DependencyTrait,
};
use crate::application::context::domain_query::{
    DomainQueryService, DomainQueryTrait, DomainRequest,
};
use crate::application::context::history::{HistoryRequest, HistoryService, HistoryTrait};
use crate::application::context::models::{AlertSeverity, ContextOptions};
use crate::application::context::pipeline::ContextPipeline;
use crate::application::context::plan::{PlanRequest, PlanService, PlanServiceTrait};
use crate::application::context::stages::RetrieverStage;
use crate::application::context::verify::{VerifyRequest, VerifyService, VerifyTrait};
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

// ---------------------------------------------------------------------------
// dt context
// ---------------------------------------------------------------------------

pub async fn handle_context(
    task: String,
    worlds: Option<String>,
    max_tokens: Option<usize>,
    thread_id: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: context --task {task} --worlds {:?} --max-tokens {:?} --thread-id {:?}",
        worlds,
        max_tokens,
        thread_id,
    );

    let options = ContextOptions {
        worlds: worlds.map(|w| w.split(',').map(|s| s.trim().to_string()).collect()),
        max_tokens,
        thread_id: thread_id.clone(),
        ..ContextOptions::default()
    };

    // Connect to Memgraph and create a RetrieverStage that can return
    // real data from the knowledge graph.
    let retriever = if let Some(ref g) = graph {
        RetrieverStage::new(
            g.clone(),
            None::<Arc<dyn VectorRepository>>,
            embed, // real SiliconFlow embed if available, None → NoopEmbedService internally
        )
    } else {
        RetrieverStage::empty()
    };

    // Build pipeline with the live retriever.
    let pipeline = ContextPipeline::default().with_retriever(retriever);
    match pipeline.execute(&task, &options).await {
        Ok(ctx) => {
            println!("Context for: \"{task}\"");
            println!(
                "  Thread:     {}",
                ctx.thread
                    .as_ref()
                    .map(|t| t.title.as_str())
                    .unwrap_or("(none)")
            );
            println!("  Reality:    {} items", ctx.reality.count);
            println!("  Knowledge:  {} items", ctx.knowledge.count);
            println!("  Memory:     {} items", ctx.memory.count);
            println!("  Semantic:   {} items", ctx.semantic.count);
            println!("  Runtime:    {} items", ctx.runtime.count);
            println!("  Reasoning:  {} items", ctx.reasoning.count);
            println!("  Tokens:     ~{}", ctx.estimated_tokens);

            // Show alerts
            for alert in &ctx.alerts {
                let sev = match alert.severity {
                    AlertSeverity::Info => "INFO",
                    AlertSeverity::Warning => "WARN",
                    AlertSeverity::Critical => "CRIT",
                };
                println!("  [{sev}] {}: {}", alert.source, alert.message);
            }

            // Show reality world items (most commonly useful)
            if !ctx.reality.items.is_empty() {
                println!("\nReality:");
                for item in &ctx.reality.items {
                    println!(
                        "  [{:.2}] {} → {}",
                        item.score,
                        item.label,
                        item.content.chars().take(80).collect::<String>()
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Context pipeline failed: {e}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// dt plan
// ---------------------------------------------------------------------------

pub async fn handle_plan(
    task: String,
    context: Option<String>,
    thread_id: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: plan --task {task} --context {:?} --thread-id {:?}",
        context,
        thread_id,
    );

    let request = PlanRequest {
        task: task.clone(),
        domain: None,
        context_json: context.clone(),
        style: None,
    };

    match graph {
        Some(graph) => {
            let plan_svc = PlanService::new(graph);
            match plan_svc.plan(&request).await {
                Ok(plan) => {
                    println!("Plan for: \"{}\"", plan.task);
                    if let Some(ref pb) = plan.matched_playbook {
                        println!(
                            "  Matched playbook: {} (score: {:.2}, domain: {})",
                            pb.title, pb.match_score, pb.domain
                        );
                    }
                    if plan.is_generic {
                        println!("  (generic plan — no playbook matched)");
                    }
                    println!(
                        "  Impact: {} risk, ~{} min",
                        plan.estimated_impact.risk, plan.estimated_impact.total_minutes
                    );
                    if !plan.estimated_impact.services.is_empty() {
                        println!("  Services: {}", plan.estimated_impact.services.join(", "));
                    }
                    println!("  Steps:");
                    for step in &plan.plan {
                        println!(
                            "    {}. {} [{}] ~{}min",
                            step.order,
                            step.action,
                            step.target.as_deref().unwrap_or("-"),
                            step.estimated_minutes.unwrap_or(0),
                        );
                        if let Some(ref notes) = step.notes {
                            println!("       └─ {notes}");
                        }
                    }
                }
                Err(e) => eprintln!("Plan failed: {e}"),
            }
        }
        None => {
            eprintln!("Graph database unavailable — cannot generate plan");
            println!("Plan: task=\"{task}\" (graph database unavailable)");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// dt domain
// ---------------------------------------------------------------------------

pub async fn handle_domain(
    name: String,
    depth: usize,
    include_code: bool,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: domain --name {name} --depth {depth} --include-code {include_code}",
    );

    let request = DomainRequest {
        domain: name.clone(),
        depth: Some(depth as u32),
    };

    match graph {
        Some(graph) => {
            let domain_svc = DomainQueryService::new(graph);
            match domain_svc.query(&request).await {
                Ok(model) => {
                    println!(
                        "Domain: \"{}\" ({} concepts, {} services, {} playbooks)",
                        model.domain,
                        model.concepts.len(),
                        model.services.len(),
                        model.playbooks.len(),
                    );
                    if !model.concepts.is_empty() {
                        println!("  Concepts:");
                        for c in &model.concepts {
                            println!(
                                "    [{}] {} → {}",
                                c.entity_type,
                                c.name,
                                c.description.chars().take(60).collect::<String>(),
                            );
                        }
                    }
                    if !model.services.is_empty() {
                        println!("  Services: {}", model.services.join(", "));
                    }
                    if !model.playbooks.is_empty() {
                        println!("  Playbooks: {}", model.playbooks.join(", "));
                    }
                    if !model.sub_domains.is_empty() {
                        println!("  Sub-domains: {}", model.sub_domains.join(", "));
                    }
                }
                Err(e) => eprintln!("Domain query failed: {e}"),
            }
        }
        None => {
            eprintln!("Graph database unavailable — cannot query domain");
            println!("Domain: name=\"{name}\" depth={depth} include-code={include_code} (graph database unavailable)");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// dt history
// ---------------------------------------------------------------------------

pub async fn handle_history(
    task: String,
    domain: Option<String>,
    days: u32,
    limit: usize,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: history --task {task} --domain {:?} --days {days} --limit {limit}",
        domain,
    );

    let since = chrono::Utc::now() - chrono::Duration::days(days as i64);
    let request = HistoryRequest {
        task: task.clone(),
        limit: Some(limit),
        since: Some(since.to_rfc3339()),
        entity_types: domain.as_ref().map(|d| vec![d.clone()]),
        project: None,
    };

    match graph {
        Some(graph) => {
            let history_svc = HistoryService::new(graph);
            match history_svc.search(&request).await {
                Ok(result) => {
                    println!(
                        "History for: \"{}\" ({} similar tasks found)",
                        result.query, result.total_found
                    );
                    for t in &result.similar_tasks {
                        let success = match t.success {
                            Some(true) => "✓",
                            Some(false) => "✗",
                            None => "?",
                        };
                        println!(
                            "  [{:.2}] [{}] {} {success} — {}",
                            t.score,
                            t.entity_type,
                            t.title,
                            t.description.chars().take(80).collect::<String>(),
                        );
                        if let Some(ref ts) = t.timestamp {
                            println!("         at {ts}");
                        }
                    }
                }
                Err(e) => eprintln!("History search failed: {e}"),
            }
        }
        None => {
            eprintln!("Graph database unavailable — cannot search history");
            println!("History: task=\"{task}\" (graph database unavailable)");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// dt dependency
// ---------------------------------------------------------------------------

pub async fn handle_dependency(
    target: String,
    direction: String,
    depth: usize,
    dep_type: String,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: dependency --target {target} --direction {direction} --depth {depth} --type {dep_type}",
    );

    let request = DependencyRequest {
        target: target.clone(),
        direction: Some(direction.clone()),
        max_depth: Some(depth as u32),
        project: None,
    };

    match graph {
        Some(graph) => {
            let dep_svc = DependencyService::new(graph);
            match dep_svc.analyse(&request).await {
                Ok(dep_graph) => {
                    println!("Dependency analysis for: \"{}\"", dep_graph.target);
                    println!("  Upstream:   {} entities", dep_graph.upstream.count);
                    for e in &dep_graph.upstream.entities {
                        println!("    [{}/d{}] {}", e.entity_type, e.distance, e.name);
                        if let Some(ref f) = e.source_file {
                            println!("              → {f}");
                        }
                    }
                    println!("  Downstream: {} entities", dep_graph.downstream.count);
                    for e in &dep_graph.downstream.entities {
                        println!("    [{}/d{}] {}", e.entity_type, e.distance, e.name);
                    }
                    println!(
                        "  Impact: {} risk — {} upstream, {} downstream affected",
                        dep_graph.impact_analysis.risk,
                        dep_graph.impact_analysis.affected_upstream_count,
                        dep_graph.impact_analysis.affected_downstream_count,
                    );
                    if !dep_graph.impact_analysis.services.is_empty() {
                        println!(
                            "  Services: {}",
                            dep_graph.impact_analysis.services.join(", ")
                        );
                    }
                }
                Err(e) => eprintln!("Dependency analysis failed: {e}"),
            }
        }
        None => {
            eprintln!("Graph database unavailable — cannot analyze dependencies");
            println!(
                "Dependency: target=\"{target}\" direction={direction} depth={depth} type={dep_type} (graph database unavailable)"
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// dt verify
// ---------------------------------------------------------------------------

pub async fn handle_verify(
    files: Vec<String>,
    check_config: bool,
    check_db: bool,
    check_api: bool,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: verify --files {:?} --check-config {check_config} --check-db {check_db} --check-api {check_api}",
        files,
    );

    let request = VerifyRequest {
        files: files.clone(),
        project: None,
        thorough: Some(true),
    };

    match graph {
        Some(graph) => {
            let verify_svc = VerifyService::new(graph);
            match verify_svc.verify(&request).await {
                Ok(report) => {
                    let status = match report.overall {
                        crate::application::context::verify::Status::Pass => "PASS",
                        crate::application::context::verify::Status::Warn => "WARN",
                        crate::application::context::verify::Status::Fail => "FAIL",
                    };
                    println!(
                        "Verify: {} files — {} passed, {} warned, {} failed — overall: {status}",
                        files.len(),
                        report.passed,
                        report.warned,
                        report.failed,
                    );
                    for check in &report.checks {
                        let s = match check.status {
                            crate::application::context::verify::Status::Pass => "✓",
                            crate::application::context::verify::Status::Warn => "⚠",
                            crate::application::context::verify::Status::Fail => "✗",
                        };
                        let target = check.target.as_deref().unwrap_or("-");
                        println!(
                            "  {s} [{check_category}] {description} ({target}): {detail}",
                            check_category = check.category,
                            description = check.description,
                            detail = check.detail,
                        );
                    }
                    if !report.suggestions.is_empty() {
                        println!("Suggestions:");
                        for s in &report.suggestions {
                            println!("  → {s}");
                        }
                    }
                }
                Err(e) => eprintln!("Verification failed: {e}"),
            }
        }
        None => {
            eprintln!("Graph database unavailable — cannot verify");
            println!("Verify: files={:?} (graph database unavailable)", files);
        }
    }

    Ok(())
}
