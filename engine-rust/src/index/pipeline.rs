// Shared parallel pipeline for dt build / dt index.
// Both commands share: parallel hashing, parallel parsing, batch embed + write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rusqlite::params;

use crate::client::{embed, neo4j, qdrant};
use crate::index::convert::{build_qdrant_point, split_class_path};
use crate::parser::{Parser, ParsedFile};
use crate::scanner;

// ── chunk sizes ───────────────────────────────────────────────────────────
pub const QDRANT_CHUNK: usize = 1000;
pub const NEO4J_CHUNK: usize = 2000;

// ── parallel SHA1 ─────────────────────────────────────────────────────────

/// 并行计算所有文件的 SHA1（CPU 密集，使用 std::thread::scope）。
pub fn compute_hashes_parallel(files: &[PathBuf], root: &str) -> Vec<(String, String, u64)> {
    let num_threads = thread_count().min(files.len().max(1));
    let chunk_size = files.len().div_ceil(num_threads);

    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk_size)
            .map(|chunk| {
                s.spawn(move || {
                    let mut results: Vec<(String, String, u64)> =
                        Vec::with_capacity(chunk.len());
                    for f in chunk {
                        let rel = scanner::rel_path(root, f);
                        let content = match std::fs::read_to_string(f) {
                            Ok(c) => c,
                            Err(_) => continue,
                        };
                        let hash = crate::common::hash::sha1_hex(&content);
                        let mtime = std::fs::metadata(f)
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| {
                                t.duration_since(std::time::UNIX_EPOCH).ok()
                            })
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        results.push((rel, hash, mtime));
                    }
                    results
                })
            })
            .collect();

        let mut all = Vec::with_capacity(files.len());
        for h in handles {
            all.extend(h.join().unwrap());
        }
        all
    })
}

// ── parallel parse ────────────────────────────────────────────────────────

/// 并行解析文件列表，每个线程复用 Parser 实例。
/// 使用 `std::thread::scope` 避免 tree_sitter::Parser 的 !Send 限制。
pub fn parse_files_parallel(
    root: &str,
    project: &str,
    files: &[String],
) -> Vec<Result<(String, ParsedFile)>> {
    if files.is_empty() {
        return vec![];
    }

    let num_threads = thread_count().min(files.len());
    let chunk_size = files.len().div_ceil(num_threads);
    let root = Arc::new(root.to_string());
    let project = Arc::new(project.to_string());

    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk_size)
            .map(|chunk| {
                let root = Arc::clone(&root);
                let project = Arc::clone(&project);
                s.spawn(move || {
                    let mut parser = match Parser::new() {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!(
                                "[warn] parser init failed, skipping {} files: {}",
                                chunk.len(),
                                e
                            );
                            return chunk
                                .iter()
                                .map(|_rel| Err(anyhow::anyhow!("parser init: {}", e)))
                                .collect::<Vec<_>>();
                        }
                    };
                    chunk
                        .iter()
                        .map(|rel| {
                            let fpath = Path::new(root.as_str()).join(rel);
                            if !fpath.exists() {
                                return Err(anyhow::anyhow!("file not found: {}", rel));
                            }
                            parser
                                .parse_file(
                                    &fpath.to_string_lossy(),
                                    project.as_str(),
                                    root.as_str(),
                                )
                                .map(|pf| (rel.clone(), pf))
                                .map_err(|e| anyhow::anyhow!("{}: {}", rel, e))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();

        let mut all = Vec::with_capacity(files.len());
        for h in handles {
            all.extend(h.join().unwrap());
        }
        all
    })
}

// ── batch embed + write ───────────────────────────────────────────────────

/// 从解析结果中收集全部 methods → 批量 embed → 分块写入 Qdrant + Neo4j。
/// 返回成功写入的 method 数量。
pub async fn embed_and_write_all(
    collection: &str,
    parsed_files: &[(String, ParsedFile)],
) -> Result<usize> {
    let total_methods: usize = parsed_files
        .iter()
        .map(|(_, pf)| pf.methods.len())
        .sum();
    if total_methods == 0 {
        return Ok(0);
    }

    let mut all_texts: Vec<String> = Vec::with_capacity(total_methods);
    let mut method_entries: Vec<&crate::models::MethodBlock> =
        Vec::with_capacity(total_methods);

    for (_, pf) in parsed_files {
        for m in &pf.methods {
            all_texts.push(m.search_text.clone());
            method_entries.push(m);
        }
    }

    // batch embed
    let vectors = embed::embed_batch(all_texts).await?;

    // Qdrant upsert (chunked)
    let qdrant_points: Vec<_> = method_entries
        .iter()
        .enumerate()
        .filter_map(|(i, m)| vectors.get(i).map(|v| build_qdrant_point(m, v)))
        .collect();
    for chunk in qdrant_points.chunks(QDRANT_CHUNK) {
        qdrant::upsert_points(collection, chunk.to_vec()).await?;
    }

    // Neo4j write (chunked)
    let neo4j_nodes: Vec<neo4j::MethodNode> =
        method_entries.iter().map(|m| (*m).into()).collect();
    for chunk in neo4j_nodes.chunks(NEO4J_CHUNK) {
        neo4j::write_methods_batch(chunk).await?;
    }

    Ok(vectors.len())
}

// ── class relationships ───────────────────────────────────────────────────

/// 汇总所有文件的 class → method 映射。
pub fn build_class_entries(
    parsed_files: &[(String, ParsedFile)],
    project: &str,
) -> Vec<neo4j::ClassBatchEntry> {
    let mut class_map: HashMap<String, neo4j::ClassBatchEntry> = HashMap::new();

    for (_, pf) in parsed_files {
        for c in &pf.classes {
            let entry = class_map
                .entry(c.class_id.clone())
                .or_insert_with(|| {
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
            for m in &pf.methods {
                if m.class_name == c.name {
                    entry.method_ids.push(m.method_id.clone());
                }
            }
        }
    }

    class_map.into_values().collect()
}

// ── SQLite snapshots ──────────────────────────────────────────────────────

/// 批量写入 SQLite 快照，使用预计算的 hash/mtime 避免重复 I/O。
pub fn write_sqlite_snapshots(
    db: &rusqlite::Connection,
    project: &str,
    parsed_files: &[(String, ParsedFile)],
    file_hashes: &[(String, String, u64)], // (rel, sha1, mtime)
) -> Result<()> {
    let hash_map: HashMap<&str, (&str, u64)> = file_hashes
        .iter()
        .map(|(rel, sha1, mtime)| (rel.as_str(), (sha1.as_str(), *mtime)))
        .collect();

    for (rel, pf) in parsed_files {
        let (sha1, mtime) = match hash_map.get(rel.as_str()) {
            Some(v) => v,
            None => {
                eprintln!("[warn] no hash for {}, skipping SQLite snapshot", rel);
                continue;
            }
        };
        let _ = db.execute(
            "INSERT OR REPLACE INTO file_snapshots \
             (file_path, project, file_sha1, file_mtime, method_count, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![rel, project, sha1, mtime, pf.methods.len()],
        );
    }
    Ok(())
}

// ── SQLite init ───────────────────────────────────────────────────────────

pub fn init_sqlite() -> Result<rusqlite::Connection> {
    let path = crate::config::SQLITE_PATH;
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let db = rusqlite::Connection::open(path)?;
    db.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS file_snapshots (
            file_path TEXT NOT NULL,
            project TEXT NOT NULL,
            file_sha1 TEXT NOT NULL,
            file_mtime REAL NOT NULL DEFAULT 0,
            method_count INTEGER DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (file_path, project)
        );
    ",
    )?;
    Ok(db)
}

// ── utility ───────────────────────────────────────────────────────────────

pub fn thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

// ── 项目语言/类型检测 ─────────────────────────────────────────────────────

/// 从解析结果统计语言分布，返回 (language, project_type)
pub fn detect_project_meta(parsed_files: &[(String, ParsedFile)]) -> (String, String) {
    use std::collections::HashMap;

    let mut lang_counts: HashMap<String, usize> = HashMap::new();
    for (_, pf) in parsed_files {
        for m in &pf.methods {
            *lang_counts.entry(m.language.clone()).or_default() += 1;
        }
        // 也统计只有 class 没有 method 的文件
        for c in &pf.classes {
            *lang_counts.entry(c.language.clone()).or_default() += 1;
        }
    }

    if lang_counts.is_empty() {
        return ("-".into(), "-".into());
    }

    // 找占比最高的语言
    let total: usize = lang_counts.values().sum();
    let (dominant, count) = lang_counts.iter().max_by_key(|(_, &c)| c).unwrap();
    let ratio = *count as f64 / total as f64;

    let language = lang_display(dominant);
    let project_type = classify_project_type(&lang_counts, ratio, total);

    (language, project_type)
}

fn lang_display(lang: &str) -> String {
    match lang {
        "java" => "Java",
        "javascript" => "JavaScript",
        "typescript" => "TypeScript",
        "python" => "Python",
        "go" => "Go",
        "rust" => "Rust",
        "php" => "PHP",
        _ => lang,
    }.to_string()
}

fn classify_project_type(counts: &std::collections::HashMap<String, usize>, dominant_ratio: f64, total: usize) -> String {
    if total < 10 {
        return "脚本/工具".into();
    }

    let frontend_keywords = ["javascript", "typescript", "vue"];
    let backend_keywords = ["java", "go", "rust", "python", "php"];

    let is_frontend = counts.iter().any(|(k, _)| frontend_keywords.contains(&k.as_str()));
    let is_backend = counts.iter().any(|(k, _)| backend_keywords.contains(&k.as_str()));

    if dominant_ratio > 0.8 {
        // 单一语言占绝对主导
        let lang = counts.iter().max_by_key(|(_, &c)| c).unwrap().0.as_str();
        match lang {
            "javascript" | "typescript" => "前端".into(),
            "vue" => "前端".into(),
            "html" => "静态前端".into(),
            _ => format!("{}后端", lang_display(lang)),
        }
    } else if is_frontend && is_backend {
        "全栈".into()
    } else if is_frontend {
        "前端".into()
    } else {
        "后端".into()
    }
}
