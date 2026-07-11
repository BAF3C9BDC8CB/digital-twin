//! Nacos synchronisation module.
//!
//! Provides an HTTP client for the Nacos REST API and two [`SyncSource`]
//! implementations:
//!
//! - [`ConfigSyncSource`] — syncs configuration data → NacosConfig / ConfigKey / Database nodes.
//! - [`ServiceSyncSource`] — syncs service registry data → NacosService / Service nodes.

pub mod client;
pub mod config_sync;
pub mod service_sync;

pub use client::NacosClient;
pub use config_sync::ConfigSyncSource;
pub use service_sync::ServiceSyncSource;
