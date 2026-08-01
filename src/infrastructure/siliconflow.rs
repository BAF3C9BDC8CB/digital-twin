//! SiliconFlow OpenAI-compatible HTTP client.
//!
//! Communicates with SiliconFlow's cloud API for text embeddings, document
//! reranking, and chat completions. The API surface is fully compatible
//! with the OpenAI format.
//!
//! # Endpoints
//!
//! | Method   | Endpoint                     | Purpose        |
//! |----------|------------------------------|----------------|
//! | `POST`   | `/v1/embeddings`             | Embedding      |
//! | `POST`   | `/v1/rerank`                 | Document rerank|
//! | `POST`   | `/v1/chat/completions`       | Chat completion|
//! | `GET`    | `/v1/models`                 | Health check   |
//!
//! # Rate Limiting
//!
//! SiliconFlow enforces TPM (tokens per minute) limits. HTTP 429 and 503
//! responses are retried automatically with exponential backoff.

use async_trait::async_trait;
use std::time::Duration;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// Maximum number of retry attempts for rate-limited requests.
const MAX_RETRIES: u32 = 3;

/// Base delay in ms for exponential backoff (1s, 2s, 4s).
const RETRY_BASE_DELAY_MS: u64 = 1000;

/// Default model names (overridable via environment variables).
const DEFAULT_EMBED_MODEL: &str = "BAAI/bge-m3";
const DEFAULT_RERANKER_MODEL: &str = "BAAI/bge-reranker-v2-m3";

/// Detect proxy from environment variables.
///
/// Priority:
/// 1. `SILICONFLOW_PROXY` env var
/// 2. Standard `HTTPS_PROXY` / `HTTP_PROXY` env vars
fn build_http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(120));

    let proxy_url = detect_proxy();

    if let Some(url) = proxy_url {
        if let Ok(proxy) = reqwest::Proxy::all(&url) {
            builder = builder.proxy(proxy);
        }
    }

    builder.build().unwrap_or_default()
}

/// Detect proxy URL from environment variables.
fn detect_proxy() -> Option<String> {
    // Priority 1: explicit env var
    if let Ok(url) = std::env::var("SILICONFLOW_PROXY") {
        if !url.is_empty() {
            return Some(url);
        }
    }

    // Priority 2: standard proxy env vars
    std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .or_else(|_| std::env::var("http_proxy"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// Return the embed model name from env `SILICONFLOW_EMBED_MODEL` or default.
pub fn embed_model_from_env() -> String {
    std::env::var("SILICONFLOW_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
}

/// Return the reranker model name from env `SILICONFLOW_RERANKER_MODEL` or default.
pub fn reranker_model_from_env() -> String {
    std::env::var("SILICONFLOW_RERANKER_MODEL")
        .unwrap_or_else(|_| DEFAULT_RERANKER_MODEL.to_string())
}

/// Return the LLM model name from env `SILICONFLOW_LLM_MODEL` or empty.
pub fn llm_model_from_env() -> String {
    std::env::var("SILICONFLOW_LLM_MODEL").unwrap_or_default()
}

/// Return the API key from env `SILICONFLOW_API_KEY` or empty.
pub fn api_key_from_env() -> String {
    std::env::var("SILICONFLOW_API_KEY").unwrap_or_default()
}

/// Return the base URL from env `SILICONFLOW_BASE_URL` or default.
pub fn base_url_from_env() -> String {
    std::env::var("SILICONFLOW_BASE_URL")
        .unwrap_or_else(|_| "https://api.siliconflow.cn/v1".to_string())
}

// ---------------------------------------------------------------------------
// SiliconFlowClient
// ---------------------------------------------------------------------------

/// HTTP client for SiliconFlow's OpenAI-compatible cloud API.
///
/// Provides typed methods for embeddings, reranking, and chat completions.
/// All requests include `Authorization: Bearer <api_key>` for authentication.
///
/// # Concurrency
///
/// Unlike the local inference server, SiliconFlow's cloud API handles
/// concurrent requests natively — no semaphore is needed.
pub struct SiliconFlowClient {
    /// Shared reqwest HTTP client with keep-alive connection pool.
    http: reqwest::Client,
    /// Base URL of the SiliconFlow API (e.g. `https://api.siliconflow.cn/v1`).
    base_url: String,
    /// API key for Bearer authentication.
    api_key: String,
    /// Embedding model name (e.g. `"BAAI/bge-m3"`).
    model_embed: String,
    /// Reranker model name (e.g. `"BAAI/bge-reranker-v2-m3"`).
    model_reranker: String,
    /// Chat / LLM model name (e.g. `"Qwen/Qwen3-8B"`).
    model_llm: String,
}

impl SiliconFlowClient {
    /// Create a new `SiliconFlowClient`.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_embed: impl Into<String>,
        model_reranker: impl Into<String>,
        model_llm: impl Into<String>,
    ) -> Self {
        Self {
            http: build_http_client(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model_embed: model_embed.into(),
            model_reranker: model_reranker.into(),
            model_llm: model_llm.into(),
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Build an authenticated POST request for the given path.
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
    }

    /// Execute a request with retry logic for rate limiting.
    async fn request_with_retry(
        &self,
        req: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<reqwest::Response, DtError> {
        let mut last_error = String::new();

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let delay = Duration::from_millis(RETRY_BASE_DELAY_MS * (1 << (attempt - 1)));
                tracing::warn!(
                    "SiliconFlow {} 第 {}/{} 次尝试失败: {}，{:?} 后重试",
                    operation,
                    attempt,
                    MAX_RETRIES,
                    last_error,
                    delay
                );
                tokio::time::sleep(delay).await;
            }

            // Build a fresh request each time (reqwest::RequestBuilder is not Clone)
            let req_built = req.try_clone().ok_or_else(|| {
                DtError::Repository("SiliconFlow: failed to clone request".into())
            })?;

            match req_built.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    // Success
                    if status.is_success() {
                        return Ok(resp);
                    }

                    let body = resp.text().await.unwrap_or_default();

                    // Rate limited — retryable
                    if status.as_u16() == 429 || status.as_u16() == 503 {
                        last_error = format!("HTTP {}: {}", status, &body[..body.len().min(200)]);
                        continue;
                    }

                    // Other errors — not retryable
                    return Err(DtError::Repository(format!(
                        "SiliconFlow {} error ({}): {}",
                        operation, status, body
                    )));
                }
                Err(e) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {}", e);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "SiliconFlow {} request failed: {}",
                        operation, e
                    )));
                }
            }
        }

        Err(DtError::Repository(format!(
            "SiliconFlow {} failed after {} retries: {}",
            operation, MAX_RETRIES, last_error
        )))
    }

    // -----------------------------------------------------------------------
    // Rerank
    // -----------------------------------------------------------------------

    /// Rerank `documents` against `query` using the configured reranker model.
    ///
    /// Returns a relevance score for each document, in the original input order.
    /// Scores are typically in `[0, 1]` where higher means more relevant.
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
            .map_err(|e| DtError::Repository(format!("SiliconFlow rerank parse: {e}")))?;

        let results = json["results"].as_array().ok_or_else(|| {
            DtError::Repository("SiliconFlow: missing 'results' in rerank response".into())
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

    // -----------------------------------------------------------------------
    // Chat
    // -----------------------------------------------------------------------

    /// Send a chat completion request with standard system/user prompts.
    ///
    /// Returns the text content of the first choice.
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
            .map_err(|e| DtError::Repository(format!("SiliconFlow chat parse: {e}")))?;

        let msg = &json["choices"][0]["message"];

        // Some reasoning models (e.g. Qwen3.5-9B) put the actual response in
        // `reasoning_content` and leave `content` empty.  Try `content` first,
        // then fall back to `reasoning_content`.
        let content = msg["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| msg["reasoning_content"].as_str().filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                DtError::Repository(
                    "SiliconFlow: missing content/reasoning_content in chat response".into(),
                )
            })?;

        Ok(content.to_string())
    }
}

// ---------------------------------------------------------------------------
// EmbedService trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl EmbedService for SiliconFlowClient {
    /// Generate embeddings for a batch of texts via `POST /v1/embeddings`.
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
            .map_err(|e| DtError::Repository(format!("SiliconFlow embed parse: {e}")))?;

        let data = json["data"].as_array().ok_or_else(|| {
            DtError::Repository("SiliconFlow: missing 'data' in embed response".into())
        })?;

        let mut embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for item in data {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let embedding: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| {
                    DtError::Repository("SiliconFlow: missing 'embedding' in response".into())
                })?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            embeddings.push((index, embedding));
        }

        embeddings.sort_by_key(|(idx, _)| *idx);
        Ok(embeddings.into_iter().map(|(_, v)| v).collect())
    }

    /// Check service health by listing models via `GET /v1/models`.
    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));

        match self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => Ok(HealthStatus::Healthy),
            Ok(resp) => Ok(HealthStatus::Unhealthy(format!(
                "SiliconFlow health: HTTP {}",
                resp.status()
            ))),
            Err(e) => Ok(HealthStatus::Unhealthy(format!("SiliconFlow health: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmService / RerankService trait implementations
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmService for SiliconFlowClient {
    async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError> {
        // Delegate to existing chat() method
        SiliconFlowClient::chat(self, system_prompt, user_prompt, temperature, max_tokens).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        // Delegate to existing EmbedService::health_check
        <Self as EmbedService>::health_check(self).await
    }

    fn capabilities(&self) -> LlmCapabilities {
        LlmCapabilities {
            embed: true,
            rerank: true,
            chat: !self.model_llm.is_empty(),
            max_tokens: 4096,
        }
    }
}

#[async_trait]
impl RerankService for SiliconFlowClient {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        // Delegate to existing rerank() method
        SiliconFlowClient::rerank(self, query, documents).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        <Self as EmbedService>::health_check(self).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_fields() {
        let client = SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-test-key",
            "BAAI/bge-m3",
            "BAAI/bge-reranker-v2-m3",
            "Qwen/Qwen3-8B",
        );
        assert_eq!(client.base_url, "https://api.siliconflow.cn/v1");
        assert_eq!(client.api_key, "sk-test-key");
        assert_eq!(client.model_embed, "BAAI/bge-m3");
        assert_eq!(client.model_reranker, "BAAI/bge-reranker-v2-m3");
        assert_eq!(client.model_llm, "Qwen/Qwen3-8B");
    }

    #[test]
    fn client_is_send_sync() {
        fn assert_send<T: Send>(_t: &T) {}
        fn assert_sync<T: Sync>(_t: &T) {}

        let client = SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-test-key",
            "BAAI/bge-m3",
            "BAAI/bge-reranker-v2-m3",
            "Qwen/Qwen3-8B",
        );
        assert_send(&client);
        assert_sync(&client);
    }

    #[test]
    fn post_builds_correct_url() {
        let client = SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "llm",
        );
        // Check the URL construction via the internal method
        let req = client.post("/embeddings");
        let built = req.build().unwrap();
        assert_eq!(
            built.url().as_str(),
            "https://api.siliconflow.cn/v1/embeddings"
        );
        assert_eq!(
            built
                .headers()
                .get("Authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer sk-key"
        );
    }

    #[test]
    fn capabilities_reflects_model_llm_config() {
        use crate::domain::traits::LlmService;
        let with_llm = SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "Qwen/Qwen3-8B",
        );
        let caps = with_llm.capabilities();
        assert!(caps.embed);
        assert!(caps.rerank);
        assert!(caps.chat);
        assert_eq!(caps.max_tokens, 4096);

        let no_llm = SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "",
        );
        assert!(!no_llm.capabilities().chat);
    }
}
