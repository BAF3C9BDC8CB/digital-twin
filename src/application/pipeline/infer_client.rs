//! HTTP client for the `dt-inference-server` (Python, localhost:50052).
//!
//! The inference server is the only GPU‑backed component in the system.  All
//! LLM chat, NLP analysis (HanLP), and text‑embedding requests flow through
//! this client.
//!
//! # Concurrency
//!
//! An [`Arc<Semaphore>`] caps the number of in‑flight HTTP requests so the
//! pipeline never overwhelms the inference server, which is typically the
//! bottleneck in the processing chain.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Default SiliconFlow API URL.
const SILICONFLOW_DEFAULT_URL: &str = "https://api.siliconflow.cn/v1";

// ---------------------------------------------------------------------------
// Public response / DTO types
// ---------------------------------------------------------------------------

/// OpenAI‑compatible chat completion response.
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

/// NLP analysis result (HanLP).
#[derive(Debug, Serialize, Deserialize)]
pub struct NlpResponse {
    pub entities: Vec<NamedEntity>,
    pub keywords: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NamedEntity {
    pub text: String,
    pub tag: String,
}

// ---------------------------------------------------------------------------
// Request bodies (private — only used internally)
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

#[derive(Debug, Serialize)]
struct HanlpRequest {
    text: String,
    tasks: Vec<String>,
    custom_entities: HashMap<String, Vec<String>>,
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
// Client
// ---------------------------------------------------------------------------

/// HTTP client for SiliconFlow's cloud API (OpenAI-compatible).
///
/// All public methods return `Result<T, String>` — the error string is
/// always safe to log or propagate as an internal error message.
pub struct InferClient {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    semaphore: Arc<Semaphore>,
}

impl InferClient {
    /// Build a new client that targets `base_url` and allows at most
    /// `max_concurrent` in‑flight requests at any time.
    ///
    /// `api_key` is the SiliconFlow API key. Falls back to `SILICONFLOW_API_KEY` env var.
    /// `model` is the model name (default: Qwen/Qwen3-8B).
    pub fn new(base_url: String, max_concurrent: usize) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("reqwest::Client::builder() should never fail with stock settings");

        Self {
            client,
            base_url: if base_url.is_empty() {
                SILICONFLOW_DEFAULT_URL.to_string()
            } else {
                base_url
            },
            api_key: std::env::var("SILICONFLOW_API_KEY").unwrap_or_default(),
            model: std::env::var("SILICONFLOW_LLM_MODEL").unwrap_or_default(),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Check whether the SiliconFlow API is reachable.
    ///
    /// Returns `Ok(true)` when the API responds with HTTP 200 at `GET /v1/models`.
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

    /// Send a chat completion request to SiliconFlow (OpenAI‑compatible).
    ///
    /// The `/v1/chat/completions` endpoint is invoked with the given
    /// `system_prompt` and `user_prompt`.  The response is parsed into a
    /// [`ChatResponse`] whose `choices[0].message.content` holds the model's
    /// reply.
    pub async fn chat(
        &self,
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
            model: self.model.clone(),
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

    /// NLP analysis via the HanLP endpoint.
    ///
    /// Sends a `POST /v1/nlp/hanlp` request with the given text, tasks,
    /// and custom entity dictionaries.  The inference server uses the
    /// custom dictionaries to improve NER recall for project-specific
    /// entity types (service names, components, business terms, etc.).
    pub async fn hanlp_analyze(
        &self,
        text: &str,
        tasks: &[String],
        custom_entities: &HashMap<String, Vec<String>>,
    ) -> Result<NlpResponse, String> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| format!("semaphore acquire failed: {e}"))?;

        let url = format!("{}/v1/nlp/hanlp", self.base_url);

        let body = HanlpRequest {
            text: text.to_string(),
            tasks: tasks.to_vec(),
            custom_entities: custom_entities.clone(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("hanlp request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text_body = resp.text().await.unwrap_or_default();
            return Err(format!("hanlp returned HTTP {status}: {text_body}"));
        }

        resp.json::<NlpResponse>()
            .await
            .map_err(|e| format!("hanlp response parse failed: {e}"))
    }

    /// Embed a batch of texts via `POST /v1/embeddings`.
    ///
    /// Returns one `Vec<f32>` vector per input text in the same order.
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_client_can_be_constructed() {
        let client = InferClient::new("http://localhost:50052".into(), 8);
        // Spot-check that the semaphore was created with the requested permits.
        assert!(client.semaphore.available_permits() <= 16);
    }

    #[tokio::test]
    async fn hanlp_analyze_returns_error_for_now() {
        let client = InferClient::new("http://localhost:50052".into(), 4);
        let custom = HashMap::new();
        let result = client
            .hanlp_analyze("hello", &["ner".into()], &custom)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("hanlp"));
    }

    #[test]
    fn chat_response_roundtrip() {
        let json = r#"{"choices":[{"message":{"content":"Hello!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content, "Hello!");
    }

    #[test]
    fn nlp_response_roundtrip() {
        let json = r#"{"entities":[{"text":"Apple","tag":"ORG"}],"keywords":["business"]}"#;
        let resp: NlpResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.entities.len(), 1);
        assert_eq!(resp.entities[0].text, "Apple");
        assert_eq!(resp.keywords, vec!["business"]);
    }
}
