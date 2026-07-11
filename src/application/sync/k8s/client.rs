//! Kuboard K8s API Client — authentication and proxied K8s API calls.
//!
//! The Kuboard proxy translates K8s API paths:
//! ```text
//! POST {kuboard_url}/api/login.kuboard.cn/v4/login  → Bearer token
//! GET  {kuboard_url}/k8s-api/{cluster_id}/api/v1/... → K8s API proxy
//! ```
//!
//! Authentication uses username + base64-encoded password.

// Some methods (configmaps, ingresses, pvcs) are scaffolded for future use
// and called by the full sync pipeline when those resource types are added.
#![allow(dead_code)]
// Private type warnings: KuboardClient is pub but returns pub(crate) types.
// This is intentional — the crate-internal types are fine within the same crate.
#![allow(private_interfaces)]

use super::types::{
    ConfigMapItem, DeploymentItem, IngressItem, K8sItemList, KuboardLoginResp,
    NodeItem, PVCItem, PodItem, ServiceItem,
};
use super::K8sSyncConfig;
use crate::domain::error::DtError;

/// HTTP client wrapper for Kuboard-authenticated K8s API calls.
pub struct KuboardClient {
    http: reqwest::Client,
    config: K8sSyncConfig,
    /// Cached Bearer token, obtained after successful login.
    token: String,
    /// Base URL for proxied K8s API calls.
    base_url: String,
}

impl KuboardClient {
    /// Create a new client and authenticate against Kuboard.
    ///
    /// # Errors
    /// Returns `DtError::Grpc` (or `DtError::General`) if login fails or TLS
    /// configuration is invalid.
    pub async fn connect(config: K8sSyncConfig) -> Result<Self, DtError> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(config.skip_tls_verify)
            .build()
            .map_err(|e| DtError::Grpc(format!("failed to build HTTP client: {e}")))?;

        let token = kuboard_login(&http, &config).await?;
        let base_url = config.k8s_api_base();

        Ok(Self {
            http,
            config,
            token,
            base_url,
        })
    }

    // ── Resource fetchers ───────────────────────────────────────

    /// Fetch deployments for a namespace.
    pub async fn fetch_deployments(&self, namespace: &str) -> Vec<DeploymentItem> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{}/deployments",
            self.base_url, namespace
        );
        fetch_items::<DeploymentItem>(&self.http, &url, &self.token).await
    }

    /// Fetch services for a namespace.
    pub async fn fetch_services(&self, namespace: &str) -> Vec<ServiceItem> {
        let url = format!(
            "{}/api/v1/namespaces/{}/services",
            self.base_url, namespace
        );
        fetch_items::<ServiceItem>(&self.http, &url, &self.token).await
    }

    /// Fetch configmaps for a namespace.
    pub async fn fetch_configmaps(&self, namespace: &str) -> Vec<ConfigMapItem> {
        let url = format!(
            "{}/api/v1/namespaces/{}/configmaps",
            self.base_url, namespace
        );
        fetch_items::<ConfigMapItem>(&self.http, &url, &self.token).await
    }

    /// Fetch ingresses for a namespace.
    pub async fn fetch_ingresses(&self, namespace: &str) -> Vec<IngressItem> {
        let url = format!(
            "{}/apis/networking.k8s.io/v1/namespaces/{}/ingresses",
            self.base_url, namespace
        );
        fetch_items::<IngressItem>(&self.http, &url, &self.token).await
    }

    /// Fetch persistent volume claims for a namespace.
    pub async fn fetch_pvcs(&self, namespace: &str) -> Vec<PVCItem> {
        let url = format!(
            "{}/api/v1/namespaces/{}/persistentvolumeclaims",
            self.base_url, namespace
        );
        fetch_items::<PVCItem>(&self.http, &url, &self.token).await
    }

    /// Fetch cluster nodes (global resource — no namespace).
    pub async fn fetch_nodes(&self) -> Vec<NodeItem> {
        let url = format!("{}/api/v1/nodes", self.base_url);
        fetch_items::<NodeItem>(&self.http, &url, &self.token).await
    }

    /// Fetch pods for a namespace (CLI display, not persisted in Neo4j).
    pub async fn fetch_pods(&self, namespace: &str) -> Vec<PodItem> {
        let url = format!(
            "{}/api/v1/namespaces/{}/pods",
            self.base_url, namespace
        );
        fetch_items::<PodItem>(&self.http, &url, &self.token).await
    }

    /// Fetch pod logs via Kuboard proxy.
    ///
    /// URL: `{base_url}/api/v1/namespaces/{namespace}/pods/{pod}/log?tailLines={tail}`
    pub async fn get_pod_logs(
        &self,
        pod: &str,
        namespace: &str,
        tail_lines: Option<u32>,
    ) -> Result<String, DtError> {
        let tail = tail_lines.unwrap_or(500);
        let url = format!(
            "{}/api/v1/namespaces/{}/pods/{}/log?tailLines={}",
            self.base_url, namespace, pod, tail
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|e| DtError::Grpc(format!("K8s pod log request failed {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(DtError::Grpc(format!(
                "K8s API HTTP {} for {url}",
                resp.status()
            )));
        }

        resp.text()
            .await
            .map_err(|e| DtError::Grpc(format!("K8s pod log read failed: {e}")))
    }

    /// Returns a reference to the base config.
    pub fn config(&self) -> &K8sSyncConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Authenticate with Kuboard and return a Bearer token.
async fn kuboard_login(http: &reqwest::Client, cfg: &K8sSyncConfig) -> Result<String, DtError> {
    use base64::Engine;

    let b64_password = base64::engine::general_purpose::STANDARD.encode(cfg.password.as_bytes());
    let login_url = format!("{}/api/login.kuboard.cn/v4/login", cfg.server);

    let body = serde_json::json!({
        "username": cfg.username,
        "password": b64_password,
    });

    let resp: KuboardLoginResp = http
        .post(&login_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| DtError::Grpc(format!("Kuboard login request failed: {e}")))?
        .json()
        .await
        .map_err(|e| DtError::Grpc(format!("Kuboard login parse failed: {e}")))?;

    if resp.code != 200 {
        return Err(DtError::Grpc(format!(
            "Kuboard login rejected: code={}",
            resp.code
        )));
    }

    resp.data
        .map(|d| d.access_token)
        .ok_or_else(|| DtError::Grpc("Kuboard login response missing accessToken".into()))
}

// ---------------------------------------------------------------------------
// Generic fetchers
// ---------------------------------------------------------------------------

/// Fetch a paginated K8s list resource, deserialising into `K8sItemList<T>`.
async fn fetch_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<T, DtError> {
    let resp = http
        .get(url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| DtError::Grpc(format!("K8s API request failed {url}: {e}")))?;

    if !resp.status().is_success() {
        return Err(DtError::Grpc(format!(
            "K8s API HTTP {} for {url}",
            resp.status()
        )));
    }

    resp.json()
        .await
        .map_err(|e| DtError::Grpc(format!("K8s API JSON parse failed {url}: {e}")))
}

/// Fetch a list of items, swallowing errors and returning an empty vec on failure.
async fn fetch_items<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Vec<T> {
    match fetch_json::<K8sItemList<T>>(http, url, token).await {
        Ok(list) => list.items,
        Err(e) => {
            tracing::warn!("[k8s] fetch failed: {e}");
            vec![]
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
    fn config_effective_namespaces_defaults() {
        let cfg = K8sSyncConfig::default();
        let ns = cfg.effective_namespaces();
        assert_eq!(ns.len(), 2);
        assert!(ns.contains(&"newoffen".to_string()));
        assert!(ns.contains(&"newoffen-test".to_string()));
    }

    #[test]
    fn config_effective_namespaces_custom() {
        let cfg = K8sSyncConfig {
            namespaces: vec!["my-ns".into()],
            ..Default::default()
        };
        let ns = cfg.effective_namespaces();
        assert_eq!(ns, vec!["my-ns"]);
    }

    #[test]
    fn config_k8s_api_base() {
        let cfg = K8sSyncConfig {
            server: "https://kuboard.example.com".into(),
            cluster_id: "cluster-1".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.k8s_api_base(),
            "https://kuboard.example.com/k8s-api/cluster-1"
        );
    }

    #[test]
    fn config_k8s_api_base_trailing_slash() {
        let cfg = K8sSyncConfig {
            server: "https://kuboard.example.com/".into(),
            cluster_id: "cluster-1".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.k8s_api_base(),
            "https://kuboard.example.com/k8s-api/cluster-1"
        );
    }
}
