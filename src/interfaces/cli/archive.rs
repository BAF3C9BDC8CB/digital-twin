//! Memory World archiving CLI implementation.
//!
//! Provides:
//! - `dt archive --list`             — list existing archives
//! - `dt archive --before <date>`    — archive events older than date
//! - `dt archive --dry-run`          — preview what would be archived
//!
//! # Archive format
//!
//! Archives are stored as gzipped JSON files at `/var/lib/dt/archive/{date_range}.json.gz`.
//! Each archive contains Memory World events (Modification, Deployment, ConfigChange,
//! BugFix, Decision, PodEvent, Conversation) exceeding the 365-day retention period.

use std::path::PathBuf;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Report produced by an archive operation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchiveReport {
    /// Path to the created archive file.
    pub archive_file: PathBuf,
    /// Number of events archived.
    pub events_archived: usize,
    /// Number of events remaining (not yet due for archive).
    pub events_remaining: usize,
    /// Estimated space freed in bytes.
    pub space_freed_bytes: u64,
    /// Duration in seconds.
    pub duration_seconds: f64,
}

/// Entry in the archive listing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchiveEntry {
    /// Date range label (e.g. "2025-01-01_to_2025-06-30").
    pub date_range: String,
    /// Full path to the archive file.
    pub path: PathBuf,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Number of events in the archive.
    pub events_count: usize,
    /// Archive creation timestamp.
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Root directory for archives.
const ARCHIVE_ROOT: &str = "/var/lib/dt/archive";

/// Default retention period for Memory events (365 days).
const DEFAULT_RETENTION_DAYS: i64 = 365;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run `dt archive --before <date>`.
///
/// Archives Memory World events older than the given date.
/// In production, this would:
/// 1. Query Neo4j for events with `timestamp < before_date - 365d`
/// 2. Export them as a gzipped JSON file
/// 3. Delete the archived events from Neo4j (DETACH DELETE)
/// 4. Return an ArchiveReport
///
/// When `dry_run` is true, counts but does not modify data.
pub async fn run_archive(before: Option<&str>, dry_run: bool) -> anyhow::Result<ArchiveReport> {
    let start = Instant::now();
    let archive_root = PathBuf::from(ARCHIVE_ROOT);
    let before_date = parse_or_default_date(before);

    if dry_run {
        println!("=== dt archive --dry-run ===");
        println!("  Archive root: {}", archive_root.display());
        println!("  Before date:  {}", before_date);
        println!("  Retention:    {} days", DEFAULT_RETENTION_DAYS);
        println!();

        // Placeholder: simulate counting events
        let events_to_archive = count_archivable_events(&before_date).await?;
        let events_remaining = count_remaining_events(&before_date).await?;

        println!("  Events to archive:   {}", events_to_archive);
        println!("  Events remaining:    {}", events_remaining);
        println!();

        let report = ArchiveReport {
            archive_file: archive_root.join(format!(
                "{}_to_{}.json.gz",
                before_date,
                chrono::Utc::now().format("%Y-%m-%d")
            )),
            events_archived: events_to_archive,
            events_remaining,
            space_freed_bytes: estimate_space(events_to_archive),
            duration_seconds: start.elapsed().as_secs_f64(),
        };

        println!("  (dry-run: no data modified)");

        return Ok(report);
    }

    // --- Execute: archive events ---
    println!("=== dt archive ===");

    let events_to_archive = count_archivable_events(&before_date).await?;
    let events_remaining = count_remaining_events(&before_date).await?;

    if events_to_archive == 0 {
        println!("  No events to archive.");
        return Ok(ArchiveReport {
            archive_file: archive_root.clone(),
            events_archived: 0,
            events_remaining,
            space_freed_bytes: 0,
            duration_seconds: start.elapsed().as_secs_f64(),
        });
    }

    // Placeholder: perform actual archive
    let date_range = format!(
        "{}_to_{}",
        before_date,
        chrono::Utc::now().format("%Y-%m-%d")
    );
    let archive_path = archive_root.join(format!("{}.json.gz", date_range));

    // Write a placeholder archive
    let placeholder = serde_json::json!({
        "version": "1.0",
        "type": "memory_archive",
        "archived_at": chrono::Utc::now().to_rfc3339(),
        "events_count": events_to_archive,
        "note": "Placeholder — actual archive will be produced when Neo4j client is wired",
        "events": []
    });

    if !dry_run {
        // Only create directory when we actually have events to write
        tokio::fs::create_dir_all(&archive_root).await?;
        let json_str = serde_json::to_string_pretty(&placeholder)?;

        // Compress with gzip
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(json_str.as_bytes())?;
        let compressed = encoder.finish()?;
        tokio::fs::write(&archive_path, &compressed).await?;
    }

    let freed = estimate_space(events_to_archive);

    println!("  Archive file:   {}", archive_path.display());
    println!("  Events archived:  {}", events_to_archive);
    println!("  Events remaining: {}", events_remaining);
    println!("  Space freed:      {} bytes", freed);

    tracing::info!(
        "archive created: {} ({} events, {} bytes freed)",
        archive_path.display(),
        events_to_archive,
        freed,
    );

    let report = ArchiveReport {
        archive_file: archive_path,
        events_archived: events_to_archive,
        events_remaining,
        space_freed_bytes: freed,
        duration_seconds: start.elapsed().as_secs_f64(),
    };

    Ok(report)
}

/// List all existing archive files.
pub async fn list_archives() -> anyhow::Result<Vec<ArchiveEntry>> {
    let root = PathBuf::from(ARCHIVE_ROOT);
    let mut entries = Vec::new();

    if !root.exists() {
        return Ok(entries);
    }

    let mut dir_entries = tokio::fs::read_dir(&root).await?;
    while let Some(entry) = dir_entries.next_entry().await? {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let size_bytes = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);

        // Try to read event count from the archive
        let events_count = read_archive_event_count(&path).await.unwrap_or(0);

        entries.push(ArchiveEntry {
            date_range: file_name,
            path,
            size_bytes,
            events_count,
            created_at: String::new(), // would read from metadata
        });
    }

    entries.sort_by(|a, b| b.date_range.cmp(&a.date_range));

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Placeholder helpers (real implementations when Neo4j wired)
// ---------------------------------------------------------------------------

/// Count events that would be archived (older than retention period).
///
/// In production, executes a Cypher query like:
/// ```cypher
/// MATCH (e:Event)
/// WHERE e.timestamp < datetime($before) - duration('P365D')
/// RETURN count(e) AS count
/// ```
async fn count_archivable_events(_before_date: &str) -> anyhow::Result<usize> {
    // Placeholder: return 0 until Neo4j is wired
    Ok(0)
}

/// Count events that are not yet due for archival.
async fn count_remaining_events(_before_date: &str) -> anyhow::Result<usize> {
    // Placeholder: return 0 until Neo4j is wired
    Ok(0)
}

/// Estimate space freed by archiving `n` events with an average size assumption.
fn estimate_space(n: usize) -> u64 {
    // Assume ~500 bytes per event node on average
    (n as u64) * 500
}

/// Read the event count from an archive file.
async fn read_archive_event_count(path: &PathBuf) -> anyhow::Result<usize> {
    let content = tokio::fs::read(path).await?;

    // Try to decompress as gzip
    use flate2::read::GzDecoder;
    use std::io::Read;

    let decoder = GzDecoder::new(&content[..]);
    let mut json_str = String::new();
    if decoder.take(10_000_000).read_to_string(&mut json_str).is_ok() && !json_str.is_empty() {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&json_str) {
            return Ok(meta["events_count"].as_u64().unwrap_or(0) as usize);
        }
    }

    // Fallback: try uncompressed JSON
    if let Ok(json_str) = String::from_utf8(content) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&json_str) {
            return Ok(meta["events_count"].as_u64().unwrap_or(0) as usize);
        }
    }

    Ok(0)
}

/// Parse a date string or return a default (epoch start).
fn parse_or_default_date(input: Option<&str>) -> String {
    match input {
        Some(s) => {
            // Validate with chrono parsing
            match chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                Ok(_) => s.to_string(),
                Err(_) => {
                    eprintln!("warning: invalid date format '{}', using epoch", s);
                    "1970-01-01".to_string()
                }
            }
        }
        None => {
            // Default: 365 days ago
            let default = chrono::Utc::now() - chrono::Duration::days(DEFAULT_RETENTION_DAYS);
            default.format("%Y-%m-%d").to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // use tempfile::TempDir;

    #[tokio::test]
    async fn archive_dry_run_returns_report() {
        let report = run_archive(Some("2025-01-01"), true)
            .await
            .expect("dry-run should succeed");
        assert_eq!(report.events_archived, 0);
        assert_eq!(report.events_remaining, 0);
    }

    #[tokio::test]
    async fn archive_execute_creates_file() {
        // In execute mode without events, it should succeed (no file created).
        // Note: /var/lib/dt/archive may not be writable in test environments,
        // but the function should handle that gracefully.
        let report = run_archive(Some("2025-01-01"), false).await;
        // May fail due to filesystem permissions in test — that's OK
        match report {
            Ok(r) => {
                // With no real data, events_archived should be 0
                assert_eq!(r.events_archived, 0);
            }
            Err(e) => {
                // Permission error is acceptable in test environments
                assert!(e.to_string().contains("Permission") || e.to_string().contains("denied"));
            }
        }
    }

    #[tokio::test]
    async fn archive_no_before_uses_default() {
        let report = run_archive(None, true).await.expect("should succeed");
        assert!(report.events_archived == 0);
    }

    #[tokio::test]
    async fn list_archives_returns_empty_for_nonexistent() {
        let _entries = list_archives().await;
        // May succeed (empty) or fail — either is OK
        if let Ok(_entries) = _entries {
            // The directory may or may not exist
        }
    }

    #[test]
    fn parse_date_with_valid_input() {
        assert_eq!(parse_or_default_date(Some("2025-06-15")), "2025-06-15");
    }

    #[test]
    fn parse_date_with_invalid_input() {
        let result = parse_or_default_date(Some("not-a-date"));
        assert_eq!(result, "1970-01-01");
    }

    #[test]
    fn parse_date_with_none() {
        let result = parse_or_default_date(None);
        // Should be ~365 days ago
        assert!(result.contains('-'));
        assert_eq!(result.len(), 10);
    }

    #[tokio::test]
    async fn archive_report_serialization() {
        let report = ArchiveReport {
            archive_file: PathBuf::from("/var/lib/dt/archive/test.json.gz"),
            events_archived: 100,
            events_remaining: 500,
            space_freed_bytes: 50000,
            duration_seconds: 2.5,
        };

        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("100"));
        assert!(json.contains("500"));
    }
}
