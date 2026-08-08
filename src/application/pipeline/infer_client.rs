//! SiliconFlow 云 API（OpenAI 兼容）与 XInference 的 HTTP 客户端。
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
    pub content: String,
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
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("信号量获取失败: {e}"))?;

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
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("SiliconFlow 对话请求失败: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("SiliconFlow 对话返回 HTTP {status}: {text}"));
        }

        resp.json::<ChatResponse>()
            .await
            .map_err(|e| format!("对话响应解析失败: {e}"))
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
// XInference 对话客户端
// ---------------------------------------------------------------------------

/// XInference 的 OpenAI 兼容本地 API（仅对话）的 HTTP 客户端。
pub struct XInferenceChatClient {
    client: Client,
    base_url: String,
    api_key: String,
    semaphore: Arc<Semaphore>,
}

impl XInferenceChatClient {
    /// 构建一个新的 XInference 对话客户端。
    pub fn new(base_url: String, api_key: String, max_concurrent: usize) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest::Client::builder() 不应失败"),
            base_url,
            api_key,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// 检查 XInference 服务器是否可达。
    pub async fn health_check(&self) -> Result<bool, String> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));

        match self.client.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(e) => Err(format!("XInference 健康检查失败: {e}")),
        }
    }

    /// 向 XInference 发送 chat completion 请求。
    pub async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, String> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("信号量获取失败: {e}"))?;

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt},
            ],
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        });

        let mut req = self.client.post(&url).json(&body);
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("XInference 对话请求失败: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("XInference 对话返回 HTTP {status}: {text}"));
        }

        resp.json::<ChatResponse>()
            .await
            .map_err(|e| format!("对话响应解析失败: {e}"))
    }
}

// ---------------------------------------------------------------------------
// 统一对话客户端 trait
// ---------------------------------------------------------------------------

/// 统一对话客户端 trait——由 SiliconFlow 与 XInference 共同实现。
#[async_trait]
pub trait ChatClient: Send + Sync {
    async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
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

#[async_trait]
impl ChatClient for XInferenceChatClient {
    async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<ChatResponse, String> {
        XInferenceChatClient::chat(
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
        XInferenceChatClient::health_check(self).await
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
