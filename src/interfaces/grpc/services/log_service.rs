//! LogService gRPC 实现（桩实现——proto 尚未编译）。
//!
//! 当 `proto/log.proto` 通过 tonic-build 编译后，生成的
//! `LogService` trait 将在此实现。
//!
//! 当前方案：Rust 守护进程的 JSON 文件写入器直接处理所有日志
//! 聚合。gRPC `StreamLogs` 端点将把外部（Python）日志条目转发到
//! 同一 tracing 管线。

/// LogService 处理器。
///
/// # 未来（启用 proto 编译时）
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

    /// 通过 tracing 管线将日志条目写入守护进程的 JSON 行文件。
    ///
    /// 供 Python 的 `GrpcLogHandler` 转发日志条目使用。
    /// 该条目会通过 `tracing` 重新发出，从而出现在同一个
    /// JSON 文件中。
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
