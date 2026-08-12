//! 运行时组装 — 连接后端服务(Memgraph/Qdrant/Embed/SQLite/Hooks)。
//!
//! 原为 `main.rs` 的私有函数; 抽取为库模块后, CLI 与 `dt-mcp`
//! 共用同一套连接逻辑, 避免两处维护。

use std::path::PathBuf;
use std::sync::Arc;

use crate::application::hooks::HookEngine;
use crate::application::pipeline::config::PipelineConfig;
use crate::application::sync::batch::SyncAccumulator;
use crate::application::sync::kg_bridge::KgBridge;
use crate::application::sync::queue::VectorQueue;
use crate::domain::traits::{
    EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use crate::domain::types::{BatchConfig, ScanConfig};
use serde::Deserialize;

// ---- 配置结构(config.yaml) --------------------------------------

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub projects: Vec<ProjectGroup>,
    #[serde(default)]
    pub services: ServiceConfig,
    #[serde(default)]
    pub batch: BatchConfig,
    #[serde(default)]
    pub scanner: ScannerFileConfig,
}

/// config.yaml 的 `scanner` 段 —— 构建扫描器的忽略规则。
#[derive(Debug, Deserialize, Default)]
pub struct ScannerFileConfig {
    #[serde(default)]
    pub ignore_dirs: Vec<String>,
    #[serde(default)]
    pub ignore_ext: Vec<String>,
    #[serde(default)]
    pub ignore_files: Vec<String>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ServiceConfig {
    #[serde(default, alias = "memgraph")]
    pub graph: GraphDbConfig,
    #[serde(default)]
    pub qdrant: QdrantServiceConfig,
    #[serde(default)]
    pub jenkins: JenkinsEndpointConfig,
    #[serde(default)]
    pub sqlite: SqliteConfig,
}

#[derive(Debug, Deserialize)]
pub struct GraphDbConfig {
    pub url: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl Default for GraphDbConfig {
    fn default() -> Self {
        Self {
            url: Some("bolt://localhost:7687".to_string()),
            user: Some("memgraph".to_string()),
            password: Some(String::new()),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct QdrantServiceConfig {
    #[serde(default)]
    pub url: Option<String>,
}

/// 来自 config.yaml `services.jenkins` 的 Jenkins 连接信息。
#[derive(Debug, Deserialize, Default)]
pub struct JenkinsEndpointConfig {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// 来自 config.yaml `services.sqlite` 的 SQLite 快照存储配置。
#[derive(Debug, Deserialize)]
pub struct SqliteConfig {
    /// SQLite 快照数据库文件的路径。
    #[serde(default = "default_sqlite_path")]
    pub path: String,
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

#[derive(Debug, Deserialize)]
pub struct ProjectGroup {
    pub base: String,
    #[serde(default)]
    pub items: Vec<serde_yaml::Value>,
}

// ---- 配置加载与解析 ----------------------------------------------

/// 解析 `~/.config/...`，无需引入 `dirs` crate。
pub fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(suffix))
}

/// 从 `~/.config/digital-twin/config.yaml` 加载配置。
pub fn load_config() -> Option<DaemonConfig> {
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

/// 将 config.yaml 的 `scanner` 段转换为 `ScanConfig`。
///
/// 用户配置的列表与内置默认值**合并**（而非覆盖），确保常见噪音目录
/// 始终被忽略；`max_file_size` 未配置时用默认 500KB。
pub fn scan_config_from(cfg: &DaemonConfig) -> ScanConfig {
    let mut sc = ScanConfig::default();
    for d in &cfg.scanner.ignore_dirs {
        if !d.is_empty() {
            sc.ignore_dirs.insert(d.clone());
        }
    }
    for f in &cfg.scanner.ignore_files {
        if !f.is_empty() {
            sc.ignore_files.insert(f.clone());
        }
    }
    for e in &cfg.scanner.ignore_ext {
        let e = e.trim();
        if !e.is_empty() {
            sc.ignore_ext.insert(if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            });
        }
    }
    if let Some(m) = cfg.scanner.max_file_size {
        if m > 0 {
            sc.max_file_size = m;
        }
    }
    sc
}

/// 将项目组扁平化为 `(name, full_path)` 对。
pub fn resolve_project_paths(cfg: &DaemonConfig) -> Vec<(String, PathBuf)> {
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
                _ => {}
            }
        }
    }
    out
}

/// 从 config.yaml `services.graph` 解析 Memgraph Bolt URI。
pub fn resolve_graph_bolt_url(cfg: &GraphDbConfig) -> String {
    match &cfg.url {
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            if let Some(host) = url
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .split(':')
                .next()
            {
                format!("bolt://{host}:7687")
            } else {
                "bolt://localhost:7687".to_string()
            }
        }
        Some(url) if url.starts_with("bolt://") => url.clone(),
        Some(url) => format!("bolt://{url}:7687"),
        None => "bolt://localhost:7687".to_string(),
    }
}

// ---- 后端连接 ------------------------------------------------------

/// 使用 config.yaml 中的值（或合理默认值）连接 Memgraph。
pub async fn connect_graph() -> Option<Arc<dyn GraphRepository>> {
    let cfg = load_config()?;
    let bolt_url = resolve_graph_bolt_url(&cfg.services.graph);
    let user = cfg.services.graph.user.as_deref().unwrap_or("memgraph");
    let password = cfg.services.graph.password.as_deref().unwrap_or("");

    match crate::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password)
        .await
    {
        Ok(client) => {
            tracing::info!("Memgraph 已连接: {bolt_url}");
            Some(Arc::new(client) as Arc<dyn GraphRepository>)
        }
        Err(e) => {
            tracing::warn!("Memgraph 连接失败 (将使用 noop): {e}");
            None
        }
    }
}

/// 连接 Hook 引擎(依赖 graph + event-hooks.yaml)。
pub async fn connect_hook_engine() -> Option<Arc<HookEngine>> {
    let graph = connect_graph().await?;
    let path = dirs_like_home_config(".config/digital-twin/event-hooks.yaml")?;
    match crate::application::hooks::HookRegistry::from_file(&path) {
        Ok(registry) => {
            tracing::info!("HookRegistry 已加载: {}", path.display());
            Some(Arc::new(HookEngine::new(Arc::new(registry), graph)))
        }
        Err(e) => {
            tracing::warn!("加载 HookRegistry 失败 {}: {e}", path.display());
            None
        }
    }
}

/// 连接 Memgraph, 返回原始客户端(用于 schema/clean 等)。
pub async fn connect_memgraph() -> Option<crate::infrastructure::memgraph::MemgraphClient> {
    let cfg = load_config()?;
    let bolt_url = resolve_graph_bolt_url(&cfg.services.graph);
    let user = cfg.services.graph.user.as_deref().unwrap_or("memgraph");
    let password = cfg.services.graph.password.as_deref().unwrap_or("");

    match crate::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password)
        .await
    {
        Ok(client) => {
            tracing::info!("Memgraph 已连接: {bolt_url}");
            Some(client)
        }
        Err(e) => {
            tracing::warn!("Memgraph 连接失败 (将使用 noop): {e}");
            None
        }
    }
}

/// 连接 Qdrant 向量库。
pub async fn connect_vector() -> Option<Arc<dyn VectorRepository>> {
    let cfg = load_config()?;
    let qdrant_uri = cfg
        .services
        .qdrant
        .url
        .as_deref()
        .unwrap_or("http://localhost:6334");

    match crate::infrastructure::qdrant::QdrantClient::connect(qdrant_uri).await {
        Ok(client) => {
            tracing::info!("Qdrant 已连接: {qdrant_uri}");
            let repo = crate::infrastructure::qdrant::QdrantRepo::new(client);
            Some(Arc::new(repo) as Arc<dyn VectorRepository>)
        }
        Err(e) => {
            tracing::warn!("Qdrant 连接失败 (将使用 noop): {e}");
            None
        }
    }
}

/// 连接 Embed 路由(从 pipeline.yaml providers 构建)。
pub async fn connect_embed() -> Option<Arc<dyn EmbedService>> {
    let pipeline_cfg = PipelineConfig::load().ok()?;
    let pcfg = pipeline_cfg.providers?;

    let sf = pcfg.siliconflow.as_ref();
    let xi = pcfg.xinference.as_ref();

    let sf_url = sf.map(|s| s.url.as_str()).unwrap_or("");
    let xi_url = xi.map(|s| s.url.as_str()).unwrap_or("");
    if sf_url.is_empty() && xi_url.is_empty() {
        tracing::warn!("pipeline.yaml providers: 所有 provider URL 为空，跳过 embed 服务");
        return None;
    }

    let api_key_fallback = || std::env::var("SILICONFLOW_API_KEY").unwrap_or_default();

    let cfg = crate::infrastructure::embedder::ProviderConfig {
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
        siliconflow_max_concurrent: sf.map(|s| s.max_concurrent).unwrap_or(20),
        xinference_url: xi_url.to_string(),
        xinference_api_key: xi.map(|s| s.api_key.clone()).unwrap_or_default(),
        xinference_model_embed: xi.map(|s| s.model_embed.clone()).unwrap_or_default(),
        xinference_model_reranker: xi.map(|s| s.model_reranker.clone()).unwrap_or_default(),
        xinference_model_llm: xi.map(|s| s.model_llm.clone()).unwrap_or_default(),
        embed_provider: pcfg.embed_provider.clone(),
        rerank_provider: pcfg.rerank_provider.clone(),
        llm_provider: pcfg.llm_provider.clone(),
    };
    Some(crate::infrastructure::embedder::create_embed_router(cfg))
}

pub async fn build_kg_bridge(
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    queue: Option<Arc<VectorQueue>>,
) -> Option<Arc<KgBridge>> {
    let g = graph?;
    let embed = queue.as_ref()?.embed_service().clone();
    let v = vector.unwrap_or_else(|| {
        Arc::new(crate::infrastructure::qdrant::repo::NoopVectorRepo)
            as Arc<dyn VectorRepository>
    });
    let bridge = KgBridge::new(g, embed, v);
    Some(Arc::new(bridge.with_queue(queue?)))
}

pub async fn build_sync_acc(
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    queue: Option<Arc<VectorQueue>>,
) -> Option<Arc<SyncAccumulator>> {
    let bridge = build_kg_bridge(graph, vector, queue.clone()).await?;
    Some(Arc::new(SyncAccumulator::spawn(bridge, queue?)))
}

/// 连接 SQLite 快照存储。
pub async fn connect_snapshot() -> Option<Arc<dyn SnapshotRepository>> {
    let db_path = load_config()
        .map(|c| c.services.sqlite.path.clone())
        .unwrap_or_else(default_sqlite_path);

    match crate::infrastructure::sqlite::SqliteRepo::open(&db_path) {
        Ok(repo) => {
            tracing::info!("SQLite 快照存储已连接: {db_path}");
            Some(Arc::new(repo) as Arc<dyn SnapshotRepository>)
        }
        Err(e) => {
            tracing::warn!("SQLite 快照存储不可用: {e} — 增量构建已禁用");
            None
        }
    }
}

// ---- DtRuntime: 一次连接, 供 CLI 与 dt-mcp 共用 -------------------

/// 运行时组装结果: 所有后端连接 + 派生组件。
pub struct DtRuntime {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub embed: Option<Arc<dyn EmbedService>>,
    pub snapshot: Option<Arc<dyn SnapshotRepository>>,
    pub hook_engine: Option<Arc<HookEngine>>,
    pub queue: Option<Arc<VectorQueue>>,
    pub sync_acc: Option<Arc<SyncAccumulator>>,
    pub kg_bridge: Option<Arc<KgBridge>>,
    pub projects: Vec<(String, PathBuf)>,
    pub batch_config: Option<BatchConfig>,
    pub scan_config: Option<ScanConfig>,
}

impl DtRuntime {
    /// 连接全部后端(任一失败降级为 None, 由 handler 内部 noop 兜底)。
    pub async fn connect() -> Self {
        let graph = connect_graph().await;
        let embed = connect_embed().await;
        let vector = connect_vector().await;
        let snapshot = connect_snapshot().await;
        let hook_engine = connect_hook_engine().await;

        let queue = embed
            .clone()
            .map(|e| Arc::new(VectorQueue::spawn(e)));
        let kg_bridge = build_kg_bridge(graph.clone(), vector.clone(), queue.clone()).await;
        let sync_acc = build_sync_acc(graph.clone(), vector.clone(), queue.clone()).await;

        let cfg = load_config();
        let projects = cfg.as_ref().map(resolve_project_paths).unwrap_or_default();
        let batch_config = cfg.as_ref().map(|c| c.batch.clone());
        let scan_config = cfg.as_ref().map(scan_config_from);

        Self {
            graph,
            vector,
            embed,
            snapshot,
            hook_engine,
            queue,
            sync_acc,
            kg_bridge,
            projects,
            batch_config,
            scan_config,
        }
    }
}
