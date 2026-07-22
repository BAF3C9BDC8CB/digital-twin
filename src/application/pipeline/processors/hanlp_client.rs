//! HanLP NLP processor — calls the inference server's NLP endpoint for
//! named-entity recognition and keyword extraction.
//!
//! **Current status**: placeholder — the HanLP endpoint is not yet
//! implemented on `dt-inference-server`.  This processor always produces
//! an empty output.
//!
//! Produces a [`ProcessorOutput`] with:
//! - `"entities"`  — array of `{text, tag}` named entities (empty for now)
//! - `"keywords"`  — array of keyword strings (empty for now)
//! - `"status"`    — `"unavailable"` string

use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::infer_client::InferClient;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;

/// NLP processor that calls the inference server's HanLP endpoint.
///
/// Currently a placeholder — the underlying [`InferClient::hanlp_analyze`]
/// returns an error because the endpoint is not yet implemented.  The
/// processor catches that error gracefully and emits an empty result so
/// downstream stages can still proceed.
pub struct HanlpClientProcessor {
    client: Arc<InferClient>,
}

impl HanlpClientProcessor {
    /// Create a new processor that sends NLP requests to the given
    /// inference server.
    pub fn new(base_url: String) -> Self {
        let client = Arc::new(InferClient::new(base_url, 4));
        Self { client }
    }

    /// Create a processor from an existing shared client.
    pub fn with_client(client: Arc<InferClient>) -> Self {
        Self { client }
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

        // Call the inference server.  The endpoint currently returns an
        // error — we catch it and emit an empty result so the pipeline
        // is not blocked.
        match self
            .client
            .hanlp_analyze(text, &["ner".to_string(), "keyword".to_string()])
            .await
        {
            Ok(nlp_response) => {
                let entities: Vec<serde_json::Value> = nlp_response
                    .entities
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "text": e.text,
                            "tag": e.tag,
                        })
                    })
                    .collect();
                output.set("entities", entities);
                output.set("keywords", nlp_response.keywords);
                output.set("status", "available");
            }
            Err(_err) => {
                // HanLP endpoint not ready — return empty results.
                output.set("entities", serde_json::Value::Array(vec![]));
                output.set("keywords", serde_json::Value::Array(vec![]));
                output.set("status", "unavailable");
            }
        }

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
        let processor = HanlpClientProcessor::new("http://localhost:50052".into());
        assert!(processor.matches(Path::new("Main.java")));
        assert!(processor.matches(Path::new("app.py")));
        assert!(processor.matches(Path::new("readme.md")));
        assert!(processor.matches(Path::new("config.yaml")));
        assert!(!processor.matches(Path::new("image.png")));
        assert!(!processor.matches(Path::new("data.bin")));
    }

    #[tokio::test]
    async fn returns_empty_placeholder_output() {
        let processor = HanlpClientProcessor::new("http://localhost:50052".into());
        let ctx = make_context("test.java", "class Foo {}");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        // The HanLP endpoint is not implemented, so status should be
        // "unavailable" and entity/keyword lists should be empty.
        assert_eq!(
            output.get("status").and_then(|v| v.as_str()),
            Some("unavailable")
        );
        assert!(output.get("entities").is_some());
        assert!(output.get("keywords").is_some());
    }

    #[tokio::test]
    async fn returns_empty_for_empty_text() {
        let processor = HanlpClientProcessor::new("http://localhost:50052".into());
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
        let processor = HanlpClientProcessor::new("http://localhost:50052".into());
        assert_eq!(processor.name(), "hanlp");
        assert_eq!(processor.priority(), 80);
    }
}
