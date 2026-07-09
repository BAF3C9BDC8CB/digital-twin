use std::path::Path;
use anyhow::Result;

use crate::config;
use crate::client::{neo4j, qdrant, embed};
use crate::index::convert::{build_qdrant_point, split_class_path};
use crate::index::callgraph;
use crate::index::pipeline;
use crate::scanner;
use crate::parser::Parser;

pub async fn run_update(root: &str, project: &str, file: &str) -> Result<()> {
    let fpath = Path::new(root).join(file);
    if !fpath.exists() {
        eprintln!("[error] file not found: {}", fpath.display());
        return Ok(());
    }
    let content = std::fs::read_to_string(&fpath)?;
    let rel = scanner::rel_path(root, &fpath);
    let collection = format!("{}_methods", project);

    embed::health().await?;
    neo4j::health().await?;
    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    println!("[update] {}: removing old data...", rel);
    if let Err(e) = qdrant::delete_points_by_filter(&collection, &rel).await {
        eprintln!("[warn] qdrant delete {}: {}", rel, e);
    }
    if let Err(e) = neo4j::delete_methods_by_file(project, &rel).await {
        eprintln!("[warn] neo4j delete {}: {}", rel, e);
    }

    let mut p = Parser::new()?;
    let parsed = p.parse_file(&fpath.to_string_lossy(), project, root)?;

    if parsed.methods.is_empty() {
        println!("[update] {}: no methods to index", rel);
        let db = pipeline::init_sqlite()?;
        let hash = crate::common::hash::sha1_hex(&content);
        db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64],
        )?;
        return Ok(());
    }

    let texts: Vec<String> = parsed.methods.iter().map(|m| m.search_text.clone()).collect();
    println!("[update] {}: embedding {} methods...", rel, texts.len());
    let vectors = embed::embed_batch(texts).await?;

    let qdrant_points: Vec<_> = parsed.methods.iter().enumerate()
        .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
        .collect();
    let neo4j_nodes: Vec<neo4j::MethodNode> = parsed.methods.iter().map(|m| m.into()).collect();

    qdrant::upsert_points(&collection, qdrant_points).await?;
    neo4j::write_methods_batch(&neo4j_nodes).await?;

    // Class relationships (H6: only match by class_name, not file_path)
    if !parsed.classes.is_empty() {
        let mut class_map: std::collections::HashMap<String, neo4j::ClassBatchEntry> = std::collections::HashMap::new();
        for c in &parsed.classes {
            class_map.entry(c.class_id.clone()).or_insert_with(|| {
                let (pkg, _) = split_class_path(&c.name, &c.file_path);
                neo4j::ClassBatchEntry {
                    class_id: c.class_id.clone(),
                    name: c.name.clone(),
                    project: project.to_string(),
                    file_path: c.file_path.clone(),
                    package_or_module: pkg,
                    method_ids: parsed.methods.iter()
                        .filter(|m| m.class_name == c.name)
                        .map(|m| m.method_id.clone())
                        .collect(),
                }
            });
        }
        let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
        neo4j::write_classes_batch(&class_vec).await?;
    }

    // Incremental CALLS (H4)
    let rels = callgraph::rebuild_calls_for_files(project, &[rel.clone()]).await?;
    println!("[rels] created {} CALLS relationships", rels);

    let db = pipeline::init_sqlite()?;
    let hash = crate::common::hash::sha1_hex(&content);
    db.execute(
        "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64, parsed.methods.len()],
    )?;

    println!("[done] {} updated ({} methods)", rel, parsed.methods.len());
    Ok(())
}
