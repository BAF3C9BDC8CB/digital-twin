//! 日志上下文工具——Trace ID 关联。

/// 为跨进程关联而存储的 Trace ID。
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

// ---------------------------------------------------------------------------
// Span 辅助函数
// ---------------------------------------------------------------------------

/// 在当前 span 上设置 trace ID。
///
/// 若没有活跃的 span，则该操作为空操作。
#[allow(dead_code)]
pub fn set_trace_id(id: impl Into<String>) {
    let id = TraceId(id.into());
    tracing::Span::current().record("trace_id", tracing::field::display(&id.0));
}

/// 生成并在当前 span 上设置新的随机 trace ID。
#[allow(dead_code)]
pub fn generate_trace_id() -> String {
    let id = uuid::Uuid::new_v4().to_string().replace('-', "")[..8].to_string();
    set_trace_id(&id);
    id
}
