//! MetricsService gRPC implementation (stub — proto not yet compiled).
//!
//! Once `proto/metrics.proto` is compiled via tonic-build, the generated
//! `MetricsService` trait will be implemented here.
//!
//! For now, the service is callable directly (no gRPC wiring) — metrics
//! can be queried via `MetricsCollector::global().snapshot()`.

use crate::shared::logging::metrics::{MetricSnapshot, MetricsCollector};

/// MetricsService handler.
///
/// # Future (when proto compilation is enabled)
///
/// ```ignore
/// #[tonic::async_trait]
/// impl dt_metrics_proto::metrics_service_server::MetricsService for MetricsServiceImpl {
///     type WatchMetricsStream = ...;
///     async fn get_metrics(&self, req: Request<MetricsRequest>) -> Result<Response<MetricsResponse>, Status> { ... }
///     async fn watch_metrics(&self, req: Request<MetricsRequest>) -> Result<Response<Self::WatchMetricsStream>, Status> { ... }
/// }
/// ```
#[derive(Clone)]
pub struct MetricsServiceImpl;

impl MetricsServiceImpl {
    pub fn new() -> Self {
        Self
    }

    /// Take a point-in-time snapshot of all registered metrics.
    pub fn snapshot(&self) -> MetricSnapshot {
        MetricsCollector::global().snapshot()
    }

    /// Register built-in metrics that the daemon tracks from startup.
    pub fn register_builtins() {
        let m = MetricsCollector::global();

        // Build duration histogram
        m.histogram_linear("dt_build_duration_seconds", 0.0, 5.0, 20);

        // Embed service metrics
        m.counter("dt_embed_requests_total");
        m.gauge("dt_embed_queue_depth");

        // Neo4j connection pool
        m.gauge("dt_neo4j_connection_pool_size");

        // Qdrant write throughput
        m.counter("dt_qdrant_write_bytes_total");

        // Plugin health
        m.gauge("dt_plugin_health_status");

        // Context pipeline metrics
        m.histogram_linear("dt_context_total_duration_seconds", 0.0, 1.0, 10);
        m.histogram_linear("dt_context_world_query_duration", 0.0, 0.5, 10);

        // Write coordinator
        m.gauge("dt_write_coordinator_active_locks");
    }
}

impl Default for MetricsServiceImpl {
    fn default() -> Self {
        Self::new()
    }
}
