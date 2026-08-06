//! DtCore 服务的 Sync gRPC 处理器。
//!
//! 委托 [`KgBridge`] 将知识图谱节点同步到
//! Qdrant 向量库，用于语义搜索。

use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::proto::dt::core::*;
use std::sync::Arc;
use std::time::Instant;
use tonic::Status;

/// `Sync` RPC 的处理器——将 KG 节点同步到向量库。
pub async fn handle_sync(
    req: SyncRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
) -> Result<SyncResponse, Status> {
    let start = Instant::now();

    let graph = graph.ok_or_else(|| Status::unavailable("图后端不可用"))?;

    let vector = vector.ok_or_else(|| Status::unavailable("Qdrant 向量后端不可用"))?;

    // 若可用则使用真实的 SiliconFlow embed API，否则回退到零向量 noop。
    let embed: Arc<dyn EmbedService> = embed.unwrap_or_else(|| {
        tracing::warn!("SiliconFlow 不可用，同步将产生零向量嵌入");
        Arc::new(crate::infrastructure::embedder::NoopEmbedService::default())
    });

    let bridge = crate::application::sync::kg_bridge::KgBridge::new(graph, embed, vector);

    let report = if req.incremental {
        bridge.sync_incremental().await
    } else {
        bridge.sync_all().await
    };

    match report {
        Ok(r) => {
            let elapsed = start.elapsed().as_secs_f64();
            Ok(SyncResponse {
                nodes_synced: r.items_created as i32,
                nodes_skipped: r.items_skipped as i32,
                elapsed_secs: elapsed,
            })
        }
        Err(e) => Err(Status::internal(format!("同步失败: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sync_requires_graph() {
        let req = SyncRequest {
            incremental: true,
            labels: vec![],
        };
        let result = handle_sync(req, None, None, None).await;
        assert!(result.is_err());
    }
}
