//! Pipeline LLM 对话客户端（OpenAI 兼容，多端点失败顺延）。
//!
//! 所有 pipeline LLM chat 请求都流经该客户端。2026-09-06 起：
//! - 旧单一 `SiliconFlowChatClient`（固定一份 url/key、模型名由调用方传入、
//!   `enable_thinking` 特判）重构为 [`PooledChatClient`]——内部持 [`EndpointPool`]，
//!   模型名按端点级覆盖解析，请求失败自动顺延到下一端点/模型；
//! - 健康检查与超时/信号量语义保留。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::application::pipeline::config::{PipelineConfig, ProviderEndpoint};
use crate::infrastructure::siliconflow::{EndpointPool, OpenAiEndpoint};

// ---------------------------------------------------------------------------
// 公开响应 / DTO 类型
// ---------------------------------------------------------------------------

/// OpenAI 兼容的 chat completion 响应。
#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: Message,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    #[serde(deserialize_with = "deserialize_message_content")]
    pub content: String,
}

fn deserialize_message_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                serde_json::Value::String(s) => Some(s),
                serde_json::Value::Object(item) => {
                    item.get("text").and_then(|v| v.as_str()).map(str::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    })
}

// ---------------------------------------------------------------------------
// PooledChatClient——多端点失败顺延的对话客户端
// ---------------------------------------------------------------------------

/// 由 `providers.llm` 端点池支持的对话客户端。
///
/// 实现 [`ChatClient`]（带 `model` 参数）。端点池内每个端点已按其配置
/// 解析模型名；调用方传入的 `model` 仅用于日志/兼容（旧 SiliconFlowChatClient
/// 曾以 `"default"` 调用且 URL 拼接少了一段 `/v1` 路径——现已统一走
/// [`OpenAiEndpoint`] 的标准路径拼接）。
pub struct PooledChatClient {
    pool: EndpointPool,
}

impl PooledChatClient {
    /// 从 `providers.llm` 端点池构建。
    pub fn from_providers(
        eps: &[ProviderEndpoint],
        strategy: crate::application::pipeline::config::EndpointStrategy,
        global_proxy: Option<&crate::application::pipeline::config::ProxyConfig>,
        default_model: &str,
        embed_default: &str,
        rerank_default: &str,
    ) -> Self {
        let pool = EndpointPool::from_config(
            eps,
            strategy,
            global_proxy,
            default_model,
            embed_default,
            rerank_default,
            default_model,
        );
        Self { pool }
    }

    /// 从已加载 pipeline 配置构建（llm 池）。
    pub fn from_pipeline(cfg: &PipelineConfig) -> Self {
        let providers = cfg.providers.as_ref();
        let (eps, strategy, proxy) = match providers {
            Some(p) => (&p.llm[..], p.strategy, p.proxy.as_ref()),
            None => (&[][..], Default::default(), None),
        };
        Self::from_providers(eps, strategy, proxy, &cfg.llm_model(), &cfg.embed_model(), &cfg.rerank_model())
    }

    /// 池是否为空。
    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    /// 池内端点数量（日志/诊断用）。
    pub fn pool_len(&self) -> usize {
        self.pool.len()
    }

    /// 构建一个自身池的浅拷贝（Arc 共享端点成员）——给要求持有
    /// `Arc<dyn ChatClient>` 的处理器复用。
    pub fn to_arc(self) -> Arc<dyn ChatClient> {
        Arc::new(self)
    }
}

#[async_trait]
impl ChatClient for PooledChatClient {
    async fn chat(
        &self,
        _model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        _json_mode: bool,
        file: &str,
        chunk: &str,
    ) -> Result<ChatResponse, String> {
        // 池化请求：端点内已解析模型名；file/chunk 仅用于日志（pool.run 内
        // 端点会打印自身 label；此处把来源并入 user_prompt 无意义，日志由
        // 上层 processor 自带 file/chunk 上下文——OpenAiEndpoint.chat 是
        // 纯 chat 语义，返回值与原一致）。
        let _ = (file, chunk);

        // pool.run 的错误类型为 DtError；这里直接让 String 错误作为 E，
        // 需要 DtError: From<String> 不存在——改用 DtError 并最后转 String。
        let system_prompt = system_prompt.to_string();
        let user_prompt = user_prompt.to_string();
        let text = self
            .pool
            .run(move |endpoint, _| {
                let system_prompt = system_prompt.clone();
                let user_prompt = user_prompt.clone();
                Box::pin(async move {
                    endpoint
                        .chat(&system_prompt, &user_prompt, temperature, max_tokens)
                        .await
                })
            })
            .await
            .map_err(|e| e.to_string())?;

        Ok(ChatResponse {
            choices: vec![Choice {
                message: Message { content: text },
            }],
        })
    }

    async fn health_check(&self) -> Result<bool, String> {
        match self.pool.health_check().await {
            crate::domain::types::HealthStatus::Healthy => Ok(true),
            _ => Ok(false),
        }
    }
}

// ---------------------------------------------------------------------------
// 统一对话客户端 trait
// ---------------------------------------------------------------------------

/// 统一对话客户端 trait——由池化客户端实现。
#[async_trait]
pub trait ChatClient: Send + Sync {
    /// `file`/`chunk` 为请求来源上下文（仅用于日志/追踪；无上下文时可传 ""）。
    async fn chat(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
        json_mode: bool,
        file: &str,
        chunk: &str,
    ) -> Result<ChatResponse, String>;
    async fn health_check(&self) -> Result<bool, String>;
}

// ---------------------------------------------------------------------------
// 兼容别名（旧结构已删除——新代码用 PooledChatClient）
// ---------------------------------------------------------------------------

/// 旧 `SiliconFlowChatClient` 兼容构造：默认池化客户端（从 pipeline 配置）。
pub type SiliconFlowChatClient = PooledChatClient;

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_response_accepts_null_and_array_content() {
        let null: ChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"message": {"content": null}}]
        }))
        .unwrap();
        assert_eq!(null.choices[0].message.content, "");
        let parts: ChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"message": {"content": [{"type":"text","text":"a"},{"text":"b"}]}}]
        }))
        .unwrap();
        assert_eq!(parts.choices[0].message.content, "ab");
        let strings: ChatResponse = serde_json::from_value(serde_json::json!({
            "choices": [{"message": {"content": ["a", "b"]}}]
        }))
        .unwrap();
        assert_eq!(strings.choices[0].message.content, "ab");
    }

    #[test]
    fn empty_pipeline_builds_empty_pool() {
        let client = PooledChatClient::from_pipeline(&PipelineConfig::default());
        assert!(client.is_empty());
    }

    #[test]
    fn chat_response_roundtrip() {
        let json = r#"{"choices":[{"message":{"content":"Hello!"}}]}"#;
        let resp: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content, "Hello!");
    }
}
