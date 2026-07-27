//! Full rebuild strategy — wipes all project data and rebuilds from scratch.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::FileSnapshot;
use std::collections::HashMap;
use std::path::Path;

use super::BuildStrategy;

/// Full rebuild: process every file, delete all previous data for the project.
pub struct FullRebuildStrategy;

#[async_trait]
impl BuildStrategy for FullRebuildStrategy {
    fn name(&self) -> &'static str {
        "full"
    }

    fn force_rebuild(&self) -> bool {
        true
    }

    async fn select_files(
        &self,
        _root: &Path,
        all_files: &[std::path::PathBuf],
        _snapshot_repo: Option<&dyn SnapshotRepository>,
        _project: &str,
    ) -> Result<(Vec<std::path::PathBuf>, Vec<String>), DtError> {
        // In full rebuild, ALL files are candidates for processing
        Ok((all_files.to_vec(), Vec::new()))
    }

    async fn prepare(
        &self,
        graph: Option<&dyn GraphRepository>,
        _vector: Option<&dyn VectorRepository>,
        project: &str,
    ) -> Result<(), DtError> {
        // Delete all Method, Class, and Module nodes for this project
        if let Some(graph) = graph {
            let mut params = HashMap::new();
            params.insert("project".to_string(), serde_json::Value::String(project.to_string()));

            // Delete CALLS relationships first
            let _ = graph.write_query(
                "MATCH (m:Method {project: $project})-[r:CALLS]->() DELETE r",
                params.clone(),
            ).await;

            // Delete CONTAINS relationships
            let _ = graph.write_query(
                "MATCH (c:Class {project: $project})-[r:CONTAINS]->() DELETE r",
                params.clone(),
            ).await;

            // Delete methods
            let _ = graph.write_query(
                "MATCH (m:Method {project: $project}) DETACH DELETE m",
                params.clone(),
            ).await;

            // Delete classes
            let _ = graph.write_query(
                "MATCH (c:Class {project: $project}) DETACH DELETE c",
                params.clone(),
            ).await;

            // Delete modules
            let _ = graph.write_query(
                "MATCH (m:Module {project: $project}) DETACH DELETE m",
                params,
            ).await;
        }

        Ok(())
    }

    async fn update_snapshots(
        &self,
        snapshot_repo: &dyn SnapshotRepository,
        project: &str,
        snapshots: &[FileSnapshot],
    ) -> Result<(), DtError> {
        // For full rebuild, first clear all old snapshots, then insert fresh ones
        let _ = snapshot_repo.delete_project(project).await;
        snapshot_repo.save_snapshots(project, snapshots).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_name() {
        let s = FullRebuildStrategy;
        assert_eq!(s.name(), "full");
    }
}
