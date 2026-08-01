//! Core trait abstractions for the Digital Twin system.
//!
//! Defines repository, service, and plugin traits that form the
//! contract between layers.

use crate::domain::error::DtError;
use crate::domain::types::{FileSnapshot, HealthStatus, ParseResult};
use async_trait::async_trait;
use std::path::Path;

/// Repository abstraction for the graph database (Bolt driver).
#[async_trait]
pub trait GraphRepository: Send + Sync + 'static {
    /// Execute a read-only Cypher query.
    async fn read_query(
        &self,
        query: &str,
        params: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, crate::domain::error::DtError>;

    /// Execute a write Cypher query.
    async fn write_query(
        &self,
        query: &str,
        params: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, crate::domain::error::DtError>;

    /// Check connection health.
    async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError>;
}

/// Repository abstraction for the Qdrant vector database (gRPC driver).
#[async_trait]
pub trait VectorRepository: Send + Sync + 'static {
    /// Ensure a collection exists, creating it if necessary.
    async fn ensure_collection(
        &self,
        collection: &str,
        vector_dim: u32,
    ) -> Result<(), crate::domain::error::DtError>;

    /// Search for nearest neighbours given an embedding vector.
    async fn search(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
    ) -> Result<Vec<serde_json::Value>, crate::domain::error::DtError>;

    /// Search for nearest neighbours with a payload filter (R7).
    ///
    /// `filter` uses the Qdrant filter JSON shape
    /// (`{"must": [{"key": ..., "match": {"value": ...}}], "should": [...],
    /// "must_not": [...]}`).
    ///
    /// The default implementation calls [`VectorRepository::search`] and then
    /// filters the returned hits by their `payload` — correct but slower.
    /// Backends with native filter support (e.g. Qdrant) should override this
    /// with a server-side filtered query. The existing [`VectorRepository::search`]
    /// signature is unchanged for backward compatibility.
    async fn search_with_filter(
        &self,
        collection: &str,
        vector: Vec<f32>,
        limit: u64,
        filter: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, crate::domain::error::DtError> {
        let hits = self.search(collection, vector, limit).await?;
        Ok(hits
            .into_iter()
            .filter(|hit| payload_matches_filter(hit.get("payload"), &filter))
            .collect())
    }

    /// Upsert points into a collection.
    async fn upsert(
        &self,
        collection: &str,
        points: Vec<serde_json::Value>,
    ) -> Result<(), crate::domain::error::DtError>;

    /// Delete points matching a filter condition.
    async fn delete_by_filter(
        &self,
        collection: &str,
        filter: serde_json::Value,
    ) -> Result<(), crate::domain::error::DtError>;

    /// List all collection names.
    async fn list_collections(&self) -> Result<Vec<String>, crate::domain::error::DtError>;

    /// Get detailed info about a specific collection.
    async fn collection_info(
        &self,
        name: &str,
    ) -> Result<crate::domain::types::CollectionInfo, crate::domain::error::DtError>;

    /// Delete a collection and all its points.
    async fn delete_collection(&self, name: &str) -> Result<(), crate::domain::error::DtError>;

    /// Check connection health.
    async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError>;
}

/// Post-filter a search hit's payload against a Qdrant-style filter JSON.
///
/// Supports the `must` / `should` / `must_not` clause arrays, each holding
/// `{"key": <field>, "match": {"value": <scalar>}}` conditions. An absent
/// payload never matches a `must` condition. This backs the default
/// [`VectorRepository::search_with_filter`] implementation.
fn payload_matches_filter(payload: Option<&serde_json::Value>, filter: &serde_json::Value) -> bool {
    let clause_matches = |clause: &serde_json::Value| -> bool {
        let key = clause.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let expected = clause.get("match").and_then(|m| m.get("value"));
        match expected {
            Some(expected) => payload.and_then(|p| p.get(key)) == Some(expected),
            // Unsupported condition shape — do not exclude the hit.
            None => true,
        }
    };

    let must_ok = filter
        .get("must")
        .and_then(|c| c.as_array())
        .map(|conds| conds.iter().all(clause_matches))
        .unwrap_or(true);
    let must_not_ok = filter
        .get("must_not")
        .and_then(|c| c.as_array())
        .map(|conds| !conds.iter().any(clause_matches))
        .unwrap_or(true);
    let should_ok = match filter.get("should").and_then(|c| c.as_array()) {
        // `should` only constrains when no `must` clause is present (Qdrant
        // semantics); with `must` present it is a boost, not a filter.
        Some(conds) if !conds.is_empty() && filter.get("must").is_none() => {
            conds.iter().any(clause_matches)
        }
        _ => true,
    };

    must_ok && must_not_ok && should_ok
}

/// Repository abstraction for SQLite file snapshots (change detection).
#[async_trait]
pub trait SnapshotRepository: Send + Sync + 'static {
    /// Get the last known snapshot for a specific file.
    async fn get_snapshot(
        &self,
        project: &str,
        path: &str,
    ) -> Result<Option<FileSnapshot>, DtError>;

    /// Save (upsert) one or more file snapshots.
    async fn save_snapshots(
        &self,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError>;

    /// Delete all snapshots for a project.
    async fn delete_project(&self, project: &str) -> Result<u64, DtError>;

    /// List all snapshots for a project.
    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError>;

    /// Mark a file as having completed LLM analysis, with the current file hash.
    async fn mark_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<(), DtError>;

    /// Check whether a file has already been LLM-analyzed with the same content.
    /// Returns `true` only if previously analyzed AND the file hash matches.
    async fn is_llm_analyzed(
        &self,
        project: &str,
        file_path: &str,
        file_sha1: &str,
    ) -> Result<bool, DtError>;

    /// Clear all LLM analysis progress for a project (used on full rebuild).
    async fn clear_llm_progress(&self, project: &str) -> Result<(), DtError>;

    /// Mark a pipeline step as completed for a file, keyed by file content hash.
    /// Steps: "tree_sitter", "chunk", "hanlp", "llm", "embed", "store".
    async fn mark_step_done(
        &self,
        project: &str,
        file_path: &str,
        step: &str,
        file_hash: &str,
    ) -> Result<(), DtError>;

    /// Check whether a pipeline step has already been completed for a file
    /// with the same content hash.  Returns `true` only if the exact step+file+hash
    /// combination exists in the progress table.
    async fn is_step_done(
        &self,
        project: &str,
        file_path: &str,
        step: &str,
        file_hash: &str,
    ) -> Result<bool, DtError>;

    /// Clear all pipeline step progress for a project (used on full rebuild).
    async fn clear_step_progress(&self, project: &str) -> Result<(), DtError>;

    /// Check storage health.
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

/// Embedding service abstraction.
#[async_trait]
pub trait EmbedService: Send + Sync + 'static {
    /// Generate embeddings for a batch of texts.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError>;

    /// Check service health.
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

/// LLM (chat completion) service abstraction.
#[async_trait]
pub trait LlmService: Send + Sync + 'static {
    /// Send a chat completion request.
    async fn chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String, DtError>;

    /// Check service health.
    async fn health_check(&self) -> Result<HealthStatus, DtError>;

    /// Return this provider's capabilities.
    fn capabilities(&self) -> LlmCapabilities;
}

/// Rerank service abstraction.
#[async_trait]
pub trait RerankService: Send + Sync + 'static {
    /// Rerank documents against a query. Returns relevance scores in original order.
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>, DtError>;

    /// Check service health.
    async fn health_check(&self) -> Result<HealthStatus, DtError>;
}

/// Provider capability declaration.
#[derive(Debug, Clone, Default)]
pub struct LlmCapabilities {
    /// Supports embedding.
    pub embed: bool,
    /// Supports reranking.
    pub rerank: bool,
    /// Supports LLM chat completion.
    pub chat: bool,
    /// Maximum tokens per response.
    pub max_tokens: u32,
}

/// Parse strategy trait — implemented per programming language.
///
/// Each language parser can independently determine if it can handle a file
/// and produce parsed entities from source text.
#[async_trait]
pub trait ParseStrategy: Send + Sync {
    /// Return the language this parser handles.
    fn language(&self) -> crate::domain::types::Language;

    /// Returns `true` if this parser can handle the given file.
    fn can_parse(&self, path: &Path) -> bool;

    /// Parse source text into methods and classes.
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError>;
}

/// Build service abstraction — orchestrates the entire build pipeline.
#[async_trait]
pub trait BuildService: Send + Sync + 'static {
    /// Full/incremental build for a project.
    async fn build(
        &self,
        project: &str,
        root: &Path,
    ) -> Result<crate::domain::types::BuildReport, DtError>;

    /// Single-file update (for real-time hook triggers).
    async fn update_file(&self, project: &str, path: &Path) -> Result<(), DtError>;

    /// Remove all data for a project.
    async fn delete_project(&self, project: &str) -> Result<(), DtError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that traits are object-safe (can be used as `dyn Trait`).
    #[test]
    fn traits_are_object_safe() {
        // If these traits weren't object-safe they'd fail at compile time.
        fn _accept_graph(_: &dyn GraphRepository) {}
        fn _accept_vector(_: &dyn VectorRepository) {}
        fn _accept_snapshot(_: &dyn SnapshotRepository) {}
        fn _accept_embed(_: &dyn EmbedService) {}
        fn _accept_parse(_: &dyn ParseStrategy) {}
        fn _accept_llm(_: &dyn LlmService) {}
        fn _accept_rerank(_: &dyn RerankService) {}
    }

    /// Stub repo that only implements `search` — exercises the default
    /// `search_with_filter` post-filtering (R7 backward compatibility).
    struct StubVectorRepo {
        hits: Vec<serde_json::Value>,
    }

    #[async_trait]
    impl VectorRepository for StubVectorRepo {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }

        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.hits.clone())
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

        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.to_string(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }

        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn default_search_with_filter_post_filters_by_payload() {
        let repo = StubVectorRepo {
            hits: vec![
                serde_json::json!({"score": 0.9, "payload": {"project": "a", "type": "Service"}}),
                serde_json::json!({"score": 0.8, "payload": {"project": "b", "type": "Service"}}),
                serde_json::json!({"score": 0.7, "payload": {"type": "Service"}}), // no project key
            ],
        };
        let hits = repo
            .search_with_filter(
                "kg_nodes",
                vec![0.0],
                5,
                serde_json::json!({"must": [{"key": "project", "match": {"value": "a"}}]}),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["payload"]["project"], serde_json::json!("a"));
    }

    #[tokio::test]
    async fn default_search_with_filter_must_not_excludes() {
        let repo = StubVectorRepo {
            hits: vec![
                serde_json::json!({"score": 0.9, "payload": {"project": "a"}}),
                serde_json::json!({"score": 0.8, "payload": {"project": "b"}}),
            ],
        };
        let hits = repo
            .search_with_filter(
                "c",
                vec![0.0],
                5,
                serde_json::json!({"must_not": [{"key": "project", "match": {"value": "b"}}]}),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["payload"]["project"], serde_json::json!("a"));
    }

    #[tokio::test]
    async fn default_search_with_filter_should_without_must_is_disjunctive() {
        let repo = StubVectorRepo {
            hits: vec![
                serde_json::json!({"score": 0.9, "payload": {"project": "a"}}),
                serde_json::json!({"score": 0.8, "payload": {"project": "b"}}),
                serde_json::json!({"score": 0.7, "payload": {"project": "c"}}),
            ],
        };
        let hits = repo
            .search_with_filter(
                "c",
                vec![0.0],
                5,
                serde_json::json!({"should": [
                    {"key": "project", "match": {"value": "a"}},
                    {"key": "project", "match": {"value": "b"}},
                ]}),
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn default_search_with_filter_empty_filter_passes_all() {
        let repo = StubVectorRepo {
            hits: vec![serde_json::json!({"score": 0.9, "payload": {"project": "a"}})],
        };
        let hits = repo
            .search_with_filter("c", vec![0.0], 5, serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }
}
