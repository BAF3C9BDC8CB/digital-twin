use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Serialize)]
struct EmbedBatchReq { texts: Vec<String> }

#[derive(Deserialize)]
struct EmbedBatchResp { vectors: Vec<Vec<f32>>, dim: usize }

pub async fn health() -> Result<(String, usize)> {
    let cfg = config::load();
    #[derive(Deserialize)]
    struct HealthResp { status: String, model: String, dim: usize }
    let resp: HealthResp = reqwest::get(format!("{}/health", cfg.services.embed_server.url))
        .await?.json().await?;
    Ok((resp.model, resp.dim))
}

pub async fn embed_batch(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let cfg = config::load();
    let client = reqwest::Client::new();
    let resp: EmbedBatchResp = client
        .post(format!("{}/embed-batch", cfg.services.embed_server.url))
        .json(&EmbedBatchReq { texts })
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.vectors)
}
