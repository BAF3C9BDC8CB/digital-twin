//! Build streaming / log retrieval logic (stub).
//!
//! Future: implements `Build` (server-side streaming) and `GetBuildLog` RPCs.
//! Stream build console output in real-time via Jenkins API.

/// Placeholder for build streaming.
pub struct BuildStream;

impl BuildStream {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuildStream {
    fn default() -> Self {
        Self::new()
    }
}
