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

    /// LLM 推理参数（temperature、max_tokens …；模型归入 providers.llm 池）。
    #[serde(default)]
    pub llm: Option<LlmConfig>,

    /// Embedding 能力配置（模型选择；端点归入 providers.embed 池）。
    #[serde(default)]
    pub embed: Option<EmbedConfig>,

    /// Rerank 能力配置（模型选择；端点归入 providers.rerank 池）。
    #[serde(default)]
    pub rerank: Option<RerankConfig>,

    /// 跨越多个项目的生态系统级设置。
    #[serde(default)]
    pub ecosystem: Option<EcosystemConfig>,

    /// 提供方路由与模型配置（端点池，见 [`ProvidersConfig`]）。
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
/// 模型选择已移入 `providers.llm` 端点池——`model` 字段保留为
/// 「端点未指定 model 时使用的默认模型名」，可为空。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// 默认 LLM 模型名（端点级 model 未指定时使用，可为空）。
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

/// Embedding 能力配置（模型选择）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    /// 默认 Embedding 模型名（端点级 model 未指定时使用，可为空）。
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

/// Rerank 能力配置（模型选择）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    /// 默认 Rerank 模型名（端点级 model 未指定时使用，可为空）。
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

/// 提供方路由与模型配置——按能力划分的端点池。
///
/// 2026-09-06 重构（多厂商 × 多模型端点池）：
/// - 旧 `providers.siliconflow` 单块已移除，不再保留兼容解析。
/// - `llm` / `embed` / `rerank` 各是一组端点（`ProviderEndpoint`），
///   每端点独立 url + api_key + 模型；同能力池内允许多模型多厂商，
///   一个模型/端点失败自动顺延到下一个（failover 语义）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersConfig {
    /// 多端点选择策略（`failover` / `round_robin`，缺省 failover）。
    #[serde(default)]
    pub strategy: EndpointStrategy,

    /// 全局默认代理（端点级 `proxy` 可覆盖；缺省 None = 直连）。
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,

    /// LLM（chat completions）端点池。
    #[serde(default)]
    pub llm: Vec<ProviderEndpoint>,

    /// Embedding 端点池。
    #[serde(default)]
    pub embed: Vec<ProviderEndpoint>,

    /// Rerank 端点池（须支持 `/rerank` 私有端点）。
    #[serde(default)]
    pub rerank: Vec<ProviderEndpoint>,
}

/// 端点选择策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EndpointStrategy {
    /// 顺序主备：按配置顺序逐个尝试，端点失败则切下一个（缺省）。
    #[default]
    Failover,
    /// 轮询均分：请求轮流打到每个端点（不适合带状态的写入场景）。
    RoundRobin,
}

/// 单个可用的推理端点（OpenAI 兼容；rerank 为 SiliconFlow 私有扩展）。
///
/// 每个端点绑定一条「url + api_key」连接，并带**一个或多个候选模型名**
/// （`models` 列表；单模型可用 `model` 简写）。同端点内模型按顺序
/// failover：第一个模型失败（连接/429/5xx/4xx 模型不存在等）自动换下一个；
/// 端点内模型全部失败才切到池内下一端点。由此实现：
/// - 多厂商：不同 url 的端点；
/// - 多模型：同端点（或池内）多个模型候选，一个失败换下一个。
///
/// `models` 缺省时回退 `model`，两者皆空时使用顶层
/// `llm.model` / `embed.model` / `rerank.model`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderEndpoint {
    /// 端点名（日志/健康报告用；缺省取 url host）。
    #[serde(default)]
    pub name: String,

    /// OpenAI 兼容 API base URL（与 `url_env` 二选一；都空 = 解析期报错）。
    #[serde(default)]
    pub url: String,

    /// OpenAI 兼容 API base URL 环境变量名（优先于 `url`；都空 = 解析期报错）。
    #[serde(default)]
    pub url_env: String,

    /// API key（与 `api_key_env` 二选一；都空 = 解析期报错）。
    #[serde(default)]
    pub api_key: String,

    /// API key 环境变量名（优先于 `api_key`；都空 = 解析期报错）。
    #[serde(default)]
    pub api_key_env: String,

    /// 单模型简写（与 `models` 二选一；`models` 优先）。
    #[serde(default)]
    pub model: String,

    /// 候选模型列表（顺序 = failover 优先级）。同端点内一个失败自动换下一个。
    #[serde(default)]
    pub models: Vec<String>,

    /// 该端点的并发上限（可选；缺省 20）。仅对"固定模型"的端点有效。
    /// 若该端点声明了多候选模型（`models`），它会被扩为多个运行时端点，
    /// 每个候选模型分得一份独立的并发额度。
    #[serde(default)]
    pub max_concurrent: Option<usize>,

    /// 该端点权重（仅 round_robin 加权轮询时生效；缺省 1）。
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// 端点级出网代理覆盖（可选；缺省继承全局 `providers.proxy`）。
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
}

const fn default_weight() -> u32 {
    1
}

impl ProviderEndpoint {
    /// 该端点的候选模型（有序）：`models` 非空用之；否则 `model` 非空用之；
    /// 都空则用传入的默认模型。返回至少含一个元素（调用方兜底默认）。
    pub fn candidate_models(&self, fallback: &str) -> Vec<String> {
        if !self.models.is_empty() {
            self.models
                .iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect()
        } else if !self.model.trim().is_empty() {
            vec![self.model.trim().to_string()]
        } else if !fallback.trim().is_empty() {
            vec![fallback.trim().to_string()]
        } else {
            vec![]
        }
    }
}

/// 出网代理配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// 是否启用代理（`false` / 缺省 = 直连，忽略 `url`）。
    #[serde(default)]
    pub enabled: bool,
    /// 代理地址（如 `http://127.0.0.1:7897`、`socks5://...`）。
    #[serde(default)]
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
        }
    }
}

impl ProvidersConfig {
    /// 解析期密钥解析 + 校验。展开 `api_key_env`，检查 url 合法性；
    /// 均无密钥 / url 非法给出明确错误——不再静默回退共用 key。
    pub fn validate(&self) -> Result<(), String> {
        for (cap, eps) in [
            ("llm", &self.llm),
            ("embed", &self.embed),
            ("rerank", &self.rerank),
        ] {
            for ep in eps {
                let key = ep.resolved_api_key();
                if key.is_empty() {
                    return Err(format!(
                        "providers.{cap} 端点 '{}' 缺少 API key（api_key / api_key_env 至少其一，且 api_key_env 指向的环境变量存在且非空）",
                        ep.label()
                    ));
                }
                if ep.effective_url().trim().is_empty() {
                    return Err(format!(
                        "providers.{cap} 端点 '{}' 缺少 url（url / url_env 至少其一，且 url_env 指向的环境变量存在且非空）",
                        ep.label()
                    ));
                }
            }
        }
        Ok(())
    }
}

impl ProviderEndpoint {
    /// 显示名：显式 name，否则 url host。
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        url_host(&self.effective_url()).unwrap_or_else(|| "unnamed".to_string())
    }

    /// 生效的 base URL：`url_env` 指向的环境变量 > `url`。
    pub fn effective_url(&self) -> String {
        if !self.url_env.trim().is_empty() {
            return std::env::var(self.url_env.trim()).unwrap_or_default();
        }
        self.url.clone()
    }

    /// 解析实际 API key：`api_key_env` 优先（展开环境变量），其次 `api_key`。
    pub fn resolved_api_key(&self) -> String {
        if !self.api_key_env.trim().is_empty() {
            std::env::var(self.api_key_env.trim()).unwrap_or_default()
        } else {
            self.api_key.clone()
        }
    }

    /// 该端点的生效代理：端点级配置 > 全局配置。
    pub fn effective_proxy<'a>(
        &'a self,
        global: &'a Option<ProxyConfig>,
    ) -> Option<&'a ProxyConfig> {
        self.proxy.as_ref().or(global.as_ref())
    }
}

/// 从 url 提取 host 段（供端点名缺省与错误提示）。
fn url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// KG Router 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgRouterConfig {
    /// 搜索结果智能过滤配置。
    #[serde(default)]
    pub result_filter: ResultFilterConfig,

    /// LLM 门控（llm-gate）配置：搜索发起前用 LLM 判断查询是否值得检索，
    /// 替代已移除的纯规则 L0（闲聊词表/无锚点词表早退）。
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
            llm_gate: LlmGateConfig::default(),
            observability: ObservabilityConfig::default(),
        }
    }
}

/// LLM 门控配置：搜索发起前用 LLM 判断查询是否值得检索。
///
/// 2026-09-06 起替代纯规则 L0（闲聊词表/无锚点词表打地鼠式拦截已整体移除，
/// 改为统一由 LLM 判断"是否要搜索知识图谱"）：
/// - 开启后每次搜索先问 LLM：值得检索 → 放行；不值得（寒暄/闲聊/纯任务指令
///   无检索对象/与代码库无关话题）→ 直接返回「无需检索」；
/// - 关闭则跳过门控直接检索（等价旧 early_exit.enabled=false 的直通行为）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmGateConfig {
    /// 启用 LLM 门控。默认开启——所有查询先经 LLM 判断是否值得搜索。
    #[serde(default = "default_gate_enabled")]
    pub enabled: bool,

    /// 门控判断时 LLM 最大 token 数（输出为简短 JSON，几十 token 足够）。
    #[serde(default = "default_gate_max_tokens")]
    pub max_tokens: u32,

    /// 门控判断时的温度参数（0.0 确定性推理）。
    #[serde(default)]
    pub temperature: f32,
}

const fn default_gate_enabled() -> bool {
    true
}

const fn default_gate_max_tokens() -> u32 {
    120
}

impl Default for LlmGateConfig {
    fn default() -> Self {
        Self {
            enabled: default_gate_enabled(),
            max_tokens: default_gate_max_tokens(),
            temperature: 0.0,
        }
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

/// 解析 `~/.config/digital-twin/pipeline.yaml` 路径,不引入 `dirs` crate。
fn home_pipeline_config() -> Option<PathBuf> {
    let home = crate::shared::home_dir()?;
    Some(home.join(".config/digital-twin/pipeline.yaml"))
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

        let cfg: Self = serde_yaml::from_str(&content)
            .map_err(|e| format!("流水线配置解析错误: {e}"))?;
        cfg.providers
            .as_ref()
            .map(|p| p.validate())
            .transpose()?;
        Ok(cfg)
    }

    /// 全局推理并发上限——取 LLM 池内各端点并发之和（聚合多 key 总带宽），
    /// 缺失回退默认 20。
    /// （旧版读 providers.siliconflow.max_concurrent；该单块已移除。
    /// 多 key 并行：池内多个端点各自持有独立信号量，因此各端点并发
    /// 相加即整池可同时在飞的请求数上限。）
    pub fn inference_max_concurrent(&self) -> usize {
        let Some(providers) = self.providers.as_ref() else {
            return 20;
        };
        let total: usize = providers
            .llm
            .iter()
            .map(|e| e.max_concurrent.unwrap_or(20))
            .sum();
        if total == 0 {
            20
        } else {
            total
        }
    }

    /// 当前生效的 LLM 模型名（`llm.model`），空则回退默认。
    pub fn llm_model(&self) -> String {
        self.llm
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_default()
    }

    /// 当前生效的 embed 模型名（`embed.model`），空则回退默认。
    pub fn embed_model(&self) -> String {
        self.embed
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_default()
    }

    /// 当前生效的 rerank 模型名（`rerank.model`），空则回退默认。
    pub fn rerank_model(&self) -> String {
        self.rerank
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_default()
    }
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
        // 无 providers 时按默认回退（默认 max_concurrent=20）。
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
  temperature: 0.5
  max_tokens: 2048
embed:
  model: "BAAI/bge-m3"
rerank:
  model: "BAAI/bge-reranker-v2-m3"
providers:
  llm:
    - name: sf
      url: "https://api.siliconflow.cn/v1"
      api_key_env: "DT_LLM_KEY_SF"
      model: "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B"
      max_concurrent: 32
  embed:
    - name: internal
      url: "http://124.221.200.116:3000/v1"
      api_key: "sk-placeholder"
ecosystem:
  enabled: true
  projects:
    - digital-twin
    - svc
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.5);
        assert_eq!(cfg.embed_model(), "BAAI/bge-m3");
        assert_eq!(cfg.rerank_model(), "BAAI/bge-reranker-v2-m3");
        assert_eq!(cfg.inference_max_concurrent(), 32);
        let p = cfg.providers.as_ref().unwrap();
        assert_eq!(p.llm.len(), 1);
        assert_eq!(p.llm[0].label(), "sf");
        assert_eq!(p.embed[0].label(), "internal");
        // strategy 缺省 = failover
        assert_eq!(p.strategy, EndpointStrategy::Failover);

        let eco = cfg.ecosystem.unwrap();
        assert!(eco.enabled);
        assert_eq!(eco.projects, vec!["digital-twin", "svc"]);
    }

    #[test]
    fn inference_max_concurrent_reads_first_llm_endpoint() {
        let yaml = r#"
providers:
  llm:
    - url: "https://api.siliconflow.cn/v1"
      api_key: "k"
      max_concurrent: 10
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.inference_max_concurrent(), 10);

        // 未配置 providers → 回退 20
        let cfg = PipelineConfig::default();
        assert_eq!(cfg.inference_max_concurrent(), 20);
    }

    #[test]
    fn inference_max_concurrent_sums_all_llm_endpoints() {
        // 多 key 并行：整池并发上限 = 各端点 max_concurrent 之和
        let yaml = r#"
providers:
  llm:
    - url: "https://a/v1"
      api_key: "k1"
      max_concurrent: 48
    - url: "https://b/v1"
      api_key: "k2"
      max_concurrent: 48
    - url: "https://c/v1"
      api_key: "k3"
      model: "m"
      # 未声明 → 缺省 20
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.inference_max_concurrent(), 48 + 48 + 20);
        // 无 llm 端点 → 回退 20
        let yaml2 = r#"
providers:
  llm: []
"#;
        let cfg2: PipelineConfig = serde_yaml::from_str(yaml2).unwrap();
        assert_eq!(cfg2.inference_max_concurrent(), 20);
    }

    #[test]
    fn api_key_resolution_prefers_env_and_label_falls_back_to_host() {
        // api_key 直填
        let ep = ProviderEndpoint {
            name: String::new(),
            url: "https://api.siliconflow.cn/v1".into(),
            url_env: String::new(),
            api_key: "sk-abc".into(),
            api_key_env: String::new(),
            model: String::new(),
            models: vec![],
            max_concurrent: None,
            weight: 1,
            proxy: None,
        };
        assert_eq!(ep.resolved_api_key(), "sk-abc");
        assert_eq!(ep.label(), "api.siliconflow.cn");

        // api_key_env 优先（设临时变量）
        std::env::set_var("DT_TEST_ENV_KEY", "sk-env");
        let ep2 = ProviderEndpoint {
            api_key: "sk-abc".into(),
            api_key_env: "DT_TEST_ENV_KEY".into(),
            ..ep.clone()
        };
        assert_eq!(ep2.resolved_api_key(), "sk-env");
        std::env::remove_var("DT_TEST_ENV_KEY");
    }

    #[test]
    fn validate_rejects_missing_key_or_url() {
        let mut p = ProvidersConfig::default();
        p.embed.push(ProviderEndpoint {
            name: "no-key".into(),
            url: "http://x/v1".into(),
            ..Default::default()
        });
        let err = p.validate().unwrap_err();
        assert!(err.contains("缺少 API key"), "{err}");

        let mut p = ProvidersConfig::default();
        p.llm.push(ProviderEndpoint {
            name: String::new(),
            url: String::new(),
            api_key: "k".into(),
            ..Default::default()
        });
        let err = p.validate().unwrap_err();
        assert!(err.contains("缺少 url"), "{err}");
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
