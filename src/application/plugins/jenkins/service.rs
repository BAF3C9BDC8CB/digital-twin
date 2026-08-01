//! Jenkins Plugin — native REST API client (no external binary).
//!
//! Uses `JenkinsApiClient` for all Jenkins operations via direct HTTP.

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;

use crate::application::plugins::jenkins::client::JenkinsApiClient;
use crate::application::plugins::Plugin;
use crate::domain::error::DtError;

/// Jenkins CI/CD plugin backed by native HTTP client.
#[derive(Default)]
pub struct JenkinsPluginService {
    client: JenkinsApiClient,
}

impl JenkinsPluginService {
    /// Create a new Jenkins plugin service.
    pub fn new(client: JenkinsApiClient) -> Self {
        Self { client }
    }

    // ── CLI-facing methods ──────────────────────────────────────────────────

    /// List all Jenkins jobs.
    pub async fn list_jobs(&self) -> Result<String, DtError> {
        self.client.list_jobs().await
    }

    /// Show build parameters for a job.
    pub async fn get_params(&self, job: &str) -> Result<String, DtError> {
        self.client.get_params(job).await
    }

    /// Show build history for a job.
    pub async fn get_history(&self, job: &str, limit: Option<u32>) -> Result<String, DtError> {
        self.client.get_history(job, limit).await
    }

    /// Get console output for a specific build of a job.
    pub async fn get_build_log(&self, job: &str, build: Option<&str>) -> Result<String, DtError> {
        self.client.get_build_log(job, build).await
    }

    /// Trigger a build for a job.
    ///
    /// Parameters:
    /// - `job`      — Jenkins job name
    /// - `params`   — build parameters as `&[(key, value)]`
    pub async fn trigger_build(
        &self,
        job: &str,
        params: &[(&str, &str)],
    ) -> Result<String, DtError> {
        self.client.trigger_build(job, params).await
    }
}

#[async_trait]
impl Plugin for JenkinsPluginService {
    fn id(&self) -> &'static str {
        "jenkins"
    }

    fn name(&self) -> &'static str {
        "Jenkins CI/CD Operations"
    }

    fn version(&self) -> &'static str {
        "0.2.0"
    }

    fn register_grpc(
        &self,
        _server: &mut tonic::transport::server::Server,
    ) -> Result<(), PluginError> {
        // TODO: wire generated JenkinsPluginServer when proto is compiled
        Ok(())
    }

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        ctx.log
            .info("[jenkins] plugin initialized (native HTTP client)");
        Ok(())
    }

    async fn health(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }
}
