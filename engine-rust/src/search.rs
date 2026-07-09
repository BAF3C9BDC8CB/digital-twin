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
    path: Option<&str>,
    limit: usize,
    search_all: bool,
    output_json: bool,
    expand: bool,
) -> Result<()> {
    if query.trim().is_empty() {
        println!("错误: 搜索关键词不能为空");
        return Ok(());
    }

    embed::health().await?;

    if expand {
        return run_expanded_search(query, project, path, limit, search_all, output_json).await;
    }

    let vector = embed::embed_batch(vec![query.to_string()]).await?
        .into_iter().next().unwrap_or_default();

    if search_all {
        run_all_search(&vector, limit, output_json).await?;
        return Ok(());
    }

    if let Some(p) = path {
        run_path_search(&vector, p, limit, output_json).await?;
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

fn expand_queries(query: &str) -> Vec<(String, f64)> {
    vec![
        (query.to_string(), 1.0),
        (format!("{} 实现 函数 代码 类 方法", query), 0.85),
        (format!("{} 定义 逻辑", query), 0.75),
    ]
}

async fn run_all_search(vector: &[f32], limit: usize, output_json: bool) -> Result<()> {
    let cfg = config::load();
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .get(format!("{}/collections", cfg.services.qdrant.url))
        .send().await?.json().await?;
    let collections = resp["result"]["collections"].as_array().cloned().unwrap_or_default();
    let mut all = Vec::new();
    let per_col = (limit / collections.len().max(1)).max(1);
    for col in collections {
        let name = col["name"].as_str().unwrap_or("");
        if !name.ends_with("_methods") { continue; }
        if let Ok(results) = qdrant::search(name, vector.to_vec(), per_col).await {
            all.extend(enrich_results(results).await.unwrap_or_default());
        }
    }
    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(limit);
    print_results(all, output_json);
    Ok(())
}

/// Search all projects whose path is under the given directory.
/// Uses config.yaml to resolve project paths, filters by path prefix,
/// then searches each matching project's Qdrant collection.
async fn run_path_search(vector: &[f32], path: &str, limit: usize, output_json: bool) -> Result<()> {
    let projects = resolve_path_projects(path);

    if projects.is_empty() {
        eprintln!("dt search: 路径 '{}' 下没有找到已配置的项目", path);
        eprintln!("  提示: 用 dt list --all 查看所有已配置项目");
        eprintln!("  提示: 用 --all 跨所有项目搜索");
        return Ok(());
    }

    eprintln!("dt search: 在 {} 个项目下搜索路径 '{}'", projects.len(), path);
    for (name, proj_path) in &projects {
        eprintln!("  - {} ({})", name, proj_path);
    }

    let per_proj = (limit / projects.len().max(1)).max(1);
    let mut all = Vec::new();

    for (name, _) in &projects {
        let collection = format!("{}_methods", name);
        if let Ok(results) = qdrant::search(&collection, vector.to_vec(), per_proj).await {
            all.extend(enrich_results(results).await.unwrap_or_default());
        }
    }

    all.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(limit);
    print_results(all, output_json);
    Ok(())
}

async fn run_expanded_search(
    query: &str,
    project: Option<&str>,
    path: Option<&str>,
    limit: usize,
    search_all: bool,
    output_json: bool,
) -> Result<()> {
    let variants = expand_queries(query);
    let mut merged: std::collections::HashMap<String, SearchResultItem> = std::collections::HashMap::new();

    for (variant, weight) in &variants {
        let vector = embed::embed_batch(vec![variant.clone()]).await?
            .into_iter().next().unwrap_or_default();

        let results: Vec<qdrant::SearchResult> = if search_all {
            let cfg = config::load();
            let client = reqwest::Client::new();
            let resp: serde_json::Value = client
                .get(format!("{}/collections", cfg.services.qdrant.url))
                .send().await?.json().await?;
            let collections = resp["result"]["collections"].as_array().cloned().unwrap_or_default();
            let per_col = (limit / collections.len().max(1)).max(1);
            let mut all = Vec::new();
            for col in collections {
                let name = col["name"].as_str().unwrap_or("");
                if !name.ends_with("_methods") { continue; }
                if let Ok(r) = qdrant::search(name, vector.clone(), per_col).await {
                    all.extend(r);
                }
            }
            all
        } else if let Some(p) = path {
            // Path mode: resolve projects under path, search all matching collections
            let projects = resolve_path_projects(p);
            if projects.is_empty() { continue; }
            let per_proj = (limit / projects.len().max(1)).max(1);
            let mut all = Vec::new();
            for (name, _) in &projects {
                let collection = format!("{}_methods", name);
                if let Ok(r) = qdrant::search(&collection, vector.clone(), per_proj).await {
                    all.extend(r);
                }
            }
            all
        } else {
            let project_name = project.unwrap_or("unknown");
            let collection = format!("{}_methods", project_name);
            qdrant::search(&collection, vector.clone(), limit * 2).await.unwrap_or_default()
        };

        let enriched = enrich_results(results).await?;
        for mut item in enriched {
            item.score *= weight;
            let key = item.method_id.clone();
            if !merged.contains_key(&key) || item.score > merged[&key].score {
                merged.insert(key, item);
            }
        }
    }

    let mut sorted: Vec<SearchResultItem> = merged.into_values().collect();
    sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    sorted.truncate(limit);
    print_results(sorted, output_json);
    Ok(())
}

/// Resolve projects whose path falls under the given directory.
fn resolve_path_projects(path: &str) -> Vec<(String, String)> {
    let cfg = config::load();
    let canonical = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());

    cfg.projects()
        .into_iter()
        .filter(|(_, proj_path)| {
            proj_path.starts_with(&canonical)
                || proj_path.starts_with(path)
                || std::fs::canonicalize(proj_path)
                    .map(|cp| cp.to_string_lossy().starts_with(&canonical))
                    .unwrap_or(false)
        })
        .collect()
}

#[derive(Debug, serde::Serialize)]
struct KgSearchResult {
    score: f64,
    element_id: String,
    name: String,
    labels: Vec<String>,
    description: String,
}

pub async fn run_search_kg(query: &str, limit: usize) -> Result<()> {
    embed::health().await?;

    let vector = embed::embed_batch(vec![query.to_string()]).await?
        .into_iter().next().unwrap_or_default();

    let results = match qdrant::search("kg_nodes", vector, limit).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("错误: 无法搜索 kg_nodes 集合\n原因: {}\n\n请先运行 `dt kg-sync` 同步知识图谱节点到向量库。", e);
            return Ok(());
        }
    };

    let items: Vec<KgSearchResult> = results.into_iter().map(|r| {
        let p = &r.payload;
        KgSearchResult {
            score: r.score,
            element_id: p.get("elementId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            labels: p.get("labels").and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            description: p.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        }
    }).collect();

    for (_i, item) in items.iter().enumerate() {
        println!(
            "[{:.3}] {} [{}] {}",
            item.score,
            item.name,
            item.labels.join(", "),
            &item.description.chars().take(80).collect::<String>()
        );
        println!("    elementId: {}", item.element_id);
    }

    Ok(())
}
