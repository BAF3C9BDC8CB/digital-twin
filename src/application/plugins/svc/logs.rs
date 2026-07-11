//! Service log tailing (stub).
//!
//! Future: implements `GetLogs` server-side streaming RPC, tails local
//! service log files.

/// Placeholder for log streaming.
pub struct ServiceLogs;

impl ServiceLogs {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServiceLogs {
    fn default() -> Self {
        Self::new()
    }
}
