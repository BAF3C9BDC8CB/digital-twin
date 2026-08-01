//! Full rebuild strategy — wipes all project data and rebuilds from scratch.

use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::FileSnapshot;
use crate::shared::collections::{DOC_CHUNKS, KG_NODES};
use async_trait::async_trait;
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
        vector: Option<&dyn VectorRepository>,
        project: &str,
    ) -> Result<(), DtError> {
        // §7.5: clear this project's vectors first — extracted-entity points
        // (kg_nodes) and evidence blocks (doc_chunks) are project-scoped via
        // the payload `project` key. Failures are logged, not fatal: the
        // subsequent upserts are idempotent by deterministic point ids.
        if let Some(vector) = vector {
            let project_filter = serde_json::json!({
                "must": [{"key": "project", "match": {"value": project}}],
            });
            for collection in [KG_NODES, DOC_CHUNKS] {
                if let Err(e) = vector
                    .delete_by_filter(collection, project_filter.clone())
                    .await
                {
                    tracing::warn!(
                        "[full_rebuild] clear {collection} vectors for {project} failed: {e}"
                    );
                }
            }
        }

        // Delete all Method, Class, and Module nodes for this project
        if let Some(graph) = graph {
            let mut params = HashMap::new();
            params.insert(
                "project".to_string(),
                serde_json::Value::String(project.to_string()),
            );

            // Delete CALLS relationships first
            let _ = graph
                .write_query(
                    "MATCH (m:Method {project: $project})-[r:CALLS]->() DELETE r",
                    params.clone(),
                )
                .await;

            // Delete CONTAINS relationships
            let _ = graph
                .write_query(
                    "MATCH (c:Class {project: $project})-[r:CONTAINS]->() DELETE r",
                    params.clone(),
                )
                .await;

            // Delete methods
            let _ = graph
                .write_query(
                    "MATCH (m:Method {project: $project}) DETACH DELETE m",
                    params.clone(),
                )
                .await;

            // Delete classes
            let _ = graph
                .write_query(
                    "MATCH (c:Class {project: $project}) DETACH DELETE c",
                    params.clone(),
                )
                .await;

            // Delete modules
            let _ = graph
                .write_query(
                    "MATCH (m:Module {project: $project}) DETACH DELETE m",
                    params,
                )
                .await;
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
