//! DtCoreServiceImpl — implements the generated `dt.core.DtCore` gRPC trait.
//!
//! This struct holds backend connections (graph, vector) and delegates each
//! RPC method to the appropriate handler function in the sibling modules:
//!
//! | RPC          | Handler module                |
//! |-------------|-------------------------------|
//! | `Build`      | [`build_service::handle_build`] |
//! | `Search`     | [`build_service::handle_search`] |
//! | `GetContext` | [`context_service::handle_get_context`] |
//! | `RecordEvent`| [`memory_service::handle_record_event`] |
//! | `Memorize`   | [`knowledge_service::handle_memorize`] |
//! | `Sync`       | [`sync_service::handle_sync`] |

use crate::domain::traits::{GraphRepository, VectorRepository};
use crate::proto::dt::core::dt_core_server::DtCore;
use crate::proto::dt::core::*;
use crate::proto::dt::common;
use std::sync::Arc;
use tonic::{Request, Response, Status};

use super::{build_service, context_service, sync_service, knowledge_service, memory_service};

/// gRPC service implementation for `dt.core.DtCore`.
///
/// Holds optional backend connections. When a backend is `None`, RPCs that
/// require it will return `Status::UNAVAILABLE`.
#[derive(Clone)]
pub struct DtCoreServiceImpl {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
}

impl DtCoreServiceImpl {
    /// Create a new service instance with the given backends.
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
    ) -> Self {
        Self { graph, vector }
    }
}

#[tonic::async_trait]
impl DtCore for DtCoreServiceImpl {
    async fn build(
        &self,
        request: Request<BuildRequest>,
    ) -> Result<Response<BuildResponse>, Status> {
        let req = request.into_inner();
        let resp = build_service::handle_build(req, self.graph.clone(), self.vector.clone()).await?;
        Ok(Response::new(resp))
    }

    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let req = request.into_inner();
        let resp = build_service::handle_search(req, self.graph.clone(), self.vector.clone()).await?;
        Ok(Response::new(resp))
    }

    async fn get_context(
        &self,
        request: Request<ContextRequest>,
    ) -> Result<Response<ContextResponse>, Status> {
        let req = request.into_inner();
        let resp =
            context_service::handle_get_context(req, self.graph.clone(), self.vector.clone())
                .await?;
        Ok(Response::new(resp))
    }

    async fn record_event(
        &self,
        request: Request<EventRequest>,
    ) -> Result<Response<common::Empty>, Status> {
        let req = request.into_inner();
        let resp = memory_service::handle_record_event(req, self.graph.clone()).await?;
        Ok(Response::new(resp))
    }

    async fn memorize(
        &self,
        request: Request<MemorizeRequest>,
    ) -> Result<Response<common::Empty>, Status> {
        let req = request.into_inner();
        let resp = knowledge_service::handle_memorize(req, self.graph.clone()).await?;
        Ok(Response::new(resp))
    }

    async fn sync(
        &self,
        request: Request<SyncRequest>,
    ) -> Result<Response<SyncResponse>, Status> {
        let req = request.into_inner();
        let resp = sync_service::handle_sync(req, self.graph.clone(), self.vector.clone()).await?;
        Ok(Response::new(resp))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_impl_is_cloneable() {
        let svc = DtCoreServiceImpl::new(None, None);
        let _clone = svc.clone();
    }

    #[test]
    fn service_impl_new_accepts_options() {
        let svc = DtCoreServiceImpl::new(None, None);
        assert!(svc.graph.is_none());
        assert!(svc.vector.is_none());
    }
}
