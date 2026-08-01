//! HanLP REST API client — local NLP service for Chinese text processing.
//!
//! Provides named-entity recognition, keyword extraction, and text summarization
//! via a locally deployed HanLP HTTP server.
//!
//! # Endpoints
//!
//! | Method | Endpoint          | Purpose        |
//! |--------|-------------------|----------------|
//! | `POST` | `/analyze`        | Full NLP analysis |
//! | `GET`  | `/health`         | Health check   |
//!
//! # Configuration
//!
//! ```yaml
//! services:
//!   hanlp:
//!     url: http://localhost:8765
//!     api_key: ""
//!     model: hanlp
//! ```

use async_trait::async_trait;
use std::time::Duration;

use crate::domain::error::DtError;
use crate::domain::types::HealthStatus;

/// Maximum retry attempts for transient errors.
const MAX_RETRIES: u32 = 2;

/// Base delay in ms for retry backoff.
const RETRY_BASE_DELAY_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a HanLP NLP analysis.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HanlpResult {
    /// Named entities extracted from the text.
    #[serde(default)]
    pub entities: Vec<NamedEntity>,
    /// Keywords extracted from the text.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Summary/snippet of the text.
    #[serde(default)]
    pub summary: String,
}

/// A single named entity from HanLP.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NamedEntity {
    /// The entity text.
    pub text: String,
    /// Entity type tag (e.g. "NS" for place, "NT" for time, "ORG" for organization).
    #[serde(default)]
    pub tag: String,
    /// Frequency of occurrence in the text.
    #[serde(default)]
    pub frequency: usize,
}

// ---------------------------------------------------------------------------
// HanlpClient
// ---------------------------------------------------------------------------

/// HTTP client for a local HanLP REST API server.
pub struct HanlpClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl HanlpClient {
    /// Create a new HanLP client.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    /// Build a POST request to the given path.
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
        let mut last_error = String::new();

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                let delay = Duration::from_millis(RETRY_BASE_DELAY_MS * (1 << (attempt - 1)));
                tracing::warn!(
                    "HanLP {} 第 {}/{} 次尝试失败: {}，{:?} 后重试",
                    operation,
                    attempt,
                    MAX_RETRIES,
                    last_error,
                    delay
                );
                tokio::time::sleep(delay).await;
            }

            let req_built = req
                .try_clone()
                .ok_or_else(|| DtError::Repository("HanLP: failed to clone request".into()))?;

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
                        "HanLP {} error ({}): {}",
                        operation, status, body
                    )));
                }
                Err(e) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {}", e);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "HanLP {} request failed: {}",
                        operation, e
                    )));
                }
            }
        }

        Err(DtError::Repository(format!(
            "HanLP {} failed after {} retries: {}",
            operation, MAX_RETRIES, last_error
        )))
    }

    /// Perform full NLP analysis on the given text.
    ///
    /// Returns extracted entities, keywords, and summary.
    pub async fn analyze(&self, text: &str) -> Result<HanlpResult, DtError> {
        if text.trim().is_empty() {
            return Ok(HanlpResult {
                entities: vec![],
                keywords: vec![],
                summary: String::new(),
            });
        }

        let body = serde_json::json!({
            "text": text,
            "tasks": ["ner/ms", "ner/pku", "ner/ontonotes", "keywords", "summary"],
        });

        let resp = self
            .request_with_retry(self.post("/analyze").json(&body), "analyze")
            .await?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| DtError::Repository(format!("HanLP analyze parse: {e}")))?;

        // Parse entities from NER results
        let mut entities: Vec<NamedEntity> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Try different NER task keys
        for task_key in &["ner/ms", "ner/pku", "ner/ontonotes"] {
            if let Some(ner_list) = json.get(*task_key).and_then(|v| v.as_array()) {
                for item in ner_list {
                    if let (Some(text_val), Some(tag_val)) = (
                        item.get(0).and_then(|v| v.as_str()),
                        item.get(1).and_then(|v| v.as_str()),
                    ) {
                        let key = format!("{}:{}", text_val, tag_val);
                        if seen.insert(key) {
                            entities.push(NamedEntity {
                                text: text_val.to_string(),
                                tag: tag_val.to_string(),
                                frequency: 1,
                            });
                        } else {
                            // Increment frequency for duplicates
                            if let Some(e) = entities
                                .iter_mut()
                                .find(|e| e.text == text_val && e.tag == tag_val)
                            {
                                e.frequency += 1;
                            }
                        }
                    }
                }
            }
        }

        // Parse keywords
        let keywords: Vec<String> = json
            .get("keywords")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Parse summary
        let summary = json
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(HanlpResult {
            entities,
            keywords,
            summary,
        })
    }

    /// Check service health.
    pub async fn health_check(&self) -> Result<HealthStatus, DtError> {
        let url = format!("{}/health", self.base_url.trim_end_matches('/'));
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => Ok(HealthStatus::Healthy),
            Ok(resp) => Ok(HealthStatus::Unhealthy(format!(
                "HanLP health: HTTP {}",
                resp.status()
            ))),
            Err(e) => Ok(HealthStatus::Unhealthy(format!("HanLP health: {e}"))),
        }
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
        let client = HanlpClient::new("http://localhost:8765", "");
        assert_eq!(client.base_url, "http://localhost:8765");
        assert!(client.api_key.is_empty());
    }

    #[test]
    fn hanlp_result_default() {
        let result = HanlpResult::default();
        assert!(result.entities.is_empty());
        assert!(result.keywords.is_empty());
        assert!(result.summary.is_empty());
    }

    #[test]
    fn hanlp_result_serialization() {
        let result = HanlpResult {
            entities: vec![NamedEntity {
                text: "微服务".into(),
                tag: "NN".into(),
                frequency: 3,
            }],
            keywords: vec!["微服务".into(), "架构".into()],
            summary: "本文介绍了微服务架构".into(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("微服务"));
        assert!(json.contains("keywords"));
    }

    #[test]
    fn client_is_send_sync() {
        fn assert_send<T: Send>(_t: &T) {}
        fn assert_sync<T: Sync>(_t: &T) {}
        let client = HanlpClient::new("http://localhost:8765", "");
        assert_send(&client);
        assert_sync(&client);
    }
}
