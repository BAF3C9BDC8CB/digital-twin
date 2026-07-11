//! Svc plugin — local microservice lifecycle management.
//!
//! Provides gRPC services for listing, starting, stopping, restarting,
//! and tailing logs of local microservices.
//!
//! **Generated from:** `proto/plugin_svc.proto`
//!
//! NOTE: tonic-build proto compilation is deferred.

pub mod logs;
pub mod manager;
pub mod service;

// pub use service::SvcPluginService;
