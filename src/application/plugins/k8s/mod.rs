//! K8s plugin — Kubernetes operations.
//!
//! Provides gRPC services for pod/deployment/service listing, log streaming,
//! log download, and cluster status.
//!
//! **Generated from:** `proto/plugin_k8s.proto`
//!
//! NOTE: tonic-build proto compilation is deferred until proto definitions
//! are finalised. The service module currently uses stub implementations.

pub mod logs;
pub mod service;
pub mod status;

// use crate::application::plugins::Plugin;

// Re-export the plugin service struct.
// pub use service::K8sPluginService;
