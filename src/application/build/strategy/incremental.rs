//! Incremental build strategy — only processes files that have changed since
//! the last build, using SHA1 hashes stored in SQLite.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::FileSnapshot;
use std::collections::HashMap;
use std::path::Path;

use super::BuildStrategy;

/// Incremental strategy: compare SHA1 hashes against stored snapshots.
///
/// Only processes files that are new or have changed. Deletes data for
/// files that no longer exist.
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
        // Compute current hashes
        let current_hashes = crate::infrastructure::scanner::compute_hashes(root, all_files);
        let mut current_map: HashMap<String, (String, f64)> = HashMap::new();
        for (path, hash, mtime) in &current_hashes {
            current_map.insert(path.clone(), (hash.clone(), *mtime));
        }

        // Load stored snapshots
        let mut stored_map: HashMap<String, String> = HashMap::new();
        if let Some(repo) = snapshot_repo {
            if let Ok(snapshots) = repo.list_snapshots(project).await {
                for s in &snapshots {
                    stored_map.insert(s.file_path.clone(), s.file_sha1.clone());
                }
            }
        }

        // Detect changes
        let (changed_paths, deleted_paths) =
            crate::infrastructure::scanner::detect_changes(&current_map, &stored_map);

        // Map changed relative paths back to absolute paths
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
        // For incremental builds, we don't wipe everything.
        // Individual file deletes happen at the file level in the pipeline.
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
