//! Jenkins 插件——CI/CD 操作。
//!
//! 提供 gRPC 服务：作业列表、参数查看、历史查看、
//! 触发构建（含流式输出）以及获取构建日志。
//!
//! **由以下文件生成：** `proto/plugin_jenkins.proto`
//!
//! NOTE: tonic-build proto 编译暂缓。

pub mod build;
pub mod client;
pub mod service;

// pub use service::JenkinsPluginService;
