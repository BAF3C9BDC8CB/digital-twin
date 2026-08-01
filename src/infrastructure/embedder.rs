//! Embedding service implementations — text → vector conversion.
//!
//! Provides [`NoopEmbedService`] for testing/failover, and a factory
//! function [`create_embed_router`] that builds a provider router from
//! configuration (SiliconFlow + XInference).

use crate::domain::error::DtError;
use crate::domain::traits::EmbedService;
use crate::domain::types::HealthStatus;
use async_trait::async_trait;
use std::sync::Arc;

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
        Ok(texts
            .iter()
            .map(|_| vec![0.0_f32; self.dim as usize])
            .collect())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Healthy)
    }
}

// ---------------------------------------------------------------------------
// Factory function — build EmbedProviderRouter from config
// ---------------------------------------------------------------------------

/// Provider configuration for creating an [`EmbedProviderRouter`].
///
/// Each provider is optional — only the configured one needs to be present.
pub struct ProviderConfig {
    /// SiliconFlow client configuration.
    pub siliconflow_url: String,
    pub siliconflow_api_key: String,
    pub siliconflow_model_embed: String,
    pub siliconflow_model_reranker: String,
    pub siliconflow_model_llm: String,
    /// XInference client configuration.
    pub xinference_url: String,
    pub xinference_api_key: String,
    pub xinference_model_embed: String,
    pub xinference_model_reranker: String,
    pub xinference_model_llm: String,
    /// Routing configuration.
    pub embed_provider: String,
    pub rerank_provider: String,
    pub llm_provider: String,
}

impl ProviderConfig {
    /// Return a default SiliconFlow-only config (for fallback when pipeline.yaml is unavailable).
    pub fn default_siliconflow() -> Self {
        Self {
            siliconflow_url: "https://api.siliconflow.cn/v1".into(),
            siliconflow_api_key: std::env::var("SILICONFLOW_API_KEY").unwrap_or_default(),
            siliconflow_model_embed: "BAAI/bge-m3".into(),
            siliconflow_model_reranker: "BAAI/bge-reranker-v2-m3".into(),
            siliconflow_model_llm: "Qwen3-14B".into(),
            xinference_url: String::new(),
            xinference_api_key: String::new(),
            xinference_model_embed: String::new(),
            xinference_model_reranker: String::new(),
            xinference_model_llm: String::new(),
            embed_provider: "siliconflow".into(),
            rerank_provider: "siliconflow".into(),
            llm_provider: "siliconflow".into(),
        }
    }
}

/// Build an [`EmbedProviderRouter`] from provider configuration.
///
/// Creates `SiliconFlowClient` and `XInferenceClient` as configured,
/// then wraps them in a router with the specified routing rules.
pub fn create_embed_router(cfg: ProviderConfig) -> Arc<dyn EmbedService> {
    use crate::infrastructure::provider_router::{EmbedProviderRouter, ProviderRouterConfig};

    let siliconflow = if !cfg.siliconflow_url.is_empty() {
        Some(Arc::new(
            crate::infrastructure::siliconflow::SiliconFlowClient::new(
                cfg.siliconflow_url,
                cfg.siliconflow_api_key,
                cfg.siliconflow_model_embed,
                cfg.siliconflow_model_reranker,
                cfg.siliconflow_model_llm,
            ),
        ))
    } else {
        None
    };

    let xinference = if !cfg.xinference_url.is_empty() {
        Some(Arc::new(
            crate::infrastructure::xinference::XInferenceClient::new(
                cfg.xinference_url,
                cfg.xinference_api_key,
                cfg.xinference_model_embed,
                cfg.xinference_model_reranker,
                cfg.xinference_model_llm,
            ),
        ))
    } else {
        None
    };

    let router_config = ProviderRouterConfig {
        embed_provider: cfg.embed_provider,
        rerank_provider: cfg.rerank_provider,
        llm_provider: cfg.llm_provider,
    };

    let router = EmbedProviderRouter::new(siliconflow, xinference, router_config);
    Arc::new(router)
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
