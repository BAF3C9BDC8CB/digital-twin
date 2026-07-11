//! Error types for the Digital Twin system.

/// Top-level error enum for the application.
#[derive(Debug, thiserror::Error)]
pub enum DtError {
    /// Generic error with message.
    #[error("{0}")]
    General(String),
    /// I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization/deserialization error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Repository error — wraps underlying storage errors.
    #[error("repository: {0}")]
    Repository(String),
    /// Configuration error.
    #[error("config: {0}")]
    Config(String),
    /// gRPC / network error.
    #[error("grpc: {0}")]
    Grpc(String),
    /// Not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Timeout.
    #[error("timeout: {0}")]
    Timeout(String),
    /// Network / HTTP error.
    #[error("network: {0}")]
    Network(String),
}

impl From<&str> for DtError {
    fn from(s: &str) -> Self {
        DtError::General(s.to_string())
    }
}

impl From<anyhow::Error> for DtError {
    fn from(e: anyhow::Error) -> Self {
        DtError::General(e.to_string())
    }
}
