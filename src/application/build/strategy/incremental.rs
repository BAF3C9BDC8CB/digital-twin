//! Incremental build strategy — only processes files that have changed since
//! the last build, using SHA1 hashes stored in SQLite.

use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::FileSnapshot;
use async_trait::async_trait;
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
        // Load stored snapshots FIRST (now we keep mtime for fast-path).
        let mut stored_map: HashMap<String, (String, f64)> = HashMap::new();
        if let Some(repo) = snapshot_repo {
            if let Ok(snapshots) = repo.list_snapshots(project).await {
                for s in &snapshots {
                    stored_map.insert(s.file_path.clone(), (s.file_sha1.clone(), s.file_mtime));
                }
            }
        }

        // Build current-hash map, using mtime fast-path: if a file's mtime
        // matches the stored snapshot, reuse the stored hash (skip SHA-256).
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

            // mtime unchanged → reuse stored hash (no file read, no SHA-256)
            if let Some((stored_hash, stored_mtime)) = stored_map.get(&rel) {
                if (current_mtime - stored_mtime).abs() < 1.0 {
                    current_map.insert(rel, (stored_hash.clone(), current_mtime));
                    continue;
                }
            }

            // mtime changed or new file → compute SHA-256
            if let Ok((hash, _)) = crate::infrastructure::scanner::compute_file_hash(path) {
                current_map.insert(rel, (hash, current_mtime));
            }
        }

        // Detect changes (same logic, now with mtime-aware current_map)
        let mut stored_hash_only: HashMap<String, String> = HashMap::new();
        for (k, (hash, _)) in &stored_map {
            stored_hash_only.insert(k.clone(), hash.clone());
        }
        let (changed_paths, deleted_paths) =
            crate::infrastructure::scanner::detect_changes(&current_map, &stored_hash_only);

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
