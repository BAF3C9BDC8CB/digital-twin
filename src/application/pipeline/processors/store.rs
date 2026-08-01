//! Store processor — the Consolidate 整合层 entry point in the pipeline
//! (方案 §6, Task 2/R8).
//!
//! Thin shell: consumes `outputs["llm"]["graphs"]` (`Vec<ExtractedGraph>`,
//! Task 1) and `outputs["chunk"]` (block texts), then delegates to
//! [`Consolidator`] for normalisation, two-level disambiguation, graph
//! writes and dual vector writes. Files without a `graphs` output (code
//! files, raw-text path) are skipped untouched.
//!
//! All three backing stores are optional — without them the processor is a
//! no-op so the pipeline still runs in degraded environments.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::application::knowledge::extract::{Consolidator, ExtractedGraph};
use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

/// Final pipeline stage that persists extracted knowledge.
///
/// # Dependency injection
///
/// All three repository fields are `Option` so the processor can be
/// constructed even when the backing services are not available.
pub struct StoreProcessor {
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
}

impl StoreProcessor {
    /// Create a new store processor with optional backing stores.
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
        }
    }

    /// Build a `StoreProcessor` with only a graph repository.
    pub fn with_graph(graph: Arc<dyn GraphRepository>) -> Self {
        Self {
            graph: Some(graph),
            vector: None,
            embed: None,
        }
    }

    /// Build a `StoreProcessor` with graph and vector repositories
    /// and an embed service.
    pub fn with_all(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
    ) -> Self {
        Self {
            graph: Some(graph),
            vector: Some(vector),
            embed: Some(embed),
        }
    }
}

#[async_trait]
impl Processor for StoreProcessor {
    fn name(&self) -> &str {
        "store"
    }

    fn priority(&self) -> i32 {
        10
    }

    fn matches(&self, _file_path: &Path) -> bool {
        // Always runs — last in the chain; skips files without graphs.
        true
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        // ── R8: files without a graphs output are skipped untouched. ──
        let Some(llm_out) = ctx.outputs.get("llm") else {
            return Ok(output);
        };
        let Some(graphs_val) = llm_out.get("graphs") else {
            return Ok(output);
        };
        let graphs: Vec<ExtractedGraph> =
            serde_json::from_value(graphs_val.clone()).map_err(|e| {
                DtError::General(format!("store: llm graphs output contract broken: {e}"))
            })?;

        // All three backends are required for consolidation.
        let (Some(graph), Some(vector), Some(embed)) = (&self.graph, &self.vector, &self.embed)
        else {
            tracing::warn!("store: 后端未齐备（graph/vector/embed），跳过 consolidate");
            return Ok(output);
        };

        // ── Block texts + doc identity from the chunk processor output. ──
        let chunk_out = ctx.outputs.get("chunk").ok_or_else(|| {
            DtError::General("store: graphs present without chunk output".to_string())
        })?;
        let doc_id = chunk_out
            .get("doc_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DtError::General("store: chunk output missing doc_id".to_string()))?
            .to_string();
        let doc_type = chunk_out
            .get("doc_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let mut block_texts: HashMap<u32, String> = HashMap::new();
        if let Some(chunks) = chunk_out.get("chunks").and_then(|v| v.as_array()) {
            for chunk in chunks {
                let index = chunk.get("chunk_index").and_then(|v| v.as_u64());
                let text = chunk.get("text").and_then(|v| v.as_str());
                if let (Some(index), Some(text)) = (index, text) {
                    block_texts.insert(index as u32, text.to_string());
                }
            }
        }

        let consolidator = Consolidator::new(graph.clone(), vector.clone(), embed.clone());
        let stats = consolidator
            .consolidate_document(
                &ctx.project_name,
                &doc_id,
                &ctx.file_path.to_string_lossy(),
                &doc_type,
                &graphs,
                &block_texts,
            )
            .await?;

        // ── R9: counters for the engine's build report. ──
        output.set("entities_merged", stats.entities_merged);
        output.set("entities_created", stats.entities_created);
        output.set("relations_written", stats.relations_written);
        output.set("relations_orphaned", stats.relations_orphaned);
        output.set("degraded_blocks", stats.degraded_blocks);
        output.set("blocks_processed", stats.blocks_processed);
        output.set("empty_blocks", stats.empty_blocks);

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::output::ProcessorOutput;
    use crate::domain::types::{CollectionInfo, HealthStatus};
    use std::path::PathBuf;
    use std::sync::Mutex;

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

    fn llm_output_with_graphs(graphs: serde_json::Value) -> ProcessorOutput {
        let mut out = ProcessorOutput::new();
        out.set("graphs", graphs);
        out
    }

    fn chunk_output() -> ProcessorOutput {
        let mut out = ProcessorOutput::new();
        out.set(
            "doc_id",
            serde_json::Value::String("dt://doc/test/a.md".to_string()),
        );
        out.set(
            "doc_type",
            serde_json::Value::String("markdown".to_string()),
        );
        out.set(
            "chunks",
            serde_json::json!([{"chunk_index": 0, "text": "原文块文本"}]),
        );
        out
    }

    // ── Minimal backend mocks for the happy path ─────────────────────

    struct MockGraph {
        writes: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::json!([]))
        }

        async fn write_query(
            &self,
            q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.writes.lock().unwrap().push(q.to_string());
            if q.contains("RETURN elementId(e)") {
                return Ok(serde_json::json!([{"eid": "4:0:1"}]));
            }
            Ok(serde_json::json!([]))
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    struct MockVector;

    #[async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }

        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(vec![])
        }

        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            Ok(())
        }

        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }

        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }

        async fn collection_info(&self, n: &str) -> Result<CollectionInfo, DtError> {
            Ok(CollectionInfo {
                name: n.to_string(),
                points_count: 0,
                vector_dim: 1024,
                model_version: "bge-m3".into(),
            })
        }

        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    struct MockEmbed;

    #[async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1, 0.2]).collect())
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    // ── Tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn always_matches() {
        let processor = StoreProcessor::new(None, None, None);
        assert!(processor.matches(Path::new("anything.bin")));
        assert!(processor.matches(Path::new("no_extension")));
        assert!(processor.matches(Path::new("")));
    }

    #[tokio::test]
    async fn name_and_priority() {
        let processor = StoreProcessor::new(None, None, None);
        assert_eq!(processor.name(), "store");
        assert_eq!(processor.priority(), 10);
    }

    #[tokio::test]
    async fn skips_code_file_without_graphs_output() {
        let processor = StoreProcessor::new(None, None, None);
        let ctx = make_context("main.rs", "fn main() {}", vec![]);
        let output = processor.execute(&ctx).await.unwrap();
        assert!(output.get("entities_created").is_none());
    }

    #[tokio::test]
    async fn skips_llm_output_without_graphs_key() {
        // Code-file llm output ({response,prompt_name,model}) → skip.
        let processor = StoreProcessor::new(None, None, None);
        let mut llm_out = ProcessorOutput::new();
        llm_out.set(
            "response",
            serde_json::Value::String("analysis".to_string()),
        );
        let ctx = make_context("main.rs", "fn main() {}", vec![("llm", llm_out)]);
        let output = processor.execute(&ctx).await.unwrap();
        assert!(output.get("entities_created").is_none());
    }

    #[tokio::test]
    async fn no_backends_is_a_noop() {
        let processor = StoreProcessor::new(None, None, None);
        let graphs = serde_json::json!([{
            "doc_id": "dt://doc/test/a.md", "block_index": 0,
            "block_summary": "s", "entities": [], "relations": [], "degraded": false
        }]);
        let ctx = make_context(
            "a.md",
            "原文块文本",
            vec![
                ("llm", llm_output_with_graphs(graphs)),
                ("chunk", chunk_output()),
            ],
        );
        let output = processor.execute(&ctx).await.unwrap();
        assert!(output.get("entities_created").is_none());
    }

    #[tokio::test]
    async fn happy_path_consolidates_and_reports_counters() {
        let graph = Arc::new(MockGraph {
            writes: Mutex::new(vec![]),
        });
        let processor =
            StoreProcessor::with_all(graph.clone(), Arc::new(MockVector), Arc::new(MockEmbed));
        let graphs = serde_json::json!([{
            "doc_id": "dt://doc/test/a.md", "block_index": 0,
            "block_summary": "块摘要",
            "entities": [{
                "mention": "支付网关(提及)", "canonical_name": "支付网关",
                "type": "Service", "summary": "路由支付请求",
                "keywords": ["支付"]
            }],
            "relations": [], "degraded": false
        }]);
        let ctx = make_context(
            "a.md",
            "原文块文本",
            vec![
                ("llm", llm_output_with_graphs(graphs)),
                ("chunk", chunk_output()),
            ],
        );
        let output = processor.execute(&ctx).await.unwrap();
        assert_eq!(
            output.get("entities_created").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            output.get("blocks_processed").and_then(|v| v.as_u64()),
            Some(1)
        );
        // Entity MERGE went to the graph backend.
        assert!(graph
            .writes
            .lock()
            .unwrap()
            .iter()
            .any(|q| q.contains("MERGE (e:Entity {entity_id: $entity_id})")));
    }

    #[tokio::test]
    async fn broken_graphs_contract_is_an_error() {
        let processor = StoreProcessor::new(None, None, None);
        let ctx = make_context(
            "a.md",
            "text",
            vec![(
                "llm",
                llm_output_with_graphs(serde_json::json!({"not": "a list"})),
            )],
        );
        assert!(processor.execute(&ctx).await.is_err());
    }
}
