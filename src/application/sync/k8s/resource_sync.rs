//! ResourceSyncSource — K8s resource sync for V2 schema.
//!
//! Fetches deployments, services, and nodes from K8s via the Kuboard proxy
//! and writes them as `K8sDeployment`, `K8sService`, and `Server` nodes in
//! Neo4j.
//!
//! ## What is synced
//!
//! | K8s Object    | Neo4j Label     | Sync Condition        |
//! |---------------|-----------------|-----------------------|
//! | Deployment    | K8sDeployment   | always                |
//! | Service       | K8sService      | always                |
//! | Node (Worker) | Server          | full sync only        |

use std::collections::HashMap;
use std::time::Instant;

use super::client::KuboardClient;
use crate::application::sync::k8s::types::{DeploymentItem, K8sServer, NodeItem, ServiceItem};
use super::K8sSyncConfig;
use crate::shared::coordinator::WriteCoordinator;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ============================================================================
// K8sSyncSummary — per-resource-type report
// ============================================================================

/// Summary of a single K8s resource sync operation.
#[derive(Debug, Clone)]
pub struct K8sSyncSummary {
    /// Resource type name (e.g. "k8s/deployments").
    pub resource: String,
    /// Number of items fetched from the K8s API.
    pub items_fetched: usize,
    /// Items written to Neo4j (created or updated via MERGE).
    pub items_written: usize,
    /// Items skipped due to write coordinator conflict.
    pub items_skipped: usize,
    /// Items that failed to write.
    pub items_failed: usize,
    /// Error messages collected during sync.
    pub errors: Vec<String>,
    /// Wall-clock elapsed time in milliseconds.
    pub elapsed_ms: u64,
}

impl K8sSyncSummary {
    fn with_errors(
        resource: impl Into<String>,
        fetched: usize,
        written: usize,
        failed: usize,
        errors: Vec<String>,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            resource: resource.into(),
            items_fetched: fetched,
            items_written: written,
            items_skipped: 0,
            items_failed: failed,
            errors,
            elapsed_ms,
        }
    }

    /// Returns `true` if no write errors occurred.
    pub fn is_success(&self) -> bool {
        self.items_failed == 0 && self.errors.is_empty()
    }
}

// ============================================================================
// K8sResourceSync — unified entry point
// ============================================================================

/// Orchestrates the full K8s resource sync: deployments, services, nodes.
pub struct K8sResourceSync {
    config: K8sSyncConfig,
    /// Optional limit for testing/dry-runs.
    limit: Option<usize>,
}

impl K8sResourceSync {
    /// Create a new sync instance.
    pub fn new(config: K8sSyncConfig) -> Self {
        Self {
            config,
            limit: None,
        }
    }

    /// Set an item limit per resource type (useful for testing/dev).
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Run the complete sync: login, fetch, write.
    ///
    /// Returns a vector of per-resource-type summaries.
    pub async fn run(
        &self,
        graph: &dyn GraphRepository,
    ) -> Result<Vec<K8sSyncSummary>, DtError> {
        tracing::info!("[k8s] Connecting to Kuboard ({})...", self.config.server);
        let client = KuboardClient::connect(self.config.clone()).await?;
        tracing::info!("[k8s] Authenticated successfully");

        let coordinator = WriteCoordinator::new();
        let mut summaries = Vec::new();

        let namespaces = self.config.effective_namespaces();

        // ── Ensure namespace nodes exist ──
        for ns in &namespaces {
            ensure_namespace(graph, ns).await?;
        }

        // ── Deployments ──
        {
            let start = Instant::now();
            let mut all_deployments = Vec::new();
            for ns in &namespaces {
                let mut items = client.fetch_deployments(ns).await;
                if let Some(l) = self.limit {
                    items.truncate(l);
                }
                all_deployments.extend(items);
            }

            let summary = sync_deployments(graph, &all_deployments, &coordinator, start).await?;
            summaries.push(summary);
        }

        // ── Services ──
        {
            let start = Instant::now();
            let mut all_services = Vec::new();
            for ns in &namespaces {
                let mut items = client.fetch_services(ns).await;
                if let Some(l) = self.limit {
                    items.truncate(l);
                }
                all_services.extend(items);
            }

            let summary = sync_services(graph, &all_services, &coordinator, start).await?;
            summaries.push(summary);
        }

        // ── Nodes → Servers (from pods via nodeName + hostIP) ──
        if self.limit.is_none() {
            let start = Instant::now();
            let mut node_map: std::collections::HashMap<String, (String, String)> =
                std::collections::HashMap::new();
            for ns in &namespaces {
                let pods = client.fetch_pods(ns).await;
                for pod in pods {
                    let node = pod.spec.node_name.unwrap_or_default();
                    let ip = pod.status.host_ip.unwrap_or_default();
                    if !node.is_empty() && !ip.is_empty() {
                        node_map.entry(node.clone()).or_insert((node, ip));
                    }
                }
            }
            let servers: Vec<K8sServer> = node_map
                .into_values()
                .map(|(name, ip)| K8sServer {
                    server_id: K8sServer::make_server_id(&name),
                    name: name.clone(),
                    hostname: ip,
                    service_type: "kubernetes_node".into(),
                    cpu_cores: String::new(),
                    memory_gb: String::new(),
                    url: String::new(),
                    description: format!("K8s node {}", name),
                })
                .collect();
            let summary = sync_servers(graph, &servers, &coordinator, start).await?;
            summaries.push(summary);
        }

        // ── Cross-linking ──
        run_cross_linking(graph, &namespaces).await?;

        Ok(summaries)
    }
}

// ============================================================================
// Deployment sync
// ============================================================================

async fn sync_deployments(
    graph: &dyn GraphRepository,
    items: &[DeploymentItem],
    _coordinator: &WriteCoordinator,
    start: Instant,
) -> Result<K8sSyncSummary, DtError> {
    let fetched = items.len();
    let mut written = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for dep in items {
        let name = &dep.metadata.name;
        let ns = dep.metadata.namespace.as_deref().unwrap_or("default");

        let image = dep
            .spec
            .template
            .spec
            .containers
            .first()
            .map(|c| c.image.clone())
            .unwrap_or_default();

        let replicas = dep.spec.replicas.unwrap_or(0);
        let strategy = dep
            .spec
            .strategy
            .as_ref()
            .and_then(|s| s.strategy_type.as_deref())
            .unwrap_or("RollingUpdate")
            .to_string();

        let available = dep.status.available_replicas.unwrap_or(0);
        let labels = dep
            .metadata
            .labels
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default())
            .unwrap_or_else(|| "{}".to_string());

        let created_at = dep
            .metadata
            .creation_timestamp
            .as_deref()
            .unwrap_or("")
            .to_string();

        // Write K8sDeployment node
        let params: HashMap<String, serde_json::Value> = [
            ("name".to_string(), serde_json::Value::String(name.clone())),
            ("ns".to_string(), serde_json::Value::String(ns.to_string())),
            ("image".to_string(), serde_json::Value::String(image)),
            ("replicas".to_string(), serde_json::json!(replicas)),
            ("available".to_string(), serde_json::json!(available)),
            ("strategy".to_string(), serde_json::Value::String(strategy)),
            ("labels".to_string(), serde_json::Value::String(labels)),
            ("created_at".to_string(), serde_json::Value::String(created_at)),
        ]
        .into_iter()
        .collect();

        let query = r#"
            MERGE (d:K8sDeployment {name: $name, namespace: $ns})
            ON CREATE SET
                d.image = $image, d.replicas = $replicas,
                d.available = $available, d.strategy = $strategy,
                d.labels = $labels, d.created_at = $created_at
            ON MATCH SET
                d.image = $image, d.replicas = $replicas,
                d.available = $available, d.strategy = $strategy,
                d.labels = $labels, d.created_at = $created_at
        "#;

        match graph.write_query(query, params).await {
            Ok(_) => {
                written += 1;
            }
            Err(e) => {
                failed += 1;
                errors.push(format!("deployment {}/{}: {e}", ns, name));
                continue;
            }
        };

        // Link Deployment → Namespace
        let link_params: HashMap<String, serde_json::Value> = [
            ("ns".to_string(), serde_json::Value::String(ns.to_string())),
            ("name".to_string(), serde_json::Value::String(name.clone())),
        ]
        .into_iter()
        .collect();

        let link_query = r#"
            MATCH (ns:Namespace {name: $ns})
            MATCH (d:K8sDeployment {name: $name, namespace: $ns})
            MERGE (ns)-[:HAS_DEPLOYMENT]->(d)
        "#;

        let _ = graph.write_query(link_query, link_params).await;
    }

    Ok(K8sSyncSummary::with_errors(
        "k8s/deployments",
        fetched,
        written,
        failed,
        errors,
        start.elapsed().as_millis() as u64,
    ))
}

// ============================================================================
// Service sync
// ============================================================================

async fn sync_services(
    graph: &dyn GraphRepository,
    items: &[ServiceItem],
    _coordinator: &WriteCoordinator,
    start: Instant,
) -> Result<K8sSyncSummary, DtError> {
    let fetched = items.len();
    let mut written = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for svc in items {
        let name = &svc.metadata.name;
        let ns = svc.metadata.namespace.as_deref().unwrap_or("default");
        let svc_type = svc.spec.svc_type.as_deref().unwrap_or("ClusterIP").to_string();
        let cluster_ip = svc.spec.cluster_ip.as_deref().unwrap_or("").to_string();

        let params: HashMap<String, serde_json::Value> = [
            ("name".to_string(), serde_json::Value::String(name.clone())),
            ("ns".to_string(), serde_json::Value::String(ns.to_string())),
            ("svc_type".to_string(), serde_json::Value::String(svc_type)),
            ("cluster_ip".to_string(), serde_json::Value::String(cluster_ip)),
        ]
        .into_iter()
        .collect();

        let query = r#"
            MERGE (s:K8sService {name: $name, namespace: $ns})
            ON CREATE SET s.type = $svc_type, s.cluster_ip = $cluster_ip
            ON MATCH SET s.type = $svc_type, s.cluster_ip = $cluster_ip
        "#;

        match graph.write_query(query, params).await {
            Ok(_) => {
                written += 1;
            }
            Err(e) => {
                failed += 1;
                errors.push(format!("service {}/{}: {e}", ns, name));
                continue;
            }
        };

        // Link Service → Namespace
        let link_params: HashMap<String, serde_json::Value> = [
            ("ns".to_string(), serde_json::Value::String(ns.to_string())),
            ("name".to_string(), serde_json::Value::String(name.clone())),
        ]
        .into_iter()
        .collect();

        let link_query = r#"
            MATCH (ns:Namespace {name: $ns})
            MATCH (s:K8sService {name: $name, namespace: $ns})
            MERGE (ns)-[:HAS_SERVICE]->(s)
        "#;

        let _ = graph.write_query(link_query, link_params).await;
    }

    Ok(K8sSyncSummary::with_errors(
        "k8s/services",
        fetched,
        written,
        failed,
        errors,
        start.elapsed().as_millis() as u64,
    ))
}

// ============================================================================
// Server sync (from K8s Nodes)
// ============================================================================

async fn sync_servers(
    graph: &dyn GraphRepository,
    items: &[K8sServer],
    _coordinator: &WriteCoordinator,
    start: Instant,
) -> Result<K8sSyncSummary, DtError> {
    let fetched = items.len();
    let mut written = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for srv in items {
        let params: HashMap<String, serde_json::Value> = [
            ("server_id".to_string(), serde_json::Value::String(srv.server_id.clone())),
            ("name".to_string(), serde_json::Value::String(srv.name.clone())),
            ("hostname".to_string(), serde_json::Value::String(srv.hostname.clone())),
            ("service_type".to_string(), serde_json::Value::String(srv.service_type.clone())),
            ("cpu_cores".to_string(), serde_json::Value::String(srv.cpu_cores.clone())),
            ("memory_gb".to_string(), serde_json::Value::String(srv.memory_gb.clone())),
        ]
        .into_iter()
        .collect();

        let query = r#"
            MERGE (s:Server {server_id: $server_id})
            ON CREATE SET
                s.name = $name, s.hostname = $hostname,
                s.service_type = $service_type,
                s.cpu_cores = $cpu_cores, s.memory_gb = $memory_gb
            ON MATCH SET
                s.name = $name, s.hostname = $hostname,
                s.service_type = $service_type,
                s.cpu_cores = $cpu_cores, s.memory_gb = $memory_gb
        "#;

        match graph.write_query(query, params).await {
            Ok(_) => {
                written += 1;
            }
            Err(e) => {
                errors.push(format!("server {}: {e}", srv.name));
            }
        };
    }

    Ok(K8sSyncSummary::with_errors(
        "k8s/servers",
        fetched,
        written,
        errors.len(),
        errors,
        start.elapsed().as_millis() as u64,
    ))
}

// ---------------------------------------------------------------------------
// Namespace helper
// ---------------------------------------------------------------------------

async fn ensure_namespace(graph: &dyn GraphRepository, ns: &str) -> Result<(), DtError> {
    let params: HashMap<String, serde_json::Value> = [
        ("name".to_string(), serde_json::Value::String(ns.to_string())),
    ]
    .into_iter()
    .collect();

    graph
        .write_query("MERGE (ns:Namespace {name: $name})", params)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cross-linking
// ---------------------------------------------------------------------------

async fn run_cross_linking(
    graph: &dyn GraphRepository,
    namespaces: &[String],
) -> Result<(), DtError> {
    for ns in namespaces {
        // ── K8sService → NacosService (name match) ──
        let _ = graph
            .write_query(
                r#"
                MATCH (svc:K8sService {namespace: $k8s_ns})
                MATCH (ns:NacosService)
                WHERE svc.name = ns.name
                   OR (svc.name STARTS WITH ns.name
                       AND (svc.name ENDS WITH '-stable' OR svc.name ENDS WITH '-svc'))
                MERGE (svc)-[:EXPOSES]->(ns)
                "#,
                [("k8s_ns".to_string(), serde_json::Value::String(ns.to_string()))]
                    .into_iter()
                    .collect(),
            )
            .await;

        // ── K8sDeployment → NacosConfig (name prefix match) ──
        let _ = graph
            .write_query(
                r#"
                MATCH (d:K8sDeployment {namespace: $k8s_ns})
                MATCH (c:NacosConfig)
                WITH d, c,
                    replace(replace(d.name, '-stable', ''), '-svc', '') AS dep_base,
                    split(c.data_id, '.')[0] AS cfg_raw
                WITH d, c, dep_base,
                    replace(replace(replace(cfg_raw, '-prod', ''), '-test', ''), '_test', '') AS cfg_base
                WHERE dep_base = cfg_base OR dep_base CONTAINS cfg_base OR cfg_base CONTAINS dep_base
                MERGE (d)-[:CONFIGURED_BY]->(c)
                "#,
                [("k8s_ns".to_string(), serde_json::Value::String(ns.to_string()))]
                    .into_iter()
                    .collect(),
            )
            .await;

        // ── K8sDeployment → NacosService (name match) ──
        let _ = graph
            .write_query(
                r#"
                MATCH (d:K8sDeployment {namespace: $k8s_ns})
                MATCH (ns:NacosService)
                WHERE d.name = ns.name
                   OR (d.name STARTS WITH ns.name
                       AND (d.name ENDS WITH '-stable' OR d.name ENDS WITH '-svc'))
                MERGE (d)-[:DEPLOYS]->(ns)
                "#,
                [("k8s_ns".to_string(), serde_json::Value::String(ns.to_string()))]
                    .into_iter()
                    .collect(),
            )
            .await;

        // ── K8s Namespace → NacosNamespace (env match) ──
        let env_name = if ns == "newoffen" { "prod" } else { "test" };
        let _ = graph
            .write_query(
                r#"
                MATCH (kns:Namespace {name: $k8s_ns})
                MATCH (nns:NacosNamespace)
                WHERE nns.namespace CONTAINS $env OR nns.description CONTAINS $env
                MERGE (kns)-[:MAPS_TO]->(nns)
                "#,
                [
                    ("k8s_ns".to_string(), serde_json::Value::String(ns.to_string())),
                    ("env".to_string(), serde_json::Value::String(env_name.to_string())),
                ]
                    .into_iter()
                    .collect(),
            )
            .await;

        // ── Server → Namespace(K8s) (NODE_IN) ──
        let _ = graph
            .write_query(
                r#"
                MATCH (s:Server)
                MATCH (ns:Namespace {name: $k8s_ns})
                MERGE (s)-[:NODE_IN]->(ns)
                "#,
                [("k8s_ns".to_string(), serde_json::Value::String(ns.to_string()))]
                    .into_iter()
                    .collect(),
            )
            .await;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::domain::types::HealthStatus;

    struct MockGraph {
        write_queries: std::sync::Mutex<Vec<String>>,
    }

    impl MockGraph {
        fn new() -> Self {
            Self {
                write_queries: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::json!([]))
        }

        async fn write_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_queries.lock().unwrap().push(query.to_string());
            Ok(serde_json::json!({"ok": true}))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    fn make_test_deployment(name: &str, ns: &str) -> DeploymentItem {
        DeploymentItem {
            metadata: crate::application::sync::k8s::types::K8sMeta {
                name: name.to_string(),
                namespace: Some(ns.to_string()),
                uid: Some(format!("uid-{}", name)),
                labels: Some(
                    [("app".to_string(), name.to_string())]
                        .into_iter()
                        .collect(),
                ),
                creation_timestamp: Some("2026-01-01T00:00:00Z".to_string()),
                owner_references: None,
            },
            spec: crate::application::sync::k8s::types::DeploymentSpec {
                replicas: Some(3),
                strategy: Some(crate::application::sync::k8s::types::DeploymentStrategy {
                    strategy_type: Some("RollingUpdate".to_string()),
                }),
                template: crate::application::sync::k8s::types::PodTemplateSpec {
                    spec: crate::application::sync::k8s::types::PodSpec {
                        containers: vec![crate::application::sync::k8s::types::ContainerItem {
                            name: "app".to_string(),
                            image: "registry/app:v1".to_string(),
                            ports: None,
                            resources: None,
                        }],
                        node_name: None,
                    },
                },
            },
            status: crate::application::sync::k8s::types::DeploymentStatus {
                available_replicas: Some(3),
                conditions: None,
            },
        }
    }

    fn make_test_service(name: &str, ns: &str) -> ServiceItem {
        ServiceItem {
            metadata: crate::application::sync::k8s::types::K8sMeta {
                name: name.to_string(),
                namespace: Some(ns.to_string()),
                uid: Some(format!("uid-{}", name)),
                labels: None,
                creation_timestamp: None,
                owner_references: None,
            },
            spec: crate::application::sync::k8s::types::ServiceSpec {
                svc_type: Some("ClusterIP".to_string()),
                cluster_ip: Some("10.0.0.1".to_string()),
                selector: None,
                ports: None,
            },
        }
    }

    fn make_test_node(name: &str) -> NodeItem {
        NodeItem {
            metadata: crate::application::sync::k8s::types::K8sMeta {
                name: name.to_string(),
                namespace: None,
                uid: Some(format!("uid-{}", name)),
                labels: None,
                creation_timestamp: None,
                owner_references: None,
            },
            status: crate::application::sync::k8s::types::NodeStatus {
                capacity: Some(crate::application::sync::k8s::types::NodeResources {
                    cpu: Some("8".to_string()),
                    memory: Some("32Gi".to_string()),
                    pods: Some("110".to_string()),
                }),
                addresses: Some(vec![crate::application::sync::k8s::types::NodeAddress {
                    addr_type: Some("InternalIP".to_string()),
                    address: Some("192.168.1.10".to_string()),
                }]),
            },
        }
    }

    #[tokio::test]
    async fn sync_deployments_writes_nodes() {
        let graph = MockGraph::new();
        let items = vec![make_test_deployment("myapp", "newoffen")];
        let summary = sync_deployments(&graph, &items, &WriteCoordinator::new(), Instant::now())
            .await
            .unwrap();

        assert_eq!(summary.items_fetched, 1);
        assert_eq!(summary.items_written, 1);
        assert!(summary.is_success());

        let queries = graph.write_queries.lock().unwrap();
        assert!(queries.iter().any(|q| q.contains("MERGE (d:K8sDeployment")));
        assert!(queries.iter().any(|q| q.contains("HAS_DEPLOYMENT")));
    }

    #[tokio::test]
    async fn sync_services_writes_nodes() {
        let graph = MockGraph::new();
        let items = vec![make_test_service("myapp-svc", "newoffen")];
        let summary = sync_services(&graph, &items, &WriteCoordinator::new(), Instant::now())
            .await
            .unwrap();

        assert_eq!(summary.items_fetched, 1);
        assert_eq!(summary.items_written, 1);
        assert!(summary.is_success());

        let queries = graph.write_queries.lock().unwrap();
        assert!(queries.iter().any(|q| q.contains("MERGE (s:K8sService")));
        assert!(queries.iter().any(|q| q.contains("HAS_SERVICE")));
    }

    #[tokio::test]
    async fn sync_servers_writes_nodes() {
        let graph = MockGraph::new();
        let items = vec![make_test_node("worker-1")];
        let summary = sync_servers(&graph, &items, &WriteCoordinator::new(), Instant::now())
            .await
            .unwrap();

        assert_eq!(summary.items_fetched, 1);
        assert_eq!(summary.items_written, 1);
        assert!(summary.is_success());

        let queries = graph.write_queries.lock().unwrap();
        assert!(queries.iter().any(|q| q.contains("MERGE (s:Server")));
    }

    #[test]
    fn k8s_sync_summary_with_errors() {
        let s = K8sSyncSummary::with_errors(
            "k8s/test",
            10,
            8,
            2,
            vec!["err1".into()],
            200,
        );
        assert_eq!(s.items_fetched, 10);
        assert_eq!(s.items_written, 8);
        assert_eq!(s.items_failed, 2);
        assert!(!s.is_success());
    }
}
