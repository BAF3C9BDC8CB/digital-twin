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

    /// KG Router 路由与过滤配置。
    #[serde(default)]
    pub kg_router: Option<KgRouterConfig>,
}

const fn default_enabled() -> bool {
    true
}

// ---------------------------------------------------------------------------
// 子配置
// ---------------------------------------------------------------------------

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
}

const fn default_temperature() -> f32 {
    0.1
}

const fn default_max_tokens() -> u32 {
    4096
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
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

    /// 通用 OpenAI-compatible LLM provider（glmcoding / opencode-go 等任意网关）。
    /// 旧配置键 `glmcoding` 仍可反序列化（serde alias 兼容）。
    #[serde(default, alias = "glmcoding")]
    pub openai_compatible: Option<OpenAICompatibleProviderConfig>,
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
    /// LLM 单次回复最大 token 数（与模型上下文长度相关，可按模型调整）。
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
}

const fn default_llm_max_tokens() -> u32 {
    512
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
            max_tokens: default_llm_max_tokens(),
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
    /// 本地推理服务最大并发请求数（默认 16，与历史 inference_server 默认一致）。
    #[serde(default = "default_xi_max_concurrent")]
    pub max_concurrent: usize,
    /// LLM 单次回复最大 token 数（与模型上下文长度相关，可按模型调整）。
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
}

fn default_xi_url() -> String {
    "http://localhost:9997/v1".into()
}

const fn default_xi_max_concurrent() -> usize {
    16
}

impl Default for XInferenceProviderConfig {
    fn default() -> Self {
        Self {
            url: default_xi_url(),
            api_key: String::new(),
            model_embed: String::new(),
            model_reranker: String::new(),
            model_llm: String::new(),
            max_concurrent: default_xi_max_concurrent(),
            max_tokens: default_llm_max_tokens(),
        }
    }
}

/// 通用 OpenAI-compatible provider configuration（glmcoding / opencode-go 等网关）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAICompatibleProviderConfig {
    #[serde(default = "default_openai_compatible_url")]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_openai_compatible_model")]
    pub model_llm: String,
    #[serde(default = "default_openai_compatible_protocol")]
    pub protocol: String,
    #[serde(default = "default_openai_compatible_max_concurrent")]
    pub max_concurrent: usize,
    /// LLM 单次回复最大 token 数（与模型上下文长度相关，可按模型调整）。
    /// deepseek 等推理模型需要预留 reasoning 空间，太小会 content 为空。
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
}

fn default_openai_compatible_url() -> String {
    "https://glmcoding.cn".into()
}
fn default_openai_compatible_model() -> String {
    "deepseek-v4-flash".into()
}
fn default_openai_compatible_protocol() -> String {
    "openai".into()
}
const fn default_openai_compatible_max_concurrent() -> usize {
    32
}

impl Default for OpenAICompatibleProviderConfig {
    fn default() -> Self {
        Self {
            url: default_openai_compatible_url(),
            api_key: String::new(),
            model_llm: default_openai_compatible_model(),
            protocol: default_openai_compatible_protocol(),
            max_concurrent: default_openai_compatible_max_concurrent(),
            max_tokens: default_llm_max_tokens(),
        }
    }
}

/// KG Router 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgRouterConfig {
    /// 启用任务感知路由。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 搜索结果智能过滤配置。
    #[serde(default)]
    pub result_filter: ResultFilterConfig,

    /// L0 提前拦截（early-exit）配置：判断查询是否值得检索，避免闲聊/寒暄
    /// 也触发搜索。对应文档「三级 Router」的 L0 Gate 层。
    #[serde(default)]
    pub early_exit: EarlyExitConfig,

    /// 可观测性配置。
    #[serde(default)]
    pub observability: ObservabilityConfig,
}

impl Default for KgRouterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            result_filter: ResultFilterConfig::default(),
            early_exit: EarlyExitConfig::default(),
            observability: ObservabilityConfig::default(),
        }
    }
}

/// L0 提前拦截（early-exit）配置。
///
/// 在 `dt router` 真正发起检索之前，先用纯规则判断查询是否值得搜索：
/// 命中项目/服务/类名/配置名等实体特征或检索意图词 → 放行；
/// 只有寒暄/算术/通用闲聊 → 直接返回「无需检索」，省掉一次 KG 搜索。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EarlyExitConfig {
    /// 启用 L0 提前拦截。默认开启——仅拦截明显无需检索的查询，
    /// 命中任一实体/意图特征即放行，不影响 `dt search` 一致性。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for EarlyExitConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// 搜索结果过滤配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultFilterConfig {
    /// 启用过滤功能。
    /// 默认关闭——保证 `dt router` 结果与 `dt search` 一致；开启后复用现有 LLM
    /// 移除与查询完全不符的结果（可通过配置文件或 `--filter true` 开启）。
    #[serde(default)]
    pub enabled: bool,

    /// 相关性阈值（0.0-1.0），低于此值的结果被移除。
    #[serde(default = "default_filter_threshold")]
    pub threshold: f32,

    /// 过滤判断时 LLM 最大 token 数。
    #[serde(default = "default_filter_max_tokens")]
    pub max_tokens: u32,

    /// 过滤判断时的温度参数（建议 0.0 确定性推理）。
    #[serde(default)]
    pub temperature: f32,
}

const fn default_filter_threshold() -> f32 {
    0.6
}

const fn default_filter_max_tokens() -> u32 {
    200
}

impl Default for ResultFilterConfig {
    fn default() -> Self {
        Self {
            enabled: false, // 默认关闭，保证与 dt search 结果一致；可配置开启
            threshold: default_filter_threshold(),
            max_tokens: default_filter_max_tokens(),
            temperature: 0.0,
        }
    }
}

/// 可观测性配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// 记录所有 LLM 调用到 Memgraph。
    #[serde(default = "default_true")]
    pub log_calls: bool,

    /// 记录过滤详情。
    #[serde(default = "default_true")]
    pub log_filter_results: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_calls: true,
            log_filter_results: true,
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

    /// 返回当前 `llm_provider` 的全局并发上限，供 ProcessorEngine（文件级
    /// LLM 分析并发）与 LLM 客户端 semaphore 共用。
    ///
    /// 缺失时按 provider 类型回退，与各 provider 结构体的 serde 默认一致：
    /// `openai_compatible`=32、`xinference`=16、siliconflow 及其他=20。
    /// 旧配置键 `glmcoding` 经 serde alias 已并入 `openai_compatible`。
    pub fn llm_provider_max_concurrent(&self) -> usize {
        let provider = self
            .providers
            .as_ref()
            .map(|p| p.llm_provider.as_str())
            .unwrap_or("siliconflow");
        let providers = self.providers.as_ref();
        match provider {
            "openai_compatible" | "glmcoding" => providers
                .and_then(|p| p.openai_compatible.as_ref())
                .map(|c| c.max_concurrent)
                .unwrap_or(32),
            "xinference" => providers
                .and_then(|p| p.xinference.as_ref())
                .map(|c| c.max_concurrent)
                .unwrap_or(16),
            _ => providers
                .and_then(|p| p.siliconflow.as_ref())
                .map(|c| c.max_concurrent)
                .unwrap_or(20),
        }
    }
}

/// 解析 `~/.config/digital-twin/pipeline.yaml` 路径,不引入 `dirs` crate。
fn home_pipeline_config() -> Option<PathBuf> {
    let home = crate::shared::home_dir()?;
    Some(home.join(".config/digital-twin/pipeline.yaml"))
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            processors: ProcessorsConfig::default(),
            llm: Some(LlmConfig::default()),
            ecosystem: Some(EcosystemConfig::default()),
            providers: Some(ProvidersConfig::default()),
            kg_router: Some(KgRouterConfig::default()),
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
        // 无 providers 时按 siliconflow 默认回退（默认 max_concurrent=20）。
        assert_eq!(cfg.llm_provider_max_concurrent(), 20);
        assert!(cfg.processors.tree_sitter);
        assert!(!cfg.processors.ocr);
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.1);
        assert_eq!(cfg.llm.as_ref().unwrap().max_tokens, 4096);
    }

    #[test]
    fn deserialize_full_config() {
        let yaml = r#"
enabled: true
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
providers:
  llm_provider: openai_compatible
  openai_compatible:
    url: "https://opencode.ai/zen/go"
    max_concurrent: 32
ecosystem:
  enabled: true
  projects:
    - digital-twin
    - svc
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.5);
        assert_eq!(cfg.llm_provider_max_concurrent(), 32);

        let eco = cfg.ecosystem.unwrap();
        assert!(eco.enabled);
        assert_eq!(eco.projects, vec!["digital-twin", "svc"]);
    }

    #[test]
    fn llm_provider_max_concurrent_reads_current_provider() {
        // openai_compatible 显式配置
        let yaml = r#"
providers:
  llm_provider: openai_compatible
  openai_compatible:
    max_concurrent: 8
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.llm_provider_max_concurrent(), 8);

        // xinference 显式配置
        let yaml = r#"
providers:
  llm_provider: xinference
  xinference:
    max_concurrent: 4
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.llm_provider_max_concurrent(), 4);

        // xinference 缺 max_concurrent 字段 → 默认 16
        let yaml = r#"
providers:
  llm_provider: xinference
  xinference:
    url: "http://localhost:9997/v1"
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.llm_provider_max_concurrent(), 16);

        // siliconflow 显式配置
        let yaml = r#"
providers:
  llm_provider: siliconflow
  siliconflow:
    max_concurrent: 10
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.llm_provider_max_concurrent(), 10);

        // 旧键 glmcoding（serde alias）兼容：键仍可反序列化进 openai_compatible
        let yaml = r#"
providers:
  llm_provider: glmcoding
  glmcoding:
    max_concurrent: 12
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.llm_provider_max_concurrent(), 12);
        assert!(cfg.providers.as_ref().unwrap().openai_compatible.is_some());
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
