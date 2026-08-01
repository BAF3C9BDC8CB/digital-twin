//! K8s Event Timeline sync (skeleton).
//!
//! K8s Events are ephemeral — they exist in the Runtime world and are NOT
//! persisted to Memgraph. This module provides a skeleton for streaming K8s
//! events into the timeline for real-time monitoring and alerting.
//!
//! ## Future direction
//!
//! - Stream K8s events via the K8s Watch API (long-lived HTTP connection).
//! - Correlate with existing Memgraph entities (K8sDeployment, K8sService).
//! - Emit structured log/tracing events for alerting pipelines.
//! - Optionally persist high-severity events as `PodEvent` nodes via
//!   `(:PodEvent)-[:TRIGGERED_BY]->(:K8sDeployment)`.
//!
//! For now, this is an explicit no-op skeleton.

use crate::domain::error::DtError;

/// Streaming K8s event watcher (placeholder).
///
/// In the future this will establish a K8s Watch connection and stream
/// events into the tracing/logging pipeline. For now it returns `Ok(())`
/// immediately.
pub struct K8sEventTimelineSync;

impl K8sEventTimelineSync {
    /// Placeholder: start watching K8s events.
    ///
    /// # Arguments
    /// * `_namespace` — namespace to watch (empty string = all namespaces).
    /// * `_resource_version` — starting resource version for the watch.
    ///
    /// # Returns
    /// Always `Ok(())` in this skeleton implementation.
    pub async fn watch(&self, _namespace: &str, _resource_version: &str) -> Result<(), DtError> {
        tracing::debug!("[k8s/timeline] watch() called — skeleton, no-op");
        Ok(())
    }

    /// Placeholder: stop watching K8s events.
    pub async fn stop(&self) -> Result<(), DtError> {
        tracing::debug!("[k8s/timeline] stop() called — skeleton, no-op");
        Ok(())
    }
}

impl Default for K8sEventTimelineSync {
    fn default() -> Self {
        Self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skeleton_watch_returns_ok() {
        let sync = K8sEventTimelineSync::default();
        assert!(sync.watch("newoffen", "0").await.is_ok());
    }

    #[tokio::test]
    async fn skeleton_stop_returns_ok() {
        let sync = K8sEventTimelineSync::default();
        assert!(sync.stop().await.is_ok());
    }

    #[test]
    fn skeleton_default_constructs() {
        let _sync = K8sEventTimelineSync::default();
    }
}
