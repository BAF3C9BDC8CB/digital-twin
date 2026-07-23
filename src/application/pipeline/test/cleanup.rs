//! Test data cleanup — removes all `test-` prefixed nodes from Memgraph and
//! all `test-` prefixed collections from Qdrant.
//!
//! This ensures the test runner is self-contained and leaves no trace by
//! default (unless `--keep` is passed).

use crate::domain::traits::{GraphRepository, VectorRepository};
use std::collections::HashMap;
use std::sync::Arc;

/// Delete all test data: nodes where project starts with "test-" AND
/// nodes whose labels contain `test-` prefix. Also deletes test-* Qdrant collections.
pub async fn cleanup_test_data(
    graph: &Arc<dyn GraphRepository>,
    vector: &Arc<dyn VectorRepository>,
) -> Result<usize, String> {
    let mut total_cleaned = 0usize;

    // Delete by project property (new approach)
    let q1 = "MATCH (n) WHERE n.project = 'test-pipeline' DETACH DELETE n RETURN count(*) AS deleted";
    if let Ok(result) = graph.write_query(q1, HashMap::new()).await {
        if let Some(arr) = result.as_array().and_then(|a| a.first()) {
            if let Some(c) = arr.get("deleted").and_then(|v| v.as_i64()) {
                total_cleaned += c as usize;
            }
        }
    }

    // Delete by label prefix (old approach, for any remaining data)
    let q2 = "MATCH (n) WHERE any(label IN labels(n) WHERE label STARTS WITH 'test-') DETACH DELETE n RETURN count(*) AS deleted";
    if let Ok(result) = graph.write_query(q2, HashMap::new()).await {
        if let Some(arr) = result.as_array().and_then(|a| a.first()) {
            if let Some(c) = arr.get("deleted").and_then(|v| v.as_i64()) {
                total_cleaned += c as usize;
            }
        }
    }

    tracing::info!(deleted = total_cleaned, "Cleaned up test data (graph)");

    // Delete test- Qdrant collections
    if let Ok(collections) = vector.list_collections().await {
        let test_cols: Vec<String> = collections.into_iter()
            .filter(|name| name.starts_with("test-"))
            .collect();
        for name in &test_cols {
            let _ = vector.delete_collection(name).await;
        }
        if !test_cols.is_empty() {
            tracing::info!(count = test_cols.len(), "Cleaned up test Qdrant collections");
        }
    }

    Ok(total_cleaned)
}
