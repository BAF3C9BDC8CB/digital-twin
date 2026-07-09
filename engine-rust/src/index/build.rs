use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use rusqlite::params;

use crate::client::{neo4j, qdrant, embed};
use crate::config;
use crate::index::pipeline;
use crate::index::callgraph;
use crate::scanner;

// ── entry point ────────────────────────────────────────────────────────────
pub async fn run_build(root: &str, project: &str) -> Result<()> {
    println!("[build] {} @ {}", project, root);

    // ── 0. health checks ───────────────────────────────────────────────
    let (model, dim) = embed::health().await?;
    println!("[embed] {} (dim={})", model, dim);
    neo4j::health().await?;
    neo4j::ensure_schema().await?;

    let collection = format!("{}_methods", project);
    qdrant::ensure_collection(&collection, dim).await?;

    let wall_t0 = Instant::now();

    // ── 1. scan files ──────────────────────────────────────────────────
    let files = scanner::collect_files(root);
    println!("[scan] found {} files", files.len());
    if files.is_empty() {
        println!("[done] no files to process");
        return Ok(());
    }

    // ── 2. parallel SHA1 ───────────────────────────────────────────────
    let t_hash = Instant::now();
    let file_hashes = pipeline::compute_hashes_parallel(&files, root);
    println!(
        "[hash] {} files ({:.1}s)",
        file_hashes.len(),
        t_hash.elapsed().as_secs_f64()
    );

    // ── 3. SQLite diff ─────────────────────────────────────────────────
    let db = pipeline::init_sqlite()?;
    let (changed, deleted, unchanged) = detect_changes(&db, project, &file_hashes)?;
    println!(
        "[compare] {} unchanged, {} changed/new, {} deleted",
        unchanged,
        changed.len(),
        deleted.len()
    );

    if changed.is_empty() && deleted.is_empty() {
        println!("[done] nothing to do");
        return Ok(());
    }

    // ── 4. batch delete (changed + deleted) ────────────────────────────
    let all_to_delete: Vec<String> = changed
        .iter()
        .chain(deleted.iter())
        .cloned()
        .collect();

    if !all_to_delete.is_empty() {
        let t_del = Instant::now();
        if let Err(e) = neo4j::delete_methods_by_files_batch(project, &all_to_delete).await {
            eprintln!("[warn] neo4j batch delete failed: {}", e);
        }
        if let Err(e) =
            qdrant::delete_points_by_files_batch(&collection, &all_to_delete).await
        {
            eprintln!("[warn] qdrant batch delete failed: {}", e);
        }
        for f in &deleted {
            let _ = db.execute(
                "DELETE FROM file_snapshots WHERE file_path = ?1 AND project = ?2",
                params![f, project],
            );
        }
        println!(
            "[delete] {} entries ({:.1}s)",
            all_to_delete.len(),
            t_del.elapsed().as_secs_f64()
        );
    }

    // ── 5. parallel parse ──────────────────────────────────────────────
    let t_parse = Instant::now();
    if !changed.is_empty() {
        let results = pipeline::parse_files_parallel(root, project, &changed);
        let ok = results.iter().filter(|r| r.is_ok()).count();
        if ok < changed.len() {
            eprintln!("[warn] {} / {} files failed to parse", changed.len() - ok, changed.len());
        }
        let parsed_files: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
        println!(
            "[parse] {} / {} files ({:.1}s)",
            ok, changed.len(),
            t_parse.elapsed().as_secs_f64()
        );

        let method_count = parsed_files.iter().map(|(_, pf)| pf.methods.len()).sum::<usize>();
        println!("[methods] {} total", method_count);

        // ── 6. batch embed + write ─────────────────────────────────────
        let t_write = Instant::now();
        let written = pipeline::embed_and_write_all(&collection, &parsed_files).await?;
        println!(
            "[write] {} vectors → qdrant + neo4j ({:.1}s)",
            written,
            t_write.elapsed().as_secs_f64()
        );

        // ── 7. class relationships ──────────────────────────────────────
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

        // ── 8. SQLite snapshots ─────────────────────────────────────────
        pipeline::write_sqlite_snapshots(&db, project, &parsed_files, &file_hashes)?;

        // ── 9. incremental CALLS ────────────────────────────────────────
        let rels = callgraph::rebuild_calls_for_files(project, &changed).await?;
        println!("[rels] created {} CALLS relationships", rels);

        // ── 10. project meta ────────────────────────────────────────────
        let (lang, ptype) = pipeline::detect_project_meta(&parsed_files);
        let _ = neo4j::write_project_meta(project, &lang, &ptype).await;
        println!("[meta] {} / {}", lang, ptype);
    }

    println!(
        "[done] build complete ({:.1}s total)",
        wall_t0.elapsed().as_secs_f64()
    );

    // 同步到 config.yaml
    let _ = config::sync_project_to_config(project, root);

    Ok(())
}

// ── SQLite change detection ────────────────────────────────────────────────

fn detect_changes(
    db: &rusqlite::Connection,
    project: &str,
    file_hashes: &[(String, String, u64)],
) -> Result<(Vec<String>, Vec<String>, usize)> {
    let mut changed: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut unchanged = 0usize;

    for (rel, hash, _mtime) in file_hashes {
        let prev: Option<(String, f64)> = db
            .query_row(
                "SELECT file_sha1, file_mtime FROM file_snapshots \
                 WHERE file_path = ?1 AND project = ?2",
                params![rel, project],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .ok();
        match prev {
            Some((prev_hash, _)) if prev_hash == *hash => unchanged += 1,
            _ => changed.push(rel.clone()),
        }
    }

    let mut stmt = db.prepare("SELECT file_path FROM file_snapshots WHERE project = ?1")?;
    let db_files: Vec<String> = stmt
        .query_map(params![project], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    let current_set: HashSet<&str> = file_hashes.iter().map(|(r, _, _)| r.as_str()).collect();
    for f in &db_files {
        if !current_set.contains(f.as_str()) {
            deleted.push(f.clone());
        }
    }

    Ok((changed, deleted, unchanged))
}
