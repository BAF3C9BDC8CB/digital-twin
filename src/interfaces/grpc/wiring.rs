//! Dependency injection (DI) assembly for the daemon.
//!
//! Creates and wires all service implementations together, wrapping them
//! with [`WriteCoordinator`] locks to prevent concurrent-write conflicts
//! across the three ingestion sources (OpenCode hooks, manual builds, cron
//! syncs).
//!
//! Backend connections (Memgraph, Qdrant) are created lazily at wire-time by
//! reading `config.yaml`.  If either backend is unreachable, the
//! corresponding field in [`AppComponents`] is set to `None` — callers
//! (e.g. the gRPC server) must fall back to no-op implementations.
//! The SiliconFlow API client is also connected here; if unavailable it falls
//! back to [`NoopEmbedService`] (zero-vector embedding).

use crate::application::hooks::engine::HookEngine;
use crate::application::hooks::registry::HookRegistry;
use crate::domain::traits::{BuildService, EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::BatchConfig;
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
    #[serde(default, alias = "memgraph")]
    graph: GraphDbConfig,
    #[serde(default)]
    qdrant: QdrantServiceConfig,
    #[serde(default)]
    sqlite: SqliteConfig,
    #[serde(default)]
    embed_server: EmbedServerConfig,
}

#[derive(Debug, Deserialize)]
struct GraphDbConfig {
    url: Option<String>,
    user: Option<String>,
    password: Option<String>,
}

impl Default for GraphDbConfig {
    fn default() -> Self {
        Self {
            url: Some("bolt://localhost:7687".to_string()),
            user: Some("memgraph".to_string()),
            password: Some("".to_string()),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct QdrantServiceConfig {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SqliteConfig {
    #[serde(default = "default_sqlite_path")]
    path: String,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self { path: default_sqlite_path() }
    }
}

fn default_sqlite_path() -> String {
    "/var/lib/digital-twin/snapshots.db".to_string()
}

#[derive(Debug, Deserialize, Default)]
struct EmbedServerConfig {
    #[serde(default = "default_embed_url")]
    url: String,
}

fn default_embed_url() -> String {
    "http://[::1]:50052".to_string()
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
    /// Memgraph graph repository (None if connection failed).
    pub graph: Option<Arc<dyn GraphRepository>>,
    /// Qdrant vector repository (None if connection failed).
    pub vector: Option<Arc<dyn VectorRepository>>,
    /// SiliconFlow embed service (None if unavailable — callers fall back).
    pub embed: Option<Arc<dyn EmbedService>>,
    /// Hook engine for event-driven knowledge graph writes.
    /// None if event-hooks.yaml is missing or malformed.
    pub hook_engine: Option<Arc<HookEngine>>,
}

// ---------------------------------------------------------------------------
// DI assembly
// ---------------------------------------------------------------------------

/// Assemble all application components.
///
/// Reads `config.yaml` (falling back through the usual search paths) and
/// attempts to connect to Memgraph and Qdrant.  If either backend is
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
    let embed = connect_embed().await;

    // Snapshot backend is optional; the pipeline adapts.
    let snapshot: Option<Arc<dyn SnapshotRepository>> = None;

    // ---- Parser registry (all language parsers) ----
    let parser_registry = Arc::new(ParserRegistry::new());

    // ---- Build service (inner, un-coordinated) ----
    let batch_config = BatchConfig::default();
    let build_inner = Arc::new(BuildServiceImpl::new(
        parser_registry,
        graph.clone(),
        vector.clone(),
        snapshot,
        embed.clone(),
        None, // siliconflow — not wired through gRPC yet
        false, // gRPC builds default to incremental
        batch_config,
    ));

    // ---- Wrap with WriteCoordinator ----
    let build = Arc::new(CoordinatedBuildService::new(
        build_inner,
        Arc::clone(&coordinator),
    )) as Arc<dyn BuildService>;

    // ---- Hook engine (event-driven side effects) ----
    let hook_engine = graph.as_ref().and_then(|g| {
        let path = dirs_like_home_config(".config/digital-twin/event-hooks.yaml")?;
        match HookRegistry::from_file(&path) {
            Ok(registry) => {
                tracing::info!("HookRegistry loaded from {}", path.display());
                let registry = Arc::new(registry);
                Some(Arc::new(HookEngine::new(registry, g.clone())))
            }
            Err(e) => {
                tracing::warn!("failed to load {}: {e}", path.display());
                None
            }
        }
    });

    tracing::info!(
        "DI assembly complete: 1 service(s) wired (graph={}, vector={}, hooks={})",
        graph.is_some(),
        vector.is_some(),
        hook_engine.is_some(),
    );

    AppComponents {
        build,
        coordinator,
        graph,
        vector,
        embed,
        hook_engine,
    }
}

// ---------------------------------------------------------------------------
// config helpers (mirrors main.rs)
// ---------------------------------------------------------------------------

/// Load configuration from `~/.config/digital-twin/config.yaml`.
fn load_config() -> Option<DaemonConfig> {
    let path = dirs_like_home_config(".config/digital-twin/config.yaml")?;
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_yaml::from_str::<DaemonConfig>(&content) {
            Ok(cfg) => {
                tracing::info!("loaded config from {}", path.display());
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!("failed to parse {}: {e}", path.display());
                None
            }
        },
        Err(e) => {
            tracing::warn!("failed to read {}: {e}", path.display());
            None
        }
    }
}

/// Resolve `~/.config/...` without pulling in the `dirs` crate.
fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(suffix))
}

/// Resolve the Memgraph Bolt URI from config.yaml `services.graph`.
///
/// If `url` is set but uses HTTP scheme (e.g. `http://localhost:7474`),
/// converts it to Bolt (`bolt://localhost:7687`).  If no URL is configured,
/// returns the default `bolt://localhost:7687`.
fn resolve_graph_bolt_url(cfg: &GraphDbConfig) -> String {
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

/// Connect to Memgraph using values from config.yaml (or sensible defaults).
async fn connect_graph() -> Option<Arc<dyn GraphRepository>> {
    let cfg = load_config()?;
    let bolt_url = resolve_graph_bolt_url(&cfg.services.graph);
    let user = cfg.services.graph.user.as_deref().unwrap_or("memgraph");
    let password = cfg.services.graph.password.as_deref().unwrap_or("");

    match crate::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password).await {
        Ok(client) => {
            tracing::info!("Memgraph connected: {}", bolt_url);
            Some(Arc::new(client) as Arc<dyn GraphRepository>)
        }
        Err(e) => {
            tracing::warn!("Memgraph connection failed (will use noop): {}", e);
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

/// Connect to the embedding service using the provider router.
///
/// Reads config from env vars with sensible defaults, building a
/// [`EmbedProviderRouter`] that supports both SiliconFlow and XInference.
async fn connect_embed() -> Option<Arc<dyn EmbedService>> {
    use crate::infrastructure::embedder::ProviderConfig;

    let provider_cfg = ProviderConfig {
        siliconflow_url: std::env::var("SILICONFLOW_BASE_URL").unwrap_or_else(|_| "https://api.siliconflow.cn/v1".into()),
        siliconflow_api_key: std::env::var("SILICONFLOW_API_KEY").unwrap_or_default(),
        siliconflow_model_embed: std::env::var("SILICONFLOW_EMBED_MODEL").unwrap_or_else(|_| "BAAI/bge-m3".into()),
        siliconflow_model_reranker: std::env::var("SILICONFLOW_RERANKER_MODEL").unwrap_or_else(|_| "BAAI/bge-reranker-v2-m3".into()),
        siliconflow_model_llm: std::env::var("SILICONFLOW_LLM_MODEL").unwrap_or_default(),
        xinference_url: String::new(),
        xinference_api_key: String::new(),
        xinference_model_embed: String::new(),
        xinference_model_reranker: String::new(),
        xinference_model_llm: String::new(),
        embed_provider: "siliconflow".into(),
        rerank_provider: "siliconflow".into(),
        llm_provider: "siliconflow".into(),
    };

    let router = crate::infrastructure::embedder::create_embed_router(provider_cfg);
    tracing::info!("Embed provider router created (siliconflow, from env)");
    Some(router)
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
