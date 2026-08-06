//! 同步服务——外部系统同步的编排层。
//!
//! 提供 [`SyncService`] trait 以及具体的 [`NacosSyncService`]，
//! 协调 Nacos 的配置与服务同步写入 Memgraph。
//!
//! # WriteCoordinator 集成
//!
//! 开始前，服务会通过 [`WriteCoordinator::has_active_writes`] 检查
//! 是否有构建正在进行。若为 `true`，则跳过同步，各来源返回
//! [`SyncReport::skipped`]。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

use super::nacos::client::NacosClient;
use super::nacos::config_sync::ConfigSyncSource;
use super::nacos::service_sync::ServiceSyncSource;
use super::traits::{SyncReport, SyncSource};

// ---------------------------------------------------------------------------
// NacosConfig——从 config.yaml 解析
// ---------------------------------------------------------------------------

/// Nacos 连接配置，从 config.yaml 的 `services.nacos` 段解析。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NacosConfig {
    /// 测试环境 URL。
    pub test: String,
    /// 生产环境 URL。
    pub prod: String,
}

/// 同步子系统所需的 config.yaml 子集。
#[derive(Debug, Clone, serde::Deserialize)]
struct SyncAppConfig {
    services: ServicesSection,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ServicesSection {
    nacos: Option<NacosConfig>,
}

// ---------------------------------------------------------------------------
// SyncService trait
// ---------------------------------------------------------------------------

/// 为一个或多个外部系统编排同步操作。
///
/// 实现方管理生命周期：检查冲突、运行来源、汇总报告并发出 tracing 跨度。
#[async_trait]
pub trait SyncService: Send + Sync {
    /// 为给定环境运行所有已注册的同步来源。
    ///
    /// 返回每个来源的报告向量。
    async fn sync(&self, env: &str) -> Result<Vec<SyncReport>, DtError>;

    /// 将知识图谱业务标签节点同步到 Qdrant 向量库。
    ///
    /// 当 `incremental` 为 `true` 时，仅处理 `_kg_synced_at` 属性
    /// 为 `NULL` 的节点。默认实现返回错误——请为可访问向量化器和
    /// 向量仓库的同步服务覆写该方法。
    async fn kg_sync(&self, _incremental: bool) -> Result<SyncReport, DtError> {
        Err(DtError::Config("该同步服务未实现 kg_sync".into()))
    }
}

// ---------------------------------------------------------------------------
// NacosSyncService
// ---------------------------------------------------------------------------

/// 将 Nacos 配置与服务注册数据同步到 Memgraph 知识图谱的
/// 具体 [`SyncService`] 实现。
pub struct NacosSyncService {
    /// 用于写入 V2 节点的图仓库。
    graph: Arc<dyn GraphRepository>,
    /// Nacos REST API 的 HTTP 客户端。
    nacos_config: NacosConfig,
}

impl NacosSyncService {
    /// 从图仓库和 Nacos 配置创建新服务。
    pub fn new(graph: Arc<dyn GraphRepository>, nacos_config: NacosConfig) -> Self {
        Self {
            graph,
            nacos_config,
        }
    }

    /// 从 YAML 配置文件路径加载 Nacos 配置。
    ///
    /// 读取 `config.yaml` 中的 `services.nacos` 段。
    pub fn from_config_file(
        graph: Arc<dyn GraphRepository>,
        config_path: &str,
    ) -> Result<Self, DtError> {
        let content = std::fs::read_to_string(config_path)
            .map_err(|e| DtError::Config(format!("无法读取配置文件 {config_path}: {e}")))?;

        let cfg: SyncAppConfig = serde_yaml::from_str(&content)
            .map_err(|e| DtError::Config(format!("config.yaml 无效: {e}")))?;

        let nacos_config = cfg
            .services
            .nacos
            .ok_or_else(|| DtError::Config("config.yaml 缺少 services.nacos 段".into()))?;

        Ok(Self::new(graph, nacos_config))
    }

    /// 解析给定环境的 Nacos 基础 URL。
    fn resolve_url(&self, env: &str) -> Result<&str, DtError> {
        match env {
            "test" => Ok(&self.nacos_config.test),
            "prod" => Ok(&self.nacos_config.prod),
            _ => Err(DtError::Config(format!(
                "未知的 Nacos 环境 '{env}'：应为 'test' 或 'prod'"
            ))),
        }
    }
}

#[async_trait]
impl SyncService for NacosSyncService {
    async fn sync(&self, env: &str) -> Result<Vec<SyncReport>, DtError> {
        let base_url = self.resolve_url(env)?;
        let env_label = format!("nacos/{env}");

        tracing::info!("[nacos-sync] 开始为 {env_label} 同步（{base_url}）");

        let client = NacosClient::new(base_url);
        let mut reports: Vec<SyncReport> = Vec::with_capacity(2);

        // ── 配置同步 ───────────────────────────────────────────
        let config_source = ConfigSyncSource::new(client.clone(), env.to_string());
        let start = Instant::now();
        let report = config_source.sync(self.graph.as_ref()).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        let final_report = SyncReport {
            elapsed_ms: elapsed,
            ..report
        };
        tracing::info!(
            "[nacos-sync] 配置同步完成: {} 个配置分布在 {} 个命名空间（{}ms）",
            final_report.configs,
            final_report.namespaces,
            final_report.elapsed_ms,
        );
        reports.push(final_report);

        // ── 服务同步 ──────────────────────────────────────────
        let service_source = ServiceSyncSource::new(client, env.to_string());
        let start = Instant::now();
        let report = service_source.sync(self.graph.as_ref()).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        let final_report = SyncReport {
            elapsed_ms: elapsed,
            ..report
        };
        tracing::info!(
            "[nacos-sync] 服务同步完成: {} 个服务分布在 {} 个命名空间（{}ms）",
            final_report.services,
            final_report.namespaces,
            final_report.elapsed_ms,
        );
        reports.push(final_report);

        Ok(reports)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::traits::GraphRepository;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::collections::HashMap;

    /// 最小化模拟 GraphRepository。
    struct MockRepo;

    #[async_trait]
    impl GraphRepository for MockRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[test]
    fn nacos_config_deserialize_from_yaml() {
        let yaml = r#"
services:
  nacos:
    test: https://nacos-test.example.com/nacos
    prod: https://nacos-prod.example.com/nacos
"#;
        let cfg: SyncAppConfig = serde_yaml::from_str(yaml).expect("解析");
        let n = cfg.services.nacos.expect("nacos 段");
        assert_eq!(n.test, "https://nacos-test.example.com/nacos");
        assert_eq!(n.prod, "https://nacos-prod.example.com/nacos");
    }

    #[test]
    fn sync_report_skipped_fields() {
        let r = SyncReport::skipped("nacos");
        assert!(r.skipped);
        assert_eq!(r.configs, 0);
    }

    #[test]
    fn resolve_url_returns_correct_env() {
        let cfg = NacosConfig {
            test: "https://t.nacos/nacos".into(),
            prod: "https://p.nacos/nacos".into(),
        };
        let svc = NacosSyncService::new(Arc::new(MockRepo), cfg);
        assert_eq!(svc.resolve_url("test").unwrap(), "https://t.nacos/nacos");
        assert_eq!(svc.resolve_url("prod").unwrap(), "https://p.nacos/nacos");
        assert!(svc.resolve_url("staging").is_err());
    }
}
