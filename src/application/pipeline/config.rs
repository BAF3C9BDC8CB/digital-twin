//! Pipeline configuration types.
//!
//! The top-level [`PipelineConfig`] is loaded from `config/pipeline.yaml`
//! and controls which processors are active, how they connect to the
//! inference server, and what LLM presets to use.

use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Complete pipeline configuration.
///
/// Deserialised from `config/pipeline.yaml` (or a custom path).  Every
/// field has a sensible default so the file is entirely optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Master enable / disable switch for the entire pipeline.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Connection settings for the `dt-inference-server` (Python).
    #[serde(default)]
    pub inference_server: InferenceServerConfig,

    /// Per-processor feature flags.
    #[serde(default)]
    pub processors: ProcessorsConfig,

    /// LLM inference parameters (temperature, max tokens, …).
    #[serde(default)]
    pub llm: Option<LlmConfig>,

    /// Ecosystem-level settings that span multiple projects.
    #[serde(default)]
    pub ecosystem: Option<EcosystemConfig>,

    /// Provider routing and model configuration.
    /// Controls which provider handles embed / rerank / LLM capabilities
    /// and which model to use for each.
    #[serde(default)]
    pub providers: Option<ProvidersConfig>,
}

const fn default_enabled() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Sub-configs
// ---------------------------------------------------------------------------

/// Inference server connection parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceServerConfig {
    /// HTTP(S) URL of the inference server.
    ///
    /// Default: `"https://api.siliconflow.cn/v1"`.
    #[serde(default = "default_infer_url")]
    pub url: String,

    /// Optional gRPC URL.
    ///
    /// The inference server also exposes a gRPC endpoint (default port
    /// 50051) that may be used in the future for streaming.
    #[serde(default)]
    pub grpc_url: Option<String>,

    /// Maximum number of concurrent HTTP requests to the inference server.
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

/// Feature flags that enable / disable individual pipeline processors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorsConfig {
    /// Tree‑sitter AST parser.
    #[serde(default = "default_true")]
    pub tree_sitter: bool,

    /// HanLP NLP analysis (Chinese NLP).
    #[serde(default = "default_true")]
    pub hanlp: bool,

    /// LLM‑based analysis (chat completions).
    #[serde(default = "default_true")]
    pub llm: bool,

    /// Text chunking / splitting.
    #[serde(default = "default_true")]
    pub chunk: bool,

    /// Raw text extraction (plain text, markdown, etc.).
    #[serde(default = "default_true")]
    pub extract_text: bool,

    /// OCR processing (off by default — requires additional dependencies).
    #[serde(default)]
    pub ocr: bool,

    /// Store results to graph + vector databases.
    #[serde(default = "default_true")]
    pub store: bool,

    /// Skip embedding (向量生成). Set to false when you already have vectors
    /// in Qdrant and want to avoid re-embedding (e.g., no bge-m3 model running).
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
            hanlp: true,
            llm: true,
            chunk: true,
            extract_text: true,
            ocr: false,
            store: true,
            embed: true,
        }
    }
}

/// LLM inference parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Sampling temperature (0.0 = deterministic, 1.0 = creative).
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// Maximum number of tokens the model may generate in one response.
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

/// Ecosystem-level settings.
///
/// When the ecosystem is enabled the pipeline processes every registered
/// project in a single batch, which is useful for global re‑indexing or
/// cross‑project dependency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcosystemConfig {
    /// Enable ecosystem (multi‑project) mode.
    #[serde(default)]
    pub enabled: bool,

    /// An explicit list of project names to include.
    ///
    /// When empty the pipeline falls back to *all* registered projects.
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

/// Provider routing and model configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    /// Which provider handles embedding ("siliconflow" or "xinference").
    #[serde(default = "default_embed_provider")]
    pub embed_provider: String,

    /// Which provider handles reranking ("siliconflow" or "xinference").
    #[serde(default = "default_rerank_provider")]
    pub rerank_provider: String,

    /// Which provider handles LLM chat ("siliconflow" or "xinference").
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,

    /// SiliconFlow provider configuration.
    #[serde(default)]
    pub siliconflow: Option<SiliconFlowProviderConfig>,

    /// XInference provider configuration.
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

/// SiliconFlow provider configuration.
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
}

fn default_sf_url() -> String {
    "https://api.siliconflow.cn/v1".into()
}

impl Default for SiliconFlowProviderConfig {
    fn default() -> Self {
        Self {
            url: default_sf_url(),
            api_key: String::new(),
            model_embed: String::new(),
            model_reranker: String::new(),
            model_llm: String::new(),
        }
    }
}

/// XInference provider configuration.
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
// Loading
// ---------------------------------------------------------------------------

impl PipelineConfig {
    /// Load [`PipelineConfig`] from `config/pipeline.yaml` relative to the
    /// current working directory.
    ///
    /// If the file does not exist, a default configuration is returned
    /// silently.  If the file exists but is malformed, an error string is
    /// returned.
    pub fn load() -> Result<Self, String> {
        let path = Path::new("config/pipeline.yaml");
        if !path.exists() {
            return Ok(Self::default());
        }

        let content =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {path:?}: {e}"))?;

        serde_yaml::from_str(&content).map_err(|e| format!("pipeline config parse error: {e}"))
    }
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
// Tests
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
  hanlp: false
  llm: true
  chunk: true
  extract_text: true
  ocr: false
  store: true
llm:
  temperature: 0.5
  max_tokens: 2048
ecosystem:
  enabled: true
  projects:
    - digital-twin
    - svc
"#;
        let cfg: PipelineConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.inference_server.url, "http://infer:50052");
        assert!(!cfg.processors.hanlp);
        assert_eq!(cfg.llm.as_ref().unwrap().temperature, 0.5);

        let eco = cfg.ecosystem.unwrap();
        assert!(eco.enabled);
        assert_eq!(eco.projects, vec!["digital-twin", "svc"]);
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        // This test runs from the workspace root — we make no assumptions
        // about the cwd here, just verify the behaviour.
        let cfg = PipelineConfig::load().unwrap_or_default();
        // The config may or may not exist, but we should always get a valid
        // object back without panicking.
        let _ = cfg.enabled;
    }
}
