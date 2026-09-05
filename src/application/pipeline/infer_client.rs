//! SiliconFlow 云 API（OpenAI 兼容）的 HTTP 客户端。
//!
//! 所有 LLM chat 与文本 embedding 请求都流经该客户端。
//!
//! # 并发
//!
//! 一个 [`Arc<Semaphore>`] 限制在途 HTTP 请求数，使流水线永远不会压垮
//! 推理服务器。

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

/// 默认的 SiliconFlow API URL。
const SILICONFLOW_DEFAULT_URL: &str = "https://api.siliconflow.cn/v1";

// ---------------------------------------------------------------------------
// 公开响应 / DTO 类型
// ---------------------------------------------------------------------------

/// OpenAI 兼容的 chat completion 响应。
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: Message,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    #[serde(deserialize_with = "deserialize_message_content")]
    pub content: String,
}

fn deserialize_message_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(s) => Some(s),
                serde_json::Value::Object(item) => {
                    item.get("text").and_then(|v| v.as_str()).map(str::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    })
}

// ---------------------------------------------------------------------------
// 请求体（私有——仅供内部使用）
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct EmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Debug, Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
}

// ---------------------------------------------------------------------------
// SiliconFlow 客户端
// ---------------------------------------------------------------------------

/// SiliconFlow 云 API（OpenAI 兼容）的 HTTP 客户端。
pub struct SiliconFlowChatClient {
    client: Client,
    base_url: String,
    api_key: String,
    semaphore: Arc<Semaphore>,
}

impl SiliconFlowChatClient {
    /// 构建一个以 `base_url` 为目标、支持最大并发请求数的新客户端。
    ///
    /// `api_key` 为空时回退到 `SILICONFLOW_API_KEY` 环境变量。
    pub fn new(base_url: String, api_key: String, max_concurrent: usize) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("reqwest::Client::builder() 不应失败");

        let api_key = if api_key.is_empty() {
            std::env::var("SILICONFLOW_API_KEY").unwrap_or_default()
        } else {
            api_key
        };

        Self {
            client,
            base_url: if base_url.is_empty() {
                SILICONFLOW_DEFAULT_URL.to_string()
            } else {
                base_url
            },
            api_key,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// 检查 SiliconFlow API 是否可达。
    pub async fn health_check(&self) -> Result<bool, String> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));

        match self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
        {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(e) => Err(format!("SiliconFlow 健康检查失败: {e}")),
        }
    }

    /// 向 SiliconFlow 发送 chat completion 请求（OpenAI 兼容）。
    pub async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, String> {
        let started = Instant::now();
        tracing::info!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, elapsed_ms = 0u128, stage = "request_start", "SiliconFlow request_start");
        tracing::info!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, elapsed_ms = started.elapsed().as_millis(), stage = "semaphore_wait_start", "SiliconFlow semaphore_wait_start");
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("信号量获取失败: {e}"))?;
        tracing::info!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, elapsed_ms = started.elapsed().as_millis(), stage = "semaphore_acquired", "SiliconFlow semaphore_acquired");

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let body = ChatRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: user_prompt.to_string(),
                },
            ],
            temperature,
            max_tokens,
            stream: false,
            enable_thinking: model
                .to_ascii_lowercase()
                .contains("deepseek-v3.2")
                .then_some(false),
        };

        tracing::info!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, elapsed_ms = started.elapsed().as_millis(), stage = "send_start", "SiliconFlow send_start");
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("SiliconFlow 对话请求失败: {e}"))?;

        let status = resp.status();
        tracing::info!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, status = %status, elapsed_ms = started.elapsed().as_millis(), stage = "response_received", "SiliconFlow response_received");
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, status = %status, response_body_bytes = text.len(), elapsed_ms = started.elapsed().as_millis(), retry_reason = "http_error", "SiliconFlow error (body omitted)");
            return Err(format!("SiliconFlow 对话返回 HTTP {status}"));
        }

        let parsed = resp
            .json::<ChatResponse>()
            .await
            .map_err(|e| format!("对话响应解析失败: {e}"));
        tracing::info!(task = "pipeline", run = "live", file = "unknown", chunk = "unknown", attempt = 0u32, provider = "siliconflow", model = %model, elapsed_ms = started.elapsed().as_millis(), total_ms = started.elapsed().as_millis(), stage = "request_end", "SiliconFlow request_end");
        parsed
    }

    /// 通过 POST /v1/embeddings 嵌入一批文本。
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("信号量获取失败: {e}"))?;

        let url = format!("{}/v1/embeddings", self.base_url);

        let body = EmbedRequest {
            model: "default".into(),
            input: texts.to_vec(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("embed 请求失败: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("embed 返回 HTTP {status}: {text}"));
        }

        let embed_resp = resp
            .json::<EmbedResponse>()
            .await
            .map_err(|e| format!("embed 响应解析失败: {e}"))?;

        Ok(embed_resp.data.into_iter().map(|d| d.embedding).collect())
    }
}

// ---------------------------------------------------------------------------
// 统一对话客户端 trait
// ---------------------------------------------------------------------------

/// 统一对话客户端 trait——由 SiliconFlow 与 OpenAI-Compatible 共同实现。
#[async_trait]
pub trait ChatClient: Send + Sync {
    async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        json_mode: bool,
    ) -> Result<ChatResponse, String>;
    async fn health_check(&self) -> Result<bool, String>;
}

#[async_trait]
impl ChatClient for SiliconFlowChatClient {
    async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        _json_mode: bool,
    ) -> Result<ChatResponse, String> {
        SiliconFlowChatClient::chat(
            self,
            model,
            system_prompt,
            user_prompt,
            temperature,
            max_tokens,
        )
        .await
    }
    async fn health_check(&self) -> Result<bool, String> {
        SiliconFlowChatClient::health_check(self).await
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_response_accepts_null_and_array_content() {
        let null: ChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"message": {"content": null}}]
        }))
        .unwrap();
        assert_eq!(null.choices[0].message.content, "");
        let parts: ChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"message": {"content": [{"type":"text","text":"a"},{"text":"b"}]}}]
        }))
        .unwrap();
        assert_eq!(parts.choices[0].message.content, "ab");
        let strings: ChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"message": {"content": ["a", "b"]}}]
        }))
        .unwrap();
        assert_eq!(strings.choices[0].message.content, "ab");
    }

    #[test]
    fn silicon_flow_chat_client_can_be_constructed() {
        let client = SiliconFlowChatClient::new(SILICONFLOW_DEFAULT_URL.into(), String::new(), 8);
        assert!(client.semaphore.available_permits() <= 16);
    }

    #[test]
    fn chat_response_roundtrip() {
        let json = r#"{"choices":[{"message":{"content":"Hello!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content, "Hello!");
    }
}
