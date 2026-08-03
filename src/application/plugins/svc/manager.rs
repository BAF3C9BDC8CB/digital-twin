//! 服务管理器——跟踪运行中的微服务进程（占位实现）。
//!
//! 后续将使用 tokio::process::Command（异步）管理子进程，
//! 跟踪 PID，并通过 SIGTERM 实现优雅停机。

/// 服务生命周期管理的占位实现。
pub struct ServiceManager;

impl ServiceManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServiceManager {
    fn default() -> Self {
        Self::new()
    }
}
