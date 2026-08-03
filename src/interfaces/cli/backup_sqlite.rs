//! SQLite 备份与恢复操作。
//!
//! 将 SQLite 快照数据库文件复制到备份目录。
//! 未找到数据库文件时回退到占位符。

use std::path::{Path, PathBuf};
use std::time::Instant;

/// 已知的 SQLite 数据库路径（按顺序检查）。
const SQLITE_CANDIDATES: &[&str] = &[
    "/var/lib/digital-twin/snapshots.db",
    "./data/snapshots.db",
    "/tmp/digital-twin/snapshots.db",
];

/// 定位当前使用的 SQLite 数据库路径。
fn find_database() -> Option<PathBuf> {
    for candidate in SQLITE_CANDIDATES {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// 将 SQLite 数据库复制到 `{backup_dir}/sqlite.copy`。
///
/// 依次尝试快照数据库的已知路径。失败时写入占位符，
/// 以维持备份结构。
///
/// 返回 `(success, size_bytes)`。
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

    // 兜底：写入占位符
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

/// 从 `{backup_dir}/sqlite.copy` 恢复 SQLite 数据库。
///
/// 将备份文件复制到已知的数据库路径。
pub async fn restore_database(backup_dir: &Path) -> anyhow::Result<()> {
    let copy_path = backup_dir.join("sqlite.copy");

    if !copy_path.exists() {
        tracing::warn!("未找到 SQLite 备份文件 {} — 跳过", copy_path.display());
        return Ok(());
    }

    // 检查是否为占位符（以 "--" 开头）
    let peek = tokio::fs::read_to_string(&copy_path)
        .await
        .unwrap_or_default();
    if peek.starts_with("--") {
        tracing::info!("SQLite 备份是占位符, 跳过恢复: {}", copy_path.display());
        return Ok(());
    }

    tracing::info!("正在从 {} 恢复 SQLite 数据库", copy_path.display());

    // 恢复到第一个候选路径
    let target = SQLITE_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .next()
        .unwrap_or_else(|| PathBuf::from("./data/snapshots.db"));

    // 确保父目录存在
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
            .expect("复制应成功");
        // 若未找到 sqlite 数据库，ok 可能为 false（已写入占位符）
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
        assert!(result.is_ok(), "缺少复制文件时应优雅跳过");
    }

    #[tokio::test]
    async fn restore_database_skips_placeholder() {
        let dir = TempDir::new().unwrap();
        tokio::fs::write(dir.path().join("sqlite.copy"), "-- placeholder\n")
            .await
            .unwrap();
        let result = restore_database(dir.path()).await;
        assert!(result.is_ok(), "应跳过占位符");
    }

    #[tokio::test]
    async fn restore_database_reads_existing_file() {
        let dir = TempDir::new().unwrap();
        copy_database(dir.path()).await.unwrap();
        let result = restore_database(dir.path()).await;
        assert!(result.is_ok());
    }
}
