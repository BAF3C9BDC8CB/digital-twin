//! Sync gRPC handler for the DtCore service.
//!
//! Delegates to [`KgBridge`] to synchronise knowledge-graph nodes into the
//! Qdrant vector store for semantic search.

use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::proto::dt::core::*;
use std::sync::Arc;
use std::time::Instant;
use tonic::Status;

/// Handler for `Sync` RPC — sync KG nodes to vector store.
pub async fn handle_sync(
    req: SyncRequest,
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
) -> Result<SyncResponse, Status> {
    let start = Instant::now();

    let graph = graph.ok_or_else(|| Status::unavailable("Graph backend not available"))?;

    let vector =
        vector.ok_or_else(|| Status::unavailable("Qdrant vector backend not available"))?;

    // Use real SiliconFlow embed API if available, fall back to zero-vector noop.
    let embed: Arc<dyn EmbedService> = embed.unwrap_or_else(|| {
        tracing::warn!("SiliconFlow unavailable, sync will produce zero-vector embeddings");
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
        Err(e) => Err(Status::internal(format!("Sync failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// Tests
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
