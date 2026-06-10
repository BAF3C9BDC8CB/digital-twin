use anyhow::Result;
use chrono::Utc;
use sha2::{Digest, Sha256};
use serde_json::json;

use crate::neo4j;

pub async fn write_knowledge(
    knowledge_type: &str,
    entity_id: &str,
    _entity_type: Option<&str>,
    project: Option<&str>,
    details: Option<&str>,
) -> Result<()> {
    let ts = Utc::now().timestamp();
    let raw = format!("knowledge::{}::{}", entity_id, ts);
    let kid = hex::encode(&Sha256::digest(raw.as_bytes())[..20]);
    let details_str = details.unwrap_or("");

    // Parse structured fields from details
    let mut root_cause = String::new();
    let mut fix_summary = String::new();
    let mut title = String::new();
    let mut description = String::new();
    let mut remaining = details_str.to_string();

    let fix_summary_ref = &mut fix_summary;
    for (prefix, target) in [
        ("root_cause:", &mut root_cause as &mut String),
        ("fix:", fix_summary_ref as &mut String),
        ("decision:", &mut title),
        ("reason:", &mut description),
    ] {
        // Check if details starts with "key: value; more..."
        if let Some(pos) = remaining.find(prefix) {
            let start = pos + prefix.len();
            let end = remaining[start..].find(';').map(|e| start + e).unwrap_or(remaining.len());
            let val = remaining[start..end].trim().to_string();
            if !val.is_empty() {
                target.push_str(&val);
                // Remove this key from remaining for later clean details
                remaining = format!("{} {}", &remaining[..pos], &remaining[end.min(remaining.len())..]);
            }
        }
    }

    // If details had structured fields, use remaining as cleaner details
    let final_details = if root_cause.is_empty() && fix_summary.is_empty() && title.is_empty() && description.is_empty() {
        details_str.to_string()
    } else {
        remaining.split_whitespace().collect::<Vec<_>>().join(" ")
    };

    let stmt = "\
MERGE (k:Knowledge {id: $id})
ON CREATE SET
    k.name = $name,
    k.title = $title,
    k.context = $context,
    k.details = $details,
    k.description = $description,
    k.project = $project,
    k.root_cause = $root_cause,
    k.fix_summary = $fix_summary,
    k.updatedAt = $updated_at,
    k.updatedBy = 'dt'";

    neo4j::run_cypher_raw(stmt, json!({
        "id": kid,
        "name": entity_id,
        "title": title,
        "context": knowledge_type,
        "details": final_details,
        "description": description,
        "project": project.unwrap_or(""),
        "root_cause": root_cause,
        "fix_summary": fix_summary,
        "updated_at": ts,
    })).await?;

    println!("📝 已将 [{}: {}] 记录到知识图谱", knowledge_type, entity_id);
    Ok(())
}
