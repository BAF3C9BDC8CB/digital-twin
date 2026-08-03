//! Kuboard K8s API 客户端——认证与代理的 K8s API 调用。
//!
//! Kuboard 代理会转换 K8s API 路径：
//! ```text
//! POST {kuboard_url}/api/login.kuboard.cn/v4/login  → Bearer token
//! GET  {kuboard_url}/k8s-api/{cluster_id}/api/v1/... → K8s API 代理
//! ```
//!
//! 认证使用用户名 + base64 编码的密码。

// 部分方法（configmaps、ingresses、pvcs）为将来使用预留脚手架，
// 当这些资源类型加入完整同步流水线时会被调用。
#![allow(dead_code)]
// 私有类型告警：KuboardClient 为 pub，但返回 pub(crate) 类型。
// 这是有意为之——crate 内部类型在同一 crate 内使用没有问题。
#![allow(private_interfaces)]

use super::types::{
    ConfigMapItem, DeploymentItem, IngressItem, K8sItemList, KuboardLoginResp, NodeItem, PVCItem,
    PodItem, ServiceItem,
};
use super::K8sSyncConfig;
use crate::domain::error::DtError;

/// 用于 Kuboard 认证的 K8s API 调用的 HTTP 客户端包装器。
pub struct KuboardClient {
    http: reqwest::Client,
    config: K8sSyncConfig,
    /// 登录成功后缓存的 Bearer token。
    token: String,
    /// 代理的 K8s API 调用的基础 URL。
    base_url: String,
}

impl KuboardClient {
    /// 创建新客户端并针对 Kuboard 完成认证。
    ///
    /// # 错误
    /// 若登录失败或 TLS 配置无效，返回 `DtError::Grpc`（或 `DtError::General`）。
    pub async fn connect(config: K8sSyncConfig) -> Result<Self, DtError> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(config.skip_tls_verify)
            .build()
            .map_err(|e| DtError::Grpc(format!("构建 HTTP 客户端失败: {e}")))?;

        let token = kuboard_login(&http, &config).await?;
        let base_url = config.k8s_api_base();

        Ok(Self {
            http,
            config,
            token,
            base_url,
        })
    }

    // ── 资源获取器 ───────────────────────────────────────

    /// 获取指定命名空间的 Deployment。
    pub async fn fetch_deployments(&self, namespace: &str) -> Vec<DeploymentItem> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{}/deployments",
            self.base_url, namespace
        );
        fetch_items::<DeploymentItem>(&self.http, &url, &self.token).await
    }

    /// 获取指定命名空间的 Service。
    pub async fn fetch_services(&self, namespace: &str) -> Vec<ServiceItem> {
        let url = format!("{}/api/v1/namespaces/{}/services", self.base_url, namespace);
        fetch_items::<ServiceItem>(&self.http, &url, &self.token).await
    }

    /// 获取指定命名空间的 ConfigMap。
    pub async fn fetch_configmaps(&self, namespace: &str) -> Vec<ConfigMapItem> {
        let url = format!(
            "{}/api/v1/namespaces/{}/configmaps",
            self.base_url, namespace
        );
        fetch_items::<ConfigMapItem>(&self.http, &url, &self.token).await
    }

    /// 获取指定命名空间的 Ingress。
    pub async fn fetch_ingresses(&self, namespace: &str) -> Vec<IngressItem> {
        let url = format!(
            "{}/apis/networking.k8s.io/v1/namespaces/{}/ingresses",
            self.base_url, namespace
        );
        fetch_items::<IngressItem>(&self.http, &url, &self.token).await
    }

    /// 获取指定命名空间的持久卷声明。
    pub async fn fetch_pvcs(&self, namespace: &str) -> Vec<PVCItem> {
        let url = format!(
            "{}/api/v1/namespaces/{}/persistentvolumeclaims",
            self.base_url, namespace
        );
        fetch_items::<PVCItem>(&self.http, &url, &self.token).await
    }

    /// 获取集群节点（全局资源——无命名空间）。
    pub async fn fetch_nodes(&self) -> Vec<NodeItem> {
        let url = format!("{}/api/v1/nodes", self.base_url);
        fetch_items::<NodeItem>(&self.http, &url, &self.token).await
    }

    /// 获取指定命名空间的 Pod（用于 CLI 展示，不持久化到图数据库）。
    pub async fn fetch_pods(&self, namespace: &str) -> Vec<PodItem> {
        let url = format!("{}/api/v1/namespaces/{}/pods", self.base_url, namespace);
        fetch_items::<PodItem>(&self.http, &url, &self.token).await
    }

    /// 通过 Kuboard 代理获取 Pod 日志。
    ///
    /// URL：`{base_url}/api/v1/namespaces/{namespace}/pods/{pod}/log?tailLines={tail}`
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
            .map_err(|e| DtError::Grpc(format!("K8s Pod 日志请求失败 {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(DtError::Grpc(format!(
                "K8s API HTTP {} (url={url})",
                resp.status()
            )));
        }

        resp.text()
            .await
            .map_err(|e| DtError::Grpc(format!("K8s Pod 日志读取失败: {e}")))
    }

    /// 返回基础配置的引用。
    pub fn config(&self) -> &K8sSyncConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// 认证辅助函数
// ---------------------------------------------------------------------------

/// 使用 Kuboard 完成认证并返回 Bearer token。
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
        .map_err(|e| DtError::Grpc(format!("Kuboard 登录请求失败: {e}")))?
        .json()
        .await
        .map_err(|e| DtError::Grpc(format!("Kuboard 登录响应解析失败: {e}")))?;

    if resp.code != 200 {
        return Err(DtError::Grpc(format!(
            "Kuboard 登录被拒绝: code={}",
            resp.code
        )));
    }

    resp.data
        .map(|d| d.access_token)
        .ok_or_else(|| DtError::Grpc("Kuboard 登录响应缺少 accessToken".into()))
}

// ---------------------------------------------------------------------------
// 通用获取器
// ---------------------------------------------------------------------------

/// 获取分页的 K8s 列表资源，反序列化为 `K8sItemList<T>`。
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
        .map_err(|e| DtError::Grpc(format!("K8s API 请求失败 {url}: {e}")))?;

    if !resp.status().is_success() {
        return Err(DtError::Grpc(format!(
            "K8s API HTTP {} (url={url})",
            resp.status()
        )));
    }

    resp.json()
        .await
        .map_err(|e| DtError::Grpc(format!("K8s API JSON 解析失败 {url}: {e}")))
}

/// 获取条目列表，出错时吞掉错误并返回空 vec。
async fn fetch_items<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    token: &str,
) -> Vec<T> {
    match fetch_json::<K8sItemList<T>>(http, url, token).await {
        Ok(list) => list.items,
        Err(e) => {
            tracing::warn!("[k8s] 获取失败: {e}");
            vec![]
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
