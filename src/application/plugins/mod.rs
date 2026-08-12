//! Digital Twin 系统的内置插件（服务实现）。
//!
//! 提供：
//! - `Plugin` trait——所有插件的统一生命周期
//! - 内置插件：`jenkins`

pub mod jenkins;

use crate::domain::types::{HealthStatus, PluginContext, PluginError};
use async_trait::async_trait;

/// 核心插件 trait。
///
/// 每个插件都必须实现该 trait。所有方法均为异步且不得阻塞运行时。
/// 插件参与统一的生命周期（init → health → shutdown）。
///
/// # 约束（不可协商）
///
/// 1. **禁止直接访问文件系统**——所有路径操作都通过 `PluginContext`。
/// 2. **禁止调用子进程**——所有外部交互都通过抽象客户端。
/// 3. **禁止阻塞 I/O**——所有 I/O 必须为异步。
/// 4. **必须实现 `health()`**——返回值决定可用性。
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// 该插件的唯一标识符（例如 "jenkins"）。
    fn id(&self) -> &'static str;

    /// 人类可读的显示名称。
    fn name(&self) -> &'static str;

    /// 语义化版本号字符串。
    fn version(&self) -> &'static str;

    /// 一次性初始化。构造完成后调用一次。
    /// 用于打开连接、加载配置等。
    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// 运行时健康检查。定期调用以判断插件可用性。
    async fn health(&self) -> Result<HealthStatus, PluginError>;

    /// 优雅停机。
    async fn shutdown(&self) -> Result<(), PluginError>;
}
