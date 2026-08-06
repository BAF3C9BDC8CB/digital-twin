//! `dt nacos-sync`、`dt k8s-sync` 和 `dt kg-sync` 的 CLI 处理器。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::sync::Arc;

use crate::application::sync::traits::SyncSource;
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

/// 处理 `dt nacos-sync`——将 Nacos 配置和服务同步到 KG。
///
/// `graph` 必须已预先连接；`nacos_url` 由调用方从配置解析。
pub async fn handle_nacos_sync(
    env: String,
    graph: Option<Arc<dyn GraphRepository>>,
    nacos_url: &str,
) -> anyhow::Result<()> {
    tracing::info!("dt-daemon CLI: nacos-sync --env {env}");

    println!("Nacos 同步: env={env}, url={nacos_url}");

    if let Some(graph) = graph {
        // 同时执行配置与服务同步。
        let client = crate::application::sync::nacos::client::NacosClient::new(nacos_url);
        let config_source = crate::application::sync::nacos::config_sync::ConfigSyncSource::new(
            client.clone(),
            env.clone(),
        );
        match config_source.sync(&*graph).await {
            Ok(report) => {
                let total_configs = report.configs;
                let total_ns = report.namespaces;
                println!(
                    "配置同步: {} 个命名空间, {} 个配置",
                    total_ns, total_configs,
                );
                tracing::info!(
                    "nacos-sync 配置: {} 个配置分布在 {} 个命名空间",
                    total_configs,
                    total_ns,
                );
            }
            Err(e) => {
                tracing::error!("nacos-sync 配置同步失败: {e}");
                println!("配置同步失败: {e}");
            }
        }

        // 执行服务同步。
        let service_source = crate::application::sync::nacos::service_sync::ServiceSyncSource::new(
            client,
            env.clone(),
        );
        match service_source.sync(&*graph).await {
            Ok(report) => {
                println!(
                    "服务同步: {} 个命名空间, {} 个服务",
                    report.namespaces, report.services,
                );
                tracing::info!(
                    "nacos-sync 服务: {} 个服务分布在 {} 个命名空间",
                    report.services,
                    report.namespaces,
                );
            }
            Err(e) => {
                tracing::error!("nacos-sync 服务同步失败: {e}");
                println!("服务同步失败: {e}");
            }
        }

        // 清理孤立的 NacosConfig 节点（namespace=null、data_id=null）。
        let cleanup_cypher = "MATCH (c:NacosConfig) \
            WHERE c.namespace IS NULL OR c.namespace = '' \
            OR c.data_id IS NULL OR c.data_id = '' \
            DETACH DELETE c";
        match graph
            .write_query(cleanup_cypher, std::collections::HashMap::new())
            .await
        {
            Ok(_) => tracing::info!("孤立的 NacosConfig 节点已清理"),
            Err(e) => tracing::warn!("清理孤立的 NacosConfig 节点失败: {e}"),
        }
    } else {
        tracing::warn!("图数据库不可用——nacos-sync 已跳过");
        println!("Nacos 同步: 图数据库不可用——已跳过");
    }

    tracing::info!("nacos-sync 完成: env={env}");
    Ok(())
}

/// 处理 `dt k8s-sync`——将 K8s 资源同步到 KG。
pub async fn handle_k8s_sync(
    dry_run: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    k8s_cfg: Option<crate::application::sync::k8s::K8sSyncConfig>,
) -> anyhow::Result<()> {
    tracing::info!("dt-daemon CLI: k8s-sync --dry-run {dry_run}");

    if let Some(k8s) = k8s_cfg {
        println!("K8s 同步: server={}, dry_run={}", k8s.server, dry_run);

        if dry_run {
            println!("演练模式——将同步命名空间: {:?}", k8s.effective_namespaces());
            tracing::info!("k8s-sync 演练完成");
            return Ok(());
        }

        if let Some(graph) = graph {
            let sync = crate::application::sync::k8s::resource_sync::K8sResourceSync::new(k8s);
            match sync.run(&*graph).await {
                Ok(summaries) => {
                    for s in &summaries {
                        println!(
                            "  {}: 获取 {} 个, 写入 {} 个, {}ms",
                            s.resource, s.items_fetched, s.items_written, s.elapsed_ms
                        );
                        if !s.is_success() {
                            for err in &s.errors {
                                println!("    错误: {err}");
                            }
                        }
                    }
                    tracing::info!(
                        "k8s-sync 完成: {:?}",
                        summaries.iter().map(|s| &s.resource).collect::<Vec<_>>()
                    );
                }
                Err(e) => {
                    tracing::error!("k8s-sync 失败: {e}");
                    println!("K8s 同步失败: {e}");
                }
            }
        } else {
            tracing::warn!("图数据库不可用——k8s-sync 已跳过");
            println!("K8s 同步: 图数据库不可用——已跳过");
        }
    } else {
        println!("未配置 K8s——跳过");
        tracing::warn!("未配置 K8s——跳过");
    }

    Ok(())
}

/// 处理 `dt kg-sync`——将 KG 节点同步到 Qdrant 向量库。
pub async fn handle_kg_sync(
    incremental: bool,
    labels: Option<String>,
    config_chunks: bool,
    graph: Option<Arc<dyn GraphRepository>>,
    queue: Option<Arc<crate::application::sync::queue::VectorQueue>>,
) -> anyhow::Result<()> {
    tracing::warn!("dt kg-sync 已弃用——请改用 `dt build --source knowledge`");
    tracing::info!(
        "dt-daemon CLI: kg-sync --incremental {incremental} --labels {:?}",
        labels,
    );

    println!("KG 同步: incremental={incremental}");
    if let Some(ref l) = labels {
        println!("  labels: {l}");
    }

    if let Some(graph) = graph {
        // 若可用则使用 VectorQueue，否则直接连接。
        let (embed, vector) = if let Some(ref q) = queue {
            let v: Arc<dyn VectorRepository> = {
                let qdrant_url = std::env::var("QDRANT_URL")
                    .unwrap_or_else(|_| "http://localhost:6334".to_string());
                match crate::infrastructure::qdrant::QdrantClient::connect(&qdrant_url).await {
                    Ok(client) => {
                        tracing::info!("kg-sync 已连接 Qdrant");
                        Arc::new(crate::infrastructure::qdrant::QdrantRepo::new(client))
                    }
                    Err(e) => {
                        tracing::warn!("kg-sync 的 Qdrant 不可用: {e}");
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
                    siliconflow_model_embed:
                        crate::infrastructure::siliconflow::embed_model_from_env(),
                    siliconflow_model_reranker:
                        crate::infrastructure::siliconflow::reranker_model_from_env(),
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
                tracing::info!("kg-sync 已创建 Embed provider 路由");
                svc
            };
            let qdrant_url =
                std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6334".to_string());
            let vector: Arc<dyn VectorRepository> =
                match crate::infrastructure::qdrant::QdrantClient::connect(&qdrant_url).await {
                    Ok(client) => {
                        tracing::info!("kg-sync 已连接 Qdrant");
                        Arc::new(crate::infrastructure::qdrant::QdrantRepo::new(client))
                    }
                    Err(e) => {
                        tracing::warn!("kg-sync 的 Qdrant 不可用: {e}");
                        Arc::new(crate::infrastructure::qdrant::repo::NoopVectorRepo)
                    }
                };
            (embed, vector)
        };

        let mut bridge =
            crate::application::sync::kg_bridge::KgBridge::new(graph.clone(), embed, vector);
        if let Some(q) = queue {
            bridge = bridge.with_queue(q);
        }

        // 若请求了配置分块同步
        if config_chunks {
            println!("正在同步配置分块...");
            let report = bridge.sync_config_chunks().await?;
            println!(
                "配置分块: 已同步 {} 个, {}ms",
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
            "KG 同步完成: 已同步 {} 个节点, {}ms",
            report.items_created, report.elapsed_ms,
        );
    } else {
        tracing::warn!("图数据库不可用——kg-sync 已跳过");
        println!("KG 同步: 图数据库不可用——已跳过");
    }

    tracing::info!("kg-sync 完成: incremental={incremental}");
    Ok(())
}
