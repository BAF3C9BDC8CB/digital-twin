//! 增量构建策略 — 仅处理自上次构建以来发生变更的文件，
//! 使用存储在 SQLite 中的 SHA1 哈希进行比较。

use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::FileSnapshot;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;

use super::BuildStrategy;

/// 增量策略：将 SHA1 哈希与已存快照进行比较。
///
/// 仅处理新增或变更的文件；删除已不存在文件的数据。
pub struct IncrementalStrategy;

#[async_trait]
impl BuildStrategy for IncrementalStrategy {
    fn name(&self) -> &'static str {
        "incremental"
    }

    async fn select_files(
        &self,
        root: &Path,
        all_files: &[std::path::PathBuf],
        snapshot_repo: Option<&dyn SnapshotRepository>,
        project: &str,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), DtError> {
        // 先加载已存快照（现在保留 mtime 用于快速路径）。
        let mut stored_map: HashMap<String, (String, f64)> = HashMap::new();
        if let Some(repo) = snapshot_repo {
            if let Ok(snapshots) = repo.list_snapshots(project).await {
                for s in &snapshots {
                    stored_map.insert(s.file_path.clone(), (s.file_sha1.clone(), s.file_mtime));
                }
            }
        }

        // 构建当前哈希映射，使用 mtime 快速路径：若文件的 mtime
        // 与已存快照一致，则复用已存哈希（跳过 SHA-256）。
        let mut current_map: HashMap<String, (String, f64)> = HashMap::new();
        for path in all_files {
            let rel = crate::infrastructure::scanner::rel_path(root, path);
            let current_mtime = path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);

            // mtime 未变 → 复用已存哈希（不读文件、不做 SHA-256）
            if let Some((stored_hash, stored_mtime)) = stored_map.get(&rel) {
                if (current_mtime - stored_mtime).abs() < 1.0 {
                    current_map.insert(rel, (stored_hash.clone(), current_mtime));
                    continue;
                }
            }

            // mtime 已变或新文件 → 计算 SHA-256
            if let Ok((hash, _)) = crate::infrastructure::scanner::compute_file_hash(path) {
                current_map.insert(rel, (hash, current_mtime));
            }
        }

        // 检测变更（相同逻辑，现使用带 mtime 的 current_map）
        let mut stored_hash_only: HashMap<String, String> = HashMap::new();
        for (k, (hash, _)) in &stored_map {
            stored_hash_only.insert(k.clone(), hash.clone());
        }
        let (changed_paths, deleted_paths) =
            crate::infrastructure::scanner::detect_changes(&current_map, &stored_hash_only);

        // 将变更的相对路径映射回绝对路径
        let changed_files: Vec<std::path::PathBuf> = changed_paths
            .iter()
            .map(|p| root.join(p))
            .filter(|p| p.exists())
            .collect();

        Ok((changed_files, deleted_paths))
    }

    async fn prepare(
        &self,
        _graph: Option<&dyn GraphRepository>,
        _vector: Option<&dyn VectorRepository>,
        _project: &str,
    ) -> Result<(), DtError> {
        // 增量构建不清空全部数据。
        // 单个文件的删除在流水线的文件级完成。
        Ok(())
    }

    async fn update_snapshots(
        &self,
        snapshot_repo: &dyn SnapshotRepository,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError> {
        snapshot_repo.save_snapshots(project, snapshots).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_name() {
        let s = IncrementalStrategy;
        assert_eq!(s.name(), "incremental");
    }
}
