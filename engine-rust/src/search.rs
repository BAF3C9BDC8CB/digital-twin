use anyhow::Result;
use serde::Serialize;

use crate::config;
use crate::client::{embed, qdrant};

#[derive(Debug, Serialize)]
pub struct SearchResultItem {
    pub method_id: String,
    pub name: String,
    pub signature: String,
    pub file_path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub language: String,
    pub project: String,
    pub score: f64,
    pub source_code: String,
    pub comment: String,
    pub class_name: String,
    pub calls: Vec<String>,
}

pub async fn run_search(
    query: &str,
    project: Option<&str>,
    limit: usize,
    search_all: bool,
    output_json: bool,
) -> Result<()> {
    embed::health().await?;
    let cfg = config::load();
    let client = reqwest::Client::new();

    let vector = embed::embed_batch(vec![query.to_string()]).await?
        .into_iter().next().unwrap_or_default();

    if search_all {
        let resp: serde_json::Value = client
            .get(format!("{}/collections", cfg.services.qdrant.url))
            .send().await?.json().await?;
        let collections = resp["result"]["collections"].as_array().cloned().unwrap_or_default();
        let mut all = Vec::new();
        let per_col = (limit / collections.len().max(1)).max(1);
        for col in collections {
            let name = col["name"].as_str().unwrap_or("");
            if !name.ends_with("_methods") { continue; }
            if let Ok(results) = qdrant::search(name, vector.clone(), per_col).await {
                all.extend(enrich_results(results).await.unwrap_or_default());
            }
        }
        all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(limit);
        print_results(all, output_json);
        return Ok(());
    }

    let project_name = project.unwrap_or("unknown");
    let collection = format!("{}_methods", project_name);
    let results = qdrant::search(&collection, vector.clone(), limit).await?;
    let enriched = enrich_results(results).await?;
    print_results(enriched, output_json);
    Ok(())
}

async fn enrich_results(results: Vec<qdrant::SearchResult>) -> Result<Vec<SearchResultItem>> {
    let items: Vec<SearchResultItem> = results.into_iter().map(|r| {
        let p = &r.payload;
        SearchResultItem {
            method_id: p.get("method_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            signature: p.get("signature").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            file_path: p.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            start_line: p.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0),
            end_line: p.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0),
            language: p.get("language").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            project: p.get("project").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            score: r.score,
            source_code: p.get("source_code").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            comment: p.get("comment").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            class_name: p.get("class_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            calls: p.get("calls").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }
    }).collect();
    Ok(items)
}

fn print_results(results: Vec<SearchResultItem>, json_output: bool) {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
        return;
    }
    if results.is_empty() {
        println!("  无结果");
        return;
    }
    for (i, r) in results.iter().enumerate() {
        println!(
            "\n--- [{}] {} (score: {:.4}) ---",
            i + 1, r.name, r.score
        );
        if !r.class_name.is_empty() {
            println!("    类: {}", r.class_name);
        }
        println!("    文件: {}:{}:{}", r.file_path, r.start_line, r.end_line);
        println!("    语言: {} | 项目: {}", r.language, r.project);
        if !r.signature.is_empty() {
            println!("    {}", &r.signature.chars().take(200).collect::<String>());
        }
        if !r.calls.is_empty() {
            println!("    calls: {}", r.calls.join(", "));
        }
    }
    println!();
}
