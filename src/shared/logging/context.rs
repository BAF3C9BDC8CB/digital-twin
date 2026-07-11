//! Namespaced logger for plugins — `PluginLogger`.
//!
//! Every plugin receives a `PluginLogger` instance scoped to its plugin name.
//! Messages are prefixed with the plugin name because `tracing` event macros
//! require a `&'static str` target. The Rust module path provides the
//! `target` field in JSON output.
//!
//! # Usage
//!
//! ```ignore
//! let log = PluginLogger::new("k8s");
//! log.info("pods listed");
//! log.warn("slow response");
//! ```

/// Trace ID stored for cross-process correlation.
#[derive(Clone, Debug)]
pub struct TraceId(pub String);

/// A logger handle bound to a specific plugin name.
///
/// All messages emitted through this logger have the plugin name as a
/// `[prefix]` in the message and the Rust module path as `target`.
///
/// The logger is cheap to clone (`Clone`) and safe to share across threads
/// (`Send + Sync`).
#[derive(Clone)]
pub struct PluginLogger {
    /// The plugin name, e.g. `"k8s"`, `"svc"`, `"jenkins"`.
    pub target: String,
}

impl PluginLogger {
    /// Create a new logger for the given plugin name.
    ///
    /// `name` is typically the plugin `id()` (e.g. `"k8s"`, `"svc"`, `"jenkins"`).
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
// Span helpers
// ---------------------------------------------------------------------------

/// Set a trace ID on the current span.
///
/// If no span is active, this is a no-op.
#[allow(dead_code)]
pub fn set_trace_id(id: impl Into<String>) {
    let id = TraceId(id.into());
    tracing::Span::current().record("trace_id", tracing::field::display(&id.0));
}

/// Generate and set a new random trace ID on the current span.
#[allow(dead_code)]
pub fn generate_trace_id() -> String {
    let id = uuid::Uuid::new_v4().to_string().replace('-', "")[..8].to_string();
    set_trace_id(&id);
    id
}
