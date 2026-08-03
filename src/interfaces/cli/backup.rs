//! 备份模块——从 dt-backup 组件重新导出，并编排
//! backup / restore / list / verify 操作。
//!
//! 该文件将原 dt-backup crate 集成到单 crate 架构中。
//! 各备份目标已拆分为独立文件（backup_memgraph、backup_qdrant、backup_sqlite、backup_verify）。

pub mod memgraph {
    //! Memgraph 备份辅助函数。
    pub use crate::interfaces::cli::backup_memgraph::*;
}
pub mod qdrant {
    //! Qdrant 备份辅助函数。
    pub use crate::interfaces::cli::backup_qdrant::*;
}
pub mod sqlite {
    //! SQLite 备份辅助函数。
    pub use crate::interfaces::cli::backup_sqlite::*;
}
pub mod verify {
    //! 备份校验辅助函数。
    pub use crate::interfaces::cli::backup_verify::*;
}

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

/// 备份成功运行后返回的备份报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupReport {
    pub location: PathBuf,
    pub date: String,
    pub targets: BackupTargets,
    pub duration_seconds: f64,
}

/// 各备份目标的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupTargets {
    pub memgraph: bool,
    pub memgraph_size_bytes: u64,
    pub qdrant: bool,
    pub qdrant_size_bytes: u64,
    pub sqlite: bool,
    pub sqlite_size_bytes: u64,
}

/// 列表输出中展示的备份条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub date: String,
    pub total_size_bytes: u64,
    pub file_count: usize,
}

/// 校验和检查后的校验报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub backup_dir: PathBuf,
    pub date: String,
    pub all_valid: bool,
    pub files: Vec<VerifyFileResult>,
    pub duration_seconds: f64,
}

/// 单文件的校验结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFileResult {
    pub file_name: String,
    pub valid: bool,
    pub expected: String,
    pub actual: String,
}

/// 宿主机上的默认备份根目录。
const BACKUP_ROOT: &str = "/var/backups/digital-twin";

/// 创建新备份。
///
/// 1. 在 `BACKUP_ROOT` 下创建带日期的目录。
/// 2. 导出 Memgraph、快照 Qdrant 集合、复制 SQLite。
/// 3. 生成校验和。
/// 4. 返回 `BackupReport`。
pub async fn create_backup() -> Result<BackupReport> {
    let start = Instant::now();
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let backup_dir = PathBuf::from(BACKUP_ROOT).join(&date);

    tracing::info!("正在为 {date} 创建备份, 保存至 {}", backup_dir.display());

    // 确保备份目录存在
    tokio::fs::create_dir_all(&backup_dir).await?;

    // ---- 备份每个组件 ----
    let (memgraph_ok, memgraph_size) =
        crate::interfaces::cli::backup_memgraph::dump_graph(&backup_dir).await?;
    let (qdrant_ok, qdrant_size) =
        crate::interfaces::cli::backup_qdrant::snapshot_collections(&backup_dir).await?;
    let (sqlite_ok, sqlite_size) =
        crate::interfaces::cli::backup_sqlite::copy_database(&backup_dir).await?;

    // ---- 生成校验和 ----
    if let Err(e) = crate::interfaces::cli::backup_verify::generate_checksums(&backup_dir).await {
        tracing::warn!("校验和生成失败: {e}");
    }

    let duration = start.elapsed().as_secs_f64();

    let report = BackupReport {
        location: backup_dir,
        date,
        targets: BackupTargets {
            memgraph: memgraph_ok,
            memgraph_size_bytes: memgraph_size,
            qdrant: qdrant_ok,
            qdrant_size_bytes: qdrant_size,
            sqlite: sqlite_ok,
            sqlite_size_bytes: sqlite_size,
        },
        duration_seconds: duration,
    };

    Ok(report)
}

/// 按日期恢复备份。
///
/// 查找 `{BACKUP_ROOT}/{date}/` 并恢复每个组件。
pub async fn restore_backup(date: &str) -> Result<()> {
    let backup_dir = PathBuf::from(BACKUP_ROOT).join(date);

    if !backup_dir.exists() {
        eprintln!("备份目录未找到: {}", backup_dir.display());
        return Ok(());
    }

    tracing::info!("正在从 {date} 恢复备份");

    // ---- 恢复每个组件 ----
    crate::interfaces::cli::backup_memgraph::restore_graph(&backup_dir).await?;
    crate::interfaces::cli::backup_qdrant::restore_collections(&backup_dir).await?;
    crate::interfaces::cli::backup_sqlite::restore_database(&backup_dir).await?;

    tracing::info!("{date} 备份恢复完成");

    Ok(())
}

/// 列出可用的备份。
///
/// 扫描 `BACKUP_ROOT` 下的带日期目录，并返回
/// 每个目录的摘要信息。
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
        // 仅包含日期格式的目录（YYYY-MM-DD）
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

    // 按日期降序排序
    entries.sort_by(|a, b| b.date.cmp(&a.date));

    Ok(entries)
}

/// 按日期校验备份文件的校验和。
pub async fn verify_backup_files(date: &str) -> Result<VerifyReport> {
    let backup_dir = PathBuf::from(BACKUP_ROOT).join(date);

    if !backup_dir.exists() {
        eprintln!("备份目录未找到: {}", backup_dir.display());
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
