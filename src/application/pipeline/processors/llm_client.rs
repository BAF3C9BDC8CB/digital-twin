//! LLM analysis processor -- calls the inference server's
//! `/v1/chat/completions` endpoint with a prompt selected according to
//! which upstream processors have already run.
//!
//! Prompt selection logic:
//! - If the context contains `tree_sitter` output -> `"code_with_ast"`
//!   (single call, unparsed response — legacy code path, unchanged)
//! - If the context contains `chunk` output       -> `"document_with_nlp"`
//!   (block-level extraction loop, 方案 §5.2: one LLM call per chunk,
//!   HanLP candidates injected per aligned `block_index`)
//! - Otherwise                                     -> `"raw_text"`
//!   (single call, unparsed response — unchanged)
//!
//! Block-level extraction produces a [`ProcessorOutput`] with:
//! - `"graphs"`         -- array of [`ExtractedGraph`], one per chunk
//! - `"response"`       -- all blocks' raw responses joined with `"\n\n"`
//!   (kept for the legacy store consumer; Task 2 removes it)
//! - `"prompt_name"`    -- `"document_with_nlp"`
//! - `"model"`          -- the model identifier string
//! - `"degraded_count"` -- blocks whose JSON parse failed even after one retry
//! - `"block_count"`    -- number of chunks processed
//!
//! Single-call paths keep the legacy output: `{"response", "prompt_name",
//! "model"}` — byte-for-byte unchanged.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::application::knowledge::extract::{
    degraded_graph, parse_block_response, ExtractedGraph,
};
use crate::application::pipeline::config::LlmConfig;
use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::infer_client::ChatClient;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::application::pipeline::prompt::PromptRegistry;
use crate::domain::error::DtError;

/// Correction hint appended to the user prompt when retrying after a JSON
/// parse failure (§5.5).
const JSON_CORRECTION: &str =
    "【修正】上一次回复不是合法 JSON。仅输出 JSON：不要 markdown 围栏，不要额外说明。";

/// LLM-powered analysis processor.
///
/// Uses the configured [`ChatClient`] (SiliconFlow or XInference) to call
/// the chat endpoint. The prompt template is selected dynamically based
/// on which prior processors produced output.
pub struct LlmClientProcessor {
    client: Arc<dyn ChatClient>,
    model: String,
    prompt_registry: Arc<PromptRegistry>,
    llm_config: LlmConfig,
}

impl LlmClientProcessor {
    /// Create a new LLM analysis processor.
    pub fn new(
        client: Arc<dyn ChatClient>,
        model: String,
        prompt_registry: Arc<PromptRegistry>,
        llm_config: LlmConfig,
    ) -> Self {
        Self {
            client,
            model,
            prompt_registry,
            llm_config,
        }
    }
}

#[async_trait]
impl Processor for LlmClientProcessor {
    fn name(&self) -> &str {
        "llm"
    }

    fn priority(&self) -> i32 {
        60
    }

    fn matches(&self, file_path: &Path) -> bool {
        matches!(
            file_path.extension().and_then(|e| e.to_str()),
            Some(
                "java"
                    | "py"
                    | "rs"
                    | "go"
                    | "ts"
                    | "tsx"
                    | "js"
                    | "jsx"
                    | "php"
                    | "md"
                    | "txt"
                    | "yaml"
                    | "yml"
                    | "properties"
            )
        )
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        // 1. Select the prompt based on which upstream outputs exist.
        let prompt_name = select_prompt(ctx);

        // 2. Block-level extraction for documents; legacy single call otherwise.
        if prompt_name == "document_with_nlp" {
            self.execute_block_extraction(ctx).await
        } else {
            self.execute_single_call(ctx, &prompt_name).await
        }
    }
}

impl LlmClientProcessor {
    /// Legacy single-call path (`code_with_ast` / `raw_text`): render once,
    /// call once, return the unparsed response. Output shape unchanged.
    async fn execute_single_call(
        &self,
        ctx: &PipelineContext,
        prompt_name: &str,
    ) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        let render_ctx = build_render_context(ctx);
        let (system_prompt, user_prompt) = self
            .prompt_registry
            .render(prompt_name, &render_ctx)
            .map_err(|e| DtError::General(format!("prompt render error: {e}")))?;

        let response_text = self
            .chat_content(&system_prompt, &user_prompt)
            .await
            .map_err(|e| DtError::General(format!("LLM chat error: {e}")))?;

        output.set("response", response_text);
        output.set("prompt_name", prompt_name);
        output.set("model", self.model.clone());

        Ok(output)
    }

    /// Block-level extraction loop (§5.2): one LLM call per chunk, serial.
    ///
    /// Each block's prompt is rendered with the chunk text and the HanLP
    /// candidates aligned by `block_index` (empty placeholders when HanLP is
    /// absent). Each response is parsed into an [`ExtractedGraph`]; blocks
    /// that fail parsing even after one retry degrade to an empty graph with
    /// `degraded = true` (§5.5).
    async fn execute_block_extraction(
        &self,
        ctx: &PipelineContext,
    ) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        let chunk_out = ctx
            .get_output("chunk")
            .ok_or_else(|| DtError::General("chunk output missing".to_string()))?;
        let doc_id = chunk_out
            .get("doc_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                format!(
                    "dt://doc/{}/{}",
                    ctx.project_name,
                    ctx.file_path.to_string_lossy()
                )
            });
        let empty_chunks = Vec::new();
        let chunks = chunk_out
            .get("chunks")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty_chunks);

        // HanLP candidates keyed by block_index for per-block injection.
        let hanlp_map: HashMap<u64, &serde_json::Value> = ctx
            .get_output("hanlp")
            .and_then(|o| o.get("hanlp_blocks"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| {
                        b.get("block_index")
                            .and_then(|v| v.as_u64())
                            .map(|i| (i, b))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut graphs: Vec<ExtractedGraph> = Vec::with_capacity(chunks.len());
        let mut raw_responses: Vec<String> = Vec::with_capacity(chunks.len());

        for (pos, chunk) in chunks.iter().enumerate() {
            let block_index = chunk
                .get("chunk_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(pos as u64);
            let block_text = chunk
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let (entities, keywords) =
                format_hanlp_candidates(hanlp_map.get(&block_index).copied());
            let render_ctx = build_block_render_context(ctx, block_text, &entities, &keywords);
            let (system_prompt, user_prompt) = self
                .prompt_registry
                .render("document_with_nlp", &render_ctx)
                .map_err(|e| DtError::General(format!("prompt render error: {e}")))?;

            let (raw, graph) = self
                .extract_block(&system_prompt, &user_prompt, &doc_id, block_index as u32)
                .await;
            if let Some(raw) = raw {
                raw_responses.push(raw);
            }
            graphs.push(graph);
        }

        let degraded_count = graphs.iter().filter(|g| g.degraded).count();

        output.set("graphs", &graphs);
        output.set("response", raw_responses.join("\n\n"));
        output.set("prompt_name", "document_with_nlp");
        output.set("model", self.model.clone());
        output.set("degraded_count", degraded_count);
        output.set("block_count", chunks.len());

        Ok(output)
    }

    /// One block's LLM call + JSON parse. On parse failure retries once with
    /// a JSON-correction hint; a second failure (or a chat error) degrades
    /// the block (§5.5). Returns the final attempt's raw response (if any)
    /// and the resulting graph.
    async fn extract_block(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        doc_id: &str,
        block_index: u32,
    ) -> (Option<String>, ExtractedGraph) {
        let raw = match self.chat_content(system_prompt, user_prompt).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("块 {block_index} LLM 调用失败, 降级: {e}");
                return (None, degraded_graph(doc_id, block_index));
            }
        };
        match parse_block_response(&raw, doc_id, block_index) {
            Ok(g) => return (Some(raw), g),
            Err(e) => tracing::warn!("块 {block_index} JSON 解析失败, 重试一次: {e}"),
        }

        let retry_prompt = format!("{user_prompt}\n\n{JSON_CORRECTION}");
        let retry_raw = match self.chat_content(system_prompt, &retry_prompt).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("块 {block_index} 重试调用失败, 降级: {e}");
                return (Some(raw), degraded_graph(doc_id, block_index));
            }
        };
        match parse_block_response(&retry_raw, doc_id, block_index) {
            Ok(g) => (Some(retry_raw), g),
            Err(e) => {
                tracing::warn!("块 {block_index} 重试后仍无法解析, 降级: {e}");
                (Some(retry_raw), degraded_graph(doc_id, block_index))
            }
        }
    }

    /// Call the chat endpoint and extract the first choice's content.
    async fn chat_content(&self, system_prompt: &str, user_prompt: &str) -> Result<String, String> {
        let resp = self
            .client
            .chat(
                &self.model,
                system_prompt,
                user_prompt,
                self.llm_config.temperature,
                self.llm_config.max_tokens,
            )
            .await?;
        Ok(resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default())
    }
}

/// Select the most appropriate prompt name based on available upstream
/// outputs in the pipeline context.
fn select_prompt(ctx: &PipelineContext) -> String {
    if ctx.outputs.contains_key("tree_sitter") {
        "code_with_ast".to_string()
    } else if ctx.outputs.contains_key("chunk") {
        "document_with_nlp".to_string()
    } else {
        "raw_text".to_string()
    }
}

/// Build the render context for the legacy single-call paths
/// (`code_with_ast` / `raw_text`).
fn build_render_context(ctx: &PipelineContext) -> serde_json::Value {
    serde_json::json!({
        "file_path": ctx.file_path.to_string_lossy(),
        "project_name": ctx.project_name,
        "file_text": ctx.file_text,
    })
}

/// Build the render context for one block (§5.2): the flat keys
/// `file_path` / `file_text` (block text) / `entities` / `keywords`.
fn build_block_render_context(
    ctx: &PipelineContext,
    block_text: &str,
    entities: &str,
    keywords: &str,
) -> serde_json::Value {
    serde_json::json!({
        "file_path": ctx.file_path.to_string_lossy(),
        "project_name": ctx.project_name,
        "file_text": block_text,
        "entities": entities,
        "keywords": keywords,
    })
}

/// Render one HanLP block's candidates into readable prompt strings
/// (e.g. `- 支付网关 (NN, 频次3)`). Missing or empty candidates become
/// `"（无）"` so no `${...}` placeholder survives rendering.
fn format_hanlp_candidates(block: Option<&serde_json::Value>) -> (String, String) {
    let entities = block
        .and_then(|b| b.get("entities"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let text = e.get("text").and_then(|v| v.as_str())?;
                    let tag = e.get("tag").and_then(|v| v.as_str()).unwrap_or("");
                    let freq = e.get("frequency").and_then(|v| v.as_u64()).unwrap_or(0);
                    Some(format!("- {text} ({tag}, 频次{freq})"))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "（无）".to_string());

    let keywords = block
        .and_then(|b| b.get("keywords"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "（无）".to_string());

    (entities, keywords)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::extract::ExtractedGraph;
    use crate::application::pipeline::infer_client::{ChatResponse, Choice, Message};
    use crate::application::pipeline::output::ProcessorOutput;

    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // ── Mock ChatClient ───────────────────────────────────────────────

    struct MockChatClient {
        script: Mutex<VecDeque<Result<String, String>>>,
        calls: Mutex<Vec<(String, String)>>,
    }

    impl MockChatClient {
        fn new(responses: Vec<Result<String, String>>) -> Self {
            Self {
                script: Mutex::new(responses.into()),
                calls: Mutex::new(vec![]),
            }
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChatClient for MockChatClient {
        async fn chat(
            &self,
            _model: &str,
            system_prompt: &str,
            user_prompt: &str,
            _temperature: f32,
            _max_tokens: u32,
        ) -> Result<ChatResponse, String> {
            self.calls
                .lock()
                .unwrap()
                .push((system_prompt.to_string(), user_prompt.to_string()));
            let content = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock script exhausted")?;
            Ok(ChatResponse {
                choices: vec![Choice {
                    message: Message { content },
                }],
            })
        }

        async fn health_check(&self) -> Result<bool, String> {
            Ok(true)
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────

    fn test_registry() -> Arc<PromptRegistry> {
        Arc::new(
            PromptRegistry::load(Path::new("config/prompts")).expect("config/prompts must load"),
        )
    }

    fn make_processor(client: Arc<MockChatClient>) -> LlmClientProcessor {
        LlmClientProcessor::new(
            client,
            "qwen3.5".to_string(),
            test_registry(),
            LlmConfig::default(),
        )
    }

    fn make_context(
        file_name: &str,
        text: &str,
        outputs: Vec<(&str, ProcessorOutput)>,
    ) -> PipelineContext {
        let mut ctx = PipelineContext::new(
            PathBuf::from(file_name),
            text.to_string(),
            "test".to_string(),
        );
        for (name, out) in outputs {
            ctx.add_output(name, out);
        }
        ctx
    }

    fn make_chunk_output(texts: &[(u64, &str)]) -> ProcessorOutput {
        let mut out = ProcessorOutput::new();
        let chunks: Vec<serde_json::Value> = texts
            .iter()
            .map(|(idx, text)| {
                serde_json::json!({
                    "chunk_id": format!("dt://doc/test/doc.md#{idx}"),
                    "text": text,
                    "chunk_index": idx,
                })
            })
            .collect();
        out.set("chunks", chunks);
        out.set("doc_id", "dt://doc/test/doc.md");
        out.set("chunk_count", texts.len());
        out
    }

    fn make_hanlp_output(blocks: Vec<serde_json::Value>) -> ProcessorOutput {
        let mut out = ProcessorOutput::new();
        out.set("hanlp_blocks", blocks);
        out.set("status", "ok");
        out
    }

    const VALID_JSON_0: &str = r#"{"block_summary":"块0概述","entities":[{"mention":"支付网关服务","canonical_name":"支付网关","type":"Service","summary":"处理支付路由","keywords":["支付"]}],"relations":[]}"#;
    const VALID_JSON_1: &str = r#"{"block_summary":"块1概述","entities":[],"relations":[]}"#;

    // ── Prompt selection ──────────────────────────────────────────────

    #[test]
    fn selects_code_with_ast_when_tree_sitter_present() {
        let ts_out = ProcessorOutput::new();
        let ctx = make_context("main.rs", "fn main() {}", vec![("tree_sitter", ts_out)]);
        assert_eq!(select_prompt(&ctx), "code_with_ast");
    }

    #[test]
    fn selects_document_with_nlp_when_chunk_present() {
        let chunk_out = ProcessorOutput::new();
        let ctx = make_context("doc.md", "some text", vec![("chunk", chunk_out)]);
        assert_eq!(select_prompt(&ctx), "document_with_nlp");
    }

    #[test]
    fn selects_raw_text_when_no_upstream_output() {
        let ctx = make_context("readme.md", "# Hello", vec![]);
        assert_eq!(select_prompt(&ctx), "raw_text");
    }

    #[test]
    fn tree_sitter_takes_precedence_over_chunk() {
        let ts_out = ProcessorOutput::new();
        let chunk_out = ProcessorOutput::new();
        let ctx = make_context(
            "app.java",
            "class App {}",
            vec![("tree_sitter", ts_out), ("chunk", chunk_out)],
        );
        assert_eq!(select_prompt(&ctx), "code_with_ast");
    }

    // ── Render context ────────────────────────────────────────────────

    #[test]
    fn build_render_context_contains_file_info() {
        let ctx = make_context("src/lib.rs", "pub fn foo() {}", vec![]);
        let json = build_render_context(&ctx);
        assert_eq!(json["file_path"], "src/lib.rs");
        assert_eq!(json["file_text"], "pub fn foo() {}");
        assert_eq!(json["project_name"], "test");
    }

    #[test]
    fn format_hanlp_candidates_renders_readable_list() {
        let block = serde_json::json!({
            "block_index": 0,
            "entities": [{"text": "支付网关", "tag": "NN", "frequency": 3}],
            "keywords": ["支付", "路由"]
        });
        let (entities, keywords) = format_hanlp_candidates(Some(&block));
        assert_eq!(entities, "- 支付网关 (NN, 频次3)");
        assert_eq!(keywords, "支付, 路由");
    }

    #[test]
    fn format_hanlp_candidates_absent_or_empty_uses_placeholder() {
        assert_eq!(
            format_hanlp_candidates(None),
            ("（无）".to_string(), "（无）".to_string())
        );
        let empty = serde_json::json!({"block_index": 0, "entities": [], "keywords": []});
        assert_eq!(
            format_hanlp_candidates(Some(&empty)),
            ("（无）".to_string(), "（无）".to_string())
        );
    }

    // ── Block extraction (document_with_nlp path) ─────────────────────

    #[tokio::test]
    async fn block_extraction_produces_graph_per_chunk() {
        let chunk_out = make_chunk_output(&[(0, "块0文本"), (1, "块1文本")]);
        let ctx = make_context("doc.md", "全文", vec![("chunk", chunk_out)]);
        let mock = Arc::new(MockChatClient::new(vec![
            Ok(VALID_JSON_0.to_string()),
            Ok(VALID_JSON_1.to_string()),
        ]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(
            out.get("prompt_name").and_then(|v| v.as_str()),
            Some("document_with_nlp")
        );
        assert_eq!(out.get("model").and_then(|v| v.as_str()), Some("qwen3.5"));
        assert_eq!(out.get("block_count").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(out.get("degraded_count").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some(format!("{VALID_JSON_0}\n\n{VALID_JSON_1}").as_str())
        );

        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(out.get("graphs").unwrap().clone()).unwrap();
        assert_eq!(graphs.len(), 2);
        assert_eq!(graphs[0].doc_id, "dt://doc/test/doc.md");
        assert_eq!(graphs[0].block_index, 0);
        assert_eq!(graphs[0].block_summary, "块0概述");
        assert_eq!(graphs[0].entities[0].canonical_name, "支付网关");
        assert!(!graphs[0].degraded);
        assert_eq!(graphs[1].block_index, 1);

        // One serial LLM call per chunk.
        assert_eq!(mock.calls().len(), 2);
    }

    #[tokio::test]
    async fn block_extraction_injects_aligned_hanlp_candidates() {
        let chunk_out = make_chunk_output(&[(0, "块0文本"), (1, "块1文本")]);
        let hanlp_out = make_hanlp_output(vec![
            serde_json::json!({
                "block_index": 0,
                "entities": [{"text": "支付网关", "tag": "NN", "frequency": 3}],
                "keywords": ["支付", "路由"]
            }),
            serde_json::json!({"block_index": 1, "entities": [], "keywords": []}),
        ]);
        let ctx = make_context(
            "doc.md",
            "全文",
            vec![("chunk", chunk_out), ("hanlp", hanlp_out)],
        );
        let mock = Arc::new(MockChatClient::new(vec![
            Ok(VALID_JSON_0.to_string()),
            Ok(VALID_JSON_1.to_string()),
        ]));
        let processor = make_processor(mock.clone());

        processor.execute(&ctx).await.unwrap();
        let calls = mock.calls();

        // Block 0 gets its own candidates; block 1 gets the placeholder.
        assert!(calls[0].1.contains("- 支付网关 (NN, 频次3)"));
        assert!(calls[0].1.contains("支付, 路由"));
        assert!(calls[1].1.contains("（无）"));
        // Block text — not the whole file — is injected per block.
        assert!(calls[0].1.contains("块0文本"));
        assert!(!calls[0].1.contains("块1文本"));
        assert!(calls[1].1.contains("块1文本"));
        // No unresolved template placeholder survives rendering.
        for (_, user) in &calls {
            assert!(!user.contains("${"), "template residue in: {user}");
        }
    }

    #[tokio::test]
    async fn block_extraction_without_hanlp_uses_placeholder() {
        let chunk_out = make_chunk_output(&[(0, "块0文本")]);
        let ctx = make_context("doc.md", "全文", vec![("chunk", chunk_out)]);
        let mock = Arc::new(MockChatClient::new(vec![Ok(VALID_JSON_0.to_string())]));
        let processor = make_processor(mock.clone());

        processor.execute(&ctx).await.unwrap();
        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.contains("（无）"));
        assert!(!calls[0].1.contains("${"));
    }

    #[tokio::test]
    async fn parse_failure_retries_once_with_correction_then_succeeds() {
        let chunk_out = make_chunk_output(&[(0, "块0文本")]);
        let ctx = make_context("doc.md", "全文", vec![("chunk", chunk_out)]);
        let mock = Arc::new(MockChatClient::new(vec![
            Ok("这不是 JSON".to_string()),
            Ok(VALID_JSON_0.to_string()),
        ]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(mock.calls().len(), 2);
        // The retry carries the "JSON only" correction hint.
        assert!(mock.calls()[1].1.contains("仅输出 JSON"));
        assert_eq!(out.get("degraded_count").and_then(|v| v.as_u64()), Some(0));
        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(out.get("graphs").unwrap().clone()).unwrap();
        assert!(!graphs[0].degraded);
        assert_eq!(graphs[0].block_summary, "块0概述");
    }

    #[tokio::test]
    async fn double_parse_failure_degrades_block() {
        let chunk_out = make_chunk_output(&[(0, "块0文本")]);
        let ctx = make_context("doc.md", "全文", vec![("chunk", chunk_out)]);
        let mock = Arc::new(MockChatClient::new(vec![
            Ok("garbage one".to_string()),
            Ok("garbage two".to_string()),
        ]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(mock.calls().len(), 2);
        assert_eq!(out.get("degraded_count").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(out.get("block_count").and_then(|v| v.as_u64()), Some(1));
        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(out.get("graphs").unwrap().clone()).unwrap();
        assert!(graphs[0].degraded);
        assert!(graphs[0].entities.is_empty());
        assert!(graphs[0].relations.is_empty());
        assert_eq!(graphs[0].doc_id, "dt://doc/test/doc.md");
        // The last raw response is kept in the legacy "response" field.
        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some("garbage two")
        );
    }

    #[tokio::test]
    async fn chat_error_degrades_block_without_retry() {
        let chunk_out = make_chunk_output(&[(0, "块0文本")]);
        let ctx = make_context("doc.md", "全文", vec![("chunk", chunk_out)]);
        let mock = Arc::new(MockChatClient::new(vec![Err(
            "connection refused".to_string()
        )]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(mock.calls().len(), 1);
        assert_eq!(out.get("degraded_count").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(out.get("response").and_then(|v| v.as_str()), Some(""));
        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(out.get("graphs").unwrap().clone()).unwrap();
        assert!(graphs[0].degraded);
    }

    // ── Legacy single-call paths (unchanged) ──────────────────────────

    #[tokio::test]
    async fn raw_text_path_keeps_legacy_output() {
        let ctx = make_context("readme.md", "# Hello", vec![]);
        let mock = Arc::new(MockChatClient::new(vec![Ok("分析结果".to_string())]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some("分析结果")
        );
        assert_eq!(
            out.get("prompt_name").and_then(|v| v.as_str()),
            Some("raw_text")
        );
        assert_eq!(out.get("model").and_then(|v| v.as_str()), Some("qwen3.5"));
        assert!(out.get("graphs").is_none());
        assert!(out.get("block_count").is_none());
        assert!(out.get("degraded_count").is_none());
        assert_eq!(mock.calls().len(), 1);
    }

    #[tokio::test]
    async fn code_path_keeps_legacy_output() {
        let ts_out = ProcessorOutput::new();
        let ctx = make_context("main.rs", "fn main() {}", vec![("tree_sitter", ts_out)]);
        let mock = Arc::new(MockChatClient::new(vec![Ok("代码分析".to_string())]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(
            out.get("response").and_then(|v| v.as_str()),
            Some("代码分析")
        );
        assert_eq!(
            out.get("prompt_name").and_then(|v| v.as_str()),
            Some("code_with_ast")
        );
        assert_eq!(out.get("model").and_then(|v| v.as_str()), Some("qwen3.5"));
        assert!(out.get("graphs").is_none());
        assert!(out.get("block_count").is_none());
        assert!(out.get("degraded_count").is_none());
        assert_eq!(mock.calls().len(), 1);
    }

    #[test]
    fn matches_code_and_doc_extensions() {
        let matches = |path: &Path| -> bool {
            matches!(
                path.extension().and_then(|e| e.to_str()),
                Some(
                    "java"
                        | "py"
                        | "rs"
                        | "go"
                        | "ts"
                        | "tsx"
                        | "js"
                        | "jsx"
                        | "php"
                        | "md"
                        | "txt"
                        | "yaml"
                        | "yml"
                        | "properties"
                )
            )
        };
        assert!(matches(Path::new("main.rs")));
        assert!(matches(Path::new("readme.md")));
        assert!(!matches(Path::new("image.png")));
    }
}
