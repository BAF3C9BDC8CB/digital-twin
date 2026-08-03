//! Collection creation parameters for Qdrant.
//!
//! （CollectionKind / model_version 命名体系已随统一检索退役——U 系列清理；
//! 现役集合名常量见 shared/collections.rs。）

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
    // Distance tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_distance_as_str() {
        assert_eq!(Distance::Cosine.as_str(), "Cosine");
        assert_eq!(Distance::Dot.as_str(), "Dot");
        assert_eq!(Distance::Euclidean.as_str(), "Euclidean");
    }
}
