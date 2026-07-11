//! Embedding service gRPC client — text → vector conversion.
//!
//! Communicates with `dt-embed` gRPC server (defined in `proto/embed.proto`).
//! This client is used by the pipeline to generate embeddings for code chunks.
//!
//! ## Implementations
//!
//! - `GrpcEmbedService` — real gRPC client using tonic-generated stubs.
//! - `EmbedClient` — legacy stub (kept for backward compat, 384-dim zeros).
//! - `NoopEmbedService` — always returns zero-vectors of a configurable dim.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::EmbedService;
use crate::domain::types::HealthStatus;
use std::time::Instant;

// ---------------------------------------------------------------------------
// GrpcEmbedService — real tonic client
// ---------------------------------------------------------------------------

/// Real embedding service gRPC client using `proto/embed.proto`.
///
/// Connects to a dt-embed gRPC server (future: Python gRPC wrapper around
/// BGE-M3 model) and calls `EmbedService::Embed` to convert text batches
/// into float vectors.
///
/// # Usage
///
/// ```ignore
/// let svc = GrpcEmbedService::connect("http://[::1]:50052").await?;
/// let vecs = svc.embed_batch(&["fn test() {}".to_string()]).await?;
/// ```
pub struct GrpcEmbedService {
    client: crate::proto::dt::embed::embed_service_client::EmbedServiceClient<
        tonic::transport::Channel,
    >,
}

impl GrpcEmbedService {
    /// Connect to the dt-embed gRPC server at `addr` (e.g. `http://[::1]:50052`).
    pub async fn connect(addr: &str) -> Result<Self, DtError> {
        let endpoint = tonic::transport::Endpoint::from_shared(addr.to_string())
            .map_err(|e| DtError::Repository(format!("embed channel: {e}")))?;
        let channel = endpoint
            .connect()
            .await
            .map_err(|e| DtError::Repository(format!("embed connect: {e}")))?;
        let client =
            crate::proto::dt::embed::embed_service_client::EmbedServiceClient::new(channel);
        Ok(Self { client })
    }
}

#[async_trait]
impl EmbedService for GrpcEmbedService {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        use crate::proto::dt::embed::EmbedRequest;

        if texts.is_empty() {
            return Ok(vec![]);
        }

        let start = Instant::now();
        let request = tonic::Request::new(EmbedRequest {
            texts: texts.to_vec(),
            model: String::new(),
        });

        let mut client = self.client.clone();
        let response = client
            .embed(request)
            .await
            .map_err(|e| DtError::Repository(format!("embed call: {e}")))?;

        let vectors: Vec<Vec<f32>> = response
            .into_inner()
            .embeddings
            .into_iter()
            .map(|e| e.vector)
            .collect();

        tracing::debug!(
            "embed: {} texts → {} vectors in {:?}",
            texts.len(),
            vectors.len(),
            start.elapsed()
        );

        Ok(vectors)
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        use crate::proto::dt::common::Empty;

        let mut client = self.client.clone();
        match client.health(tonic::Request::new(Empty {})).await {
            Ok(_) => Ok(HealthStatus::Healthy),
            Err(e) => Ok(HealthStatus::Unhealthy(format!("embed health: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// EmbedClient — legacy stub (backward compat)
// ---------------------------------------------------------------------------

/// Embedding service gRPC client (stub — zero-length vectors).
///
/// Kept for backward compatibility.  Prefer `GrpcEmbedService` for real
/// embedding or `NoopEmbedService` for tests.
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
    /// (default 384 for BGE-M3 compatibility).
    pub fn new(dim: u32) -> Self {
        Self { dim }
    }
}

impl Default for NoopEmbedService {
    fn default() -> Self {
        Self { dim: 384 }
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
        let client = EmbedClient::new("http://localhost:50052".into());
        let texts = vec!["hello".to_string(), "world".to_string()];
        let result = client.embed(texts, None).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 384);
    }

    #[tokio::test]
    async fn embed_client_empty_input() {
        let client = EmbedClient::new("http://localhost:50052".into());
        let result = client.embed(vec![], None).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn embed_batch_trait_method() {
        let client = EmbedClient::new("http://localhost:50052".into());
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
        assert_eq!(result[0].len(), 384);
    }

    #[tokio::test]
    async fn noop_custom_dim() {
        let svc = NoopEmbedService::new(768);
        let texts = vec!["x".to_string()];
        let result = svc.embed_batch(&texts).await.unwrap();
        assert_eq!(result[0].len(), 768);
    }
}
