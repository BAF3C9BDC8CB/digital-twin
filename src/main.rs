//! Digital Twin V2 守护进程 — 组合根与 gRPC 服务器。
//!
//! # 双模式
//!
//! 该守护进程二进制支持三种运行模式：
//!
//! 1. **服务器模式**（默认）— 启动 gRPC 服务器。
//! 2. **CLI 模式** — 以已识别的子命令（如 `build`、`search`）调用时，执行命令并退出。

use clap::{Parser, Subcommand};
use digital_twin::domain::traits::{
    EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use std::path::PathBuf;
use std::sync::Arc;

// ---- CLI 定义 ----

#[derive(Parser)]
#[command(name = "dt", version = env!("CARGO_PKG_VERSION"), about = "Digital Twin 命令行工具")]
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
    ///       dt memorize --action delete <entity-id> <details>   # 删除记忆（图+向量）
    ///       dt memorize --action update <entity-id> <details> --supersede <old-id>  # 版本化更新
    Memorize {
        /// 知识类型: Decision | KnowledgeAdded | Environment | Dependencies。
        knowledge_type: String,

        /// 实体的唯一标识符。
        entity_id: String,

        /// 人类可读的详情，key: value 格式（分号分隔）。
        details: String,

        /// 操作类型: write(默认) | delete | update/supersede。
        #[arg(long = "action")]
        action: Option<String>,

        /// 版本化更新时被取代的旧实体 ID。
        #[arg(long = "supersede")]
        supersede: Option<String>,

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

    /// 查看 digital-twin 统一日志（纯文本格式，tail/grep 语义）。
    ///
    /// 用法: dt logs [-f] [-k 关键词] [-n 行数]
    ///   dt logs         — 打印最后 50 行
    ///   dt logs -f      — 实时跟随（类似 tail -f）
    ///   dt logs -k 搜索 — 只显示包含"搜索"的行（-k 可重复）
    Logs {
        /// 实时跟随输出（类似 tail -f），Ctrl-C 退出。
        #[arg(long = "follow", short = 'f')]
        follow: bool,

        /// 只显示包含关键词的行（可重复指定多个，取并集）。
        #[arg(long = "keyword", short = 'k')]
        keywords: Vec<String>,

        /// 显示的末尾行数（默认 50）。
        #[arg(long = "lines", short = 'n', default_value_t = 50)]
        lines: usize,
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

    /// 智能路由搜索（统一检索入口）。
    ///
    /// 用法: dt router <query> [--world all|code|knowledge|doc|config|memory] [--limit 10]
    ///       [--json] [--project P] [--filter BOOL] [--threshold 0.6] [--file-type F]
    ///       [--content-type T] [--show-content] [--explain]
    /// 统一 dt search 与 dt router：自动意图路由 + L0 闲聊/无锚点拦截 + LLM 过滤，
    /// 并保留显式 --file-type/--content-type/--show-content 精确过滤。
    Router {
        /// 搜索查询字符串。
        query: String,

        /// 搜索世界限定。
        #[arg(long = "world", default_value = "all")]
        world: String,

        /// 结果数量上限。
        #[arg(long = "limit", default_value = "10")]
        limit: usize,

        /// 输出纯 JSON。
        #[arg(long = "json")]
        json: bool,

        /// 限定到某个项目。
        #[arg(long = "project", short = 'p')]
        project: Option<String>,

        /// 启用 LLM 智能过滤。
        /// 未指定时读取配置 `kg_router.result_filter.enabled`。
        #[arg(long = "filter", value_name = "BOOL")]
        enable_filter: Option<bool>,

        /// 过滤相关性阈值（0.0-1.0）。
        #[arg(long = "threshold", default_value = "0.6")]
        filter_threshold: f32,

        /// 显示路由决策过程。
        #[arg(long = "explain")]
        explain: bool,

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
// ---- 连接薄转发: 实现已抽取到 digital_twin::runtime ----
use digital_twin::application::hooks::HookEngine;
use digital_twin::application::sync::batch::SyncAccumulator;
use digital_twin::application::sync::kg_bridge::KgBridge;
use digital_twin::application::sync::queue::VectorQueue;
use digital_twin::domain::types::ScanConfig;
use digital_twin::runtime::DaemonConfig;

fn load_config() -> Option<DaemonConfig> {
    digital_twin::runtime::load_config()
}

fn scan_config_from(cfg: &DaemonConfig) -> ScanConfig {
    digital_twin::runtime::scan_config_from(cfg)
}

fn resolve_project_paths(cfg: &DaemonConfig) -> Vec<(String, PathBuf)> {
    digital_twin::runtime::resolve_project_paths(cfg)
}

fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    digital_twin::runtime::dirs_like_home_config(suffix)
}

async fn connect_graph() -> Option<Arc<dyn GraphRepository>> {
    digital_twin::runtime::connect_graph().await
}

async fn connect_hook_engine() -> Option<Arc<HookEngine>> {
    digital_twin::runtime::connect_hook_engine().await
}

async fn connect_memgraph() -> Option<digital_twin::infrastructure::memgraph::MemgraphClient> {
    digital_twin::runtime::connect_memgraph().await
}

async fn connect_vector() -> Option<Arc<dyn VectorRepository>> {
    digital_twin::runtime::connect_vector().await
}

async fn connect_embed() -> Option<Arc<dyn EmbedService>> {
    digital_twin::runtime::connect_embed().await
}

async fn build_kg_bridge(
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    queue: Option<Arc<VectorQueue>>,
) -> Option<Arc<KgBridge>> {
    digital_twin::runtime::build_kg_bridge(graph, vector, queue).await
}

async fn build_sync_acc(
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    queue: Option<Arc<VectorQueue>>,
) -> Option<Arc<SyncAccumulator>> {
    digital_twin::runtime::build_sync_acc(graph, vector, queue).await
}

async fn connect_snapshot() -> Option<Arc<dyn SnapshotRepository>> {
    digital_twin::runtime::connect_snapshot().await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 通过 dt-log 初始化统一日志（JSON → 文件 + stderr 兜底；异步写入）
    // guard 必须存活到 main 结束——drop 时冲刷日志队列，保证不丢。
    let _log_guard = digital_twin::shared::logging::init::init_logging()?;

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
            digital_twin::interfaces::cli::cleanup::run_clean(
                confirm,
                memgraph
                    .as_ref()
                    .map(|c| c as &dyn digital_twin::domain::traits::GraphRepository),
                vector
                    .as_deref()
                    .map(|c| c as &dyn digital_twin::domain::traits::VectorRepository),
                snapshot
                    .as_deref()
                    .map(|c| c as &dyn digital_twin::domain::traits::SnapshotRepository),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt backup ----
        Some(Commands::Backup { action }) => {
            match action.unwrap_or(BackupAction::Create) {
                BackupAction::Create => {
                    println!("=== dt backup ===");
                    let report = digital_twin::interfaces::cli::backup::create_backup().await?;
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
                    let entries = digital_twin::interfaces::cli::backup::list_backups().await?;

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
                    digital_twin::interfaces::cli::backup::restore_backup(&date).await?;
                    println!("恢复完成。");
                }
                BackupAction::Verify { date } => {
                    println!("=== dt backup verify {date} ===");
                    let report =
                        digital_twin::interfaces::cli::backup::verify_backup_files(&date).await?;

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
            digital_twin::interfaces::cli::cleanup::run_schema_init(
                memgraph
                    .as_ref()
                    .map(|c| c as &dyn digital_twin::domain::traits::GraphRepository),
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt router (智能路由搜索, 统一检索入口) ----
        Some(Commands::Router {
            query,
            world,
            limit,
            json,
            project,
            enable_filter,
            filter_threshold,
            explain,
            file_type,
            content_type,
            show_content,
        }) => {
            digital_twin::interfaces::cli::router::handle_router_search(
                &query,
                &world,
                limit,
                json,
                &project,
                enable_filter,
                filter_threshold,
                explain,
                file_type,
                content_type,
                show_content,
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
            digital_twin::interfaces::cli::cleanup::run_health(
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
            action,
            supersede,
            entity_type,
            project,
        }) => {
            let graph = connect_graph().await;
            let embed = connect_embed().await;
            let vector = connect_vector().await;
            let queue = embed.clone().map(|e| {
                Arc::new(digital_twin::application::sync::queue::VectorQueue::spawn(
                    e,
                ))
            });
            let sync_acc = build_sync_acc(graph.clone(), vector.clone(), queue).await;
            digital_twin::interfaces::cli::memorize::handle_memorize(
                knowledge_type,
                entity_id,
                entity_type,
                project,
                details,
                graph,
                sync_acc,
                action,
                supersede,
                vector,
                embed,
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
            let queue = embed.map(|e| {
                Arc::new(digital_twin::application::sync::queue::VectorQueue::spawn(
                    e,
                ))
            });
            let kg_bridge = build_kg_bridge(graph, vector, queue).await;
            digital_twin::interfaces::cli::event::handle_event(
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
            let queue = embed.map(|e| {
                Arc::new(digital_twin::application::sync::queue::VectorQueue::spawn(
                    e,
                ))
            });
            let sync_acc = build_sync_acc(graph.clone(), vector, queue).await;
            digital_twin::interfaces::cli::learn::handle_learn(
                task, entities, pattern, pitfalls, decisions, thread_id, success, project, graph,
                sync_acc,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt build ----
        Some(Commands::Build {
            path,
            mut name,
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

                    let queue = Arc::new(
                        digital_twin::application::sync::queue::VectorQueue::spawn(embed.clone()),
                    );

                    let incremental = !full;
                    digital_twin::interfaces::cli::sync::handle_kg_sync(
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
                digital_twin::interfaces::cli::build::handle_build_all(
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
            } else if let Some(ref f) = file {
                // --file 单独使用：从 config.yaml 匹配文件所属项目根，
                // 使 `dt build --file <绝对路径>` 无需显式 --path/--name
                // （MCP dt_build 对文件路径即如此调用；此前会 panic）。
                let cfg = load_config();
                let projects = cfg.as_ref().map(resolve_project_paths).unwrap_or_default();
                let file_path = PathBuf::from(f);
                let matched = projects
                    .into_iter()
                    .filter(|(_, p)| file_path.starts_with(p))
                    .max_by_key(|(_, p)| p.components().count());
                match matched {
                    Some((proj_name, proj_path)) => {
                        name = Some(proj_name);
                        proj_path
                    }
                    None => path.expect("必须提供 --path"),
                }
            } else {
                path.expect("必须提供 --path")
            };

            let batch_config = load_config().map(|c| c.batch).unwrap_or_default();
            let scan_config = load_config()
                .map(|c| scan_config_from(&c))
                .unwrap_or_default();

            let pipeline = !no_pipeline;
            digital_twin::interfaces::cli::build::handle_build(
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
            digital_twin::interfaces::cli::build::handle_search(
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
            digital_twin::interfaces::cli::sense::handle_sense(
                path, json, projects, graph, vector, snapshot, ignored,
            )
            .await?;
            return Ok(());
        }

        // ---- CLI 模式: dt logs ----
        Some(Commands::Logs {
            follow,
            keywords,
            lines,
        }) => {
            digital_twin::interfaces::cli::logs::handle_logs(follow, &keywords, lines)?;
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
