// List all projects from config.yaml with indexing status

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use base64::Engine;

use crate::config;

pub async fn run_list(filter: Option<&str>, all: bool) -> Result<()> {
    let cfg = config::load();
    let projects = cfg.projects();

    if projects.is_empty() {
        println!("config.yaml 中没有配置项目。");
        if all {
            println!("  使用 dt build --path <path> --name <name> 添加项目。");
        }
        return Ok(());
    }

    // 过滤器：支持前缀或包含匹配
    let filtered: Vec<(String, String)> = if let Some(f) = filter {
        projects.into_iter().filter(|(name, _)| name.contains(f)).collect()
    } else {
        projects
    };

    if filtered.is_empty() {
        println!("没有匹配 '{}' 的项目。", filter.unwrap_or(""));
        return Ok(());
    }

    let t0 = Instant::now();

    // 快速模式只输出名称和路径，不查询后端
    if !all {
        println!("{:<40} {:<10} {}", "项目名", "磁盘", "路径");
        println!("{}", "-".repeat(90));
        for (name, path) in &filtered {
            let disk = if Path::new(path).is_dir() { "✅" } else { "❌" };
            println!("{:<42} {:<10} {}", name, disk, path);
        }
        println!("\n共 {} 个项目。使用 --all 查看详细状态。", filtered.len());
        println!("耗时 {:.1}s", t0.elapsed().as_secs_f64());
        return Ok(());
    }

    // 全量模式：查询 Neo4j + Qdrant
    println!("{:<30} {:<4} {:>6} {:>6} {:>8} {:>8}  {}", "项目名", "磁盘", "向量", "方法", "语言", "类型", "路径");
    println!("{}", "-".repeat(120));

    for (name, path) in &filtered {
        let disk = if Path::new(path).is_dir() { "✅" } else { "❌" };

        // 查询 Neo4j meta
        let (neo4j_count, language, project_type) = get_project_stats(name).await;
        // 查询 Qdrant
        let collection = format!("{}_methods", name);
        let qdrant_count = get_qdrant_count(&collection).await;

        let qdrant_str = match qdrant_count {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };
        let neo4j_str = match neo4j_count {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };

        println!(
            "{:<30} {:<4} {:>6} {:>6} {:>8} {:>8}  {}",
            name, disk, qdrant_str, neo4j_str, language, project_type, path,
        );
    }

    println!("\n共 {} 个项目，耗时 {:.1}s", filtered.len(), t0.elapsed().as_secs_f64());
    Ok(())
}

/// 查询项目统计: (method_count, language, project_type)
async fn get_project_stats(project: &str) -> (Option<u64>, String, String) {
    let cfg = config::load();
    let url = format!(
        "{}/db/neo4j/tx/commit",
        cfg.services.neo4j.url.trim_end_matches('/')
    );
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(
            format!("{}:{}", cfg.services.neo4j.user, cfg.services.neo4j.password).as_bytes(),
        )
    );

    let body = serde_json::json!({
        "statements": [{
            "statement": "OPTIONAL MATCH (m:Method {project: $project}) \
                          OPTIONAL MATCH (p:Project {name: $project}) \
                          RETURN count(m) as c, p.language, p.project_type",
            "parameters": {"project": project}
        }]
    });

    let client = crate::client::get_client();
    let resp = match client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", &auth)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return (None, "-".into(), "-".into()),
    };

    if !resp.status().is_success() {
        return (None, "-".into(), "-".into());
    }

    let data: serde_json::Value = match resp.json().await {
        Ok(d) => d,
        Err(_) => return (None, "-".into(), "-".into()),
    };

    let row = &data["results"][0]["data"];
    if row.as_array().map_or(true, |a| a.is_empty()) {
        return (None, "-".into(), "-".into());
    }

    let count = row[0]["row"][0].as_u64();
    let lang = row[0]["row"][1].as_str().unwrap_or("-").to_string();
    let ptype = row[0]["row"][2].as_str().unwrap_or("-").to_string();

    (count, lang, ptype)
}

async fn get_qdrant_count(collection: &str) -> Option<u64> {
    let url = format!(
        "{}/collections/{}",
        config::load().services.qdrant.url.trim_end_matches('/'),
        collection
    );
    let client = crate::client::get_client();
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: serde_json::Value = resp.json().await.ok()?;
    data["result"]["points_count"].as_u64()
}
