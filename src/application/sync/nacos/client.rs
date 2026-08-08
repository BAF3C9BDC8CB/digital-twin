//! Nacos HTTP API 客户端。
//!
//! 对 Nacos Open API v1 的轻量封装，为配置与服务发现端点
//! 提供类型化的请求/响应方法。
//!
//! # 使用的端点
//!
//! | 方法 | 端点 | 用途 |
//! |--------|----------|---------|
//! | GET    | `/v1/console/namespaces` | 列出所有命名空间 |
//! | GET    | `/v1/cs/configs?pageNo=...&pageSize=...&tenant=...` | 分页配置列表 |
//! | GET    | `/v1/cs/configs?dataId=...&group=...&tenant=...&show=all` | 配置内容 |
//! | GET    | `/v1/ns/catalog/services?namespaceId=...` | 服务列表 |

use crate::domain::error::DtError;
use reqwest::Client as HttpClient;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::time::{Duration, Instant};

const NACOS_MAX_RETRIES: u32 = 3;
const NACOS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NACOS_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const NACOS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const NACOS_RETRY_BASE_MS: u64 = 200;

fn transient_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or_else(|| {
        let base = NACOS_RETRY_BASE_MS.saturating_mul(1u64 << attempt.min(6));
        Duration::from_millis(base + ((attempt as u64 * 37) % 101))
    })
}

// ---------------------------------------------------------------------------
// 响应类型
// ---------------------------------------------------------------------------

/// `/v1/console/namespaces` 返回的包装结构。
#[derive(Debug, Clone, Deserialize)]
pub struct NamespaceListResponse {
    pub data: Vec<NamespaceItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NamespaceItem {
    #[serde(rename = "namespace")]
    pub namespace_id: String,
    #[serde(rename = "namespaceShowName")]
    pub namespace_show_name: String,
    #[serde(rename = "configCount")]
    pub config_count: i64,
}

/// `/v1/cs/configs`（列表模式）返回的包装结构。
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigListResponse {
    #[serde(rename = "totalCount")]
    pub total_count: i64,
    #[serde(rename = "pageItems")]
    pub page_items: Vec<ConfigListItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigListItem {
    #[serde(rename = "dataId")]
    pub data_id: String,
    pub group: String,
}

/// `/v1/cs/configs?show=all`（详情模式）返回的包装结构。
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigDetailResponse {
    #[serde(default, rename = "dataId")]
    pub data_id: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, rename = "type")]
    pub config_type: Option<String>,
}

/// `/v1/ns/catalog/services` 返回的包装结构。
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceListResponse {
    pub count: i64,
    #[serde(rename = "serviceList")]
    pub service_list: Vec<ServiceItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceItem {
    pub name: String,
    #[serde(rename = "groupName")]
    pub group_name: String,
    #[serde(rename = "clusterCount")]
    pub cluster_count: Option<i64>,
    #[serde(rename = "ipCount")]
    pub ip_count: i64,
    #[serde(rename = "healthyInstanceCount")]
    pub healthy_instance_count: i64,
}

/// `/v1/ns/catalog/instances` 返回的包装结构。
#[derive(Debug, Clone, Deserialize)]
pub struct InstanceListResponse {
    pub count: Option<i64>,
    pub list: Option<Vec<InstanceItem>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstanceItem {
    #[serde(rename = "instanceId")]
    pub instance_id: String,
    pub ip: String,
    pub port: i64,
    #[serde(default)]
    pub weight: f64,
    pub healthy: bool,
    pub enabled: bool,
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(rename = "clusterName")]
    pub cluster_name: Option<String>,
    #[serde(rename = "serviceName")]
    pub service_name: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// NacosClient
// ---------------------------------------------------------------------------

/// Nacos Open API v1 的 HTTP 客户端。
///
/// # 克隆
///
/// 廉价克隆——底层 `reqwest::Client` 内部使用 `Arc`。
#[derive(Debug, Clone)]
pub struct NacosClient {
    base_url: String,
    http: HttpClient,
}

impl NacosClient {
    /// 创建指向指定 Nacos 服务器的新客户端。
    ///
    /// `base_url` 应为完整的 URL 前缀，例如
    /// `https://nacos.newoffen.net/nacos`。
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = HttpClient::builder()
            .connect_timeout(NACOS_CONNECT_TIMEOUT)
            .timeout(NACOS_REQUEST_TIMEOUT)
            .build()
            .expect("Nacos HTTP client configuration must be valid");
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        page: Option<i64>,
    ) -> Result<T, DtError> {
        let started = Instant::now();
        let mut last_error = String::new();
        for attempt in 0..=NACOS_MAX_RETRIES {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            tracing::info!(target: "pipeline_diagnostics", event = "nacos.request_start", endpoint, page = ?page, attempt = attempt + 1, elapsed_ms);
            let result =
                tokio::time::timeout(NACOS_RESPONSE_TIMEOUT, self.http.get(endpoint).send()).await;
            match result {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    let status = resp.status();
                    let parsed =
                        tokio::time::timeout(NACOS_RESPONSE_TIMEOUT, resp.json::<T>()).await;
                    match parsed {
                        Ok(Ok(value)) => {
                            tracing::info!(target: "pipeline_diagnostics", event = "nacos.response", endpoint, page = ?page, attempt = attempt + 1, elapsed_ms = started.elapsed().as_millis() as u64, status = status.as_u16());
                            return Ok(value);
                        }
                        Ok(Err(e)) => last_error = format!("response parse: {e}"),
                        Err(_) => last_error = "response timeout".into(),
                    }
                }
                Ok(Ok(resp)) => {
                    let status = resp.status();
                    let retryable = transient_status(status);
                    last_error = format!("HTTP {}", status);
                    tracing::warn!(target: "pipeline_diagnostics", event = "nacos.error", endpoint, page = ?page, attempt = attempt + 1, elapsed_ms = started.elapsed().as_millis() as u64, status = status.as_u16(), retryable);
                    if !retryable {
                        break;
                    }
                    if attempt < NACOS_MAX_RETRIES {
                        let delay = backoff(attempt, retry_after(resp.headers()));
                        tracing::warn!(target: "pipeline_diagnostics", event = "nacos.retry", endpoint, page = ?page, attempt = attempt + 1, elapsed_ms = started.elapsed().as_millis() as u64, delay_ms = delay.as_millis() as u64);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                }
                Ok(Err(e)) => {
                    last_error = e.to_string();
                    let retryable = e.is_connect() || e.is_timeout() || e.is_request();
                    tracing::warn!(target: "pipeline_diagnostics", event = "nacos.error", endpoint, page = ?page, attempt = attempt + 1, elapsed_ms = started.elapsed().as_millis() as u64, retryable, error = %last_error);
                    if retryable && attempt < NACOS_MAX_RETRIES {
                        let delay = backoff(attempt, None);
                        tracing::warn!(target: "pipeline_diagnostics", event = "nacos.retry", endpoint, page = ?page, attempt = attempt + 1, elapsed_ms = started.elapsed().as_millis() as u64, delay_ms = delay.as_millis() as u64);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                }
                Err(_) => last_error = "response timeout".into(),
            }
        }
        Err(DtError::Network(format!(
            "Nacos request failed endpoint={} page={:?} attempts={} elapsed_ms={} error={}",
            endpoint,
            page,
            NACOS_MAX_RETRIES + 1,
            started.elapsed().as_millis(),
            last_error
        )))
    }

    /// Perform a lightweight connectivity preflight.
    pub async fn health_check(&self) -> Result<(), DtError> {
        let url = format!("{}/v1/console/namespaces", self.base_url);
        let _: NamespaceListResponse = self.get_json(&url, None).await?;
        Ok(())
    }

    /// 列出所有命名空间。
    pub async fn list_namespaces(&self) -> Result<NamespaceListResponse, DtError> {
        let url = format!("{}/v1/console/namespaces", self.base_url);
        self.get_json(&url, None).await
    }

    /// 获取一页配置元数据。
    ///
    /// 当 `pageItems` 为空时返回 `None`。
    pub async fn list_configs(
        &self,
        tenant: &str,
        page_no: i64,
        page_size: i64,
    ) -> Result<Option<ConfigListResponse>, DtError> {
        let url = format!(
            "{}/v1/cs/configs?dataId=&group=&appName=&config_tags=&pageNo={}&pageSize={}&search=blur&tenant={}",
            self.base_url, page_no, page_size, tenant
        );
        let body: ConfigListResponse = self.get_json(&url, Some(page_no)).await?;

        if body.page_items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }

    /// 获取单个配置条目的完整内容。
    pub async fn get_config_detail(
        &self,
        data_id: &str,
        group: &str,
        tenant: &str,
    ) -> Result<ConfigDetailResponse, DtError> {
        let url = format!(
            "{}/v1/cs/configs?dataId={}&group={}&tenant={}&show=all",
            self.base_url,
            urlencode(data_id),
            urlencode(group),
            tenant,
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DtError::Network(e.to_string()))?;
        resp.json()
            .await
            .map_err(|e| DtError::Network(format!("解析配置详情失败: {e}")))
    }

    /// 列出命名空间中所有已注册的服务。
    pub async fn list_services(
        &self,
        tenant: &str,
    ) -> Result<Option<ServiceListResponse>, DtError> {
        let url = format!(
            "{}/v1/ns/catalog/services?hasIpCount=true&withInstances=false&pageNo=1&pageSize=200&serviceNameParam=&groupNameParam=&namespaceId={}",
            self.base_url, tenant
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DtError::Network(e.to_string()))?;
        let body: ServiceListResponse = resp
            .json()
            .await
            .map_err(|e| DtError::Network(format!("解析服务列表失败: {e}")))?;

        if body.service_list.is_empty() {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }

    /// 列出命名空间中某个服务的实例。
    pub async fn list_instances(
        &self,
        service_name: &str,
        namespace_id: &str,
    ) -> Result<Option<InstanceListResponse>, DtError> {
        let url = format!(
            "{}/v1/ns/catalog/instances?serviceName={}&namespaceId={}&clusterName=DEFAULT&pageNo=1&pageSize=200",
            self.base_url,
            urlencode(service_name),
            namespace_id
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DtError::Network(e.to_string()))?;
        let body: InstanceListResponse = resp
            .json()
            .await
            .map_err(|e| DtError::Network(format!("解析实例列表失败: {e}")))?;

        match &body.list {
            Some(list) if !list.is_empty() => Ok(Some(body)),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// URL 编码辅助函数
// ---------------------------------------------------------------------------

/// 对可能包含特殊字符的 Nacos dataId/group 值进行简单百分号编码。
fn urlencode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_is_bounded_and_safe() {
        assert!(transient_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(transient_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(transient_status(reqwest::StatusCode::SERVICE_UNAVAILABLE));
        assert!(transient_status(reqwest::StatusCode::GATEWAY_TIMEOUT));
        assert!(!transient_status(reqwest::StatusCode::BAD_REQUEST));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        assert_eq!(backoff(0, None), Duration::from_millis(200));
        assert!(backoff(3, None) < Duration::from_secs(2));
    }

    #[test]
    fn urlencode_simple() {
        assert_eq!(urlencode("hello-world"), "hello-world");
    }

    #[test]
    fn urlencode_spaces() {
        assert_eq!(urlencode("hello world"), "hello%20world");
    }

    #[test]
    fn urlencode_special_chars() {
        let encoded = urlencode("app=test&key=val");
        assert!(encoded.contains("%3D"));
        assert!(encoded.contains("%26"));
    }

    #[test]
    fn client_creation() {
        let c = NacosClient::new("https://example.com/nacos");
        assert_eq!(c.base_url, "https://example.com/nacos");
    }

    #[test]
    fn client_clone_is_cheap() {
        let c1 = NacosClient::new("https://example.com/nacos");
        let c2 = c1.clone();
        assert_eq!(c2.base_url, "https://example.com/nacos");
    }

    #[test]
    fn deserialize_namespace_response() {
        let json = r#"{
            "data": [
                {"namespace": "ns1", "namespaceShowName": "Test", "configCount": 5},
                {"namespace": "ns2", "namespaceShowName": "Prod", "configCount": 42}
            ]
        }"#;
        let resp: NamespaceListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 2);
        assert_eq!(resp.data[0].namespace_id, "ns1");
        assert_eq!(resp.data[1].config_count, 42);
    }

    #[test]
    fn deserialize_config_list_response() {
        let json = r#"{
            "totalCount": 2,
            "pageItems": [
                {"dataId": "app.yaml", "group": "DEFAULT_GROUP"},
                {"dataId": "db.properties", "group": "DB_GROUP"}
            ]
        }"#;
        let resp: ConfigListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.total_count, 2);
        assert_eq!(resp.page_items[0].data_id, "app.yaml");
    }

    #[test]
    fn deserialize_config_detail_response() {
        let json = r#"{
            "dataId": "app.yaml",
            "group": "DEFAULT_GROUP",
            "content": "server:\n  port: 8080",
            "type": "yaml"
        }"#;
        let resp: ConfigDetailResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data_id.unwrap(), "app.yaml");
        assert_eq!(resp.content.unwrap(), "server:\n  port: 8080");
        assert_eq!(resp.config_type.unwrap(), "yaml");
    }

    #[test]
    fn deserialize_config_detail_missing_fields() {
        let json = r#"{}"#;
        let resp: ConfigDetailResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.is_none());
        assert!(resp.config_type.is_none());
    }

    #[test]
    fn deserialize_service_list_response() {
        let json = r#"{
            "count": 3,
            "serviceList": [
                {"name": "user-service", "groupName": "DEFAULT_GROUP", "clusterCount": 1, "ipCount": 2, "healthyInstanceCount": 2},
                {"name": "order-service", "groupName": "ORDER", "clusterCount": 2, "ipCount": 4, "healthyInstanceCount": 3}
            ]
        }"#;
        let resp: ServiceListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.count, 3);
        assert_eq!(resp.service_list.len(), 2);
        assert_eq!(resp.service_list[1].healthy_instance_count, 3);
    }
}
