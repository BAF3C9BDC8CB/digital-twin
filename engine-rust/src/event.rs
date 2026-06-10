use anyhow::Result;
use chrono::Utc;
use sha2::{Digest, Sha256};
use serde_json::json;

use crate::neo4j;

pub async fn write_event(
    event_type: &str,
    entity_id: &str,
    entity_type: Option<&str>,
    project: Option<&str>,
    details: Option<&str>,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let raw = format!("{}::{}::{}", event_type, entity_id, ts);
    let eid = hex::encode(&Sha256::digest(raw.as_bytes())[..20]);

    let prj = project.unwrap_or("");
    let etype = entity_type.unwrap_or("");

    // Merge Event node with correct projection
    let stmt = "\
MERGE (e:Event {event_id: $eid})
ON CREATE SET
    e.type = $type,
    e.entity_id = $entity_id,
    e.entity_type = $entity_type,
    e.project = $project,
    e.details = $details,
    e.timestamp = $ts
WITH e
OPTIONAL MATCH (p:Project {name: $project})
OPTIONAL MATCH (j:JenkinsJob {name: $entity_id})
OPTIONAL MATCH (s:Software {name: $entity_id})
OPTIONAL MATCH (n:NacosConfig {data_id: $entity_id})
WITH e, p, j, s, n
FOREACH (_ IN CASE WHEN p IS NOT NULL THEN [1] END |
    MERGE (e)-[:RELATES_TO]->(p))
FOREACH (_ IN CASE WHEN j IS NOT NULL AND $type = 'Deploy' THEN [1] END |
    MERGE (e)-[:DEPLOYED_JOB]->(j))
FOREACH (_ IN CASE WHEN s IS NOT NULL AND $type = 'SoftwareInstalled' THEN [1] END |
    MERGE (e)-[:INSTALLED_SOFTWARE]->(s))
FOREACH (_ IN CASE WHEN n IS NOT NULL AND $type = 'ConfigChange' THEN [1] END |
    MERGE (e)-[:SYNCED_NAMESPACE]->(n))";

    let resp = neo4j::run_cypher_raw(stmt, json!({
        "eid": eid,
        "type": event_type,
        "entity_id": entity_id,
        "entity_type": etype,
        "project": prj,
        "details": details.unwrap_or(""),
        "ts": ts,
    })).await?;

    if let Some(errors) = resp["errors"].as_array() {
        if !errors.is_empty() {
            eprintln!("Neo4j errors: {:?}", errors);
        }
    }

    println!("📝 已将 [{}: {}] 记录到知识图谱", event_type, entity_id);
    Ok(())
}
