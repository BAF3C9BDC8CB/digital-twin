//! DtCoreServiceImpl——实现生成的 `dt.core.DtCore` gRPC trait。
//!
//! 该结构持有后端连接（graph、vector），并将每个 RPC 方法
//! 委托给兄弟模块中对应的处理函数：
//!
//! | RPC          | 处理器模块                     |
//! |-------------|-------------------------------|
//! | `Build`      | [`build_service::handle_build`] |
//! | `Search`     | [`build_service::handle_search`] |
//! | `RecordEvent`| [`memory_service::handle_record_event`] |
//! | `Memorize`   | [`knowledge_service::handle_memorize`] |
//! | `Sync`       | [`sync_service::handle_sync`] |

use crate::application::hooks::HookEngine;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::proto::dt::common;
use crate::proto::dt::core::dt_core_server::DtCore;
use crate::proto::dt::core::*;
use std::sync::Arc;
use tonic::{Request, Response, Status};

use super::{build_service, knowledge_service, memory_service, sync_service};

/// `dt.core.DtCore` 的 gRPC 服务实现。
///
/// 持有可选的后端连接。当某后端为 `None` 时，依赖它的 RPC
/// 将返回 `Status::UNAVAILABLE`。
#[derive(Clone)]
pub struct DtCoreServiceImpl {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub embed: Option<Arc<dyn EmbedService>>,
    pub hook_engine: Option<Arc<HookEngine>>,
}

impl DtCoreServiceImpl {
    /// 使用给定的后端创建新的服务实例。
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
        hook_engine: Option<Arc<HookEngine>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            hook_engine,
        }
    }
}

#[tonic::async_trait]
impl DtCore for DtCoreServiceImpl {
    async fn build(
        &self,
        request: Request<BuildRequest>,
    ) -> Result<Response<BuildResponse>, Status> {
        let req = request.into_inner();
        let resp =
            build_service::handle_build(req, self.graph.clone(), self.vector.clone()).await?;
        Ok(Response::new(resp))
    }

    async fn search(
        &self,
        request: Request<SearchRequest>,
    ) -> Result<Response<SearchResponse>, Status> {
        let req = request.into_inner();
        let resp =
            build_service::handle_search(req, self.graph.clone(), self.vector.clone()).await?;
        Ok(Response::new(resp))
    }

    async fn record_event(
        &self,
        request: Request<EventRequest>,
    ) -> Result<Response<common::Empty>, Status> {
        let req = request.into_inner();
        let resp =
            memory_service::handle_record_event(req, self.graph.clone(), self.hook_engine.clone())
                .await?;
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

    async fn sync(&self, request: Request<SyncRequest>) -> Result<Response<SyncResponse>, Status> {
        let req = request.into_inner();
        let resp = sync_service::handle_sync(
            req,
            self.graph.clone(),
            self.vector.clone(),
            self.embed.clone(),
        )
        .await?;
        Ok(Response::new(resp))
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_impl_is_cloneable() {
        let svc = DtCoreServiceImpl::new(None, None, None, None);
        let _clone = svc.clone();
    }

    #[test]
    fn service_impl_new_accepts_options() {
        let svc = DtCoreServiceImpl::new(None, None, None, None);
        assert!(svc.graph.is_none());
        assert!(svc.vector.is_none());
        assert!(svc.embed.is_none());
        assert!(svc.hook_engine.is_none());
    }
}
