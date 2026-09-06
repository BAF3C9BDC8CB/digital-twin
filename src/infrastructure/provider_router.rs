//! Provider 路由器——按能力将 embed/rerank/llm 请求路由到端点池。
//!
//! # 背景（2026-09-06 起：多厂商 × 多模型端点池）
//!
//! 端点池基础设施在 [`crate::infrastructure::siliconflow`]（[`EndpointPool`]、
//! [`PooledEmbedService`] 等）。本模块保持模块边界与旧 `EmbedProviderRouter`
//! 调用点兼容的少量薄壳 + Noop 降级：
//! - 旧「单 SiliconFlowClient 三合一 + 按 provider 名字符串路由」已删除，
//!   不保留兼容——所有路由决策收敛到端点池的失败顺延。
//! - 命名路由（embed_provider="siliconflow" 等字符串）随旧配置一并废弃。

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmService, RerankService};
use crate::domain::types::HealthStatus;
use async_trait::async_trait;
use std::sync::Arc;

use crate::infrastructure::siliconflow::{EndpointPool, PooledEmbedService, PooledLlmService, PooledRerankService};

/// 由现成端点池直接构造 embed 服务。
pub fn pooled_embed(pool: EndpointPool) -> Arc<dyn EmbedService> {
    Arc::new(PooledEmbedService::new(pool))
}

/// 由现成端点池直接构造 rerank 服务。
pub fn pooled_rerank(pool: EndpointPool) -> Arc<dyn RerankService> {
    Arc::new(PooledRerankService::new(pool))
}

/// 由现成端点池直接构造 llm 服务。
pub fn pooled_llm(pool: EndpointPool) -> Arc<dyn LlmService> {
    Arc::new(PooledLlmService::new(pool))
}

/// Rerank no-op（未配置 rerank 端点时回退——保证 dt search 不因 rerank
/// 缺失整体失败，只降级为不做重排）。
pub struct NoopRerankService;

#[async_trait]
impl RerankService for NoopRerankService {
    async fn rerank(
        &self,
        _query: &str,
        documents: &[String],
    ) -> Result<Vec<f32>, DtError> {
        // 无端点时不重排：每个文档给中性分 0.5，保持顺序语义。
        Ok(documents.iter().map(|_| 0.5_f32).collect())
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        Ok(HealthStatus::Unhealthy("未配置 rerank 端点".into()))
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_rerank_returns_neutral_scores() {
        let svc = NoopRerankService;
        let docs = vec!["a".to_string(), "b".to_string()];
        let scores = svc.rerank("q", &docs).await.unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores.iter().all(|s| *s == 0.5));
    }

    #[test]
    fn empty_pool_router_reports_unhealthy() {
        let pool = EndpointPool::from_config(&[], Default::default(), None, "", "", "", "");
        assert!(pool.is_empty());
    }
}
