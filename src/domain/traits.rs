//! Core trait abstractions for the Digital Twin system.
//!
//! Defines repository, service, and plugin traits that form the
//! contract between layers.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::types::{HealthStatus, FileSnapshot, ParseResult};
use std::path::Path;

/// Repository abstraction for the Neo4j knowledge graph (Bolt driver).
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

/// Repository abstraction for SQLite file snapshots (change detection).
#[async_trait]
pub trait SnapshotRepository: Send + Sync + 'static {
    /// Get the last known snapshot for a specific file.
    async fn get_snapshot(&self, project: &str, path: &str) -> Result<Option<FileSnapshot>, DtError>;

    /// Save (upsert) one or more file snapshots.
    async fn save_snapshots(&self, project: &str, snapshots: &[FileSnapshot]) -> Result<(), DtError>;

    /// Delete all snapshots for a project.
    async fn delete_project(&self, project: &str) -> Result<u64, DtError>;

    /// List all snapshots for a project.
    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>, DtError>;

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
    fn parse(
        &self,
        source: &str,
        path: &Path,
        project: &str,
    ) -> Result<ParseResult, DtError>;
}

/// Build service abstraction — orchestrates the entire build pipeline.
#[async_trait]
pub trait BuildService: Send + Sync + 'static {
    /// Full/incremental build for a project.
    async fn build(&self, project: &str, root: &Path) -> Result<crate::domain::types::BuildReport, DtError>;

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
    }
}
