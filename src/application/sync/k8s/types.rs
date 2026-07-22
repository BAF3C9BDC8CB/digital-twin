//! V2 domain types for K8s sync.
//!
//! These are the graph node representations for entities that belong in the
//! **Reality World** (persisted to the knowledge graph).
//!
//! ## What goes into the graph database
//!
//! | K8s Object    | Graph Label | Rationale |
//! |---------------|-----------------|-----------|
//! | Deployment    | `K8sDeployment` | Desired-state infrastructure |
//! | Service       | `K8sService`    | Stable network endpoint |
//! | Node (Worker) | `Server`        | Physical/virtual machine asset |
//! | Pod           | _(not stored)_  | Ephemeral — Runtime only |
//!
//! API response structs below have allow(dead_code) because serde needs every
//! field for deserialization even if the current sync logic doesn't read them.

#![allow(dead_code)] // API response fields used by serde, not all read by sync logic

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// K8sDeployment
// ---------------------------------------------------------------------------

/// A K8s Deployment persisted as a `:K8sDeployment` node in the graph database.
///
/// Uniqueness constraint: `(name, namespace)`.
/// Relationship: `(:ServiceInstance)-[:DEPLOYED_AS]->(:K8sDeployment)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sDeployment {
    /// Deployment name (e.g. `my-app-stable`).
    pub name: String,
    /// K8s namespace.
    pub namespace: String,
    /// Container image (first container).
    pub image: String,
    /// Desired number of replicas.
    pub replicas: i64,
    /// Number of available replicas.
    pub available: i64,
    /// Deployment strategy (e.g. `RollingUpdate`).
    pub strategy: String,
    /// Labels as a JSON-encoded string.
    pub labels: String,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
}

impl K8sDeployment {
    /// Composite key for uniqueness: `(name, namespace)`.
    pub fn composite_key(&self) -> String {
        format!("{}::{}", self.namespace, self.name)
    }
}

// ---------------------------------------------------------------------------
// K8sService
// ---------------------------------------------------------------------------

/// A K8s Service persisted as a `:K8sService` node in the graph database.
///
/// Relationship: `(:Namespace)-[:HAS_SERVICE]->(:K8sService)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sService {
    /// Service name.
    pub name: String,
    /// K8s namespace.
    pub namespace: String,
    /// Cluster-internal IP address.
    pub cluster_ip: String,
    /// Service type (`ClusterIP`, `NodePort`, `LoadBalancer`, `ExternalName`).
    #[serde(rename = "type")]
    pub svc_type: String,
}

impl K8sService {
    /// Composite key for uniqueness: `(name, namespace)`.
    pub fn composite_key(&self) -> String {
        format!("{}::{}", self.namespace, self.name)
    }
}

// ---------------------------------------------------------------------------
// Server (from K8s Node)
// ---------------------------------------------------------------------------

/// A Server node derived from a K8s worker node.
///
/// Persisted as a `:Server` node in the graph database.
/// Relationship: `(:Server)-[:DEPLOYED_IN]->(:Environment)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sServer {
    /// Unique server identifier (generated via SHA256 of node name).
    pub server_id: String,
    /// K8s node name (used as server name).
    pub name: String,
    /// Node hostname.
    pub hostname: String,
    /// Service type (always `"kubernetes_node"` for K8s nodes).
    pub service_type: String,
    /// CPU capacity (e.g. `"8"`).
    pub cpu_cores: String,
    /// Memory capacity (e.g. `"32Gi"`).
    pub memory_gb: String,
    /// Kuboard URL for this cluster.
    pub url: String,
    /// Human-readable description.
    pub description: String,
}

impl K8sServer {
    /// Generate a `server_id` from the node name.
    pub fn make_server_id(node_name: &str) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(node_name.as_bytes());
        format!("server::k8s::{}", hex::encode(&hash[..10]))
    }
}

// ---------------------------------------------------------------------------
// Low-level API response types (K8s JSON shapes)
// ---------------------------------------------------------------------------

/// Paginated list response from K8s API.
#[derive(Debug, Deserialize)]
pub(crate) struct K8sItemList<T> {
    pub items: Vec<T>,
}

/// Common K8s ObjectMeta (subset).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct K8sMeta {
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub labels: Option<std::collections::HashMap<String, String>>,
    #[serde(default, rename = "creationTimestamp")]
    pub creation_timestamp: Option<String>,
    #[serde(default, rename = "ownerReferences")]
    pub owner_references: Option<Vec<OwnerRef>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OwnerRef {
    pub kind: String,
    pub name: String,
}

// ── Pod (for CLI display, not persisted in the graph database) ───────────

#[derive(Debug, Deserialize)]
pub(crate) struct PodItem {
    pub metadata: K8sMeta,
    pub status: PodStatus,
    #[serde(default)]
    pub spec: PodSpecBrief,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PodStatus {
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default, rename = "hostIP")]
    pub host_ip: Option<String>,
    #[serde(default, rename = "podIP")]
    pub pod_ip: Option<String>,
    #[serde(default, rename = "startTime")]
    pub start_time: Option<String>,
    #[serde(default, rename = "containerStatuses")]
    pub container_statuses: Option<Vec<ContainerStatus>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContainerStatus {
    pub name: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default, rename = "restartCount")]
    pub restart_count: Option<i64>,
    #[serde(default)]
    pub state: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct PodSpecBrief {
    #[serde(default, rename = "nodeName")]
    pub node_name: Option<String>,
}

// ── Deployment ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct DeploymentItem {
    pub metadata: K8sMeta,
    pub spec: DeploymentSpec,
    pub status: DeploymentStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeploymentSpec {
    #[serde(default)]
    pub replicas: Option<i64>,
    #[serde(default)]
    pub strategy: Option<DeploymentStrategy>,
    pub template: PodTemplateSpec,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeploymentStrategy {
    #[serde(rename = "type", default)]
    pub strategy_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PodTemplateSpec {
    pub spec: PodSpec,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PodSpec {
    #[serde(default)]
    pub containers: Vec<ContainerItem>,
    #[serde(default, rename = "nodeName")]
    pub node_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContainerItem {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub ports: Option<Vec<ContainerPort>>,
    #[serde(default)]
    pub resources: Option<ResourceSpec>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContainerPort {
    #[serde(default, rename = "containerPort")]
    pub container_port: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourceSpec {
    #[serde(default)]
    pub limits: Option<ResourceEntry>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourceEntry {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeploymentStatus {
    #[serde(default, rename = "availableReplicas")]
    pub available_replicas: Option<i64>,
    #[serde(default)]
    pub conditions: Option<Vec<DeploymentCondition>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeploymentCondition {
    #[serde(rename = "type", default)]
    pub condition_type: Option<String>,
}

// ── Service ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ServiceItem {
    pub metadata: K8sMeta,
    pub spec: ServiceSpec,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServiceSpec {
    #[serde(rename = "type", default)]
    pub svc_type: Option<String>,
    #[serde(default, rename = "clusterIP")]
    pub cluster_ip: Option<String>,
    #[serde(default)]
    pub selector: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub ports: Option<Vec<ServicePort>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServicePort {
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
}

// ── Node ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct NodeItem {
    pub metadata: K8sMeta,
    pub status: NodeStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeStatus {
    #[serde(default)]
    pub capacity: Option<NodeResources>,
    #[serde(default)]
    pub addresses: Option<Vec<NodeAddress>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeResources {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub pods: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeAddress {
    #[serde(rename = "type", default)]
    pub addr_type: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
}

// ── ConfigMap ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigMapItem {
    pub metadata: K8sMeta,
    #[serde(default)]
    pub data: Option<std::collections::HashMap<String, String>>,
}

// ── Ingress ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct IngressItem {
    pub metadata: K8sMeta,
    pub spec: IngressSpec,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngressSpec {
    #[serde(default)]
    pub rules: Option<Vec<IngressRule>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngressRule {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http: Option<IngressHttp>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngressHttp {
    pub paths: Vec<IngressPath>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngressPath {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub backend: IngressBackend,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct IngressBackend {
    #[serde(default)]
    pub service: Option<IngressServiceRef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IngressServiceRef {
    pub name: String,
}

// ── PVC ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct PVCItem {
    pub metadata: K8sMeta,
    pub status: PVCStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PVCStatus {
    #[serde(default)]
    pub phase: Option<String>,
}

// ── Kuboard Login ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct KuboardLoginResp {
    pub code: i64,
    #[serde(default)]
    pub data: Option<KuboardLoginData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KuboardLoginData {
    #[serde(rename = "accessToken")]
    pub access_token: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k8s_deployment_composite_key() {
        let d = K8sDeployment {
            name: "myapp-stable".into(),
            namespace: "newoffen".into(),
            image: "registry/myapp:v1".into(),
            replicas: 3,
            available: 3,
            strategy: "RollingUpdate".into(),
            labels: r#"{"app":"myapp"}"#.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        assert_eq!(d.composite_key(), "newoffen::myapp-stable");
    }

    #[test]
    fn k8s_service_composite_key() {
        let s = K8sService {
            name: "myapp-svc".into(),
            namespace: "newoffen".into(),
            cluster_ip: "10.0.0.1".into(),
            svc_type: "ClusterIP".into(),
        };
        assert_eq!(s.composite_key(), "newoffen::myapp-svc");
    }

    #[test]
    fn k8s_server_make_id() {
        let id = K8sServer::make_server_id("worker-1");
        assert!(id.starts_with("server::k8s::"));
        assert_eq!(id.len(), "server::k8s::".len() + 20); // 10 bytes hex
    }

    #[test]
    fn k8s_server_id_deterministic() {
        let id1 = K8sServer::make_server_id("worker-1");
        let id2 = K8sServer::make_server_id("worker-1");
        assert_eq!(id1, id2);
    }

    #[test]
    fn k8s_server_id_unique() {
        let id1 = K8sServer::make_server_id("worker-1");
        let id2 = K8sServer::make_server_id("worker-2");
        assert_ne!(id1, id2);
    }
}
