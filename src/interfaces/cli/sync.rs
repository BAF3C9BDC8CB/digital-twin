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
        tracing::warn!("Graph database unavailable — nacos-sync skipped");
        println!("Nacos sync: graph database unavailable — skipped");
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
            tracing::warn!("Graph database unavailable — k8s-sync skipped");
            println!("K8s sync: graph database unavailable — skipped");
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
    config_chunks: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    queue: Option<Arc<crate::application::sync::queue::VectorQueue>>,
) -> anyhow::Result<()> {
    tracing::warn!("dt kg-sync is deprecated — use `dt build --source knowledge` instead");
    tracing::info!(
        "dt-daemon CLI: kg-sync --incremental {incremental} --labels {:?}",
        labels,
    );

    println!("KG sync: incremental={incremental}");
    if let Some(ref l) = labels {
        println!("  labels: {l}");
    }

    if let Some(graph) = graph {
        // Use VectorQueue if available, else connect directly.
        let (embed, vector) = if let Some(ref q) = queue {
            let v: Arc<dyn VectorRepository> = {
                let qdrant_url = std::env::var("QDRANT_URL")
                    .unwrap_or_else(|_| "http://localhost:6334".to_string());
                match crate::infrastructure::qdrant::QdrantClient::connect(&qdrant_url).await {
                    Ok(client) => {
                        tracing::info!("Qdrant connected for kg-sync");
                        Arc::new(crate::infrastructure::qdrant::QdrantRepo::new(client))
                    }
                    Err(e) => {
                        tracing::warn!("Qdrant unavailable for kg-sync: {e}");
                        Arc::new(crate::infrastructure::qdrant::repo::NoopVectorRepo)
                    }
                }
            };
            (q.embed_service().clone(), v)
        } else {
            let embed: Arc<dyn EmbedService> = {
                let cfg = crate::infrastructure::embedder::ProviderConfig {
                    siliconflow_url: crate::infrastructure::siliconflow::base_url_from_env(),
                    siliconflow_api_key: crate::infrastructure::siliconflow::api_key_from_env(),
                    siliconflow_model_embed: crate::infrastructure::siliconflow::embed_model_from_env(),
                    siliconflow_model_reranker: crate::infrastructure::siliconflow::reranker_model_from_env(),
                    siliconflow_model_llm: crate::infrastructure::siliconflow::llm_model_from_env(),
                    xinference_url: String::new(),
                    xinference_api_key: String::new(),
                    xinference_model_embed: String::new(),
                    xinference_model_reranker: String::new(),
                    xinference_model_llm: String::new(),
                    embed_provider: "siliconflow".into(),
                    rerank_provider: "siliconflow".into(),
                    llm_provider: "siliconflow".into(),
                };
                let svc = crate::infrastructure::embedder::create_embed_router(cfg);
                tracing::info!("Embed provider router created for kg-sync");
                svc
            };
            let qdrant_url = std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6334".to_string());
            let vector: Arc<dyn VectorRepository> = match crate::infrastructure::qdrant::QdrantClient::connect(&qdrant_url).await {
                Ok(client) => {
                    tracing::info!("Qdrant connected for kg-sync");
                    Arc::new(crate::infrastructure::qdrant::QdrantRepo::new(client))
                }
                Err(e) => {
                    tracing::warn!("Qdrant unavailable for kg-sync: {e}");
                    Arc::new(crate::infrastructure::qdrant::repo::NoopVectorRepo)
                }
            };
            (embed, vector)
        };

        let mut bridge =
            crate::application::sync::kg_bridge::KgBridge::new(
                graph.clone(),
                embed,
                vector,
            );
        if let Some(q) = queue {
            bridge = bridge.with_queue(q);
        }

        // Sync config chunks if requested
        if config_chunks {
            println!("Syncing config chunks...");
            let report = bridge.sync_config_chunks().await?;
            println!(
                "Config chunks: {} synced, {}ms",
                report.items_created, report.elapsed_ms,
            );
            return Ok(());
        }

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
        tracing::warn!("Graph database unavailable — kg-sync skipped");
        println!("KG sync: graph database unavailable — skipped");
    }

    tracing::info!("kg-sync complete: incremental={incremental}");
    Ok(())
}
