//! Nacos 同步模块。
//!
//! 提供 Nacos REST API 的 HTTP 客户端，以及两个 [`SyncSource`] 实现：
//!
//! - [`ConfigSyncSource`]——同步配置数据 → NacosConfig / ConfigKey / Database 节点。
//! - [`ServiceSyncSource`]——同步服务注册数据 → NacosService / Service 节点。

pub mod client;
pub mod config_sync;
pub mod service_sync;
pub mod virtual_file_source;

pub use client::NacosClient;
pub use config_sync::ConfigSyncSource;
pub use service_sync::ServiceSyncSource;
pub use virtual_file_source::NacosVirtualFileSource;
