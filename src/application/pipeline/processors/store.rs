//! Store processor — persists pipeline results to Memgraph (graph database)
//! and Qdrant (vector database).
//!
//! This is the final stage in the processing chain and always runs (its
//! `matches()` returns `true` for every path).  It collects entities from
//! upstream processors (tree_sitter, hanlp, llm), writes structured data
//! to Memgraph via Cypher queries, generates embeddings, and upserts
//! vectors into Qdrant.
//!
//! All three backing stores are optional — the processor degrades
//! gracefully when a store is not configured.
//!
//! Produces a [`ProcessorOutput`] with:
//! - `"graph_nodes"`   — number of nodes written to Memgraph
//! - `"vector_points"` — number of points upserted into Qdrant
//! - `"errors"`        — list of non-fatal error messages encountered
//!   during storage

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

/// Final pipeline stage that persists processed data.
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
        // Always runs — last in the chain.
        true
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();
        let mut errors: Vec<String> = Vec::new();

        // ── Step 1: Collect entities from upstream processors ──────────
        let entities = collect_entities(ctx);

        // ── Step 2: Write to Memgraph ──────────────────────────────────
        let graph_nodes = if let Some(ref graph) = self.graph {
            match write_to_graph(graph, &entities, ctx).await {
                Ok(count) => count,
                Err(e) => {
                    errors.push(format!("graph write failed: {e}"));
                    0
                }
            }
        } else {
            0
        };
        output.set("graph_nodes", graph_nodes);

        // ── Step 3: Generate embeddings and write to Qdrant ───────────
        let vector_points = if let (Some(ref embed), Some(ref vector)) = (&self.embed, &self.vector)
        {
            match write_to_vector(embed, vector, &entities, ctx).await {
                Ok(count) => count,
                Err(e) => {
                    errors.push(format!("vector write failed: {e}"));
                    0
                }
            }
        } else {
            0
        };
        output.set("vector_points", vector_points);

        output.set("errors", errors);
        output.set("entity_count", entities.len());

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Helper: entity collection
// ---------------------------------------------------------------------------

/// A unified entity descriptor collected from upstream processors.
#[derive(Debug, Clone, serde::Serialize)]
struct CollectedEntity {
    /// Source processor name (e.g. "tree_sitter", "hanlp", "llm").
    source: String,
    /// Entity type label (e.g. "class", "method", "keyword", "ner").
    entity_type: String,
    /// Entity name or identifier.
    name: String,
    /// Optional human-readable description.
    description: String,
    /// Optional file path the entity was extracted from.
    file_path: String,
    /// Full text representation for embedding.
    text_for_embedding: String,
}

/// Walk the context's processor outputs and collect all entities into a
/// flat list for storage.
fn collect_entities(ctx: &PipelineContext) -> Vec<CollectedEntity> {
    let mut entities: Vec<CollectedEntity> = Vec::new();

    // Entities from tree_sitter
    if let Some(ts_out) = ctx.outputs.get("tree_sitter") {
        if let Some(entities_val) = ts_out.get("entities") {
            // Methods
            if let Some(methods) = entities_val.get("methods").and_then(|v| v.as_array()) {
                for m in methods {
                    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let sig = m.get("signature").and_then(|v| v.as_str()).unwrap_or("");
                    entities.push(CollectedEntity {
                        source: "tree_sitter".into(),
                        entity_type: "method".into(),
                        name: name.to_string(),
                        description: sig.to_string(),
                        file_path: m
                            .get("file_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        text_for_embedding: format!("{}: {}", name, sig),
                    });
                }
            }
            // Classes
            if let Some(classes) = entities_val.get("classes").and_then(|v| v.as_array()) {
                for c in classes {
                    let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    entities.push(CollectedEntity {
                        source: "tree_sitter".into(),
                        entity_type: "class".into(),
                        name: name.to_string(),
                        description: c
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("class")
                            .to_string(),
                        file_path: c
                            .get("file_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        text_for_embedding: format!(
                            "{}: {}",
                            c.get("kind")
                                .and_then(|v| v.as_str())
                                .unwrap_or("class"),
                            name
                        ),
                    });
                }
            }
        }
    }

    // Entities from hanlp / NLP
    if let Some(hanlp_out) = ctx.outputs.get("hanlp") {
        if let Some(entities_val) = hanlp_out.get("entities").and_then(|v| v.as_array()) {
            for e in entities_val {
                let text = e.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let tag = e.get("tag").and_then(|v| v.as_str()).unwrap_or("");
                entities.push(CollectedEntity {
                    source: "hanlp".into(),
                    entity_type: "ner".into(),
                    name: text.to_string(),
                    description: tag.to_string(),
                    file_path: ctx.file_path.to_string_lossy().to_string(),
                    text_for_embedding: format!("NER[{}]: {}", tag, text),
                });
            }
        }
        if let Some(keywords) = hanlp_out.get("keywords").and_then(|v| v.as_array()) {
            for kw in keywords {
                if let Some(text) = kw.as_str() {
                    entities.push(CollectedEntity {
                        source: "hanlp".into(),
                        entity_type: "keyword".into(),
                        name: text.to_string(),
                        description: String::new(),
                        file_path: ctx.file_path.to_string_lossy().to_string(),
                        text_for_embedding: format!("keyword: {}", text),
                    });
                }
            }
        }
    }

    // Entities from LLM — treat the full response as a single entity
    if let Some(llm_out) = ctx.outputs.get("llm") {
        if let Some(response) = llm_out.get("response").and_then(|v| v.as_str()) {
            if !response.is_empty() {
                entities.push(CollectedEntity {
                    source: "llm".into(),
                    entity_type: "analysis".into(),
                    name: format!("llm_analysis_{}", ctx.file_path.to_string_lossy()),
                    description: String::new(),
                    file_path: ctx.file_path.to_string_lossy().to_string(),
                    text_for_embedding: response.to_string(),
                });
            }
        }
    }

    entities
}

// ---------------------------------------------------------------------------
// Helper: write to Memgraph
// ---------------------------------------------------------------------------

/// Write collected entities to Memgraph via Cypher queries.
///
/// Returns the number of nodes created.
async fn write_to_graph(
    graph: &Arc<dyn GraphRepository>,
    entities: &[CollectedEntity],
    ctx: &PipelineContext,
) -> Result<usize, DtError> {
    if entities.is_empty() {
        return Ok(0);
    }

    let mut count = 0usize;

    for entity in entities {
        let query = concat!(
            "MERGE (n:Entity {name: $name, file_path: $file_path, project: $project}) ",
            "SET n.source = $source, n.entity_type = $entity_type, ",
            "n.description = $description, n.text_for_embedding = $text_for_embedding, ",
            "n.pipeline_run = timestamp() ",
            "RETURN n.name"
        );

        let mut params: HashMap<String, serde_json::Value> = HashMap::new();
        params.insert("name".into(), serde_json::Value::String(entity.name.clone()));
        params.insert("file_path".into(), serde_json::Value::String(entity.file_path.clone()));
        params.insert(
            "project".into(),
            serde_json::Value::String(ctx.project_name.clone()),
        );
        params.insert(
            "source".into(),
            serde_json::Value::String(entity.source.clone()),
        );
        params.insert(
            "entity_type".into(),
            serde_json::Value::String(entity.entity_type.clone()),
        );
        params.insert(
            "description".into(),
            serde_json::Value::String(entity.description.clone()),
        );
        params.insert(
            "text_for_embedding".into(),
            serde_json::Value::String(entity.text_for_embedding.clone()),
        );

        graph.as_ref().write_query(query, params).await?;
        count += 1;
    }

    Ok(count)
}

// ---------------------------------------------------------------------------
// Helper: write to Qdrant via embedding
// ---------------------------------------------------------------------------

/// Generate embeddings for each entity and upsert them into Qdrant.
///
/// Returns the number of points upserted.
async fn write_to_vector(
    embed: &Arc<dyn EmbedService>,
    vector: &Arc<dyn VectorRepository>,
    entities: &[CollectedEntity],
    ctx: &PipelineContext,
) -> Result<usize, DtError> {
    if entities.is_empty() {
        return Ok(0);
    }

    let collection = format!("{}_entities", ctx.project_name);
    let dim = 1024; // BGE-M3 default dimension — may change in future.

    // Ensure the collection exists.
    vector.as_ref().ensure_collection(&collection, dim).await?;

    // Collect texts to embed.
    let texts: Vec<String> = entities.iter().map(|e| e.text_for_embedding.clone()).collect();

    // Generate embeddings.
    let embeddings = embed.as_ref().embed_batch(&texts).await?;

    // Build points.
    let points: Vec<serde_json::Value> = entities
        .iter()
        .zip(embeddings.iter())
        .enumerate()
        .map(|(idx, (entity, emb))| {
            serde_json::json!({
                "id": idx as u64,
                "vector": emb,
                "payload": {
                    // ---- identity ----
                    "name": entity.name,
                    "entity_type": entity.entity_type,
                    // ---- source ----
                    "file_path": entity.file_path,
                    "project": ctx.project_name,
                    "source": entity.source,
                    // ---- content ----
                    "text": entity.text_for_embedding,
                }
            })
        })
        .collect();

    // Upsert in batches.
    const BATCH_SIZE: usize = 100;
    let mut upserted = 0usize;

    for batch in points.chunks(BATCH_SIZE) {
        vector
            .as_ref()
            .upsert(&collection, batch.to_vec())
            .await
            .map_err(|e| DtError::General(format!("vector upsert batch failed: {e}")))?;
        upserted += batch.len();
    }

    Ok(upserted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::pipeline::output::ProcessorOutput;
    use std::path::PathBuf;

    fn make_context(file_name: &str, text: &str, outputs: Vec<(&str, ProcessorOutput)>) -> PipelineContext {
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

    #[tokio::test]
    async fn always_matches() {
        let processor = StoreProcessor::new(None, None, None);
        assert!(processor.matches(Path::new("anything.bin")));
        assert!(processor.matches(Path::new("no_extension")));
        assert!(processor.matches(Path::new("")));
    }

    #[tokio::test]
    async fn empty_when_no_repos() {
        let processor = StoreProcessor::new(None, None, None);
        let ctx = make_context("main.rs", "fn main() {}", vec![]);
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.get("graph_nodes").and_then(|v| v.as_u64()), Some(0));
        assert_eq!(
            output.get("vector_points").and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(output.get("entity_count").and_then(|v| v.as_u64()), Some(0));
    }

    #[tokio::test]
    async fn collects_entities_from_tree_sitter_output() {
        let processor = StoreProcessor::new(None, None, None);
        let mut ts_out = ProcessorOutput::new();
        ts_out.set(
            "entities",
            serde_json::json!({
                "classes": [{"name": "Foo", "kind": "Class", "file_path": "/test/Foo.java"}],
                "methods": [{"name": "bar", "signature": "fn bar()", "file_path": "/test/Foo.java"}],
            }),
        );
        let ctx = make_context("Foo.java", "class Foo {}", vec![("tree_sitter", ts_out)]);
        let entities = collect_entities(&ctx);
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].source, "tree_sitter");
        assert_eq!(entities[0].entity_type, "method");
        assert_eq!(entities[0].name, "bar");
        assert_eq!(entities[1].entity_type, "class");
        assert_eq!(entities[1].name, "Foo");
    }

    #[tokio::test]
    async fn handles_no_upstream_outputs() {
        let processor = StoreProcessor::new(None, None, None);
        let ctx = make_context("main.rs", "fn main() {}", vec![]);
        let entities = collect_entities(&ctx);
        assert!(entities.is_empty());
    }

    #[tokio::test]
    async fn name_and_priority() {
        let processor = StoreProcessor::new(None, None, None);
        assert_eq!(processor.name(), "store");
        assert_eq!(processor.priority(), 10);
    }

    #[test]
    fn collected_entity_is_serializable() {
        let e = CollectedEntity {
            source: "test".into(),
            entity_type: "method".into(),
            name: "foo".into(),
            description: "does stuff".into(),
            file_path: "/tmp/test.rs".into(),
            text_for_embedding: "foo: does stuff".into(),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["name"], "foo");
        assert_eq!(json["source"], "test");
    }
}
