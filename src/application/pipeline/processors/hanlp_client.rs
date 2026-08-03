//! HanLP NLP 处理器——调用本地 HanLP REST API 服务进行
//! 命名实体识别与关键词提取。
//!
//! 块级数据流（方案 §5.2 / R2）：当 chunk 处理器已运行时，HanLP 分析每个
//! chunk，使其候选与按 `block_index`（= `chunk.chunk_index`）对齐的块一致。
//! 单个块的失败只记录警告并为该块产生空候选，不会中断整个文件。没有 chunk
//! 输出时，回退为将全文作为一个块来分析（block_index = 0，旧行为）。
//!
//! 产生一个 [`ProcessorOutput`]，包含：
//! - `"hanlp_blocks"` —— `{block_index, entities[{text, tag,
//!   frequency}], keywords}` 数组，按 `block_index` 与块对齐
//! - `"status"`       —— `"ok"` 或 `"empty"`

use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::infrastructure::hanlp::{HanlpClient, HanlpResult};

/// 调用本地 HanLP REST API 进行中文文本分析的 NLP 处理器。
pub struct HanlpClientProcessor {
    client: Arc<HanlpClient>,
}

impl HanlpClientProcessor {
    /// 创建由给定 [`HanlpClient`] 支撑的新处理器。
    pub fn new(client: Arc<HanlpClient>) -> Self {
        Self { client }
    }

    /// 将一个块的 HanLP 结果映射为其 JSON 输出形状。`None` 结果
    /// （失败或空块）产生空候选数组。
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

    /// 从 chunk 处理器输出中收集 `(block_index, text)` 对。
    /// 当没有 chunk 输出时，回退为将整个文件作为单块处理。
    /// 当没有可分析内容时返回空的 Vec。
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
        // HanLP 面向文档 / 文本内容——而非结构化代码文件。
        // 与 chunk 处理器的扩展名集合保持一致。
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
                            // 旧行为：全文分析失败即报错。
                            return Err(DtError::Repository(format!("HanLP 分析失败: {e}")));
                        }
                        // 单块失败：记录警告并让该块候选为空——不中断整个文件。
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::hanlp::NamedEntity;
    use std::path::PathBuf;

    /// 不可达的 HanLP 端点——连接被拒绝可快速失败。
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
        // 文档格式匹配——与 chunk 处理器保持一致。
        assert!(processor.matches(Path::new("readme.md")));
        assert!(processor.matches(Path::new("notes.txt")));
        assert!(processor.matches(Path::new("docs.markdown")));
        assert!(processor.matches(Path::new("guide.rst")));
        assert!(processor.matches(Path::new("manual.adoc")));
        assert!(processor.matches(Path::new("config.yaml")));
        assert!(processor.matches(Path::new("config.yml")));
        assert!(processor.matches(Path::new("app.properties")));
        // 结构化代码文件不匹配
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
        // 非连续索引证明我们使用 chunk.chunk_index 而非位置。
        ctx.add_output(
            "chunk",
            make_chunk_output(&[(5, "第一段内容"), (7, "第二段内容")]),
        );

        let output = processor.execute(&ctx).await.unwrap();

        assert_eq!(output.get("status").and_then(|v| v.as_str()), Some("ok"));
        let blocks = output
            .get("hanlp_blocks")
            .and_then(|v| v.as_array())
            .expect("hanlp_blocks 必须是数组");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["block_index"], serde_json::json!(5));
        assert_eq!(blocks[1]["block_index"], serde_json::json!(7));
        // 单块失败（服务器不可达）降级为空候选，
        // 不中断整个文件。
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
        // 保留旧行为：全文分析失败即报错。
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
