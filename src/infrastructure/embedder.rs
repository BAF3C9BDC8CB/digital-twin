//! Embedding 服务实现——文本到向量的转换。
//!
//! 提供用于测试/故障切换的 [`NoopEmbedService`]，以及一个工厂
//! 函数 [`create_embed_router`]，它根据配置构建 provider 路由器。
//!
//! # 架构（2026-09-05 起）
//!
//! 唯一 provider 实现为 SiliconFlow（OpenAI 兼容超集：chat/embeddings 为
//! OpenAI 标准协议，rerank 为 SiliconFlow 私有扩展端点）。

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmService, RerankService};
use crate::domain::types::HealthStatus;
use async_trait::async_trait;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// NoopEmbedService——测试/离线开发使用的零向量服务
// ---------------------------------------------------------------------------

/// 返回零向量的 no-op 向量化服务。
///
/// 在 SiliconFlow API 不可用时使用。
/// 维度可配置，用于测试环境。
pub struct NoopEmbedService {
    dim: u32,
}

impl NoopEmbedService {
    /// 创建返回 `dim` 维零向量的 no-op 服务
    /// （默认 1024，以兼容 BGE-M3）。
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
// 工厂函数——根据配置构建 EmbedProviderRouter
// ---------------------------------------------------------------------------

/// 用于创建 [`EmbedProviderRouter`] 的 provider 配置。
///
/// provider 可选——只需配置所需的那一个。
pub struct ProviderConfig {
    /// SiliconFlow 客户端配置。
    pub siliconflow_url: String,
    pub siliconflow_api_key: String,
    pub siliconflow_model_embed: String,
    pub siliconflow_model_reranker: String,
    pub siliconflow_model_llm: String,
    pub siliconflow_max_concurrent: usize,
    /// 路由配置。
    pub embed_provider: String,
    pub rerank_provider: String,
    pub llm_provider: String,
}

impl ProviderConfig {
    /// 返回默认的仅 SiliconFlow 配置（用于 pipeline.yaml 不可用时的回退）。
    pub fn default_siliconflow() -> Self {
        Self {
            siliconflow_url: "https://api.siliconflow.cn/v1".into(),
            siliconflow_api_key: std::env::var("SILICONFLOW_API_KEY").unwrap_or_default(),
            siliconflow_model_embed: "BAAI/bge-m3".into(),
            siliconflow_model_reranker: "BAAI/bge-reranker-v2-m3".into(),
            siliconflow_model_llm: "Qwen3-14B".into(),
            siliconflow_max_concurrent: 20,
            embed_provider: "siliconflow".into(),
            rerank_provider: "siliconflow".into(),
            llm_provider: "siliconflow".into(),
        }
    }
}

/// 用配置好的客户端构建路由器（embed/rerank 构造函数共用）。
fn build_provider_router(
    cfg: ProviderConfig,
) -> crate::infrastructure::provider_router::EmbedProviderRouter {
    use crate::infrastructure::provider_router::{EmbedProviderRouter, ProviderRouterConfig};

    let siliconflow = if !cfg.siliconflow_url.is_empty() {
        Some(Arc::new(
            crate::infrastructure::siliconflow::SiliconFlowClient::new(
                cfg.siliconflow_url,
                cfg.siliconflow_api_key,
                cfg.siliconflow_model_embed,
                cfg.siliconflow_model_reranker,
                cfg.siliconflow_model_llm,
                cfg.siliconflow_max_concurrent,
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

    EmbedProviderRouter::new(siliconflow, router_config)
}

/// 根据 provider 配置构建 [`EmbedProviderRouter`]。
///
/// 按配置创建 `SiliconFlowClient`，然后用指定的路由规则包装进路由器。
pub fn create_embed_router(cfg: ProviderConfig) -> Arc<dyn EmbedService> {
    Arc::new(build_provider_router(cfg))
}

/// 构建一个作为 [`RerankService`] 使用的 [`EmbedProviderRouter`]（S5 首个业务调用点）。
pub fn create_rerank_router(cfg: ProviderConfig) -> Arc<dyn RerankService> {
    Arc::new(build_provider_router(cfg))
}

/// 构建一个作为 [`LlmService`] 使用的 [`EmbedProviderRouter`]——复用现有 LLM 接入，
/// 供鉴别/过滤等需要自然语言判别的调用使用。与 embed/rerank 共享同一 provider 配置。
pub fn create_llm_router(cfg: ProviderConfig) -> Arc<dyn LlmService> {
    Arc::new(build_provider_router(cfg))
}

// ---------------------------------------------------------------------------
// 测试
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

    #[test]
    fn create_rerank_router_returns_configured_service() {
        let svc = create_rerank_router(ProviderConfig::default_siliconflow());
        // 对象安全 + 构造成功即可（路由正确性由 provider_router 既有测试保证）
        fn _accept(_: Arc<dyn crate::domain::traits::RerankService>) {}
        _accept(svc);
    }
}
