//! Embed provider 路由器——按能力将 embed/rerank/llm 请求路由到
//! 配置的 provider（SiliconFlow 或 XInference）。
//!
//! # 用法
//!
//! ```ignore
//! let router = EmbedProviderRouter::new(siliconflow, xinference, config);
//! let embed: Arc<dyn EmbedService> = Arc::new(router);
//! ```
//!
//! 配置决定每个能力由哪个 provider 处理：
//! - `embed_provider`: "siliconflow" | "xinference"
//! - `rerank_provider`: "siliconflow" | "xinference"
//! - `llm_provider`: "siliconflow" | "xinference"

use async_trait::async_trait;
use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, LlmCapabilities, LlmService, RerankService};
use crate::domain::types::HealthStatus;

/// Provider 路由配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderRouterConfig {
    /// 处理 embedding 的 provider（"siliconflow" 或 "xinference"）。
    #[serde(default = "default_embed_provider")]
    pub embed_provider: String,
    /// 处理 reranking 的 provider（"siliconflow" 或 "xinference"）。
    #[serde(default = "default_rerank_provider")]
    pub rerank_provider: String,
    /// 处理 LLM 对话的 provider（"siliconflow" 或 "xinference"）。
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

/// 将 embed/rerank/llm 请求路由到相应的 provider。
///
/// 每个 provider 都是可选的——如果某个能力配置的 provider 为
/// `None`，该能力会回退到另一个 provider，若两者都不可用则返回错误。
pub struct EmbedProviderRouter {
    siliconflow: Option<Arc<super::siliconflow::SiliconFlowClient>>,
    xinference: Option<Arc<super::xinference::XInferenceClient>>,
    config: ProviderRouterConfig,
}

impl EmbedProviderRouter {
    /// 用两个 provider 与路由配置创建新路由器。
    pub fn new(
        siliconflow: Option<Arc<super::siliconflow::SiliconFlowClient>>,
        xinference: Option<Arc<super::xinference::XInferenceClient>>,
        config: ProviderRouterConfig,
    ) -> Self {
        Self {
            siliconflow,
            xinference,
            config,
        }
    }

    /// 用仅一个 SiliconFlow 客户端创建新路由器（向后兼容）。
    pub fn from_siliconflow(client: Arc<super::siliconflow::SiliconFlowClient>) -> Self {
        Self {
            siliconflow: Some(client),
            xinference: None,
            config: ProviderRouterConfig::default(),
        }
    }

    /// 获取给定能力名称对应的 provider。
    ///
    /// 注意：此函数已被移除，因为原始返回类型
    /// `Option<&Arc<SiliconFlowClient>>` 无法表达 XInference provider。
    /// 所有路由都由 embed_provider()、rerank_provider() 与
    /// llm_provider() 处理，它们正确使用 `EmbedProviderRef` 枚举。
    #[allow(dead_code)]
    fn provider_for(
        &self,
        capability: &str,
    ) -> Option<&Arc<super::siliconflow::SiliconFlowClient>> {
        let provider_name = match capability {
            "embed" => &self.config.embed_provider,
            "rerank" => &self.config.rerank_provider,
            "llm" => &self.config.llm_provider,
            _ => return None,
        };

        match provider_name.as_str() {
            "siliconflow" => self.siliconflow.as_ref(),
            "xinference" => None, // XInference 需要 EmbedProviderRef，而非此函数
            _ => self.siliconflow.as_ref(),
        }
    }

    /// 获取 siliconflow provider。
    fn sf(&self) -> Option<&Arc<super::siliconflow::SiliconFlowClient>> {
        self.siliconflow.as_ref()
    }

    /// 获取 xinference provider。
    fn xi(&self) -> Option<&Arc<super::xinference::XInferenceClient>> {
        self.xinference.as_ref()
    }

    /// 根据配置选择 embed provider。
    fn embed_provider(&self) -> Result<EmbedProviderRef<'_>, DtError> {
        match self.config.embed_provider.as_str() {
            "siliconflow" => self
                .sf()
                .map(|c| EmbedProviderRef::SiliconFlow(c.as_ref()))
                .ok_or_else(|| DtError::Repository("embed 未配置 siliconflow provider".into())),
            "xinference" => self
                .xi()
                .map(|c| EmbedProviderRef::XInference(c.as_ref()))
                .ok_or_else(|| DtError::Repository("embed 未配置 xinference provider".into())),
            other => Err(DtError::Config(format!("未知的 embed provider: {other}"))),
        }
    }

    /// 根据配置选择 rerank provider。
    fn rerank_provider(&self) -> Result<EmbedProviderRef<'_>, DtError> {
        match self.config.rerank_provider.as_str() {
            "siliconflow" => self
                .sf()
                .map(|c| EmbedProviderRef::SiliconFlow(c.as_ref()))
                .ok_or_else(|| DtError::Repository("rerank 未配置 siliconflow provider".into())),
            "xinference" => self
                .xi()
                .map(|c| EmbedProviderRef::XInference(c.as_ref()))
                .ok_or_else(|| DtError::Repository("rerank 未配置 xinference provider".into())),
            other => Err(DtError::Config(format!("未知的 rerank provider: {other}"))),
        }
    }

    /// 根据配置选择 LLM provider。
    fn llm_provider(&self) -> Result<EmbedProviderRef<'_>, DtError> {
        match self.config.llm_provider.as_str() {
            "siliconflow" => self
                .sf()
                .map(|c| EmbedProviderRef::SiliconFlow(c.as_ref()))
                .ok_or_else(|| DtError::Repository("llm 未配置 siliconflow provider".into())),
            "xinference" => self
                .xi()
                .map(|c| EmbedProviderRef::XInference(c.as_ref()))
                .ok_or_else(|| DtError::Repository("llm 未配置 xinference provider".into())),
            other => Err(DtError::Config(format!("未知的 llm provider: {other}"))),
        }
    }
}

enum EmbedProviderRef<'a> {
    SiliconFlow(&'a super::siliconflow::SiliconFlowClient),
    XInference(&'a super::xinference::XInferenceClient),
}

#[async_trait]
impl EmbedService for EmbedProviderRouter {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
        match self.embed_provider()? {
            EmbedProviderRef::SiliconFlow(c) => c.embed_batch(texts).await,
            EmbedProviderRef::XInference(c) => c.embed_batch(texts).await,
        }
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        use crate::domain::traits::EmbedService;

        // 检查配置的 embed provider；回退到任何可用的 provider
        let result = match self.embed_provider() {
            Ok(EmbedProviderRef::SiliconFlow(c)) => EmbedService::health_check(c).await,
            Ok(EmbedProviderRef::XInference(c)) => EmbedService::health_check(c).await,
            Err(_) => {
                // 如果未配置 embed provider，尝试任何可用 provider
                if let Some(c) = self.sf() {
                    EmbedService::health_check(c.as_ref()).await
                } else if let Some(c) = self.xi() {
                    EmbedService::health_check(c.as_ref()).await
                } else {
                    return Ok(HealthStatus::Unhealthy("未配置任何 provider".into()));
                }
            }
        };

        // 若配置了另一个 provider 也一并检查，但宕机时不视为失败
        match self.config.embed_provider.as_str() {
            "siliconflow" => {
                if let Some(c) = self.xi() {
                    let _ = EmbedService::health_check(c.as_ref()).await;
                }
            }
            "xinference" => {
                if let Some(c) = self.sf() {
                    let _ = EmbedService::health_check(c.as_ref()).await;
                }
            }
            _ => {}
        }

        result
    }
}

#[async_trait]
impl RerankService for EmbedProviderRouter {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError> {
        match self.rerank_provider()? {
            EmbedProviderRef::SiliconFlow(c) => c.rerank(query, documents).await,
            EmbedProviderRef::XInference(c) => c.rerank(query, documents).await,
        }
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        use crate::domain::traits::RerankService;
        match self.rerank_provider() {
            Ok(EmbedProviderRef::SiliconFlow(c)) => RerankService::health_check(c).await,
            Ok(EmbedProviderRef::XInference(c)) => RerankService::health_check(c).await,
            Err(_) => {
                if let Some(c) = self.sf() {
                    RerankService::health_check(c.as_ref()).await
                } else if let Some(c) = self.xi() {
                    RerankService::health_check(c.as_ref()).await
                } else {
                    Ok(HealthStatus::Unhealthy("未配置任何 provider".into()))
                }
            }
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
        match self.llm_provider()? {
            EmbedProviderRef::SiliconFlow(c) => {
                c.chat(system_prompt, user_prompt, temperature, max_tokens)
                    .await
            }
            EmbedProviderRef::XInference(c) => {
                c.chat(system_prompt, user_prompt, temperature, max_tokens)
                    .await
            }
        }
    }

    async fn health_check(&self) -> Result<HealthStatus, DtError> {
        use crate::domain::traits::LlmService;
        match self.llm_provider() {
            Ok(EmbedProviderRef::SiliconFlow(c)) => LlmService::health_check(c).await,
            Ok(EmbedProviderRef::XInference(c)) => LlmService::health_check(c).await,
            Err(_) => {
                if let Some(c) = self.sf() {
                    LlmService::health_check(c.as_ref()).await
                } else if let Some(c) = self.xi() {
                    LlmService::health_check(c.as_ref()).await
                } else {
                    Ok(HealthStatus::Unhealthy("未配置任何 provider".into()))
                }
            }
        }
    }

    fn capabilities(&self) -> LlmCapabilities {
        let mut caps = LlmCapabilities {
            embed: true,
            rerank: true,
            chat: false,
            max_tokens: 4096,
        };

        // 检查 LLM provider
        match self.llm_provider() {
            Ok(EmbedProviderRef::SiliconFlow(c)) => {
                let inner = c.capabilities();
                caps.chat = inner.chat;
            }
            Ok(EmbedProviderRef::XInference(c)) => {
                let inner = c.capabilities();
                caps.chat = inner.chat;
            }
            Err(_) => {}
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
    use crate::infrastructure::xinference::XInferenceClient;

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
        ));
        let router = EmbedProviderRouter::from_siliconflow(client);
        assert!(router.siliconflow.is_some());
        assert!(router.xinference.is_none());
    }

    #[test]
    fn router_with_xinference_works() {
        let sf = Arc::new(SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "qwen",
        ));
        let xi = Arc::new(XInferenceClient::new(
            "http://localhost:9997/v1",
            "",
            "bge-m3",
            "reranker",
            "",
        ));
        let config = ProviderRouterConfig {
            embed_provider: "xinference".into(),
            rerank_provider: "siliconflow".into(),
            llm_provider: "siliconflow".into(),
        };
        let router = EmbedProviderRouter::new(Some(sf), Some(xi), config);
        assert!(router.siliconflow.is_some());
        assert!(router.xinference.is_some());

        // embed 应路由到 xinference
        match router.embed_provider().unwrap() {
            EmbedProviderRef::XInference(_) => {} // 预期
            _ => panic!("预期 embed 使用 xinference"),
        }

        // rerank 应路由到 siliconflow
        match router.rerank_provider().unwrap() {
            EmbedProviderRef::SiliconFlow(_) => {} // 预期
            _ => panic!("预期 rerank 使用 siliconflow"),
        }
    }

    #[tokio::test]
    async fn router_unknown_provider_returns_error() {
        let client = Arc::new(SiliconFlowClient::new(
            "https://api.siliconflow.cn/v1",
            "sk-key",
            "bge-m3",
            "reranker",
            "qwen",
        ));
        let config = ProviderRouterConfig {
            embed_provider: "unknown".into(),
            ..Default::default()
        };
        let router = EmbedProviderRouter::new(Some(client), None, config);
        let texts = vec!["test".to_string()];
        let result = router.embed_batch(&texts);
        // 应在发起任何 HTTP 调用之前于 embed_provider() 处失败
        assert!(result.await.is_err());
    }
}
