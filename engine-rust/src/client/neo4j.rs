use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use base64::Engine;

use crate::config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodNode {
    pub method_id: String,
    pub project: String,
    pub file_path: String,
    pub language: String,
    pub package_or_module: String,
    pub class_name: String,
    pub name: String,
    pub signature: String,
    pub params: String,
    pub return_type: String,
    pub start_line: i64,
    pub end_line: i64,
    pub calls: Vec<String>,
}

fn neo4j_url() -> String {
    let cfg = config::load();
    let base = cfg.services.neo4j.url.trim_end_matches('/');
    format!("{}/db/neo4j/tx/commit", base)
}

fn auth_header() -> String {
    let cfg = config::load();
    let cred = format!("{}:{}", cfg.services.neo4j.user, cfg.services.neo4j.password);
    format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, cred.as_bytes()))
}

pub async fn run_cypher_raw(statement: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let body = json!({
        "statements": [{
            "statement": statement,
            "parameters": params
        }]
    });
    let client = crate::client::get_client();
    let resp = client
        .post(neo4j_url())
        .header("Content-Type", "application/json")
        .header("Authorization", auth_header())
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("Neo4j request failed ({}): {}", status, text));
    }
    let data: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow!("JSON parse failed (status {}): {} | body: {}", status, e, &text[..200.min(text.len())]))?;
    Ok(data)
}

pub async fn ensure_schema() -> Result<()> {
    let constraints = [
        "CREATE CONSTRAINT IF NOT EXISTS FOR (m:Method) REQUIRE m.method_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (c:Class) REQUIRE c.class_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (e:Event) REQUIRE e.event_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (k:Knowledge) REQUIRE k.id IS UNIQUE",
        "CREATE INDEX IF NOT EXISTS FOR (m:Method) ON (m.project)",
        "CREATE INDEX IF NOT EXISTS FOR (m:Method) ON (m.name)",
        "CREATE INDEX IF NOT EXISTS FOR (m:Method) ON (m.file_path)",
        "CREATE INDEX IF NOT EXISTS FOR (e:Event) ON (e.type)",
        "CREATE INDEX IF NOT EXISTS FOR (e:Event) ON (e.timestamp)",
        "CREATE INDEX IF NOT EXISTS FOR (k:Knowledge) ON (k.project)",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosConfig) REQUIRE n.config_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosService) REQUIRE n.service_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosInstance) REQUIRE n.instance_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:Environment) REQUIRE n.name IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:NacosNamespace) REQUIRE n.namespace_id IS UNIQUE",
        "CREATE CONSTRAINT IF NOT EXISTS FOR (n:KubernetesCluster) REQUIRE n.name IS UNIQUE",
        "CREATE INDEX IF NOT EXISTS FOR (n:K8sPod) ON (n.ip)",
        "CREATE INDEX IF NOT EXISTS FOR (n:K8sPod) ON (n.name)",
        "CREATE INDEX IF NOT EXISTS FOR (n:Deployment) ON (n.name)",
        "CREATE INDEX IF NOT EXISTS FOR (n:K8sService) ON (n.name)",
        "CREATE INDEX IF NOT EXISTS FOR (n:NacosConfig) ON (n.data_id)",
        "CREATE INDEX IF NOT EXISTS FOR (n:NacosService) ON (n.name)",
    ];
    for cypher in &constraints {
        run_cypher_raw(cypher, json!({})).await?;
    }
    Ok(())
}

pub async fn write_methods_batch(methods: &[MethodNode]) -> Result<()> {
    if methods.is_empty() { return Ok(()); }
    let stmt = "\
UNWIND $methods AS m
MERGE (n:Method {method_id: m.method_id})
SET n.project = m.project,
    n.file_path = m.file_path,
    n.language = m.language,
    n.package_or_module = m.package_or_module,
    n.class_name = m.class_name,
    n.name = m.name,
    n.signature = m.signature,
    n.params = m.params,
    n.return_type = m.return_type,
    n.start_line = m.start_line,
    n.end_line = m.end_line,
    n.calls = m.calls";
    let methods_val: Vec<serde_json::Value> = methods.iter().map(|m| {
        json!({
            "method_id": m.method_id, "project": m.project, "file_path": m.file_path,
            "language": m.language, "package_or_module": m.package_or_module,
            "class_name": m.class_name, "name": m.name, "signature": m.signature,
            "params": m.params, "return_type": m.return_type,
            "start_line": m.start_line, "end_line": m.end_line, "calls": m.calls,
        })
    }).collect();
    run_cypher_raw(stmt, json!({"methods": methods_val})).await?;
    Ok(())
}

pub async fn write_classes_batch(classes: &[ClassBatchEntry]) -> Result<()> {
    if classes.is_empty() { return Ok(()); }
    let stmt = "\
UNWIND $classes AS c
MERGE (n:Class {class_id: c.class_id})
SET n.name = c.name,
    n.project = c.project,
    n.file_path = c.file_path,
    n.package_or_module = c.package_or_module
WITH n, c
UNWIND c.method_ids AS mid
MATCH (m:Method {method_id: mid})
MERGE (n)-[:CONTAINS]->(m)";
    let classes_val: Vec<serde_json::Value> = classes.iter().map(|c| {
        json!({
            "class_id": c.class_id, "name": c.name, "project": c.project,
            "file_path": c.file_path, "package_or_module": c.package_or_module,
            "method_ids": c.method_ids,
        })
    }).collect();
    run_cypher_raw(stmt, json!({"classes": classes_val})).await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassBatchEntry {
    pub class_id: String,
    pub name: String,
    pub project: String,
    pub file_path: String,
    pub package_or_module: String,
    pub method_ids: Vec<String>,
}

pub async fn delete_methods_by_file(project: &str, file_path: &str) -> Result<()> {
    let stmt = "\
MATCH (m:Method {project: $project, file_path: $file_path})
OPTIONAL MATCH (c:Class)-[r:CONTAINS]->(m)
DELETE r
WITH m
DETACH DELETE m";
    run_cypher_raw(stmt, json!({"project": project, "file_path": file_path})).await?;
    Ok(())
}

pub async fn delete_all_methods(project: &str) -> Result<()> {
    run_cypher_raw(
        "MATCH (m:Method {project: $project}) DETACH DELETE m",
        json!({"project": project}),
    ).await?;
    run_cypher_raw(
        "MATCH (c:Class) WHERE NOT (c)-[:CONTAINS]->() DELETE c",
        json!({}),
    ).await?;
    Ok(())
}

pub async fn create_call_relationships(project: &str) -> Result<u64> {
    let stmt = "\
MATCH (caller:Method {project: $project})
UNWIND caller.calls AS called_name
WITH caller, called_name
WHERE size(called_name) >= 3 AND called_name <> caller.name
MATCH (callee:Method {project: $project, name: called_name})
WHERE callee.method_id <> caller.method_id
MERGE (caller)-[:CALLS]->(callee)
RETURN count(*) AS created";
    let resp = run_cypher_raw(stmt, json!({"project": project})).await?;
    let count = resp["results"][0]["data"][0]["row"][0].as_i64().unwrap_or(0);
    Ok(count as u64)
}

pub async fn create_call_relationships_incremental(
    project: &str,
    file_paths: &[String],
) -> Result<u64> {
    if file_paths.is_empty() {
        return Ok(0);
    }
    let stmt = "\
MATCH (caller:Method {project: $project})
WHERE caller.file_path IN $file_paths
UNWIND caller.calls AS called_name
WITH caller, called_name
WHERE size(called_name) >= 3 AND called_name <> caller.name
MATCH (callee:Method {project: $project, name: called_name})
WHERE callee.method_id <> caller.method_id
MERGE (caller)-[:CALLS]->(callee)
RETURN count(*) AS created";
    let resp = run_cypher_raw(
        stmt,
        json!({"project": project, "file_paths": file_paths}),
    ).await?;
    let count = resp["results"][0]["data"][0]["row"][0].as_i64().unwrap_or(0);
    Ok(count as u64)
}

pub async fn health() -> Result<()> {
    let body = json!({"statements": [{"statement": "RETURN 1", "parameters": {}}]});
    let client = crate::client::get_client();
    let resp = client
        .post(neo4j_url())
        .header("Content-Type", "application/json")
        .header("Authorization", auth_header())
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Neo4j unavailable: {}", text));
    }
    Ok(())
}
