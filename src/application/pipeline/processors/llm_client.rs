//! LLM 分析处理器——调用推理服务器的 `/v1/chat/completions` 端点，
//! 提示词根据哪些上游处理器已运行来选择。
//!
//! 提示词选择逻辑：
//! - Nacos 来源（`source_kind == Nacos` 或虚拟路径以 `dt://nacos/` 开头）
//!   → `"nacos_config"`（F4 词表，块级提取）
//! - 上下文包含 `tree_sitter` 输出 → `"code_with_ast"`
//!   （单次调用、不解析响应——旧代码路径，保持不变）
//! - 上下文包含 `chunk` 输出       → `"document_with_nlp"`
//!   （块级提取循环，方案 §5.2：每个 chunk 一次 LLM 调用）
//! - 其他情况                     → `"raw_text"`
//!   （单次调用、不解析响应——保持不变）
//!
//! 块级提取产生一个 [`ProcessorOutput`]，包含：
//! - `"graphs"`         —— [`ExtractedGraph`] 数组，每个 chunk 一个
//! - `"response"`       —— 所有块的原始响应以 `"\n\n"` 连接
//!   （为旧 store 消费者保留；Task 2 会移除它）
//! - `"prompt_name"`    —— `"document_with_nlp"` 或 `"nacos_config"`
//! - `"model"`          —— 模型标识字符串
//! - `"degraded_count"` —— 即使重试一次后 JSON 解析仍失败的块数
//! - `"block_count"`    —— 已处理的 chunk 数
//!
//! 单次调用路径保留旧输出：`{"response", "prompt_name",
//! "model"}`——逐字节不变。

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use std::time::Instant;

use crate::application::knowledge::extract::{
    degraded_graph, parse_block_response, parse_nacos_block_response, ExtractedGraph,
};
use crate::application::pipeline::config::LlmConfig;
use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::infer_client::ChatClient;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::application::pipeline::prompt::PromptRegistry;
use crate::application::pipeline::virtual_file::FileSourceKind;
use crate::domain::error::DtError;

/// JSON 解析失败后重试时附加到用户提示词中的修正提示（§5.5）。
const JSON_CORRECTION: &str =
    "【修正】上一次回复不是合法 JSON。仅输出 JSON：不要 markdown 围栏，不要额外说明。";

/// 基于 LLM 的分析处理器。
///
/// 使用配置的 [`ChatClient`]（SiliconFlow 或 XInference）调用对话端点。
/// 提示词模板根据哪些前置处理器产生了输出而动态选择。
pub struct LlmClientProcessor {
    client: Arc<dyn ChatClient>,
    model: String,
    prompt_registry: Arc<PromptRegistry>,
    llm_config: LlmConfig,
}

impl LlmClientProcessor {
    /// 创建新的 LLM 分析处理器。
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

    fn matches(&self, ctx: &PipelineContext) -> bool {
        matches!(
            ctx.file_path.extension().and_then(|e| e.to_str()),
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
        // 1. 根据存在哪些上游输出来选择提示词。
        let prompt_name = select_prompt(ctx);

        // 2. 文档/Nacos 配置走块级提取；其他情况走旧式单次调用。
        if prompt_name == "document_with_nlp" || prompt_name == "nacos_config" {
            self.execute_block_extraction(ctx, &prompt_name).await
        } else {
            self.execute_single_call(ctx, &prompt_name).await
        }
    }
}

impl LlmClientProcessor {
    /// 旧式单次调用路径（`code_with_ast` / `raw_text`）：渲染一次、
    /// 调用一次，返回未解析的响应。输出形状不变。
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
            .map_err(|e| DtError::General(format!("提示词渲染错误: {e}")))?;

        let response_text = self
            .chat_content(&system_prompt, &user_prompt)
            .await
            .map_err(|e| DtError::General(format!("LLM 对话错误: {e}")))?;

        output.set("response", response_text);
        output.set("prompt_name", prompt_name);
        output.set("model", self.model.clone());

        Ok(output)
    }

    /// 块级提取：每个 chunk 一次 LLM 调用，受配置的单文件并发限制。
    ///
    /// 每个块的提示词使用块文本渲染（候选字段为空占位）。
    /// 每个响应解析为 [`ExtractedGraph`]；`prompt_name == "nacos_config"`
    /// 时使用 F4 专用解析（name/purpose schema → ExtractedEntity），
    /// 否则使用通用 `parse_block_response`。
    /// 即使重试一次后解析仍失败的块，降级为 `degraded = true` 的空图（§5.5）。
    async fn execute_block_extraction(
        &self,
        ctx: &PipelineContext,
        prompt_name: &str,
    ) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        let chunk_out = ctx
            .get_output("chunk")
            .ok_or_else(|| DtError::General("缺少 chunk 输出".to_string()))?;
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
        let chunks_owned = chunks.clone();

        let file_started = Instant::now();
        tracing::info!(task = "pipeline", run = "live", file = %ctx.file_path.display(), chunk = "all", attempt = 0u32, provider = "siliconflow", model = %self.model, elapsed_ms = 0u128, stage = "file_start", chunks = chunks.len(), "LLM file_start");
        let limit = self.llm_config.chunk_concurrency.max(1);
        let mut results = stream::iter(chunks_owned.into_iter().enumerate().map(|(pos, chunk)| {
            let doc_id = doc_id.clone();
            async move {
            let block_index = chunk.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(pos as u64);
            let block_text = chunk.get("text").and_then(|v| v.as_str()).unwrap_or_default();
            let render_ctx = build_block_render_context(ctx, block_text, "（无）", "（无）");
            let rendered = self.prompt_registry.render(prompt_name, &render_ctx);
            let chunk_started = Instant::now();
            tracing::info!(task = "pipeline", run = "live", file = %ctx.file_path.display(), chunk = block_index, attempt = 0u32, provider = "siliconflow", model = %self.model, elapsed_ms = file_started.elapsed().as_millis(), stage = "chunk_start", "LLM chunk_start");
            let result = match rendered {
                Ok((system_prompt, user_prompt)) => self.extract_block(&system_prompt, &user_prompt, &doc_id, block_index as u32, prompt_name).await,
                Err(_e) => (None, degraded_graph(&doc_id, block_index as u32)),
            };
            tracing::info!(task = "pipeline", run = "live", file = %ctx.file_path.display(), chunk = block_index, attempt = 0u32, provider = "siliconflow", model = %self.model, elapsed_ms = file_started.elapsed().as_millis(), total_ms = chunk_started.elapsed().as_millis(), stage = "chunk_done", "LLM chunk_done");
            (block_index, result)
        }})).buffer_unordered(limit).collect::<Vec<_>>().await;
        results.sort_by_key(|(index, _)| *index);
        let graphs: Vec<ExtractedGraph> = results
            .iter()
            .map(|(_, (_, graph))| graph.clone())
            .collect();
        let raw_responses: Vec<String> = results
            .into_iter()
            .filter_map(|(_, (raw, _))| raw)
            .collect();

        let degraded_count = graphs.iter().filter(|g| g.degraded).count();

        output.set("graphs", &graphs);
        output.set("response", raw_responses.join("\n\n"));
        output.set("prompt_name", prompt_name);
        output.set("model", self.model.clone());
        output.set("degraded_count", degraded_count);
        output.set("block_count", chunks.len());

        tracing::info!(task = "pipeline", run = "live", file = %ctx.file_path.display(), chunk = "all", attempt = 0u32, provider = "siliconflow", model = %self.model, elapsed_ms = file_started.elapsed().as_millis(), total_ms = file_started.elapsed().as_millis(), stage = "file_done", degraded = degraded_count > 0, "LLM file_done");
        Ok(output)
    }

    /// 一个块的 LLM 调用 + JSON 解析。解析失败时带 JSON 修正提示重试一次；
    /// 第二次失败（或对话错误）将该块降级（§5.5）。返回最终尝试的原始响应
    /// （若有）与结果图。
    async fn extract_block(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        doc_id: &str,
        block_index: u32,
        prompt_name: &str,
    ) -> (Option<String>, ExtractedGraph) {
        let parse = |raw: &str| -> Result<ExtractedGraph, serde_json::Error> {
            if prompt_name == "nacos_config" {
                parse_nacos_block_response(raw, doc_id, block_index)
            } else {
                parse_block_response(raw, doc_id, block_index)
            }
        };

        let raw = match self.chat_content(system_prompt, user_prompt).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("块 {block_index} LLM 调用失败, 降级: {e}");
                return (None, degraded_graph(doc_id, block_index));
            }
        };
        match parse(&raw) {
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
        match parse(&retry_raw) {
            Ok(g) => (Some(retry_raw), g),
            Err(e) => {
                tracing::warn!("块 {block_index} 重试后仍无法解析, 降级: {e}");
                (Some(retry_raw), degraded_graph(doc_id, block_index))
            }
        }
    }

    /// 调用对话端点并提取第一个 choice 的内容。
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

/// 根据流水线上下文中可用的上游输出选择最合适的提示词名。
fn select_prompt(ctx: &PipelineContext) -> String {
    // G3: Nacos 来源（source_kind == Nacos 或虚拟路径以 dt://nacos/ 开头）
    // 路由到 F4 nacos_config 词表。优先级最高——Nacos 配置即使带 chunk 输出
    // 也绝不能用 document_with_nlp（实测 12 实体全为 Config 类型）。
    if ctx.source_kind == FileSourceKind::Nacos
        || ctx.file_path.to_string_lossy().starts_with("dt://nacos/")
    {
        return "nacos_config".to_string();
    }
    if ctx.outputs.contains_key("tree_sitter") {
        "code_with_ast".to_string()
    } else if ctx.outputs.contains_key("chunk") {
        "document_with_nlp".to_string()
    } else {
        "raw_text".to_string()
    }
}

/// 为旧式单次调用路径（`code_with_ast` / `raw_text`）构建渲染上下文。
fn build_render_context(ctx: &PipelineContext) -> serde_json::Value {
    serde_json::json!({
        "file_path": ctx.file_path.to_string_lossy(),
        "project_name": ctx.project_name,
        "file_text": ctx.file_text,
    })
}

/// 为单个块（§5.2）构建渲染上下文：扁平键
/// `file_path` / `file_text`（块文本）/ `entities` / `keywords`。
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

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::knowledge::extract::{EntityType, ExtractedGraph};
    use crate::application::pipeline::infer_client::{ChatResponse, Choice, Message};
    use crate::application::pipeline::output::ProcessorOutput;
    use crate::application::pipeline::FileSourceKind;

    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
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
                .expect("mock 脚本已耗尽")?;
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

    // ── 辅助 ───────────────────────────────────────────────────────

    fn test_registry() -> Arc<PromptRegistry> {
        Arc::new(PromptRegistry::load_default().expect("config/prompts 必须能加载"))
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
            FileSourceKind::Fs,
            None,
            None,
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

    const VALID_JSON_0: &str = r#"{"block_summary":"块0概述","entities":[{"mention":"支付网关服务","canonical_name":"支付网关","type":"Service","summary":"处理支付路由","keywords":["支付"]}],"relations":[]}"#;
    const VALID_JSON_1: &str = r#"{"block_summary":"块1概述","entities":[],"relations":[]}"#;

    // ── 提示词选择 ──────────────────────────────────────────────

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

    /// G3: Nacos 来源（source_kind == Nacos）路由到 nacos_config 词表，
    /// 即使带 chunk 输出也优先于 document_with_nlp。
    #[test]
    fn nacos_source_routes_to_nacos_config() {
        let chunk_out = ProcessorOutput::new();
        let mut ctx = make_context(
            "dt://nacos/prod/application.yaml",
            "server.port: 8080",
            vec![("chunk", chunk_out)],
        );
        // 手动改为 Nacos 来源
        ctx.source_kind = FileSourceKind::Nacos;
        assert_eq!(select_prompt(&ctx), "nacos_config");
    }

    /// G3: 虚拟路径以 dt://nacos/ 开头即路由（兼容 source_kind 未正确传播的场景）。
    #[test]
    fn nacos_virtual_path_routes_to_nacos_config() {
        let chunk_out = ProcessorOutput::new();
        let ctx = make_context(
            "dt://nacos/prod/application.yaml",
            "server.port: 8080",
            vec![("chunk", chunk_out)],
        );
        assert_eq!(select_prompt(&ctx), "nacos_config");
    }

    /// G3: Fs 来源 + 非 nacos 路径不受影响（回归保护）。
    #[test]
    fn fs_yaml_still_uses_document_with_nlp() {
        let chunk_out = ProcessorOutput::new();
        let ctx = make_context(
            "config/application.yaml",
            "a: b",
            vec![("chunk", chunk_out)],
        );
        assert_eq!(select_prompt(&ctx), "document_with_nlp");
    }

    /// G3: Jenkins 虚拟路径不误路由到 nacos_config。
    #[test]
    fn jenkins_path_does_not_route_to_nacos() {
        let ctx = make_context("dt://jenkins/order-service-deploy", "build log", vec![]);
        assert_eq!(select_prompt(&ctx), "raw_text");
    }

    // ── 渲染上下文 ────────────────────────────────────────────────

    #[test]
    fn build_render_context_contains_file_info() {
        let ctx = make_context("src/lib.rs", "pub fn foo() {}", vec![]);
        let json = build_render_context(&ctx);
        assert_eq!(json["file_path"], "src/lib.rs");
        assert_eq!(json["file_text"], "pub fn foo() {}");
        assert_eq!(json["project_name"], "test");
    }

    // ── 块级提取（document_with_nlp 路径） ─────────────────────

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

        // 每个 chunk 一次串行 LLM 调用。
        assert_eq!(mock.calls().len(), 2);
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
        // 重试携带"仅输出 JSON"修正提示。
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
        // 最后一次原始响应保留在旧的 "response" 字段中。
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

    // ── 旧式单次调用路径（不变） ──────────────────────────

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

    // ── G3: nacos_config 块级提取端到端 ─────────────────────────

    /// Nacos 配置经块级提取应产出 F4 词表类型（NacosConfig/ConfigKey/...），
    /// 不归一为 Other、无词表外 WARN。prompt_name 与解析均走 nacos_config。
    #[tokio::test]
    async fn nacos_block_extraction_uses_f4_vocabulary() {
        let chunk_out = make_chunk_output(&[(
            0,
            "server.port: 8080\nspring.datasource.url: jdbc:mysql://db",
        )]);
        let mut ctx = make_context(
            "dt://nacos/prod/application.yaml",
            "server.port: 8080",
            vec![("chunk", chunk_out)],
        );
        ctx.source_kind = FileSourceKind::Nacos;

        let nacos_json = r#"{
            "summary": "应用配置",
            "entities": [
                {"name": "application.yaml", "type": "NacosConfig", "purpose": "完整配置文件"},
                {"name": "server.port", "type": "ConfigKey", "purpose": "服务端口"},
                {"name": "order_db", "type": "Database", "purpose": "订单库连接"}
            ],
            "relations": [
                {"from": "application.yaml", "to": "order_db", "type": "CONTAINS", "evidence": "spring.datasource.url"}
            ]
        }"#;
        let mock = Arc::new(MockChatClient::new(vec![Ok(nacos_json.to_string())]));
        let processor = make_processor(mock.clone());

        let out = processor.execute(&ctx).await.unwrap();

        assert_eq!(
            out.get("prompt_name").and_then(|v| v.as_str()),
            Some("nacos_config")
        );
        assert_eq!(out.get("block_count").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(out.get("degraded_count").and_then(|v| v.as_u64()), Some(0));
        // 渲染使用 nacos_config 模板（system prompt 含词表说明）
        assert!(mock.calls()[0].0.contains("Nacos 配置分析助手"));

        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(out.get("graphs").unwrap().clone()).unwrap();
        assert_eq!(graphs.len(), 1);
        let types: Vec<EntityType> = graphs[0].entities.iter().map(|e| e.entity_type).collect();
        assert_eq!(
            types,
            vec![
                EntityType::NacosConfig,
                EntityType::ConfigKey,
                EntityType::Database
            ]
        );
        assert_eq!(graphs[0].entities[0].canonical_name, "application.yaml");
        assert_eq!(graphs[0].relations[0].relation, "CONTAINS");
        assert!(!graphs[0].degraded);
        // 单次 LLM 调用（1 chunk）
        assert_eq!(mock.calls().len(), 1);
    }
}
