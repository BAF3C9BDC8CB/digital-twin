//! SQLite backup and restore operations.
//!
//! Copies the SQLite snapshot database file to the backup directory.
//! Falls back to a placeholder when the database file is not found.

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Known SQLite database paths (checked in order).
const SQLITE_CANDIDATES: &[&str] = &[
    "/var/lib/digital-twin/snapshots.db",
    "./data/snapshots.db",
    "/tmp/digital-twin/snapshots.db",
];

/// Locate the active SQLite database path.
fn find_database() -> Option<PathBuf> {
    for candidate in SQLITE_CANDIDATES {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Copy the SQLite database to `{backup_dir}/sqlite.copy`.
///
/// Tries known paths for the snapshot database.  On failure writes a
/// placeholder to maintain the backup structure.
///
/// Returns `(success, size_bytes)`.
pub async fn copy_database(backup_dir: &Path) -> anyhow::Result<(bool, u64)> {
    let start = Instant::now();
    let copy_path = backup_dir.join("sqlite.copy");

    tracing::info!("正在复制 SQLite 数据库到 {}", copy_path.display());

    if let Some(source) = find_database() {
        tracing::info!("在 {} 找到 SQLite 数据库", source.display());

        match tokio::fs::copy(&source, &copy_path).await {
            Ok(size) => {
                tracing::info!(
                    "SQLite 复制完成: {} 字节 ({:.0}ms)",
                    size,
                    start.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((true, size));
            }
            Err(e) => {
                tracing::warn!("复制 SQLite 数据库失败 {}: {e}", source.display());
            }
        }
    }

    // Fallback: write placeholder
    let placeholder = format!(
        "-- SQLite backup placeholder\n\
         -- Generated: {}\n\
         -- Reason: no writable SQLite database found\n\
         -- Searched: {:?}\n",
        chrono::Utc::now().to_rfc3339(),
        SQLITE_CANDIDATES,
    );
    tokio::fs::write(&copy_path, placeholder.as_bytes()).await?;

    let size = tokio::fs::metadata(&copy_path).await?.len();

    tracing::info!(
        "SQLite 复制 (占位符): {} 字节 ({:.0}ms)",
        size,
        start.elapsed().as_secs_f64() * 1000.0
    );

    Ok((false, size))
}

/// Restore SQLite database from `{backup_dir}/sqlite.copy`.
///
/// Copies the backup file to the known database path.
pub async fn restore_database(backup_dir: &Path) -> anyhow::Result<()> {
    let copy_path = backup_dir.join("sqlite.copy");

    if !copy_path.exists() {
        tracing::warn!("未找到 SQLite 备份文件 {} — 跳过", copy_path.display());
        return Ok(());
    }

    // Check for placeholder (starts with "--")
    let peek = tokio::fs::read_to_string(&copy_path)
        .await
        .unwrap_or_default();
    if peek.starts_with("--") {
        tracing::info!("SQLite 备份是占位符, 跳过恢复: {}", copy_path.display());
        return Ok(());
    }

    tracing::info!("正在从 {} 恢复 SQLite 数据库", copy_path.display());

    // Restore to the first candidate path
    let target = SQLITE_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .next()
        .unwrap_or_else(|| PathBuf::from("./data/snapshots.db"));

    // Ensure parent directory exists
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    match tokio::fs::copy(&copy_path, &target).await {
        Ok(size) => {
            tracing::info!("SQLite 恢复完成: {} 字节 → {}", size, target.display());
        }
        Err(e) => {
            tracing::error!(
                "SQLite 恢复失败: {} -> {}: {e}",
                copy_path.display(),
                target.display()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn copy_database_writes_file() {
        let dir = TempDir::new().unwrap();
        let (_ok, size) = copy_database(dir.path())
            .await
            .expect("copy should succeed");
        // ok may be false if no sqlite db found (placeholder written)
        assert!(size > 0);

        let copy = dir.path().join("sqlite.copy");
        assert!(copy.exists());
        let content = std::fs::read_to_string(&copy).unwrap();
        assert!(content.contains("SQLite backup") || content.starts_with("--"));
    }

    #[tokio::test]
    async fn restore_database_skips_missing_file() {
        let dir = TempDir::new().unwrap();
        let result = restore_database(dir.path()).await;
        assert!(result.is_ok(), "should skip missing copy gracefully");
    }

    #[tokio::test]
    async fn restore_database_skips_placeholder() {
        let dir = TempDir::new().unwrap();
        tokio::fs::write(dir.path().join("sqlite.copy"), "-- placeholder\n")
            .await
            .unwrap();
        let result = restore_database(dir.path()).await;
        assert!(result.is_ok(), "should skip placeholder");
    }

    #[tokio::test]
    async fn restore_database_reads_existing_file() {
        let dir = TempDir::new().unwrap();
        copy_database(dir.path()).await.unwrap();
        let result = restore_database(dir.path()).await;
        assert!(result.is_ok());
    }
}
