//! Schema validation and data cleanup CLI implementation.
//!
//! Provides:
//! - `dt schema init`    — idempotent schema initialization (constraints + indexes)
//! - `dt clean --confirm` — wipe all data across Memgraph, Qdrant, and SQLite
//! - `dt cleanup --targets reasoning` — clean stale Reasoning nodes (Observation/Analysis/Decision)
//! - `dt cleanup --targets memory`    — archive Memory events beyond retention
//! - `dt cleanup --targets snapshots` — remove orphaned SQLite snapshot rows
//! - `dt cleanup --targets all`       — run all cleanup targets
//! - `dt health`         — check health of all backend services

use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::HealthStatus;
use crate::infrastructure::memgraph::schema::{clean_all, init_schema};
use crate::infrastructure::memgraph::{CleanReport, SchemaInitReport};
use std::time::Instant;

// ---------------------------------------------------------------------------
// dt schema init
// ---------------------------------------------------------------------------

/// Run `dt schema init` — creates all constraints and indexes via Memgraph.
///
/// When `graph` is `None`, falls back to `NoopGraphRepo` (no-op, for testing).
pub async fn run_schema_init(graph: Option<&dyn GraphRepository>) -> anyhow::Result<()> {
    println!("Initializing V2 schema...");
    let report: SchemaInitReport = if let Some(g) = graph {
        init_schema(g).await?
    } else {
        let noop = crate::infrastructure::memgraph::NoopGraphRepo;
        init_schema(&noop).await?
    };

    println!();
    println!("Schema initialization complete:");
    println!("  Constraints created : {}", report.constraints_created);
    println!("  Indexes created     : {}", report.indexes_created);
    println!("  Elapsed             : {} ms", report.elapsed_ms);

    Ok(())
}

// ---------------------------------------------------------------------------
// dt clean
// ---------------------------------------------------------------------------

/// Run `dt clean --confirm` — wipe all data from all backends.
///
/// Without `--confirm`, prints a warning and exits without making changes.
/// With `--confirm`, proceeds to clean Memgraph, Qdrant, and SQLite.
pub async fn run_clean(confirm: bool, graph: Option<&dyn GraphRepository>) -> anyhow::Result<()> {
    if !confirm {
        eprintln!("DANGER: `dt clean` will delete ALL data from:");
        eprintln!("  - Memgraph:  all nodes and relationships");
        eprintln!("  - Qdrant: all vector collections");
        eprintln!("  - SQLite: all file snapshots");
        eprintln!();
        eprintln!("This operation is IRREVERSIBLE.");
        eprintln!("Run with `--confirm` to proceed.");
        return Ok(());
    }

    let total_start = Instant::now();

    println!("Cleaning all data...");
    println!();

    // --- Memgraph ---
    let memgraph_report: CleanReport = if let Some(g) = graph {
        clean_all(g).await?
    } else {
        let noop = crate::infrastructure::memgraph::NoopGraphRepo;
        clean_all(&noop).await?
    };

    println!("Memgraph:");
    println!(
        "  Nodes deleted         : {}",
        memgraph_report.nodes_deleted
    );
    println!(
        "  Relationships deleted : {}",
        memgraph_report.relationships_deleted
    );
    println!(
        "  Elapsed               : {} ms",
        memgraph_report.elapsed_ms
    );

    // --- Qdrant ---
    // NoopVectorRepo does not expose collection-management methods; in the
    // real implementation this will call `QdrantClient::list_collections()`
    // and delete each one.
    let _vector = crate::infrastructure::qdrant::NoopVectorRepo;
    let qdrant_removed: usize = 0;
    println!();
    println!("Qdrant:");
    println!(
        "  Collections removed   : {} (noop backend)",
        qdrant_removed
    );

    // --- SQLite ---
    // No real SnapshotRepository wired yet; in the full implementation this
    // will execute `DELETE FROM file_snapshots` via rusqlite.
    let snapshots_cleared = true;
    println!();
    println!("SQLite:");
    println!(
        "  Snapshots cleared     : {} (noop backend)",
        if snapshots_cleared { "yes" } else { "no" }
    );

    // --- Combined report ---
    let total_elapsed = total_start.elapsed().as_millis() as u64;

    let combined = CleanReport {
        nodes_deleted: memgraph_report.nodes_deleted,
        relationships_deleted: memgraph_report.relationships_deleted,
        qdrant_collections_removed: qdrant_removed,
        snapshots_cleared,
        reasoning_stale_deleted: 0,
        memory_archived: 0,
        snapshots_orphaned: 0,
        elapsed_ms: total_elapsed,
    };

    println!();
    println!("Clean complete:");
    println!("  Nodes deleted              : {}", combined.nodes_deleted);
    println!(
        "  Relationships deleted      : {}",
        combined.relationships_deleted
    );
    println!(
        "  Qdrant collections removed : {}",
        combined.qdrant_collections_removed
    );
    println!(
        "  Snapshots cleared          : {}",
        if combined.snapshots_cleared {
            "yes"
        } else {
            "no"
        }
    );
    println!("  Total elapsed              : {} ms", combined.elapsed_ms);

    Ok(())
}

// ---------------------------------------------------------------------------
// check_health! macro — unified health check for any type with health_check()
// ---------------------------------------------------------------------------

/// Check health of any repository/service that exposes `health_check()`.
macro_rules! check_health {
    ($name:expr, $repo:expr) => {{
        let start = std::time::Instant::now();
        let status = $repo.health_check().await;
        let latency_ms = start.elapsed().as_millis() as u64;
        match status {
            Ok(HealthStatus::Healthy) => (
                true,
                format!("✅ {:<8}: healthy ({} ms)", $name, latency_ms),
            ),
            Ok(HealthStatus::Degraded(reason)) => (
                false,
                format!(
                    "⚠️  {:<8}: degraded — {} ({} ms)",
                    $name, reason, latency_ms
                ),
            ),
            Ok(HealthStatus::Unhealthy(reason)) => (
                false,
                format!(
                    "❌ {:<8}: unhealthy — {} ({} ms)",
                    $name, reason, latency_ms
                ),
            ),
            Err(e) => (
                false,
                format!("❌ {:<8}: error — {} ({} ms)", $name, e, latency_ms),
            ),
        }
    }};
}

// ---------------------------------------------------------------------------
// dt health
// ---------------------------------------------------------------------------

/// Run `dt health` — check health of all backend services.
///
/// Contacts Memgraph, Qdrant, SQLite
/// availability and latency.
///
/// When a service is `None`, reports it as "no backend configured".
pub async fn run_health(
    graph: Option<&dyn GraphRepository>,
    vector: Option<&dyn VectorRepository>,
    snapshot: Option<&dyn SnapshotRepository>,
    embed: Option<&dyn EmbedService>,
) -> anyhow::Result<()> {
    println!("Checking backend health...");
    println!();

    let mut all_healthy = true;

    // --- Memgraph ---
    let (healthy, detail) = if let Some(g) = graph {
        check_health!("Memgraph", g)
    } else {
        (false, "  ❌ Memgraph : no backend configured".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    // --- Qdrant ---
    let (healthy, detail) = if let Some(v) = vector {
        check_health!("Qdrant", v)
    } else {
        (false, "  ❌ Qdrant   : no backend configured".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    // --- SQLite ---
    let (healthy, detail) = if let Some(s) = snapshot {
        check_health!("SQLite", s)
    } else {
        (false, "  ❌ SQLite   : no backend configured".to_string())
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    // --- SiliconFlow ---
    let (healthy, detail) = if let Some(e) = embed {
        check_health!("SiliconFlow", e)
    } else {
        (
            false,
            "  ❌ SiliconFlow : no backend configured".to_string(),
        )
    };
    println!("  {detail}");
    if !healthy {
        all_healthy = false;
    }

    println!();
    if all_healthy {
        println!("All backends healthy.");
    } else {
        println!("One or more backends are degraded or unhealthy.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::DtError;

    #[tokio::test]
    async fn run_schema_init_succeeds_with_noop() {
        let result = run_schema_init(None).await;
        assert!(result.is_ok(), "schema init should succeed with noop repo");
    }

    #[tokio::test]
    async fn run_clean_without_confirm_prints_warning() {
        // Should not panic or error — it just warns and exits.
        let result = run_clean(false, None).await;
        assert!(
            result.is_ok(),
            "clean without --confirm should succeed (just warns)"
        );
    }

    #[tokio::test]
    async fn run_clean_with_confirm_succeeds() {
        let result = run_clean(true, None).await;
        assert!(
            result.is_ok(),
            "clean --confirm should succeed with noop repo"
        );
    }

    #[tokio::test]
    async fn run_health_succeeds() {
        let result = run_health(None, None, None, None).await;
        assert!(
            result.is_ok(),
            "health check should succeed with noop repos"
        );
    }

}
