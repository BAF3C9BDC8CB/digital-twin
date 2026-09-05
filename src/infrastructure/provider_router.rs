//! Embed provider 路由器——按能力将 embed/rerank/llm 请求路由到
//! 配置的 provider（SiliconFlow OpenAI 兼容网关，唯一实现）。
//!
//! # 背景
//!
//! SiliconFlow 官方 API 本身即 OpenAI 兼容超集：
//! - `/chat/completions`：OpenAI 标准（chat-completions-post）
//! - `/embeddings`：OpenAI 标准（embeddings-post）
//! - `/rerank`：SiliconFlow 私有扩展端点（rerank-post，非 OpenAI 标准）
//!
//! 因此 `SiliconFlowClient` 是唯一 provider 实现，同时承载三种能力。
//! 配置名仅 `siliconflow`。

use async_trait::async_trait;
use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// Provider 路由配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderRouterConfig {
    /// 处理 embedding 的 provider（当前仅支持 "siliconflow"）。
    #[serde(default = "default_embed_provider")]
    pub embed_provider: String,
    /// 处理 reranking 的 provider（当前仅支持 "siliconflow"）。
    #[serde(default = "default_rerank_provider")]
    pub rerank_provider: String,
    /// 处理 LLM 对话的 provider（当前仅支持 "siliconflow"）。
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,
}

impl Default for ProviderRouterConfig {
    fn default() -> Self {
        Self {
            embed_provider: default_embed_provider(),
            rerank_provider: default_rerank_provider(),
            llm_provider: default_llm_provider(),
        }
    }
}

fn default_embed_provider() -> String {
    "siliconflow".to_string()
}
fn default_rerank_provider() -> String {
    "siliconflow".to_string()
}
fn default_llm_provider() -> String {
    "siliconflow".to_string()
}

/// 校验 provider 配置名是否受支持（当前仅 SiliconFlow）。
fn ensure_supported(name: &str, capability: &str) -> Result<(), DtError> {
    match name {
        "siliconflow" => Ok(()),
        other => Err(DtError::Config(format!(
            "未知的 {capability} provider: {other}"
        ))),
    }
}

/// 将 embed/rerank/llm 请求路由到 SiliconFlow provider。
///
/// provider 可选——未配置时对应能力返回错误，health_check 则回退报告不可用。
pub struct EmbedProviderRouter {
    siliconflow: Option<Arc<super::siliconflow::SiliconFlowClient>>,
    config: ProviderRouterConfig,
}

impl EmbedProviderRouter {
    /// 用 SiliconFlow 客户端与路由配置创建新路由器。
    pub fn new(
        siliconflow: Option<Arc<super::siliconflow::SiliconFlowClient>>,
        config: ProviderRouterConfig,
    ) -> Self {
        Self {
            siliconflow,
            config,
        }
    }

    /// 用仅一个 SiliconFlow 客户端创建新路由器（向后兼容）。
    pub fn from_siliconflow(client: Arc<super::siliconflow::SiliconFlowClient>) -> Self {
        Self {
            siliconflow: Some(client),
            config: ProviderRouterConfig::default(),
        }
    }

    /// 获取 siliconflow provider。
    fn sf(&self) -> Option<&Arc<super::siliconflow::SiliconFlowClient>> {
        self.siliconflow.as_ref()
    }

    /// 根据配置选择 embed provider。
    fn embed_provider(&self) -> Result<&Arc<super::siliconflow::SiliconFlowClient>, DtError> {
        ensure_supported(&self.config.embed_provider, "embed")?;
        self.sf()
            .ok_or_else(|| DtError::Repository("embed 未配置 siliconflow provider".into()))
    }

    /// 根据配置选择 rerank provider。
    fn rerank_provider(&self) -> Result<&Arc<super::siliconflow::SiliconFlowClient>, DtError> {
        ensure_supported(&self.config.rerank_provider, "rerank")?;
        self.sf()
            .ok_or_else(|| DtError::Repository("rerank 未配置 siliconflow provider".into()))
    }

    /// 根据配置选择 LLM provider。
    fn llm_provider(&self) -> Result<&Arc<super::siliconflow::SiliconFlowClient>, DtError> {
        ensure_supported(&self.config.llm_provider, "llm")?;
        self.sf()
            .ok_or_else(|| DtError::Repository("llm 未配置 siliconflow provider".into()))
    }
}

#[async_trait]
impl EmbedService for EmbedProviderRouter {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        self.embed_provider()?.embed_batch(texts).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        match self.embed_provider() {
            Ok(c) => EmbedService::health_check(c.as_ref()).await,
            Err(_) => match self.sf() {
                Some(c) => EmbedService::health_check(c.as_ref()).await,
                None => Ok(HealthStatus::Unhealthy(
                    "未配置 siliconflow provider".into(),
                )),
            },
        }
    }
}

#[async_trait]
impl RerankService for EmbedProviderRouter {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        self.rerank_provider()?.rerank(query, documents).await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        match self.rerank_provider() {
            Ok(c) => RerankService::health_check(c.as_ref()).await,
            Err(_) => match self.sf() {
                Some(c) => RerankService::health_check(c.as_ref()).await,
                None => Ok(HealthStatus::Unhealthy(
                    "未配置 siliconflow provider".into(),
                )),
            },
        }
    }
}

#[async_trait]
impl LlmService for EmbedProviderRouter {
    async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError> {
        self.llm_provider()?
            .chat(system_prompt, user_prompt, temperature, max_tokens)
            .await
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        use crate::domain::traits::LlmService;
        match self.llm_provider() {
            Ok(c) => LlmService::health_check(c.as_ref()).await,
            Err(_) => match self.sf() {
                Some(c) => LlmService::health_check(c.as_ref()).await,
                None => Ok(HealthStatus::Unhealthy(
                    "未配置 siliconflow provider".into(),
                )),
            },
        }
    }

    fn capabilities(&self) -> LlmCapabilities {
        let mut caps = LlmCapabilities {
            embed: true,
            rerank: true,
            chat: false,
            max_tokens: 4096,
        };
        if let Ok(c) = self.llm_provider() {
            let inner = c.capabilities();
            caps.chat = inner.chat;
        }
        caps
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::siliconflow::SiliconFlowClient;

    #[test]
    fn router_uses_default_config() {
        let config = ProviderRouterConfig::default();
        assert_eq!(config.embed_provider, "siliconflow");
        assert_eq!(config.rerank_provider, "siliconflow");
        assert_eq!(config.llm_provider, "siliconflow");
    }

    #[test]
    fn router_from_siliconflow_works() {
        let client = Arc::new(SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "qwen",
            20,
        ));
        let router = EmbedProviderRouter::from_siliconflow(client);
        assert!(router.siliconflow.is_some());
    }

    #[tokio::test]
    async fn router_unknown_provider_returns_error() {
        let client = Arc::new(SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "qwen",
            20,
        ));
        let config = ProviderRouterConfig {
            embed_provider: "unknown".into(),
            ..Default::default()
        };
        let router = EmbedProviderRouter::new(Some(client), config);
        let texts = vec!["test".to_string()];
        let result = router.embed_batch(&texts);
        // 应在发起任何 HTTP 调用之前于 embed_provider() 处失败
        assert!(result.await.is_err());
    }
}
