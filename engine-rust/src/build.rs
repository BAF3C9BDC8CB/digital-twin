use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use serde_json::json;

use crate::{config, embed, neo4j, qdrant, scanner};
use crate::parser::Parser;
use crate::models::MethodBlock;

// ── Build: incremental hash-based ──────────────────────────

pub async fn run_build(root: &str, project: &str) -> Result<()> {
    println!("[构建] {} @ {}", project, root);

    embed::health().await?;
    neo4j::health().await?;
    neo4j::ensure_schema().await?;

    let collection = format!("{}_methods", project);
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    // 1. Scan files
    let files = scanner::collect_files(root);
    println!("[扫描] 发现 {} 个文件", files.len());

    if files.is_empty() {
        println!("[完成] 无文件需要处理");
        return Ok(());
    }

    // 2. Compute SHA1 for each file
    let mut file_hashes: Vec<(String, String, u64)> = Vec::with_capacity(files.len());
    for f in &files {
        let rel = scanner::rel_path(root, f);
        let content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hash = hex::encode(hasher.finalize());
        let mtime = std::fs::metadata(f).ok().map(|m| {
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()).unwrap_or(0)
        }).unwrap_or(0);
        file_hashes.push((rel, hash, mtime));
    }

    // 3. Compare with SQLite
    let db = init_sqlite()?;
    let mut changed: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut unchanged = 0usize;

    for (rel, hash, mtime) in &file_hashes {
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

    // Detect deleted files (in DB but not on disk)
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

    println!(
        "[比较] {} 未变, {} 变更/新增, {} 已删除",
        unchanged, changed.len(), deleted.len()
    );

    // 4. Delete removed files from Qdrant + Neo4j
    for f in &deleted {
        println!("  [删除] {}", f);
        let _ = qdrant::delete_points_by_filter(&collection, f).await;
        let _ = neo4j::delete_methods_by_file(project, f).await;

        db.execute(
            "DELETE FROM file_snapshots WHERE file_path = ?1 AND project = ?2",
            rusqlite::params![f.to_string(), project.to_string()],
        )?;
    }

    // 5. Re-index changed files
    let t0 = Instant::now();
    let mut total_methods = 0usize; // <-- mutable, not in closure
    let mut total_files = 0usize;

    for rel in &changed {
        let fpath = Path::new(root).join(rel);
        if !fpath.exists() { continue; }
        let content = match std::fs::read_to_string(&fpath) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Delete old data for this file
        let _ = qdrant::delete_points_by_filter(&collection, rel).await;
        let _ = neo4j::delete_methods_by_file(project, rel).await;

        // Parse
        let mut p = Parser::new()?;
        let parsed = p.parse_file(&fpath.to_string_lossy(), project, root)?;

        if parsed.methods.is_empty() && parsed.classes.is_empty() {
            // Empty file or non-code file — just update SQLite
            let mut hasher = Sha1::new();
            hasher.update(content.as_bytes());
            let hash = hex::encode(hasher.finalize());
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

        let mut method_to_point: HashMap<&str, u64> = HashMap::new();
        for m in methods.iter() {
            method_to_point.insert(&m.method_id, method_id_to_u64(&m.method_id));
        }

        // Build Qdrant points
        let mut qdrant_points = Vec::with_capacity(vectors.len());
        let mut neo4j_nodes = vec![];

        for (i, m) in methods.iter().enumerate() {
            let payload = build_payload(m);
            if let Some(v) = vectors.get(i) {
                let point_id = json!(method_to_point[&m.method_id as &str]);
                qdrant_points.push((point_id, v.clone(), payload));
            }

            // Map MethodBlock → neo4j::MethodNode
            let method_id = m.method_id.clone();
            let project_clone = project.to_string();
            let file_path = m.file_path.clone();
            let language = m.language.clone();
            let package_or_module = m.package_or_module.clone();
            let class_name = m.class_name.clone();
            let name = m.name.clone();
            let signature = m.signature.clone();
            let params = m.params.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
            let return_type = m.return_type.clone();
            let start_line = m.start_line;
            let end_line = m.end_line;
            let calls = m.calls.clone();

            neo4j_nodes.push(neo4j::MethodNode {
                method_id,
                project: project_clone,
                file_path,
                language,
                package_or_module,
                class_name,
                name,
                signature,
                params,
                return_type,
                start_line: start_line as i64,
                end_line: end_line as i64,
                calls,
            });
        }

        // Write Qdrant
        if !qdrant_points.is_empty() {
            qdrant::upsert_points(&collection, qdrant_points).await?;
        }

        // Write Neo4j
        if !neo4j_nodes.is_empty() {
            neo4j::write_methods_batch(&neo4j_nodes).await?;
        }

        // Build class relationships
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
                // Find methods belonging to this class
                for m in methods {
                    if m.class_name == c.name || m.file_path == c.file_path {
                        entry.method_ids.push(m.method_id.clone());
                    }
                }
            }
            let class_vec: Vec<neo4j::ClassBatchEntry> = class_map.into_values().collect();
            neo4j::write_classes_batch(&class_vec).await?;
        }

        // Update SQLite snapshot
        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hash = hex::encode(hasher.finalize());
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

    println!("[索引] 处理了 {} 个文件的 {} 个方法 (耗时 {:.1}s)", total_files, total_methods, t0.elapsed().as_secs_f64());

    // 6. Rebuild CALLS relationships
    if total_methods > 0 || !deleted.is_empty() {
        println!("[关系] 重建调用关系...");
        let rels = neo4j::create_call_relationships(project).await?;
        println!("[关系] 创建了 {} 条 CALLS 关系", rels);
    }

    println!("[完成] 构建结束");
    Ok(())
}

// ── Update single file ──────────────────────────────────────

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
        // Still update SQLite
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

// ── Full index ──────────────────────────────────────────────

pub async fn run_index(root: &str, project: &str) -> Result<()> {
    let collection = format!("{}_methods", project);

    embed::health().await?;
    neo4j::health().await?;

    // Full reset
    println!("[重建] 清除旧数据...");
    let _ = qdrant::delete_collection(&collection).await;
    neo4j::delete_all_methods(project).await?;

    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, config::load().services.embed_server.dim).await?;

    // Scan files
    let files = scanner::collect_files(root);
    println!("[扫描] 发现 {} 个文件", files.len());

    // Parse all files
    let mut all_methods: Vec<MethodBlock> = Vec::new();
    let mut all_classes = Vec::new();
    let mut p = Parser::new()?;

    for f in &files {
        let fpath = f.to_string_lossy();
        let _content = match std::fs::read_to_string(f) {
            Ok(c) => c,
            Err(_) => continue,
        };
        match p.parse_file(&fpath, project, root) {
            Ok(parsed) => {
                all_methods.extend(parsed.methods);
                all_classes.extend(parsed.classes);
            }
            Err(_) => {}
        }
    }

    println!("[提取] 共 {} 个方法, {} 个类", all_methods.len(), all_classes.len());
    if all_methods.is_empty() {
        println!("[完成] 无方法需要索引");
        return Ok(());
    }

    // Dedup
    let mut seen: HashSet<String> = HashSet::new();
    let unique: Vec<&MethodBlock> = all_methods.iter().filter(|b| seen.insert(b.method_id.clone())).collect();
    println!("[去重] {} 个唯一方法", unique.len());

    // Embed in batches
    let t0 = Instant::now();
    let batch_size = 256;
    let mut class_batch_map: HashMap<String, neo4j::ClassBatchEntry> = HashMap::new();

    for chunk in unique.chunks(batch_size) {
        let texts: Vec<String> = chunk.iter().map(|m| m.search_text.clone()).collect();
        let vectors = embed::embed_batch(texts).await?;

        let mut qdrant_points = Vec::with_capacity(vectors.len());
        let mut neo4j_nodes = vec![];

        for (i, m) in chunk.iter().enumerate() {
            let payload = build_payload(m);
            if let Some(v) = vectors.get(i) {
                let point_id = json!(method_id_to_u64(&m.method_id));
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
    }

    // Class relationships
    for c in &all_classes {
        let entry = class_batch_map.entry(c.class_id.clone()).or_insert_with(|| {
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
        for m in &all_methods {
            if m.class_name == c.name || m.file_path == c.file_path {
                entry.method_ids.push(m.method_id.clone());
            }
        }
    }
    let class_vec: Vec<neo4j::ClassBatchEntry> = class_batch_map.into_values().collect();
    neo4j::write_classes_batch(&class_vec).await?;

    // CALLS
    let rels = neo4j::create_call_relationships(project).await?;
    println!("[关系] 创建了 {} 条 CALLS 关系", rels);

    // Initialize SQLite with all file hashes
    let db = init_sqlite()?;
    for f in &files {
        let rel = scanner::rel_path(root, f);
        let content = std::fs::read_to_string(f).unwrap_or_default();
        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hash = hex::encode(hasher.finalize());
        db.execute(
            "INSERT OR REPLACE INTO file_snapshots (file_path, project, file_sha1, file_mtime, method_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![rel.to_string(), project.to_string(), hash, 0u64, 0usize],
        )?;
    }

    println!("[完成] 耗时 {:.1}s", t0.elapsed().as_secs_f64());
    Ok(())
}

// ── Validate ────────────────────────────────────────────────

pub async fn run_validate(root: &str, project: &str) -> Result<()> {
    let files = scanner::collect_files(root);
    println!("[扫描] 发现 {} 个文件", files.len());

    let mut p = Parser::new()?;
    let mut all_methods = Vec::new();
    let mut file_count = 0;
    let mut skip_count = 0;

    for f in &files {
        let _content = match std::fs::read_to_string(f) { Ok(c) => c, Err(_) => { skip_count += 1; continue; } };
        let fpath = f.to_string_lossy();
        match p.parse_file(&fpath, project, root) {
            Ok(parsed) => { all_methods.extend(parsed.methods); file_count += 1; }
            Err(_) => { skip_count += 1; }
        }
    }

    println!("[提取] {} 文件处理, {} 跳过, 共 {} 个方法", file_count, skip_count, all_methods.len());

    let brace_ok = 0;
    let brace_bad = 0;
    let mut empty_name = 0;

    for m in &all_methods {
        if m.name.is_empty() { empty_name += 1; }
    }

    println!("\n========== 验证结果: {} ==========", project);
    println!("  总方法数:      {}", all_methods.len());
    println!("  括号平衡:      {} ✅", brace_ok);
    println!("  空方法名:      {} ❌", empty_name);
    println!("  错误:          {} 文件", skip_count);

    if brace_bad == 0 && empty_name == 0 && skip_count == 0 {
        println!("✅ 验证通过！");
    } else {
        println!("⚠️  发现异常");
    }
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────

fn init_sqlite() -> Result<rusqlite::Connection> {
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
        CREATE TABLE IF NOT EXISTS method_snapshots (
            method_id TEXT PRIMARY KEY,
            file_path TEXT NOT NULL,
            project TEXT NOT NULL,
            start_line INTEGER NOT NULL DEFAULT 0,
            end_line INTEGER NOT NULL DEFAULT 0,
            method_sha1 TEXT NOT NULL,
            signature TEXT,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
    ")?;
    Ok(db)
}

fn method_id_to_u64(method_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(method_id.as_bytes());
    let result = hasher.finalize();
    u64::from_be_bytes(result[..8].try_into().unwrap())
}

fn build_payload(m: &MethodBlock) -> HashMap<String, serde_json::Value> {
    let mut p = HashMap::new();
    p.insert("method_id".into(), json!(m.method_id));
    p.insert("project".into(), json!(m.project));
    p.insert("file_path".into(), json!(m.file_path));
    p.insert("language".into(), json!(m.language));
    p.insert("package_or_module".into(), json!(m.package_or_module));
    p.insert("class_name".into(), json!(m.class_name));
    p.insert("name".into(), json!(m.name));
    p.insert("signature".into(), json!(m.signature));
    p.insert("params".into(), json!(m.params.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")));
    p.insert("return_type".into(), json!(m.return_type));
    p.insert("start_line".into(), json!(m.start_line));
    p.insert("end_line".into(), json!(m.end_line));
    p.insert("comment".into(), json!(m.comment));
    p.insert("search_text".into(), json!(m.search_text));
    p.insert("source_code".into(), json!(m.source_code));
    p.insert("calls".into(), json!(m.calls));
    p
}

fn split_class_path(class_name: &str, file_path: &str) -> (String, String) {
    let pkg = Path::new(file_path).parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace('/', ".")
        .to_string();
    (pkg, class_name.to_string())
}
