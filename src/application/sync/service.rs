//! Sync service — orchestration layer for external system synchronisation.
//!
//! Provides the [`SyncService`] trait and a concrete [`NacosSyncService`]
//! that coordinates config and service sync from Nacos into Memgraph.
//!
//! # WriteCoordinator integration
//!
//! Before starting, the service checks whether a build is in progress via
//! [`WriteCoordinator::has_active_writes`]. If `true`, the sync is skipped
//! and each source returns [`SyncReport::skipped`].

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::sync::Arc;
use std::time::Instant;

use super::nacos::client::NacosClient;
use super::nacos::config_sync::ConfigSyncSource;
use super::nacos::service_sync::ServiceSyncSource;
use super::traits::{SyncReport, SyncSource};

// ---------------------------------------------------------------------------
// NacosConfig — parsed from config.yaml
// ---------------------------------------------------------------------------

/// Nacos connection configuration, parsed from config.yaml's `services.nacos` section.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct NacosConfig {
    /// Test environment URL.
    pub test: String,
    /// Production environment URL.
    pub prod: String,
}

/// The subset of config.yaml needed by the sync subsystem.
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

/// Orchestrates sync operations for one or more external systems.
///
/// Implementations manage the lifecycle: check for conflicts, run sources,
/// aggregate reports, and emit tracing spans.
#[async_trait]
pub trait SyncService: Send + Sync {
    /// Run all registered sync sources for the given environment.
    ///
    /// Returns a vector of per-source reports.
    async fn sync(&self, env: &str) -> Result<Vec<SyncReport>, DtError>;

    /// Sync knowledge-graph business-label nodes to the Qdrant vector store.
    ///
    /// When `incremental` is `true`, only nodes whose `_kg_synced_at` property
    /// is `NULL` are processed.  Default implementation returns an error —
    /// override this for sync services that have access to an embedder and
    /// vector repository.
    async fn kg_sync(&self, _incremental: bool) -> Result<SyncReport, DtError> {
        Err(DtError::Config(
            "kg_sync is not implemented for this sync service".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// NacosSyncService
// ---------------------------------------------------------------------------

/// Concrete [`SyncService`] that syncs Nacos configuration and service registry
/// data into the Memgraph knowledge graph.
pub struct NacosSyncService {
    /// Graph repository for writing V2 nodes.
    graph: Arc<dyn GraphRepository>,
    /// HTTP client for Nacos REST API.
    nacos_config: NacosConfig,
}

impl NacosSyncService {
    /// Create a new service from a graph repository and Nacos config.
    pub fn new(graph: Arc<dyn GraphRepository>, nacos_config: NacosConfig) -> Self {
        Self {
            graph,
            nacos_config,
        }
    }

    /// Load Nacos config from a YAML config file path.
    ///
    /// Reads the `services.nacos` section from `config.yaml`.
    pub fn from_config_file(
        graph: Arc<dyn GraphRepository>,
        config_path: &str,
    ) -> Result<Self, DtError> {
        let content = std::fs::read_to_string(config_path)
            .map_err(|e| DtError::Config(format!("cannot read config file {config_path}: {e}")))?;

        let cfg: SyncAppConfig = serde_yaml::from_str(&content)
            .map_err(|e| DtError::Config(format!("invalid config.yaml: {e}")))?;

        let nacos_config = cfg
            .services
            .nacos
            .ok_or_else(|| DtError::Config("config.yaml missing services.nacos section".into()))?;

        Ok(Self::new(graph, nacos_config))
    }

    /// Resolve the Nacos base URL for the given environment.
    fn resolve_url(&self, env: &str) -> Result<&str, DtError> {
        match env {
            "test" => Ok(&self.nacos_config.test),
            "prod" => Ok(&self.nacos_config.prod),
            _ => Err(DtError::Config(format!(
                "unknown Nacos env '{env}': expected 'test' or 'prod'"
            ))),
        }
    }
}

#[async_trait]
impl SyncService for NacosSyncService {
    async fn sync(&self, env: &str) -> Result<Vec<SyncReport>, DtError> {
        let base_url = self.resolve_url(env)?;
        let env_label = format!("nacos/{env}");

        tracing::info!(
            "[nacos-sync] starting sync for {env_label} ({base_url})"
        );

        let client = NacosClient::new(base_url);
        let mut reports: Vec<SyncReport> = Vec::with_capacity(2);

        // ── Config sync ───────────────────────────────────────────
        let config_source = ConfigSyncSource::new(client.clone(), env.to_string());
        let start = Instant::now();
        let report = config_source.sync(self.graph.as_ref()).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        let final_report = SyncReport {
            elapsed_ms: elapsed,
            ..report
        };
        tracing::info!(
            "[nacos-sync] config sync complete: {} configs across {} namespaces ({}ms)",
            final_report.configs,
            final_report.namespaces,
            final_report.elapsed_ms,
        );
        reports.push(final_report);

        // ── Service sync ──────────────────────────────────────────
        let service_source = ServiceSyncSource::new(client, env.to_string());
        let start = Instant::now();
        let report = service_source.sync(self.graph.as_ref()).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        let final_report = SyncReport {
            elapsed_ms: elapsed,
            ..report
        };
        tracing::info!(
            "[nacos-sync] service sync complete: {} services across {} namespaces ({}ms)",
            final_report.services,
            final_report.namespaces,
            final_report.elapsed_ms,
        );
        reports.push(final_report);

        Ok(reports)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::domain::traits::GraphRepository;
    use crate::domain::types::HealthStatus;
    use std::collections::HashMap;

    /// Minimal mock GraphRepository.
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
        let cfg: SyncAppConfig = serde_yaml::from_str(yaml).expect("parse");
        let n = cfg.services.nacos.expect("nacos section");
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
