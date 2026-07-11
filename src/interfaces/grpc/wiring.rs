//! Dependency injection (DI) assembly for the daemon.
//!
//! Creates and wires all service implementations together, wrapping them
//! with [`WriteCoordinator`] locks to prevent concurrent-write conflicts
//! across the three ingestion sources (OpenCode hooks, manual builds, cron
//! syncs).
//!
//! Backend connections (Neo4j, Qdrant) are created lazily at wire-time by
//! reading `config.yaml`.  If either backend is unreachable, the
//! corresponding field in [`AppComponents`] is set to `None` — callers
//! (e.g. the gRPC server) must fall back to no-op implementations.

use crate::domain::traits::{BuildService, EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::shared::coordinator::{CoordinatedBuildService, WriteCoordinator};
use crate::infrastructure::parser::ParserRegistry;
use crate::application::build::service::BuildServiceImpl;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// config.yaml layout (library-internal, mirrors main.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DaemonConfig {
    #[serde(default)]
    services: ServiceConfig,
}

#[derive(Debug, Deserialize, Default)]
struct ServiceConfig {
    #[serde(default)]
    neo4j: Neo4jConfig,
    #[serde(default)]
    qdrant: QdrantServiceConfig,
}

#[derive(Debug, Deserialize)]
struct Neo4jConfig {
    url: Option<String>,
    user: Option<String>,
    password: Option<String>,
}

impl Default for Neo4jConfig {
    fn default() -> Self {
        Self {
            url: Some("bolt://localhost:7687".to_string()),
            user: Some("neo4j".to_string()),
            password: Some("neo4j".to_string()),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct QdrantServiceConfig {
    #[serde(default)]
    url: Option<String>,
}

// ---------------------------------------------------------------------------
// AppComponents
// ---------------------------------------------------------------------------

/// All top-level application components assembled by the DI container.
///
/// Owns the `Arc<WriteCoordinator>` so that all services share the same
/// coordinator instance.
pub struct AppComponents {
    /// The coordinated build service (file/entity/global locks applied).
    pub build: Arc<dyn BuildService>,
    /// The shared write coordinator. Exposed for cron-like consumers that
    /// need to call `has_active_writes()` before starting a sync.
    pub coordinator: Arc<WriteCoordinator>,
    /// Neo4j graph repository (None if connection failed).
    pub graph: Option<Arc<dyn GraphRepository>>,
    /// Qdrant vector repository (None if connection failed).
    pub vector: Option<Arc<dyn VectorRepository>>,
}

// ---------------------------------------------------------------------------
// DI assembly
// ---------------------------------------------------------------------------

/// Assemble all application components.
///
/// Reads `config.yaml` (falling back through the usual search paths) and
/// attempts to connect to Neo4j and Qdrant.  If either backend is
/// unreachable the corresponding `AppComponents` field is left as `None`
/// so callers can fall back to no-op implementations.
pub async fn wire() -> AppComponents {
    // ---- Write coordinator ----
    // Use `with_global_lock()` so that full builds serialise against all
    // file-level and entity-level writers.
    let coordinator = Arc::new(WriteCoordinator::with_global_lock());

    // ---- Storage backends (real connections, fall back to None) ----
    let graph = connect_graph().await;
    let vector = connect_vector().await;

    // Snapshot and embed backends are optional; the pipeline adapts.
    let snapshot: Option<Arc<dyn SnapshotRepository>> = None;
    let embed: Option<Arc<dyn EmbedService>> = None;

    // ---- Parser registry (all language parsers) ----
    let parser_registry = Arc::new(ParserRegistry::new());

    // ---- Build service (inner, un-coordinated) ----
    let build_inner = Arc::new(BuildServiceImpl::new(
        parser_registry,
        graph.clone(),
        vector.clone(),
        snapshot,
        embed,
    ));

    // ---- Wrap with WriteCoordinator ----
    let build = Arc::new(CoordinatedBuildService::new(
        build_inner,
        Arc::clone(&coordinator),
    )) as Arc<dyn BuildService>;

    tracing::info!(
        "DI assembly complete: 1 service(s) wired (graph={}, vector={})",
        graph.is_some(),
        vector.is_some(),
    );

    AppComponents {
        build,
        coordinator,
        graph,
        vector,
    }
}

// ---------------------------------------------------------------------------
// config helpers (mirrors main.rs)
// ---------------------------------------------------------------------------

/// Attempt to load config.yaml from standard search paths.
fn load_config() -> Option<DaemonConfig> {
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("./config.yaml")];
    if let Some(p) = dirs_like_home_config(".config/opencode/skills/digital-twin/config.yaml") {
        candidates.push(p);
    }

    for path in &candidates {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_yaml::from_str::<DaemonConfig>(&content) {
                    Ok(cfg) => {
                        tracing::info!("loaded config from {}", path.display());
                        return Some(cfg);
                    }
                    Err(e) => {
                        tracing::warn!("failed to parse {}: {e}", path.display());
                    }
                },
                Err(e) => {
                    tracing::warn!("failed to read {}: {e}", path.display());
                }
            }
        }
    }
    None
}

/// Resolve `~/.config/...` without pulling in the `dirs` crate.
fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(suffix))
}

/// Resolve the Neo4j Bolt URI from config.yaml `services.neo4j`.
///
/// If `url` is set but uses HTTP scheme (e.g. `http://localhost:7474`),
/// converts it to Bolt (`bolt://localhost:7687`).  If no URL is configured,
/// returns the default `bolt://localhost:7687`.
fn resolve_neo4j_bolt_url(cfg: &Neo4jConfig) -> String {
    match &cfg.url {
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            if let Some(host) = url
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .split(':')
                .next()
            {
                format!("bolt://{}:7687", host)
            } else {
                "bolt://localhost:7687".to_string()
            }
        }
        Some(url) if url.starts_with("bolt://") => url.clone(),
        Some(url) => format!("bolt://{}:7687", url),
        None => "bolt://localhost:7687".to_string(),
    }
}

/// Connect to Neo4j using values from config.yaml (or sensible defaults).
async fn connect_graph() -> Option<Arc<dyn GraphRepository>> {
    let cfg = load_config()?;
    let bolt_url = resolve_neo4j_bolt_url(&cfg.services.neo4j);
    let user = cfg.services.neo4j.user.as_deref().unwrap_or("neo4j");
    let password = cfg.services.neo4j.password.as_deref().unwrap_or("neo4j");

    match crate::infrastructure::neo4j::Neo4jClient::connect(&bolt_url, user, password).await {
        Ok(client) => {
            tracing::info!("Neo4j connected: {}", bolt_url);
            Some(Arc::new(client) as Arc<dyn GraphRepository>)
        }
        Err(e) => {
            tracing::warn!("Neo4j connection failed (will use noop): {}", e);
            None
        }
    }
}

/// Connect to Qdrant vector store using config.yaml (or sensible defaults).
async fn connect_vector() -> Option<Arc<dyn VectorRepository>> {
    let cfg = load_config()?;
    let qdrant_uri = cfg
        .services
        .qdrant
        .url
        .as_deref()
        .unwrap_or("http://localhost:6334");

    match crate::infrastructure::qdrant::QdrantClient::connect(qdrant_uri).await {
        Ok(client) => {
            tracing::info!("Qdrant connected: {}", qdrant_uri);
            let repo = crate::infrastructure::qdrant::QdrantRepo::new(client);
            Some(Arc::new(repo) as Arc<dyn VectorRepository>)
        }
        Err(e) => {
            tracing::warn!("Qdrant connection failed (will use noop): {}", e);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wire_creates_components() {
        let components = wire().await;
        assert!(!components.coordinator.has_active_writes());
        // In CI / test environments without config.yaml, graph and vector
        // will be None — but the struct itself should always be built.
    }
}
