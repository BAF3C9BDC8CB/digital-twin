//! Backup module — re-exports from dt-backup components and orchestrates
//! backup / restore / list / verify operations.
//!
//! This file integrates the former dt-backup crate into the single-crate
//! architecture.  Individual backup targets have been split into separate
//! files (backup_neo4j, backup_qdrant, backup_sqlite, backup_verify).

pub mod neo4j {
    //! Neo4j backup helpers.
    pub use crate::interfaces::cli::backup_neo4j::*;
}
pub mod qdrant {
    //! Qdrant backup helpers.
    pub use crate::interfaces::cli::backup_qdrant::*;
}
pub mod sqlite {
    //! SQLite backup helpers.
    pub use crate::interfaces::cli::backup_sqlite::*;
}
pub mod verify {
    //! Backup verification helpers.
    pub use crate::interfaces::cli::backup_verify::*;
}

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

/// Backup report returned after a successful backup run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupReport {
    pub location: PathBuf,
    pub date: String,
    pub targets: BackupTargets,
    pub duration_seconds: f64,
}

/// Per-target backup status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupTargets {
    pub neo4j: bool,
    pub neo4j_size_bytes: u64,
    pub qdrant: bool,
    pub qdrant_size_bytes: u64,
    pub sqlite: bool,
    pub sqlite_size_bytes: u64,
}

/// A backup entry shown in the list output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub date: String,
    pub total_size_bytes: u64,
    pub file_count: usize,
}

/// Verification report after checksum checking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub backup_dir: PathBuf,
    pub date: String,
    pub all_valid: bool,
    pub files: Vec<VerifyFileResult>,
    pub duration_seconds: f64,
}

/// Per-file verification result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFileResult {
    pub file_name: String,
    pub valid: bool,
    pub expected: String,
    pub actual: String,
}

/// Default backup root directory on the host.
const BACKUP_ROOT: &str = "/var/backups/digital-twin";

/// Create a new backup.
///
/// 1. Creates a date-stamped directory under `BACKUP_ROOT`.
/// 2. Dumps Neo4j, snapshots Qdrant collections, copies SQLite.
/// 3. Generates checksums.
/// 4. Returns a `BackupReport`.
pub async fn create_backup() -> Result<BackupReport> {
    let start = Instant::now();
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let backup_dir = PathBuf::from(BACKUP_ROOT).join(&date);

    tracing::info!("dt_backup: creating backup for {date} at {}", backup_dir.display());

    // Ensure backup directory exists
    tokio::fs::create_dir_all(&backup_dir).await?;

    // ---- Back up each component ----
    let (neo4j_ok, neo4j_size) =
        crate::interfaces::cli::backup_neo4j::dump_graph(&backup_dir).await?;
    let (qdrant_ok, qdrant_size) =
        crate::interfaces::cli::backup_qdrant::snapshot_collections(&backup_dir).await?;
    let (sqlite_ok, sqlite_size) =
        crate::interfaces::cli::backup_sqlite::copy_database(&backup_dir).await?;

    // ---- Generate checksums ----
    if let Err(e) = crate::interfaces::cli::backup_verify::generate_checksums(&backup_dir).await {
        tracing::warn!("checksum generation failed: {e}");
    }

    let duration = start.elapsed().as_secs_f64();

    let report = BackupReport {
        location: backup_dir,
        date,
        targets: BackupTargets {
            neo4j: neo4j_ok,
            neo4j_size_bytes: neo4j_size,
            qdrant: qdrant_ok,
            qdrant_size_bytes: qdrant_size,
            sqlite: sqlite_ok,
            sqlite_size_bytes: sqlite_size,
        },
        duration_seconds: duration,
    };

    Ok(report)
}

/// Restore a backup by date.
///
/// Looks up `{BACKUP_ROOT}/{date}/` and restores each component.
pub async fn restore_backup(date: &str) -> Result<()> {
    let backup_dir = PathBuf::from(BACKUP_ROOT).join(date);

    if !backup_dir.exists() {
        eprintln!("Backup directory not found: {}", backup_dir.display());
        return Ok(());
    }

    tracing::info!("dt_backup: restoring backup from {date}");

    // ---- Restore each component ----
    crate::interfaces::cli::backup_neo4j::restore_graph(&backup_dir).await?;
    crate::interfaces::cli::backup_qdrant::restore_collections(&backup_dir).await?;
    crate::interfaces::cli::backup_sqlite::restore_database(&backup_dir).await?;

    tracing::info!("dt_backup: restore complete for {date}");

    Ok(())
}

/// List available backups.
///
/// Scans `BACKUP_ROOT` for date-stamped directories and returns
/// summary information for each.
pub async fn list_backups() -> Result<Vec<BackupEntry>> {
    let root = PathBuf::from(BACKUP_ROOT);

    if !root.exists() {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&root).await?;

    while let Some(entry) = read_dir.next_entry().await? {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Only include date-formatted directories (YYYY-MM-DD)
        if dir_name.len() != 10 || dir_name.chars().filter(|c| *c == '-').count() != 2 {
            continue;
        }

        let mut total_size: u64 = 0;
        let mut file_count: usize = 0;

        let mut dir_entries = tokio::fs::read_dir(&path).await?;
        while let Some(file) = dir_entries.next_entry().await? {
            if file.path().is_file() {
                if let Ok(meta) = file.metadata().await {
                    total_size += meta.len();
                    file_count += 1;
                }
            }
        }

        entries.push(BackupEntry {
            date: dir_name.to_string(),
            total_size_bytes: total_size,
            file_count,
        });
    }

    // Sort by date descending
    entries.sort_by(|a, b| b.date.cmp(&a.date));

    Ok(entries)
}

/// Verify checksums of backup files by date.
pub async fn verify_backup_files(date: &str) -> Result<VerifyReport> {
    let backup_dir = PathBuf::from(BACKUP_ROOT).join(date);

    if !backup_dir.exists() {
        eprintln!("Backup directory not found: {}", backup_dir.display());
        return Ok(VerifyReport {
            backup_dir,
            date: date.to_string(),
            all_valid: true,
            files: vec![],
            duration_seconds: 0.0,
        });
    }

    crate::interfaces::cli::backup_verify::verify_backup(&backup_dir).await
}
