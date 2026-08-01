//! **Retriever Stage** — parallel query of all six knowledge worlds.
//!
//! Each world is queried independently and concurrently:
//!
//! | World     | Source      | Query Type                     |
//! |-----------|-------------|--------------------------------|
//! | Reality   | Qdrant      | Vector search for code/services|
//! | Knowledge | Memgraph    | Cypher for concepts/patterns   |
//! | Memory    | Memgraph    | Cypher for events/experiences  |
//! | Semantic  | Qdrant      | Vector search                  |
//! | Runtime   | K8s API     | Placeholder (real-time)        |
//! | Reasoning | Memgraph    | Cypher for past analyses       |

use async_trait::async_trait;
use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

use super::ContextStage;
use crate::application::context::models::{ContextState, WorldItem};

/// The six world names used throughout the pipeline.
pub const WORLDS: [&str; 6] = [
    "reality",
    "knowledge",
    "memory",
    "semantic",
    "runtime",
    "reasoning",
];

/// Retrieves initial results from all enabled knowledge worlds in parallel.
pub struct RetrieverStage {
    /// Graph repository for Cypher queries.
    graph: Option<Arc<dyn GraphRepository>>,
    /// Qdrant vector repository for semantic search.
    vector: Option<Arc<dyn VectorRepository>>,
    /// Embed service for generating query vectors.
    embed: Option<Arc<dyn EmbedService>>,
}

impl RetrieverStage {
    /// Create a RetrieverStage with all backends.
    pub fn new(
        graph: impl Into<Option<Arc<dyn GraphRepository>>>,
        vector: impl Into<Option<Arc<dyn VectorRepository>>>,
        embed: impl Into<Option<Arc<dyn EmbedService>>>,
    ) -> Self {
        Self {
            graph: graph.into(),
            vector: vector.into(),
            embed: embed.into(),
        }
    }

    /// Create a RetrieverStage with all backends set to `None` (testing / no-op).
    pub fn empty() -> Self {
        Self {
            graph: None,
            vector: None,
            embed: None,
        }
    }
}

impl Default for RetrieverStage {
    fn default() -> Self {
        Self::empty()
    }
}

#[async_trait]
impl ContextStage for RetrieverStage {
    fn name(&self) -> &str {
        "retriever"
    }

    async fn process(&self, mut state: ContextState) -> Result<ContextState, DtError> {
        let enabled_worlds: Vec<&str> = WORLDS
            .iter()
            .copied()
            .filter(|w| state.options.includes_world(w))
            .collect();

        tracing::info!(
            "[retriever] querying {} worlds for task: {}",
            enabled_worlds.len(),
            &state.task[..state.task.len().min(80)]
        );

        // Query worlds sequentially — Bolt connection pool does not work
        // correctly across tokio::spawn boundaries (BoltType params are
        // silently dropped), so we avoid parallel spawning.
        for world in enabled_worlds {
            let task = &state.task;
            let graph: Option<&dyn GraphRepository> = self.graph.as_ref().map(|a| a.as_ref());
            let vector: Option<&dyn VectorRepository> = self.vector.as_ref().map(|a| a.as_ref());
            let embed: Option<&dyn EmbedService> = self.embed.as_ref().map(|a| a.as_ref());

            match query_world(world, task, graph, vector, embed).await {
                Ok(items) => {
                    tracing::info!("[retriever] {world}: {} items retrieved", items.len());
                    match world {
                        "reality" => state.reality_raw = items,
                        "knowledge" => state.knowledge_raw = items,
                        "memory" => state.memory_raw = items,
                        "semantic" => state.semantic_raw = items,
                        "runtime" => state.runtime_raw = items,
                        "reasoning" => state.reasoning_raw = items,
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::warn!("[retriever] {world}: query failed — {e}");
                }
            }
        }

        Ok(state)
    }
}

// ---------------------------------------------------------------------------
// Per-world query logic
// ---------------------------------------------------------------------------

/// Query a single world and return a list of `WorldItem`s.
///
/// When the corresponding repository is `None`, an empty list is returned
/// (graceful degradation).
async fn query_world(
    world: &str,
    task: &str,
    graph: Option<&dyn GraphRepository>,
    vector: Option<&dyn VectorRepository>,
    embed: Option<&dyn EmbedService>,
) -> Result<Vec<WorldItem>, DtError> {
    match world {
        "reality" => query_reality(graph, vector, embed, task).await,
        "knowledge" => query_knowledge(graph, task).await,
        "memory" => query_memory(graph, task).await,
        "semantic" => query_semantic(vector, embed, task).await,
        "runtime" => query_runtime(task).await,
        "reasoning" => query_reasoning(graph, task).await,
        _ => Ok(Vec::new()),
    }
}

// ── Reality World — code, services, configurations ──────────────────────────

async fn query_reality(
    graph: Option<&dyn GraphRepository>,
    vector: Option<&dyn VectorRepository>,
    embed: Option<&dyn EmbedService>,
    task: &str,
) -> Result<Vec<WorldItem>, DtError> {
    let (Some(vector), Some(embed)) = (vector, embed) else {
        return Ok(Vec::new());
    };

    // Generate embedding for the task text
    let embeddings = embed.embed_batch(&[task.to_string()]).await?;
    let Some(query_vector) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    // Search Qdrant for semantically similar code/infra entities
    let results = vector.search("kg_nodes", query_vector, 30).await?;

    let items: Vec<WorldItem> = results
        .into_iter()
        .filter(|hit| {
            let labels = hit["payload"]["labels"].as_array();
            labels.map_or(true, |l| {
                l.iter().any(|v| {
                    v.as_str().map_or(false, |s| {
                        matches!(
                            s,
                            "Method"
                                | "Class"
                                | "Module"
                                | "Server"
                                | "Database"
                                | "K8sDeployment"
                                | "K8sService"
                                | "Service"
                                | "ServiceInstance"
                                | "NacosConfig"
                                | "Endpoint"
                                | "ConfigKey"
                        )
                    })
                })
            })
        })
        .map(|hit| {
            let id = hit["id"].as_str().unwrap_or("?").to_string();
            let payload = &hit["payload"];
            let name = payload["name"].as_str().unwrap_or("?").to_string();
            let desc = payload["description"].as_str().unwrap_or("").to_string();
            let labels = payload["labels"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let score = hit["score"].as_f64().unwrap_or(0.0);

            WorldItem::new(&id, name, desc)
                .with_score(score)
                .with_source("reality")
                .with_type(labels)
        })
        .collect();

    Ok(items)
}

// ── Knowledge World — concepts, patterns, playbooks ─────────────────────────

async fn query_knowledge(
    graph: Option<&dyn GraphRepository>,
    task: &str,
) -> Result<Vec<WorldItem>, DtError> {
    let Some(graph) = graph else {
        return Ok(Vec::new());
    };

    // Search for domain Concepts, Playbooks, Knowledge nodes.
    // Uses both full-task fragment AND tokenized keywords for better recall.
    let cypher = r#"
        MATCH (n)
        WHERE labels(n)[0] IN ['Concept', 'Playbook', 'Knowledge', 'Domain']
          AND (
            coalesce(n.name, '') CONTAINS $task_fragment
            OR coalesce(n.title, '') CONTAINS $task_fragment
            OR coalesce(n.description, '') CONTAINS $task_fragment
            OR coalesce(n.definition, '') CONTAINS $task_fragment
            OR coalesce(n.summary, '') CONTAINS $task_fragment
            OR any(kw IN $keywords WHERE
              coalesce(n.name, '') CONTAINS kw
              OR coalesce(n.title, '') CONTAINS kw
              OR coalesce(n.description, '') CONTAINS kw
              OR coalesce(n.definition, '') CONTAINS kw
              OR coalesce(n.summary, '') CONTAINS kw
            )
          )
        RETURN n.name AS name,
               labels(n)[0] AS type,
               coalesce(n.description, n.definition, n.summary, '') AS description,
               n.domain AS domain
        LIMIT 20
    "#;

    let keywords = tokenize(task);
    let mut params = std::collections::HashMap::new();
    params.insert(
        "task_fragment".to_string(),
        serde_json::Value::String(task.to_string()),
    );
    params.insert(
        "keywords".to_string(),
        serde_json::Value::Array(
            keywords
                .into_iter()
                .map(|k| serde_json::Value::String(k))
                .collect(),
        ),
    );

    let result = graph.read_query(cypher, params).await?;
    Ok(parse_graph_search_results(&result, "knowledge"))
}

// ── Memory World — events, experiences, lessons ─────────────────────────────

async fn query_memory(
    graph: Option<&dyn GraphRepository>,
    task: &str,
) -> Result<Vec<WorldItem>, DtError> {
    let Some(graph) = graph else {
        return Ok(Vec::new());
    };

    // Search for historical Events, Experiences
    let cypher = r#"
        MATCH (n)
        WHERE labels(n)[0] IN ['Experience', 'BugFix', 'Decision', 'Deployment']
           AND (
            coalesce(n.description, '') CONTAINS $task_fragment
            OR coalesce(n.title, '') CONTAINS $task_fragment
            OR coalesce(n.summary, '') CONTAINS $task_fragment
            OR any(kw IN $keywords WHERE
              coalesce(n.description, '') CONTAINS kw
              OR coalesce(n.title, '') CONTAINS kw
              OR coalesce(n.summary, '') CONTAINS kw
            )
          )
        RETURN coalesce(n.title, n.name, '') AS name,
               labels(n)[0] AS type,
               coalesce(n.description, n.summary, '') AS description
        ORDER BY n.created_at DESC
        LIMIT 15
    "#;

    let keywords = tokenize(task);
    let mut params = std::collections::HashMap::new();
    params.insert(
        "task_fragment".to_string(),
        serde_json::Value::String(task.to_string()),
    );
    params.insert(
        "keywords".to_string(),
        serde_json::Value::Array(
            keywords
                .into_iter()
                .map(|k| serde_json::Value::String(k))
                .collect(),
        ),
    );

    let result = graph.read_query(cypher, params).await?;
    Ok(parse_graph_search_results(&result, "memory"))
}

// ── Semantic World — vector search via Qdrant ───────────────────────────────

async fn query_semantic(
    vector: Option<&dyn VectorRepository>,
    embed: Option<&dyn EmbedService>,
    task: &str,
) -> Result<Vec<WorldItem>, DtError> {
    let (Some(vector), Some(embed)) = (vector, embed) else {
        return Ok(Vec::new());
    };

    // Generate embedding for the task text
    let embeddings = embed.embed_batch(&[task.to_string()]).await?;
    let Some(query_vector) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    // Search Qdrant for semantically similar documents
    let results = vector.search("kg_nodes", query_vector, 20).await?;

    let items: Vec<WorldItem> = results
        .into_iter()
        .map(|hit| {
            let id = hit["id"].as_str().unwrap_or("?").to_string();
            let payload = &hit["payload"];
            let name = payload["name"].as_str().unwrap_or("?").to_string();
            let desc = payload["description"].as_str().unwrap_or("").to_string();
            let label = payload["labels"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let score = hit["score"].as_f64().unwrap_or(0.0);

            WorldItem::new(&id, name, desc)
                .with_score(score)
                .with_source("qdrant")
                .with_type(label)
        })
        .collect();

    Ok(items)
}

// ── Runtime World — K8s API (placeholder) ───────────────────────────────────

async fn query_runtime(_task: &str) -> Result<Vec<WorldItem>, DtError> {
    // Placeholder: real K8s API queries will be wired in when the K8s client
    // is available.  For now this world returns empty.
    Ok(Vec::new())
}

// ── Reasoning World — past analysis chains ──────────────────────────────────

async fn query_reasoning(
    graph: Option<&dyn GraphRepository>,
    task: &str,
) -> Result<Vec<WorldItem>, DtError> {
    let Some(graph) = graph else {
        return Ok(Vec::new());
    };

    let keywords = tokenize(task);
    let mut params = std::collections::HashMap::new();
    params.insert(
        "task_fragment".to_string(),
        serde_json::Value::String(task.to_string()),
    );
    params.insert(
        "keywords".to_string(),
        serde_json::Value::Array(
            keywords
                .iter()
                .map(|k| serde_json::Value::String(k.clone()))
                .collect(),
        ),
    );

    let cypher = r#"
        MATCH (n)
        WHERE labels(n)[0] IN ['Thread', 'Decision', 'BugFix', 'Knowledge']
          AND (
            n.title CONTAINS $task_fragment
            OR n.description CONTAINS $task_fragment
            OR n.decision CONTAINS $task_fragment
            OR n.reason CONTAINS $task_fragment
            OR any(kw in $keywords WHERE n.title CONTAINS kw OR n.description CONTAINS kw)
          )
        RETURN coalesce(n.title, n.name, '') AS name,
               labels(n)[0] AS type,
               coalesce(n.description, n.decision, n.reason, '') AS description
        ORDER BY n.created_at DESC
        LIMIT 10
    "#;

    let result = graph.read_query(cypher, params).await?;
    Ok(parse_graph_search_results(&result, "reasoning"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Split a task string into simple keyword tokens for CONTAINS matching.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| w.len() > 2)
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .take(10)
        .collect()
}

/// Parse graph fulltext / MATCH query results into `WorldItem`s.
///
/// Expected row shape: `[name, type, description, ...]`
fn parse_graph_search_results(raw: &serde_json::Value, world: &str) -> Vec<WorldItem> {
    // Bolt driver returns a flat JSON array of objects, e.g.:
    // [{"name": "DaoBase", "type": "Class", "description": "", "score": 4.1}, ...]
    let rows = raw.as_array();
    let Some(rows) = rows else {
        return Vec::new();
    };

    let mut items = Vec::with_capacity(rows.len());

    for (i, obj) in rows.iter().enumerate() {
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let entity_type = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let description = obj
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let score = obj.get("score").and_then(|v| v.as_f64()).unwrap_or(0.5);
        let source_file = obj
            .get("source_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let id = format!("{world}::{i}");

        let mut item = WorldItem::new(id, name, description)
            .with_score(score)
            .with_type(entity_type)
            .with_source(world);

        if !source_file.is_empty() {
            item = item.with_source(source_file);
        }
        items.push(item);
    }

    items
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worlds_constant_has_six_entries() {
        assert_eq!(WORLDS.len(), 6);
    }

    #[test]
    fn tokenize_splits_and_filters() {
        let tokens = tokenize("fix payment service bug timeout");
        assert!(!tokens.is_empty());
        for t in &tokens {
            assert!(t.len() > 2, "token '{t}' too short");
            assert!(
                t.chars().all(|c| c.is_alphanumeric()),
                "token '{t}' has non-alnum"
            );
        }
    }

    #[test]
    fn tokenize_short_words_filtered() {
        let tokens = tokenize("a an in on it be we at of to he");
        assert!(tokens.is_empty());
    }

    #[test]
    fn tokenize_max_10() {
        let tokens = tokenize("one two three four five six seven eight nine ten eleven twelve");
        assert_eq!(tokens.len(), 10);
    }

    #[test]
    fn retriever_stage_empty_backend() {
        let stage = RetrieverStage::empty();
        assert_eq!(stage.name(), "retriever");
    }

    #[test]
    fn retriever_stage_new() {
        let stage = RetrieverStage::new(
            None::<Arc<dyn GraphRepository>>,
            None::<Arc<dyn VectorRepository>>,
            None::<Arc<dyn EmbedService>>,
        );
        assert_eq!(stage.name(), "retriever");
    }

    #[test]
    fn parse_graph_search_results_empty() {
        let raw = serde_json::json!([]);
        let items = parse_graph_search_results(&raw, "reality");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_graph_search_results_with_data() {
        let raw = serde_json::json!([
            {"name": "PaymentService", "type": "Service", "description": "Handles payments", "source_file": "payment-svc.java", "score": 0.95},
            {"name": "UserDB", "type": "Database", "description": "User database", "score": 0.82}
        ]);
        let items = parse_graph_search_results(&raw, "reality");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "PaymentService");
        assert_eq!(items[0].entity_type, "Service");
        assert_eq!(items[1].label, "UserDB");
    }
}
