//! Digital Twin V2 守护进程 — 组合根与 gRPC 服务器。
//!
//! # 双模式
//!
//! 该守护进程二进制支持三种运行模式：
//!
//! 1. **服务器模式**（默认）— 启动 gRPC 服务器。
//! 2. **CLI 模式** — 以已识别的子命令（如 `build`、`search`）调用时，执行命令并退出。

use clap::{Parser, Subcommand};
use dt_daemon::domain::traits::{
    EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use dt_daemon::domain::types::BatchConfig;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

// ---- CLI 定义 ----

#[derive(Parser)]
#[command(name = "dt-daemon", version = env!("CARGO_PKG_VERSION"), about = "Digital Twin 守护进程")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 清空 Memgraph、Qdrant 与 SQLite 中的所有数据。
    ///
    /// 需要 `--confirm` 才会真正执行；未提供时仅打印将要删除内容的摘要并退出。
    /// 支持 `--dry-run` 进行预览。
    Clean {
        /// 确认破坏性操作。
        #[arg(long = "confirm")]
        confirm: bool,

        /// 仅预览 — 显示将被删除的内容而不真正执行。
        #[arg(long = "dry-run")]
        dry_run: bool,

        /// 要清理的指定目标（逗号分隔）。
        /// 支持值: "all"（未指定目标时的默认值）。
        #[arg(long = "targets", value_delimiter = ',')]
        targets: Vec<String>,
    },

    /// 系统备份 — Memgraph、Qdrant 与 SQLite 的分层备份。
    ///
    /// 默认（无子命令）创建新备份。
    /// 子命令: list、restore <date>、verify <date>。
    Backup {
        #[command(subcommand)]
        action: Option<BackupAction>,
    },

    /// 模式（Schema）管理命令。
    #[command(subcommand)]
    Schema(SchemaAction),

    /// 检查所有后端服务的健康状态（Memgraph、Qdrant、SQLite）。
    Health,

    /// 写入一条知识记录（Knowledge、Experience、Concept、Domain、Playbook）。
    ///
    /// 由 `dt memorize` 与 MCP 工具 `dt_memorize` 调用，将结构化知识
    /// 持久化到 Knowledge World 子图中。
    ///
    /// 用法: dt memorize <type> <entity-id> <details> [--project <name>]
    Memorize {
        /// 知识类型: Decision | KnowledgeAdded | Environment | Dependencies。
        knowledge_type: String,

        /// 实体的唯一标识符。
        entity_id: String,

        /// 人类可读的详情，key: value 格式（分号分隔）。
        details: String,

        /// 实体类型标签（如 ArchitectureDecision、Knowledge、Experience）。
        #[arg(long = "entity-type")]
        entity_type: Option<String>,

        /// 可选的项目名称，用于作用域限定。
        #[arg(long = "project")]
        project: Option<String>,
    },

    /// 以 JSON 上下文对象触发一个具名 hook。
    ///
    /// 取代旧的 `--type` / `--entity-id` / `--details` 接口。
    /// hook 及其副作用模板在 `config/event-hooks.yaml` 中配置。
    ///
    /// 用法: dt event <hook> '<json>'
    Event {
        /// Hook 名称（如 code_modified、jenkins_deploy_completed）。
        hook_name: String,

        /// 携带 hook 副作用模板所需字段的 JSON 对象。
        context: String,
    },

    /// 从 AI 任务执行中学习 — 将结构化知识写入 Knowledge World。
    ///
    /// 接收任务名称、实体、模式、陷阱、决策以及成功/失败标志，
    /// 综合生成 Knowledge、Experience 与 Playbook 节点，
    /// 并更新 Playbook 的成功/失败计数。
    ///
    /// 用法: dt learn <task> [--pattern ...] [--pitfalls ...] [--project ...]
    Learn {
        /// 任务标题或描述（如 "支付平台迁移"）。
        task: String,

        /// 受影响的实体列表（文件、类、服务），逗号分隔。
        #[arg(long = "entities", value_delimiter = ',')]
        entities: Vec<String>,

        /// 已识别的解决方案模式。
        #[arg(long = "pattern")]
        pattern: Option<String>,

        /// 遇到过的陷阱，逗号分隔。
        #[arg(long = "pitfalls", value_delimiter = ',')]
        pitfalls: Vec<String>,

        /// 架构/技术决策，逗号分隔。
        #[arg(long = "decisions", value_delimiter = ',')]
        decisions: Vec<String>,

        /// 可选的 digital-thread ID，用于跨任务血缘追踪。
        #[arg(long = "thread-id")]
        thread_id: Option<String>,

        /// 执行是否成功。
        #[arg(long = "success")]
        success: Option<bool>,

        /// 所属项目名称。
        #[arg(long = "project")]
        project: Option<String>,
    },

    /// 将项目构建（索引）到知识图谱。
    ///
    /// `dt build` — 从 config.yaml 构建所有项目（默认）。
    /// `dt build --path <path>` — 按根路径构建项目。
    /// `dt build --name <name>` — 按 config.yaml 中的名称构建项目。
    /// `dt build --file <file>` — 单文件增量更新。
    /// `dt build --full` — 全量重建（可与 --path/--name/--file 组合）。
    Build {
        /// 项目根路径。
        #[arg(long = "path")]
        path: Option<PathBuf>,

        /// 项目名称（来自 config.yaml）。
        #[arg(long = "name", short = 'n')]
        name: Option<String>,

        /// 单文件路径（用于增量单文件更新）。
        #[arg(long = "file")]
        file: Option<PathBuf>,

        /// 全量重建 — 绕过增量快照。
        #[arg(long = "full")]
        full: bool,

        /// 构建后跳过流水线分析（默认启用）。
        #[arg(long = "no-pipeline")]
        no_pipeline: bool,

        /// 关闭构建末尾的 LLM 缺口补偿自愈（默认开启）。
        /// 语义为"关"：带此 flag 时 llm_backfill=false。
        #[arg(long = "no-llm-backfill", action = clap::ArgAction::SetFalse, default_value_t = true)]
        llm_backfill: bool,

        /// 要构建的源类型: code（默认）、knowledge（将 KG 节点同步为向量）。
        /// 使用 "knowledge" 替代 `dt kg-sync`。
        #[arg(long = "source")]
        source: Option<String>,

        /// 将自适应配置分块同步到 Qdrant 的 config_chunks 集合。
        /// （仅与 --source knowledge 配合使用）
        #[arg(long = "config-chunks")]
        config_chunks: bool,
    },

    /// 跨世界统一搜索。
    ///
    /// 用法: dt search <query> [--world all|code|knowledge|doc|config|memory] [--limit 10] [--json]
    Search {
        /// 搜索查询字符串（位置参数）。
        query: String,

        /// 要搜索的世界: all、code、knowledge、doc、config、memory。
        #[arg(long = "world", default_value = "all")]
        world: String,

        /// 结果数量上限。
        #[arg(long = "limit", default_value = "10")]
        limit: usize,

        /// 向 stdout 输出纯 JSON（用于 MCP / 脚本调用）。
        #[arg(long = "json")]
        json: bool,

        /// 限定到某个项目名称。
        #[arg(long = "project", short = 'p')]
        project: Option<String>,

        /// 按文件类型过滤：类别名（document/code/config）或具体后缀（md/yaml/java…）。
        #[arg(long = "file-type")]
        file_type: Option<String>,

        /// 按内容类型过滤：LLM 语义类型（Config/Service/Standard…）或 AST 类型（Method/Class…）。
        #[arg(long = "content-type", alias = "type")]
        content_type: Option<String>,

        /// 展开命中正文原文块（Config/Method/Doc 的 content 原样逐行显示）。
        #[arg(long = "show-content")]
        show_content: bool,
    },

    /// 环境感知：定位目录所属项目，输出索引状态与内容简报。
    ///
    /// 用法: dt sense [path] [--json]
    Sense {
        /// 目标目录（缺省为当前工作目录）。
        path: Option<std::path::PathBuf>,

        /// 输出纯 JSON 到 stdout（供 MCP / 脚本使用）。
        #[arg(long = "json")]
        json: bool,
    },

    /// （已移除）原 `dt kg-sync` 已被 `dt build --source knowledge` 替代。
    /// 保留隐藏变体仅用于输出友好提示。
    #[command(hide = true)]
    KgSync {
        /// 全量重建 — 同步所有节点（绕过增量）。
        #[arg(long = "full")]
        full: bool,

        /// 指定的标签（逗号分隔）。
        #[arg(long = "labels")]
        labels: Option<String>,

        /// 将自适应配置分块同步到 Qdrant 的 config_chunks 集合。
        #[arg(long = "config-chunks")]
        config_chunks: bool,
    },
}

#[derive(Subcommand)]
enum SchemaAction {
    /// 初始化 V2 模式 — 创建所有唯一性约束与索引。
    Init,
}

#[derive(Subcommand)]
enum BackupAction {
    /// 创建新备份（默认）。
    Create,
    /// 列出可用备份。
    List,
    /// 按日期（YYYY-MM-DD）从备份恢复。
    Restore {
        /// 备份日期（格式: YYYY-MM-DD）。
        date: String,
    },
    /// 按日期（YYYY-MM-DD）校验备份完整性。
    Verify {
        /// 备份日期（格式: YYYY-MM-DD）。
        date: String,
    },
}

// ---- 配置加载 ----

/// 从 config.yaml 提取项目路径所需的最小 YAML 子集。
#[derive(Debug, Deserialize)]
struct DaemonConfig {
    #[serde(default)]
    projects: Vec<ProjectGroup>,
    #[serde(default)]
    services: ServiceConfig,
    #[serde(default)]
    batch: BatchConfig,
    #[serde(default)]
    scanner: ScannerFileConfig,
}

/// config.yaml 的 `scanner` 段 —— 构建扫描器的忽略规则。
#[derive(Debug, Deserialize, Default)]
struct ScannerFileConfig {
    #[serde(default)]
    ignore_dirs: Vec<String>,
    #[serde(default)]
    ignore_ext: Vec<String>,
    #[serde(default)]
    ignore_files: Vec<String>,
    #[serde(default)]
    max_file_size: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct ServiceConfig {
    #[serde(default, alias = "memgraph")]
    graph: GraphDbConfig,
    #[serde(default)]
    qdrant: QdrantServiceConfig,
    #[serde(default)]
    jenkins: JenkinsEndpointConfig,
    #[serde(default)]
    sqlite: SqliteConfig,
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

/// 来自 config.yaml `services.jenkins` 的 Jenkins 连接信息。
#[derive(Debug, Deserialize, Default)]
struct JenkinsEndpointConfig {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

/// 来自 config.yaml `services.sqlite` 的 SQLite 快照存储配置。
#[derive(Debug, Deserialize)]
struct SqliteConfig {
    /// SQLite 快照数据库文件的路径。
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

#[derive(Debug, Deserialize)]
struct ProjectGroup {
    base: String,
    #[serde(default)]
    items: Vec<serde_yaml::Value>,
}

/// 从 `~/.config/digital-twin/config.yaml` 加载配置。
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

/// 将 config.yaml 的 `scanner` 段转换为 `ScanConfig`。
///
/// 用户配置的列表与内置默认值**合并**（而非覆盖），确保常见噪音目录
/// 始终被忽略；`max_file_size` 未配置时用默认 500KB。
fn scan_config_from(cfg: &DaemonConfig) -> dt_daemon::domain::types::ScanConfig {
    use dt_daemon::domain::types::ScanConfig;
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

/// 解析 `~/.config/...`，无需引入 `dirs` crate。
fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(suffix))
}

/// 将项目组扁平化为 `(name, full_path)` 对。
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
                    // 跳过无法识别的条目结构。
                }
            }
        }
    }
    out
}

/// 从 config.yaml `services.graph` 解析 Memgraph Bolt URI。
///
/// 若 `url` 已设置但使用 HTTP 协议（如 `http://localhost:7474`），
/// 则转换为 Bolt（`bolt://localhost:7687`）。若未配置 URL，
/// 返回默认值 `bolt://localhost:7687`。
fn resolve_graph_bolt_url(cfg: &GraphDbConfig) -> String {
    match &cfg.url {
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            // 从 HTTP URL 中提取主机名，使用默认 Bolt 端口
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

/// 使用 config.yaml 中的值（或合理默认值）连接 Memgraph。
/// 返回可供服务直接使用的 `Arc<dyn GraphRepository>`。
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

/// 从 `~/.config/digital-twin/event-hooks.yaml` 构建 HookEngine。
/// 若 Memgraph 不可用或配置文件缺失，则返回 `None`。
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

/// 使用 config.yaml 中的值（或合理默认值）连接 Memgraph。
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

/// 使用 config.yaml 中的值（或合理默认值）连接 Qdrant 向量存储。
/// 返回可供服务直接使用的 `Arc<dyn VectorRepository>`。
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

/// 通过 provider 路由连接嵌入服务。
///
/// 仅从 config/pipeline.yaml（PipelineConfig）读取 provider 配置。
/// 该函数是创建 embed 服务的唯一事实来源。
async fn connect_embed() -> Option<Arc<dyn dt_daemon::domain::traits::EmbedService>> {
    use dt_daemon::application::pipeline::config::PipelineConfig;

    let pipeline_cfg = PipelineConfig::load().ok()?;
    let pcfg = pipeline_cfg.providers?;

    let sf = pcfg.siliconflow.as_ref();
    let xi = pcfg.xinference.as_ref();

    // 至少有一个 provider 必须配置非空 URL
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
    Some(dt_daemon::infrastructure::embedder::create_embed_router(
        cfg,
    ))
}

/// 构建可选的 KgBridge，用于写入后自动将节点同步到 Qdrant。
///
/// 需要同时具备 `graph` 与 `vector`；`queue` 提供带优先级的嵌入能力。
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

/// 构建可选的 SyncAccumulator，用于批量累积的后台同步。
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

/// 连接 SQLite 快照存储（不可用时回退为 None）。
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

// ---- 主函数 ----

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 通过 dt-log 初始化统一日志（JSON → 文件 + stderr 兜底）
    dt_daemon::shared::logging::init::init_logging()?;

    let cli = Cli::parse();

    match cli.command {
        // ---- CLI 模式: dt clean ----
        Some(Commands::Clean {
            confirm,
            dry_run: _,
            targets: _,
        }) => {
            let memgraph = connect_memgraph().await;
            let vector = connect_vector().await;
            let snapshot = connect_snapshot().await;
            dt_daemon::interfaces::cli::cleanup::run_clean(
                confirm,
                memgraph
                    .as_ref()
                    .map(|c| c as &dyn dt_daemon::domain::traits::GraphRepository),
                vector
                    .as_deref()
                    .map(|c| c as &dyn dt_daemon::domain::traits::VectorRepository),
                snapshot
                    .as_deref()
                    .map(|c| c as &dyn dt_daemon::domain::traits::SnapshotRepository),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt backup ----
        Some(Commands::Backup { action }) => {
            match action.unwrap_or(BackupAction::Create) {
                BackupAction::Create => {
                    println!("=== dt backup ===");
                    let report = dt_daemon::interfaces::cli::backup::create_backup().await?;
                    println!();
                    println!("备份已创建:");
                    println!("  位置:       {}", report.location.display());
                    println!(
                        "  Memgraph:   {} ({} 字节)",
                        if report.targets.memgraph {
                            "✓"
                        } else {
                            "✗"
                        },
                        report.targets.memgraph_size_bytes,
                    );
                    println!(
                        "  Qdrant:     {} ({} 字节)",
                        if report.targets.qdrant { "✓" } else { "✗" },
                        report.targets.qdrant_size_bytes,
                    );
                    println!(
                        "  SQLite:     {} ({} 字节)",
                        if report.targets.sqlite { "✓" } else { "✗" },
                        report.targets.sqlite_size_bytes,
                    );
                    println!("  耗时:       {:.1}s", report.duration_seconds,);
                }
                BackupAction::List => {
                    println!("=== dt backup list ===");
                    let entries = dt_daemon::interfaces::cli::backup::list_backups().await?;

                    if entries.is_empty() {
                        println!("未找到任何备份。");
                    } else {
                        println!(" {:<12}  {:<10}  {:>8}", "日期", "大小", "文件数");
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
                        println!("共 {} 个备份", entries.len());
                    }
                }
                BackupAction::Restore { date } => {
                    println!("=== dt backup restore {date} ===");
                    dt_daemon::interfaces::cli::backup::restore_backup(&date).await?;
                    println!("恢复完成。");
                }
                BackupAction::Verify { date } => {
                    println!("=== dt backup verify {date} ===");
                    let report =
                        dt_daemon::interfaces::cli::backup::verify_backup_files(&date).await?;

                    println!();
                    if report.all_valid {
                        println!("✅ 所有校验和有效!");
                    } else {
                        println!("❌ 检测到校验和不匹配:");
                    }

                    for file in &report.files {
                        let status = if file.valid { "✅" } else { "❌" };
                        println!(
                            "  {} {} — 期望: {}",
                            status,
                            file.file_name,
                            &file.expected[..32.min(file.expected.len())],
                        );
                    }
                    println!("  耗时: {:.1}s", report.duration_seconds,);
                }
            }

            return Ok(());
        }

        // ---- CLI 模式: dt schema init ----
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

        // ---- CLI 模式: dt health ----
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

        // ---- CLI 模式: dt memorize ----
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

        // ---- CLI 模式: dt event ----
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

        // ---- CLI 模式: dt learn ----
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

        // ---- CLI 模式: dt build ----
        Some(Commands::Build {
            path,
            name,
            file,
            full,
            no_pipeline,
            llm_backfill,
            source,
            config_chunks,
        }) => {
            if let Some(ref src) = source {
                if src == "knowledge" {
                    tracing::info!("dt build --source knowledge: 同步 KG 节点到向量库");
                    let graph = connect_graph().await;
                    let embed = connect_embed().await;
                    let vector = connect_vector().await;

                    if graph.is_none() || embed.is_none() || vector.is_none() {
                        eprintln!(
                            "错误: build --source knowledge 需要 Memgraph + Qdrant + embed 后端"
                        );
                        std::process::exit(1);
                    }

                    let graph = graph.unwrap();
                    let embed = embed.unwrap();

                    let queue = Arc::new(dt_daemon::application::sync::queue::VectorQueue::spawn(
                        embed.clone(),
                    ));

                    let incremental = !full;
                    dt_daemon::interfaces::cli::sync::handle_kg_sync(
                        incremental,
                        None,
                        config_chunks,
                        Some(graph),
                        Some(queue),
                    )
                    .await?;
                    return Ok(());
                } else {
                    eprintln!("错误: 未知的 source 类型 '{src}'。支持: knowledge");
                    std::process::exit(1);
                }
            }

            // 无任何参数 → 从 config.yaml 构建所有项目
            if path.is_none() && name.is_none() && file.is_none() {
                let memgraph = connect_memgraph().await;
                let graph: Option<Arc<dyn GraphRepository>> =
                    memgraph.map(|c| Arc::new(c) as Arc<dyn GraphRepository>);

                let embed = connect_embed().await;
                let vector = connect_vector().await;
                let snapshot = connect_snapshot().await;

                let Some(cfg) = load_config() else {
                    eprintln!("错误: 未找到 config.yaml");
                    std::process::exit(1);
                };

                let projects = resolve_project_paths(&cfg);
                if projects.is_empty() {
                    eprintln!("错误: config.yaml 中未配置任何项目");
                    std::process::exit(1);
                }

                let batch_config = cfg.batch.clone();
                let pipeline = !no_pipeline;
                let scan_config = scan_config_from(&cfg);
                dt_daemon::interfaces::cli::build::handle_build_all(
                    projects,
                    full,
                    pipeline,
                    llm_backfill,
                    graph,
                    vector,
                    embed,
                    snapshot,
                    batch_config,
                    scan_config,
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

            // 指定 --name 时，从 config.yaml 解析实际路径。
            // 例如 --name order-center → /data/aflmProjects/aflm/uvp-order-center
            let actual_path = if let Some(ref n) = name {
                let cfg = load_config();
                cfg.as_ref()
                    .and_then(|c| {
                        resolve_project_paths(c)
                            .into_iter()
                            .find(|(proj_name, _)| proj_name == n)
                            .map(|(_, proj_path)| proj_path)
                    })
                    .unwrap_or_else(|| path.expect("必须提供 --path"))
            } else {
                path.expect("必须提供 --path")
            };

            let batch_config = load_config().map(|c| c.batch).unwrap_or_default();
            let scan_config = load_config()
                .map(|c| scan_config_from(&c))
                .unwrap_or_default();

            let pipeline = !no_pipeline;
            dt_daemon::interfaces::cli::build::handle_build(
                actual_path,
                name,
                file,
                full,
                pipeline,
                llm_backfill,
                graph,
                vector,
                embed,
                snapshot,
                batch_config,
                scan_config,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt search ----
        Some(Commands::Search {
            query,
            world,
            limit,
            json,
            project,
            file_type,
            content_type,
            show_content,
        }) => {
            let graph = connect_graph().await;
            let vector = connect_vector().await;
            dt_daemon::interfaces::cli::build::handle_search(
                query,
                world,
                limit,
                json,
                project,
                file_type,
                content_type,
                show_content,
                graph,
                vector,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt sense ----
        Some(Commands::Sense { path, json }) => {
            let cfg = load_config();
            let projects = cfg.as_ref().map(resolve_project_paths).unwrap_or_default();
            let graph = connect_graph().await;
            let vector = connect_vector().await;
            let snapshot = connect_snapshot().await;
            let ignored = dirs_like_home_config(".config/digital-twin/ignored_dirs.yaml");
            dt_daemon::interfaces::cli::sense::handle_sense(
                path, json, projects, graph, vector, snapshot, ignored,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt kg-sync（已移除，友好提示）----
        Some(Commands::KgSync { .. }) => {
            eprintln!("⚠️  dt kg-sync 已移除（2026-08-12）。请改用: dt build --source knowledge");
            return Ok(());
        }

        // ---- CLI 模式: dt jcli ----
        // ---- 无命令：打印帮助 ----
        None => {
            use clap::CommandFactory;
            Cli::command().print_help().ok();
            println!();
            return Ok(());
        }
    }

    Ok(())
}
