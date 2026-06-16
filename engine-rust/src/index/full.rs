use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::Result;

use crate::config;
use crate::client::{neo4j, qdrant, embed};
use crate::index::convert::build_qdrant_point;
use crate::index::callgraph;
use crate::scanner;
use crate::parser::Parser;

pub async fn run_index(root: &str, project: &str) -> Result<()> {
    let collection = format!("{}_methods", project);

    embed::health().await?;
    neo4j::health().await?;

    println!("[reindex] clearing old data...");
    if let Err(e) = qdrant::delete_collection(&collection).await {
        eprintln!("[warn] qdrant delete_collection: {}", e);
    }
    neo4j::delete_all_methods(project).await?;

    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    let files = scanner::collect_files(root);
    println!("[scan] found {} files", files.len());

    let mut p = Parser::new()?;
    let mut total_methods = 0usize;
    let mut all_classes = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let t0 = Instant::now();

    let batch_size = 128usize;
    let mut batch_methods: Vec<crate::models::MethodBlock> = Vec::with_capacity(batch_size);

    for f in &files {
        let fpath = f.to_string_lossy();
        let _content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parsed = match p.parse_file(&fpath, project, root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        all_classes.extend(parsed.classes);

        for m in parsed.methods {
            if seen.insert(m.method_id.clone()) {
                batch_methods.push(m);
                if batch_methods.len() >= batch_size {
                    process_batch(project, &collection, &batch_methods).await?;
                    total_methods += batch_methods.len();
                    batch_methods.clear();
                }
            }
        }
    }
    if !batch_methods.is_empty() {
        process_batch(project, &collection, &batch_methods).await?;
        total_methods += batch_methods.len();
    }

    // Class relationships (H6: only match by class_name)
    if !all_classes.is_empty() {
        let mut class_map: HashMap<String, neo4j::ClassBatchEntry> = HashMap::new();
        for c in &all_classes {
            class_map.entry(c.class_id.clone()).or_insert_with(|| {
                let (pkg, _) = crate::index::convert::split_class_path(&c.name, &c.file_path);
                neo4j::ClassBatchEntry {
                    class_id: c.class_id.clone(),
                    name: c.name.clone(),
                    project: project.to_string(),
                    file_path: c.file_path.clone(),
                    package_or_module: pkg,
                    method_ids: vec![],
                }
            });
        }
        let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
        neo4j::write_classes_batch(&class_vec).await?;
    }

    let rels = callgraph::rebuild_calls_for_project(project).await?;
    println!("[rels] created {} CALLS relationships", rels);

    // Init SQLite cache
    let db = crate::index::build::init_sqlite()?;
    for f in &files {
        let rel = scanner::rel_path(root, f);
        let content = std::fs::read_to_string(f).unwrap_or_default();
        let hash = crate::common::hash::sha1_hex(&content);
        let _ = db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64, 0usize],
        );
    }

    println!("[done] {} methods indexed in {:.1}s", total_methods, t0.elapsed().as_secs_f64());
    Ok(())
}

async fn process_batch(
    _project: &str,
    collection: &str,
    methods: &[crate::models::MethodBlock],
) -> Result<()> {
    let texts: Vec<String> = methods.iter().map(|m| m.search_text.clone()).collect();
    let vectors = embed::embed_batch(texts).await?;

    let qdrant_points: Vec<_> = methods.iter().enumerate()
        .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
        .collect();

    let neo4j_nodes: Vec<neo4j::MethodNode> = methods.iter().map(|m| m.into()).collect();

    if !qdrant_points.is_empty() {
        qdrant::upsert_points(collection, qdrant_points).await?;
    }
    if !neo4j_nodes.is_empty() {
        neo4j::write_methods_batch(&neo4j_nodes).await?;
    }
    Ok(())
}
