//! HanLP REST API 客户端——用于中文文本处理的本地 NLP 服务。
//!
//! 通过本地部署的 HanLP HTTP 服务器提供命名实体识别、关键词提取
//! 与文本摘要能力。
//!
//! # 端点
//!
//! | Method | Endpoint          | Purpose        |
//! |--------|-------------------|----------------|
//! | `POST` | `/analyze`        | Full NLP analysis |
//! | `GET`  | `/health`         | Health check   |
//!
//! # 配置
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

/// 瞬时错误的最大重试次数。
const MAX_RETRIES: u32 = 2;

/// 重试退避的基础延迟（毫秒）。
const RETRY_BASE_DELAY_MS: u64 = 500;

// ---------------------------------------------------------------------------
// 公开类型
// ---------------------------------------------------------------------------

/// 一次 HanLP NLP 分析的结果。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HanlpResult {
    /// 从文本中抽取的命名实体。
    #[serde(default)]
    pub entities: Vec<NamedEntity>,
    /// 从文本中抽取的关键词。
    #[serde(default)]
    pub keywords: Vec<String>,
    /// 文本的摘要/片段。
    #[serde(default)]
    pub summary: String,
}

/// 来自 HanLP 的单个命名实体。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NamedEntity {
    /// 实体文本。
    pub text: String,
    /// 实体类型标签（如 "NS" 表示地点、"NT" 表示时间、"ORG" 表示机构）。
    #[serde(default)]
    pub tag: String,
    /// 在文本中出现的频次。
    #[serde(default)]
    pub frequency: usize,
}

// ---------------------------------------------------------------------------
// HanlpClient
// ---------------------------------------------------------------------------

/// 本地 HanLP REST API 服务器的 HTTP 客户端。
pub struct HanlpClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl HanlpClient {
    /// 创建新的 HanLP 客户端。
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

    /// 向给定路径构建一个 POST 请求。
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
                .ok_or_else(|| DtError::Repository("HanLP: 请求克隆失败".into()))?;

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
                        "HanLP {} 错误 ({}): {}",
                        operation, status, body
                    )));
                }
                Err(e) => {
                    if e.is_timeout() || e.is_connect() {
                        last_error = format!("connection: {}", e);
                        continue;
                    }
                    return Err(DtError::Repository(format!(
                        "HanLP {} 请求失败: {}",
                        operation, e
                    )));
                }
            }
        }

        Err(DtError::Repository(format!(
            "HanLP {} 重试 {} 次后仍失败: {}",
            operation, MAX_RETRIES, last_error
        )))
    }

    /// 对给定文本执行完整 NLP 分析。
    ///
    /// 返回抽取出的实体、关键词与摘要。
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
            .map_err(|e| DtError::Repository(format!("HanLP analyze 解析: {e}")))?;

        // 从 NER 结果中解析实体
        let mut entities: Vec<NamedEntity> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // 尝试不同的 NER 任务键
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
                            // 对重复实体累加频次
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

        // 解析关键词
        let keywords: Vec<String> = json
            .get("keywords")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // 解析摘要
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

    /// 检查服务健康状态。
    pub async fn health_check(&self) -> Result<HealthStatus, DtError> {
        let url = format!("{}/health", self.base_url.trim_end_matches('/'));
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => Ok(HealthStatus::Healthy),
            Ok(resp) => Ok(HealthStatus::Unhealthy(format!(
                "HanLP 健康检查: HTTP {}",
                resp.status()
            ))),
            Err(e) => Ok(HealthStatus::Unhealthy(format!("HanLP 健康检查: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
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
