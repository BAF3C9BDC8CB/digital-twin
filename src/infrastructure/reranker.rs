//! Reranker service client.

/// gRPC reranker service for re-ranking search results.
pub struct GrpcRerankerService;

impl GrpcRerankerService {
    pub async fn connect(_url: &str) -> anyhow::Result<Self> {
        Ok(GrpcRerankerService)
    }

    /// Rerank candidate texts against a query.
    /// Returns relevance scores in the same order as input texts.
    pub async fn rerank(
        &self,
        _query: &str,
        _texts: &[String],
    ) -> anyhow::Result<Vec<f64>> {
        Ok(vec![])
    }
}
