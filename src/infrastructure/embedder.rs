//! Embedding service implementations — text → vector conversion.
//!
//! Currently provides [`NoopEmbedService`] for testing/failover (zero-vectors).
//! Production embedding is handled by [`SiliconFlowClient`].

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::EmbedService;
use crate::domain::types::HealthStatus;

// ---------------------------------------------------------------------------
// NoopEmbedService — zero-vectors for testing / offline development
// ---------------------------------------------------------------------------

/// No-op embed service returning zero-vectors.
///
/// Used when SiliconFlow API is unavailable.
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
