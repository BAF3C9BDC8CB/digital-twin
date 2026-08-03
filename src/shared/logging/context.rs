//! 插件的命名空间日志器——`PluginLogger`。
//!
//! 每个插件都会收到一个绑定到其插件名的 `PluginLogger` 实例。
//! 消息会以插件名为前缀，因为 `tracing` 事件宏要求 `&'static str` target。
//! Rust 模块路径提供 JSON 输出中的 `target` 字段。
//!
//! # 用法
//!
//! ```ignore
//! let log = PluginLogger::new("k8s");
//! log.info("pods listed");
//! log.warn("slow response");
//! ```

/// 为跨进程关联而存储的 Trace ID。
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

/// 绑定到特定插件名的日志器句柄。
///
/// 通过该日志器发出的所有消息都会以插件名作为 `[前缀]`，
/// 并以 Rust 模块路径作为 `target`。
///
/// 该日志器克隆成本低（`Clone`），且可安全跨线程共享（`Send + Sync`）。
#[derive(Clone)]
pub struct PluginLogger {
    /// 插件名，例如 `"k8s"`、`"svc"`、`"jenkins"`。
    pub target: String,
}

impl PluginLogger {
    /// 为给定的插件名创建新的日志器。
    ///
    /// `name` 通常是插件的 `id()`（例如 `"k8s"`、`"svc"`、`"jenkins"`）。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            target: name.into(),
        }
    }

    #[track_caller]
    pub fn info(&self, msg: &str) {
        tracing::info!("[{}] {}", self.target, msg);
    }

    #[track_caller]
    pub fn warn(&self, msg: &str) {
        tracing::warn!("[{}] {}", self.target, msg);
    }

    #[track_caller]
    pub fn error(&self, msg: &str) {
        tracing::error!("[{}] {}", self.target, msg);
    }

    #[track_caller]
    pub fn debug(&self, msg: &str) {
        tracing::debug!("[{}] {}", self.target, msg);
    }

    #[track_caller]
    pub fn trace(&self, msg: &str) {
        tracing::trace!("[{}] {}", self.target, msg);
    }
}

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
