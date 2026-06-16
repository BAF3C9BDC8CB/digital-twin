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
    let client = crate::client::get_client();
    let resp: HealthResp = client
        .get(format!("{}/health", cfg.services.embed_server.url))
        .send().await?.json().await?;
    Ok((resp.model, resp.dim))
}

pub async fn embed_batch(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let cfg = config::load();
    let client = crate::client::get_client();
    let resp: EmbedBatchResp = client
        .post(format!("{}/embed-batch", cfg.services.embed_server.url))
        .json(&EmbedBatchReq { texts })
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.vectors)
}
