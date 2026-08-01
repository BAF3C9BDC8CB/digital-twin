//! HTTP client for the SiliconFlow cloud API (OpenAI-compatible) and XInference.
//!
//! All LLM chat and text-embedding requests flow through this client.
//!
//! # Concurrency
//!
//! An [`Arc<Semaphore>`] caps the number of in-flight HTTP requests so the
//! pipeline never overwhelms the inference server.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Default SiliconFlow API URL.
const SILICONFLOW_DEFAULT_URL: &str = "https://api.siliconflow.cn/v1";

// ---------------------------------------------------------------------------
// Public response / DTO types
// ---------------------------------------------------------------------------

/// OpenAI-compatible chat completion response.
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
// Request bodies (private -- only used internally)
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
// SiliconFlow Client
// ---------------------------------------------------------------------------

/// HTTP client for SiliconFlow's cloud API (OpenAI-compatible).
pub struct SiliconFlowChatClient {
    client: Client,
    base_url: String,
    api_key: String,
    semaphore: Arc<Semaphore>,
}

impl SiliconFlowChatClient {
    /// Build a new client that targets `base_url` with max concurrent requests.
    pub fn new(base_url: String, max_concurrent: usize) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("reqwest::Client::builder() should never fail");

        Self {
            client,
            base_url: if base_url.is_empty() {
                SILICONFLOW_DEFAULT_URL.to_string()
            } else {
                base_url
            },
            api_key: std::env::var("SILICONFLOW_API_KEY").unwrap_or_default(),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// Check whether the SiliconFlow API is reachable.
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
            Err(e) => Err(format!("SiliconFlow health check failed: {e}")),
        }
    }

    /// Send a chat completion request to SiliconFlow (OpenAI-compatible).
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
            .map_err(|e| format!("semaphore acquire failed: {e}"))?;

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
            .map_err(|e| format!("SiliconFlow chat request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("SiliconFlow chat returned HTTP {status}: {text}"));
        }

        resp.json::<ChatResponse>()
            .await
            .map_err(|e| format!("chat response parse failed: {e}"))
    }

    /// Embed a batch of texts via POST /v1/embeddings.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("semaphore acquire failed: {e}"))?;

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
            .map_err(|e| format!("embed request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("embed returned HTTP {status}: {text}"));
        }

        let embed_resp = resp
            .json::<EmbedResponse>()
            .await
            .map_err(|e| format!("embed response parse failed: {e}"))?;

        Ok(embed_resp.data.into_iter().map(|d| d.embedding).collect())
    }
}

// ---------------------------------------------------------------------------
// XInference Chat Client
// ---------------------------------------------------------------------------

/// HTTP client for XInference's OpenAI-compatible local API (chat only).
pub struct XInferenceChatClient {
    client: Client,
    base_url: String,
    api_key: String,
    semaphore: Arc<Semaphore>,
}

impl XInferenceChatClient {
    /// Build a new XInference chat client.
    pub fn new(base_url: String, api_key: String, max_concurrent: usize) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest::Client::builder() should never fail"),
            base_url,
            api_key,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    /// Check whether the XInference server is reachable.
    pub async fn health_check(&self) -> Result<bool, String> {
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));

        match self.client.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(e) => Err(format!("XInference health check failed: {e}")),
        }
    }

    /// Send a chat completion request to XInference.
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
            .map_err(|e| format!("semaphore acquire failed: {e}"))?;

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
            .map_err(|e| format!("XInference chat request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("XInference chat returned HTTP {status}: {text}"));
        }

        resp.json::<ChatResponse>()
            .await
            .map_err(|e| format!("chat response parse failed: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Unified Chat Client Trait
// ---------------------------------------------------------------------------

/// Unified chat client trait -- implemented by both SiliconFlow and XInference.
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silicon_flow_chat_client_can_be_constructed() {
        let client = SiliconFlowChatClient::new(SILICONFLOW_DEFAULT_URL.into(), 8);
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
