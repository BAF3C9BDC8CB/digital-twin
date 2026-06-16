use anyhow::Result;
use crate::config;
use crate::client::{neo4j, embed};

pub async fn run_health() -> Result<()> {
    println!("=== Digital Twin Health Check ===\n");

    // Neo4j
    print!("[1/3] Neo4j ({}) ... ", config::load().services.neo4j.url);
    match neo4j::health().await {
        Ok(_) => println!("✅"),
        Err(e) => println!("❌ {}", e),
    }

    // Embed Server
    print!("[2/3] Embed Server ({}) ... ", config::load().services.embed_server.url);
    match embed::health().await {
        Ok((model, dim)) => println!("✅ ({} dim={})", model, dim),
        Err(e) => println!("❌ {}", e),
    }

    // Qdrant
    print!("[3/3] Qdrant ({}) ... ", config::load().services.qdrant.url);
    match check_qdrant().await {
        Ok(collections) => println!("✅ ({} collections)", collections),
        Err(e) => println!("❌ {}", e),
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
