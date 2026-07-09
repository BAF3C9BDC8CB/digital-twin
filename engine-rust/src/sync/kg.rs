use anyhow::Result;
use serde_json::json;
use sha2::Digest;
use std::collections::HashMap;

use crate::config;
use crate::client::{embed, neo4j, qdrant};

const KG_COLLECTION: &str = "kg_nodes";

const DEFAULT_SYNC_LABELS: &[&str] = &[
    "Infrastructure", "Server", "Database", "Project",
    "Environment", "Knowledge", "Software", "Configuration",
    "NacosConfig", "NacosService", "NacosNamespace",
];

const TEXT_PROPS: &[&str] = &[
    "name", "description", "service_type", "auth_user", "hostname",
    "host", "url", "source_file", "data_id", "service_name",
    "db_type", "db_label", "environment", "namespace", "group_name",
    "title", "content", "type", "stack", "root_path",
];

fn build_search_text(node: &serde_json::Value) -> String {
    let mut parts = Vec::new();

    let svc_type = node.get("service_type")
        .and_then(|v| v.as_str()).unwrap_or("");

    // 基础设施节点加标签关键词
    if matches!(svc_type, "mysql" | "mongodb" | "redis" | "kafka" | "elasticsearch" | "zookeeper") {
        parts.push("[数据库] [基础设施] [凭证] [账号] [密码]".to_string());
    } else if matches!(svc_type, "gitlab" | "jenkins" | "elk" | "nacos") {
        parts.push("[服务] [基础设施] [平台] [工具]".to_string());
    }

    for prop in TEXT_PROPS {
        if *prop == "name" { continue; }
        if let Some(val) = node.get(prop).and_then(|v| v.as_str()) {
            if !val.is_empty() {
                parts.push(val.to_string());
            }
        }
    }

    // 名称放最后（作为精确匹配信号）
    if let Some(name) = node.get("name").and_then(|v| v.as_str()) {
        parts.push(name.to_string());
    }

    parts.join(" ")
}

fn build_payload(node: &serde_json::Value, element_id: &str, labels: &[String]) -> HashMap<String, serde_json::Value> {
    let mut payload = HashMap::new();
    payload.insert("elementId".into(), json!(element_id));
    payload.insert("name".into(), node.get("name").cloned().unwrap_or(json!("")));
    payload.insert("labels".into(), json!(labels));
    payload.insert("service_type".into(), node.get("service_type").cloned().unwrap_or(json!("")));
    payload.insert("environment".into(), node.get("environment").cloned().unwrap_or(json!("")));
    payload.insert("description".into(), {
        let desc = node.get("description").and_then(|v| v.as_str()).unwrap_or("");
        json!(&desc[..desc.len().min(200)])
    });
    payload.insert("source".into(), json!("kg"));
    payload
}

pub async fn run_kg_sync(labels: Option<Vec<String>>, incremental: bool, dry_run: bool) -> Result<()> {
    embed::health().await?;
    neo4j::health().await?;

    let cfg = config::load();
    let dim = cfg.services.embed_server.dim;
    qdrant::ensure_collection(KG_COLLECTION, dim).await?;

    let sync_labels: Vec<String> = labels.unwrap_or_else(|| {
        DEFAULT_SYNC_LABELS.iter().map(|s| s.to_string()).collect()
    });

    // 构建标签条件
    let label_conds: Vec<String> = sync_labels.iter()
        .map(|l| format!("n:{}", l)).collect();
    let label_clause = label_conds.join(" OR ");

    let cypher = if incremental {
        format!("MATCH (n) WHERE ({}) AND (n._kg_synced_at IS NULL) RETURN n, elementId(n) AS eid, labels(n) AS lbls", label_clause)
    } else {
        format!("MATCH (n) WHERE ({}) RETURN n, elementId(n) AS eid, labels(n) AS lbls", label_clause)
    };

    eprintln!("[query] Fetching nodes...");
    let resp = neo4j::run_cypher_raw(&cypher, json!({})).await?;

    let rows: Vec<Vec<serde_json::Value>> = resp["results"][0]["data"]
        .as_array().unwrap_or(&vec![])
        .iter()
        .map(|d| d["row"].as_array().cloned().unwrap_or_default())
        .collect();

    if rows.is_empty() {
        eprintln!("[query] No nodes found to sync");
        return Ok(());
    }

    eprintln!("[query] Found {} nodes to sync", rows.len());

    if dry_run {
        for (i, row) in rows.iter().take(10).enumerate() {
            let node = &row[0];
            let labels: Vec<String> = row[2].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let text = &build_search_text(node).chars().take(100).collect::<String>();
            eprintln!("  [{}] [{:?}] {}", i + 1, labels, text);
        }
        eprintln!("  ... and {} more (dry-run)", rows.len().saturating_sub(10));
        return Ok(());
    }

    let batch_size = 64;
    let total = rows.len();
    let mut synced = 0usize;

    for chunk in rows.chunks(batch_size) {
        // 嵌入
        let texts: Vec<String> = chunk.iter()
            .map(|row| build_search_text(&row[0]))
            .collect();
        let vectors = embed::embed_batch(texts).await?;

        // 构建 Qdrant points
        let mut points: Vec<(serde_json::Value, Vec<f32>, HashMap<String, serde_json::Value>)> = Vec::new();
        for (i, row) in chunk.iter().enumerate() {
            let node = &row[0];
            let eid = row[1].as_str().unwrap_or("");
            let labels: Vec<String> = row[2].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            let payload = build_payload(node, eid, &labels);

            // 生成确定性 UUID 作为 point ID
            let hash = sha2::Sha256::digest(eid.as_bytes());
            let uuid_str = format!(
                "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
                u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]),
                u16::from_be_bytes([hash[4], hash[5]]),
                u16::from_be_bytes([hash[6], hash[7]]) & 0x0fff,
                u16::from_be_bytes([hash[8], hash[9]]) & 0x3fff | 0x8000,
                u64::from_be_bytes([
                    hash[10], hash[11], hash[12], hash[13],
                    hash[14], hash[15], 0, 0
                ]) >> 16,
            );

            points.push((json!(uuid_str), vectors[i].clone(), payload));
        }

        qdrant::upsert_points(KG_COLLECTION, points).await?;

        // 标记 Neo4j 节点已同步
        let eids: Vec<&str> = chunk.iter()
            .filter_map(|row| row[1].as_str())
            .collect();

        neo4j::run_cypher_raw(
            "UNWIND $eids AS eid MATCH (n) WHERE elementId(n) = eid SET n._kg_synced_at = datetime()",
            json!({"eids": eids}),
        ).await?;

        synced += chunk.len();
        eprintln!("[sync] {}/{} nodes synced...", synced, total);
    }

    eprintln!("[done] {} KG nodes synced to Qdrant {}", synced, KG_COLLECTION);
    Ok(())
}
