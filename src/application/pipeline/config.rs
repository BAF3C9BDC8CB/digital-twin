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
    /// 各处理器的功能开关。
    #[serde(default)]
    pub processors: ProcessorsConfig,

    /// LLM 推理参数（model、temperature、max_tokens …）。
    #[serde(default)]
    pub llm: Option<LlmConfig>,

    /// Embedding 能力配置（模型选择）。
    #[serde(default)]
    pub embed: Option<EmbedConfig>,

    /// Rerank 能力配置（模型选择）。
    #[serde(default)]
    pub rerank: Option<RerankConfig>,

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

    /// 文档分级门控（doc-gate）配置：构建时用 LLM 判断文档价值
    /// （high/medium/low），决定是否做详细的实体/关系提取。
    /// high=详细提取；medium=只提实体+摘要（抑制关系）；low=跳过 LLM
    /// 提取，仅保留块级索引（doc_chunks）与 Document 节点。
    #[serde(default)]
    pub doc_gate: Option<DocGateConfig>,
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

/// 文档分级门控（doc-gate）配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocGateConfig {
    /// 总开关：false（默认）时文档一律走详细提取（现状行为）。
    #[serde(default)]
    pub enabled: bool,

    /// LLM 判定采样的文档前 N 字符（默认 4000，控制 gate 调用成本）。
    #[serde(default = "default_gate_preview_chars")]
    pub preview_chars: usize,

    /// gate 判定失败时降级的提取档位：high（保守）/ medium / low。
    #[serde(default)]
    pub fallback: DocGateLevel,
}

/// 文档价值档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocGateLevel {
    /// 详细提取：实体 + 关系 + 摘要（现状 document_with_nlp 全量）。
    High,
    /// 只提取实体 + 摘要，抑制关系三元组（图瘦身，去噪音边）。
    Medium,
    /// 跳过 LLM 提取：仅块级索引（doc_chunks 向量）+ Document 节点。
    Low,
}

impl Default for DocGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preview_chars: default_gate_preview_chars(),
            fallback: DocGateLevel::High,
        }
    }
}

impl Default for DocGateLevel {
    fn default() -> Self {
        DocGateLevel::High
    }
}

const fn default_gate_preview_chars() -> usize {
    4000
}

/// LLM 推理参数（唯一能力块：代码/文档分析与路由过滤共用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// LLM 模型名（SiliconFlow 模型 ID，如 `deepseek-ai/DeepSeek-R1-0528-Qwen3-8B`）。
    #[serde(default)]
    pub model: String,

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
            model: String::new(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
        }
    }
}

/// Embedding 能力配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// Embedding 模型名（如 `BAAI/bge-m3`）。
    #[serde(default)]
    pub model: String,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
        }
    }
}

/// Rerank 能力配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    /// Rerank 模型名（如 `BAAI/bge-reranker-v2-m3`）。
    #[serde(default)]
    pub model: String,
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
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
    /// SiliconFlow 连接配置（唯一推理 provider）。
    #[serde(default)]
    pub siliconflow: Option<SiliconFlowProviderConfig>,
}

/// SiliconFlow 连接配置（唯一推理 provider）。
///
/// 模型选择与推理参数不在此处——按能力归入顶层 `llm` / `embed` / `rerank` 块；
/// 此处只描述「连到哪、用什么凭据、多大并发」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiliconFlowProviderConfig {
    /// API base URL（OpenAI 兼容端点；缺省为 SiliconFlow 云 API）。
    #[serde(default = "default_sf_url")]
    pub url: String,
    /// API key。留空时回退到 `SILICONFLOW_API_KEY` 环境变量。
    #[serde(default)]
    pub api_key: String,
    /// SiliconFlow 云 API 最大并发请求数（embed/rerank/LLM 共享）。
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
            max_concurrent: default_sf_max_concurrent(),
        }
    }
}

/// KG Router 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgRouterConfig {
    /// 搜索结果智能过滤配置。
    #[serde(default)]
    pub result_filter: ResultFilterConfig,

    /// L0 提前拦截（early-exit）配置：判断查询是否值得检索，避免闲聊/寒暄
    /// 也触发搜索。对应文档「三级 Router」的 L0 Gate 层。
    #[serde(default)]
    pub early_exit: EarlyExitConfig,

    /// LLM 门控（llm-gate）配置：用 LLM 判断"是否值得搜索"，替代纯规则 L0。
    /// 规则 L0 拦截后，若开启本项则再用 LLM 二次确认（规则放行的直接过，
    /// 规则拦截的问 LLM，LLM 说该搜则仍放行）——不对称策略，防漏搜。
    #[serde(default)]
    pub llm_gate: LlmGateConfig,

    /// 可观测性配置。
    #[serde(default)]
    pub observability: ObservabilityConfig,
}

impl Default for KgRouterConfig {
    fn default() -> Self {
        Self {
            result_filter: ResultFilterConfig::default(),
            early_exit: EarlyExitConfig::default(),
            llm_gate: LlmGateConfig::default(),
            observability: ObservabilityConfig::default(),
        }
    }
}

/// LLM 门控配置：搜索发起前用 LLM 判断查询是否值得检索。
///
/// 设计（不对称策略）：
/// - 规则 L0（early_exit）判定「该搜」→ 直接放行，不调 LLM（省延迟）；
/// - 规则 L0 判定「不该搜」（闲聊/无锚点）→ 调 LLM 二次确认：
///   LLM 说该搜 → 放行（规则误杀兜底）；LLM 说不该搜 → 拦截返回空。
/// 这样 LLM 只处理规则拿不准的少数查询，延迟与成本可控，且不会漏搜。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmGateConfig {
    /// 启用 LLM 门控。默认关闭——只走纯规则 L0（现状）；开启后规则拦截的
    /// 查询会再问一次 LLM 确认。
    #[serde(default)]
    pub enabled: bool,

    /// 门控判断时 LLM 最大 token 数（输出为简短 JSON，几十 token 足够）。
    #[serde(default = "default_gate_max_tokens")]
    pub max_tokens: u32,

    /// 门控判断时的温度参数（0.0 确定性推理）。
    #[serde(default)]
    pub temperature: f32,
}

const fn default_gate_max_tokens() -> u32 {
    120
}

impl Default for LlmGateConfig {
    fn default() -> Self {
        Self {
            enabled: false, // 默认关：不改变现有纯规则行为
            max_tokens: default_gate_max_tokens(),
            temperature: 0.0,
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

    /// 返回 SiliconFlow 的全局并发上限，供 ProcessorEngine（文件级
    /// LLM 分析并发）与 LLM 客户端 semaphore 共用。
    ///
    /// 缺失时回退到默认值（20）。
    pub fn inference_max_concurrent(&self) -> usize {
        self.providers
            .as_ref()
            .and_then(|p| p.siliconflow.as_ref())
            .map(|c| c.max_concurrent)
            .unwrap_or(20)
    }

    /// 当前生效的 LLM 模型名（`llm.model`），空则回退默认。
    pub fn llm_model(&self) -> String {
        model_or_env(
            self.llm.as_ref(),
            crate::infrastructure::siliconflow::llm_model_from_env,
        )
    }

    /// 当前生效的 embed 模型名（`embed.model`），空则回退默认。
    pub fn embed_model(&self) -> String {
        model_or_env(
            self.embed.as_ref(),
            crate::infrastructure::siliconflow::embed_model_from_env,
        )
    }

    /// 当前生效的 rerank 模型名（`rerank.model`），空则回退默认。
    pub fn rerank_model(&self) -> String {
        model_or_env(
            self.rerank.as_ref(),
            crate::infrastructure::siliconflow::reranker_model_from_env,
        )
    }
}

/// 从配置中提取模型名，空则回退环境变量默认值。
///
/// 统一处理 llm/embed/rerank 三种配置的模型名提取逻辑：
/// 1. 配置存在且 `model` 非空 → 返回配置值
/// 2. 配置缺失或 `model` 为空 → 调用 `env_fallback()` 获取默认值
fn model_or_env<C>(config: Option<&C>, env_fallback: fn() -> String) -> String
where
    C: HasModel,
{
    config
        .and_then(|c| {
            let m = c.model();
            if m.is_empty() {
                None
            } else {
                Some(m.to_string())
            }
        })
        .unwrap_or_else(env_fallback)
}

/// 提供 `.model()` 访问器的配置类型 trait。
trait HasModel {
    fn model(&self) -> &str;
}

impl HasModel for LlmConfig {
    fn model(&self) -> &str {
        &self.model
    }
}

impl HasModel for EmbedConfig {
    fn model(&self) -> &str {
        &self.model
    }
}

impl HasModel for RerankConfig {
    fn model(&self) -> &str {
        &self.model
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
            processors: ProcessorsConfig::default(),
            llm: Some(LlmConfig::default()),
            embed: Some(EmbedConfig::default()),
            rerank: Some(RerankConfig::default()),
            ecosystem: Some(EcosystemConfig::default()),
            providers: Some(ProvidersConfig::default()),
            kg_router: Some(KgRouterConfig::default()),
            doc_gate: Some(DocGateConfig::default()),
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
        // 无 providers 时按 siliconflow 默认回退（默认 max_concurrent=20）。
        assert_eq!(cfg.inference_max_concurrent(), 20);
        assert!(cfg.processors.tree_sitter);
        assert!(!cfg.processors.ocr);
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.1);
        assert_eq!(cfg.llm.as_ref().unwrap().max_tokens, 4096);
    }

    #[test]
    fn deserialize_full_config() {
        let yaml = r#"
processors:
  tree_sitter: true
  llm: true
  chunk: true
  extract_text: true
  ocr: false
  store: true
llm:
  model: "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B"
  temperature: 0.5
  max_tokens: 2048
embed:
  model: "BAAI/bge-m3"
rerank:
  model: "BAAI/bge-reranker-v2-m3"
providers:
  siliconflow:
    url: "https://api.siliconflow.cn/v1"
    max_concurrent: 32
ecosystem:
  enabled: true
  projects:
    - digital-twin
    - svc
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        let llm = cfg.llm.as_ref().unwrap();
        assert_eq!(llm.model, "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B");
        assert_eq!(llm.temperature, 0.5);
        assert_eq!(cfg.llm_model(), "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B");
        assert_eq!(cfg.embed_model(), "BAAI/bge-m3");
        assert_eq!(cfg.rerank_model(), "BAAI/bge-reranker-v2-m3");
        assert_eq!(cfg.inference_max_concurrent(), 32);

        let eco = cfg.ecosystem.unwrap();
        assert!(eco.enabled);
        assert_eq!(eco.projects, vec!["digital-twin", "svc"]);
    }

    #[test]
    fn inference_max_concurrent_reads_current_provider() {
        // siliconflow 显式配置
        let yaml = r#"
providers:
  siliconflow:
    max_concurrent: 10
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.inference_max_concurrent(), 10);

        // 未配置 providers → 回退 20
        let cfg = PipelineConfig::default();
        assert_eq!(cfg.inference_max_concurrent(), 20);
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        // 该测试从工作区根目录运行——这里不对 cwd 做任何假设，
        // 仅验证行为。
        let cfg = PipelineConfig::load().unwrap_or_default();
        // 配置文件可能存在也可能不存在，但我们总应得到一个有效的
        // 对象且不会 panic。
        let _ = cfg.processors;
    }
}
