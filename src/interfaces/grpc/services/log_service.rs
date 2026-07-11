//! LogService gRPC implementation (stub — proto not yet compiled).
//!
//! Once `proto/log.proto` is compiled via tonic-build, the generated
//! `LogService` trait will be implemented here.
//!
//! Current approach: the Rust daemon's JSON-file writer handles all log
//! aggregation directly. The gRPC `StreamLogs` endpoint will forward
//! external (Python) log entries to the same tracing pipeline.

/// LogService handler.
///
/// # Future (when proto compilation is enabled)
///
/// ```ignore
/// #[tonic::async_trait]
/// impl dt_log_proto::log_service_server::LogService for LogServiceImpl {
///     type StreamLogsStream = ...;
///     type QueryLogsStream = ...;
///     async fn stream_logs(&self, req: Request<Streaming<LogEntry>>) -> Result<Response<LogAck>, Status> { ... }
///     async fn query_logs(&self, req: Request<LogQuery>) -> Result<Response<Self::QueryLogsStream>, Status> { ... }
/// }
/// ```
#[derive(Clone)]
pub struct LogServiceImpl;

impl LogServiceImpl {
    pub fn new() -> Self {
        Self
    }

    /// Write a log entry to the daemon's JSON-line file via the tracing pipeline.
    ///
    /// This is used by the Python `GrpcLogHandler` to forward log entries.
    /// The entry is re-emitted through `tracing` so it appears in the same
    /// JSON file.
    pub fn handle_entry(
        &self,
        level: &str,
        target: &str,
        plugin: Option<&str>,
        message: &str,
        trace_id: Option<&str>,
    ) {
        let plugin_tag = plugin.unwrap_or(target);
        let trace = trace_id.unwrap_or("-");
        match level {
            "TRACE" => tracing::trace!(plugin = plugin_tag, trace_id = trace, "{}", message),
            "DEBUG" => tracing::debug!(plugin = plugin_tag, trace_id = trace, "{}", message),
            "WARN" => tracing::warn!(plugin = plugin_tag, trace_id = trace, "{}", message),
            "ERROR" => tracing::error!(plugin = plugin_tag, trace_id = trace, "{}", message),
            _ => tracing::info!(plugin = plugin_tag, trace_id = trace, "{}", message),
        }
    }
}

impl Default for LogServiceImpl {
    fn default() -> Self {
        Self::new()
    }
}
