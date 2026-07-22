//! gRPC server assembly — owns the tonic Router, registers all plugins,
//! and starts the daemon listener.
//!
//! Backend connections (Memgraph, Qdrant) are obtained via
//! [`crate::interfaces::grpc::wiring::wire()`].  If either backend is
//! unreachable the server falls back to no-op implementations and logs
//! a warning — the daemon starts regardless.

use crate::interfaces::grpc::wiring;
use crate::interfaces::grpc::auth::auth_interceptor;
use crate::domain::types::{AppConfig, PluginContext, PluginLogger};
use crate::application::plugins::registry::PluginRegistry;
use std::net::SocketAddr;
use std::sync::Arc;

/// Build and run the gRPC server.
///
/// 1. Call [`wiring::wire()`] to assemble real backend connections
/// 2. Create `PluginContext` (falling back to no-op repos if connections failed)
/// 3. Register builtin plugins into `PluginRegistry`
/// 4. Initialize each plugin
/// 5. Create tonic Server, wire all plugins' gRPC services + DtCore
/// 6. Add bootstrap health service to get the final Router
/// 7. Bind and serve
pub async fn run(config: AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = config.listen_addr.parse()?;
    tracing::info!("dt-daemon binding to {}", addr);

    // ---- Assemble backend components via wiring ----
    let components = wiring::wire().await;

    // ---- Plugin context ----
    // If real backends are unavailable (None), fall back to no-op
    // implementations so the server still starts.
    let graph: Arc<dyn crate::domain::traits::GraphRepository> = match components.graph {
        Some(g) => {
            tracing::info!("using real Memgraph backend for gRPC server");
            g
        }
        None => {
            tracing::warn!("Memgraph unavailable — gRPC server will use NoopGraphRepo");
            Arc::new(crate::infrastructure::memgraph::NoopGraphRepo)
        }
    };
    let vector: Arc<dyn crate::domain::traits::VectorRepository> = match components.vector {
        Some(v) => {
            tracing::info!("using real Qdrant backend for gRPC server");
            v
        }
        None => {
            tracing::warn!("Qdrant unavailable — gRPC server will use NoopVectorRepo");
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

    // ---- Plugin registry ----
    let mut registry = PluginRegistry::new();

    // Register builtin plugins (default stubs for server mode)
    registry.register(Arc::new(
        crate::application::plugins::k8s::service::K8sPluginService::default(),
    ))?;
    registry.register(Arc::new(
        crate::application::plugins::svc::service::SvcPluginService::default(),
    ))?;
    registry.register(Arc::new(
        crate::application::plugins::jenkins::service::JenkinsPluginService::default(),
    ))?;

    tracing::info!("registered {} plugins", registry.len());

    // Init all plugins
    let init_results = registry.init_all(&ctx).await;
    for (id, res) in &init_results {
        match res {
            Ok(()) => tracing::info!("plugin [{}] initialized", id),
            Err(e) => tracing::warn!("plugin [{}] init failed: {}", id, e),
        }
    }

    // ---- Build gRPC router ----
    let mut server = tonic::transport::Server::builder();

    // Wire all plugins (each calls server.add_service internally).
    registry.wire_grpc(&mut server)?;

    // Register DtCore gRPC service — delegates dt build/search/context/
    // event/memorize/sync to the same application-layer services used
    // by the CLI.  Passes real backend Arc handles so the service can
    // access Memgraph and Qdrant directly.
    let dt_core_impl =
        crate::interfaces::grpc::services::dt_core_service::DtCoreServiceImpl::new(
            Some(graph.clone()),
            Some(vector.clone()),
            components.embed.clone(),
            components.hook_engine.clone(),
        );
    server.add_service(
        crate::proto::dt::core::dt_core_server::DtCoreServer::new(dt_core_impl),
    );

    // Apply the auth interceptor layer so every request gets a Role injected
    // into its extensions. tower::ServiceBuilder chains multiple layers
    // together — currently just auth, but ready for rate-limiting etc.
    let auth_layer = tower::ServiceBuilder::new()
        .layer(tonic::service::interceptor(auth_interceptor));
    let mut router = server.layer(auth_layer);

    // Bootstrap: add tonic's built-in health service.
    // The Router now includes the auth layer — all services (plugins + DtCore + health)
    // are behind the interceptor.
    let (_health_reporter, health_service) = tonic_health::server::health_reporter();
    let router = router.add_service(health_service);

    // ---- Serve ----
    tracing::info!("dt-daemon listening on {}", addr);
    router.serve(addr).await?;

    Ok(())
}
