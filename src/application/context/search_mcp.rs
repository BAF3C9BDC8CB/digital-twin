//! **CrossWorldSearch** — cross-world semantic search (4.8).
//!
//! Searches across all available knowledge worlds (code, knowledge graph,
//! documents, vector store) in a single query, returning blended results.
//!
//! # MCP tool: `dt_search`
//!
//! ```text
//! dt_search(query: str, world?: str, limit?: int)
//!   → CrossWorldResult JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// Input for cross-world search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchRequest {
    /// Natural-language search query.
    pub query: String,
    /// Target world: "code", "knowledge", "doc", "all".  Default: "all".
    pub world: Option<String>,
    /// Maximum results per world.
    pub limit: Option<usize>,
    /// Filter by project.
    pub project: Option<String>,
}

/// A single search hit from any world.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    /// Unique identifier.
    pub id: String,
    /// Short label / title.
    pub title: String,
    /// Content or summary snippet.
    pub snippet: String,
    /// Source world ("code", "knowledge", "doc", "vector").
    pub source_world: String,
    /// Entity type (Method, Class, Knowledge, Document, etc.).
    pub entity_type: String,
    /// Relevance score [0.0, 1.0].
    pub score: f64,
    /// Source file or reference URL.
    pub source_ref: Option<String>,
    /// Source file path (code world).
    pub file_path: Option<String>,
    /// Start line number (code world).
    pub start_line: Option<u32>,
    /// End line number (code world).
    pub end_line: Option<u32>,
    /// Function/method signature (code world).
    pub signature: Option<String>,
    /// Called function names (code world).
    pub calls: Vec<String>,
    /// Element ID from the knowledge graph.
    pub element_id: Option<String>,
}

/// Output of cross-world search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrossWorldResult {
    /// The original query.
    pub query: String,
    /// Which world(s) were searched.
    pub world: String,
    /// Aggregated results.
    pub hits: Vec<SearchHit>,
    /// Total number of hits.
    pub total: usize,
    /// Per-world hit counts for transparency.
    pub per_world_counts: std::collections::HashMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// Performs cross-world semantic search.
#[async_trait::async_trait]
pub trait CrossWorldSearchTrait: Send + Sync {
    /// Search across worlds.
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError>;
}

/// Canonical implementation of [`CrossWorldSearchTrait`].
pub struct CrossWorldSearch {
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
}

impl CrossWorldSearch {
    /// Create with backends.
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
    ) -> Self {
        Self { graph, vector, embed }
    }

    /// Create with no backends (for testing).
    pub fn empty() -> Self {
        Self {
            graph: None,
            vector: None,
            embed: None,
        }
    }

    /// Search the Reality World (code entities) via Qdrant vector search.
    ///
    /// Queries `{project}_methods` collections (or all `*_methods` when no project
    /// is specified) and extracts full payload fields including start_line, end_line,
    /// calls, and method_id.  Score threshold is read from `DT_SEARCH_MIN_SCORE`
    /// (default 0.3).
    async fn search_code(
        &self,
        query: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };

        let embeddings = embed.embed_batch(&[query.to_string()]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        // Discover method collections – filter by project when given
        let collections = vector.list_collections().await?;
        let method_cols: Vec<String> = collections
            .into_iter()
            .filter(|c| c.ends_with("_methods"))
            .filter(|c| project.map_or(true, |p| c == &format!("{}_methods", p)))
            .collect();

        if method_cols.is_empty() {
            return Ok(Vec::new());
        }

        // Score threshold from environment (default 0.3)
        let min_score = std::env::var("DT_SEARCH_MIN_SCORE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.3);

        let mut all_hits = Vec::new();
        for col in &method_cols {
            match vector.search(col, query_vec.clone(), limit as u64).await {
                Ok(results) => {
                    for hit in results {
                        let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        if score < min_score {
                            continue;
                        }
                        let payload =
                            hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
                        let name =
                            payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        if name.is_empty() || name == "?" {
                            continue;
                        }

                        let calls: Vec<String> = payload
                            .get("calls")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| c.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        all_hits.push(SearchHit {
                            id: hit
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string(),
                            title: name.to_string(),
                            snippet: payload
                                .get("signature")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            source_world: "code".into(),
                            entity_type: "Method".into(),
                            score,
                            source_ref: None,
                            file_path: payload
                                .get("file_path")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            start_line: payload
                                .get("start_line")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32),
                            end_line: payload
                                .get("end_line")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32),
                            signature: payload
                                .get("signature")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            calls,
                            element_id: payload
                                .get("method_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        });
                    }
                }
                Err(e) => tracing::warn!("Qdrant search on {col}: {e}"),
            }
        }

        // Sort by score descending, cap to limit
        all_hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_hits.truncate(limit);
        Ok(all_hits)
    }

    /// Search the Knowledge World via graph.
    async fn search_knowledge(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let Some(ref graph) = self.graph else {
            return Ok(Vec::new());
        };

        let cypher = r#"
            MATCH (n)
            WHERE labels(n)[0] IN ['Concept', 'Playbook', 'Knowledge', 'Experience', 'Decision']
              AND (n.name CONTAINS $fragment
                OR n.title CONTAINS $fragment
                OR n.description CONTAINS $fragment
                OR n.summary CONTAINS $fragment
                OR n.definition CONTAINS $fragment)
            RETURN toString(id(n)) AS id,
                   coalesce(n.name, n.title, '') AS title,
                   coalesce(n.description, n.summary, n.definition, '') AS snippet,
                   labels(n)[0] AS type,
                   '' AS source_ref
            LIMIT $limit
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "fragment".to_string(),
            serde_json::Value::String(query.to_string()),
        );
        params.insert("limit".to_string(), serde_json::json!(limit as i64));

        let result = graph.read_query(cypher, params).await?;
        let rows = crate::application::context::graph_parse::parse_graph_rows(&result);
        let hits: Vec<SearchHit> = rows
            .into_iter()
            .map(|row| SearchHit {
                id: row
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                title: row
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                snippet: row
                    .get("snippet")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                source_world: "knowledge".into(),
                entity_type: row
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                score: row.get("score").and_then(|v| v.as_f64()).unwrap_or(0.5),
                source_ref: row
                    .get("source_ref")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                file_path: None,
                start_line: None,
                end_line: None,
                signature: None,
                calls: vec![],
                element_id: None,
            })
            .collect();
        Ok(hits)
    }

    /// Search the vector store via Qdrant.
    async fn search_vector(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };

        let embeddings = embed.embed_batch(&[query.to_string()]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        let results = vector.search("kg_nodes", query_vec, limit as u64).await?;

        let hits = results
            .into_iter()
            .map(|hit| {
                let payload = &hit["payload"];
                SearchHit {
                    id: hit["id"].as_str().unwrap_or("?").to_string(),
                    title: payload["name"].as_str().unwrap_or("?").to_string(),
                    snippet: payload["description"].as_str().unwrap_or("").to_string(),
                    source_world: "vector".into(),
                    entity_type: "Document".into(),
                    score: hit["score"].as_f64().unwrap_or(0.0),
                    source_ref: None,
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    signature: None,
                    calls: vec![],
                    element_id: None,
                }
            })
            .collect();

        Ok(hits)
    }


}

#[async_trait::async_trait]
impl CrossWorldSearchTrait for CrossWorldSearch {
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError> {
        let world = request.world.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(20);
        let project = request.project.as_deref();

        let mut per_world = std::collections::HashMap::new();
        let mut all_hits = Vec::new();

        // Search code world
        if world == "all" || world == "code" {
            if let Ok(hits) = self.search_code(&request.query, project, limit).await {
                per_world.insert("code".to_string(), hits.len());
                all_hits.extend(hits);
            }
        }

        // Search knowledge world
        if world == "all" || world == "knowledge" {
            if let Ok(hits) = self.search_knowledge(&request.query, limit).await {
                per_world.insert("knowledge".to_string(), hits.len());
                all_hits.extend(hits);
            }
        }

        // Search vector world
        if world == "all" || world == "doc" || world == "vector" {
            if let Ok(hits) = self.search_vector(&request.query, limit).await {
                per_world.insert("vector".to_string(), hits.len());
                all_hits.extend(hits);
            }
        }

        // Sort by score descending
        all_hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Cap to limit across all worlds
        let total = all_hits.len();
        all_hits.truncate(limit);

        Ok(CrossWorldResult {
            query: request.query.clone(),
            world: world.to_string(),
            hits: all_hits,
            total,
            per_world_counts: per_world,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_hit_construction() {
        let hit = SearchHit {
            id: "4:abc".into(),
            title: "PaymentService".into(),
            snippet: "Handles payment processing".into(),
            source_world: "code".into(),
            entity_type: "Service".into(),
            score: 0.95,
            source_ref: Some("src/payment.rs".into()),
            file_path: Some("src/payment.rs".into()),
            start_line: Some(10),
            end_line: Some(45),
            signature: Some("pub fn process()".into()),
            calls: vec!["validate".into(), "save".into()],
            element_id: Some("4:xyz".into()),
        };
        assert_eq!(hit.source_world, "code");
        assert!(hit.score > 0.9);
        assert_eq!(hit.start_line, Some(10));
        assert_eq!(hit.calls.len(), 2);
    }

    #[test]
    fn cross_world_result_empty() {
        let result = CrossWorldResult {
            query: "test".into(),
            world: "all".into(),
            hits: vec![],
            total: 0,
            per_world_counts: std::collections::HashMap::new(),
        };
        assert_eq!(result.total, 0);
    }

    #[test]
    fn cross_world_result_serialization() {
        let mut counts = std::collections::HashMap::new();
        counts.insert("code".into(), 5);
        counts.insert("knowledge".into(), 3);

        let result = CrossWorldResult {
            query: "auth".into(),
            world: "all".into(),
            hits: vec![SearchHit {
                id: "1".into(),
                title: "AuthService".into(),
                snippet: "manages auth".into(),
                source_world: "code".into(),
                entity_type: "Service".into(),
                score: 0.98,
                source_ref: None,
                file_path: None,
                start_line: None,
                end_line: None,
                signature: None,
                calls: vec![],
                element_id: None,
            }],
            total: 8,
            per_world_counts: counts,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("AuthService"));
        assert!(json.contains("\"total\":8"));
    }

    #[test]
    fn search_request_defaults() {
        let req = SearchRequest {
            query: "payment".into(),
            world: None,
            limit: None,
            project: None,
        };
        assert_eq!(req.query, "payment");
        assert_eq!(req.world, None);
    }
}
