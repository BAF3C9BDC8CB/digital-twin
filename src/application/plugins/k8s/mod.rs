//! K8s 插件——Kubernetes 操作。
//!
//! 提供 gRPC 服务：Pod/Deployment/Service 列表、日志流式输出、
//! 日志下载以及集群状态。
//!
//! **由以下文件生成：** `proto/plugin_k8s.proto`
//!
//! NOTE: tonic-build proto 编译在 proto 定义定稿前暂缓。
//! 服务模块目前使用占位实现。

pub mod logs;
pub mod service;
pub mod status;

// use crate::application::plugins::Plugin;

// 重新导出插件服务结构体。
// pub use service::K8sPluginService;
