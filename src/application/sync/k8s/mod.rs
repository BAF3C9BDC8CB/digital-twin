//! K8s 同步模块——V2 模式：K8sDeployment、K8sService、Server（来自节点）。
//!
//! ## V2 设计
//! - **K8sDeployment**：以标签 `K8sDeployment` 持久化到 Memgraph。
//! - **K8sService**：以标签 `K8sService` 持久化到 Memgraph。
//! - **Server**（来自 K8s 节点）：以标签 `Server` 持久化到 Memgraph。

pub mod client;
pub mod resource_sync;
pub mod timeline_sync;
pub mod types;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// K8s 同步配置
// ---------------------------------------------------------------------------

/// 通过 Kuboard 代理连接 K8s 集群所需的配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct K8sSyncConfig {
    /// Kuboard 服务器 URL（例如 `https://kuboard.example.com`）。
    pub server: String,
    /// Kuboard 登录用户名。
    pub username: String,
    /// Kuboard 登录密码（明文；发送至登录端点时以 base64 编码）。
    pub password: String,
    /// Kuboard 中注册的 K8s 集群 ID。
    pub cluster_id: String,
    /// 若为 `true`，跳过 TLS 证书校验（仅限开发环境）。
    #[serde(default)]
    pub skip_tls_verify: bool,
    /// 要同步的命名空间。为空时使用内置默认列表。
    #[serde(default)]
    pub namespaces: Vec<String>,
}

impl K8sSyncConfig {
    /// 返回要同步的命名空间列表，为空时回退到内置默认值。
    pub fn effective_namespaces(&self) -> Vec<String> {
        if self.namespaces.is_empty() {
            vec!["newoffen".to_string(), "newoffen-test".to_string()]
        } else {
            self.namespaces.clone()
        }
    }

    /// 构建通过 Kuboard 代理调用 K8s API 的基础 URL。
    pub fn k8s_api_base(&self) -> String {
        format!(
            "{}/k8s-api/{}",
            self.server.trim_end_matches('/'),
            self.cluster_id
        )
    }
}
