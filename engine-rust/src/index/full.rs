use std::time::Instant;

use anyhow::Result;

use crate::client::{neo4j, qdrant, embed};
use crate::config;
use crate::index::pipeline;
use crate::index::callgraph;
use crate::scanner;

// ── entry point ────────────────────────────────────────────────────────────
pub async fn run_index(root: &str, project: &str) -> Result<()> {
    println!("[index] {} @ {} (full rebuild)", project, root);

    let wall_t0 = Instant::now();

    // ── 0. health checks ───────────────────────────────────────────────
    let (_model, dim) = embed::health().await?;
    neo4j::health().await?;

    let collection = format!("{}_methods", project);

    // ── 1. wipe old data ───────────────────────────────────────────────
    println!("[reindex] clearing old data...");
    if let Err(e) = qdrant::delete_collection(&collection).await {
        eprintln!("[warn] qdrant delete_collection: {}", e);
    }
    neo4j::delete_all_methods(project).await?;

    neo4j::ensure_schema().await?;
    qdrant::ensure_collection(&collection, dim).await?;

    // ── 2. scan files ──────────────────────────────────────────────────
    let files = scanner::collect_files(root);
    println!("[scan] found {} files", files.len());
    if files.is_empty() {
        println!("[done] no files to process");
        return Ok(());
    }

    // ── 3. parallel SHA1 (提前算好，供后续 SQLite 复用) ──────────────
    let t_hash = Instant::now();
    let file_hashes = pipeline::compute_hashes_parallel(&files, root);
    println!(
        "[hash] {} files ({:.1}s)",
        file_hashes.len(),
        t_hash.elapsed().as_secs_f64()
    );

    // ── 4. collect all file paths for parsing ──────────────────────────
    let all_paths: Vec<String> = file_hashes.iter().map(|(rel, _, _)| rel.clone()).collect();

    // ── 5. parallel parse ──────────────────────────────────────────────
    let t_parse = Instant::now();
    let results = pipeline::parse_files_parallel(root, project, &all_paths);
    let ok = results.iter().filter(|r| r.is_ok()).count();
    if ok < all_paths.len() {
        eprintln!(
            "[warn] {} / {} files failed to parse",
            all_paths.len() - ok,
            all_paths.len()
        );
    }
    let parsed_files: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
    println!(
        "[parse] {} / {} files ({:.1}s)",
        ok, all_paths.len(),
        t_parse.elapsed().as_secs_f64()
    );

    let method_count: usize = parsed_files
        .iter()
        .map(|(_, pf)| pf.methods.len())
        .sum();
    println!("[methods] {} total", method_count);

    // ── 6. batch embed + write ─────────────────────────────────────────
    let t_write = Instant::now();
    let written = pipeline::embed_and_write_all(&collection, &parsed_files).await?;
    println!(
        "[write] {} vectors → qdrant + neo4j ({:.1}s)",
        written,
        t_write.elapsed().as_secs_f64()
    );

    // ── 7. class relationships ─────────────────────────────────────────
    let t_class = Instant::now();
    let class_entries = pipeline::build_class_entries(&parsed_files, project);
    if !class_entries.is_empty() {
        neo4j::write_classes_batch(&class_entries).await?;
    }
    println!(
        "[class] {} classes ({:.1}s)",
        class_entries.len(),
        t_class.elapsed().as_secs_f64()
    );

    // ── 8. SQLite snapshots ────────────────────────────────────────────
    let db = pipeline::init_sqlite()?;
    pipeline::write_sqlite_snapshots(&db, project, &parsed_files, &file_hashes)?;

    // ── 9. full CALLS rebuild ──────────────────────────────────────────
    let t_rels = Instant::now();
    let rels = callgraph::rebuild_calls_for_project(project).await?;
    println!(
        "[rels] created {} CALLS relationships ({:.1}s)",
        rels,
        t_rels.elapsed().as_secs_f64()
    );

    // ── 10. project meta ────────────────────────────────────────────────
    let (lang, ptype) = pipeline::detect_project_meta(&parsed_files);
    let _ = neo4j::write_project_meta(project, &lang, &ptype).await;
    println!("[meta] {} / {}", lang, ptype);

    println!(
        "[done] index complete: {} methods in {} files ({:.1}s total)",
        method_count, ok,
        wall_t0.elapsed().as_secs_f64()
    );

    // 同步到 config.yaml
    let _ = config::sync_project_to_config(project, root);

    Ok(())
}
