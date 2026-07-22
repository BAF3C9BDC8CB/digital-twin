//! Test data cleanup — removes all `test-` prefixed nodes from Memgraph and
//! all `test-` prefixed collections from Qdrant.
//!
//! This ensures the test runner is self-contained and leaves no trace by
//! default (unless `--keep` is passed).

use crate::domain::traits::{GraphRepository, VectorRepository};
use std::collections::HashMap;
use std::sync::Arc;

/// Delete all nodes whose labels contain the `test-` prefix and all `test-*`
/// Qdrant collections.
///
/// Returns the total number of nodes deleted.
pub async fn cleanup_test_data(
    graph: &Arc<dyn GraphRepository>,
    vector: &Arc<dyn VectorRepository>,
) -> Result<usize, String> {
    let mut total_cleaned = 0usize;

    // ── Step 1: Delete test- nodes from Memgraph ──────────────────────
    // We use a Cypher query that matches any node whose labels contain the
    // `test-` prefix and detach-deletes them all in one shot, returning the
    // count.
    let delete_query = concat!(
        "MATCH (n) ",
        "WHERE any(label IN labels(n) WHERE label STARTS WITH 'test-') ",
        "DETACH DELETE n ",
        "RETURN count(*) AS deleted"
    );

    match graph
        .write_query(delete_query, HashMap::new())
        .await
    {
        Ok(result) => {
            if let Some(arr) = result.as_array() {
                if let Some(first) = arr.first() {
                    if let Some(count) = first.get("deleted").and_then(|v| v.as_i64()) {
                        total_cleaned += count as usize;
                    }
                }
            }
            tracing::info!(
                deleted = total_cleaned,
                "Cleaned up test- prefixed graph nodes"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to clean up test- graph nodes");
            return Err(format!("graph cleanup failed: {e}"));
        }
    }

    // ── Step 2: Delete test- collections from Qdrant ──────────────────
    match vector.list_collections().await {
        Ok(collections) => {
            let test_collections: Vec<String> = collections
                .into_iter()
                .filter(|name| name.starts_with("test-"))
                .collect();

            for name in &test_collections {
                match vector.delete_collection(name).await {
                    Ok(_) => {
                        tracing::info!(collection = %name, "Deleted test- Qdrant collection");
                    }
                    Err(e) => {
                        tracing::warn!(
                            collection = %name,
                            error = %e,
                            "Failed to delete test- Qdrant collection"
                        );
                    }
                }
            }

            if !test_collections.is_empty() {
                tracing::info!(
                    count = test_collections.len(),
                    "Cleaned up test- prefixed Qdrant collections"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list Qdrant collections for cleanup");
            return Err(format!("Qdrant list_collections failed: {e}"));
        }
    }

    Ok(total_cleaned)
}
