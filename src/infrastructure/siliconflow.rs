//! SiliconFlow OpenAI 兼容 HTTP 客户端。
//!
//! 与 SiliconFlow 的云 API 通信，用于文本 embedding、文档
//! rerank 与 chat completion。该 API 表面与 OpenAI 格式完全兼容。
//!
//! # 端点
//!
//! | Method   | Endpoint                     | Purpose        |
//! |----------|------------------------------|----------------|
//! | `POST`   | `/v1/embeddings`             | Embedding      |
//! | `POST`   | `/v1/rerank`                 | Document rerank|
//! | `POST`   | `/v1/chat/completions`       | Chat completion|
//! | `GET`    | `/v1/models`                 | Health check   |
//!
//! # 速率限制
//!
//! SiliconFlow 强制执行 TPM（每分钟 token 数）限制。HTTP 429 与 503
//! 响应会以指数退避自动重试。

use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// 受速率限制请求的最大重试次数。
const MAX_RETRIES: u32 = 3;
const REQUEST_DEADLINE_SECS: u64 = 180;

/// 指数退避的基础延迟（毫秒）（1s、2s、4s）。
const RETRY_BASE_DELAY_MS: u64 = 1000;

/// Failures safe to retry without replaying permanent client errors.
pub fn is_transient_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Parse Retry-After as delta-seconds (HTTP-date is intentionally left to the
/// caller because provider clocks are not guaranteed to be synchronized).
pub fn retry_after_delay(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or_else(|| {
        let exponential = RETRY_BASE_DELAY_MS.saturating_mul(1u64 << attempt.min(6));
        // Small deterministic jitter avoids synchronized workers without using
        // a process-wide rate-limit claim.
        Duration::from_millis(exponential + ((attempt as u64 * 37) % 101))
    })
}

/// 默认模型名（可通过环境变量覆盖）。
const DEFAULT_EMBED_MODEL: &str = "BAAI/bge-m3";
const DEFAULT_RERANKER_MODEL: &str = "BAAI/bge-reranker-v2-m3";

/// 从环境变量检测代理。
///
/// 优先级：
/// 1. `SILICONFLOW_PROXY` 环境变量
/// 2. 标准的 `HTTPS_PROXY` / `HTTP_PROXY` 环境变量
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

/// 从环境变量检测代理 URL。
fn detect_proxy() -> Option<String> {
    // 优先级 1：显式环境变量
    if let Ok(url) = std::env::var("SILICONFLOW_PROXY") {
        if !url.is_empty() {
            return Some(url);
        }
    }

    // 优先级 2：标准代理环境变量
    std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .or_else(|_| std::env::var("http_proxy"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// 从环境变量 `SILICONFLOW_EMBED_MODEL` 或默认值返回 embed 模型名。
pub fn embed_model_from_env() -> String {
    std::env::var("SILICONFLOW_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
}

/// 从环境变量 `SILICONFLOW_RERANKER_MODEL` 或默认值返回 reranker 模型名。
pub fn reranker_model_from_env() -> String {
    std::env::var("SILICONFLOW_RERANKER_MODEL")
        .unwrap_or_else(|_| DEFAULT_RERANKER_MODEL.to_string())
}

/// 从环境变量 `SILICONFLOW_LLM_MODEL` 或空串返回 LLM 模型名。
pub fn llm_model_from_env() -> String {
    std::env::var("SILICONFLOW_LLM_MODEL").unwrap_or_default()
}

/// 从环境变量 `SILICONFLOW_API_KEY` 或空串返回 API key。
pub fn api_key_from_env() -> String {
    std::env::var("SILICONFLOW_API_KEY").unwrap_or_default()
}

/// 从环境变量 `SILICONFLOW_BASE_URL` 或默认值返回基础 URL。
pub fn base_url_from_env() -> String {
    std::env::var("SILICONFLOW_BASE_URL")
        .unwrap_or_else(|_| "https://api.siliconflow.cn/v1".to_string())
}

// ---------------------------------------------------------------------------
// SiliconFlowClient
// ---------------------------------------------------------------------------

/// SiliconFlow OpenAI 兼容云 API 的 HTTP 客户端。
///
/// 为 embedding、rerank 与 chat completion 提供类型化方法。
/// 所有请求都携带 `Authorization: Bearer <api_key>` 以完成认证。
///
/// # 并发
///
/// 与本地推理服务器不同，SiliconFlow 的云 API 原生支持
/// 并发请求——无需信号量。
pub struct SiliconFlowClient {
    /// 带 keep-alive 连接池的共享 reqwest HTTP 客户端。
    http: reqwest::Client,
    /// SiliconFlow API 的基础 URL（如 `https://api.siliconflow.cn/v1`）。
    base_url: String,
    /// 用于 Bearer 认证的 API key。
    api_key: String,
    /// Embedding 模型名（如 `"BAAI/bge-m3"`）。
    model_embed: String,
    /// Reranker 模型名（如 `"BAAI/bge-reranker-v2-m3"`）。
    model_reranker: String,
    /// Chat / LLM 模型名（如 `"Qwen/Qwen3-8B"`）。
    model_llm: String,
    semaphore: Arc<Semaphore>,
}

impl SiliconFlowClient {
    /// 创建新的 `SiliconFlowClient`。
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model_embed: impl Into<String>,
        model_reranker: impl Into<String>,
        model_llm: impl Into<String>,
        max_concurrent: usize,
    ) -> Self {
        Self {
            http: build_http_client(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            model_embed: model_embed.into(),
            model_reranker: model_reranker.into(),
            model_llm: model_llm.into(),
            semaphore: Arc::new(Semaphore::new(max_concurrent.max(1))),
        }
    }

    // -----------------------------------------------------------------------
    // 内部辅助函数
    // -----------------------------------------------------------------------

    /// 为给定路径构建一个带认证的 POST 请求。
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
    }

    /// 带速率限制重试逻辑地执行一个请求。
    async fn request_with_retry(
        &self,
        req: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<reqwest::Response, DtError> {
        let deadline = Instant::now() + Duration::from_secs(REQUEST_DEADLINE_SECS);
        let mut last_error = String::new();
        let mut server_delay: Option<Duration> = None;

        for attempt in 0..=MAX_RETRIES {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if attempt > 0 {
                let delay = retry_delay(attempt - 1, server_delay.take());
                tracing::warn!(
                    "SiliconFlow {} 第 {}/{} 次尝试失败: {}，{:?} 后重试",
                    operation,
                    attempt,
                    MAX_RETRIES,
                    last_error,
                    delay
                );
                tokio::time::sleep(delay.min(remaining)).await;
            }

            // 仅在实际发送时占用 permit，退避等待期间释放并发槽位。
            let _permit =
                self.semaphore.acquire().await.map_err(|e| {
                    DtError::Network(format!("SiliconFlow 并发信号量获取失败: {e}"))
                })?;

            // 每次构建一个全新的请求（reqwest::RequestBuilder 不可 Clone）
            let req_built = req
                .try_clone()
                .ok_or_else(|| DtError::Repository("SiliconFlow: 请求克隆失败".into()))?;

            match tokio::time::timeout(remaining, req_built.send()).await {
                Ok(Ok(resp)) => {
                    let status = resp.status();

                    // 成功
                    if status.is_success() {
                        return Ok(resp);
                    }

                    server_delay = retry_after_delay(resp.headers());
                    let body = resp.text().await.unwrap_or_default();

                    // 速率受限——可重试
                    if is_transient_status(status) {
                        last_error = format!(
                            "HTTP {}: {}",
                            status,
                            body.chars().take(200).collect::<String>()
                        );
                        continue;
                    }

                    // 其他错误——不可重试
                    return Err(DtError::Repository(format!(
                        "SiliconFlow {} 错误 ({}): {}",
                        operation, status, body
                    )));
                }
                Ok(Err(e)) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {}", e);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "SiliconFlow {} 请求失败: {}",
                        operation, e
                    )));
                }
                Err(_) => {
                    last_error = format!("请求超过总 deadline {} 秒", REQUEST_DEADLINE_SECS);
                    break;
                }
            }
        }

        Err(DtError::Repository(format!(
            "SiliconFlow {} 重试 {} 次后仍失败: {}",
            operation, MAX_RETRIES, last_error
        )))
    }

    // -----------------------------------------------------------------------
    // Rerank 重排序
    // -----------------------------------------------------------------------

    /// 使用配置的 reranker 模型对 `query` 重新排序 `documents`。
    ///
    /// 按原始输入顺序返回每个文档的相关性得分。
    /// 得分通常在 `[0, 1]`，越高表示越相关。
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
            DtError::Repository("SiliconFlow: rerank 响应中缺少 'results'".into())
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
    // Chat 对话
    // -----------------------------------------------------------------------

    /// 发送带有标准 system/user 提示词的 chat completion 请求。
    ///
    /// 返回第一个 choice 的文本内容。
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
            .map_err(|e| DtError::Repository(format!("SiliconFlow chat 解析: {e}")))?;

        let msg = &json["choices"][0]["message"];

        // 某些推理模型（如 Qwen3.5-9B）把实际响应放在
        // `reasoning_content` 里，而 `content` 为空。先尝试 `content`，
        // 再回退到 `reasoning_content`。
        let content = msg["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| msg["reasoning_content"].as_str().filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                DtError::Repository("SiliconFlow: chat 响应中缺少 content/reasoning_content".into())
            })?;

        Ok(content.to_string())
    }
}

// ---------------------------------------------------------------------------
// EmbedService trait 实现
// ---------------------------------------------------------------------------

#[async_trait]
impl EmbedService for SiliconFlowClient {
    /// 通过 `POST /v1/embeddings` 为一批文本生成 embedding。
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
            .map_err(|e| DtError::Repository(format!("SiliconFlow embed 解析: {e}")))?;

        let data = json["data"]
            .as_array()
            .ok_or_else(|| DtError::Repository("SiliconFlow: embed 响应中缺少 'data'".into()))?;

        let mut embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for item in data {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let embedding: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| DtError::Repository("SiliconFlow: 响应中缺少 'embedding'".into()))?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();
            embeddings.push((index, embedding));
        }

        embeddings.sort_by_key(|(idx, _)| *idx);
        Ok(embeddings.into_iter().map(|(_, v)| v).collect())
    }

    /// 通过 `GET /v1/models` 列出模型以检查服务健康。
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
                "SiliconFlow 健康检查: HTTP {}",
                resp.status()
            ))),
            Err(e) => Ok(HealthStatus::Unhealthy(format!(
                "SiliconFlow 健康检查: {e}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmService / RerankService trait 实现
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
        // 委托给已有的 chat() 方法
        SiliconFlowClient::chat(self, system_prompt, user_prompt, temperature, max_tokens).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        // 委托给已有的 EmbedService::health_check
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
        // 委托给已有的 rerank() 方法
        SiliconFlowClient::rerank(self, query, documents).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        <Self as EmbedService>::health_check(self).await
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_classifies_status_and_retry_after() {
        assert!(is_transient_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_transient_status(reqwest::StatusCode::BAD_REQUEST));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after_delay(&headers), Some(Duration::from_secs(7)));
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "not-a-duration".parse().unwrap(),
        );
        assert_eq!(retry_after_delay(&headers), None);
    }

    #[test]
    fn new_sets_fields() {
        let client = SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-test-key",
            "BAAI/bge-m3",
            "BAAI/bge-reranker-v2-m3",
            "Qwen/Qwen3-8B",
            20,
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
            20,
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
            20,
        );
        // 通过内部方法检查 URL 构造
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
            20,
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
            20,
        );
        assert!(!no_llm.capabilities().chat);
    }
}
