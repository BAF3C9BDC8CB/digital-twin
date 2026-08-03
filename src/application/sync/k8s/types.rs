//! K8s 同步的 V2 领域类型。
//!
//! 这些是属于 **Reality World**（持久化到知识图谱）实体的图节点表示。
//!
//! ## 哪些内容进入图数据库
//!
//! | K8s 对象    | 图标签 | 说明 |
//! |---------------|-----------------|-----------|
//! | Deployment    | `K8sDeployment` | 期望状态的基础设施 |
//! | Service       | `K8sService`    | 稳定的网络端点 |
//! | Node (Worker) | `Server`        | 物理/虚拟机资产 |
//! | Pod           | _（不存储）_  | 瞬态——仅存在于 Runtime |
//!
//! 下面的 API 响应结构体带有 allow(dead_code)，因为即使当前同步逻辑
//! 不读取某些字段，serde 反序列化也需要每个字段。

#![allow(dead_code)] // API 响应字段由 serde 使用，同步逻辑并非全部读取

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// K8sDeployment
// ---------------------------------------------------------------------------

/// 以 `:K8sDeployment` 节点持久化到图数据库的 K8s Deployment。
///
/// 唯一性约束：`(name, namespace)`。
/// 关系：`(:ServiceInstance)-[:DEPLOYED_AS]->(:K8sDeployment)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sDeployment {
    /// Deployment 名称（例如 `my-app-stable`）。
    pub name: String,
    /// K8s 命名空间。
    pub namespace: String,
    /// 容器镜像（第一个容器）。
    pub image: String,
    /// 期望的副本数。
    pub replicas: i64,
    /// 可用副本数。
    pub available: i64,
    /// Deployment 策略（例如 `RollingUpdate`）。
    pub strategy: String,
    /// JSON 编码字符串形式的标签。
    pub labels: String,
    /// 创建时间戳（RFC 3339）。
    pub created_at: String,
}

impl K8sDeployment {
    /// 唯一性复合键：`(name, namespace)`。
    pub fn composite_key(&self) -> String {
        format!("{}::{}", self.namespace, self.name)
    }
}

// ---------------------------------------------------------------------------
// K8sService
// ---------------------------------------------------------------------------

/// 以 `:K8sService` 节点持久化到图数据库的 K8s Service。
///
/// 关系：`(:Namespace)-[:HAS_SERVICE]->(:K8sService)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sService {
    /// 服务名。
    pub name: String,
    /// K8s 命名空间。
    pub namespace: String,
    /// 集群内部 IP 地址。
    pub cluster_ip: String,
    /// 服务类型（`ClusterIP`、`NodePort`、`LoadBalancer`、`ExternalName`）。
    #[serde(rename = "type")]
    pub svc_type: String,
}

impl K8sService {
    /// 唯一性复合键：`(name, namespace)`。
    pub fn composite_key(&self) -> String {
        format!("{}::{}", self.namespace, self.name)
    }
}

// ---------------------------------------------------------------------------
// Server（来自 K8s Node）
// ---------------------------------------------------------------------------

/// 由 K8s worker 节点派生的 Server 节点。
///
/// 以 `:Server` 节点持久化到图数据库。
/// 关系：`(:Server)-[:DEPLOYED_IN]->(:Environment)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K8sServer {
    /// 唯一服务器标识（通过对节点名做 SHA256 生成）。
    pub server_id: String,
    /// K8s 节点名（用作服务器名）。
    pub name: String,
    /// 节点主机名。
    pub hostname: String,
    /// 服务类型（K8s 节点始终为 `"kubernetes_node"`）。
    pub service_type: String,
    /// CPU 容量（例如 `"8"`）。
    pub cpu_cores: String,
    /// 内存容量（例如 `"32Gi"`）。
    pub memory_gb: String,
    /// 该集群的 Kuboard URL。
    pub url: String,
    /// 人类可读的描述。
    pub description: String,
}

impl K8sServer {
    /// 根据节点名生成 `server_id`。
    pub fn make_server_id(node_name: &str) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(node_name.as_bytes());
        format!("server::k8s::{}", hex::encode(&hash[..10]))
    }
}

// ---------------------------------------------------------------------------
// 底层 API 响应类型（K8s JSON 结构）
// ---------------------------------------------------------------------------

/// K8s API 的分页列表响应。
#[derive(Debug, Deserialize)]
pub(crate) struct K8sItemList<T> {
    pub items: Vec<T>,
}

/// 通用 K8s ObjectMeta（子集）。
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

// ── Pod（用于 CLI 展示，不持久化到图数据库） ───────────

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
        assert_eq!(id.len(), "server::k8s::".len() + 20); // 10 字节 hex
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
