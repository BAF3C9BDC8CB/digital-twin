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
use dt_daemon::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use dt_daemon::domain::types::AppConfig;
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
    /// summary of what would be deleted and exits.
    ///
    /// Use `--targets reasoning` to only clean stale Reasoning nodes
    /// (Observation, Analysis, Decision) that have been marked stale for
    /// more than 30 days. Supports `--dry-run` for preview.
    Clean {
        /// Confirm the destructive operation.
        #[arg(long = "confirm")]
        confirm: bool,

        /// Preview only — show what would be deleted without executing.
        #[arg(long = "dry-run")]
        dry_run: bool,

        /// Specific targets to clean (comma-separated).
        /// Supported: "reasoning", "all" (default when no targets specified).
        #[arg(long = "targets", value_delimiter = ',')]
        targets: Vec<String>,
    },

    /// Tiered cleanup with dry-run, execute, and per-target selection.
    ///
    /// Supports:
    /// - `reasoning`  — delete stale Reasoning nodes (Observation/Analysis/Decision >30d)
    /// - `memory`     — archive Memory World events beyond retention (365d)
    /// - `snapshots`  — remove orphaned SQLite snapshot rows
    /// - `all`        — run all cleanup targets
    Cleanup {
        /// Preview only — show what would be cleaned without executing.
        #[arg(long = "dry-run")]
        dry_run: bool,

        /// Execute the cleanup (otherwise defaults to dry-run).
        #[arg(long = "exec")]
        execute: bool,

        /// Specific targets to clean (comma-separated).
        /// Supported: "reasoning", "memory", "snapshots", "all".
        #[arg(long = "targets", value_delimiter = ',', default_value = "all")]
        targets: Vec<String>,
    },

    /// System backup — tiered backup of Memgraph, Qdrant, and SQLite.
    ///
    /// Default (no subcommand) creates a new backup.
    /// Subcommands: list, restore <date>, verify <date>.
    Backup {
        #[command(subcommand)]
        action: Option<BackupAction>,
    },

    /// Memory World archiving — archive events beyond retention to compressed files.
    ///
    /// `dt archive` — dry-run preview.
    /// `dt archive <YYYY-MM-DD>` — archive events before this date.
    /// `dt archive --exec <YYYY-MM-DD>` — execute (without --exec, runs dry-run).
    /// `dt archive --list` — list existing archive files.
    Archive {
        /// Cutoff date — archive events before this date (format: YYYY-MM-DD).
        before: Option<String>,

        /// Execute the archive (otherwise runs dry-run preview).
        #[arg(long = "exec")]
        execute: bool,

        /// List existing archive files.
        #[arg(long = "list")]
        list: bool,
    },

    /// Schema management commands.
    #[command(subcommand)]
    Schema(SchemaAction),

    /// Check health of all backend services (Memgraph, Qdrant, SQLite, dt-embed).
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
    /// `dt build --full` — full rebuild (can combine with --path/--name/--file).
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
    },

    /// Semantic code search across worlds.
    ///
    /// Usage: dt search <query> [--world code|knowledge|doc|all] [--limit 10]
    Search {
        /// Search query string.
        query: String,

        /// Which world to search: code, knowledge, doc, all.
        #[arg(long = "world", default_value = "code")]
        world: String,

        /// Limit results.
        #[arg(long = "limit", default_value = "10")]
        limit: usize,

        /// Scope to a project name (searches only {project}_methods in Qdrant).
        #[arg(long = "project", short = 'p')]
        project: Option<String>,

        /// Scope path.
        #[arg(long = "path")]
        path: Option<PathBuf>,
    },

    /// Semantic search of KG nodes via Qdrant vector store.
    ///
    /// Usage: dt search-kg <query> [--limit 10]
    SearchKg {
        /// Search query string (positional).
        query: String,

        /// Limit results.
        #[arg(long = "limit", default_value = "10")]
        limit: usize,
    },

    /// Build aggregated six-world context for a task.
    ///
    /// Usage: dt context <task> [--worlds ...] [--max-tokens ...]
    Context {
        /// Task description.
        task: String,

        /// Worlds to query (comma-separated).
        #[arg(long = "worlds")]
        worlds: Option<String>,

        /// Max tokens.
        #[arg(long = "max-tokens")]
        max_tokens: Option<usize>,

        /// Thread ID.
        #[arg(long = "thread-id")]
        thread_id: Option<String>,
    },

    /// Generate execution plan by matching playbooks.
    ///
    /// Usage: dt plan <task> [--context ...] [--thread-id ...]
    Plan {
        /// Task description.
        task: String,

        /// Optional context from dt_context output.
        #[arg(long = "context")]
        context: Option<String>,

        /// Thread ID.
        #[arg(long = "thread-id")]
        thread_id: Option<String>,
    },

    /// Query domain knowledge model subgraph.
    ///
    /// Usage: dt domain <name> [--depth 2] [--include-code]
    Domain {
        /// Domain name (e.g. "支付", "部署").
        name: String,

        /// Traversal depth.
        #[arg(long = "depth", default_value = "2")]
        depth: usize,

        /// Include code entities.
        #[arg(long = "include-code")]
        include_code: bool,
    },

    /// Retrieve similar historical tasks from Memory World.
    ///
    /// Usage: dt history <task> [--domain ...] [--days 90] [--limit 5]
    History {
        /// Task description for similarity matching.
        task: String,

        /// Domain filter.
        #[arg(long = "domain")]
        domain: Option<String>,

        /// Lookback days.
        #[arg(long = "days", default_value = "90")]
        days: u32,

        /// Max results.
        #[arg(long = "limit", default_value = "5")]
        limit: usize,
    },

    /// Analyze call-chain and dependency impact.
    ///
    /// Usage: dt dependency <target> [--direction both] [--depth 2] [--type all]
    Dependency {
        /// Target entity (method name, class name, service name).
        target: String,

        /// Direction: upstream, downstream, both.
        #[arg(long = "direction", default_value = "both")]
        direction: String,

        /// Traversal depth.
        #[arg(long = "depth", default_value = "2")]
        depth: usize,

        /// Dependency type: code, config, service, all.
        #[arg(long = "type", default_value = "all")]
        dep_type: String,
    },

    /// Verify consistency after code changes.
    ///
    /// Usage: dt verify <files> [--check-config] [--check-db] [--check-api]
    Verify {
        /// Changed file paths (comma-separated).
        #[arg(value_delimiter = ',')]
        files: Vec<String>,

        /// Check Nacos config consistency.
        #[arg(long = "check-config")]
        check_config: bool,

        /// Check database schema consistency.
        #[arg(long = "check-db")]
        check_db: bool,

        /// Check API signature consistency.
        #[arg(long = "check-api")]
        check_api: bool,
    },

    /// Query system metrics via gRPC (no HTTP).
    Metrics {
        /// Watch mode — continuous output.
        #[arg(long = "watch")]
        watch: bool,

        /// Poll interval in seconds.
        #[arg(long = "interval", default_value = "5")]
        interval: u64,

        /// Filter metric names (glob, e.g. "dt_build*").
        #[arg(long = "filter")]
        filter: Option<String>,
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

    /// Manage Digital Thread lifecycle.
    ///
    /// Subcommands: list, get <id>, create <name>, close <id>,
    ///              add-session <thread-id> <session-id>, add-decision <thread-id> <decision-id>
    Thread {
        #[command(subcommand)]
        action: Option<ThreadAction>,
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

#[derive(Subcommand)]
enum ThreadAction {
    /// List all threads.
    List,
    /// Get thread details by ID.
    Get {
        /// Thread ID.
        thread_id: String,
    },
    /// Create a new thread.
    Create {
        /// Thread name.
        name: String,
        /// Thread description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Close a thread.
    Close {
        /// Thread ID.
        thread_id: String,
    },
    /// Add a session to a thread.
    AddSession {
        /// Thread ID.
        thread_id: String,
        /// Session ID.
        session_id: String,
    },
    /// Add a decision to a thread.
    AddDecision {
        /// Thread ID.
        thread_id: String,
        /// Decision ID.
        decision_id: String,
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
    embed_server: EmbedServerConfig,
    #[serde(default)]
    reranker: RerankerConfig,
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
        Self { path: default_sqlite_path() }
    }
}

fn default_sqlite_path() -> String {
    "/var/lib/digital-twin/snapshots.db".to_string()
}

/// dt-embed gRPC server configuration from config.yaml `services.embed_server`.
#[derive(Debug, Deserialize, Default)]
struct EmbedServerConfig {
    /// URL of the dt-embed gRPC server.
    #[serde(default = "default_embed_url")]
    url: String,
}

fn default_embed_url() -> String {
    "http://[::1]:50051".to_string()
}

/// dt-reranker gRPC server configuration from config.yaml `services.reranker`.
#[derive(Debug, Deserialize, Default)]
struct RerankerConfig {
    /// URL of the dt-reranker gRPC server.
    #[serde(default = "default_reranker_url")]
    url: String,
}

fn default_reranker_url() -> String {
    "http://[::1]:50051".to_string()
}

#[derive(Debug, Deserialize)]
struct ProjectGroup {
    base: String,
    #[serde(default)]
    items: Vec<serde_yaml::Value>,
}

/// Attempt to find and load the project configuration file.
///
/// Search order:
/// 1. `./config.yaml` (working directory)
/// 2. `~/.config/opencode/skills/digital-twin/config.yaml`
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

    match dt_daemon::infrastructure::memgraph::MemgraphClient::connect(
        &bolt_url, user, password,
    )
    .await
    {
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

/// Build a HookEngine from the Memgraph graph connection and event-hooks.yaml.
/// Returns `None` if Memgraph is unavailable or the config file is missing.
async fn connect_hook_engine() -> Option<Arc<dt_daemon::application::hooks::HookEngine>> {
    let graph = connect_graph().await?;
    match dt_daemon::application::hooks::HookRegistry::from_file("config/event-hooks.yaml") {
        Ok(registry) => {
            tracing::info!("HookRegistry loaded from config/event-hooks.yaml");
            Some(Arc::new(dt_daemon::application::hooks::HookEngine::new(
                Arc::new(registry),
                graph,
            )))
        }
        Err(e) => {
            tracing::warn!("failed to load config/event-hooks.yaml: {e} — hook engine disabled");
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

    match dt_daemon::infrastructure::memgraph::MemgraphClient::connect(
        &bolt_url, user, password,
    )
    .await
    {
        Ok(client) => {
            tracing::info!("Memgraph connected: {}", bolt_url);
            Some(client)
        }
        Err(e) => {
            tracing::warn!("Memgraph connection failed (will use noop): {}", e);
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
            tracing::info!("Qdrant connected: {}", qdrant_uri);
            let repo = dt_daemon::infrastructure::qdrant::QdrantRepo::new(client);
            Some(Arc::new(repo) as Arc<dyn dt_daemon::domain::traits::VectorRepository>)
        }
        Err(e) => {
            tracing::warn!("Qdrant connection failed (will use noop): {}", e);
            None
        }
    }
}

/// Connect to the dt-embed gRPC service (falls back to NoopEmbedService).
async fn connect_embed() -> Option<Arc<dyn dt_daemon::domain::traits::EmbedService>> {
    let url = load_config()
        .map(|c| c.services.embed_server.url.clone())
        .unwrap_or_else(default_embed_url);

    match dt_daemon::infrastructure::embedder::GrpcEmbedService::connect(&url).await {
        Ok(svc) => {
            tracing::info!("dt-embed connected via gRPC: {url}");
            Some(Arc::new(svc) as Arc<dyn dt_daemon::domain::traits::EmbedService>)
        }
        Err(e) => {
            tracing::warn!(
                "dt-embed gRPC unavailable at {url}: {e} — using NoopEmbedService (zero-vectors)"
            );
            Some(Arc::new(
                dt_daemon::infrastructure::embedder::NoopEmbedService::default(),
            )
                as Arc<dyn dt_daemon::domain::traits::EmbedService>)
        }
    }
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
            tracing::info!("SQLite snapshot store connected: {db_path}");
            Some(Arc::new(repo) as Arc<dyn dt_daemon::domain::traits::SnapshotRepository>)
        }
        Err(e) => {
            tracing::warn!("SQLite snapshot store unavailable: {e} — incremental builds disabled");
            None
        }
    }
}

/// Build a resolved K8sSyncConfig from config.yaml services.k8s.
fn resolve_k8s_config(config: &Option<DaemonConfig>) -> Option<dt_daemon::application::sync::k8s::K8sSyncConfig> {
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

    // Register built-in metrics
    dt_daemon::interfaces::grpc::services::metrics_service::MetricsServiceImpl::register_builtins();

    let cli = Cli::parse();

    match cli.command {
        // ---- CLI mode: dt clean ----
        Some(Commands::Clean { confirm, dry_run, targets }) => {
            if targets.iter().any(|t| t == "reasoning") {
                dt_daemon::interfaces::cli::cleanup::run_clean_reasoning(dry_run).await?;
                return Ok(());
            }

            let memgraph = connect_memgraph().await;
            dt_daemon::interfaces::cli::cleanup::run_clean(
                confirm,
                memgraph.as_ref().map(|c| c as &dyn dt_daemon::domain::traits::GraphRepository),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt cleanup (tiered cleanup) ----
        Some(Commands::Cleanup { dry_run, execute, targets }) => {
            let is_dry_run = dry_run || !execute;

            if targets.iter().any(|t| t == "all") {
                if is_dry_run {
                    println!("=== dt cleanup --targets all (dry-run) ===");
                    println!("  Use --exec to perform actual cleanup.");
                    println!();
                }
                dt_daemon::interfaces::cli::cleanup::run_cleanup_all(is_dry_run).await?;
                return Ok(());
            }

            let mut any_target = false;

            if targets.iter().any(|t| t == "reasoning") {
                any_target = true;
                dt_daemon::interfaces::cli::cleanup::run_clean_reasoning(is_dry_run).await?;
            }

            if targets.iter().any(|t| t == "memory") {
                any_target = true;
                dt_daemon::interfaces::cli::cleanup::run_cleanup_memory(is_dry_run).await?;
            }

            if targets.iter().any(|t| t == "snapshots") {
                any_target = true;
                dt_daemon::interfaces::cli::cleanup::run_cleanup_snapshots(is_dry_run).await?;
            }

            if !any_target {
                eprintln!(
                    "Unknown targets: {:?}. Supported: reasoning, memory, snapshots, all",
                    targets
                );
            }

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
                        if report.targets.memgraph { "✓" } else { "✗" },
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
                    println!(
                        "  Duration:   {:.1}s",
                        report.duration_seconds,
                    );
                }
                BackupAction::List => {
                    println!("=== dt backup list ===");
                    let entries = dt_daemon::interfaces::cli::backup::list_backups().await?;

                    if entries.is_empty() {
                        println!("No backups found.");
                    } else {
                        println!(
                            " {:<12}  {:<10}  {:>8}",
                            "DATE", "SIZE", "FILES"
                        );
                        println!(
                            " {:<12}  {:<10}  {:>8}",
                            "------------", "----------", "--------"
                        );
                        for entry in &entries {
                            println!(
                                " {:<12}  {:>8} B  {:>8}",
                                entry.date,
                                entry.total_size_bytes,
                                entry.file_count,
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
                    let report = dt_daemon::interfaces::cli::backup::verify_backup_files(&date).await?;

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
                    println!(
                        "  Duration: {:.1}s",
                        report.duration_seconds,
                    );
                }
            }

            return Ok(());
        }

        // ---- CLI mode: dt archive ----
        Some(Commands::Archive { before, execute, list: list_flag }) => {
            if list_flag {
                println!("=== dt archive --list ===");
                let entries = dt_daemon::interfaces::cli::archive::list_archives().await?;

                if entries.is_empty() {
                    println!("No archives found.");
                } else {
                    println!(
                        " {:<30}  {:>8}  {:>6}",
                        "DATE RANGE", "SIZE", "EVENTS"
                    );
                    println!(
                        " {:<30}  {:>8}  {:>6}",
                        "------------------------------", "--------", "------"
                    );
                    for entry in &entries {
                        println!(
                            " {:<30}  {:>7}B  {:>6}",
                            entry.date_range,
                            entry.size_bytes,
                            entry.events_count,
                        );
                    }
                    println!();
                    println!("Total: {} archive(s)", entries.len());
                }

                return Ok(());
            }

            let dry_run = !execute;
            let report = dt_daemon::interfaces::cli::archive::run_archive(before.as_deref(), dry_run).await?;

            if !dry_run {
                println!();
                println!("Archive created:");
                println!("  File:    {}", report.archive_file.display());
                println!("  Events:  {}", report.events_archived);
                println!("  Space:   {} bytes freed", report.space_freed_bytes);
                println!("  Time:    {:.1}s", report.duration_seconds);
            }

            return Ok(());
        }

        // ---- CLI mode: dt schema init ----
        Some(Commands::Schema(SchemaAction::Init)) => {
            let memgraph = connect_memgraph().await;
            dt_daemon::interfaces::cli::cleanup::run_schema_init(
                memgraph.as_ref().map(|c| c as &dyn dt_daemon::domain::traits::GraphRepository),
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
            let queue = embed.map(|e| Arc::new(
                dt_daemon::application::sync::queue::VectorQueue::spawn(e),
            ));
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
        Some(Commands::Event {
            hook_name,
            context,
        }) => {
            let hook_engine = connect_hook_engine().await;
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let vector = connect_vector().await;
            let queue = embed.map(|e| Arc::new(
                dt_daemon::application::sync::queue::VectorQueue::spawn(e),
            ));
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
            let queue = embed.map(|e| Arc::new(
                dt_daemon::application::sync::queue::VectorQueue::spawn(e),
            ));
            let sync_acc = build_sync_acc(graph.clone(), vector, queue).await;
            dt_daemon::interfaces::cli::learn::handle_learn(
                task,
                entities,
                pattern,
                pitfalls,
                decisions,
                thread_id,
                success,
                project,
                graph,
                sync_acc,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt build ----
        Some(Commands::Build { path, name, file, full }) => {
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

                dt_daemon::interfaces::cli::build::handle_build_all(
                    projects, full, graph, vector, embed, snapshot,
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

            dt_daemon::interfaces::cli::build::handle_build(
                actual_path, name, file, full, graph, vector, embed, snapshot,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt search ----
        Some(Commands::Search { query, world, limit, path, project }) => {
            let graph = connect_graph().await;
            let vector = connect_vector().await;
            dt_daemon::interfaces::cli::build::handle_search(
                query, world, limit, path, project, graph, vector,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt search-kg ----
        Some(Commands::SearchKg { query, limit }) => {
            tracing::info!("dt-daemon CLI: search-kg \"{query}\" --limit {limit}");
            let graph = connect_graph().await;
            let vector = connect_vector().await;
            dt_daemon::interfaces::cli::build::handle_search_kg(
                query, limit, graph, vector,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt context ----
        Some(Commands::Context { task, worlds, max_tokens, thread_id }) => {
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            dt_daemon::interfaces::cli::context::handle_context(
                task, worlds, max_tokens, thread_id, graph, embed,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt plan ----
        Some(Commands::Plan { task, context, thread_id }) => {
            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::context::handle_plan(
                task, context, thread_id, graph,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt domain ----
        Some(Commands::Domain { name, depth, include_code }) => {
            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::context::handle_domain(
                name, depth, include_code, graph,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt history ----
        Some(Commands::History { task, domain, days, limit }) => {
            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::context::handle_history(
                task, domain, days, limit, graph,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt dependency ----
        Some(Commands::Dependency { target, direction, depth, dep_type }) => {
            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::context::handle_dependency(
                target, direction, depth, dep_type, graph,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt verify ----
        Some(Commands::Verify { files, check_config, check_db, check_api }) => {
            let graph = connect_graph().await;
            dt_daemon::interfaces::cli::context::handle_verify(
                files, check_config, check_db, check_api, graph,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt metrics ----
        Some(Commands::Metrics { watch, interval, filter }) => {
            tracing::info!(
                "dt-daemon CLI: metrics --watch {watch} --interval {interval} --filter {:?}",
                filter,
            );

            if watch {
                println!("Metrics: watch mode, interval={interval}s");
                if let Some(ref f) = filter {
                    println!("  filter: {f}");
                }
            } else {
                println!("Metrics snapshot:");
                if let Some(ref f) = filter {
                    println!("  filter: {f}");
                }
            }
            tracing::info!("metrics query complete (placeholder)");

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
        Some(Commands::KgSync { full, labels, config_chunks }) => {
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let queue = embed.map(|e| Arc::new(
                dt_daemon::application::sync::queue::VectorQueue::spawn(e),
            ));
            let incremental = !full;
            dt_daemon::interfaces::cli::sync::handle_kg_sync(incremental, labels, config_chunks, graph, queue).await?;
            return Ok(());
        }

        // ---- CLI mode: dt thread ----
        Some(Commands::Thread { action }) => {
            let graph = connect_graph().await;
            let (action_str, name, description, thread_id, session_id, decision_id) =
                match action.unwrap_or(ThreadAction::List) {
                    ThreadAction::List => ("list".into(), None, None, None, None, None),
                    ThreadAction::Get { thread_id } => ("get".into(), None, None, Some(thread_id), None, None),
                    ThreadAction::Create { name, description } => ("create".into(), Some(name), description, None, None, None),
                    ThreadAction::Close { thread_id } => ("close".into(), None, None, Some(thread_id), None, None),
                    ThreadAction::AddSession { thread_id, session_id } => ("add-session".into(), None, None, Some(thread_id), Some(session_id), None),
                    ThreadAction::AddDecision { thread_id, decision_id } => ("add-decision".into(), None, None, Some(thread_id), None, Some(decision_id)),
                };
            dt_daemon::interfaces::cli::thread::handle_thread(
                action_str, name, description, thread_id, session_id, decision_id, graph,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI mode: dt kub ----
        Some(Commands::Kub { action, namespace, pod, since, output, resource }) => {
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
        Some(Commands::Jcli { action, job, build, limit, params, env }) => {
            let config = load_config();
            let jenkins_creds = config.as_ref().and_then(|c| {
                let j = &c.services.jenkins;
                let url = j.url.as_deref()?;
                let user = j.user.as_deref()?;
                let token = j.token.as_deref()?;
                if url.is_empty() { return None; }
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
                if url.is_empty() { return None; }
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
                    tracing::info!("dt-daemon CLI: daemon status");
                    let memgraph = connect_memgraph().await;
                    let qdrant = connect_vector().await;
                    let snapshot = connect_snapshot().await;
                    let embed = connect_embed().await;
                    dt_daemon::interfaces::cli::cleanup::run_health(
                        memgraph.as_ref().map(|c| c as &dyn GraphRepository),
                        qdrant.as_deref().map(|c| c as &dyn VectorRepository),
                        snapshot.as_deref().map(|c| c as &dyn SnapshotRepository),
                        embed.as_deref().map(|c| c as &dyn EmbedService),
                    ).await?;
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
            tracing::info!("dt-daemon starting (server mode)");

            let config = AppConfig {
                listen_addr: std::env::var("DT_LISTEN_ADDR")
                    .unwrap_or_else(|_| "127.0.0.1:50051".into()),
                ..AppConfig::default()
            };

            // Listen for Ctrl+C so we can shut down gracefully
            let shutdown = tokio::spawn(async {
                tokio::signal::ctrl_c().await.ok();
                tracing::info!("received shutdown signal");
            });

            // Run server (blocks until error or explicit shutdown)
            tokio::select! {
                result = dt_daemon::interfaces::grpc::server::run(config) => {
                    if let Err(e) = result {
                        tracing::error!("server error: {}", e);
                    }
                }
                _ = shutdown => {
                    tracing::info!("dt-daemon shutting down");
                }
            }
        }
    }

    Ok(())
}
