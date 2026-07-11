//! Jenkins plugin — CI/CD operations.
//!
//! Provides gRPC services for listing jobs, viewing params, history,
//! triggering builds (with streaming output), and retrieving build logs.
//!
//! **Generated from:** `proto/plugin_jenkins.proto`
//!
//! NOTE: tonic-build proto compilation is deferred.

pub mod build;
pub mod client;
pub mod service;

// pub use service::JenkinsPluginService;
