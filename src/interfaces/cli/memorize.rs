//! CLI handler for `dt memorize` — write structured knowledge into the KG.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::knowledge::knowledge::service::{
    DefaultKnowledgeService, KnowledgeService,
};
use crate::application::sync::batch::SyncAccumulator;
use crate::domain::traits::GraphRepository;

/// Handle `dt memorize` — write a knowledge entry (Knowledge, Experience, Concept, Domain, Playbook).
///
/// `graph` must be pre-connected by the caller.
/// `sync_acc` enqueues nodes for background (non-blocking) sync to Qdrant.
pub async fn handle_memorize(
    knowledge_type: String,
    entity_id: String,
    entity_type: Option<String>,
    project: Option<String>,
    details: String,
    graph: Option<Arc<dyn GraphRepository>>,
    sync_acc: Option<Arc<SyncAccumulator>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: memorize --type {knowledge_type} --entity-id {entity_id} --details {details}",
    );

    let project_name = project.as_deref().unwrap_or("unknown");
    let etype = entity_type.as_deref().unwrap_or(&knowledge_type);

    let svc = graph.as_ref().map(|g| DefaultKnowledgeService::new(Arc::clone(g)));

    // Route based on knowledge_type to the correct entity constructor.
    match knowledge_type.to_lowercase().as_str() {
        "decision" | "knowledgeadded" | "environment" | "dependencies" => {
            let knowledge = crate::application::knowledge::knowledge::knowledge_from_details(
                &entity_id,
                etype,
                project_name,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_knowledge(&knowledge).await {
                    Ok(()) => println!(
                        "Knowledge written: id={} name={} title={} domain={} project={}",
                        knowledge.knowledge_id,
                        knowledge.name,
                        knowledge.title,
                        knowledge.domain,
                        knowledge.project,
                    ),
                    Err(e) => eprintln!("Knowledge write failed: {e}"),
                }
            } else {
                tracing::warn!("Neo4j unavailable — knowledge not persisted");
                println!(
                    "Knowledge (not persisted): id={} name={} title={} domain={} project={}",
                    knowledge.knowledge_id,
                    knowledge.name,
                    knowledge.title,
                    knowledge.domain,
                    knowledge.project,
                );
            }
        }
        "experience" => {
            let experience = crate::application::knowledge::knowledge::experience_from_details(
                &entity_id,
                project_name,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_experience(&experience).await {
                    Ok(()) => println!(
                        "Experience written: id={} title={} severity={} domain={}",
                        experience.experience_id,
                        experience.title,
                        experience.severity.as_str(),
                        experience.domain,
                    ),
                    Err(e) => eprintln!("Experience write failed: {e}"),
                }
            } else {
                tracing::warn!("Neo4j unavailable — experience not persisted");
                println!(
                    "Experience (not persisted): id={} title={} severity={} domain={}",
                    experience.experience_id,
                    experience.title,
                    experience.severity.as_str(),
                    experience.domain,
                );
            }
        }
        "concept" => {
            let concept = crate::application::knowledge::knowledge::concept_from_details(
                &entity_id,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_concept(&concept).await {
                    Ok(()) => println!(
                        "Concept written: id={} name={} domain={}",
                        concept.concept_id, concept.name, concept.domain,
                    ),
                    Err(e) => eprintln!("Concept write failed: {e}"),
                }
            } else {
                tracing::warn!("Neo4j unavailable — concept not persisted");
                println!(
                    "Concept (not persisted): id={} name={} domain={}",
                    concept.concept_id, concept.name, concept.domain,
                );
            }
        }
        "domain" => {
            let domain = crate::application::knowledge::knowledge::domain_from_details(
                &entity_id,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_domain(&domain).await {
                    Ok(()) => println!(
                        "Domain written: id={} name={}",
                        domain.domain_id, domain.name,
                    ),
                    Err(e) => eprintln!("Domain write failed: {e}"),
                }
            } else {
                tracing::warn!("Neo4j unavailable — domain not persisted");
                println!(
                    "Domain (not persisted): id={} name={}",
                    domain.domain_id, domain.name,
                );
            }
        }
        "playbook" => {
            let playbook = crate::application::knowledge::knowledge::playbook_from_details(
                &entity_id,
                project_name,
                &details,
            );
            if let Some(ref svc) = svc {
                match svc.write_playbook(&playbook).await {
                    Ok(()) => println!(
                        "Playbook written: id={} name={} domain={}",
                        playbook.playbook_id, playbook.name, playbook.domain,
                    ),
                    Err(e) => eprintln!("Playbook write failed: {e}"),
                }
            } else {
                tracing::warn!("Neo4j unavailable — playbook not persisted");
                println!(
                    "Playbook (not persisted): id={} name={} domain={}",
                    playbook.playbook_id, playbook.name, playbook.domain,
                );
            }
        }
        other => {
            eprintln!(
                "Unknown knowledge type: {other}. \
                 Expected one of: Decision, KnowledgeAdded, Environment, \
                 Dependencies, Experience, Concept, Domain, Playbook"
            );
            return Ok(());
        }
    }

    // ── Auto-sync to Qdrant ──────────────────────────────────────────
    auto_sync_kg(&knowledge_type, &entity_id, sync_acc).await;

    Ok(())
}

/// Map a `knowledge_type` to its Neo4j label + id-property key, then
/// enqueue the newly-written node for background sync to Qdrant.
///
/// This returns immediately — the actual embed + upsert happens in a
/// background worker that accumulates batches for GPU efficiency.
///
/// Flushes the queue before returning so the sync completes within the
/// CLI process lifetime.
async fn auto_sync_kg(
    knowledge_type: &str,
    entity_id: &str,
    acc: Option<Arc<SyncAccumulator>>,
) {
    let acc = match acc {
        Some(a) => a,
        None => return,
    };

    let (label, key) = match knowledge_type.to_lowercase().as_str() {
        "decision" | "knowledgeadded" | "environment" | "dependencies" => {
            ("Knowledge", "knowledge_id")
        }
        "experience" => ("Experience", "experience_id"),
        "concept" => ("Concept", "concept_id"),
        "domain" => ("Domain", "domain_id"),
        "playbook" | "pattern" | "patch" | "orchestrator" => {
            ("Playbook", "playbook_id")
        }
        _ => return,
    };

    acc.enqueue(label, key, entity_id);
    acc.flush().await;
}
