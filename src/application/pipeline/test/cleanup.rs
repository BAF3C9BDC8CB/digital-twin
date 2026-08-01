//! Test data cleanup — removes all `test-` prefixed nodes from Memgraph and
//! all `test-` prefixed collections from Qdrant.
//!
//! This ensures the test runner is self-contained and leaves no trace by
//! default (unless `--keep` is passed).

use crate::domain::traits::{GraphRepository, SnapshotRepository, VectorRepository};
use std::collections::HashMap;
use std::sync::Arc;

/// Delete all test data: nodes where project starts with "test-" AND
/// nodes whose labels contain `test-` prefix. Also deletes test-* Qdrant collections
/// and test-pipeline snapshots from SQLite.
pub async fn cleanup_test_data(
    graph: &Arc<dyn GraphRepository>,
    vector: &Arc<dyn VectorRepository>,
    snapshot: Option<&Arc<dyn SnapshotRepository>>,
) -> Result<usize, String> {
    let mut total_cleaned = 0usize;

    // Delete by project property (new approach)
    let q1 =
        "MATCH (n) WHERE n.project = 'test-pipeline' DETACH DELETE n RETURN count(*) AS deleted";
    match graph.write_query(q1, HashMap::new()).await {
        Ok(result) => {
            if let Some(arr) = result.as_array().and_then(|a| a.first()) {
                if let Some(c) = arr.get("deleted").and_then(|v| v.as_i64()) {
                    total_cleaned += c as usize;
                }
            }
        }
        Err(e) => tracing::warn!("cleanup: delete test-pipeline nodes failed: {e}"),
    }

    // Delete by label prefix (old approach, for any remaining data)
    let q2 = "MATCH (n) WHERE any(label IN labels(n) WHERE label STARTS WITH 'test-') DETACH DELETE n RETURN count(*) AS deleted";
    match graph.write_query(q2, HashMap::new()).await {
        Ok(result) => {
            if let Some(arr) = result.as_array().and_then(|a| a.first()) {
                if let Some(c) = arr.get("deleted").and_then(|v| v.as_i64()) {
                    total_cleaned += c as usize;
                }
            }
        }
        Err(e) => tracing::warn!("cleanup: delete test- labelled nodes failed: {e}"),
    }

    // Clear SQLite snapshots so full rebuild re-processes all files including documents.
    // ②c fix: failures are logged, not swallowed — a silent failure here leaves stale
    // incremental progress behind, which makes the next `dt build --test` skip every
    // file while the KG is empty (the 2026-08-01 stale-progress incident).
    match snapshot {
        Some(snapshot) => {
            if let Err(e) = snapshot.delete_project("test-pipeline").await {
                tracing::warn!("cleanup: delete_project(file_snapshots) failed: {e}");
            }
            if let Err(e) = snapshot.clear_llm_progress("test-pipeline").await {
                tracing::warn!("cleanup: clear_llm_progress failed: {e}");
            }
            if let Err(e) = snapshot.clear_step_progress("test-pipeline").await {
                tracing::warn!("cleanup: clear_step_progress failed: {e}");
            }
        }
        None => {
            tracing::warn!(
                "cleanup: no SQLite snapshot store — incremental progress NOT cleared; \
                 next `dt build --test` may skip all files against an empty KG"
            );
        }
    }

    tracing::info!(deleted = total_cleaned, "Cleaned up test data (graph)");

    // Delete test- Qdrant collections
    match vector.list_collections().await {
        Ok(collections) => {
            let test_cols: Vec<String> = collections
                .into_iter()
                .filter(|name| name.starts_with("test-"))
                .collect();
            for name in &test_cols {
                if let Err(e) = vector.delete_collection(name).await {
                    tracing::warn!("cleanup: delete Qdrant collection {name} failed: {e}");
                }
            }
            if !test_cols.is_empty() {
                tracing::info!(
                    count = test_cols.len(),
                    "Cleaned up test Qdrant collections"
                );
            }
        }
        Err(e) => tracing::warn!("cleanup: list Qdrant collections failed: {e}"),
    }

    Ok(total_cleaned)
}
