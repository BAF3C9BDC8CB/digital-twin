//! CLI handlers for `dt nacos-sync`, `dt k8s-sync`, and `dt kg-sync`.
//!
//! Extracted from main.rs to keep the entrypoint lean.

use std::sync::Arc;

use crate::application::sync::traits::SyncSource;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

/// Handle `dt nacos-sync` — synchronise Nacos configuration and services into the KG.
///
/// `graph` must be pre-connected; `nacos_url` is resolved from config by the caller.
pub async fn handle_nacos_sync(
    env: String,
    graph: Option<Arc<dyn GraphRepository>>,
    nacos_url: &str,
) -> anyhow::Result<()> {
    tracing::info!("dt-daemon CLI: nacos-sync --env {env}");

    println!("Nacos sync: env={env}, url={nacos_url}");

    if let Some(graph) = graph {
        // Run both config and service sync.
        let client =
            crate::application::sync::nacos::client::NacosClient::new(nacos_url);
        let config_source =
            crate::application::sync::nacos::config_sync::ConfigSyncSource::new(
                client.clone(),
                env.clone(),
            );
        match config_source.sync(&*graph).await {
            Ok(report) => {
                let total_configs = report.configs;
                let total_ns = report.namespaces;
                println!(
                    "Config sync: {} namespaces, {} configs",
                    total_ns, total_configs,
                );
                tracing::info!(
                    "nacos-sync config: {} configs across {} namespaces",
                    total_configs, total_ns,
                );
            }
            Err(e) => {
                tracing::error!("nacos-sync config failed: {e}");
                println!("Config sync failed: {e}");
            }
        }

        // Run service sync.
        let service_source =
            crate::application::sync::nacos::service_sync::ServiceSyncSource::new(
                client,
                env.clone(),
            );
        match service_source.sync(&*graph).await {
            Ok(report) => {
                println!(
                    "Service sync: {} namespaces, {} services",
                    report.namespaces, report.services,
                );
                tracing::info!(
                    "nacos-sync service: {} services across {} namespaces",
                    report.services, report.namespaces,
                );
            }
            Err(e) => {
                tracing::error!("nacos-sync service failed: {e}");
                println!("Service sync failed: {e}");
            }
        }

        // Clean orphaned NacosConfig nodes (namespace=null, data_id=null).
        let cleanup_cypher = "MATCH (c:NacosConfig) \
            WHERE c.namespace IS NULL OR c.namespace = '' \
            OR c.data_id IS NULL OR c.data_id = '' \
            DETACH DELETE c";
        match graph.write_query(cleanup_cypher, std::collections::HashMap::new()).await {
            Ok(_) => tracing::info!("Orphan NacosConfig nodes cleaned"),
            Err(e) => tracing::warn!("Failed to clean orphan NacosConfig nodes: {e}"),
        }
    } else {
        tracing::warn!("Neo4j unavailable — nacos-sync skipped");
        println!("Nacos sync: Neo4j unavailable — skipped");
    }

    tracing::info!("nacos-sync complete: env={env}");
    Ok(())
}

/// Handle `dt k8s-sync` — synchronise K8s resources into the KG.
pub async fn handle_k8s_sync(
    dry_run: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    k8s_cfg: Option<crate::application::sync::k8s::K8sSyncConfig>,
) -> anyhow::Result<()> {
    tracing::info!("dt-daemon CLI: k8s-sync --dry-run {dry_run}");

    if let Some(k8s) = k8s_cfg {
        println!(
            "K8s sync: server={}, dry_run={}",
            k8s.server, dry_run
        );

        if dry_run {
            println!(
                "Dry-run mode — would sync namespaces: {:?}",
                k8s.effective_namespaces()
            );
            tracing::info!("k8s-sync dry-run complete");
            return Ok(());
        }

        if let Some(graph) = graph {
            let sync = crate::application::sync::k8s::resource_sync::K8sResourceSync::new(k8s);
            match sync.run(&*graph).await {
                Ok(summaries) => {
                    for s in &summaries {
                        println!(
                            "  {}: {} fetched, {} written, {}ms",
                            s.resource, s.items_fetched, s.items_written, s.elapsed_ms
                        );
                        if !s.is_success() {
                            for err in &s.errors {
                                println!("    error: {err}");
                            }
                        }
                    }
                    tracing::info!("k8s-sync complete: {:?}", summaries.iter().map(|s| &s.resource).collect::<Vec<_>>());
                }
                Err(e) => {
                    tracing::error!("k8s-sync failed: {e}");
                    println!("K8s sync failed: {e}");
                }
            }
        } else {
            tracing::warn!("Neo4j unavailable — k8s-sync skipped");
            println!("K8s sync: Neo4j unavailable — skipped");
        }
    } else {
        println!("K8s not configured — skip");
        tracing::warn!("K8s not configured — skip");
    }

    Ok(())
}

/// Handle `dt kg-sync` — synchronise KG nodes to Qdrant vector store.
pub async fn handle_kg_sync(
    incremental: bool,
    labels: Option<String>,
    graph: Option<Arc<dyn GraphRepository>>,
) -> anyhow::Result<()> {
    tracing::info!(
        "dt-daemon CLI: kg-sync --incremental {incremental} --labels {:?}",
        labels,
    );

    println!("KG sync: incremental={incremental}");
    if let Some(ref l) = labels {
        println!("  labels: {l}");
    }

    if let Some(graph) = graph {
        // Connect to real embed service (BGE-M3 gRPC)
        let embed: Arc<dyn EmbedService> = match crate::infrastructure::embedder::GrpcEmbedService::connect("http://[::1]:50052").await {
            Ok(svc) => {
                tracing::info!("dt-embed connected for kg-sync");
                Arc::new(svc)
            }
            Err(e) => {
                tracing::warn!("dt-embed unavailable for kg-sync: {e} — using NoopEmbedService");
                Arc::new(crate::infrastructure::embedder::NoopEmbedService::default())
            }
        };

        // Connect to real Qdrant vector store
        let vector: Arc<dyn VectorRepository> = match crate::infrastructure::qdrant::QdrantClient::connect("http://localhost:6334").await {
            Ok(client) => {
                tracing::info!("Qdrant connected for kg-sync");
                Arc::new(crate::infrastructure::qdrant::QdrantRepo::new(client))
            }
            Err(e) => {
                tracing::warn!("Qdrant unavailable for kg-sync: {e} — using NoopVectorRepo");
                Arc::new(crate::infrastructure::qdrant::repo::NoopVectorRepo)
            }
        };

        let bridge =
            crate::application::sync::kg_bridge::KgBridge::new(
                graph.clone(),
                embed,
                vector,
            );

        let report = if incremental {
            bridge.sync_incremental().await?
        } else {
            bridge.sync_all().await?
        };

        println!(
            "KG sync complete: {} nodes synced, {}ms",
            report.items_created, report.elapsed_ms,
        );
    } else {
        tracing::warn!("Neo4j unavailable — kg-sync skipped");
        println!("KG sync: Neo4j unavailable — skipped");
    }

    tracing::info!("kg-sync complete: incremental={incremental}");
    Ok(())
}
