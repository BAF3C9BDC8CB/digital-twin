//! Digital Twin V2 daemon — composition root and gRPC server.
//!
//! # Dual-mode
//!
//! The daemon binary operates in one of three modes:
//!
//! 1. **Server mode** (default) — starts the gRPC server.
//! 2. **CLI mode** — when invoked with a recognised subcommand (e.g. `build`,
//!    `search`), executes the command and exits.

use clap::{Parser, Subcommand};
use dt_daemon::domain::traits::{
    EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use dt_daemon::domain::types::{AppConfig, BatchConfig};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

// ---- CLI definition ----

#[derive(Parser)]
#[command(name = "dt-daemon", version = env!("CARGO_PKG_VERSION"), about = "Digital Twin daemon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Wipe all data from Memgraph, Qdrant, and SQLite.
    ///
    /// Requires `--confirm` to actually execute. Without it, prints a
    /// summary of what would be deleted and exits. Supports `--dry-run`
    /// for preview.
    Clean {
        /// Confirm the destructive operation.
        #[arg(long = "confirm")]
        confirm: bool,

        /// Preview only — show what would be deleted without executing.
        #[arg(long = "dry-run")]
        dry_run: bool,

        /// Specific targets to clean (comma-separated).
        /// Supported: "all" (default when no targets specified).
        #[arg(long = "targets", value_delimiter = ',')]
        targets: Vec<String>,

        /// Clean all test- prefixed data (nodes + Qdrant collections).
        /// Equivalent to the cleanup phase of `dt build --test`.
        #[arg(long = "test")]
        test: bool,
    },

    /// System backup — tiered backup of Memgraph, Qdrant, and SQLite.
    ///
    /// Default (no subcommand) creates a new backup.
    /// Subcommands: list, restore <date>, verify <date>.
    Backup {
        #[command(subcommand)]
        action: Option<BackupAction>,
    },

    /// Schema management commands.
    #[command(subcommand)]
    Schema(SchemaAction),

    /// Check health of all backend services (Memgraph, Qdrant, SQLite).
    Health,

    /// Write a knowledge entry (Knowledge, Experience, Concept, Domain, Playbook).
    ///
    /// Used by `dt memorize` and the MCP tool `dt_memorize` to persist
    /// structured knowledge into the Knowledge World subgraph.
    ///
    /// Usage: dt memorize <type> <entity-id> <details> [--project <name>]
    Memorize {
        /// Knowledge type: Decision | KnowledgeAdded | Environment | Dependencies.
        knowledge_type: String,

        /// Unique identifier for the entity.
        entity_id: String,

        /// Human-readable details in key: value format (semicolon-separated).
        details: String,

        /// Entity type label (e.g. ArchitectureDecision, Knowledge, Experience).
        #[arg(long = "entity-type")]
        entity_type: Option<String>,

        /// Optional project name for scoping.
        #[arg(long = "project")]
        project: Option<String>,
    },

    /// Fire a named hook with a JSON context object.
    ///
    /// Replaces the old `--type` / `--entity-id` / `--details` interface.
    /// The hook and its side-effect templates are configured in
    /// `config/event-hooks.yaml`.
    ///
    /// Usage: dt event <hook> '<json>'
    Event {
        /// Hook name (e.g. code_modified, jenkins_deploy_completed).
        hook_name: String,

        /// JSON object with fields for the hook's side-effect templates.
        context: String,
    },

    /// Learn from AI task execution — write structured knowledge into Knowledge World.
    ///
    /// Accepts task name, entities, patterns, pitfalls, decisions, and
    /// success/failure flags.  Synthesises Knowledge, Experience, and Playbook
    /// nodes and updates Playbook success/failure counters.
    ///
    /// Usage: dt learn <task> [--pattern ...] [--pitfalls ...] [--project ...]
    Learn {
        /// Task title or description (e.g. "支付平台迁移").
        task: String,

        /// Comma-separated list of affected entities (files, classes, services).
        #[arg(long = "entities", value_delimiter = ',')]
        entities: Vec<String>,

        /// Recognised solution pattern.
        #[arg(long = "pattern")]
        pattern: Option<String>,

        /// Comma-separated pitfalls encountered.
        #[arg(long = "pitfalls", value_delimiter = ',')]
        pitfalls: Vec<String>,

        /// Comma-separated architecture/technical decisions.
        #[arg(long = "decisions", value_delimiter = ',')]
        decisions: Vec<String>,

        /// Optional digital-thread ID for cross-task lineage.
        #[arg(long = "thread-id")]
        thread_id: Option<String>,

        /// Whether execution succeeded.
        #[arg(long = "success")]
        success: Option<bool>,

        /// Owning project name.
        #[arg(long = "project")]
        project: Option<String>,
    },

    /// Build (index) a project into the knowledge graph.
    ///
    /// `dt build` — build all projects from config.yaml (default).
    /// `dt build --path <path>` — build a project by root path.
    /// `dt build --name <name>` — build a project by name in config.yaml.
    /// `dt build --file <file>` — single file incremental update.
    /// `dt build --full` — full rebuild (can combine with --path/--name/--file/--test).
    /// `dt build --test` — run self-contained pipeline integration test.
    Build {
        /// Project root path.
        #[arg(long = "path")]
        path: Option<PathBuf>,

        /// Project name (from config.yaml).
        #[arg(long = "name", short = 'n')]
        name: Option<String>,

        /// Single file path (for incremental single-file update).
        #[arg(long = "file")]
        file: Option<PathBuf>,

        /// Full rebuild — bypass incremental snapshots.
        #[arg(long = "full")]
        full: bool,

        /// Skip pipeline analysis after build (enabled by default).
        #[arg(long = "no-pipeline")]
        no_pipeline: bool,

        /// Run the self-contained pipeline integration test.
        ///
        /// Creates test- prefixed nodes and collections, verifies
        /// every entity type, then cleans up automatically.
        /// Combine with --full to force a full rebuild when incremental
        /// progress is stale; use `dt clean --test` to manually clean test data.
        #[arg(long = "test")]
        test: bool,

        /// Source type to build: code (default), knowledge (sync KG nodes to vectors).
        /// Use "knowledge" as a replacement for `dt kg-sync`.
        #[arg(long = "source")]
        source: Option<String>,
    },

    /// Unified search across worlds.
    ///
    /// Usage: dt search <query> [--world all|code|knowledge|doc|config|memory] [--limit 10] [--json]
    Search {
        /// Search query string (positional).
        query: String,

        /// Which world to search: all, code, knowledge, doc, config, memory.
        #[arg(long = "world", default_value = "all")]
        world: String,

        /// Limit results.
        #[arg(long = "limit", default_value = "10")]
        limit: usize,

        /// Output pure JSON to stdout (for MCP / scripting).
        #[arg(long = "json")]
        json: bool,

        /// Scope to a project name.
        #[arg(long = "project", short = 'p')]
        project: Option<String>,
    },

    /// Synchronize Nacos configuration to Knowledge Graph.
    ///
    /// Usage: dt nacos-sync [test|prod]
    NacosSync {
        /// Target environment (default: test).
        #[arg(default_value = "test")]
        env: String,
    },

    /// Synchronize Kubernetes resources to Knowledge Graph.
    K8sSync {
        /// Dry-run mode.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },

    /// Synchronize KG nodes to Qdrant vector store.
    ///
    /// Default: incremental (only new/unsynchronized nodes).
    /// Use --full for complete rebuild.
    KgSync {
        /// Full rebuild — sync all nodes (bypass incremental).
        #[arg(long = "full")]
        full: bool,

        /// Specific labels (comma-separated).
        #[arg(long = "labels")]
        labels: Option<String>,

        /// Sync adaptive config chunks to Qdrant config_chunks collection.
        #[arg(long = "config-chunks")]
        config_chunks: bool,
    },

    /// Kubernetes operations: pods, logs, download, status (via kublog).
    Kub {
        /// Action: pods, logs, download, status.
        action: String,

        /// K8s namespace.
        #[arg(long = "ns", default_value = "default")]
        namespace: String,

        /// Pod name (for logs / download).
        #[arg(long = "pod")]
        pod: Option<String>,

        /// Log time window (e.g. "1h", "30m").
        #[arg(long = "since")]
        since: Option<String>,

        /// Output file path (for download).
        #[arg(short = 'o', long = "output")]
        output: Option<String>,

        /// Resource type for status (pods, deploy, svc).
        #[arg(long = "resource", default_value = "pods")]
        resource: String,
    },

    /// Jenkins CI/CD operations (via jcli).
    Jcli {
        /// Action: list, params, history, log, build.
        action: String,

        /// Job name.
        #[arg(long = "job", short = 'j')]
        job: Option<String>,

        /// Build number (for log).
        #[arg(long = "build")]
        build: Option<String>,

        /// Max results (for history).
        #[arg(long = "limit")]
        limit: Option<u32>,

        /// Build parameters: KEY=VALUE,... (for build).
        #[arg(long = "params")]
        params: Option<String>,

        /// Environment: test (default) or production.
        #[arg(long = "env", default_value = "test")]
        env: String,
    },

    /// Synchronize Jenkins Views, Jobs, and Builds to Knowledge Graph.
    JcSync {
        /// Specific job name to sync. Default: sync all jobs.
        #[arg(long = "job")]
        job: Option<String>,
    },

    /// Start the gRPC daemon server or show status.
    Daemon {
        /// Action: start (launch gRPC server) or status (health check).
        #[arg(default_value = "start")]
        action: String,
    },
}

#[derive(Subcommand)]
enum SchemaAction {
    /// Initialize V2 schema — create all uniqueness constraints and indexes.
    Init,
}

#[derive(Subcommand)]
enum BackupAction {
    /// Create a new backup (default).
    Create,
    /// List available backups.
    List,
    /// Restore from a backup by date (YYYY-MM-DD).
    Restore {
        /// Backup date (format: YYYY-MM-DD).
        date: String,
    },
    /// Verify backup integrity by date (YYYY-MM-DD).
    Verify {
        /// Backup date (format: YYYY-MM-DD).
        date: String,
    },
}

// ---- Config loading ----

/// Minimal YAML subset needed to extract project paths from config.yaml.
#[derive(Debug, Deserialize)]
struct DaemonConfig {
    #[serde(default)]
    projects: Vec<ProjectGroup>,
    #[serde(default)]
    services: ServiceConfig,
    #[serde(default)]
    batch: BatchConfig,
}

#[derive(Debug, Deserialize, Default)]
struct ServiceConfig {
    #[serde(default, alias = "memgraph")]
    graph: GraphDbConfig,
    #[serde(default)]
    qdrant: QdrantServiceConfig,
    #[serde(default)]
    nacos: NacosUrls,
    #[serde(default)]
    k8s: K8sEndpointConfig,
    #[serde(default)]
    jenkins: JenkinsEndpointConfig,
    #[serde(default)]
    sqlite: SqliteConfig,
    #[serde(default)]
    hanlp: HanlpConfig,
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

/// Nacos environment URLs from config.yaml `services.nacos`.
#[derive(Debug, Deserialize, Default)]
struct NacosUrls {
    #[serde(default)]
    test: Option<String>,
    #[serde(default)]
    prod: Option<String>,
}

/// K8s/Kuboard connection details from config.yaml `services.k8s`.
#[derive(Debug, Deserialize, Default)]
struct K8sEndpointConfig {
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    cluster_id: Option<String>,
    #[serde(default)]
    skip_tls_verify: Option<bool>,
}

/// Jenkins connection details from config.yaml `services.jenkins`.
#[derive(Debug, Deserialize, Default)]
struct JenkinsEndpointConfig {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

/// SQLite snapshot store configuration from config.yaml `services.sqlite`.
#[derive(Debug, Deserialize)]
struct SqliteConfig {
    /// Path to the SQLite snapshot database file.
    #[serde(default = "default_sqlite_path")]
    path: String,
}

impl Default for SqliteConfig {
    fn default() -> Self {
        Self {
            path: default_sqlite_path(),
        }
    }
}

fn default_sqlite_path() -> String {
    "/var/lib/digital-twin/snapshots.db".to_string()
}

/// HanLP service configuration from config.yaml `services.hanlp`.
#[derive(Debug, Deserialize, Default)]
struct HanlpConfig {
    /// Base URL (e.g. http://localhost:8765).
    #[serde(default)]
    url: String,
    /// API key (optional, typically empty for local).
    #[serde(default)]
    api_key: String,
}

#[derive(Debug, Deserialize)]
struct ProjectGroup {
    base: String,
    #[serde(default)]
    items: Vec<serde_yaml::Value>,
}

/// Load configuration from `~/.config/digital-twin/config.yaml`.
fn load_config() -> Option<DaemonConfig> {
    let path = dirs_like_home_config(".config/digital-twin/config.yaml")?;
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_yaml::from_str::<DaemonConfig>(&content) {
            Ok(cfg) => {
                tracing::info!("已加载配置: {}", path.display());
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!("解析配置失败 {}: {e}", path.display());
                None
            }
        },
        Err(e) => {
            tracing::warn!("读取配置文件失败 {}: {e}", path.display());
            None
        }
    }
}

/// Resolve `~/.config/...` without pulling in the `dirs` crate.
fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(suffix))
}

/// Flatten project groups into `(name, full_path)` pairs.
fn resolve_project_paths(cfg: &DaemonConfig) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for group in &cfg.projects {
        let base = PathBuf::from(&group.base);
        for item in &group.items {
            match item {
                serde_yaml::Value::String(name) => {
                    out.push((name.clone(), base.join(name)));
                }
                serde_yaml::Value::Mapping(m) => {
                    for (k, v) in m {
                        let name = k.as_str().unwrap_or("").to_string();
                        let rel = v.as_str().unwrap_or(&name).to_string();
                        out.push((name, base.join(rel)));
                    }
                }
                _ => {
                    // Skip unrecognised item shapes.
                }
            }
        }
    }
    out
}

/// Resolve the Memgraph Bolt URI from config.yaml `services.graph`.
///
/// If `url` is set but uses HTTP scheme (e.g. `http://localhost:7474`),
/// converts it to Bolt (`bolt://localhost:7687`).  If no URL is configured,
/// returns the default `bolt://localhost:7687`.
fn resolve_graph_bolt_url(cfg: &GraphDbConfig) -> String {
    match &cfg.url {
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            // Extract host from HTTP URL, use default Bolt port
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
/// Returns an `Arc<dyn GraphRepository>` ready for use by services.
async fn connect_graph() -> Option<Arc<dyn GraphRepository>> {
    let cfg = load_config()?;
    let bolt_url = resolve_graph_bolt_url(&cfg.services.graph);
    let user = cfg.services.graph.user.as_deref().unwrap_or("memgraph");
    let password = cfg.services.graph.password.as_deref().unwrap_or("");

    match dt_daemon::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password)
        .await
    {
        Ok(client) => {
            tracing::info!("Memgraph 已连接: {}", bolt_url);
            Some(Arc::new(client) as Arc<dyn GraphRepository>)
        }
        Err(e) => {
            tracing::warn!("Memgraph 连接失败 (将使用 noop): {}", e);
            None
        }
    }
}

/// Build a HookEngine from `~/.config/digital-twin/event-hooks.yaml`.
/// Returns `None` if Memgraph is unavailable or the config file is missing.
async fn connect_hook_engine() -> Option<Arc<dt_daemon::application::hooks::HookEngine>> {
    let graph = connect_graph().await?;
    let path = dirs_like_home_config(".config/digital-twin/event-hooks.yaml")?;
    match dt_daemon::application::hooks::HookRegistry::from_file(&path) {
        Ok(registry) => {
            tracing::info!("HookRegistry 已加载: {}", path.display());
            Some(Arc::new(dt_daemon::application::hooks::HookEngine::new(
                Arc::new(registry),
                graph,
            )))
        }
        Err(e) => {
            tracing::warn!("加载 HookRegistry 失败 {}: {e}", path.display());
            None
        }
    }
}

/// Connect to Memgraph using values from config.yaml (or sensible defaults).
async fn connect_memgraph() -> Option<dt_daemon::infrastructure::memgraph::MemgraphClient> {
    let cfg = load_config()?;
    let bolt_url = resolve_graph_bolt_url(&cfg.services.graph);
    let user = cfg.services.graph.user.as_deref().unwrap_or("memgraph");
    let password = cfg.services.graph.password.as_deref().unwrap_or("");

    match dt_daemon::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password)
        .await
    {
        Ok(client) => {
            tracing::info!("Memgraph 已连接: {}", bolt_url);
            Some(client)
        }
        Err(e) => {
            tracing::warn!("Memgraph 连接失败 (将使用 noop): {}", e);
            None
        }
    }
}

/// Connect to Qdrant vector store using config.yaml (or sensible defaults).
/// Returns an `Arc<dyn VectorRepository>` ready for use by services.
async fn connect_vector() -> Option<Arc<dyn dt_daemon::domain::traits::VectorRepository>> {
    let cfg = load_config()?;
    let qdrant_uri = cfg
        .services
        .qdrant
        .url
        .as_deref()
        .unwrap_or("http://localhost:6334");

    match dt_daemon::infrastructure::qdrant::QdrantClient::connect(qdrant_uri).await {
        Ok(client) => {
            tracing::info!("Qdrant 已连接: {}", qdrant_uri);
            let repo = dt_daemon::infrastructure::qdrant::QdrantRepo::new(client);
            Some(Arc::new(repo) as Arc<dyn dt_daemon::domain::traits::VectorRepository>)
        }
        Err(e) => {
            tracing::warn!("Qdrant 连接失败 (将使用 noop): {}", e);
            None
        }
    }
}

/// Connect to the embedding service using the provider router.
///
/// Reads provider config exclusively from config/pipeline.yaml (PipelineConfig).
/// This function is the single source of truth for embed service creation.
async fn connect_embed() -> Option<Arc<dyn dt_daemon::domain::traits::EmbedService>> {
    use dt_daemon::application::pipeline::config::PipelineConfig;

    let pipeline_cfg = PipelineConfig::load().ok()?;
    let pcfg = pipeline_cfg.providers?;

    let sf = pcfg.siliconflow.as_ref();
    let xi = pcfg.xinference.as_ref();

    // At least one provider must have a non-empty URL
    let sf_url = sf.map(|s| s.url.as_str()).unwrap_or("");
    let xi_url = xi.map(|s| s.url.as_str()).unwrap_or("");
    if sf_url.is_empty() && xi_url.is_empty() {
        tracing::warn!("pipeline.yaml providers: 所有 provider URL 为空，跳过 embed 服务");
        return None;
    }

    let api_key_fallback = || std::env::var("SILICONFLOW_API_KEY").unwrap_or_default();

    let cfg = dt_daemon::infrastructure::embedder::ProviderConfig {
        siliconflow_url: sf_url.to_string(),
        siliconflow_api_key: sf
            .and_then(|s| {
                if s.api_key.is_empty() {
                    None
                } else {
                    Some(s.api_key.clone())
                }
            })
            .unwrap_or_else(api_key_fallback),
        siliconflow_model_embed: sf.map(|s| s.model_embed.clone()).unwrap_or_default(),
        siliconflow_model_reranker: sf.map(|s| s.model_reranker.clone()).unwrap_or_default(),
        siliconflow_model_llm: sf.map(|s| s.model_llm.clone()).unwrap_or_default(),
        xinference_url: xi_url.to_string(),
        xinference_api_key: xi.map(|s| s.api_key.clone()).unwrap_or_default(),
        xinference_model_embed: xi.map(|s| s.model_embed.clone()).unwrap_or_default(),
        xinference_model_reranker: xi.map(|s| s.model_reranker.clone()).unwrap_or_default(),
        xinference_model_llm: xi.map(|s| s.model_llm.clone()).unwrap_or_default(),
        embed_provider: pcfg.embed_provider.clone(),
        rerank_provider: pcfg.rerank_provider.clone(),
        llm_provider: pcfg.llm_provider.clone(),
    };
    Some(dt_daemon::infrastructure::embedder::create_embed_router(
        cfg,
    ))
}

/// Connect to the HanLP local NLP service from config.yaml.
async fn connect_hanlp() -> Option<Arc<dt_daemon::infrastructure::hanlp::HanlpClient>> {
    let cfg = load_config()?;
    let url = cfg.services.hanlp.url.clone();
    let api_key = cfg.services.hanlp.api_key.clone();
    if url.is_empty() {
        tracing::info!("HanLP 未配置 — 跳过");
        return None;
    }
    let client = Arc::new(dt_daemon::infrastructure::hanlp::HanlpClient::new(
        url, api_key,
    ));
    tracing::info!("HanLP 客户端已创建");
    Some(client)
}

/// Build an optional KgBridge for auto-syncing nodes to Qdrant after writes.
///
/// Requires both `graph` and `vector`; `queue` provides priority-aware embedding.
async fn build_kg_bridge(
    graph: Option<Arc<dyn dt_daemon::domain::traits::GraphRepository>>,
    vector: Option<Arc<dyn dt_daemon::domain::traits::VectorRepository>>,
    queue: Option<Arc<dt_daemon::application::sync::queue::VectorQueue>>,
) -> Option<Arc<dt_daemon::application::sync::kg_bridge::KgBridge>> {
    let g = graph?;
    let embed = queue.as_ref()?.embed_service().clone();
    let v = vector.unwrap_or_else(|| {
        Arc::new(dt_daemon::infrastructure::qdrant::repo::NoopVectorRepo)
            as Arc<dyn dt_daemon::domain::traits::VectorRepository>
    });
    let bridge = dt_daemon::application::sync::kg_bridge::KgBridge::new(g, embed, v);
    Some(Arc::new(bridge.with_queue(queue?)))
}

/// Build an optional SyncAccumulator for batch-accumulating background sync.
async fn build_sync_acc(
    graph: Option<Arc<dyn dt_daemon::domain::traits::GraphRepository>>,
    vector: Option<Arc<dyn dt_daemon::domain::traits::VectorRepository>>,
    queue: Option<Arc<dt_daemon::application::sync::queue::VectorQueue>>,
) -> Option<Arc<dt_daemon::application::sync::batch::SyncAccumulator>> {
    let bridge = build_kg_bridge(graph, vector, queue.clone()).await?;
    Some(Arc::new(
        dt_daemon::application::sync::batch::SyncAccumulator::spawn(bridge, queue?),
    ))
}

/// Connect to the SQLite snapshot store (falls back to None if unavailable).
async fn connect_snapshot() -> Option<Arc<dyn dt_daemon::domain::traits::SnapshotRepository>> {
    let db_path = load_config()
        .map(|c| c.services.sqlite.path.clone())
        .unwrap_or_else(default_sqlite_path);

    match dt_daemon::infrastructure::sqlite::SqliteRepo::open(&db_path) {
        Ok(repo) => {
            tracing::info!("SQLite 快照存储已连接: {db_path}");
            Some(Arc::new(repo) as Arc<dyn dt_daemon::domain::traits::SnapshotRepository>)
        }
        Err(e) => {
            tracing::warn!("SQLite 快照存储不可用: {e} — 增量构建已禁用");
            None
        }
    }
}

/// Build a resolved K8sSyncConfig from config.yaml services.k8s.
fn resolve_k8s_config(
    config: &Option<DaemonConfig>,
) -> Option<dt_daemon::application::sync::k8s::K8sSyncConfig> {
    config.as_ref().and_then(|c| {
        let k8s = &c.services.k8s;
        let server = k8s.server.as_deref().unwrap_or("");
        if server.is_empty() {
            return None;
        }
        Some(dt_daemon::application::sync::k8s::K8sSyncConfig {
            server: server.to_string(),
            username: k8s.username.clone().unwrap_or_default(),
            password: k8s.password.clone().unwrap_or_default(),
            cluster_id: k8s.cluster_id.clone().unwrap_or_default(),
            skip_tls_verify: k8s.skip_tls_verify.unwrap_or(true),
            namespaces: vec![],
        })
    })
}

// ---- Main ----

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize unified logging via dt-log (JSON → file + stderr fallback)
    dt_daemon::shared::logging::init::init_logging()?;

    let cli = Cli::parse();

    match cli.command {
        // ---- CLI mode: dt clean ----
        Some(Commands::Clean {
            confirm,
            dry_run: _,
            targets: _,
            test,
        }) => {
            // Handle --test: clean test- prefixed data (fail-fast, no Noop fallback)
            if test {
                // a. Connect to real Memgraph — fail fast if unavailable
                let graph: Arc<dyn GraphRepository> = match connect_memgraph().await {
                    Some(c) => Arc::new(c) as Arc<dyn GraphRepository>,
                    None => {
                        eprintln!(
                            "error: Memgraph unavailable — clean --test requires real backends"
                        );
                        std::process::exit(1);
                    }
                };

                // b. Connect to real Qdrant — fail fast if unavailable
                let vector: Arc<dyn VectorRepository> = match connect_vector().await {
                    Some(c) => c,
                    None => {
                        eprintln!(
                            "error: Qdrant unavailable — clean --test requires real backends"
                        );
                        std::process::exit(1);
                    }
                };

                // c. Connect to SQLite for snapshot cleanup (optional, non-fatal)
                let snapshot = connect_snapshot().await;

                let deleted = dt_daemon::application::pipeline::test::cleanup::cleanup_test_data(
                    &graph,
                    &vector,
                    snapshot.as_ref(),
                )
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
                println!("Cleaned {} test- nodes and collections", deleted);
                return Ok(());
            }

            let memgraph = connect_memgraph().await;
            dt_daemon::interfaces::cli::cleanup::run_clean(
                confirm,
                memgraph
                    .as_ref()
                    .map(|c| c as &dyn dt_daemon::domain::traits::GraphRepository),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt backup ----
        Some(Commands::Backup { action }) => {
            match action.unwrap_or(BackupAction::Create) {
                BackupAction::Create => {
                    println!("=== dt backup ===");
                    let report = dt_daemon::interfaces::cli::backup::create_backup().await?;
                    println!();
                    println!("Backup created:");
                    println!("  Location:   {}", report.location.display());
                    println!(
                        "  Memgraph:   {} ({} bytes)",
                        if report.targets.memgraph {
                            "✓"
                        } else {
                            "✗"
                        },
                        report.targets.memgraph_size_bytes,
                    );
                    println!(
                        "  Qdrant:     {} ({} bytes)",
                        if report.targets.qdrant { "✓" } else { "✗" },
                        report.targets.qdrant_size_bytes,
                    );
                    println!(
                        "  SQLite:     {} ({} bytes)",
                        if report.targets.sqlite { "✓" } else { "✗" },
                        report.targets.sqlite_size_bytes,
                    );
                    println!("  Duration:   {:.1}s", report.duration_seconds,);
                }
                BackupAction::List => {
                    println!("=== dt backup list ===");
                    let entries = dt_daemon::interfaces::cli::backup::list_backups().await?;

                    if entries.is_empty() {
                        println!("No backups found.");
                    } else {
                        println!(" {:<12}  {:<10}  {:>8}", "DATE", "SIZE", "FILES");
                        println!(
                            " {:<12}  {:<10}  {:>8}",
                            "------------", "----------", "--------"
                        );
                        for entry in &entries {
                            println!(
                                " {:<12}  {:>8} B  {:>8}",
                                entry.date, entry.total_size_bytes, entry.file_count,
                            );
                        }
                        println!();
                        println!("Total: {} backup(s)", entries.len());
                    }
                }
                BackupAction::Restore { date } => {
                    println!("=== dt backup restore {date} ===");
                    dt_daemon::interfaces::cli::backup::restore_backup(&date).await?;
                    println!("Restore complete.");
                }
                BackupAction::Verify { date } => {
                    println!("=== dt backup verify {date} ===");
                    let report =
                        dt_daemon::interfaces::cli::backup::verify_backup_files(&date).await?;

                    println!();
                    if report.all_valid {
                        println!("✅ All checksums valid!");
                    } else {
                        println!("❌ Checksum mismatch detected:");
                    }

                    for file in &report.files {
                        let status = if file.valid { "✅" } else { "❌" };
                        println!(
                            "  {} {} — expected: {}",
                            status,
                            file.file_name,
                            &file.expected[..32.min(file.expected.len())],
                        );
                    }
                    println!("  Duration: {:.1}s", report.duration_seconds,);
                }
            }

            return Ok(());
        }

        // ---- CLI mode: dt schema init ----
        Some(Commands::Schema(SchemaAction::Init)) => {
            let memgraph = connect_memgraph().await;
            dt_daemon::interfaces::cli::cleanup::run_schema_init(
                memgraph
                    .as_ref()
                    .map(|c| c as &dyn dt_daemon::domain::traits::GraphRepository),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt health ----
        Some(Commands::Health) => {
            let memgraph = connect_memgraph().await;
            let qdrant = connect_vector().await;
            let snapshot = connect_snapshot().await;
            let embed = connect_embed().await;
            dt_daemon::interfaces::cli::cleanup::run_health(
                memgraph.as_ref().map(|c| c as &dyn GraphRepository),
                qdrant.as_deref().map(|c| c as &dyn VectorRepository),
                snapshot.as_deref().map(|c| c as &dyn SnapshotRepository),
                embed.as_deref().map(|c| c as &dyn EmbedService),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt memorize ----
        Some(Commands::Memorize {
            knowledge_type,
            entity_id,
            details,
            entity_type,
            project,
        }) => {
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let vector = connect_vector().await;
            let queue =
                embed.map(|e| Arc::new(dt_daemon::application::sync::queue::VectorQueue::spawn(e)));
            let sync_acc = build_sync_acc(graph.clone(), vector, queue).await;
            dt_daemon::interfaces::cli::memorize::handle_memorize(
                knowledge_type,
                entity_id,
                entity_type,
                project,
                details,
                graph,
                sync_acc,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt event ----
        Some(Commands::Event { hook_name, context }) => {
            let hook_engine = connect_hook_engine().await;
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let vector = connect_vector().await;
            let queue =
                embed.map(|e| Arc::new(dt_daemon::application::sync::queue::VectorQueue::spawn(e)));
            let kg_bridge = build_kg_bridge(graph, vector, queue).await;
            dt_daemon::interfaces::cli::event::handle_event(
                hook_name,
                context,
                hook_engine,
                kg_bridge,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt learn ----
        Some(Commands::Learn {
            task,
            entities,
            pattern,
            pitfalls,
            decisions,
            thread_id,
            success,
            project,
        }) => {
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let vector = connect_vector().await;
            let queue =
                embed.map(|e| Arc::new(dt_daemon::application::sync::queue::VectorQueue::spawn(e)));
            let sync_acc = build_sync_acc(graph.clone(), vector, queue).await;
            dt_daemon::interfaces::cli::learn::handle_learn(
                task, entities, pattern, pitfalls, decisions, thread_id, success, project, graph,
                sync_acc,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt build ----
        Some(Commands::Build {
            path,
            name,
            file,
            full,
            no_pipeline,
            test,
            source,
        }) => {
            // ── dt build --test: run self-contained pipeline integration test ──
            if test {
                tracing::info!("dt build --test: 启动流水线集成测试");

                // a. Connect to real Memgraph — fail fast if unavailable
                let graph: Arc<dyn GraphRepository> = match connect_memgraph().await {
                    Some(c) => Arc::new(c) as Arc<dyn GraphRepository>,
                    None => {
                        eprintln!(
                            "error: Memgraph unavailable — build --test requires real backends"
                        );
                        std::process::exit(1);
                    }
                };

                // b. Connect to real Qdrant — fail fast if unavailable
                let vector: Arc<dyn VectorRepository> = match connect_vector().await {
                    Some(c) => c,
                    None => {
                        eprintln!(
                            "error: Qdrant unavailable — build --test requires real backends"
                        );
                        std::process::exit(1);
                    }
                };

                // c. Connect to SiliconFlow (fallback to Noop if unavailable — embed quality doesn't affect test validity)
                let embed: Arc<dyn EmbedService> = connect_embed().await.unwrap_or_else(|| {
                    tracing::warn!("SiliconFlow 不可用，使用 NoopEmbedService");
                    Arc::new(dt_daemon::infrastructure::embedder::NoopEmbedService::default())
                        as Arc<dyn EmbedService>
                });

                // d. Connect to real SQLite snapshot store — fail fast if unavailable
                let snapshot: Arc<dyn SnapshotRepository> = match connect_snapshot().await {
                    Some(c) => c,
                    None => {
                        eprintln!("error: SQLite snapshot store unavailable — build --test requires real backends");
                        std::process::exit(1);
                    }
                };

                // e. Run build (incremental by default — first run detects no snapshots
                //    and processes all files; subsequent runs skip unchanged files).
                //    full: user-passable via `dt build --test --full` — forces a full
                //    rebuild and bypasses incremental snapshots (use when SQLite progress
                //    is stale, e.g. after the KG was wiped outside `dt clean --test`).
                //    pipeline=true: post-build pipeline ENABLED — same code path as production build,
                //    including LLM background analysis (Phase 2). This ensures --test exercises the
                //    exact same pipeline as real builds. LLM runs in background (non-blocking).
                //    `dt clean --test` remains available to wipe test data manually.
                dt_daemon::interfaces::cli::build::handle_build(
                    PathBuf::from("/data/myProject/digital-twin-v2/test"),
                    Some("test-pipeline".to_string()),
                    None, // file
                    full, // full: ②a fix — pass through user flag (was hardcoded false)
                    true, // pipeline: ENABLED — same code path as production build (Phase 4 change)
                    Some(graph.clone()),
                    Some(vector.clone()),
                    Some(embed.clone()),
                    Some(snapshot.clone()),
                    BatchConfig::default(),
                    connect_hanlp().await,
                )
                .await?;

                // h. Verify test data
                let report =
                    dt_daemon::application::pipeline::test::runner::verify_test_data(graph, vector)
                        .await;

                // i. Print the test report
                report.print();

                // j. Exit with failure code if any checks failed
                if report.failed > 0 {
                    std::process::exit(1);
                }
                return Ok(());
            }

            // ── dt build --source knowledge: replace dt kg-sync ──
            if let Some(ref src) = source {
                if src == "knowledge" {
                    tracing::info!("dt build --source knowledge: 同步 KG 节点到向量库");
                    let graph = connect_graph().await;
                    let embed = connect_embed().await;
                    let vector = connect_vector().await;

                    if graph.is_none() || embed.is_none() || vector.is_none() {
                        eprintln!("error: build --source knowledge requires Memgraph + Qdrant + embed backends");
                        std::process::exit(1);
                    }

                    let graph = graph.unwrap();
                    let embed = embed.unwrap();
                    let vector = vector.unwrap();

                    let queue = Arc::new(dt_daemon::application::sync::queue::VectorQueue::spawn(
                        embed.clone(),
                    ));

                    let incremental = !full;
                    dt_daemon::interfaces::cli::sync::handle_kg_sync(
                        incremental,
                        None,
                        false,
                        Some(graph),
                        Some(queue),
                    )
                    .await?;
                    return Ok(());
                } else {
                    eprintln!("error: unknown source type '{src}'. Supported: knowledge");
                    std::process::exit(1);
                }
            }

            // No args at all → build all projects from config.yaml
            if path.is_none() && name.is_none() && file.is_none() {
                let memgraph = connect_memgraph().await;
                let graph: Option<Arc<dyn GraphRepository>> =
                    memgraph.map(|c| Arc::new(c) as Arc<dyn GraphRepository>);

                let embed = connect_embed().await;
                let vector = connect_vector().await;
                let snapshot = connect_snapshot().await;

                let Some(cfg) = load_config() else {
                    eprintln!("error: config.yaml not found");
                    std::process::exit(1);
                };

                let projects = resolve_project_paths(&cfg);
                if projects.is_empty() {
                    eprintln!("error: no projects configured in config.yaml");
                    std::process::exit(1);
                }

                let batch_config = cfg.batch.clone();
                let pipeline = !no_pipeline;
                dt_daemon::interfaces::cli::build::handle_build_all(
                    projects,
                    full,
                    pipeline,
                    graph,
                    vector,
                    embed,
                    snapshot,
                    batch_config,
                )
                .await?;
                return Ok(());
            }

            let memgraph = connect_memgraph().await;
            let graph: Option<Arc<dyn GraphRepository>> =
                memgraph.map(|c| Arc::new(c) as Arc<dyn GraphRepository>);

            let embed = connect_embed().await;
            let vector = connect_vector().await;
            let snapshot = connect_snapshot().await;

            // When --name is given, resolve actual path from config.yaml.
            // e.g. --name order-center → /data/aflmProjects/aflm/uvp-order-center
            let actual_path = if let Some(ref n) = name {
                let cfg = load_config();
                cfg.as_ref()
                    .and_then(|c| {
                        resolve_project_paths(c)
                            .into_iter()
                            .find(|(proj_name, _)| proj_name == n)
                            .map(|(_, proj_path)| proj_path)
                    })
                    .unwrap_or_else(|| path.expect("--path is required"))
            } else {
                path.expect("--path is required")
            };

            let batch_config = load_config().map(|c| c.batch).unwrap_or_default();

            let pipeline = !no_pipeline;
            dt_daemon::interfaces::cli::build::handle_build(
                actual_path,
                name,
                file,
                full,
                pipeline,
                graph,
                vector,
                embed,
                snapshot,
                batch_config,
                connect_hanlp().await,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt search ----
        Some(Commands::Search {
            query,
            world,
            limit,
            json,
            project,
        }) => {
            let graph = connect_graph().await;
            let vector = connect_vector().await;
            dt_daemon::interfaces::cli::build::handle_search(
                query, world, limit, json, project, graph, vector,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt nacos-sync ----
        Some(Commands::NacosSync { env }) => {
            let config = load_config();
            let nacos_url = match env.as_str() {
                "test" => config
                    .as_ref()
                    .and_then(|c| c.services.nacos.test.as_deref())
                    .unwrap_or("https://nacos.newoffen.net/nacos"),
                "prod" => config
                    .as_ref()
                    .and_then(|c| c.services.nacos.prod.as_deref())
                    .unwrap_or("https://nacos.newoffen.com/nacos"),
                _ => anyhow::bail!("unknown env: {env}, expected test or prod"),
            };

            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::sync::handle_nacos_sync(env, graph, nacos_url).await?;
            return Ok(());
        }

        // ---- CLI mode: dt k8s-sync ----
        Some(Commands::K8sSync { dry_run }) => {
            let config = load_config();
            let k8s_cfg = resolve_k8s_config(&config);

            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::sync::handle_k8s_sync(dry_run, graph, k8s_cfg).await?;
            return Ok(());
        }

        // ---- CLI mode: dt kg-sync ----
        Some(Commands::KgSync {
            full,
            labels,
            config_chunks,
        }) => {
            eprintln!("⚠️  Deprecated: `dt kg-sync` is deprecated. Use `dt build --source knowledge` instead.");
            eprintln!("   The command still works but will be removed in a future release.");
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let queue =
                embed.map(|e| Arc::new(dt_daemon::application::sync::queue::VectorQueue::spawn(e)));
            let incremental = !full;
            dt_daemon::interfaces::cli::sync::handle_kg_sync(
                incremental,
                labels,
                config_chunks,
                graph,
                queue,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt kub ----
        Some(Commands::Kub {
            action,
            namespace,
            pod,
            since,
            output,
            resource,
        }) => {
            let config = load_config();
            match resolve_k8s_config(&config) {
                Some(cfg) => {
                    dt_daemon::interfaces::cli::kub::handle_kub(
                        action, namespace, pod, since, output, resource, cfg,
                    )
                    .await?;
                }
                None => {
                    eprintln!("K8s not configured in config.yaml (services.k8s). Add k8s section to enable.");
                }
            }
            return Ok(());
        }

        // ---- CLI mode: dt jcli ----
        Some(Commands::Jcli {
            action,
            job,
            build,
            limit,
            params,
            env,
        }) => {
            let config = load_config();
            let jenkins_creds = config.as_ref().and_then(|c| {
                let j = &c.services.jenkins;
                let url = j.url.as_deref()?;
                let user = j.user.as_deref()?;
                let token = j.token.as_deref()?;
                if url.is_empty() {
                    return None;
                }
                Some((url.to_string(), user.to_string(), token.to_string()))
            });

            match jenkins_creds {
                Some((url, user, token)) => {
                    let graph = connect_graph().await;
                    dt_daemon::interfaces::cli::jcli::handle_jcli(
                        action, job, build, limit, params, env, &url, &user, &token, graph,
                    )
                    .await?;
                }
                None => {
                    eprintln!("Jenkins not configured in config.yaml (services.jenkins). Add jenkins section with url/user/token to enable.");
                }
            }
            return Ok(());
        }

        // ---- CLI mode: dt jc-sync ----
        Some(Commands::JcSync { job }) => {
            let config = load_config();
            let jenkins_creds = config.as_ref().and_then(|c| {
                let j = &c.services.jenkins;
                let url = j.url.as_deref()?;
                let user = j.user.as_deref()?;
                let token = j.token.as_deref()?;
                if url.is_empty() {
                    return None;
                }
                Some((url.to_string(), user.to_string(), token.to_string()))
            });

            match jenkins_creds {
                Some((url, user, token)) => {
                    let graph = connect_graph().await;
                    dt_daemon::interfaces::cli::jenkins_sync::handle_jenkins_sync(
                        job, graph, &url, &user, &token,
                    )
                    .await?;
                }
                None => {
                    eprintln!("Jenkins not configured in config.yaml (services.jenkins). Add jenkins section with url/user/token to enable.");
                }
            }
            return Ok(());
        }

        // ---- dt daemon status ----
        Some(Commands::Daemon { action }) => {
            match action.as_str() {
                "status" => {
                    tracing::info!("dt-daemon CLI: 守护进程状态");
                    let memgraph = connect_memgraph().await;
                    let qdrant = connect_vector().await;
                    let snapshot = connect_snapshot().await;
                    let embed = connect_embed().await;
                    dt_daemon::interfaces::cli::cleanup::run_health(
                        memgraph.as_ref().map(|c| c as &dyn GraphRepository),
                        qdrant.as_deref().map(|c| c as &dyn VectorRepository),
                        snapshot.as_deref().map(|c| c as &dyn SnapshotRepository),
                        embed.as_deref().map(|c| c as &dyn EmbedService),
                    )
                    .await?;
                }
                _ => {
                    // start — fall through to server mode
                    return Ok(());
                }
            }
            return Ok(());
        }

        // ---- Server mode (default / dt daemon start) ----
        None => {
            tracing::info!("dt-daemon 启动中 (服务器模式)");

            let config = AppConfig {
                listen_addr: std::env::var("DT_LISTEN_ADDR")
                    .unwrap_or_else(|_| "127.0.0.1:50051".into()),
                ..AppConfig::default()
            };

            // Listen for Ctrl+C so we can shut down gracefully
            let shutdown = tokio::spawn(async {
                tokio::signal::ctrl_c().await.ok();
                tracing::info!("收到关闭信号");
            });

            // Run server (blocks until error or explicit shutdown)
            tokio::select! {
                result = dt_daemon::interfaces::grpc::server::run(config) => {
                    if let Err(e) = result {
                        tracing::error!("服务器错误: {}", e);
                    }
                }
                _ = shutdown => {
                    tracing::info!("dt-daemon 关闭中");
                }
            }
        }
    }

    Ok(())
}
