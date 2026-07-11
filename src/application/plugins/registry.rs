//! Plugin registry — owns all plugins and manages their lifecycle.
//!
//! The registry is created once at daemon startup, holds every loaded plugin,
//! and wires their gRPC services onto the tonic Router.

use crate::application::plugins::Plugin;
use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use std::collections::HashMap;
use std::sync::Arc;

/// Central registry for all loaded plugins.
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn Plugin>>,
    /// Plugin ID → index for fast lookup.
    index: HashMap<&'static str, usize>,
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Register a plugin. Returns error if `id()` collides with an existing entry.
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) -> Result<(), PluginError> {
        let id = plugin.id();
        if self.index.contains_key(id) {
            return Err(PluginError::InitFailed(format!(
                "duplicate plugin id: {}",
                id
            )));
        }
        let idx = self.plugins.len();
        self.plugins.push(plugin);
        self.index.insert(id, idx);
        Ok(())
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Initialize all plugins in registration order.
    pub async fn init_all(&self, ctx: &PluginContext) -> Vec<(&'static str, Result<(), PluginError>)> {
        let mut results = Vec::with_capacity(self.plugins.len());
        for p in &self.plugins {
            let res = p.init(ctx).await;
            results.push((p.id(), res));
        }
        results
    }

    /// Check health of every plugin. Returns (id, status) for each.
    pub async fn health_all(&self) -> Vec<(&'static str, Result<HealthStatus, PluginError>)> {
        let mut results = Vec::with_capacity(self.plugins.len());
        for p in &self.plugins {
            let res = p.health().await;
            results.push((p.id(), res));
        }
        results
    }

    /// Shutdown all plugins in reverse registration order.
    pub async fn shutdown_all(&self) -> Vec<(&'static str, Result<(), PluginError>)> {
        let mut results = Vec::with_capacity(self.plugins.len());
        for p in self.plugins.iter().rev() {
            let res = p.shutdown().await;
            results.push((p.id(), res));
        }
        results
    }

    /// Wire all plugin gRPC services onto the tonic Server.
    ///
    /// Each plugin calls `server.add_service(...)` for its gRPC services.
    /// Tonic accumulates routes internally; the caller extracts the final
    /// `Router` by calling `server.add_service(bootstrap_svc)` after this.
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
