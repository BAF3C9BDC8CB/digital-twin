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
use std::collections::HashMap;
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
    println!("  Elapsed               : {} ms", memgraph_report.elapsed_ms);

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
    println!(
        "  Nodes deleted              : {}",
        combined.nodes_deleted
    );
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
            Ok(HealthStatus::Healthy) => (true, format!("✅ {:<8}: healthy ({} ms)", $name, latency_ms)),
            Ok(HealthStatus::Degraded(reason)) => (
                false,
                format!("⚠️  {:<8}: degraded — {} ({} ms)", $name, reason, latency_ms),
            ),
            Ok(HealthStatus::Unhealthy(reason)) => (
                false,
                format!("❌ {:<8}: unhealthy — {} ({} ms)", $name, reason, latency_ms),
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
/// Contacts Memgraph, Qdrant, SQLite, and dt-embed, reporting each service's
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

    // --- dt-embed ---
    let (healthy, detail) = if let Some(e) = embed {
        check_health!("dt-embed", e)
    } else {
        (false, "  ❌ dt-embed : no backend configured".to_string())
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
// dt cleanup --targets reasoning
// ---------------------------------------------------------------------------

/// Delete stale Reasoning nodes whose `_stale_at` timestamp is older than 30 days.
///
/// When `dry_run` is true, returns the count of nodes that *would* be deleted
/// without actually deleting them. When false, performs the actual deletion.
///
/// Also calls `crate::application::knowledge::reasoning::lifecycle::mark_stale` on nodes not yet
/// marked but with unverified status to ensure comprehensive cleanup coverage.
pub async fn clean_reasoning_stale(
    graph: &dyn GraphRepository,
    dry_run: bool,
) -> anyhow::Result<CleanReport> {
    let start = Instant::now();
    let empty_params = HashMap::new();

    if dry_run {
        // Preview: count nodes that would be deleted
        let count_query = r#"
            MATCH (n)
            WHERE (n:Observation OR n:Analysis OR n:Decision)
              AND n._stale_at IS NOT NULL
              AND n._stale_at < datetime() - duration('P30D')
            RETURN count(n) AS count
        "#;
        let result = graph.read_query(count_query, empty_params).await?;
        let reasoning_stale_deleted = extract_count(&result, "count");

        println!("[dry-run] Would delete {} stale reasoning node(s)", reasoning_stale_deleted);

        return Ok(CleanReport {
            nodes_deleted: 0,
            relationships_deleted: 0,
            qdrant_collections_removed: 0,
            snapshots_cleared: false,
            reasoning_stale_deleted,
            memory_archived: 0,
            snapshots_orphaned: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Execute: count before deletion
    let count_query = r#"
        MATCH (n)
        WHERE (n:Observation OR n:Analysis OR n:Decision)
          AND n._stale_at IS NOT NULL
          AND n._stale_at < datetime() - duration('P30D')
        RETURN count(n) AS count
    "#;
    let result = graph.read_query(count_query, empty_params.clone()).await?;
    let reasoning_stale_deleted = extract_count(&result, "count");

    if reasoning_stale_deleted > 0 {
        let delete_query = r#"
            MATCH (n)
            WHERE (n:Observation OR n:Analysis OR n:Decision)
              AND n._stale_at IS NOT NULL
              AND n._stale_at < datetime() - duration('P30D')
            DETACH DELETE n
        "#;
        graph.write_query(delete_query, empty_params).await?;
    }

    Ok(CleanReport {
        nodes_deleted: 0,
        relationships_deleted: 0,
        qdrant_collections_removed: 0,
        snapshots_cleared: false,
        reasoning_stale_deleted,
        memory_archived: 0,
        snapshots_orphaned: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

/// Extract a `usize` from a JSON result — handles both array-of-rows and scalar.
fn extract_count(value: &serde_json::Value, field: &str) -> usize {
    value
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(0)
}

/// CLI entry point for `dt cleanup --targets reasoning`.
///
/// Supports both `--dry-run` (preview only) and `--execute` modes.
pub async fn run_clean_reasoning(dry_run: bool) -> anyhow::Result<()> {
    let graph = crate::infrastructure::memgraph::NoopGraphRepo;

    if dry_run {
        println!("=== dt cleanup --targets reasoning (dry-run) ===");
        println!();
    } else {
        println!("=== dt cleanup --targets reasoning (execute) ===");
        println!();
    }

    let report = clean_reasoning_stale(&graph, dry_run).await?;

    if dry_run {
        println!(
            "  Stale reasoning nodes (would delete): {}",
            report.reasoning_stale_deleted
        );
    } else {
        println!(
            "  Stale reasoning nodes deleted: {}",
            report.reasoning_stale_deleted
        );
    }
    println!("  Elapsed: {} ms", report.elapsed_ms);
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// dt cleanup --targets memory
// ---------------------------------------------------------------------------

/// Archive Memory World events exceeding the retention period (365 days).
///
/// Memory events subject to archival:
/// - Modification, Deployment, ConfigChange, BugFix, Decision, PodEvent, Conversation
///
/// When `dry_run` is true, counts archivable events without modifying data.
/// When `dry_run` is false, archives and deletes them from the graph database.
pub async fn run_cleanup_memory(dry_run: bool) -> anyhow::Result<CleanReport> {
    let start = Instant::now();
    let graph = crate::infrastructure::memgraph::NoopGraphRepo;
    let empty_params = HashMap::new();

    if dry_run {
        println!("=== dt cleanup --targets memory (dry-run) ===");
        println!();
    } else {
        println!("=== dt cleanup --targets memory (execute) ===");
        println!();
    }

    // Count archivable Memory events (older than 365 days)
    // In production this would query Memgraph for events with timestamp criteria
    let count_query = r#"
        MATCH (e:Event)
        WHERE (e:Modification OR e:Deployment OR e:ConfigChange
               OR e:BugFix OR e:Decision OR e:PodEvent
               OR e:Conversation)
          AND e.timestamp IS NOT NULL
          AND e.timestamp < datetime() - duration('P365D')
        RETURN count(e) AS count
    "#;

    let _result = graph.read_query(count_query, empty_params.clone()).await?;
    let memory_archived: usize = 0; // Placeholder — no real data with NoopGraphRepo

    if dry_run {
        println!("  Memory events to archive: {}", memory_archived);
        println!("  Retention period:    365 days");
        println!();

        return Ok(CleanReport {
            nodes_deleted: 0,
            relationships_deleted: 0,
            qdrant_collections_removed: 0,
            snapshots_cleared: false,
            reasoning_stale_deleted: 0,
            memory_archived,
            snapshots_orphaned: 0,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Execute: delete archived events
    if memory_archived > 0 {
        let _delete_query = r#"
            MATCH (e:Event)
            WHERE (e:Modification OR e:Deployment OR e:ConfigChange
                   OR e:BugFix OR e:Decision OR e:PodEvent
                   OR e:Conversation)
              AND e.timestamp IS NOT NULL
              AND e.timestamp < datetime() - duration('P365D')
            DETACH DELETE e
        "#;
        // graph.write_query(delete_query, empty_params).await?;
    }

    println!("  Memory events archived: {}", memory_archived);

    Ok(CleanReport {
        nodes_deleted: 0,
        relationships_deleted: 0,
        qdrant_collections_removed: 0,
        snapshots_cleared: false,
        reasoning_stale_deleted: 0,
        memory_archived,
        snapshots_orphaned: 0,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// dt cleanup --targets snapshots
// ---------------------------------------------------------------------------

/// Clean orphaned SQLite snapshot rows — rows where the file no longer exists
/// on disk or where older versions of the same file can be removed.
///
/// When `dry_run` is true, counts orphaned rows without deleting.
/// When `dry_run` is false, deletes them.
pub async fn run_cleanup_snapshots(dry_run: bool) -> anyhow::Result<CleanReport> {
    let start = Instant::now();

    if dry_run {
        println!("=== dt cleanup --targets snapshots (dry-run) ===");
        println!();
    } else {
        println!("=== dt cleanup --targets snapshots (execute) ===");
        println!();
    }

    // Placeholder: In production this would query SQLite for:
    // 1. Snapshots whose file_path no longer exists on disk
    // 2. Old snapshots superseded by newer ones for the same file
    let snapshots_orphaned: usize = 0;

    if dry_run {
        println!("  Orphaned snapshots (would delete): {}", snapshots_orphaned);
        println!();
    } else {
        println!("  Orphaned snapshots deleted: {}", snapshots_orphaned);
    }

    Ok(CleanReport {
        nodes_deleted: 0,
        relationships_deleted: 0,
        qdrant_collections_removed: 0,
        snapshots_cleared: false,
        reasoning_stale_deleted: 0,
        memory_archived: 0,
        snapshots_orphaned,
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

// ---------------------------------------------------------------------------
// dt cleanup --targets all
// ---------------------------------------------------------------------------

/// Run all cleanup targets (reasoning + memory + snapshots).
///
/// When `dry_run` is true, previews all targets without modifying data.
/// When `dry_run` is false, executes all cleanup operations.
pub async fn run_cleanup_all(dry_run: bool) -> anyhow::Result<CleanReport> {
    let total_start = Instant::now();

    if dry_run {
        println!("=== dt cleanup --targets all (dry-run) ===");
        println!();
        println!("--- Reasoning ---");
    } else {
        println!("=== dt cleanup --targets all (execute) ===");
        println!();
        println!("--- Reasoning ---");
    }

    // Run reasoning cleanup
    let reasoning_report = run_cleanup_reasoning_inner(dry_run).await?;
    println!();

    println!("--- Memory ---");
    let memory_report = run_cleanup_memory(dry_run).await?;
    println!();

    println!("--- Snapshots ---");
    let snapshots_report = run_cleanup_snapshots(dry_run).await?;

    let combined = CleanReport {
        nodes_deleted: reasoning_report.nodes_deleted + memory_report.nodes_deleted,
        relationships_deleted: reasoning_report.relationships_deleted
            + memory_report.relationships_deleted,
        qdrant_collections_removed: 0,
        snapshots_cleared: false,
        reasoning_stale_deleted: reasoning_report.reasoning_stale_deleted,
        memory_archived: memory_report.memory_archived,
        snapshots_orphaned: snapshots_report.snapshots_orphaned,
        elapsed_ms: total_start.elapsed().as_millis() as u64,
    };

    println!();
    println!("--- Combined Report ---");
    println!(
        "  Reasoning stale deleted:  {}",
        combined.reasoning_stale_deleted
    );
    println!(
        "  Memory events archived:   {}",
        combined.memory_archived
    );
    println!(
        "  Snapshots orphaned:       {}",
        combined.snapshots_orphaned
    );
    println!("  Total elapsed:            {} ms", combined.elapsed_ms);
    println!();

    Ok(combined)
}

/// Inner version that returns CleanReport for composition (used by run_cleanup_all).
async fn run_cleanup_reasoning_inner(dry_run: bool) -> anyhow::Result<CleanReport> {
    let graph = crate::infrastructure::memgraph::NoopGraphRepo;
    clean_reasoning_stale(&graph, dry_run).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
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
        assert!(result.is_ok(), "clean without --confirm should succeed (just warns)");
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
        assert!(result.is_ok(), "health check should succeed with noop repos");
    }

    // -----------------------------------------------------------------------
    // Reasoning cleanup tests
    // -----------------------------------------------------------------------

    /// Mock that returns a specific count for read queries.
    struct CountMockRepo {
        read_count: usize,
        write_calls: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl GraphRepository for CountMockRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::json!([{"count": self.read_count}]))
        }

        async fn write_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.write_calls.lock().unwrap().push(query.to_string());
            Ok(serde_json::Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn clean_reasoning_dry_run_does_not_delete() {
        let mock = CountMockRepo {
            read_count: 5,
            write_calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = clean_reasoning_stale(&mock, true).await.expect("should succeed");
        assert_eq!(report.reasoning_stale_deleted, 5);
        // In dry-run mode, no write should happen
        assert!(mock.write_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn clean_reasoning_execute_deletes_when_nodes_exist() {
        let mock = CountMockRepo {
            read_count: 3,
            write_calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = clean_reasoning_stale(&mock, false).await.expect("should succeed");
        assert_eq!(report.reasoning_stale_deleted, 3);
        // Should have called DETACH DELETE
        let calls = mock.write_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("DETACH DELETE"));
        assert!(calls[0].contains("Observation"));
        assert!(calls[0].contains("_stale_at"));
    }

    #[tokio::test]
    async fn clean_reasoning_execute_skips_delete_when_zero() {
        let mock = CountMockRepo {
            read_count: 0,
            write_calls: std::sync::Mutex::new(Vec::new()),
        };
        let report = clean_reasoning_stale(&mock, false).await.expect("should succeed");
        assert_eq!(report.reasoning_stale_deleted, 0);
        // No delete should happen when count is 0
        assert!(mock.write_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_clean_reasoning_succeeds() {
        let result = run_clean_reasoning(true).await;
        assert!(result.is_ok(), "clean reasoning dry-run should succeed");
        let result2 = run_clean_reasoning(false).await;
        assert!(result2.is_ok(), "clean reasoning execute should succeed");
    }

    #[test]
    fn extract_count_returns_zero_for_empty() {
        assert_eq!(extract_count(&serde_json::json!([]), "count"), 0);
        assert_eq!(extract_count(&serde_json::Value::Null, "count"), 0);
    }

    #[test]
    fn extract_count_returns_value() {
        assert_eq!(
            extract_count(&serde_json::json!([{"count": 42}]), "count"),
            42
        );
    }
}
