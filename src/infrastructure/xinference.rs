//! XInference client — OpenAI-compatible local inference server.
//!
//! XInference is a local model serving framework that exposes an
//! OpenAI-compatible API. This client reuses the same HTTP protocol
//! as SiliconFlowClient but is configured for local deployment.
//!
//! # Capabilities
//!
//! XInference typically supports:
//! - Embedding (BAAI/bge-m3) ✅
//! - Reranking (BAAI/bge-reranker-v2-m3) ✅
//! - LLM chat (Qwen3-14B) — optional, depends on local deployment
//!
//! When LLM is not available, `capabilities().chat = false` and
//! LLM analysis is gracefully skipped.

use async_trait::async_trait;
use std::time::Duration;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// HTTP client for XInference's OpenAI-compatible local API.
///
/// Structurally identical to SiliconFlowClient but configured for
/// local deployment (no API key, configurable model names).
pub struct XInferenceClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model_embed: String,
    model_reranker: String,
    model_llm: String,
}

impl XInferenceClient {
    /// Create a new XInferenceClient.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_embed: impl Into<String>,
        model_reranker: impl Into<String>,
        model_llm: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model_embed: model_embed.into(),
            model_reranker: model_reranker.into(),
            model_llm: model_llm.into(),
        }
    }

    /// Build an authenticated POST request (api_key optional for local).
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", "application/json");
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        req
    }

    /// Execute a request with retry logic.
    async fn request_with_retry(
        &self,
        req: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<reqwest::Response, DtError> {
        let max_retries = 3u32;
        let mut last_error = String::new();

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(1000 * (1 << (attempt - 1)));
                tracing::warn!(
                    "XInference {} 第 {}/{} 次尝试失败: {}，{:?} 后重试",
                    operation,
                    attempt,
                    max_retries,
                    last_error,
                    delay
                );
                tokio::time::sleep(delay).await;
            }

            let req_built = req
                .try_clone()
                .ok_or_else(|| DtError::Repository("XInference: failed to clone request".into()))?;

            match req_built.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp);
                    }
                    let body = resp.text().await.unwrap_or_default();
                    if status.as_u16() == 429 || status.as_u16() == 503 {
                        last_error = format!("HTTP {}: {}", status, &body[..body.len().min(200)]);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "XInference {} error ({}): {}",
                        operation, status, body
                    )));
                }
                Err(e) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {}", e);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "XInference {} request failed: {}",
                        operation, e
                    )));
                }
            }
        }

        Err(DtError::Repository(format!(
            "XInference {} failed after {} retries: {}",
            operation, max_retries, last_error
        )))
    }

    /// Chat completion (delegates to same OpenAI format as SiliconFlow).
    pub async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError> {
        let body = serde_json::json!({
            "model": self.model_llm,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });

        let resp = self
            .request_with_retry(self.post("/chat/completions").json(&body), "chat")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("XInference chat parse: {e}")))?;

        let msg = &json["choices"][0]["message"];
        let content = msg["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| msg["reasoning_content"].as_str().filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                DtError::Repository("XInference: missing content in chat response".into())
            })?;

        Ok(content.to_string())
    }

    /// Rerank documents against a query.
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        let body = serde_json::json!({
            "model": self.model_reranker,
            "query": query,
            "documents": documents,
            "return_documents": false,
        });

        let resp = self
            .request_with_retry(self.post("/rerank").json(&body), "rerank")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("XInference rerank parse: {e}")))?;

        let results = json["results"].as_array().ok_or_else(|| {
            DtError::Repository("XInference: missing 'results' in rerank response".into())
        })?;

        let mut scores: Vec<(usize, f32)> = Vec::with_capacity(results.len());
        for item in results {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let score = item["relevance_score"].as_f64().unwrap_or(0.0) as f32;
            scores.push((index, score));
        }

        scores.sort_by_key(|(idx, _)| *idx);
        Ok(scores.into_iter().map(|(_, s)| s).collect())
    }
}

#[async_trait]
impl EmbedService for XInferenceClient {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let body = serde_json::json!({
            "model": self.model_embed,
            "input": texts,
            "encoding_format": "float",
        });

        let resp = self
            .request_with_retry(self.post("/embeddings").json(&body), "embed")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("XInference embed parse: {e}")))?;

        let data = json["data"].as_array().ok_or_else(|| {
            DtError::Repository("XInference: missing 'data' in embed response".into())
        })?;

        let mut embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for item in data {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let embedding: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| {
                    DtError::Repository("XInference: missing 'embedding' in response".into())
                })?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            embeddings.push((index, embedding));
        }

        embeddings.sort_by_key(|(idx, _)| *idx);
        Ok(embeddings.into_iter().map(|(_, v)| v).collect())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => Ok(HealthStatus::Healthy),
            Ok(resp) => Ok(HealthStatus::Unhealthy(format!(
                "XInference health: HTTP {}",
                resp.status()
            ))),
            Err(e) => Ok(HealthStatus::Unhealthy(format!("XInference health: {e}"))),
        }
    }
}

#[async_trait]
impl LlmService for XInferenceClient {
    async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError> {
        XInferenceClient::chat(self, system_prompt, user_prompt, temperature, max_tokens).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        <Self as EmbedService>::health_check(self).await
    }

    fn capabilities(&self) -> LlmCapabilities {
        LlmCapabilities {
            embed: !self.model_embed.is_empty(),
            rerank: !self.model_reranker.is_empty(),
            chat: !self.model_llm.is_empty(),
            max_tokens: 4096,
        }
    }
}

#[async_trait]
impl RerankService for XInferenceClient {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        XInferenceClient::rerank(self, query, documents).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        <Self as EmbedService>::health_check(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_fields() {
        let client = XInferenceClient::new(
            "http://localhost:9997/v1",
            "",
            "BAAI/bge-m3",
            "BAAI/bge-reranker-v2-m3",
            "", // no LLM
        );
        assert_eq!(client.base_url, "http://localhost:9997/v1");
        assert_eq!(client.model_embed, "BAAI/bge-m3");
        assert!(client.model_llm.is_empty());
    }

    #[test]
    fn capabilities_reflect_model_config() {
        let full =
            XInferenceClient::new("http://localhost:9997/v1", "", "bge-m3", "reranker", "qwen");
        let caps = full.capabilities();
        assert!(caps.embed);
        assert!(caps.rerank);
        assert!(caps.chat);

        let no_llm =
            XInferenceClient::new("http://localhost:9997/v1", "", "bge-m3", "reranker", "");
        let caps = no_llm.capabilities();
        assert!(caps.embed);
        assert!(caps.rerank);
        assert!(!caps.chat); // LLM disabled
    }

    #[test]
    fn client_is_send_sync() {
        fn assert_send<T: Send>(_t: &T) {}
        fn assert_sync<T: Sync>(_t: &T) {}
        let client =
            XInferenceClient::new("http://localhost:9997/v1", "", "bge-m3", "reranker", "");
        assert_send(&client);
        assert_sync(&client);
    }
}
