//! K8s status operations (stub).
//!
//! Future: implements `GetPods`, `GetDeployments`, `GetServices`,
//! and `GetStatus` RPCs.

/// Placeholder for K8s status queries.
pub struct K8sStatus;

impl K8sStatus {
    pub fn new() -> Self {
        Self
    }
}

impl Default for K8sStatus {
    fn default() -> Self {
        Self::new()
    }
}
