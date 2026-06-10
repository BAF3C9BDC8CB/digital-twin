use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

use crate::config;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f64,
    pub payload: HashMap<String, serde_json::Value>,
}

fn client_short() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build().unwrap()
}

fn client_long() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build().unwrap()
}

fn qdrant_url() -> String {
    config::load().services.qdrant.url.trim_end_matches('/').to_string()
}

pub async fn ensure_collection(name: &str, dim: usize) -> Result<()> {
    let url = format!("{}/collections/{}", qdrant_url(), name);
    if client_short().get(&url).send().await?.status().is_success() {
        return Ok(());
    }
    let body = json!({
        "name": name,
        "vectors": { "size": dim, "distance": "Cosine" },
        "hnsw_config": { "m": 16, "ef_construct": 100 },
        "optimizers_config": { "indexing_threshold": 10000 }
    });
    let resp = client_short()
        .put(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        if !text.contains("already exists") {
            return Err(anyhow!("创建集合失败: {}", text));
        }
    }
    Ok(())
}

pub async fn delete_collection(name: &str) -> Result<()> {
    let url = format!("{}/collections/{}", qdrant_url(), name);
    let resp = client_short().delete(&url).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("删除集合失败: {}", text));
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
    let resp = client_long()
        .put(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("upsert 失败: {}", text));
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
    let resp = client_short()
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("删除点失败: {}", text));
    }
    Ok(())
}

pub async fn search(collection: &str, vector: Vec<f32>, limit: usize) -> Result<Vec<SearchResult>> {
    let body = json!({"vector": vector, "limit": limit, "with_payload": true});
    let url = format!("{}/collections/{}/points/search", qdrant_url(), collection);
    let resp = client_short()
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send().await?;

    #[derive(Deserialize)]
    struct QdrantResp { result: Vec<QdrantPoint> }
    #[derive(Deserialize)]
    struct QdrantPoint { id: serde_json::Value, score: f64, payload: HashMap<String, serde_json::Value> }

    let data: QdrantResp = resp.json().await?;
    Ok(data.result.into_iter().map(|p| SearchResult {
        id: format!("{}", p.id),
        score: p.score,
        payload: p.payload,
    }).collect())
}
