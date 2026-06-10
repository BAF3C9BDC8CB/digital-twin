use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use sha1::{Digest, Sha1};
use serde_json::json;

use crate::{config, embed, neo4j, qdrant, scanner};
use crate::parser::Parser;
use crate::build::{init_sqlite, method_id_to_u64, build_payload, split_class_path};

pub async fn run_update(root: &str, project: &str, file: &str) -> Result<()> {
    let fpath = Path::new(root).join(file);
    if !fpath.exists() {
        println!("[错误] 文件不存在: {}", fpath.display());
        return Ok(());
    }
    let content = std::fs::read_to_string(&fpath)?;
    let rel = scanner::rel_path(root, &fpath);
    let collection = format!("{}_methods", project);

    embed::health().await?;
    neo4j::health().await?;
    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    // Delete old
    println!("[更新] {}: 删除旧数据...", rel);
    let _ = qdrant::delete_points_by_filter(&collection, &rel).await;
    let _ = neo4j::delete_methods_by_file(project, &rel).await;

    // Parse
    let mut p = Parser::new()?;
    let parsed = p.parse_file(&fpath.to_string_lossy(), project, root)?;

    if parsed.methods.is_empty() {
        println!("[更新] {}: 无方法需要索引", rel);
        let db = init_sqlite()?;
        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hash = hex::encode(hasher.finalize());
        db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64],
        )?;
        return Ok(());
    }

    // Embed
    let texts: Vec<String> = parsed.methods.iter().map(|m| m.search_text.clone()).collect();
    println!("[更新] {}: 嵌入 {} 个方法...", rel, texts.len());
    let vectors = embed::embed_batch(texts).await?;

    let mut method_to_point: HashMap<&str, u64> = HashMap::new();
    for m in &parsed.methods {
        method_to_point.insert(&m.method_id, method_id_to_u64(&m.method_id));
    }

    let mut qdrant_points = Vec::with_capacity(vectors.len());
    let mut neo4j_nodes = vec![];

    for (i, m) in parsed.methods.iter().enumerate() {
        let payload = build_payload(m);
        if let Some(v) = vectors.get(i) {
            let point_id = json!(method_to_point[&m.method_id as &str]);
            qdrant_points.push((point_id, v.clone(), payload));
        }
        neo4j_nodes.push(neo4j::MethodNode {
            method_id: m.method_id.clone(),
            project: project.to_string(),
            file_path: m.file_path.clone(),
            language: m.language.clone(),
            package_or_module: m.package_or_module.clone(),
            class_name: m.class_name.clone(),
            name: m.name.clone(),
            signature: m.signature.clone(),
            params: m.params.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "),
            return_type: m.return_type.clone(),
            start_line: m.start_line as i64,
            end_line: m.end_line as i64,
            calls: m.calls.clone(),
        });
    }

    qdrant::upsert_points(&collection, qdrant_points).await?;
    neo4j::write_methods_batch(&neo4j_nodes).await?;

    // Class relationships
    if !parsed.classes.is_empty() {
        let mut class_map: HashMap<String, neo4j::ClassBatchEntry> = HashMap::new();
        for c in &parsed.classes {
            let entry = class_map.entry(c.class_id.clone()).or_insert_with(|| {
                let (pkg, _) = split_class_path(&c.name, &c.file_path);
                neo4j::ClassBatchEntry {
                    class_id: c.class_id.clone(),
                    name: c.name.clone(),
                    project: project.to_string(),
                    file_path: c.file_path.clone(),
                    package_or_module: pkg,
                    method_ids: vec![],
                }
            });
            for m in &parsed.methods {
                if m.class_name == c.name || m.file_path == c.file_path {
                    entry.method_ids.push(m.method_id.clone());
                }
            }
        }
        let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
        neo4j::write_classes_batch(&class_vec).await?;
    }

    // Rebuild CALLS
    let rels = neo4j::create_call_relationships(project).await?;
    println!("[关系] 创建了 {} 条 CALLS 关系", rels);

    // Update SQLite
    let db = init_sqlite()?;
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    let hash = hex::encode(hasher.finalize());
    db.execute(
        "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64, parsed.methods.len()],
    )?;

    println!("[完成] {} 已更新 ({} 方法)", rel, parsed.methods.len());
    Ok(())
}
