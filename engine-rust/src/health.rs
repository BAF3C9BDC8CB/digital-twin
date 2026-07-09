use anyhow::Result;
use crate::config;
use crate::client::{neo4j, embed};

pub async fn run_health() -> Result<()> {
    println!("=== Digital Twin Health Check ===\n");

    // Neo4j
    let neo4j_url = config::load().services.neo4j.url.clone();
    print!("[1/5] Neo4j ({}) ... ", neo4j_url);
    match neo4j::health().await {
        Ok(_) => println!("✅"),
        Err(e) => println!("❌ {}", e),
    }

    // dt-embed CLI
    print!("[2/5] dt-embed CLI ({}) ... ", config::load().services.embed_server.model);
    match embed::health().await {
        Ok((model, dim)) => println!("✅ ({} dim={})", model, dim),
        Err(e) => println!("❌ {}", e),
    }

    // Qdrant
    print!("[3/5] Qdrant ({}) ... ", config::load().services.qdrant.url);
    match check_qdrant().await {
        Ok(count) => println!("✅ ({} collections)", count),
        Err(e) => println!("❌ {}", e),
    }

    // KG→Qdrant Bridge: kg_nodes collection
    print!("[4/5] KG Bridge (kg_nodes) ... ");
    match check_kg_bridge().await {
        Ok(()) => println!("✅"),
        Err(e) => println!("❌ {} (run `dt kg-sync`)", e),
    }

    // Neo4j Fulltext Index: infra_search
    print!("[5/5] Fulltext Index (infra_search) ... ");
    match check_fulltext_index().await {
        Ok(()) => println!("✅"),
        Err(e) => println!("❌ {} (run `dt build` or `dt index`)", e),
    }

    println!();
    Ok(())
}

async fn check_qdrant() -> Result<usize> {
    let url = format!("{}/collections", config::load().services.qdrant.url.trim_end_matches('/'));
    let resp: serde_json::Value = reqwest::get(&url).await?.json().await?;
    let count = resp["result"]["collections"].as_array().map(|a| a.len()).unwrap_or(0);
    Ok(count)
}

async fn check_kg_bridge() -> Result<()> {
    let body = serde_json::json!({
        "vector": vec![0.0_f32; config::load().services.embed_server.dim],
        "limit": 1,
        "with_payload": false,
    });
    let url = format!("{}/collections/kg_nodes/points/search", config::load().services.qdrant.url.trim_end_matches('/'));
    let client = crate::client::get_client();
    let resp = client.post(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("kg_nodes collection not found"));
    }
    Ok(())
}

async fn check_fulltext_index() -> Result<()> {
    use serde_json::json;
    use crate::client::neo4j;
    // 尝试查询 infra_search 全文索引
    neo4j::run_cypher_raw(
        "CALL db.index.fulltext.queryNodes('infra_search', 'health_check') YIELD node RETURN count(node) LIMIT 0",
        json!({}),
    ).await?;
    Ok(())
}
