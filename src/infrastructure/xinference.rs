//! XInference 客户端——OpenAI 兼容的本地推理服务器。
//!
//! XInference 是一个本地模型服务框架，暴露 OpenAI 兼容 API。
//! 该客户端复用与 SiliconFlowClient 相同的 HTTP 协议，
//! 但配置为本地部署。
//!
//! # 能力
//!
//! XInference 通常支持：
//! - Embedding（BAAI/bge-m3）✅
//! - Reranking（BAAI/bge-reranker-v2-m3）✅
//! - LLM chat（Qwen3-14B）——可选，取决于本地部署
//!
//! 当 LLM 不可用时，`capabilities().chat = false`，
//! LLM 分析会被优雅跳过。

use async_trait::async_trait;
use std::time::Duration;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// XInference OpenAI 兼容本地 API 的 HTTP 客户端。
///
/// 结构与 SiliconFlowClient 相同，但配置为
/// 本地部署（无 API key，模型名可配置）。
pub struct XInferenceClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model_embed: String,
    model_reranker: String,
    model_llm: String,
}

impl XInferenceClient {
    /// 创建新的 XInferenceClient。
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

    /// 构建带认证的 POST 请求（api_key 对本地可选）。
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

    /// 带重试逻辑地执行一个请求。
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
                .ok_or_else(|| DtError::Repository("XInference: 请求克隆失败".into()))?;

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
                        "XInference {} 错误 ({}): {}",
                        operation, status, body
                    )));
                }
                Err(e) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {}", e);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "XInference {} 请求失败: {}",
                        operation, e
                    )));
                }
            }
        }

        Err(DtError::Repository(format!(
            "XInference {} 重试 {} 次后仍失败: {}",
            operation, max_retries, last_error
        )))
    }

    /// Chat completion（委托给与 SiliconFlow 相同的 OpenAI 格式）。
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
            .map_err(|e| DtError::Repository(format!("XInference chat 解析: {e}")))?;

        let msg = &json["choices"][0]["message"];
        let content = msg["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| msg["reasoning_content"].as_str().filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                DtError::Repository("XInference: chat 响应中缺少 content".into())
            })?;

        Ok(content.to_string())
    }

    /// 对查询重新排序文档。
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
            .map_err(|e| DtError::Repository(format!("XInference rerank 解析: {e}")))?;

        let results = json["results"].as_array().ok_or_else(|| {
            DtError::Repository("XInference: rerank 响应中缺少 'results'".into())
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
            .map_err(|e| DtError::Repository(format!("XInference embed 解析: {e}")))?;

        let data = json["data"].as_array().ok_or_else(|| {
            DtError::Repository("XInference: embed 响应中缺少 'data'".into())
        })?;

        let mut embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for item in data {
            let index = item["index"].as_i64().unwrap_or(0) as usize;
            let embedding: Vec<f32> = item["embedding"]
                .as_array()
                .ok_or_else(|| {
                    DtError::Repository("XInference: 响应中缺少 'embedding'".into())
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
                "XInference 健康检查: HTTP {}",
                resp.status()
            ))),
            Err(e) => Ok(HealthStatus::Unhealthy(format!("XInference 健康检查: {e}"))),
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
            "", // 无 LLM
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
        assert!(!caps.chat); // LLM 已禁用
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
