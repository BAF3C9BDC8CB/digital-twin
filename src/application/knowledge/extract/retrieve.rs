//! Retrieve 检索层 — GraphRAG 式混合检索（spec S5 / 主文档 §8）。
//!
//! 管线：召回(kg_nodes) → 图扩展(SAME_AS/RELATES/elementId) → 分桶截断
//! → rerank(sigmoid) → 融合排序。任一路故障降级，不整体失败（spec §3）。

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};

// ---------------------------------------------------------------------------
// 输出契约子结构（spec §5.7.3）
// ---------------------------------------------------------------------------

/// 排序分解——权重调参（主文档 §8 收敛回写）的观测数据源。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreBreakdown {
    /// 向量召回分；邻居 = 种子分 × 0.5^hop（§5.4 近似）。
    pub semantic: f64,
    /// sigmoid 归一后；rerank_degraded 时为 0.0。
    pub rerank: f64,
    /// 正常 1.0/0.5/0.25（0.5^hop）；降级模式一阶衰减 1.0/0.5（§5.4）。
    pub graph_boost: f64,
    /// = SearchHit.score，冗余字段便于消费方免对齐。
    pub final_score: f64,
}

/// 关系摘要——命中实体对外的 top-5 关系（按 confidence 降序）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelationSnippet {
    /// RELATES.type，如 routes_to / depends_on。
    pub rel_type: String,
    /// 对端 business_id。
    pub other_end_id: String,
    /// 对端 name（展示用；不在候选集中时为空串）。
    pub other_end_name: String,
    /// "out" | "in"（相对命中实体）。
    pub direction: String,
    pub confidence: f64,
    /// 最高 confidence 边的证据句。
    pub evidence: Option<String>,
    /// 其余文档来源的补充证据数（边聚合产物；>0 表示多文档佐证）。
    pub supplementary_count: u32,
}

// ---------------------------------------------------------------------------
// 配置（env；spec §8 配置项汇总表）
// ---------------------------------------------------------------------------

/// 语义分阈值（复用 code 世界同款 env，S5 起 knowledge/doc 世界生效）。
pub fn min_score() -> f64 {
    std::env::var("DT_SEARCH_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.3)
}

/// rerank 候选总上限（分桶规则见 §5.2.3）。
pub fn rerank_top_n() -> usize {
    std::env::var("DT_KG_RERANK_TOP_N")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
}

/// 图扩展跳数白名单 {1,2}，其他值钳到 2（S5-D3；严禁直接拼请求整数进 Cypher）。
pub fn clamp_max_hops(h: u32) -> u32 {
    h.clamp(1, 2)
}

/// bge-reranker-v2-m3 返回 logit，sigmoid 归一到 [0,1]（S5-D6）。
pub fn sigmoid(x: f32) -> f64 {
    1.0 / (1.0 + (-(x as f64)).exp())
}

// ---------------------------------------------------------------------------
// Retriever — 混合检索执行器（管线在后续任务逐层填充）
// ---------------------------------------------------------------------------

/// GraphRAG 混合检索执行器。`graph`/`rerank` 可选（缺失即走对应降级路径）。
pub struct Retriever {
    pub(crate) graph: Option<Arc<dyn GraphRepository>>,
    pub(crate) vector: Arc<dyn VectorRepository>,
    pub(crate) embed: Arc<dyn EmbedService>,
    pub(crate) rerank: Option<Arc<dyn RerankService>>,
}

impl Retriever {
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            rerank,
        }
    }
}
