//! 插件注册表——持有所有插件并管理其生命周期。
//!
//! 注册表在守护进程启动时创建一次，持有所有已加载的插件，
//! 并将其 gRPC 服务装配到 tonic Router 上。

use crate::application::plugins::Plugin;
use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use std::collections::HashMap;
use std::sync::Arc;

/// 所有已加载插件的中央注册表。
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn Plugin>>,
    /// 插件 ID → 索引，用于快速查找。
    index: HashMap<&'static str, usize>,
}

impl PluginRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// 注册插件。若 `id()` 与已有条目冲突则返回错误。
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) -> Result<(), PluginError> {
        let id = plugin.id();
        if self.index.contains_key(id) {
            return Err(PluginError::InitFailed(format!(
                "插件 id 重复: {}",
                id
            )));
        }
        let idx = self.plugins.len();
        self.plugins.push(plugin);
        self.index.insert(id, idx);
        Ok(())
    }

    /// 已注册插件的数量。
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// 注册表是否为空。
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// 按注册顺序初始化所有插件。
    pub async fn init_all(
        &self,
        ctx: &PluginContext,
    ) -> Vec<(&'static str, Result<(), PluginError>)> {
        let mut results = Vec::with_capacity(self.plugins.len());
        for p in &self.plugins {
            let res = p.init(ctx).await;
            results.push((p.id(), res));
        }
        results
    }

    /// 检查每个插件的健康状态。为每个插件返回 (id, status)。
    pub async fn health_all(&self) -> Vec<(&'static str, Result<HealthStatus, PluginError>)> {
        let mut results = Vec::with_capacity(self.plugins.len());
        for p in &self.plugins {
            let res = p.health().await;
            results.push((p.id(), res));
        }
        results
    }

    /// 按注册顺序的逆序关闭所有插件。
    pub async fn shutdown_all(&self) -> Vec<(&'static str, Result<(), PluginError>)> {
        let mut results = Vec::with_capacity(self.plugins.len());
        for p in self.plugins.iter().rev() {
            let res = p.shutdown().await;
            results.push((p.id(), res));
        }
        results
    }

    /// 将所有插件的 gRPC 服务装配到 tonic Server 上。
    ///
    /// 每个插件为其 gRPC 服务调用 `server.add_service(...)`。
    /// tonic 在内部累积路由；调用方在此之后通过
    /// `server.add_service(bootstrap_svc)` 提取最终的 `Router`。
    pub fn wire_grpc(
        &self,
        server: &mut tonic::transport::server::Server,
    ) -> Result<(), PluginError> {
        for p in &self.plugins {
            p.register_grpc(server)?;
        }
        Ok(())
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
