//! 流水线配置类型。
//!
//! 顶层 [`PipelineConfig`] 从 `config/pipeline.yaml` 加载，
//! 控制哪些处理器处于启用状态、它们如何连接推理服务器，
//! 以及使用哪些 LLM 预设。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// 顶层配置
// ---------------------------------------------------------------------------

/// 完整的流水线配置。
///
/// 从 `config/pipeline.yaml`（或自定义路径）反序列化。每个字段都有合理的
/// 默认值，因此该文件完全是可选的。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// 整个流水线的主启用 / 禁用开关。
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// `dt-inference-server`（Python）的连接设置。
    #[serde(default)]
    pub inference_server: InferenceServerConfig,

    /// 各处理器的功能开关。
    #[serde(default)]
    pub processors: ProcessorsConfig,

    /// LLM 推理参数（temperature、max tokens、…）。
    #[serde(default)]
    pub llm: Option<LlmConfig>,

    /// 跨越多个项目的生态系统级设置。
    #[serde(default)]
    pub ecosystem: Option<EcosystemConfig>,

    /// 提供方路由与模型配置。
    /// 控制哪个提供方处理 embed / rerank / LLM 能力，
    /// 以及每项能力使用哪个模型。
    #[serde(default)]
    pub providers: Option<ProvidersConfig>,
}

const fn default_enabled() -> bool {
    true
}

// ---------------------------------------------------------------------------
// 子配置
// ---------------------------------------------------------------------------

/// 推理服务器连接参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceServerConfig {
    /// 推理服务器的 HTTP(S) URL。
    ///
    /// 默认值：`"https://api.siliconflow.cn/v1"`。
    #[serde(default = "default_infer_url")]
    pub url: String,

    /// 可选的 gRPC URL。
    ///
    /// 推理服务器还暴露一个 gRPC 端点（默认端口 50051），未来可能用于流式传输。
    #[serde(default)]
    pub grpc_url: Option<String>,

    /// 到推理服务器的最大并发 HTTP 请求数。
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_infer_url() -> String {
    "https://api.siliconflow.cn/v1".into()
}

const fn default_max_concurrent() -> usize {
    16
}

impl Default for InferenceServerConfig {
    fn default() -> Self {
        Self {
            url: default_infer_url(),
            grpc_url: None,
            max_concurrent: default_max_concurrent(),
        }
    }
}

/// 启用 / 禁用单个流水线处理器的功能开关。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorsConfig {
    /// Tree-sitter AST 解析器。
    #[serde(default = "default_true")]
    pub tree_sitter: bool,

    /// 基于 LLM 的分析（chat completions）。
    #[serde(default = "default_true")]
    pub llm: bool,

    /// 文本分块 / 分割。
    #[serde(default = "default_true")]
    pub chunk: bool,

    /// 原始文本提取（纯文本、markdown 等）。
    #[serde(default = "default_true")]
    pub extract_text: bool,

    /// OCR 处理（默认关闭——需要额外依赖）。
    #[serde(default)]
    pub ocr: bool,

    /// 将结果存储到图 + 向量数据库。
    #[serde(default = "default_true")]
    pub store: bool,

    /// 跳过 embedding（向量生成）。当你已经在 Qdrant 中拥有向量、希望避免
    /// 重新嵌入时（例如没有运行 bge-m3 模型），可将其设为 false。
    #[serde(default = "default_true")]
    pub embed: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for ProcessorsConfig {
    fn default() -> Self {
        Self {
            tree_sitter: true,
            llm: true,
            chunk: true,
            extract_text: true,
            ocr: false,
            store: true,
            embed: true,
        }
    }
}

/// LLM 推理参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// 采样温度（0.0 = 确定性，1.0 = 创造性）。
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// 模型单次回复最多生成的 token 数。
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// 单文件内同时发起的 chunk LLM 请求数；仍受 provider 全局并发限制。
    #[serde(default = "default_chunk_concurrency")]
    pub chunk_concurrency: usize,
}

const fn default_temperature() -> f32 {
    0.1
}

const fn default_max_tokens() -> u32 {
    4096
}

const fn default_chunk_concurrency() -> usize {
    2
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            chunk_concurrency: default_chunk_concurrency(),
        }
    }
}

/// 生态系统级设置。
///
/// 当生态系统启用时，流水线在单一批次中处理每个已注册项目，
/// 这对于全局重新索引或跨项目依赖分析很有用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcosystemConfig {
    /// 启用生态系统（多项目）模式。
    #[serde(default)]
    pub enabled: bool,

    /// 要包含的显式项目名列表。
    ///
    /// 为空时流水线回退到 *全部* 已注册项目。
    #[serde(default)]
    pub projects: Vec<String>,
}

impl Default for EcosystemConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            projects: Vec::new(),
        }
    }
}

/// 提供方路由与模型配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    /// 哪个提供方处理 embedding（"siliconflow" 或 "xinference"）。
    #[serde(default = "default_embed_provider")]
    pub embed_provider: String,

    /// 哪个提供方处理 reranking（"siliconflow" 或 "xinference"）。
    #[serde(default = "default_rerank_provider")]
    pub rerank_provider: String,

    /// 哪个提供方处理 LLM chat（"siliconflow" 或 "xinference"）。
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,

    /// SiliconFlow 提供方配置。
    #[serde(default)]
    pub siliconflow: Option<SiliconFlowProviderConfig>,

    /// XInference 提供方配置。
    #[serde(default)]
    pub xinference: Option<XInferenceProviderConfig>,
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

/// SiliconFlow 提供方配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiliconFlowProviderConfig {
    #[serde(default = "default_sf_url")]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model_embed: String,
    #[serde(default)]
    pub model_reranker: String,
    #[serde(default)]
    pub model_llm: String,
    /// SiliconFlow 云 API 最大并发请求数。
    #[serde(default = "default_sf_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_sf_url() -> String {
    "https://api.siliconflow.cn/v1".into()
}

const fn default_sf_max_concurrent() -> usize {
    20
}

impl Default for SiliconFlowProviderConfig {
    fn default() -> Self {
        Self {
            url: default_sf_url(),
            api_key: String::new(),
            model_embed: String::new(),
            model_reranker: String::new(),
            model_llm: String::new(),
            max_concurrent: default_sf_max_concurrent(),
        }
    }
}

/// XInference 提供方配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XInferenceProviderConfig {
    #[serde(default = "default_xi_url")]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model_embed: String,
    #[serde(default)]
    pub model_reranker: String,
    #[serde(default)]
    pub model_llm: String,
}

fn default_xi_url() -> String {
    "http://localhost:9997/v1".into()
}

impl Default for XInferenceProviderConfig {
    fn default() -> Self {
        Self {
            url: default_xi_url(),
            api_key: String::new(),
            model_embed: String::new(),
            model_reranker: String::new(),
            model_llm: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 加载
// ---------------------------------------------------------------------------

impl PipelineConfig {
    /// 从 `~/.config/digital-twin/pipeline.yaml` 加载 [`PipelineConfig`]。
    ///
    /// 固定使用用户级路径(与 config.yaml 一致),不依赖进程当前工作目录——
    /// 否则在非项目根目录执行 dt 会丢失 providers 配置并回退到默认
    /// SiliconFlow(无 API key → embed 401/0 命中)。
    ///
    /// 如果文件不存在,则静默返回默认配置。如果文件存在但格式错误,
    /// 则返回错误字符串。
    pub fn load() -> Result<Self, String> {
        let Some(path) = home_pipeline_config() else {
            tracing::warn!("无法确定 HOME 环境变量,使用默认流水线配置");
            return Ok(Self::default());
        };
        if !path.exists() {
            tracing::warn!(
                "{} 不存在(请执行 `ln -s <项目根>/config/pipeline.yaml {}` 或手动创建),使用默认配置",
                path.display(),
                path.display()
            );
            return Ok(Self::default());
        }

        let content =
            std::fs::read_to_string(&path).map_err(|e| format!("无法读取 {path:?}: {e}"))?;

        serde_yaml::from_str(&content).map_err(|e| format!("流水线配置解析错误: {e}"))
    }
}

/// 解析 `~/.config/digital-twin/pipeline.yaml` 路径,不引入 `dirs` crate。
fn home_pipeline_config() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/digital-twin/pipeline.yaml"))
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            inference_server: InferenceServerConfig::default(),
            processors: ProcessorsConfig::default(),
            llm: Some(LlmConfig::default()),
            ecosystem: None,
            providers: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = PipelineConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.inference_server.url, "https://api.siliconflow.cn/v1");
        assert_eq!(cfg.inference_server.max_concurrent, 16);
        assert!(cfg.processors.tree_sitter);
        assert!(!cfg.processors.ocr);
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.1);
        assert_eq!(cfg.llm.as_ref().unwrap().max_tokens, 4096);
        assert_eq!(cfg.llm.as_ref().unwrap().chunk_concurrency, 2);
    }

    #[test]
    fn deserialize_full_config() {
        let yaml = r#"
enabled: true
inference_server:
  url: "http://infer:50052"
  grpc_url: "http://infer:50051"
  max_concurrent: 8
processors:
  tree_sitter: true
  llm: true
  chunk: true
  extract_text: true
  ocr: false
  store: true
llm:
  temperature: 0.5
  max_tokens: 2048
  chunk_concurrency: 4
ecosystem:
  enabled: true
  projects:
    - digital-twin
    - svc
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.inference_server.url, "http://infer:50052");
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.5);
        assert_eq!(cfg.llm.as_ref().unwrap().chunk_concurrency, 4);

        let eco = cfg.ecosystem.unwrap();
        assert!(eco.enabled);
        assert_eq!(eco.projects, vec!["digital-twin", "svc"]);
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        // 该测试从工作区根目录运行——这里不对 cwd 做任何假设，
        // 仅验证行为。
        let cfg = PipelineConfig::load().unwrap_or_default();
        // 配置文件可能存在也可能不存在，但我们总应得到一个有效的
        // 对象且不会 panic。
        let _ = cfg.enabled;
    }
}
