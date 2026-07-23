//! Embedding service gRPC client — text → vector conversion.
//!
//! Communicates with external embedding servers.
//! This client is used by the pipeline to generate embeddings for code chunks.
//!
//! ## Implementations
//!
//! - `EmbedClient` — legacy stub (kept for backward compat, 384-dim zeros).
//! - `NoopEmbedService` — always returns zero-vectors of a configurable dim.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::EmbedService;
use crate::domain::types::HealthStatus;

// ---------------------------------------------------------------------------
// EmbedClient — legacy stub (backward compat)
// ---------------------------------------------------------------------------

/// Embedding service gRPC client (stub — zero-length vectors).
///
/// Kept for backward compatibility.  Prefer `NoopEmbedService` for tests.
pub struct EmbedClient {
    uri: String,
}

impl EmbedClient {
    /// Create a new (unconnected) client.
    pub fn new(uri: String) -> Self {
        Self { uri }
    }

    /// Establish the gRPC connection (async).
    pub async fn connect(&self) -> Result<(), DtError> {
        let _ = &self.uri;
        Ok(())
    }

    /// Generate embeddings for a batch of texts (stub).
    pub async fn embed(
        &self,
        texts: Vec<String>,
        _model: Option<&str>,
    ) -> Result<Vec<Vec<f32>>, DtError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        Ok(texts.iter().map(|_| vec![0.0_f32; 384]).collect())
    }
}

#[async_trait]
impl EmbedService for EmbedClient {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        self.embed(texts.to_vec(), None).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Degraded("stub embed client".into()))
    }
}

// ---------------------------------------------------------------------------
// NoopEmbedService — zero-vectors for testing / offline development
// ---------------------------------------------------------------------------

/// No-op embed service returning zero-vectors.
///
/// Used when `dt-embed` is unavailable (e.g. Python dependencies not met).
/// Configurable dimension for test environments.
pub struct NoopEmbedService {
    dim: u32,
}

impl NoopEmbedService {
    /// Create a no-op service returning `dim`-dimensional zero vectors
    /// (default 1024 for BGE-M3 compatibility).
    pub fn new(dim: u32) -> Self {
        Self { dim }
    }
}

impl Default for NoopEmbedService {
    fn default() -> Self {
        Self { dim: 1024 }
    }
}

#[async_trait]
impl EmbedService for NoopEmbedService {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        Ok(texts.iter().map(|_| vec![0.0_f32; self.dim as usize]).collect())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Healthy)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn embed_client_returns_vectors() {
        let client = EmbedClient::new("http://localhost:50051".into());
        let texts = vec!["hello".to_string(), "world".to_string()];
        let result = client.embed(texts, None).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 384);
    }

    #[tokio::test]
    async fn embed_client_empty_input() {
        let client = EmbedClient::new("http://localhost:50051".into());
        let result = client.embed(vec![], None).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn embed_batch_trait_method() {
        let client = EmbedClient::new("http://localhost:50051".into());
        let texts = vec!["fn test() {}".to_string()];
        let result = client.embed_batch(&texts).await.unwrap();
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn noop_embed_service_works() {
        let svc = NoopEmbedService::default();
        let texts = vec!["fn foo() {}".to_string()];
        let result = svc.embed_batch(&texts).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 1024);
    }

    #[tokio::test]
    async fn noop_custom_dim() {
        let svc = NoopEmbedService::new(768);
        let texts = vec!["x".to_string()];
        let result = svc.embed_batch(&texts).await.unwrap();
        assert_eq!(result[0].len(), 768);
    }
}
