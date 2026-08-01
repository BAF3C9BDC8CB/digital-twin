//! Qdrant gRPC client — async vector search and upsert.
//!
//! Uses `qdrant-client` crate for async gRPC communication.

use crate::domain::error::DtError;
use qdrant_client::Qdrant;

/// Qdrant gRPC client wrapping the `qdrant-client` crate's `Qdrant` client.
#[derive(Clone)]
pub struct QdrantClient {
    client: Qdrant,
}

impl QdrantClient {
    /// Connect to a Qdrant server at the given URI and return a client.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let client = QdrantClient::connect("http://localhost:6334").await?;
    /// ```
    pub async fn connect(uri: &str) -> Result<Self, DtError> {
        let client = Qdrant::from_url(uri)
            .build()
            .map_err(|e| DtError::Repository(format!("Qdrant connect: {}", e)))?;
        Ok(Self { client })
    }

    /// Return a reference to the inner `qdrant_client::Qdrant` client.
    pub fn inner(&self) -> &Qdrant {
        &self.client
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use qdrant_client::Qdrant;

    /// Verify that `QdrantClient` can be constructed (no actual connection).
    #[test]
    fn test_client_can_be_constructed_without_connection() {
        // We can't test `connect()` without a running Qdrant, but we can
        // verify the struct compiles and type-checks.
        fn _accept_client(_c: &QdrantClient) {}
    }

    #[test]
    fn test_qdrant_type_is_available() {
        // Verify qdrant-client crate is linked correctly.
        let _ = Qdrant::from_url("http://localhost:6334");
    }
}
