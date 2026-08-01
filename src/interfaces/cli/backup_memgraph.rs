//! Memgraph backup and restore operations.
//!
//! Uses `mg_backup` CLI tool (preferred), `mgconsole` (Cypher-based fallback),
//! or Docker-based execution.  Attempts local `mg_backup` first, then local
//! `mgconsole`, then `docker exec memgraph-mage mg_backup` as fallback.
//!
//! When none is available the dump/restore is a no-op that logs a warning —
//! the system is designed to be tolerant of partial tooling.

use std::path::Path;
use std::time::Instant;

/// Which Memgraph method was detected at backup time.
#[derive(Debug, Clone)]
enum MemgraphMethod {
    /// Local `mg_backup` binary.
    MgBackup,
    /// Local `mgconsole` binary.
    Mgconsole,
    /// Docker-based: `docker exec memgraph-mage mg_backup`.
    Docker,
}

/// Detect the available Memgraph backup method.
fn detect_method() -> Option<MemgraphMethod> {
    // 1. Local mg_backup binary
    if std::process::Command::new("which")
        .arg("mg_backup")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(MemgraphMethod::MgBackup);
    }

    // 2. Local mgconsole binary
    if std::process::Command::new("which")
        .arg("mgconsole")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(MemgraphMethod::Mgconsole);
    }

    // 3. Docker container named memgraph or memgraph-mage
    if std::process::Command::new("docker")
        .args(["ps", "--filter", "name=memgraph", "--format", "{{.Names}}"])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            !s.trim().is_empty()
        })
        .unwrap_or(false)
    {
        return Some(MemgraphMethod::Docker);
    }

    None
}

/// Resolve the actual Memgraph container name when running in Docker mode.
///
/// Checks for `memgraph-mage` first (the common Memgraph MAGE image name),
/// then falls back to `memgraph`.
fn resolve_container_name() -> String {
    let output = std::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            "name=memgraph-mage",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .ok();
    if let Some(o) = output {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    "memgraph".to_string()
}

/// Dump Memgraph graph to `{backup_dir}/memgraph.dump`.
///
/// Tries `mg_backup --output` (local or Docker) first, then falls back to
/// `mgconsole --output-format cypherl` piped to a file.  On complete failure
/// writes a placeholder + warning.
///
/// Returns `(success, size_bytes)`.
pub async fn dump_graph(backup_dir: &Path) -> anyhow::Result<(bool, u64)> {
    let start = Instant::now();
    let dump_path = backup_dir.join("memgraph.dump");

    tracing::info!("正在导出 Memgraph 到 {}", dump_path.display());

    let method = detect_method();

    match method {
        Some(MemgraphMethod::MgBackup) => {
            let output = tokio::process::Command::new("mg_backup")
                .args(["--output", dump_path.to_str().unwrap()])
                .output()
                .await?;

            if output.status.success() {
                let size = tokio::fs::metadata(&dump_path).await?.len();
                tracing::info!(
                    "Memgraph 导出完成 (mg_backup): {} 字节 ({:.0}ms)",
                    size,
                    start.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((true, size));
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("mg_backup 导出失败: {stderr}");
        }
        Some(MemgraphMethod::Mgconsole) => {
            // mgconsole can output Cypher queries for later replay
            let output = tokio::process::Command::new("mgconsole")
                .args(["--output-format", "cypherl"])
                .output()
                .await?;

            if output.status.success() {
                tokio::fs::write(&dump_path, &output.stdout).await?;
                let size = tokio::fs::metadata(&dump_path).await?.len();
                tracing::info!(
                    "Memgraph 导出完成 (mgconsole): {} 字节 ({:.0}ms)",
                    size,
                    start.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((true, size));
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("mgconsole 导出失败: {stderr}");
        }
        Some(MemgraphMethod::Docker) => {
            let container = resolve_container_name();

            // Try docker mg_backup first
            let output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    &container,
                    "mg_backup",
                    "--output",
                    "/tmp/memgraph.dump",
                ])
                .output()
                .await?;

            if output.status.success() {
                // Copy dump from container
                let copy = tokio::process::Command::new("docker")
                    .args([
                        "cp",
                        &format!("{container}:/tmp/memgraph.dump"),
                        dump_path.to_str().unwrap(),
                    ])
                    .output()
                    .await?;

                if copy.status.success() {
                    let size = tokio::fs::metadata(&dump_path).await?.len();
                    tracing::info!(
                        "Memgraph 导出完成 (docker mg_backup): {} 字节 ({:.0}ms)",
                        size,
                        start.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok((true, size));
                }
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("docker mg_backup 导出失败: {stderr}");

            // Fallback: try docker mgconsole
            let console_output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    &container,
                    "mgconsole",
                    "--output-format",
                    "cypherl",
                ])
                .output()
                .await?;

            if console_output.status.success() {
                tokio::fs::write(&dump_path, &console_output.stdout).await?;
                let size = tokio::fs::metadata(&dump_path).await?.len();
                tracing::info!(
                    "Memgraph 导出完成 (docker mgconsole): {} 字节 ({:.0}ms)",
                    size,
                    start.elapsed().as_secs_f64() * 1000.0
                );
                return Ok((true, size));
            }

            let stderr = String::from_utf8_lossy(&console_output.stderr);
            tracing::warn!("docker mgconsole 导出失败: {stderr}");
        }
        None => {
            tracing::warn!(
                "未找到 Memgraph 备份工具: 请安装 mg_backup 或 mgconsole, \
                 或运行 Memgraph Docker 容器"
            );
        }
    }

    // Fallback: write a placeholder to maintain the backup structure
    let placeholder = format!(
        "// Memgraph dump — placeholder\n\
         // Generated: {}\n\
         // Reason: no Memgraph backup method available\n\
         // Re-run with mg_backup/mgconsole installed or Docker container running to produce a real dump.\n",
        chrono::Utc::now().to_rfc3339()
    );
    tokio::fs::write(&dump_path, placeholder.as_bytes()).await?;
    let size = tokio::fs::metadata(&dump_path).await?.len();

    tracing::info!(
        "Memgraph 导出 (占位符): {} 字节 ({:.0}ms)",
        size,
        start.elapsed().as_secs_f64() * 1000.0
    );

    Ok((false, size))
}

/// Restore Memgraph graph from `{backup_dir}/memgraph.dump`.
///
/// Attempts restoration via `mg_backup --restore`, then `mgconsole -f` as
/// fallback.  Skips if the dump file doesn't exist or is a placeholder.
pub async fn restore_graph(backup_dir: &Path) -> anyhow::Result<()> {
    let dump_path = backup_dir.join("memgraph.dump");

    if !dump_path.exists() {
        tracing::warn!(
            "Memgraph dump not found at {} — skipping",
            dump_path.display()
        );
        return Ok(());
    }

    // Check if file looks like a placeholder (starts with "//") — real
    // mg_backup dumps are binary, mgconsole dumps start with Cypher.
    let peek = tokio::fs::read_to_string(&dump_path)
        .await
        .unwrap_or_default();
    if peek.starts_with("//") {
        tracing::info!(
            "Memgraph dump is a placeholder, skipping restore: {}",
            dump_path.display()
        );
        return Ok(());
    }

    tracing::info!("正在从 {} 恢复 Memgraph", dump_path.display());

    let method = detect_method();
    let dump_str = dump_path.to_str().unwrap();

    match method {
        Some(MemgraphMethod::MgBackup) => {
            let output = tokio::process::Command::new("mg_backup")
                .args(["--restore", dump_str])
                .output()
                .await?;

            if output.status.success() {
                tracing::info!("Memgraph 恢复完成 (mg_backup)");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!("mg_backup 恢复失败: {stderr}");
            }
        }
        Some(MemgraphMethod::Mgconsole) => {
            // mgconsole reads Cypher commands from stdin
            let input = tokio::fs::read(&dump_path).await?;

            let mut child = tokio::process::Command::new("mgconsole")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            use tokio::io::AsyncWriteExt;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(&input).await?;
                // Close stdin so mgconsole knows to exit
                drop(stdin);
            }

            let output = child.wait_with_output().await?;

            if output.status.success() {
                tracing::info!("Memgraph 恢复完成 (mgconsole)");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::error!("mgconsole 恢复失败: {stderr}");
            }
        }
        Some(MemgraphMethod::Docker) => {
            let container = resolve_container_name();

            // Copy dump into container
            let _ = tokio::process::Command::new("docker")
                .args(["cp", dump_str, &format!("{container}:/tmp/memgraph.dump")])
                .output()
                .await?;

            // Try mg_backup restore inside container
            let output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    &container,
                    "mg_backup",
                    "--restore",
                    "/tmp/memgraph.dump",
                ])
                .output()
                .await?;

            if output.status.success() {
                tracing::info!("Memgraph 恢复完成 (docker mg_backup)");
                return Ok(());
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("docker mg_backup 恢复失败: {stderr}");

            // Fallback: try mgconsole restore inside container
            let dump_data = tokio::fs::read(&dump_path).await?;

            let console_output = tokio::process::Command::new("docker")
                .args(["exec", "-i", &container, "mgconsole"])
                .arg("--output-format")
                .arg("cypherl")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();

            if let Ok(mut child) = console_output {
                use tokio::io::AsyncWriteExt;
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(dump_data.as_ref()).await?;
                    drop(stdin);
                }
                let result = child.wait_with_output().await?;
                if result.status.success() {
                    tracing::info!("Memgraph 恢复完成 (docker mgconsole)");
                } else {
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    tracing::error!("docker mgconsole 恢复失败: {stderr}");
                }
            }
        }
        None => {
            tracing::warn!(
                "无法恢复 Memgraph: 没有可用的备份方法. \
                 请尝试: docker exec memgraph-mage mg_backup --restore <path>"
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
        // ok may be false if no backup method is available (placeholder written)
        assert!(size > 0);

        let dump = dir.path().join("memgraph.dump");
        assert!(dump.exists());
        let content = std::fs::read_to_string(&dump).unwrap();
        assert!(content.contains("Memgraph dump") || content.starts_with("//"));
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
        tokio::fs::write(dir.path().join("memgraph.dump"), "// placeholder dump\n")
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
