//! K8s sync module — V2 schema: K8sDeployment, K8sService, Server (from nodes).
//!
//! ## V2 Design
//! - **K8sDeployment**: persisted in Neo4j with label `K8sDeployment`.
//! - **K8sService**: persisted in Neo4j with label `K8sService`.
//! - **Server** (from K8s nodes): persisted in Neo4j with label `Server`.

pub mod client;
pub mod resource_sync;
pub mod timeline_sync;
pub mod types;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// K8s Sync Configuration
// ---------------------------------------------------------------------------

/// Configuration required to connect to a K8s cluster through the Kuboard proxy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct K8sSyncConfig {
    /// Kuboard server URL (e.g. `https://kuboard.example.com`).
    pub server: String,
    /// Kuboard login username.
    pub username: String,
    /// Kuboard login password (raw; sent base64-encoded to the login endpoint).
    pub password: String,
    /// K8s cluster ID as registered in Kuboard.
    pub cluster_id: String,
    /// If `true`, skip TLS certificate verification (dev only).
    #[serde(default)]
    pub skip_tls_verify: bool,
    /// Namespaces to sync. If empty, uses a built-in default list.
    #[serde(default)]
    pub namespaces: Vec<String>,
}

impl K8sSyncConfig {
    /// Returns the list of namespaces to sync, falling back to built-in defaults.
    pub fn effective_namespaces(&self) -> Vec<String> {
        if self.namespaces.is_empty() {
            vec!["newoffen".to_string(), "newoffen-test".to_string()]
        } else {
            self.namespaces.clone()
        }
    }

    /// Build the base URL for K8s API calls through the Kuboard proxy.
    pub fn k8s_api_base(&self) -> String {
        format!(
            "{}/k8s-api/{}",
            self.server.trim_end_matches('/'),
            self.cluster_id
        )
    }
}
