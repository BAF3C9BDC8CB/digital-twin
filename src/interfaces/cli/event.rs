//! CLI handler for `dt event` — record an event to the knowledge graph.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::service::{DefaultMemoryService, MemoryService};
use crate::domain::traits::GraphRepository;

/// Parse an EventType from a string (case-insensitive).
pub fn parse_event_type(s: &str) -> Option<EventType> {
    match s.to_lowercase().as_str() {
        "modification" => Some(EventType::Modification),
        "deployment" => Some(EventType::Deployment),
        "configchange" => Some(EventType::ConfigChange),
        "bugfix" => Some(EventType::BugFix),
        "decision" => Some(EventType::Decision),
        "conversation" => Some(EventType::Conversation),
        _ => None,
    }
}

/// Handle `dt event` — records a MemoryEvent node in the KG.
///
/// `graph` must be pre-connected by the caller.
pub async fn handle_event(
    event_type: String,
    entity_id: String,
    entity_type: String,
    project: Option<String>,
    details: String,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: event --type {event_type} --entity-id {entity_id} --entity-type {entity_type} --details {details}",
    );

    let project_name = project.as_deref().unwrap_or("unknown");
    let session_id = format!("{}-cli", chrono::Utc::now().format("%Y-%m-%d"));

    // Parse event type
    let parsed_type = match parse_event_type(&event_type) {
        Some(t) => t,
        None => {
            eprintln!(
                "Unknown event type: {event_type}. \
                 Expected: Modification, Deployment, ConfigChange, BugFix, Decision, Conversation"
            );
            return Ok(());
        }
    };

    let event = MemoryEvent {
        event_type: parsed_type,
        entity_id: entity_id.clone(),
        entity_type: entity_type.clone(),
        project: project_name.to_string(),
        details: details.clone(),
        session_id,
        timestamp: chrono::Utc::now(),
    };

    // Connect to Neo4j and record the event
    match graph {
        Some(graph) => {
            let memory_svc = DefaultMemoryService::new(graph);
            match memory_svc.record_event(&event).await {
                Ok(()) => {
                    println!(
                        "Event recorded: type={} entity_id={} entity_type={} project={}",
                        event_type, entity_id, entity_type, project_name,
                    );
                    tracing::info!(
                        "event {} → {} ({}) in project {}",
                        event_type,
                        entity_id,
                        entity_type,
                        project_name,
                    );
                }
                Err(e) => {
                    eprintln!("Event record failed: {e}");
                }
            }
        }
        None => {
            tracing::warn!("Neo4j unavailable — event not persisted");
            println!(
                "Event (not persisted): type={event_type} entity_id={entity_id} \
                 entity_type={entity_type} project={project_name}"
            );
        }
    }

    Ok(())
}
