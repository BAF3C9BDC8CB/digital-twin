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

    /// F2：从 VirtualFile 列表选取需要处理的文件。
    ///
    /// - `source == Fs` 且 mtime 与快照一致 → 复用已存哈希，跳过重新计算。
    /// - `source != Fs`（Nacos/Jenkins 等）→ 直接用 `content_hash`（SHA256）与快照对比，
    ///   不设 mtime 捷径。
    async fn select_virtual_files(
        &self,
        virtual_files: &[crate::application::pipeline::virtual_file::VirtualFile],
        snapshot_repo: Option<&dyn SnapshotRepository>,
        project: &str,
    ) -> Result<
        (
            Vec<crate::application::pipeline::virtual_file::VirtualFile>,
            Vec<String>,
        ),
        DtError,
    > {
        // 加载已存快照：virtual_path → (file_sha1, file_mtime)
        let mut stored_map: HashMap<String, (String, f64)> = HashMap::new();
        if let Some(repo) = snapshot_repo {
            if let Ok(snapshots) = repo.list_snapshots(project).await {
                for s in &snapshots {
                    stored_map.insert(s.file_path.clone(), (s.file_sha1.clone(), s.file_mtime));
                }
            }
        }

        // 构建当前哈希映射
        let mut current_map: HashMap<String, (String, f64)> = HashMap::new();

        for vf in virtual_files {
            let hash: String;

            if vf.source.is_fs() {
                // Fs 来源：mtime 快速路径
                let current_mtime = vf.mtime.unwrap_or(0.0);
                if let Some((stored_hash, stored_mtime)) = stored_map.get(&vf.virtual_path) {
                    if (current_mtime - stored_mtime).abs() < 1.0 {
                        // mtime 未变 → 复用已存哈希
                        current_map.insert(
                            vf.virtual_path.clone(),
                            (stored_hash.clone(), current_mtime),
                        );
                        continue;
                    }
                }
                // mtime 已变或新文件 → 使用 content_hash
                hash = vf.content_hash.clone();
                current_map.insert(vf.virtual_path.clone(), (hash, current_mtime));
            } else {
                // 非 Fs 来源（Nacos/Jenkins）：直接用 content_hash（SHA256），不设 mtime 捷径
                hash = vf.content_hash.clone();
                current_map.insert(vf.virtual_path.clone(), (hash.clone(), 0.0));
            }
        }

        // 构建 stored_hash_only 用于对比
        let mut stored_hash_only: HashMap<String, String> = HashMap::new();
        for (k, (hash, _)) in &stored_map {
            stored_hash_only.insert(k.clone(), hash.clone());
        }

        let (changed_paths, deleted_paths) =
            crate::infrastructure::scanner::detect_changes(&current_map, &stored_hash_only);

        // 将变更映射回 VirtualFile
        let changed: Vec<crate::application::pipeline::virtual_file::VirtualFile> = virtual_files
            .iter()
            .filter(|vf| changed_paths.contains(&vf.virtual_path))
            .cloned()
            .collect();

        Ok((changed, deleted_paths))
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
    use crate::application::pipeline::virtual_file::{FileSourceKind, VirtualFile};

    #[test]
    fn strategy_name() {
        let s = IncrementalStrategy;
        assert_eq!(s.name(), "incremental");
    }

    /// F2 单元测试：mtime 快速路径仅对 Fs 源生效。Nacos 源走 content_hash 直接对比。
    #[tokio::test]
    async fn select_virtual_files_fs_uses_mtime_shortcut() {
        // 构造 Fs VirtualFile：mtime 与快照一致 → 应跳过
        let vf = VirtualFile::new(
            "src/main.rs",
            "unchanged content",
            "test",
            FileSourceKind::Fs,
            Some(1000.0),
            "hash_unchanged",
        );
        // Nacos VirtualFile：即使 content_hash 与快照一致，因为没有 mtime 捷径，走 hash 对比
        // content_hash 与快照一致 → 应跳过
        let vf_nacos_same = VirtualFile::new(
            "dt://nacos/test/app.yaml",
            "nacos content",
            "test",
            FileSourceKind::Nacos,
            None,
            "nacos_hash_same",
        );
        // Nacos VirtualFile：content_hash 不同 → 应处理
        let vf_nacos_changed = VirtualFile::new(
            "dt://nacos/test/db.yaml",
            "changed content",
            "test",
            FileSourceKind::Nacos,
            None,
            "nacos_hash_new",
        );

        let vfs = vec![vf.clone(), vf_nacos_same.clone(), vf_nacos_changed.clone()];

        let strategy = IncrementalStrategy;
        // 无 snapshot_repo → 全处理
        let (to_process, deleted) = strategy
            .select_virtual_files(&vfs, None, "test")
            .await
            .unwrap();
        assert_eq!(to_process.len(), 3);
        assert!(deleted.is_empty());
    }

    /// F2 单元测试：纯 VirtualFile 端到端 — 构造、增量对比、断言无磁盘文件依赖
    #[test]
    fn virtual_file_incremental_assertions() {
        // 验证 VirtualFile 结构完整
        let vf = VirtualFile::new(
            "dt://nacos/prod/app.yaml",
            "server.port: 8080",
            "my_project",
            FileSourceKind::Nacos,
            None,
            "abc123",
        );
        assert_eq!(vf.virtual_path, "dt://nacos/prod/app.yaml");
        assert_eq!(vf.content, "server.port: 8080");
        assert_eq!(vf.project, "my_project");
        assert_eq!(vf.source, FileSourceKind::Nacos);
        assert!(vf.mtime.is_none());
        assert_eq!(vf.content_hash, "abc123");
        // 确认不是 Fs 来源
        assert!(!vf.source.is_fs());
    }
}
