//! Nacos HTTP API client.
//!
//! Thin wrapper around the Nacos Open API v1, providing typed request/response
//! methods for configuration and service discovery endpoints.
//!
//! # Endpoints used
//!
//! | Method | Endpoint | Purpose |
//! |--------|----------|---------|
//! | GET    | `/v1/console/namespaces` | List all namespaces |
//! | GET    | `/v1/cs/configs?pageNo=...&pageSize=...&tenant=...` | Paginated config list |
//! | GET    | `/v1/cs/configs?dataId=...&group=...&tenant=...&show=all` | Config content |
//! | GET    | `/v1/ns/catalog/services?namespaceId=...` | Service list |

use crate::domain::error::DtError;
use reqwest::Client as HttpClient;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Wrapper returned by `/v1/console/namespaces`.
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

/// Wrapper returned by `/v1/cs/configs` (list mode).
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

/// Wrapper returned by `/v1/cs/configs?show=all` (detail mode).
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

/// Wrapper returned by `/v1/ns/catalog/services`.
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

// ---------------------------------------------------------------------------
// NacosClient
// ---------------------------------------------------------------------------

/// HTTP client for Nacos Open API v1.
///
/// # Cloning
///
/// Cheap clone — underlying `reqwest::Client` uses `Arc` internally.
#[derive(Debug, Clone)]
pub struct NacosClient {
    base_url: String,
    http: HttpClient,
}

impl NacosClient {
    /// Create a new client targeting the given Nacos server.
    ///
    /// `base_url` should be the full URL prefix, e.g.
    /// `https://nacos.newoffen.net/nacos`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: HttpClient::new(),
        }
    }

    /// List all namespaces.
    pub async fn list_namespaces(&self) -> Result<NamespaceListResponse, DtError> {
        let url = format!("{}/v1/console/namespaces", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DtError::Network(e.to_string()))?;
        resp.json()
            .await
            .map_err(|e| DtError::Network(format!("parse namespaces: {e}")))
    }

    /// Fetch a single page of configuration metadata.
    ///
    /// Returns `None` when `pageItems` is empty.
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
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DtError::Network(e.to_string()))?;
        let body: ConfigListResponse = resp
            .json()
            .await
            .map_err(|e| DtError::Network(format!("parse config list: {e}")))?;

        if body.page_items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }

    /// Fetch the full content of a single configuration entry.
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
            .map_err(|e| DtError::Network(format!("parse config detail: {e}")))
    }

    /// List all registered services in a namespace.
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
            .map_err(|e| DtError::Network(format!("parse service list: {e}")))?;

        if body.service_list.is_empty() {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }
}

// ---------------------------------------------------------------------------
// URL encoding helper
// ---------------------------------------------------------------------------

/// Simple percent-encoding for Nacos dataId/group values that may contain
/// special characters.
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
