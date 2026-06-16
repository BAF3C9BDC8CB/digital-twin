use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::Result;

use crate::config;
use crate::client::{neo4j, qdrant, embed};
use crate::index::convert::{build_qdrant_point, split_class_path};
use crate::index::callgraph;
use crate::scanner;
use crate::parser::Parser;

pub async fn run_build(root: &str, project: &str) -> Result<()> {
    println!("[build] {} @ {}", project, root);

    embed::health().await?;
    neo4j::health().await?;
    neo4j::ensure_schema().await?;

    let collection = format!("{}_methods", project);
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    let files = scanner::collect_files(root);
    println!("[scan] found {} files", files.len());

    if files.is_empty() {
        println!("[done] no files to process");
        return Ok(());
    }

    // Compute SHA1 for each file
    let mut file_hashes: Vec<(String, String, u64)> = Vec::with_capacity(files.len());
    for f in &files {
        let rel = scanner::rel_path(root, f);
        let content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let hash = crate::common::hash::sha1_hex(&content);
        let mtime = std::fs::metadata(f).ok().map(|m| {
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0)
        }).unwrap_or(0);
        file_hashes.push((rel, hash, mtime));
    }

    // Compare with SQLite
    let db = init_sqlite()?;
    let mut changed: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut unchanged = 0usize;

    for (rel, hash, _mtime) in &file_hashes {
        let prev: Option<(String, f64)> = db.query_row(
            "SELECT file_sha1, file_mtime FROM file_snapshots WHERE file_path = ?1 AND project = ?2",
            rusqlite::params![rel.to_string(), project.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
        ).ok();

        match prev {
            Some((prev_hash, _)) if prev_hash == *hash => {
                unchanged += 1;
            }
            _ => changed.push(rel.clone()),
        }
    }

    // Detect deleted files
    let prj_str = project.to_string();
    let mut stmt = db.prepare("SELECT file_path FROM file_snapshots WHERE project = ?1")?;
    let db_files: Vec<String> = stmt.query_map(rusqlite::params![prj_str], |row| {
        row.get::<_, String>(0)
    })?.filter_map(|r| r.ok()).collect();

    let current_set: HashSet<&str> = file_hashes.iter().map(|(r, _, _)| r.as_str()).collect();
    for f in &db_files {
        if !current_set.contains(f.as_str()) {
            deleted.push(f.clone());
        }
    }

    println!("[compare] {} unchanged, {} changed/new, {} deleted", unchanged, changed.len(), deleted.len());

    // Delete removed files
    for f in &deleted {
        println!("  [delete] {}", f);
        if let Err(e) = qdrant::delete_points_by_filter(&collection, f).await {
            eprintln!("[warn] qdrant delete {}: {}", f, e);
        }
        if let Err(e) = neo4j::delete_methods_by_file(project, f).await {
            eprintln!("[warn] neo4j delete {}: {}", f, e);
        }
        let _ = db.execute(
            "DELETE FROM file_snapshots WHERE file_path = ?1 AND project = ?2",
            rusqlite::params![f.to_string(), project.to_string()],
        );
    }

    // Re-index changed files
    let t0 = Instant::now();
    let mut total_methods = 0usize;
    let mut total_files = 0usize;
    let all_changed: Vec<String> = changed.clone();

    for rel in &changed {
        let fpath = Path::new(root).join(rel);
        if !fpath.exists() { continue; }
        let content = match std::fs::read_to_string(&fpath) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Delete old data
        if let Err(e) = qdrant::delete_points_by_filter(&collection, rel).await {
            eprintln!("[warn] qdrant delete {}: {}", rel, e);
        }
        if let Err(e) = neo4j::delete_methods_by_file(project, rel).await {
            eprintln!("[warn] neo4j delete {}: {}", rel, e);
        }

        // Parse
        let mut p = Parser::new()?;
        let parsed = p.parse_file(&fpath.to_string_lossy(), project, root)?;

        if parsed.methods.is_empty() && parsed.classes.is_empty() {
            let hash = crate::common::hash::sha1_hex(&content);
            let mtime = std::fs::metadata(&fpath).ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0);
            db.execute(
                "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 0, datetime('now'))",
                rusqlite::params![rel.to_string(), project.to_string(), hash, mtime],
            )?;
            continue;
        }

        // Embed + index
        let methods = &parsed.methods;
        let texts: Vec<String> = methods.iter().map(|m| m.search_text.clone()).collect();
        let vectors = embed::embed_batch(texts).await?;

        let qdrant_points: Vec<_> = methods.iter().enumerate()
            .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
            .collect();
        let neo4j_nodes: Vec<neo4j::MethodNode> = methods.iter().map(|m| m.into()).collect();

        if !qdrant_points.is_empty() {
            qdrant::upsert_points(&collection, qdrant_points).await?;
        }
        if !neo4j_nodes.is_empty() {
            neo4j::write_methods_batch(&neo4j_nodes).await?;
        }

        // Class relationships (H6: only match by class_name)
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
                for m in methods {
                    if m.class_name == c.name {
                        entry.method_ids.push(m.method_id.clone());
                    }
                }
            }
            let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
            neo4j::write_classes_batch(&class_vec).await?;
        }

        // Update SQLite snapshot
        let hash = crate::common::hash::sha1_hex(&content);
        let mtime = std::fs::metadata(&fpath).ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()).unwrap_or(0);
        db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, mtime, methods.len()],
        )?;

        total_methods += methods.len();
        total_files += 1;
    }

    println!("[index] processed {} methods in {} files ({:.1}s)", total_methods, total_files, t0.elapsed().as_secs_f64());

    // Incremental CALLS (H4)
    let rels = callgraph::rebuild_calls_for_files(project, &all_changed).await?;
    println!("[rels] created {} CALLS relationships", rels);

    println!("[done] build complete");
    Ok(())
}

pub fn init_sqlite() -> Result<rusqlite::Connection> {
    let path = config::SQLITE_PATH;
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let db = rusqlite::Connection::open(path)?;
    db.execute_batch("
        CREATE TABLE IF NOT EXISTS file_snapshots (
            file_path TEXT NOT NULL,
            project TEXT NOT NULL,
            file_sha1 TEXT NOT NULL,
            file_mtime REAL NOT NULL DEFAULT 0,
            method_count INTEGER DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (file_path, project)
        );
    ")?;
    Ok(db)
}
