//! Digital Twin 系统的内置插件系统。
//!
//! 提供：
//! - `Plugin` trait——所有插件的统一生命周期
//! - `PluginRegistry`——注册、生命周期管理以及 gRPC 装配
//! - 内置插件：`k8s`、`svc`、`jenkins`

pub mod jenkins;
pub mod k8s;
pub mod registry;
pub mod svc;

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;

/// 核心插件 trait。
///
/// 每个插件都必须实现该 trait。所有方法均为异步且不得阻塞运行时。
/// 插件通过 `register_grpc()` 注册其 gRPC 服务，并参与统一的生命周期
/// （init → health → shutdown）。
///
/// # 约束（不可协商）
///
/// 1. **禁止直接访问文件系统**——所有路径操作都通过 `PluginContext`。
/// 2. **禁止调用子进程**——所有外部交互都通过 gRPC 客户端 trait。
/// 3. **禁止阻塞 I/O**——所有 I/O 必须为异步。
/// 4. **必须实现 `health()`**——返回值决定可用性。
/// 5. **错误必须映射到 gRPC Status**——使用 `PluginError`，绝不 `panic!`。
/// 6. **每个插件的 proto 文件相互独立**——各插件服务位于各自的 `.proto` 中。
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// 该插件的唯一标识符（例如 "k8s"、"svc"、"jenkins"）。
    fn id(&self) -> &'static str;

    /// 人类可读的显示名称。
    fn name(&self) -> &'static str;

    /// 语义化版本号字符串。
    fn version(&self) -> &'static str;

    /// 将该插件的 gRPC 服务注册到 tonic Server 上。
    ///
    /// 插件为其每个 gRPC 服务调用 `server.add_service(...)`。
    /// tonic 内部会跨调用累积路由；守护进程在所有插件装配完成后
    /// 提取最终的 `Router`。
    fn register_grpc(
        &self,
        server: &mut tonic::transport::server::Server,
    ) -> Result<(), PluginError>;

    /// 一次性初始化。构造完成后、gRPC 服务器启动前调用一次。
    /// 用于打开连接、加载配置等。
    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// 运行时健康检查。守护进程定期调用以判断插件可用性。
    async fn health(&self) -> Result<HealthStatus, PluginError>;

    /// 优雅停机。守护进程停止时调用。
    async fn shutdown(&self) -> Result<(), PluginError>;
}

// ---------------------------------------------------------------------------
// PluginError → tonic::Status 转换
// ---------------------------------------------------------------------------
// 由于孤儿规则（两种类型均非本 crate 定义），无法为
// tonic::Status 实现 From<PluginError>，改用自由函数。

/// 将 `PluginError` 转换为 `tonic::Status`。
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
