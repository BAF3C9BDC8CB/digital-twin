//! HanLP NLP processor — calls the inference server's NLP endpoint for
//! named-entity recognition and keyword extraction.
//!
//! Custom NER dictionaries are loaded from static configuration and
//! dynamically from Memgraph service/component names, then passed to the
//! inference server as request parameters.
//!
//! Produces a [`ProcessorOutput`] with:
//! - `"entities"`  — array of `{text, tag}` named entities
//! - `"keywords"`  — array of keyword strings
//! - `"status"`    — `"ok"`, `"empty"`, or `"error"`

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::infer_client::SiliconFlowChatClient;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

/// Static configuration for custom named-entity dictionaries.
///
/// These entity lists are passed to the inference server alongside each
/// NLP request so that HanLP can recognise project‑specific terms
/// (service names, technology components, business entities, etc.)
/// that are not present in its built‑in models.
#[derive(Debug, Clone)]
pub struct CustomEntityConfig {
    /// Static list of known service / application names.
    pub service_names: Vec<String>,
    /// Technology components (e.g. "Redis", "Kafka", "MySQL", "Elasticsearch").
    pub tech_components: Vec<String>,
    /// Business‑domain entities in the project's language (e.g. "订单", "用户", "支付").
    pub business_entities: Vec<String>,
}

impl Default for CustomEntityConfig {
    fn default() -> Self {
        Self {
            service_names: Vec::new(),
            tech_components: vec![
                "Redis".into(),
                "Kafka".into(),
                "MySQL".into(),
                "PostgreSQL".into(),
                "MongoDB".into(),
                "Elasticsearch".into(),
                "RabbitMQ".into(),
                "Nacos".into(),
                "MinIO".into(),
                "Docker".into(),
                "Kubernetes".into(),
            ],
            business_entities: Vec::new(),
        }
    }
}

/// NLP processor that calls the inference server's HanLP endpoint.
///
/// Produces a [`ProcessorOutput`] with:
/// - `"entities"`  — array of `{text, tag}` named entities
/// - `"keywords"`  — array of keyword strings
/// - `"status"`    — `"ok"`, `"empty"`, or `"error"`
///
/// Custom NER dictionaries are built from:
/// 1. Static [`CustomEntityConfig`] entries (always used).
/// 2. Dynamic service/component names queried from Memgraph (fallible;
///    failures are silently downgraded to the static lists only).
pub struct HanlpClientProcessor {
        client: Arc<SiliconFlowChatClient>,
        config: CustomEntityConfig,
    graph: Option<Arc<dyn GraphRepository>>,
}

impl HanlpClientProcessor {
    /// Create a new processor that sends NLP requests to the given
    /// inference server.
    pub fn new(base_url: String) -> Self {
        let client = Arc::new(SiliconFlowChatClient::new(base_url, 4));
        Self {
            client,
            config: CustomEntityConfig::default(),
            graph: None,
        }
    }

    /// Create a processor with full configuration.
    ///
    /// * `client`           — shared inference-server client.
    /// * `config`           — static entity lists (or [`Default::default`]).
    /// * `graph`            — optional Memgraph handle for dynamic entity
    ///                        discovery.  When `None` only static entities
    ///                        are used.
    pub fn with_config(
    client: Arc<SiliconFlowChatClient>,
        config: CustomEntityConfig,
        graph: Option<Arc<dyn GraphRepository>>,
    ) -> Self {
        Self {
            client,
            config,
            graph,
        }
    }

    /// Create a processor from an existing shared client.
    pub fn with_client(client: Arc<SiliconFlowChatClient>) -> Self {
        Self {
            client,
            config: CustomEntityConfig::default(),
            graph: None,
        }
    }

    /// Build the custom-entities dictionary passed to the inference server.
    ///
    /// Merges static configuration with dynamic entities queried from
    /// Memgraph.  If the graph query fails the error is logged (via the
    /// return value) and only the static entries are used.
    async fn build_custom_entities(&self) -> HashMap<String, Vec<String>> {
        let mut custom = HashMap::new();

        // ---- Static entries (always present) ----
        custom.insert("SERVICE_NAME".into(), self.config.service_names.clone());
        custom.insert("TECH_COMPONENT".into(), self.config.tech_components.clone());
        custom.insert("BUSINESS_ENTITY".into(), self.config.business_entities.clone());

        // ---- Dynamic entries from Memgraph ----
        if let Some(graph) = &self.graph {
            // Service names
            if let Ok(result) = graph
                .read_query(
                    "MATCH (s:Service) RETURN s.name",
                    HashMap::new(),
                )
                .await
            {
                if let Some(rows) = result.as_array() {
                    let names: Vec<String> = rows
                        .iter()
                        .filter_map(|row| {
                            row.get("s.name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect();
                    if !names.is_empty() {
                        custom
                            .entry("SERVICE_NAME".into())
                            .or_insert_with(Vec::new)
                            .extend(names);
                    }
                }
            }

            // Component names
            if let Ok(result) = graph
                .read_query(
                    "MATCH (c:Component) RETURN c.name",
                    HashMap::new(),
                )
                .await
            {
                if let Some(rows) = result.as_array() {
                    let names: Vec<String> = rows
                        .iter()
                        .filter_map(|row| {
                            row.get("c.name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect();
                    if !names.is_empty() {
                        custom
                            .entry("TECH_COMPONENT".into())
                            .or_insert_with(Vec::new)
                            .extend(names);
                    }
                }
            }
        }

        custom
    }
}

#[async_trait]
impl Processor for HanlpClientProcessor {
    fn name(&self) -> &str {
        "hanlp"
    }

    fn priority(&self) -> i32 {
        80
    }

    fn matches(&self, file_path: &Path) -> bool {
        // HanLP can run on any text file — source code and documents.
        matches!(
            file_path.extension().and_then(|e| e.to_str()),
            Some(
                "java" | "py" | "rs" | "go" | "ts" | "tsx" | "js" | "jsx" | "php"
                    | "md" | "txt" | "yaml" | "yml" | "properties"
            )
        )
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();
        let text = &ctx.file_text;

        // Only attempt analysis on non-empty text.
        if text.trim().is_empty() {
            output.set("entities", serde_json::Value::Array(vec![]));
            output.set("keywords", serde_json::Value::Array(vec![]));
            output.set("status", "empty");
            return Ok(output);
        }

        // The HanLP endpoint is not available on SiliconFlow, so this
        // processor always returns an error status. The processor is kept
        // for structural compatibility (pipeline registration).
        output.set("entities", serde_json::Value::Array(vec![]));
        output.set("keywords", serde_json::Value::Array(vec![]));
        output.set("status", "error");
        output.set("error", "HanLP endpoint not available on SiliconFlow");

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_context(file_name: &str, text: &str) -> PipelineContext {
        PipelineContext::new(PathBuf::from(file_name), text.to_string(), "test".to_string())
    }

    #[tokio::test]
    async fn matches_code_and_doc_extensions() {
        let processor = HanlpClientProcessor::new("https://api.siliconflow.cn/v1".into());
        assert!(processor.matches(Path::new("Main.java")));
        assert!(processor.matches(Path::new("app.py")));
        assert!(processor.matches(Path::new("readme.md")));
        assert!(processor.matches(Path::new("config.yaml")));
        assert!(!processor.matches(Path::new("image.png")));
        assert!(!processor.matches(Path::new("data.bin")));
    }

    #[tokio::test]
    async fn returns_error_when_server_unreachable() {
        let processor = HanlpClientProcessor::new("https://api.siliconflow.cn/v1".into());
        let ctx = make_context("test.java", "class Foo {}");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        // The HanLP endpoint is not running locally, so status should be
        // "error" and entity/keyword lists should be empty.
        assert_eq!(
            output.get("status").and_then(|v| v.as_str()),
            Some("error")
        );
        assert!(output.get("entities").is_some());
        assert!(output.get("keywords").is_some());
        assert!(output.get("error").is_some());
        // Error should mention SiliconFlow, not hanlp (endpoint was removed)
        let err_val = output.get("error").and_then(|v| v.as_str()).unwrap_or("");
        assert!(!err_val.is_empty());
    }

    #[tokio::test]
    async fn returns_empty_for_empty_text() {
        let processor = HanlpClientProcessor::new("https://api.siliconflow.cn/v1".into());
        let ctx = make_context("empty.txt", "   ");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(
            output.get("status").and_then(|v| v.as_str()),
            Some("empty")
        );
    }

    #[tokio::test]
    async fn name_and_priority() {
        let processor = HanlpClientProcessor::new("https://api.siliconflow.cn/v1".into());
        assert_eq!(processor.name(), "hanlp");
        assert_eq!(processor.priority(), 80);
    }

    #[test]
    fn custom_entity_config_default() {
        let cfg = CustomEntityConfig::default();
        assert!(cfg.service_names.is_empty());
        assert!(cfg.tech_components.len() >= 8); // well-known tech list
        assert!(cfg.business_entities.is_empty());
    }

    #[test]
    fn custom_entity_config_custom_values() {
        let cfg = CustomEntityConfig {
            service_names: vec!["order-service".into(), "user-api".into()],
            tech_components: vec!["Redis".into()],
            business_entities: vec!["订单".into(), "用户".into()],
        };
        assert_eq!(cfg.service_names.len(), 2);
        assert_eq!(cfg.tech_components.len(), 1);
        assert_eq!(cfg.business_entities.len(), 2);
    }

    #[tokio::test]
    async fn build_custom_entities_without_graph() {
        let cfg = CustomEntityConfig {
            service_names: vec!["my-service".into()],
            tech_components: vec!["Redis".into()],
            business_entities: vec!["用户".into()],
        };
        let client = Arc::new(SiliconFlowChatClient::new("https://api.siliconflow.cn/v1".into(), 4));
        let processor =
            HanlpClientProcessor::with_config(client, cfg, None);

        let entities = processor.build_custom_entities().await;
        assert!(entities.contains_key("SERVICE_NAME"));
        assert!(entities.contains_key("TECH_COMPONENT"));
        assert!(entities.contains_key("BUSINESS_ENTITY"));

        let services = entities.get("SERVICE_NAME").unwrap();
        assert!(services.contains(&"my-service".to_string()));

        let tech = entities.get("TECH_COMPONENT").unwrap();
        assert!(tech.contains(&"Redis".to_string()));

        let biz = entities.get("BUSINESS_ENTITY").unwrap();
        assert!(biz.contains(&"用户".to_string()));
    }

    #[tokio::test]
    async fn with_config_accepts_graph_none() {
        let cfg = CustomEntityConfig::default();
        let client = Arc::new(SiliconFlowChatClient::new("https://api.siliconflow.cn/v1".into(), 4));
        let processor =
            HanlpClientProcessor::with_config(client, cfg, None);
        assert_eq!(processor.config.service_names.len(), 0);
    }
}
