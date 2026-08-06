//! Jenkins 插件——原生 REST API 客户端（不依赖外部二进制）。
//!
//! 所有 Jenkins 操作均通过直接 HTTP 调用 `JenkinsApiClient` 完成。

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;

use crate::application::plugins::jenkins::client::JenkinsApiClient;
use crate::application::plugins::Plugin;
use crate::domain::error::DtError;

/// 基于原生 HTTP 客户端的 Jenkins CI/CD 插件。
#[derive(Default)]
pub struct JenkinsPluginService {
    client: JenkinsApiClient,
}

impl JenkinsPluginService {
    /// 创建新的 Jenkins 插件服务。
    pub fn new(client: JenkinsApiClient) -> Self {
        Self { client }
    }

    // ── 面向 CLI 的方法 ──────────────────────────────────────────────────

    /// 列出所有 Jenkins 作业。
    pub async fn list_jobs(&self) -> Result<String, DtError> {
        self.client.list_jobs().await
    }

    /// 显示某个作业的构建参数。
    pub async fn get_params(&self, job: &str) -> Result<String, DtError> {
        self.client.get_params(job).await
    }

    /// 显示某个作业的构建历史。
    pub async fn get_history(&self, job: &str, limit: Option<u32>) -> Result<String, DtError> {
        self.client.get_history(job, limit).await
    }

    /// 获取某个作业指定构建的控制台输出。
    pub async fn get_build_log(&self, job: &str, build: Option<&str>) -> Result<String, DtError> {
        self.client.get_build_log(job, build).await
    }

    /// 触发某个作业的构建。
    ///
    /// 参数：
    /// - `job`      — Jenkins 作业名
    /// - `params`   — 构建参数，形如 `&[(key, value)]`
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
        // TODO: proto 编译完成后装配生成的 JenkinsPluginServer
        Ok(())
    }

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        ctx.log.info("[jenkins] 插件已初始化（原生 HTTP 客户端）");
        Ok(())
    }

    async fn health(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    async fn shutdown(&self) -> Result<(), PluginError> {
        Ok(())
    }
}
