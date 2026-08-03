//! Memgraph 备份与恢复操作。
//!
//! 使用 `mg_backup` CLI 工具（首选）、`mgconsole`（基于 Cypher 的兜底），
//! 或基于 Docker 的执行方式。先尝试本机 `mg_backup`，然后本机
//! `mgconsole`，最后回退到 `docker exec memgraph-mage mg_backup`。
//!
//! 当所有工具都不可用时，dump/restore 为 no-op 并记录警告——
//! 系统设计为可容忍部分工具缺失。

use std::path::Path;
use std::time::Instant;

/// 备份时检测到的 Memgraph 方式。
#[derive(Debug, Clone)]
enum MemgraphMethod {
    /// 本机 `mg_backup` 二进制。
    MgBackup,
    /// 本机 `mgconsole` 二进制。
    Mgconsole,
    /// 基于 Docker：`docker exec memgraph-mage mg_backup`。
    Docker,
}

/// 检测可用的 Memgraph 备份方式。
fn detect_method() -> Option<MemgraphMethod> {
    // 1. 本机 mg_backup 二进制
    if std::process::Command::new("which")
        .arg("mg_backup")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(MemgraphMethod::MgBackup);
    }

    // 2. 本机 mgconsole 二进制
    if std::process::Command::new("which")
        .arg("mgconsole")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(MemgraphMethod::Mgconsole);
    }

    // 3. 名为 memgraph 或 memgraph-mage 的 Docker 容器
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

/// 在 Docker 模式下解析实际的 Memgraph 容器名。
///
/// 先检查 `memgraph-mage`（常见的 Memgraph MAGE 镜像名），
/// 然后回退到 `memgraph`。
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

/// 将 Memgraph 图谱导出到 `{backup_dir}/memgraph.dump`。
///
/// 先尝试 `mg_backup --output`（本机或 Docker），然后回退到
/// 将 `mgconsole --output-format cypherl` 输出写入文件。完全失败时
/// 写入占位符并发出警告。
///
/// 返回 `(success, size_bytes)`。
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
            // mgconsole 可输出 Cypher 查询供日后重放
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

            // 先尝试 docker mg_backup
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
                // 从容器复制 dump
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

            // 兜底：尝试 docker mgconsole
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

    // 兜底：写入占位符以维持备份结构
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

/// 从 `{backup_dir}/memgraph.dump` 恢复 Memgraph 图谱。
///
/// 先尝试通过 `mg_backup --restore` 恢复，然后以 `mgconsole -f` 作为
/// 兜底。若 dump 文件不存在或是占位符则跳过。
pub async fn restore_graph(backup_dir: &Path) -> anyhow::Result<()> {
    let dump_path = backup_dir.join("memgraph.dump");

    if !dump_path.exists() {
        tracing::warn!(
            "在 {} 未找到 Memgraph dump — 已跳过",
            dump_path.display()
        );
        return Ok(());
    }

    // 检查文件是否像占位符（以 "//" 开头）——真实的
    // mg_backup dump 是二进制，mgconsole dump 以 Cypher 开头。
    let peek = tokio::fs::read_to_string(&dump_path)
        .await
        .unwrap_or_default();
    if peek.starts_with("//") {
        tracing::info!(
            "Memgraph dump 是占位符，跳过恢复: {}",
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
            // mgconsole 从 stdin 读取 Cypher 命令
            let input = tokio::fs::read(&dump_path).await?;

            let mut child = tokio::process::Command::new("mgconsole")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            use tokio::io::AsyncWriteExt;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(&input).await?;
                // 关闭 stdin，让 mgconsole 知道可以退出
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

            // 将 dump 复制进容器
            let _ = tokio::process::Command::new("docker")
                .args(["cp", dump_str, &format!("{container}:/tmp/memgraph.dump")])
                .output()
                .await?;

            // 尝试在容器内执行 mg_backup 恢复
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

            // 兜底：尝试在容器内执行 mgconsole 恢复
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
        let (_ok, size) = dump_graph(dir.path()).await.expect("dump 应成功");
        // 若无可用备份方式，ok 可能为 false（已写入占位符）
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
        assert!(result.is_ok(), "缺少 dump 文件时应优雅跳过");
    }

    #[tokio::test]
    async fn restore_graph_skips_placeholder() {
        let dir = TempDir::new().unwrap();
        // 写入占位符 dump
        tokio::fs::write(dir.path().join("memgraph.dump"), "// placeholder dump\n")
            .await
            .unwrap();
        let result = restore_graph(dir.path()).await;
        assert!(result.is_ok(), "应优雅跳过占位符 dump");
    }

    #[tokio::test]
    async fn restore_graph_reads_existing_file() {
        let dir = TempDir::new().unwrap();
        // 先导出，再恢复
        dump_graph(dir.path()).await.unwrap();
        let result = restore_graph(dir.path()).await;
        assert!(result.is_ok());
    }
}
