//! 构建策略 — 决定处理哪些文件以及如何准备存储。

pub mod full_rebuild;
pub mod incremental;

use crate::domain::error::DtError;
use crate::domain::types::FileSnapshot;
use async_trait::async_trait;
use std::path::Path;

/// 决定构建中处理哪些文件的策略。
///
/// 实现：
/// - `IncrementalStrategy`：对 SQLite 快照做 SHA1 差异比对。
/// - `FullRebuildStrategy`：清空所有数据并从零重建。
#[async_trait]
pub trait BuildStrategy: Send + Sync {
    /// 返回策略名称（用于日志/报告）。
    fn name(&self) -> &'static str;

    /// 选择需要处理的文件（磁盘路径）。
    ///
    /// 返回 `(files_to_process, files_to_delete)`。
    async fn select_files(
        &self,
        root: &Path,
        all_files: &[std::path::PathBuf],
        snapshot_repo: Option<&dyn crate::domain::traits::SnapshotRepository>,
        project: &str,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), DtError>;

    /// 从 VirtualFile 列表中选取需要处理的文件。
    ///
    /// F2 要求：mtime 快速路径仅对 `source == Fs` 生效；
    /// `source != Fs` 直接对比 `content_hash`（SHA256），不设 mtime 捷径。
    ///
    /// 返回 `(virtual_files_to_process, virtual_paths_to_delete)`。
    async fn select_virtual_files(
        &self,
        virtual_files: &[crate::application::pipeline::virtual_file::VirtualFile],
        snapshot_repo: Option<&dyn crate::domain::traits::SnapshotRepository>,
        project: &str,
    ) -> Result<
        (
            Vec<crate::application::pipeline::virtual_file::VirtualFile>,
            Vec<String>,
        ),
        DtError,
    > {
        let _ = (virtual_files, snapshot_repo, project);
        // 默认实现：全部处理，无删除。
        Ok((virtual_files.to_vec(), Vec::new()))
    }

    /// 写入前准备存储（如删除旧数据）。
    async fn prepare(
        &self,
        graph: Option<&dyn crate::domain::traits::GraphRepository>,
        vector: Option<&dyn crate::domain::traits::VectorRepository>,
        project: &str,
    ) -> Result<(), DtError>;

    /// 写入后更新快照。
    async fn update_snapshots(
        &self,
        snapshot_repo: &dyn crate::domain::traits::SnapshotRepository,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError>;

    /// 该策略是否强制全量重建。
    /// 为 true 时，文档处理跳过 mtime 检查并重新处理所有文件。
    fn force_rebuild(&self) -> bool {
        false
    }
}
