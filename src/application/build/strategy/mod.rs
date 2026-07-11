//! Build strategies — decide which files to process and how to prepare storage.

pub mod full_rebuild;
pub mod incremental;

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::types::FileSnapshot;
use std::path::Path;

/// Strategy for determining what files to process in a build.
///
/// Implementations:
/// - `IncrementalStrategy`: SHA1 diff against SQLite snapshots.
/// - `FullRebuildStrategy`: wipe all data and rebuild from scratch.
#[async_trait]
pub trait BuildStrategy: Send + Sync {
    /// Return the strategy name (for logging/reporting).
    fn name(&self) -> &'static str;

    /// Select which files need processing.
    ///
    /// Returns `(files_to_process, files_to_delete)`.
    async fn select_files(
        &self,
        root: &Path,
        all_files: &[std::path::PathBuf],
        snapshot_repo: Option<&dyn crate::domain::traits::SnapshotRepository>,
        project: &str,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), DtError>;

    /// Prepare storage before writing (e.g., delete old data).
    async fn prepare(
        &self,
        graph: Option<&dyn crate::domain::traits::GraphRepository>,
        vector: Option<&dyn crate::domain::traits::VectorRepository>,
        project: &str,
    ) -> Result<(), DtError>;

    /// Update snapshots after writing.
    async fn update_snapshots(
        &self,
        snapshot_repo: &dyn crate::domain::traits::SnapshotRepository,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError>;
}
