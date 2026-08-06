//! K8s 插件——原生 Kuboard K8s API 客户端（不依赖外部二进制）。
//!
//! 复用 `src/application/sync/k8s/client.rs` 中现有的 `KuboardClient`
//! 执行所有 Kubernetes 操作。无子进程调用——全部通过 reqwest 走 HTTP。

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;

use crate::application::plugins::Plugin;
use crate::application::sync::k8s::client::KuboardClient;
use crate::application::sync::k8s::types::{DeploymentItem, PodItem, ServiceItem};
use crate::application::sync::k8s::K8sSyncConfig;
use crate::domain::error::DtError;

/// 基于原生 Kuboard HTTP 客户端的 K8s 插件服务。
#[derive(Default)]
pub struct K8sPluginService {
    /// Kuboard 客户端，惰性初始化。
    /// 通过 async init 方法使用内部可变模式。
    client: Option<KuboardClient>,
    config: Option<K8sSyncConfig>,
}

impl K8sPluginService {
    /// 创建新的 K8s 插件服务（暂不连接）。
    pub fn new(config: K8sSyncConfig) -> Self {
        Self {
            client: None,
            config: Some(config),
        }
    }

    /// 连接到 Kuboard（使用 CLI 方法前必须先调用）。
    pub async fn connect(&mut self) -> Result<(), DtError> {
        if let Some(cfg) = self.config.take() {
            self.client = Some(KuboardClient::connect(cfg).await?);
        }
        Ok(())
    }

    /// 获取客户端引用的私有辅助方法。
    fn client(&self) -> Result<&KuboardClient, DtError> {
        self.client
            .as_ref()
            .ok_or_else(|| DtError::General("K8s 客户端未连接——请先调用 connect()".into()))
    }

    // ── 面向 CLI 的方法 ──────────────────────────────────────────────────

    /// 获取指定命名空间的 Pod 列表。
    pub async fn get_pods(&self, namespace: &str) -> Result<String, DtError> {
        let pods = self.client()?.fetch_pods(namespace).await;
        Ok(format_pods_table(&pods))
    }

    /// 获取指定命名空间的 Deployment 列表。
    pub async fn get_deployments(&self, namespace: &str) -> Result<String, DtError> {
        let deps = self.client()?.fetch_deployments(namespace).await;
        Ok(format_deployments_table(&deps))
    }

    /// 获取指定命名空间的 Service 列表。
    pub async fn get_services(&self, namespace: &str) -> Result<String, DtError> {
        let svcs = self.client()?.fetch_services(namespace).await;
        Ok(format_services_table(&svcs))
    }

    /// 获取 Pod 日志（文本）。
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

    /// 将 Pod 日志下载到本地文件。
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
        Ok(format!("日志已写入 {output_path}"))
    }

    /// 对某类 K8s 资源（pods、deploy、svc）的通用状态查询。
    pub async fn get_status(&self, resource: &str, namespace: &str) -> Result<String, DtError> {
        match resource {
            "pods" => self.get_pods(namespace).await,
            "deploy" => self.get_deployments(namespace).await,
            "svc" => self.get_services(namespace).await,
            _ => Err(DtError::General(format!("未知资源类型: {resource}"))),
        }
    }
}

// ── 格式化辅助函数 ─────────────────────────────────────────────────────

fn format_pods_table(pods: &[PodItem]) -> String {
    if pods.is_empty() {
        return "(无 Pod)".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<10} {:<14} {:<12} {:<12} {:<22}\n",
        "名称", "就绪", "状态", "重启", "节点", "时长"
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
        return "(无 Deployment)".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<10} {:<10} {:<10}\n",
        "名称", "就绪", "期望", "策略"
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
        return "(无 Service)".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "{:<40} {:<14} {:<14} {:<30}\n",
        "名称", "类型", "集群IP", "端口"
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
        // TODO: proto 编译完成后装配生成的服务
        Ok(())
    }

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        ctx.log.info("[k8s] 插件已初始化（原生 Kuboard 客户端）");
        Ok(())
    }

    async fn health(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }
}
