//! K8s Plugin — native Kuboard K8s API client (no external binary).
//!
//! Reuses the existing `KuboardClient` from `src/application/sync/k8s/client.rs`
//! for all Kubernetes operations. No subprocess calls — all HTTP via reqwest.

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;

use crate::application::plugins::Plugin;
use crate::application::sync::k8s::client::KuboardClient;
use crate::application::sync::k8s::types::{DeploymentItem, PodItem, ServiceItem};
use crate::application::sync::k8s::K8sSyncConfig;
use crate::domain::error::DtError;

/// K8s plugin service backed by a native Kuboard HTTP client.
#[derive(Default)]
pub struct K8sPluginService {
    /// The Kuboard client, lazily initialized.
    /// Uses an internal mutability pattern via the async init method.
    client: Option<KuboardClient>,
    config: Option<K8sSyncConfig>,
}

impl K8sPluginService {
    /// Create a new K8s plugin service (does not connect yet).
    pub fn new(config: K8sSyncConfig) -> Self {
        Self {
            client: None,
            config: Some(config),
        }
    }

    /// Connect to Kuboard (must be called before using CLI methods).
    pub async fn connect(&mut self) -> Result<(), DtError> {
        if let Some(cfg) = self.config.take() {
            self.client = Some(KuboardClient::connect(cfg).await?);
        }
        Ok(())
    }

    /// Private helper to get the client reference.
    fn client(&self) -> Result<&KuboardClient, DtError> {
        self.client.as_ref().ok_or_else(|| {
            DtError::General("K8s client not connected — call connect() first".into())
        })
    }

    // ── CLI-facing methods ──────────────────────────────────────────────────

    /// Get Pod list for a namespace.
    pub async fn get_pods(&self, namespace: &str) -> Result<String, DtError> {
        let pods = self.client()?.fetch_pods(namespace).await;
        Ok(format_pods_table(&pods))
    }

    /// Get Deployment list for a namespace.
    pub async fn get_deployments(&self, namespace: &str) -> Result<String, DtError> {
        let deps = self.client()?.fetch_deployments(namespace).await;
        Ok(format_deployments_table(&deps))
    }

    /// Get Service list for a namespace.
    pub async fn get_services(&self, namespace: &str) -> Result<String, DtError> {
        let svcs = self.client()?.fetch_services(namespace).await;
        Ok(format_services_table(&svcs))
    }

    /// Get Pod logs (text).
    pub async fn get_logs(
        &self,
        pod: &str,
        namespace: &str,
        tail_lines: Option<u32>,
    ) -> Result<String, DtError> {
        self.client()?
            .get_pod_logs(pod, namespace, tail_lines)
            .await
    }

    /// Download Pod logs to a local file.
    pub async fn download_logs(
        &self,
        pod: &str,
        namespace: &str,
        tail_lines: Option<u32>,
        output_path: &str,
    ) -> Result<String, DtError> {
        let logs = self
            .client()?
            .get_pod_logs(pod, namespace, tail_lines)
            .await?;
        std::fs::write(output_path, &logs)?;
        Ok(format!("Logs written to {output_path}"))
    }

    /// Generic status query for a K8s resource type (pods, deploy, svc).
    pub async fn get_status(&self, resource: &str, namespace: &str) -> Result<String, DtError> {
        match resource {
            "pods" => self.get_pods(namespace).await,
            "deploy" => self.get_deployments(namespace).await,
            "svc" => self.get_services(namespace).await,
            _ => Err(DtError::General(format!("unknown resource: {resource}"))),
        }
    }
}

// ── Formatting helpers ─────────────────────────────────────────────────────

fn format_pods_table(pods: &[PodItem]) -> String {
    if pods.is_empty() {
        return "(no pods)".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<10} {:<14} {:<12} {:<12} {:<22}\n",
        "NAME", "READY", "STATUS", "RESTARTS", "NODE", "AGE"
    ));
    for p in pods {
        let name = &p.metadata.name;
        let phase = p.status.phase.as_deref().unwrap_or("?");
        let total_containers = p.status.container_statuses.as_ref().map_or(0, |c| c.len());
        let ready_count = p
            .status
            .container_statuses
            .as_ref()
            .map_or(0, |cs| cs.iter().filter(|c| c.ready).count());
        let restarts: i64 = p
            .status
            .container_statuses
            .as_ref()
            .map_or(0, |cs| cs.iter().filter_map(|c| c.restart_count).sum());
        let node = p.spec.node_name.as_deref().unwrap_or("-");
        let start = p.status.start_time.as_deref().unwrap_or("-");
        let age = if start != "-" {
            age_since(start)
        } else {
            "-".into()
        };

        out.push_str(&format!(
            "{:<40} {:<2}/{:<7} {:<14} {:<12} {:<12} {:<22}\n",
            truncate(name, 40),
            ready_count,
            total_containers,
            truncate(phase, 14),
            restarts,
            truncate(node, 12),
            truncate(&age, 22),
        ));
    }
    out
}

fn format_deployments_table(deps: &[DeploymentItem]) -> String {
    if deps.is_empty() {
        return "(no deployments)".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<10} {:<10} {:<10}\n",
        "NAME", "READY", "DESIRED", "STRATEGY"
    ));
    for d in deps {
        let name = &d.metadata.name;
        let desired = d.spec.replicas.unwrap_or(0);
        let available = d.status.available_replicas.unwrap_or(0);
        let strategy = d
            .spec
            .strategy
            .as_ref()
            .and_then(|s| s.strategy_type.as_deref())
            .unwrap_or("-");
        out.push_str(&format!(
            "{:<40} {:<2}/{:<7} {:<10} {:<10}\n",
            truncate(name, 40),
            available,
            desired,
            desired,
            truncate(strategy, 10),
        ));
    }
    out
}

fn format_services_table(svcs: &[ServiceItem]) -> String {
    if svcs.is_empty() {
        return "(no services)".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<14} {:<14} {:<30}\n",
        "NAME", "TYPE", "CLUSTER-IP", "PORTS"
    ));
    for s in svcs {
        let name = &s.metadata.name;
        let svc_type = s.spec.svc_type.as_deref().unwrap_or("-");
        let cluster_ip = s.spec.cluster_ip.as_deref().unwrap_or("-");
        let ports = s
            .spec
            .ports
            .as_ref()
            .map(|p_vec| {
                p_vec
                    .iter()
                    .filter_map(|p| {
                        let port = p.port?;
                        let proto = p.protocol.as_deref().unwrap_or("TCP");
                        let name = p.name.as_deref().unwrap_or("");
                        if name.is_empty() {
                            Some(format!("{port}/{proto}"))
                        } else {
                            Some(format!("{name}:{port}/{proto}"))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "{:<40} {:<14} {:<14} {:<30}\n",
            truncate(name, 40),
            truncate(svc_type, 14),
            truncate(cluster_ip, 14),
            truncate(&ports, 30),
        ));
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

fn age_since(rfc3339: &str) -> String {
    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(rfc3339) {
        let dur = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
        if dur.num_days() > 0 {
            format!("{}d", dur.num_days())
        } else if dur.num_hours() > 0 {
            format!("{}h", dur.num_hours())
        } else if dur.num_minutes() > 0 {
            format!("{}m", dur.num_minutes())
        } else {
            format!("{}s", dur.num_seconds())
        }
    } else {
        "?".into()
    }
}

#[async_trait]
impl Plugin for K8sPluginService {
    fn id(&self) -> &'static str {
        "k8s"
    }

    fn name(&self) -> &'static str {
        "Kubernetes Operations"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn register_grpc(
        &self,
        _server: &mut tonic::transport::server::Server,
    ) -> Result<(), PluginError> {
        // TODO: when proto is compiled, wire the generated service
        Ok(())
    }

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        ctx.log
            .info("[k8s] plugin initialized (native Kuboard client)");
        Ok(())
    }

    async fn health(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }
}
