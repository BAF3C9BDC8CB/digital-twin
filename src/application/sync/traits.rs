//! Synchronisation traits and types for the Digital Twin system.

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ---------------------------------------------------------------------------
// SyncReport
// ---------------------------------------------------------------------------

/// Result of a single sync operation against one resource type.
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    /// Human-readable source name (e.g. "nacos/test").
    pub source: String,

    // -- Nacos-specific --
    /// Number of namespaces processed.
    pub namespaces: usize,
    /// Number of configs upserted.
    pub configs: usize,
    /// Number of services synced.
    pub services: usize,
    /// Number of relationships created/updated.
    pub links_created: usize,

    // -- K8s-specific --
    /// Items fetched from the external API.
    pub items_fetched: usize,
    /// Items newly created in the graph database.
    pub items_created: usize,
    /// Items that already existed and were updated.
    pub items_updated: usize,
    /// Items skipped due to conflict or dedup.
    pub items_skipped: usize,
    /// Items that failed to write.
    pub items_failed: usize,
    /// Error messages collected during the sync (non-fatal).
    pub errors: Vec<String>,

    /// Wall-clock elapsed in milliseconds.
    pub elapsed_ms: u64,
    /// `true` when sync was skipped (WriteCoordinator conflict).
    pub skipped: bool,
}

impl SyncReport {
    /// Create a "skipped" report.
    pub fn skipped(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            namespaces: 0,
            configs: 0,
            services: 0,
            links_created: 0,
            items_fetched: 0,
            items_created: 0,
            items_updated: 0,
            items_skipped: 0,
            items_failed: 0,
            errors: vec![],
            elapsed_ms: 0,
            skipped: true,
        }
    }

    /// Create a successful completion report with nacos-friendly fields.
    pub fn completed(
        source: impl Into<String>,
        namespaces: usize,
        configs: usize,
        services: usize,
        links_created: usize,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            source: source.into(),
            namespaces,
            configs,
            services,
            links_created,
            items_fetched: configs + services,
            items_created: configs + services,
            items_updated: 0,
            items_skipped: 0,
            items_failed: 0,
            errors: vec![],
            elapsed_ms,
            skipped: false,
        }
    }

    /// Returns `true` if no write errors occurred.
    pub fn is_success(&self) -> bool {
        self.items_failed == 0 && self.errors.is_empty()
    }

    /// Total items written (created + updated).
    pub fn items_written(&self) -> usize {
        self.items_created + self.items_updated
    }

    /// Total operations (for summary display).
    pub fn total_ops(&self) -> usize {
        self.configs + self.services + self.links_created
    }

    /// Append an error message to the report.
    pub fn add_error(&mut self, msg: impl Into<String>) {
        self.items_failed += 1;
        self.errors.push(msg.into());
    }
}

// ---------------------------------------------------------------------------
// SyncSource trait
// ---------------------------------------------------------------------------

/// A source of external system data that can be synchronised into the
/// knowledge graph.
#[async_trait]
pub trait SyncSource: Send + Sync {
    /// Human-readable name of this source (e.g. "nacos/config").
    fn name(&self) -> &str;

    /// Execute the synchronisation.
    async fn sync(&self, graph: &dyn GraphRepository) -> Result<SyncReport, DtError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_report_skipped() {
        let r = SyncReport::skipped("test-src");
        assert!(r.skipped);
        assert_eq!(r.source, "test-src");
        assert_eq!(r.configs, 0);
        assert_eq!(r.elapsed_ms, 0);
    }

    #[test]
    fn sync_report_completed() {
        let r = SyncReport::completed("api", 3, 42, 5, 10, 1234);
        assert!(!r.skipped);
        assert_eq!(r.namespaces, 3);
        assert_eq!(r.configs, 42);
        assert_eq!(r.services, 5);
        assert_eq!(r.links_created, 10);
        assert_eq!(r.elapsed_ms, 1234);
    }

    #[test]
    fn sync_report_total_ops() {
        let r = SyncReport::completed("api", 1, 10, 5, 3, 100);
        assert_eq!(r.total_ops(), 18);
    }
}
