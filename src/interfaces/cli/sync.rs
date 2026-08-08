//! `dt kg-sync` 的 CLI 处理器。
//!
//! 从 main.rs 抽取，保持入口文件精简。

use std::sync::Arc;

use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};

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
                    siliconflow_max_concurrent: 20,
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
