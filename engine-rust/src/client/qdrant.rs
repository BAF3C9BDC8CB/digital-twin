use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

use crate::config;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f64,
    pub payload: HashMap<String, serde_json::Value>,
}

fn qdrant_url() -> String {
    config::load().services.qdrant.url.trim_end_matches('/').to_string()
}

pub async fn ensure_collection(name: &str, dim: usize) -> Result<()> {
    let url = format!("{}/collections/{}", qdrant_url(), name);
    let client = crate::client::get_client();
    if client.get(&url).send().await?.status().is_success() {
        return Ok(());
    }
    let body = json!({
        "name": name,
        "vectors": { "size": dim, "distance": "Cosine" },
        "hnsw_config": { "m": 16, "ef_construct": 100 },
        "optimizers_config": { "indexing_threshold": 10000 }
    });
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        if !text.contains("already exists") {
            return Err(anyhow!("Failed to create collection: {}", text));
        }
    }
    Ok(())
}

pub async fn delete_collection(name: &str) -> Result<()> {
    let url = format!("{}/collections/{}", qdrant_url(), name);
    let client = crate::client::get_client();
    let resp = client.delete(&url).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Failed to delete collection: {}", text));
    }
    Ok(())
}

pub async fn upsert_points(
    collection: &str,
    points: Vec<(serde_json::Value, Vec<f32>, HashMap<String, serde_json::Value>)>,
) -> Result<()> {
    let qdrant_points: Vec<serde_json::Value> = points.into_iter()
        .map(|(id, vector, payload)| json!({"id": id, "vector": vector, "payload": payload}))
        .collect();
    let body = json!({"points": qdrant_points});
    let url = format!("{}/collections/{}/points", qdrant_url(), collection);
    let client = crate::client::get_client();
    let resp = client
        .put(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Upsert failed: {}", text));
    }
    Ok(())
}

pub async fn delete_points_by_filter(collection: &str, file_path: &str) -> Result<()> {
    let body = json!({
        "filter": {
            "must": [{ "key": "file_path", "match": { "value": file_path } }]
        }
    });
    let url = format!("{}/collections/{}/points/delete", qdrant_url(), collection);
    let client = crate::client::get_client();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Failed to delete points: {}", text));
    }
    Ok(())
}

pub async fn search(collection: &str, vector: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>> {
    let body = json!({"vector": vector, "limit": limit, "with_payload": true});
    let url = format!("{}/collections/{}/points/search", qdrant_url(), collection);
    let client = crate::client::get_client();

    #[derive(Deserialize)]
    struct QdrantResp { result: Vec<QdrantPoint> }
    #[derive(Deserialize)]
    struct QdrantPoint { id: serde_json::Value, score: f64, payload: HashMap<String, serde_json::Value> }

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    let data: QdrantResp = resp.json().await?;
    Ok(data.result.into_iter().map(|p| SearchResult {
        id: format!("{}", p.id),
        score: p.score,
        payload: p.payload,
    }).collect())
}
