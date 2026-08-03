//! Svc 插件——本地微服务生命周期管理。
//!
//! 提供 gRPC 服务：本地微服务的列表、启动、停止、重启
//! 以及日志尾随。
//!
//! **由以下文件生成：** `proto/plugin_svc.proto`
//!
//! NOTE: tonic-build proto 编译暂缓。

pub mod logs;
pub mod manager;
pub mod service;

// pub use service::SvcPluginService;
