//! Text chunking processor — wraps the existing `chunker` to split
//! document files (Markdown, plain text, YAML, Properties) into
//! overlapping chunks suitable for embedding and search.
//!
//! Produces a [`ProcessorOutput`] with:
//! - `"chunks"` — JSON array of chunk objects, each containing
//!   `chunk_id`, `text`, `chunk_index`, `prev_chunk_id`,
//!   `next_chunk_id`, `start_char`, `end_char`
//! - `"doc_type"` — the detected [`DocType`](crate::shared::chunker::DocType)
//!   as a string
//! - `"chunk_count"` — number of chunks produced

use async_trait::async_trait;
use std::path::Path;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::shared::chunker::{chunk_by_type, ChunkConfig, DocType};

/// Splits document files into semantic chunks.
///
/// Handles Markdown (.md), plain text (.txt), YAML (.yaml, .yml),
/// and Properties (.properties) files.
///
/// # Configuration
///
/// Uses [`ChunkConfig::default()`] which targets ~512-token chunks
/// with ~64-token overlap at paragraph boundaries.  Override via
/// [`ChunkProcessor::with_config`] if custom sizing is needed.
pub struct ChunkProcessor {
    config: ChunkConfig,
}

impl Default for ChunkProcessor {
    fn default() -> Self {
        Self {
            config: ChunkConfig::default(),
        }
    }
}

impl ChunkProcessor {
    /// Create a new processor with the default chunk configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a processor with a custom chunk configuration.
    pub fn with_config(config: ChunkConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Processor for ChunkProcessor {
    fn name(&self) -> &str {
        "chunk"
    }

    fn priority(&self) -> i32 {
        90
    }

    fn matches(&self, file_path: &Path) -> bool {
        matches!(
            file_path.extension().and_then(|e| e.to_str()),
            Some("md" | "txt" | "yaml" | "yml" | "properties")
        )
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        // Detect document type from extension and first few lines.
        let first_lines: Vec<&str> = ctx.file_text.lines().take(5).collect();
        let doc_type = DocType::detect(
            &ctx.file_path.to_string_lossy(),
            &first_lines,
        );

        // Generate the document ID from the project and relative path.
        // If file_path is absolute we use the full path as the doc_id
        // component — the chunker only needs a unique identifier.
        let doc_id = format!(
            "dt://doc/{}/{}",
            ctx.project_name,
            ctx.file_path.to_string_lossy()
        );

        // Split the text into chunks using the type-aware strategy.
        let chunks = chunk_by_type(&ctx.file_text, &doc_id, doc_type, &self.config);

        // Serialise chunks into a JSON array.
        let chunk_values: Vec<serde_json::Value> = chunks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "chunk_id": c.chunk_id,
                    "text": c.text,
                    "chunk_index": c.chunk_index,
                    "prev_chunk_id": c.prev_chunk_id,
                    "next_chunk_id": c.next_chunk_id,
                    "start_char": c.start_char,
                    "end_char": c.end_char,
                })
            })
            .collect();

        output.set("chunks", chunk_values);
        output.set("doc_type", doc_type.as_str());
        output.set("chunk_count", chunks.len());
        output.set("doc_id", doc_id);

        Ok(output)
    }
}

/// Human-readable name for each document type.
impl DocType {
    fn as_str(&self) -> &'static str {
        match self {
            DocType::PlainText => "plain_text",
            DocType::Markdown => "markdown",
            DocType::Yaml => "yaml",
            DocType::Properties => "properties",
            DocType::EmbeddedCode => "embedded_code",
        }
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
    async fn matches_doc_extensions() {
        let processor = ChunkProcessor::new();
        assert!(processor.matches(Path::new("README.md")));
        assert!(processor.matches(Path::new("notes.txt")));
        assert!(processor.matches(Path::new("config.yaml")));
        assert!(processor.matches(Path::new("config.yml")));
        assert!(processor.matches(Path::new("app.properties")));
        assert!(!processor.matches(Path::new("main.rs")));
        assert!(!processor.matches(Path::new("Main.java")));
    }

    #[tokio::test]
    async fn processes_markdown() {
        let processor = ChunkProcessor::new();
        let md = "# Title\n\nSome paragraph content here.\n\n## Subtitle\n\nMore text.";
        let ctx = make_context("doc.md", md);
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(
            output.get("doc_type").and_then(|v| v.as_str()),
            Some("markdown")
        );
        let chunk_count = output.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0);
        assert!(chunk_count > 0);
        assert!(output.get("chunks").is_some());
    }

    #[tokio::test]
    async fn processes_plain_text() {
        let processor = ChunkProcessor::new();
        let ctx = make_context("readme.txt", "First paragraph.\n\nSecond paragraph.");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(
            output.get("doc_type").and_then(|v| v.as_str()),
            Some("plain_text")
        );
        assert!(output.get("chunks").is_some());
    }

    #[tokio::test]
    async fn processes_yaml() {
        let processor = ChunkProcessor::new();
        let yaml = "server:\n  port: 8080\n  host: localhost\n";
        let ctx = make_context("config.yaml", yaml);
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(
            output.get("doc_type").and_then(|v| v.as_str()),
            Some("yaml")
        );
    }

    #[tokio::test]
    async fn processes_properties() {
        let processor = ChunkProcessor::new();
        let props = "server.port=8080\nserver.host=localhost\n";
        let ctx = make_context("app.properties", props);
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(
            output.get("doc_type").and_then(|v| v.as_str()),
            Some("properties")
        );
    }

    #[tokio::test]
    async fn empty_text_returns_empty_chunks() {
        let processor = ChunkProcessor::new();
        let ctx = make_context("empty.txt", "   ");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.get("chunk_count").and_then(|v| v.as_u64()), Some(0));
    }

    #[tokio::test]
    async fn name_and_priority() {
        let processor = ChunkProcessor::new();
        assert_eq!(processor.name(), "chunk");
        assert_eq!(processor.priority(), 90);
    }
}
