//! gRPC 服务器装配——持有 tonic Router，注册所有插件，
//! 并启动守护进程监听器。
//!
//! 后端连接（Memgraph、Qdrant）通过
//! [`crate::interfaces::grpc::wiring::wire()`] 获取。若任一后端
//! 不可达，服务器回退到 no-op 实现并记录警告——守护进程照常启动。

use crate::application::plugins::registry::PluginRegistry;
use crate::domain::types::{AppConfig, PluginContext, PluginLogger};
use crate::interfaces::grpc::auth::auth_interceptor;
use crate::interfaces::grpc::wiring;
use std::net::SocketAddr;
use std::sync::Arc;

/// 构建并运行 gRPC 服务器。
///
/// 1. 调用 [`wiring::wire()`] 组装真实后端连接
/// 2. 创建 `PluginContext`（连接失败时回退到 no-op 仓库）
/// 3. 将内置插件注册到 `PluginRegistry`
/// 4. 初始化每个插件
/// 5. 创建 tonic Server，接入所有插件的 gRPC 服务 + DtCore
/// 6. 添加 bootstrap 健康服务以得到最终 Router
/// 7. 绑定并对外服务
pub async fn run(config: AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = config.listen_addr.parse()?;
    tracing::info!("dt-daemon 正在绑定 {}", addr);

    // ---- 通过 wiring 组装后端组件 ----
    let components = wiring::wire().await;

    // ---- 插件上下文 ----
    // 若真实后端不可用（None），回退到 no-op
    // 实现，使服务器仍能启动。
    let graph: Arc<dyn crate::domain::traits::GraphRepository> = match components.graph {
        Some(g) => {
            tracing::info!("gRPC 服务器使用真实 Memgraph 后端");
            g
        }
        None => {
            tracing::warn!("Memgraph 不可用——gRPC 服务器将使用 NoopGraphRepo");
            Arc::new(crate::infrastructure::memgraph::NoopGraphRepo)
        }
    };
    let vector: Arc<dyn crate::domain::traits::VectorRepository> = match components.vector {
        Some(v) => {
            tracing::info!("gRPC 服务器使用真实 Qdrant 后端");
            v
        }
        None => {
            tracing::warn!("Qdrant 不可用——gRPC 服务器将使用 NoopVectorRepo");
            Arc::new(crate::infrastructure::qdrant::NoopVectorRepo)
        }
    };

    let ctx = PluginContext {
        graph: graph.clone(),
        vector: vector.clone(),
        config: Arc::new(config),
        log: PluginLogger::new("dt-daemon"),
        data_dir: std::path::PathBuf::from("/var/lib/digital-twin"),
    };

    // ---- 插件注册表 ----
    let mut registry = PluginRegistry::new();

    // 注册内置插件（服务器模式的默认桩实现）
    registry.register(Arc::new(
        crate::application::plugins::k8s::service::K8sPluginService::default(),
    ))?;
    registry.register(Arc::new(
        crate::application::plugins::svc::service::SvcPluginService::default(),
    ))?;
    registry.register(Arc::new(
        crate::application::plugins::jenkins::service::JenkinsPluginService::default(),
    ))?;

    tracing::info!("已注册 {} 个插件", registry.len());

    // 初始化所有插件
    let init_results = registry.init_all(&ctx).await;
    for (id, res) in &init_results {
        match res {
            Ok(()) => tracing::info!("插件 [{}] 已初始化", id),
            Err(e) => tracing::warn!("插件 [{}] 初始化失败: {}", id, e),
        }
    }

    // ---- 构建 gRPC router ----
    let mut server = tonic::transport::Server::builder();

    // 接入所有插件（每个插件内部都会调用 server.add_service）。
    registry.wire_grpc(&mut server)?;

    // 注册 DtCore gRPC 服务——将 dt build/search/context/
    // event/memorize/sync 委托给 CLI 使用的同一批应用层服务。
    // 传入真实后端的 Arc 句柄，使服务能直接访问 Memgraph 和 Qdrant。
    let dt_core_impl = crate::interfaces::grpc::services::dt_core_service::DtCoreServiceImpl::new(
        Some(graph.clone()),
        Some(vector.clone()),
        components.embed.clone(),
        components.hook_engine.clone(),
    );
    server.add_service(crate::proto::dt::core::dt_core_server::DtCoreServer::new(
        dt_core_impl,
    ));

    // 应用认证拦截器层，使每个请求都被注入 Role 到其扩展中。
    // tower::ServiceBuilder 将多个 layer 串联在一起——目前只有 auth，
    // 但已为限流等做好准备。
    let auth_layer =
        tower::ServiceBuilder::new().layer(tonic::service::interceptor(auth_interceptor));
    let mut router = server.layer(auth_layer);

    // Bootstrap：添加 tonic 内置的健康检查服务。
    // Router 现在包含 auth 层——所有服务（插件 + DtCore + health）
    // 都在拦截器之后。
    let (_health_reporter, health_service) = tonic_health::server::health_reporter();
    let router = router.add_service(health_service);

    // ---- 对外服务 ----
    tracing::info!("dt-daemon 正在监听 {}", addr);
    router.serve(addr).await?;

    Ok(())
}
