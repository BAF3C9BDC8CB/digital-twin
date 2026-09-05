//! 文本分块处理器——包装既有的 `chunker`，将文档文件（Markdown、纯文本、
//! YAML、Properties）分割为适合 embedding 与搜索的重叠块。
//!
//! 产生一个 [`ProcessorOutput`]，包含：
//! - `"chunks"` —— chunk 对象数组，每个包含
//!   `chunk_id`、`text`、`chunk_index`、`prev_chunk_id`、
//!   `next_chunk_id`、`start_char`、`end_char`
//! - `"doc_type"` —— 检测到的 [`DocType`](crate::shared::chunker::DocType)
//!   字符串
//! - `"chunk_count"` —— 产生的 chunk 数量

use async_trait::async_trait;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::shared::chunker::{chunk_by_type, merge_nacos_chunks, ChunkConfig, DocType};

/// 将文档文件分割为语义块。
///
/// 处理 Markdown（.md）、纯文本（.txt）、YAML（.yaml、.yml）
/// 与 Properties（.properties）文件。
///
/// # 配置
///
/// 使用 [`ChunkConfig::default()`]，目标为约 2048-token 的块（8B 模型可充分
/// 利用的上下文粒度），在段落边界处约有 64-token 重叠。若需要自定义尺寸，
/// 可通过 [`ChunkProcessor::with_config`] 覆盖。
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
    /// 使用默认块配置创建新处理器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用自定义块配置创建处理器。
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

    fn matches(&self, ctx: &PipelineContext) -> bool {
        matches!(
            ctx.file_path.extension().and_then(|e| e.to_str()),
            Some("md" | "txt" | "yaml" | "yml" | "properties")
        )
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        // 从扩展名与前几行检测文档类型。
        let first_lines: Vec<&str> = ctx.file_text.lines().take(5).collect();
        let doc_type = DocType::detect(&ctx.file_path.to_string_lossy(), &first_lines);

        // 通过共享构造函数，根据项目与相对路径生成文档 ID
        // （与构建编排层的已删除路径清理共用单一来源，§6.5）。引擎传入的
        // 是项目相对路径，因此 doc_id 形如 `dt://doc/{project}/{rel_path}`。
        let doc_id = crate::domain::id::make_document_id(
            &ctx.project_name,
            &ctx.file_path.to_string_lossy(),
        );

        // 使用类型感知策略将文本分割为块。
        let mut chunks = chunk_by_type(&ctx.file_text, &doc_id, doc_type, &self.config);
        let max_chunks = std::env::var("DT_NACOS_MAX_CHUNKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0);
        let mut original_chunk_count = chunks.len();
        let mut merge_strategy = "none";
        if ctx.source_kind == crate::application::pipeline::FileSourceKind::Nacos {
            let max = max_chunks.unwrap_or(20);
            let target = std::env::var("DT_NACOS_TARGET_CHUNKS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(10);
            original_chunk_count = chunks.len();
            chunks = merge_nacos_chunks(&ctx.file_text, &doc_id, doc_type, target, max).map_err(
                |e| {
                    DtError::General(format!(
                        "Nacos safe chunk merge failed; original text retained: {e}"
                    ))
                },
            )?;
            merge_strategy = "adjacent_sections";
            tracing::info!(target: "pipeline_diagnostics", event = "nacos.chunk_diagnostic", file = %ctx.file_path.display(), original_chunk_count, chunk_count = chunks.len(), target_chunks = target, max_chunks = max, merge_strategy);
        }

        // 将块序列化为 JSON 数组。
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
        if ctx.source_kind == crate::application::pipeline::FileSourceKind::Nacos {
            output.set("source_kind", "nacos");
            output.set("original_chunk_count", original_chunk_count);
            output.set("merge_strategy", merge_strategy);
            output.set("original_text_preserved", true);
        }

        Ok(output)
    }
}

/// 每种文档类型的人类可读名称。
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::FileSourceKind;
    use std::path::PathBuf;

    fn make_context(file_name: &str, text: &str) -> PipelineContext {
        PipelineContext::new(
            PathBuf::from(file_name),
            text.to_string(),
            "test".to_string(),
            FileSourceKind::Fs,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn matches_doc_extensions() {
        let processor = ChunkProcessor::new();
        assert!(processor.matches(&PipelineContext::new(
            PathBuf::from("README.md"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
        assert!(processor.matches(&PipelineContext::new(
            PathBuf::from("notes.txt"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
        assert!(processor.matches(&PipelineContext::new(
            PathBuf::from("config.yaml"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
        assert!(processor.matches(&PipelineContext::new(
            PathBuf::from("config.yml"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
        assert!(processor.matches(&PipelineContext::new(
            PathBuf::from("app.properties"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
        assert!(!processor.matches(&PipelineContext::new(
            PathBuf::from("main.rs"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
        assert!(!processor.matches(&PipelineContext::new(
            PathBuf::from("Main.java"),
            String::new(),
            "test".into(),
            FileSourceKind::Fs,
            None,
            None,
        )));
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
        let chunk_count = output
            .get("chunk_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
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
