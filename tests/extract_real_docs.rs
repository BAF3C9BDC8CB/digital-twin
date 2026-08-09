//! Real-LLM quantified validation for the Extract layer (Task 1 / 方案 §11 S1).
//!
//! Runs the block-level extraction path (chunk → llm processors) against the
//! real documents in `test/fixtures/knowledge/` with a real chat model, then
//! measures the three acceptance metrics from the task brief:
//!
//! 1. JSON parse success rate  ≥ 90% (one retry included; denominator = total
//!    blocks; degraded blocks count as failures)
//! 2. Relation head/tail coverage in the block's entities ≥ 95%
//! 3. Prints an evenly-sampled list of 20 entities for manual accuracy review
//!    (target ≥ 80% — the review itself is done by a human/agent, recorded in
//!    the task report)
//!
//! This test is `#[ignore]` by default — `cargo test` never calls a real LLM.
//!
//! Run (local xinference, default):
//!   cargo test --test extract_real_docs -- --ignored --nocapture
//!
//! Optional SiliconFlow control group:
//!   SILICONFLOW_API_KEY=sk-... EXTRACT_PROVIDER=siliconflow \
//!   EXTRACT_MODEL=Qwen/Qwen2.5-14B-Instruct \
//!   cargo test --test extract_real_docs -- --ignored --nocapture
//!
//! Environment:
//!   EXTRACT_PROVIDER  "xinference" (default) | "siliconflow"
//!   XINFERENCE_URL    default "http://localhost:9997/v1"
//!   EXTRACT_MODEL     default "qwen3.5" (xinference) — required for siliconflow

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dt_daemon::application::knowledge::extract::ExtractedGraph;
use dt_daemon::application::pipeline::config::LlmConfig;
use dt_daemon::application::pipeline::context::PipelineContext;
use dt_daemon::application::pipeline::infer_client::{
    ChatClient, SiliconFlowChatClient, XInferenceChatClient,
};
use dt_daemon::application::pipeline::processor::Processor;
use dt_daemon::application::pipeline::processors::{ChunkProcessor, LlmClientProcessor};
use dt_daemon::application::pipeline::prompt::PromptRegistry;
use dt_daemon::application::pipeline::FileSourceKind;

const FIXTURE_DIR: &str = "test/fixtures/knowledge";

fn fixture_docs() -> Vec<PathBuf> {
    let mut docs: Vec<PathBuf> = std::fs::read_dir(FIXTURE_DIR)
        .unwrap_or_else(|e| panic!("无法读取 {FIXTURE_DIR}: {e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    docs.sort();
    docs
}

fn build_client() -> (Arc<dyn ChatClient>, String) {
    match std::env::var("EXTRACT_PROVIDER").as_deref() {
        Ok("siliconflow") => {
            let model = std::env::var("EXTRACT_MODEL")
                .unwrap_or_else(|_| "Qwen/Qwen2.5-14B-Instruct".to_string());
            (
                Arc::new(SiliconFlowChatClient::new(String::new(), String::new(), 4)),
                model,
            )
        }
        _ => {
            let url = std::env::var("XINFERENCE_URL")
                .unwrap_or_else(|_| "http://localhost:9997/v1".to_string());
            let model = std::env::var("EXTRACT_MODEL").unwrap_or_else(|_| "qwen3.5".to_string());
            (
                Arc::new(XInferenceChatClient::new(url, String::new(), 4)),
                model,
            )
        }
    }
}

#[tokio::test]
#[ignore = "calls a real LLM — run explicitly"]
async fn extract_real_docs_meets_quality_gates() {
    let docs = fixture_docs();
    assert!(
        docs.len() >= 5,
        "任务书要求至少 5 篇真实文档，实际发现 {}",
        docs.len()
    );
    println!("== documents ({}) ==", docs.len());
    for d in &docs {
        println!("  {}", d.display());
    }

    let (client, model) = build_client();
    let healthy = client.health_check().await.unwrap_or(false);
    assert!(healthy, "LLM 端点不可达 — 请先启动模型服务");
    println!("== provider healthy, model: {model} ==");

    let registry = Arc::new(
        PromptRegistry::load(Path::new("config/prompts")).expect("config/prompts 必须加载成功"),
    );
    let llm = LlmClientProcessor::new(client, model, registry, LlmConfig::default());
    let chunk = ChunkProcessor::default();

    let mut total_blocks = 0usize;
    let mut degraded_blocks = 0usize;
    let mut total_endpoints = 0usize;
    let mut covered_endpoints = 0usize;
    let mut review_pool: Vec<(String, u32, String, String, String)> = Vec::new();

    for doc in &docs {
        let file_name = doc
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let text = std::fs::read_to_string(doc)
            .unwrap_or_else(|e| panic!("无法读取 {}: {e}", doc.display()));

        let mut ctx = PipelineContext::new(
            doc.clone(),
            text,
            "knowledge-fixtures".to_string(),
            FileSourceKind::Fs,
            None, // mtime：本测试无增量对比需求
            None, // content_hash：本测试无增量对比需求
        );
        let chunk_out = chunk.execute(&ctx).await.expect("分块处理器执行失败");
        ctx.add_output("chunk", chunk_out);

        let out = llm.execute(&ctx).await.expect("LLM 处理器执行失败");
        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(out.get("graphs").expect("缺少 graphs 字段").clone())
                .expect("graphs 反序列化失败");

        let file_degraded = graphs.iter().filter(|g| g.degraded).count();
        println!(
            "-- {file_name}: {} blocks, {file_degraded} degraded",
            graphs.len()
        );

        total_blocks += graphs.len();
        degraded_blocks += file_degraded;

        for g in &graphs {
            let names: HashSet<&str> = g
                .entities
                .iter()
                .map(|e| e.canonical_name.as_str())
                .collect();
            for r in &g.relations {
                total_endpoints += 2;
                if names.contains(r.head.as_str()) {
                    covered_endpoints += 1;
                }
                if names.contains(r.tail.as_str()) {
                    covered_endpoints += 1;
                }
            }
            for e in &g.entities {
                review_pool.push((
                    file_name.clone(),
                    g.block_index,
                    e.mention.clone(),
                    e.canonical_name.clone(),
                    format!("{:?}: {}", e.entity_type, e.summary),
                ));
            }
        }
    }

    let parse_success = if total_blocks == 0 {
        0.0
    } else {
        1.0 - degraded_blocks as f64 / total_blocks as f64
    };
    let coverage = if total_endpoints == 0 {
        1.0
    } else {
        covered_endpoints as f64 / total_endpoints as f64
    };

    println!("\n== metrics ==");
    println!(
        "{}",
        serde_json::json!({
            "documents": docs.len(),
            "total_blocks": total_blocks,
            "degraded_blocks": degraded_blocks,
            "parse_success_rate": format!("{:.1}%", parse_success * 100.0),
            "relation_endpoints": total_endpoints,
            "covered_endpoints": covered_endpoints,
            "head_tail_coverage": format!("{:.1}%", coverage * 100.0),
        })
    );

    // Metric ③: evenly sample up to 20 entities across all documents for
    // manual accuracy review against the source text.
    println!("\n== entity review sample (manual check target: >=80% accurate) ==");
    let sample_n = 20.min(review_pool.len());
    let step = (review_pool.len() as f64 / sample_n.max(1) as f64).max(1.0);
    for i in 0..sample_n {
        let idx = (i as f64 * step) as usize;
        let (file, block, mention, canonical, desc) = &review_pool[idx.min(review_pool.len() - 1)];
        println!(
            "  [{:>2}] {}#{} | mention='{}' | canonical='{}' | {}",
            i + 1,
            file,
            block,
            mention,
            canonical,
            desc
        );
    }

    assert!(
        parse_success >= 0.90,
        "指标 1 未达标：解析成功率 {:.1}% < 90%",
        parse_success * 100.0
    );
    assert!(
        coverage >= 0.95,
        "指标 2 未达标：头尾实体覆盖率 {:.1}% < 95%",
        coverage * 100.0
    );
}
