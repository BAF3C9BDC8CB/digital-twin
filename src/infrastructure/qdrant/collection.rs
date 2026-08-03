//! Qdrant 的集合创建参数。
//!
//! （CollectionKind / model_version 命名体系已随统一检索退役——U 系列清理；
//! 现役集合名常量见 shared/collections.rs。）

// ---------------------------------------------------------------------------
// 集合创建参数
// ---------------------------------------------------------------------------

/// 用于向量相似度的距离度量。
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

/// 创建 Qdrant 集合的配置。
pub struct CollectionConfig {
    /// 向量维度（BGE-M3 为 1024）。
    pub vector_dim: u32,
    /// 距离度量（默认：Cosine）。
    pub distance: Distance,
    /// HNSW M 参数——图中每个节点的最大边数（默认：16）。
    pub hnsw_m: u32,
    /// HNSW ef_construct——索引构建期间的搜索深度（默认：100）。
    pub hnsw_ef_construct: u32,
    /// 触发自动索引的阈值（默认：10000）。
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
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CollectionConfig 测试
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
    // Distance 测试
    // -----------------------------------------------------------------------

    #[test]
    fn test_distance_as_str() {
        assert_eq!(Distance::Cosine.as_str(), "Cosine");
        assert_eq!(Distance::Dot.as_str(), "Dot");
        assert_eq!(Distance::Euclidean.as_str(), "Euclidean");
    }
}
