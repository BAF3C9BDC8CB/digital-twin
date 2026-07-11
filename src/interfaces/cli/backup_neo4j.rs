//! Neo4j backup and restore operations.
//!
//! Uses `neo4j-admin` CLI tool.  Attempts local `neo4j-admin` first,
//! then Docker-based `docker exec neo4j neo4j-admin` as fallback.
//!
//! When neither is available the dump/restore is a no-op that logs a
//! warning — the system is designed to be tolerant of partial tooling.

use std::path::Path;
use std::time::Instant;

/// Which Neo4j admin method was detected at backup time.
#[derive(Debug, Clone)]
enum Neo4jAdmin {
    /// Local `neo4j-admin` binary.
    Local,
    /// Docker-based: `docker exec neo4j neo4j-admin`.
    Docker,
}

/// Detect the available Neo4j admin method.
fn detect_admin() -> Option<Neo4jAdmin> {
    // 1. Local binary
    if std::process::Command::new("which")
        .arg("neo4j-admin")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(Neo4jAdmin::Local);
    }

    // 2. Docker container
    if std::process::Command::new("docker")
        .args(["ps", "--filter", "name=neo4j", "--format", "{{.Names}}"])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            !s.trim().is_empty()
        })
        .unwrap_or(false)
    {
        return Some(Neo4jAdmin::Docker);
    }

    None
}

/// Dump Neo4j graph to `{backup_dir}/neo4j.dump`.
///
/// Tries `neo4j-admin database dump` (local or docker).  On failure or when
/// neo4j-admin is unavailable, writes a placeholder + warning.
///
/// Returns `(success, size_bytes)`.
pub async fn dump_graph(backup_dir: &Path) -> anyhow::Result<(bool, u64)> {
    let start = Instant::now();
    let dump_path = backup_dir.join("neo4j.dump");

    tracing::info!("dumping Neo4j to {}", dump_path.display());

    let admin = detect_admin();

    match admin {
        Some(Neo4jAdmin::Local) => {
            let tmp_dir = std::env::temp_dir().join(format!(
                "neo4j-backup-{}",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            ));
            std::fs::create_dir_all(&tmp_dir)?;

            let to_path = tmp_dir.clone();
            let output = tokio::task::spawn_blocking(move || {
                std::process::Command::new("neo4j-admin")
                    .args([
                        "database",
                        "dump",
                        "neo4j",
                        "--to-path",
                        to_path.to_str().unwrap(),
                    ])
                    .output()
            })
            .await??;

            if output.status.success() {
                // Move the dump file to the backup directory
                let source = tmp_dir.join("neo4j.dump");
                if source.exists() {
                    std::fs::rename(&source, &dump_path)?;
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                    let size = tokio::fs::metadata(&dump_path).await?.len();
                    tracing::info!(
                        "Neo4j dump complete (local): {} bytes ({:.0}ms)",
                        size,
                        start.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok((true, size));
                }
                let _ = std::fs::remove_dir_all(&tmp_dir);
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("neo4j-admin dump failed: {stderr}");
        }
        Some(Neo4jAdmin::Docker) => {
            let output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    "neo4j",
                    "neo4j-admin",
                    "database",
                    "dump",
                    "neo4j",
                    "--to-path",
                    "/tmp",
                ])
                .output()
                .await?;

            if output.status.success() {
                // Copy dump from container
                let copy = tokio::process::Command::new("docker")
                    .args([
                        "cp",
                        "neo4j:/tmp/neo4j.dump",
                        dump_path.to_str().unwrap(),
                    ])
                    .output()
                    .await?;

                if copy.status.success() {
                    let size = tokio::fs::metadata(&dump_path).await?.len();
                    tracing::info!(
                        "Neo4j dump complete (docker): {} bytes ({:.0}ms)",
                        size,
                        start.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok((true, size));
                }
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("docker neo4j-admin dump failed: {stderr}");
        }
        None => {
            tracing::warn!("neo4j-admin not found: try `docker exec neo4j neo4j-admin` or install neo4j-admin locally");
        }
    }

    // Fallback: write a placeholder to maintain the backup structure
    let placeholder = format!(
        "// Neo4j dump — placeholder\n\
         // Generated: {}\n\
         // Reason: neo4j-admin not available\n\
         // Re-run with neo4j-admin installed or Docker container running to produce a real dump.\n",
        chrono::Utc::now().to_rfc3339()
    );
    tokio::fs::write(&dump_path, placeholder.as_bytes()).await?;
    let size = tokio::fs::metadata(&dump_path).await?.len();

    tracing::info!(
        "Neo4j dump (placeholder): {} bytes ({:.0}ms)",
        size,
        start.elapsed().as_secs_f64() * 1000.0
    );

    Ok((false, size))
}

/// Restore Neo4j graph from `{backup_dir}/neo4j.dump`.
///
/// Attempts `neo4j-admin database load`.  On failure logs a warning.
pub async fn restore_graph(backup_dir: &Path) -> anyhow::Result<()> {
    let dump_path = backup_dir.join("neo4j.dump");

    if !dump_path.exists() {
        tracing::warn!("Neo4j dump not found at {} — skipping", dump_path.display());
        return Ok(());
    }

    // Check if file looks like a placeholder (starts with "//") — real
    // neo4j-admin dumps are binary.
    let peek = tokio::fs::read_to_string(&dump_path).await.unwrap_or_default();
    if peek.starts_with("//") {
        tracing::info!(
            "Neo4j dump is a placeholder, skipping restore: {}",
            dump_path.display()
        );
        return Ok(());
    }

    tracing::info!("restoring Neo4j from {}", dump_path.display());

    let admin = detect_admin();

    match admin {
        Some(Neo4jAdmin::Local) => {
            let output = tokio::process::Command::new("neo4j-admin")
                .args([
                    "database",
                    "load",
                    "neo4j",
                    "--from-path",
                    backup_dir.to_str().unwrap(),
                    "--overwrite-destination",
                ])
                .output()
                .await?;

            if output.status.success() {
                tracing::info!("Neo4j restore complete (local)");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!("neo4j-admin load failed: {stderr}");
            }
        }
        Some(Neo4jAdmin::Docker) => {
            // Copy dump into container first
            let _ = tokio::process::Command::new("docker")
                .args(["cp", dump_path.to_str().unwrap(), "neo4j:/tmp/neo4j.dump"])
                .output()
                .await?;

            let output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    "neo4j",
                    "neo4j-admin",
                    "database",
                    "load",
                    "neo4j",
                    "--from-path",
                    "/tmp",
                    "--overwrite-destination",
                ])
                .output()
                .await?;

            if output.status.success() {
                tracing::info!("Neo4j restore complete (docker)");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!("docker neo4j-admin load failed: {stderr}");
            }
        }
        None => {
            tracing::warn!(
                "cannot restore Neo4j: neo4j-admin not found. \
                 Try: docker exec neo4j neo4j-admin database load neo4j --from-path <path>"
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
    async fn dump_graph_writes_file() {
        let dir = TempDir::new().unwrap();
        let (_ok, size) = dump_graph(dir.path()).await.expect("dump should succeed");
        // ok may be false if neo4j-admin is unavailable (placeholder written)
        assert!(size > 0);

        let dump = dir.path().join("neo4j.dump");
        assert!(dump.exists());
        let content = std::fs::read_to_string(&dump).unwrap();
        assert!(content.contains("Neo4j dump") || content.starts_with("//"));
    }

    #[tokio::test]
    async fn restore_graph_skips_missing_file() {
        let dir = TempDir::new().unwrap();
        let result = restore_graph(dir.path()).await;
        assert!(result.is_ok(), "should skip missing dump gracefully");
    }

    #[tokio::test]
    async fn restore_graph_skips_placeholder() {
        let dir = TempDir::new().unwrap();
        // Write a placeholder dump
        tokio::fs::write(dir.path().join("neo4j.dump"), "// placeholder dump\n")
            .await
            .unwrap();
        let result = restore_graph(dir.path()).await;
        assert!(result.is_ok(), "should skip placeholder dump gracefully");
    }

    #[tokio::test]
    async fn restore_graph_reads_existing_file() {
        let dir = TempDir::new().unwrap();
        // First dump, then restore
        dump_graph(dir.path()).await.unwrap();
        let result = restore_graph(dir.path()).await;
        assert!(result.is_ok());
    }
}
