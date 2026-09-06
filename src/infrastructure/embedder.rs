//! Embedding / Rerank / LLM 服务工厂——从 `providers` 端点池构建路由。
//!
//! # 架构（2026-09-06 起：多厂商 × 多模型端点池）
//!
//! `config/pipeline.yaml → providers.{llm,embed,rerank}` 各是一组端点
//! （[`ProviderEndpoint`]）。此处按能力把端点构建为 [`EndpointPool`]，
//! 再包成实现 [`EmbedService`] / [`RerankService`] / [`LlmService`] trait 的
//! 池化服务——请求失败自动顺延到池内下一个端点（多模型失败换下一个）。
//!
//! 旧 `providers.siliconflow` 单块与 `SiliconFlowClient` 三合一结构已移除，
//! 不保留兼容路径。

use crate::domain::traits::{EmbedService, LlmService, RerankService};
use crate::domain::types::HealthStatus;
use crate::infrastructure::siliconflow::{EndpointPool, PooledEmbedService, PooledLlmService, PooledRerankService};
use std::sync::Arc;

use crate::application::pipeline::config::{PipelineConfig, ProvidersConfig};

// ---------------------------------------------------------------------------
// NoopEmbedService——测试/离线开发使用的零向量服务
// ---------------------------------------------------------------------------

/// 返回零向量的 no-op 向量化服务。
///
/// 在无可用 embed 端点时使用；维度可配置，用于测试环境。
pub struct NoopEmbedService {
    dim: u32,
}

impl NoopEmbedService {
    /// 创建返回 `dim` 维零向量的 no-op 服务（默认 1024，兼容 BGE-M3）。
    pub fn new(dim: u32) -> Self {
        Self { dim }
    }
}

impl Default for NoopEmbedService {
    fn default() -> Self {
        Self { dim: 1024 }
    }
}

#[async_trait::async_trait]
impl EmbedService for NoopEmbedService {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, crate::domain::error::DtError> {
        Ok(texts
            .iter()
            .map(|_| vec![0.0_f32; self.dim as usize])
            .collect())
    }

    async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
        Ok(HealthStatus::Healthy)
    }
}

// ---------------------------------------------------------------------------
// 工厂——从 PipelineConfig 构建三种能力的池化服务
// ---------------------------------------------------------------------------

/// 一次构建 embed/rerank/llm 三种池化服务（配置加载 + 端点构建收敛于此）。
///
/// 池为空（某能力未配置端点）时对应返回 None——调用方按需降级。
pub struct PooledServices {
    pub embed: Option<Arc<dyn EmbedService>>,
    pub rerank: Option<Arc<dyn RerankService>>,
    pub llm: Option<Arc<dyn LlmService>>,
}

/// 从已加载的 pipeline 配置构建三池。
pub fn build_pooled_services(cfg: &PipelineConfig) -> PooledServices {
    let Some(providers) = cfg.providers.as_ref() else {
        return PooledServices {
            embed: None,
            rerank: None,
            llm: None,
        };
    };

    let strategy = providers.strategy;
    let global_proxy = providers.proxy.clone();
    let llm_default = cfg.llm_model();
    let embed_default = cfg.embed_model();
    let rerank_default = cfg.rerank_model();

    let embed = if providers.embed.is_empty() {
        None
    } else {
        Some(Arc::new(PooledEmbedService::new(EndpointPool::from_config(
            &providers.embed,
            strategy,
            global_proxy.as_ref(),
            &embed_default,
            &embed_default,
            &rerank_default,
            &llm_default,
        ))) as Arc<dyn EmbedService>)
    };

    let rerank = if providers.rerank.is_empty() {
        None
    } else {
        Some(Arc::new(PooledRerankService::new(EndpointPool::from_config(
            &providers.rerank,
            strategy,
            global_proxy.as_ref(),
            &rerank_default,
            &embed_default,
            &rerank_default,
            &llm_default,
        ))) as Arc<dyn RerankService>)
    };

    let llm = if providers.llm.is_empty() {
        None
    } else {
        Some(Arc::new(PooledLlmService::new(EndpointPool::from_config(
            &providers.llm,
            strategy,
            global_proxy.as_ref(),
            &llm_default,
            &embed_default,
            &rerank_default,
            &llm_default,
        ))) as Arc<dyn LlmService>)
    };

    PooledServices {
        embed,
        rerank,
        llm,
    }
}

/// 从已加载配置构建 embed 服务（无端点时回退 Noop，保持调用方健壮）。
pub fn create_embed_service(cfg: &PipelineConfig) -> Arc<dyn EmbedService> {
    build_pooled_services(cfg)
        .embed
        .unwrap_or_else(|| Arc::new(NoopEmbedService::default()))
}

/// 便捷工厂（供 build.rs / router.rs / sync.rs 旧调用点收敛）：加载配置 → 取 embed。
pub fn create_search_embed_client() -> Arc<dyn EmbedService> {
    match PipelineConfig::load() {
        Ok(cfg) => create_embed_service(&cfg),
        Err(e) => {
            tracing::warn!("无法加载 pipeline.yaml ({e})，回退 Noop embed");
            Arc::new(NoopEmbedService::default())
        }
    }
}

/// 便捷工厂：加载配置 → 取 rerank。
pub fn create_search_rerank_client() -> Arc<dyn crate::domain::traits::RerankService> {
    match PipelineConfig::load() {
        Ok(cfg) => build_pooled_services(&cfg)
            .rerank
            .unwrap_or_else(|| Arc::new(crate::infrastructure::provider_router::NoopRerankService)),
        Err(e) => {
            tracing::warn!("无法加载 pipeline.yaml ({e})，回退 Noop rerank");
            Arc::new(crate::infrastructure::provider_router::NoopRerankService)
        }
    }
}

/// 便捷工厂：加载配置 → 取 llm（供 router LLM 门控 / 结果过滤）。
pub fn create_search_llm_client() -> Arc<dyn LlmService> {
    match PipelineConfig::load() {
        Ok(cfg) => build_pooled_services(&cfg)
            .llm
            .unwrap_or_else(|| Arc::new(NoopLlmService)),
        Err(e) => {
            tracing::warn!("无法加载 pipeline.yaml ({e})，回退 Noop llm");
            Arc::new(NoopLlmService)
        }
    }
}

/// 旧 `ProviderConfig` 工厂的语义替代——直接由 pipeline 配置构建路由器。
/// 保留此模块级函数以最小化 build.rs / runtime.rs 改动（参数即来源）。
#[deprecated(note = "请直接用 build_pooled_services / create_* 工厂")]
pub fn provider_config_from_pipeline() -> ProvidersConfig {
    PipelineConfig::load()
        .ok()
        .and_then(|c| c.providers)
        .unwrap_or_default()
}

/// 由 providers 配置 + 模型默认值构建三池（供旧调用点直接使用配置对象时）。
pub fn build_router_from_providers(
    providers: &ProvidersConfig,
    llm_default: &str,
    embed_default: &str,
    rerank_default: &str,
) -> PooledServices {
    let strategy = providers.strategy;
    let global_proxy = providers.proxy.clone();

    PooledServices {
        embed: if providers.embed.is_empty() {
            None
        } else {
            Some(Arc::new(PooledEmbedService::new(EndpointPool::from_config(
                &providers.embed,
                strategy,
                global_proxy.as_ref(),
                embed_default,
                embed_default,
                rerank_default,
                llm_default,
            ))) as Arc<dyn EmbedService>)
        },
        rerank: if providers.rerank.is_empty() {
            None
        } else {
            Some(Arc::new(PooledRerankService::new(EndpointPool::from_config(
                &providers.rerank,
                strategy,
                global_proxy.as_ref(),
                rerank_default,
                embed_default,
                rerank_default,
                llm_default,
            ))) as Arc<dyn RerankService>)
        },
        llm: if providers.llm.is_empty() {
            None
        } else {
            Some(Arc::new(PooledLlmService::new(EndpointPool::from_config(
                &providers.llm,
                strategy,
                global_proxy.as_ref(),
                llm_default,
                embed_default,
                rerank_default,
                llm_default,
            ))) as Arc<dyn LlmService>)
        },
    }
}

/// 无端点时的 LLM no-op（health 报告不可用，chat 报错）。
pub struct NoopLlmService;

#[async_trait::async_trait]
impl LlmService for NoopLlmService {
    async fn chat(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<String, crate::domain::error::DtError> {
        Err(crate::domain::error::DtError::Repository(
            "未配置任何 LLM 端点".into(),
        ))
    }

    async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
        Ok(HealthStatus::Unhealthy("未配置任何 LLM 端点".into()))
    }

    fn capabilities(&self) -> crate::domain::traits::LlmCapabilities {
        crate::domain::traits::LlmCapabilities {
            chat: false,
            ..Default::default()
        }
    }
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
    fn empty_providers_builds_none() {
        let cfg = PipelineConfig::default();
        let svcs = build_pooled_services(&cfg);
        assert!(svcs.embed.is_none());
        assert!(svcs.rerank.is_none());
        assert!(svcs.llm.is_none());
    }
}
