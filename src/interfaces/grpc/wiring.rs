//! 守护进程的依赖注入（DI）装配。
//!
//! 创建并装配所有服务实现，并用 [`WriteCoordinator`] 锁包裹，
//! 以防止三个写入源（OpenCode hooks、手动构建、cron 同步）
//! 之间的并发写入冲突。
//!
//! 后端连接（Memgraph、Qdrant）在 wire 时读取 `config.yaml` 惰性创建。
//! 若任一后端不可达，[`AppComponents`] 中对应的字段被置为 `None`——
//! 调用方（例如 gRPC 服务器）必须回退到 no-op 实现。
//! SiliconFlow API 客户端也在此连接；若不可用则回退到
//! [`NoopEmbedService`]（零向量嵌入）。

use crate::application::build::service::BuildServiceImpl;
use crate::application::hooks::engine::HookEngine;
use crate::application::hooks::registry::HookRegistry;
use crate::domain::traits::{
    BuildService, EmbedService, GraphRepository, SnapshotRepository, VectorRepository,
};
use crate::domain::types::BatchConfig;
use crate::infrastructure::parser::ParserRegistry;
use crate::shared::coordinator::{CoordinatedBuildService, WriteCoordinator};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// config.yaml 布局（库内部，与 main.rs 一致）
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
        Self {
            path: default_sqlite_path(),
        }
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

/// DI 容器装配的所有顶层应用组件。
///
/// 持有 `Arc<WriteCoordinator>`，使所有服务共享同一个
/// 协调器实例。
pub struct AppComponents {
    /// 协调后的构建服务（应用了文件/实体/全局锁）。
    pub build: Arc<dyn BuildService>,
    /// 共享的写协调器。暴露给类似 cron 的消费者，它们
    /// 在开始同步前需要调用 `has_active_writes()`。
    pub coordinator: Arc<WriteCoordinator>,
    /// Memgraph 图谱仓库（连接失败时为 None）。
    pub graph: Option<Arc<dyn GraphRepository>>,
    /// Qdrant 向量仓库（连接失败时为 None）。
    pub vector: Option<Arc<dyn VectorRepository>>,
    /// SiliconFlow embed 服务（不可用时为 None——调用方自行回退）。
    pub embed: Option<Arc<dyn EmbedService>>,
    /// 事件驱动的知识图谱写入 hook 引擎。
    /// 若 event-hooks.yaml 缺失或格式错误则为 None。
    pub hook_engine: Option<Arc<HookEngine>>,
}

// ---------------------------------------------------------------------------
// DI 装配
// ---------------------------------------------------------------------------

/// 装配所有应用组件。
///
/// 读取 `config.yaml`（通过常规搜索路径回退），并尝试连接
/// Memgraph 与 Qdrant。若任一后端不可达，对应的 `AppComponents`
/// 字段保持为 `None`，调用方可回退到 no-op 实现。
pub async fn wire() -> AppComponents {
    // ---- 写协调器 ----
    // 使用 `with_global_lock()`，使全量构建与所有
    // 文件级、实体级写入方串行化。
    let coordinator = Arc::new(WriteCoordinator::with_global_lock());

    // ---- 存储后端（真实连接，失败回退为 None） ----
    let graph = connect_graph().await;
    let vector = connect_vector().await;
    let embed = connect_embed().await;

    // 快照后端是可选的；流水线会自行适配。
    let snapshot: Option<Arc<dyn SnapshotRepository>> = None;

    // ---- 解析器注册表（所有语言解析器） ----
    let parser_registry = Arc::new(ParserRegistry::new());

    // ---- 构建服务（内部、未协调） ----
    let batch_config = BatchConfig::default();
    let build_inner = Arc::new(BuildServiceImpl::new(
        parser_registry,
        graph.clone(),
        vector.clone(),
        snapshot,
        embed.clone(),
        None,  // siliconflow——尚未通过 gRPC 接入
        false, // gRPC 构建默认增量
        batch_config,
        false, // skip_embed
    ));

    // ---- 用 WriteCoordinator 包裹 ----
    let build = Arc::new(CoordinatedBuildService::new(
        build_inner,
        Arc::clone(&coordinator),
    )) as Arc<dyn BuildService>;

    // ---- Hook 引擎（事件驱动副作用） ----
    let hook_engine = graph.as_ref().and_then(|g| {
        let path = dirs_like_home_config(".config/digital-twin/event-hooks.yaml")?;
        match HookRegistry::from_file(&path) {
            Ok(registry) => {
                tracing::info!("从 {} 加载了 HookRegistry", path.display());
                let registry = Arc::new(registry);
                Some(Arc::new(HookEngine::new(registry, g.clone())))
            }
            Err(e) => {
                tracing::warn!("加载 {} 失败: {e}", path.display());
                None
            }
        }
    });

    tracing::info!(
        "DI 装配完成: 已装配 1 个服务 (graph={}, vector={}, hooks={})",
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
// 配置辅助函数（与 main.rs 一致）
// ---------------------------------------------------------------------------

/// 从 `~/.config/digital-twin/config.yaml` 加载配置。
fn load_config() -> Option<DaemonConfig> {
    let path = dirs_like_home_config(".config/digital-twin/config.yaml")?;
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_yaml::from_str::<DaemonConfig>(&content) {
            Ok(cfg) => {
                tracing::info!("从 {} 加载了配置", path.display());
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!("解析 {} 失败: {e}", path.display());
                None
            }
        },
        Err(e) => {
            tracing::warn!("读取 {} 失败: {e}", path.display());
            None
        }
    }
}

/// 解析 `~/.config/...` 路径，不引入 `dirs` crate。
fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(suffix))
}

/// 从 config.yaml `services.graph` 解析 Memgraph Bolt URI。
///
/// 若 `url` 已设置但使用 HTTP 协议（例如 `http://localhost:7474`），
/// 则转换为 Bolt（`bolt://localhost:7687`）。若未配置 URL，
/// 返回默认值 `bolt://localhost:7687`。
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

/// 使用 config.yaml（或合理默认值）中的值连接 Memgraph。
async fn connect_graph() -> Option<Arc<dyn GraphRepository>> {
    let cfg = load_config()?;
    let bolt_url = resolve_graph_bolt_url(&cfg.services.graph);
    let user = cfg.services.graph.user.as_deref().unwrap_or("memgraph");
    let password = cfg.services.graph.password.as_deref().unwrap_or("");

    match crate::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password).await
    {
        Ok(client) => {
            tracing::info!("已连接 Memgraph: {}", bolt_url);
            Some(Arc::new(client) as Arc<dyn GraphRepository>)
        }
        Err(e) => {
            tracing::warn!("Memgraph 连接失败（将使用 noop）: {}", e);
            None
        }
    }
}

/// 使用 config.yaml（或合理默认值）连接 Qdrant 向量库。
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
            tracing::info!("已连接 Qdrant: {}", qdrant_uri);
            let repo = crate::infrastructure::qdrant::QdrantRepo::new(client);
            Some(Arc::new(repo) as Arc<dyn VectorRepository>)
        }
        Err(e) => {
            tracing::warn!("Qdrant 连接失败（将使用 noop）: {}", e);
            None
        }
    }
}

/// 使用 provider 路由连接嵌入服务。
///
/// 从环境变量读取配置并带合理默认值，构建同时支持
/// SiliconFlow 与 XInference 的 [`EmbedProviderRouter`]。
async fn connect_embed() -> Option<Arc<dyn EmbedService>> {
    use crate::infrastructure::embedder::ProviderConfig;

    let provider_cfg = ProviderConfig {
        siliconflow_url: std::env::var("SILICONFLOW_BASE_URL")
            .unwrap_or_else(|_| "https://api.siliconflow.cn/v1".into()),
        siliconflow_api_key: std::env::var("SILICONFLOW_API_KEY").unwrap_or_default(),
        siliconflow_model_embed: std::env::var("SILICONFLOW_EMBED_MODEL")
            .unwrap_or_else(|_| "BAAI/bge-m3".into()),
        siliconflow_model_reranker: std::env::var("SILICONFLOW_RERANKER_MODEL")
            .unwrap_or_else(|_| "BAAI/bge-reranker-v2-m3".into()),
        siliconflow_model_llm: std::env::var("SILICONFLOW_LLM_MODEL").unwrap_or_default(),
        siliconflow_max_concurrent: 20,
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
    tracing::info!("已创建 Embed provider 路由（siliconflow，来自环境变量）");
    Some(router)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wire_creates_components() {
        let components = wire().await;
        assert!(!components.coordinator.has_active_writes());
        // 在 CI / 无 config.yaml 的测试环境中，graph 和 vector
        // 会是 None——但结构体本身始终应构建成功。
    }
}
