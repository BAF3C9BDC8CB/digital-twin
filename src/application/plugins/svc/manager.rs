//! Service manager — tracks running microservice processes (stub).
//!
//! Future: manages child processes with tokio::process::Command (async),
//! tracks PIDs, handles graceful shutdown via SIGTERM.

/// Placeholder for service lifecycle management.
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
