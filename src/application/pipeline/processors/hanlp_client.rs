//! HanLP NLP processor — calls a local HanLP REST API service for
//! named-entity recognition and keyword extraction.
//!
//! Block-level data flow (方案 §5.2 / R2): when the chunk processor has run,
//! HanLP analyses each chunk so its candidates align with the chunks by
//! `block_index` (= `chunk.chunk_index`). A single block's failure logs a
//! warning and yields empty candidates for that block without interrupting
//! the file. Without chunk output it falls back to analysing the whole text
//! as a single block (block_index = 0, old behaviour).
//!
//! Produces a [`ProcessorOutput`] with:
//! - `"hanlp_blocks"` — array of `{block_index, entities[{text, tag,
//!   frequency}], keywords}` aligned with chunks by `block_index`
//! - `"status"`       — `"ok"` or `"empty"`

use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::infrastructure::hanlp::{HanlpClient, HanlpResult};

/// NLP processor that calls a local HanLP REST API for Chinese text analysis.
pub struct HanlpClientProcessor {
    client: Arc<HanlpClient>,
}

impl HanlpClientProcessor {
    /// Create a new processor backed by the given [`HanlpClient`].
    pub fn new(client: Arc<HanlpClient>) -> Self {
        Self { client }
    }

    /// Map one block's HanLP result to its JSON output shape. A `None`
    /// result (failed or empty block) yields empty candidate arrays.
    fn block_to_json(block_index: u32, result: Option<&HanlpResult>) -> serde_json::Value {
        let (entities, keywords) = match result {
            Some(r) => (
                r.entities
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "text": e.text,
                            "tag": e.tag,
                            "frequency": e.frequency,
                        })
                    })
                    .collect::<Vec<_>>(),
                r.keywords.clone(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        serde_json::json!({
            "block_index": block_index,
            "entities": entities,
            "keywords": keywords,
        })
    }

    /// Collect `(block_index, text)` pairs from the chunk processor output.
    /// Falls back to the whole file as a single block when there is no chunk
    /// output. Returns an empty Vec when there is nothing to analyse.
    fn collect_blocks(ctx: &PipelineContext) -> Vec<(u32, String)> {
        if let Some(chunk_out) = ctx.get_output("chunk") {
            return chunk_out
                .get("chunks")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(i, c)| {
                            let index = c
                                .get("chunk_index")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(i as u64) as u32;
                            let text = c
                                .get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            (index, text)
                        })
                        .collect()
                })
                .unwrap_or_default();
        }

        if ctx.file_text.trim().is_empty() {
            Vec::new()
        } else {
            vec![(0, ctx.file_text.clone())]
        }
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
        // HanLP is for document/text content — not structured code files.
        // Aligned with the chunk processor's extension set.
        matches!(
            file_path.extension().and_then(|e| e.to_str()),
            Some("md" | "txt" | "markdown" | "rst" | "adoc" | "yaml" | "yml" | "properties")
        )
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();
        let fallback = ctx.get_output("chunk").is_none();
        let blocks = Self::collect_blocks(ctx);

        if blocks.is_empty() {
            output.set("hanlp_blocks", Vec::<serde_json::Value>::new());
            output.set("status", "empty");
            return Ok(output);
        }

        let mut hanlp_blocks = Vec::with_capacity(blocks.len());
        for (block_index, text) in blocks {
            let result = if text.trim().is_empty() {
                None
            } else {
                match self.client.analyze(&text).await {
                    Ok(r) => Some(r),
                    Err(e) => {
                        if fallback {
                            // Old behaviour: a failed full-text analysis errors.
                            return Err(DtError::Repository(format!("HanLP analysis failed: {e}")));
                        }
                        // Single-block failure: warn and leave the block's
                        // candidates empty — do not interrupt the file.
                        tracing::warn!("HanLP 块 {block_index} 分析失败, 该块候选为空: {e}");
                        None
                    }
                }
            };
            hanlp_blocks.push(Self::block_to_json(block_index, result.as_ref()));
        }

        output.set("hanlp_blocks", hanlp_blocks);
        output.set("status", "ok");

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::hanlp::NamedEntity;
    use std::path::PathBuf;

    /// Unreachable HanLP endpoint — connection refused fails fast.
    const UNREACHABLE: &str = "http://127.0.0.1:1";

    fn make_context(file_name: &str, text: &str) -> PipelineContext {
        PipelineContext::new(
            PathBuf::from(file_name),
            text.to_string(),
            "test".to_string(),
        )
    }

    fn make_chunk_output(chunks: &[(u64, &str)]) -> ProcessorOutput {
        let mut out = ProcessorOutput::new();
        let chunk_values: Vec<serde_json::Value> = chunks
            .iter()
            .map(|(idx, text)| {
                serde_json::json!({
                    "chunk_id": format!("dt://doc/test/doc.md#{idx}"),
                    "text": text,
                    "chunk_index": idx,
                })
            })
            .collect();
        out.set("chunks", chunk_values);
        out.set("doc_id", "dt://doc/test/doc.md");
        out.set("chunk_count", chunks.len());
        out
    }

    #[tokio::test]
    async fn matches_doc_extensions_including_yaml() {
        let client = Arc::new(HanlpClient::new(UNREACHABLE, ""));
        let processor = HanlpClientProcessor::new(client);
        // Doc formats match — aligned with the chunk processor.
        assert!(processor.matches(Path::new("readme.md")));
        assert!(processor.matches(Path::new("notes.txt")));
        assert!(processor.matches(Path::new("docs.markdown")));
        assert!(processor.matches(Path::new("guide.rst")));
        assert!(processor.matches(Path::new("manual.adoc")));
        assert!(processor.matches(Path::new("config.yaml")));
        assert!(processor.matches(Path::new("config.yml")));
        assert!(processor.matches(Path::new("app.properties")));
        // Structured code files do NOT match
        assert!(!processor.matches(Path::new("Main.java")));
        assert!(!processor.matches(Path::new("app.py")));
        assert!(!processor.matches(Path::new("image.png")));
        assert!(!processor.matches(Path::new("data.bin")));
    }

    #[tokio::test]
    async fn returns_empty_for_empty_text_without_chunks() {
        let client = Arc::new(HanlpClient::new(UNREACHABLE, ""));
        let processor = HanlpClientProcessor::new(client);
        let ctx = make_context("empty.txt", "   ");
        let output = processor.execute(&ctx).await.unwrap();
        assert_eq!(output.get("status").and_then(|v| v.as_str()), Some("empty"));
        assert_eq!(
            output
                .get("hanlp_blocks")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(0)
        );
    }

    #[tokio::test]
    async fn returns_empty_for_empty_chunk_list() {
        let client = Arc::new(HanlpClient::new(UNREACHABLE, ""));
        let processor = HanlpClientProcessor::new(client);
        let mut ctx = make_context("empty.md", "");
        ctx.add_output("chunk", make_chunk_output(&[]));
        let output = processor.execute(&ctx).await.unwrap();
        assert_eq!(output.get("status").and_then(|v| v.as_str()), Some("empty"));
    }

    #[tokio::test]
    async fn name_and_priority() {
        let client = Arc::new(HanlpClient::new(UNREACHABLE, ""));
        let processor = HanlpClientProcessor::new(client);
        assert_eq!(processor.name(), "hanlp");
        assert_eq!(processor.priority(), 80);
    }

    #[tokio::test]
    async fn block_output_aligned_with_chunk_indices() {
        let client = Arc::new(HanlpClient::new(UNREACHABLE, ""));
        let processor = HanlpClientProcessor::new(client);
        let mut ctx = make_context("doc.md", "第一段内容\n\n第二段内容");
        // Non-contiguous indices prove we use chunk.chunk_index, not position.
        ctx.add_output(
            "chunk",
            make_chunk_output(&[(5, "第一段内容"), (7, "第二段内容")]),
        );

        let output = processor.execute(&ctx).await.unwrap();

        assert_eq!(output.get("status").and_then(|v| v.as_str()), Some("ok"));
        let blocks = output
            .get("hanlp_blocks")
            .and_then(|v| v.as_array())
            .expect("hanlp_blocks must be an array");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["block_index"], serde_json::json!(5));
        assert_eq!(blocks[1]["block_index"], serde_json::json!(7));
        // Single-block failure (unreachable server) degrades to empty
        // candidates without interrupting the file.
        for b in blocks {
            assert_eq!(b["entities"], serde_json::json!([]));
            assert_eq!(b["keywords"], serde_json::json!([]));
        }
    }

    #[tokio::test]
    async fn fallback_without_chunk_output_errors_on_failure() {
        let client = Arc::new(HanlpClient::new(UNREACHABLE, ""));
        let processor = HanlpClientProcessor::new(client);
        let ctx = make_context("notes.markdown", "没有 chunk 输出时回退到全文单块");
        // Old behaviour preserved: a failed full-text analysis is an error.
        assert!(processor.execute(&ctx).await.is_err());
    }

    #[test]
    fn block_to_json_maps_result() {
        let result = HanlpResult {
            entities: vec![NamedEntity {
                text: "支付网关".into(),
                tag: "NN".into(),
                frequency: 3,
            }],
            keywords: vec!["支付".into()],
            summary: "忽略".into(),
        };
        let v = HanlpClientProcessor::block_to_json(4, Some(&result));
        assert_eq!(v["block_index"], serde_json::json!(4));
        assert_eq!(
            v["entities"],
            serde_json::json!([{"text": "支付网关", "tag": "NN", "frequency": 3}])
        );
        assert_eq!(v["keywords"], serde_json::json!(["支付"]));
    }

    #[test]
    fn block_to_json_maps_none_to_empty() {
        let v = HanlpClientProcessor::block_to_json(2, None);
        assert_eq!(v["block_index"], serde_json::json!(2));
        assert_eq!(v["entities"], serde_json::json!([]));
        assert_eq!(v["keywords"], serde_json::json!([]));
    }
}
