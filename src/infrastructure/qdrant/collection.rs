//! Collection naming and configuration for Qdrant.
//!
//! ## Model migration path
//!
//! Collection naming includes `{model_version}` to support future embedding
//! model migration without data loss:
//!
//! ```text
//! Old:  myproject_methods
//! New:  myproject_methods_v1
//!       myproject_methods_v2  (after model upgrade)
//! ```
//!
//! Migration steps:
//! 1. Deploy the new embed server with the new model
//! 2. Update `config.yaml` → `embed.model_version`
//! 3. Run `dt build --reindex` — creates new collection with new version suffix
//! 4. Old collection retained for 30 days (manual rollback window)
//!

/// Kinds of vector collections managed by the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
    /// Code vector embeddings (methods, functions).
    Methods,
    /// Document / text vector embeddings.
    Semantic,
    /// Knowledge-graph node vector embeddings.
    KgNodes,
}

impl CollectionKind {
    /// Human-readable suffix for collection names.
    pub fn as_str(&self) -> &'static str {
        match self {
            CollectionKind::Methods => "methods",
            CollectionKind::Semantic => "semantic",
            CollectionKind::KgNodes => "kg_nodes",
        }
    }
}

/// Build a fully-qualified Qdrant collection name.
///
/// Format per kind:
///   - `Methods`:   `{project}_methods_{model_version}`
///   - `Semantic`:  `{project}_semantic_{model_version}`
///   - `KgNodes`:   `kg_nodes_{model_version}`   (cross-project)
///
/// # Examples
///
/// ```rust
/// # use dt_daemon::infrastructure::qdrant::collection::{collection_name, CollectionKind};
/// assert_eq!(
///     collection_name("myproj", CollectionKind::Methods, "v1"),
///     "myproj_methods_v1"
/// );
/// assert_eq!(
///     collection_name("myproj", CollectionKind::Semantic, "v1"),
///     "myproj_semantic_v1"
/// );
/// assert_eq!(
///     collection_name("", CollectionKind::KgNodes, "v1"),
///     "kg_nodes_v1"
/// );
/// ```
pub fn collection_name(project: &str, kind: CollectionKind, model_version: &str) -> String {
    match kind {
        CollectionKind::Methods | CollectionKind::Semantic => {
            format!("{}_{}_{}", project, kind.as_str(), model_version)
        }
        CollectionKind::KgNodes => {
            format!("{}_{}", kind.as_str(), model_version)
        }
    }
}

// ---------------------------------------------------------------------------
// Collection creation parameters
// ---------------------------------------------------------------------------

/// Distance metric used for vector similarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distance {
    Cosine,
    Dot,
    Euclidean,
}

impl Distance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Distance::Cosine => "Cosine",
            Distance::Dot => "Dot",
            Distance::Euclidean => "Euclidean",
        }
    }
}

/// Configuration for creating a Qdrant collection.
pub struct CollectionConfig {
    /// Vector dimension (1024 for BGE-M3).
    pub vector_dim: u32,
    /// Distance metric (default: Cosine).
    pub distance: Distance,
    /// HNSW M parameter — max edges per node in the graph (default: 16).
    pub hnsw_m: u32,
    /// HNSW ef_construct — search depth during index build (default: 100).
    pub hnsw_ef_construct: u32,
    /// Threshold after which automatic indexing is triggered (default: 10000).
    pub indexing_threshold: u32,
}

impl Default for CollectionConfig {
    fn default() -> Self {
        Self {
            vector_dim: 1024,
            distance: Distance::Cosine,
            hnsw_m: 16,
            hnsw_ef_construct: 100,
            indexing_threshold: 10_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Collection naming tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_collection_name_methods_with_model_version() {
        let name = collection_name("myproj", CollectionKind::Methods, "v1");
        assert_eq!(name, "myproj_methods_v1");
    }

    #[test]
    fn test_collection_name_semantic_with_model_version() {
        let name = collection_name("myproj", CollectionKind::Semantic, "v2");
        assert_eq!(name, "myproj_semantic_v2");
    }

    #[test]
    fn test_collection_name_kg_nodes_ignores_project() {
        // KgNodes is cross-project — project name is NOT embedded.
        let name = collection_name("irrelevant", CollectionKind::KgNodes, "v3");
        assert_eq!(name, "kg_nodes_v3");
    }

    #[test]
    fn test_collection_name_different_model_versions_produce_different_names() {
        let a = collection_name("proj", CollectionKind::Methods, "v1");
        let b = collection_name("proj", CollectionKind::Methods, "v2");
        assert_ne!(a, b);
    }

    // -----------------------------------------------------------------------
    // CollectionConfig tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_collection_config_defaults() {
        let cfg = CollectionConfig::default();
        assert_eq!(cfg.vector_dim, 1024);
        assert!(matches!(cfg.distance, Distance::Cosine));
        assert_eq!(cfg.hnsw_m, 16);
        assert_eq!(cfg.hnsw_ef_construct, 100);
        assert_eq!(cfg.indexing_threshold, 10_000);
    }

    // -----------------------------------------------------------------------
    // CollectionKind tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_collection_kind_as_str() {
        assert_eq!(CollectionKind::Methods.as_str(), "methods");
        assert_eq!(CollectionKind::Semantic.as_str(), "semantic");
        assert_eq!(CollectionKind::KgNodes.as_str(), "kg_nodes");
    }

    #[test]
    fn test_collection_kind_all_variants_present() {
        // Verify all three variants exist at the type level.
        let kinds = [
            CollectionKind::Methods,
            CollectionKind::Semantic,
            CollectionKind::KgNodes,
        ];
        assert_eq!(kinds.len(), 3);
        for k in &kinds {
            assert!(!k.as_str().is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Distance tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_distance_as_str() {
        assert_eq!(Distance::Cosine.as_str(), "Cosine");
        assert_eq!(Distance::Dot.as_str(), "Dot");
        assert_eq!(Distance::Euclidean.as_str(), "Euclidean");
    }
}
