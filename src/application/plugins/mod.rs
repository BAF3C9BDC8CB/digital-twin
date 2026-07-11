//! Built-in plugin system for the Digital Twin system.
//!
//! Provides:
//! - `Plugin` trait — unified lifecycle for all plugins
//! - `PluginRegistry` — registration, lifecycle management, and gRPC wiring
//! - Builtin plugins: `k8s`, `svc`, `jenkins`

pub mod k8s;
pub mod svc;
pub mod jenkins;
pub mod registry;

use async_trait::async_trait;
use crate::domain::types::{HealthStatus, PluginContext, PluginError};

/// Core plugin trait.
///
/// Every plugin must implement this trait. All methods are async and must not
/// block the runtime. Plugins register their gRPC services via
/// `register_grpc()` and participate in a unified lifecycle (init → health →
/// shutdown).
///
/// # Constraints (non-negotiable)
///
/// 1. **No direct filesystem access** — all path operations go through `PluginContext`.
/// 2. **No subprocess calls** — all external interaction goes through gRPC client traits.
/// 3. **No blocking I/O** — all I/O must be async.
/// 4. **Must implement `health()`** — return value determines availability.
/// 5. **Errors must map to gRPC Status** — use `PluginError`, never `panic!`.
/// 6. **Proto files are independent per plugin** — each plugin's service lives in its own `.proto`.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Unique identifier for this plugin (e.g. "k8s", "svc", "jenkins").
    fn id(&self) -> &'static str;

    /// Human-readable display name.
    fn name(&self) -> &'static str;

    /// Semantic version string.
    fn version(&self) -> &'static str;

    /// Register this plugin's gRPC service(s) onto the tonic Server.
    ///
    /// The plugin calls `server.add_service(...)` for each of its gRPC
    /// services. Tonic internally accumulates routes across calls; the
    /// daemon will extract the final `Router` after all plugins are wired.
    fn register_grpc(
        &self,
        server: &mut tonic::transport::server::Server,
    ) -> Result<(), PluginError>;

    /// One-time initialization. Called once after construction, before the
    /// gRPC server starts. Use this to open connections, load config, etc.
    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Runtime health check. Called periodically by the daemon to determine
    /// plugin availability.
    async fn health(&self) -> Result<HealthStatus, PluginError>;

    /// Graceful shutdown. Called when the daemon is stopping.
    async fn shutdown(&self) -> Result<(), PluginError>;
}

// ---------------------------------------------------------------------------
// PluginError → tonic::Status conversion
// ---------------------------------------------------------------------------
// Cannot impl From<PluginError> for tonic::Status due to orphan rules
// (neither type is defined in this crate). Instead we use a free function.

/// Convert a `PluginError` into a `tonic::Status`.
pub fn plugin_error_to_status(e: &PluginError) -> tonic::Status {
    match e {
        PluginError::NotFound(_) => tonic::Status::not_found(e.to_string()),
        PluginError::InitFailed(_) => tonic::Status::internal(e.to_string()),
        PluginError::GrpcRegistration(_) => tonic::Status::internal(e.to_string()),
        PluginError::HealthCheck(_) => tonic::Status::unavailable(e.to_string()),
        PluginError::Shutdown(_) => tonic::Status::internal(e.to_string()),
        PluginError::Internal(_) => tonic::Status::internal(e.to_string()),
    }
}
