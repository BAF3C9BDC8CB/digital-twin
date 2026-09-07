//! 运行时组装 — 连接后端服务(Memgraph/Qdrant/Embed/SQLite/Hooks)。
//!
//! 原为 `main.rs` 的私有函数; 抽取为库模块后, CLI 与 `dt-mcp`
//! 共用同一套连接逻辑, 避免两处维护。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::hooks::HookEngine;
use crate::application::pipeline::config::PipelineConfig;
use crate::application::sync::batch::SyncAccumulator;
use crate::application::sync::kg_bridge::KgBridge;
use crate::application::sync::queue::VectorQueue;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::{BatchConfig, ScanConfig};
use serde::Deserialize;

// ---- 配置结构(config.yaml) --------------------------------------

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    /// 被索引/感知的代码根清单（root = 根别名 + 磁盘路径）。
    #[serde(default)]
    pub roots: Vec<serde_yaml::Value>,
    #[serde(default)]
    pub services: ServiceConfig,
    #[serde(default)]
    pub batch: BatchConfig,
    #[serde(default)]
    pub scanner: ScannerFileConfig,
    /// 日志配置（`logging.level` 在配置文件中自由选择 debug/info 等）。
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// config.yaml 的 `logging` 段 —— 日志级别控制。
///
/// 级别值可为任意 tracing 级别（`debug` / `info` / `warn` / `error`），
/// 也接受带模块前缀的 EnvFilter 写法（如 `digital_twin=debug`）。
/// 读取与优先级见 `shared::logging::init::init_logging`。
#[derive(Debug, Deserialize, Default)]
pub struct LoggingConfig {
    /// 日志级别（文件层主过滤器）。缺省时回落 `RUST_LOG` / `DT_LOG_LEVEL` / 内置默认。
    #[serde(default)]
    pub level: Option<String>,
}

/// config.yaml 的 `scanner` 段 / 独立 ignore.yaml —— 扫描忽略规则。
///
/// 推荐写法是统一 `ignore` 列表（文件与目录通吃，条目可为精确名、
/// 相对路径或含 `*` / `?` / `**` 的 glob）；`ignore_dirs` / `ignore_files` /
/// `ignore_ext` 三段式写法保留以兼容旧配置，读取后均归一化进 `ScanConfig`。
#[derive(Debug, Deserialize, Default)]
pub struct ScannerFileConfig {
    /// 统一忽略列表（新式）：精确名 / 相对路径 / glob 通配均可。
    #[serde(default)]
    pub ignore: Vec<String>,
    /// （旧式）忽略的目录：单段目录名或相对路径前缀。
    #[serde(default)]
    pub ignore_dirs: Vec<String>,
    /// （旧式）忽略的文件名（精确匹配）。
    #[serde(default)]
    pub ignore_files: Vec<String>,
    /// （旧式）忽略的扩展名（可带 `.` 前缀）。
    #[serde(default)]
    pub ignore_ext: Vec<String>,
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
    pub sqlite: SqliteConfig,
}

#[derive(Debug, Deserialize)]
pub struct GraphDbConfig {
    pub url: Option<String>,
    /// 环境变量名：从环境变量读取 url（优先于 url 字段）。
    #[serde(default)]
    pub url_env: Option<String>,
    pub user: Option<String>,
    /// 环境变量名：从环境变量读取密码（优先于 password 字段）。
    #[serde(default)]
    pub password_env: Option<String>,
    pub password: Option<String>,
}

impl GraphDbConfig {
    /// 生效的 Bolt/HTTP url：`url_env` 指向的环境变量 > `url`。
    pub fn effective_url(&self) -> Option<String> {
        if let Some(name) = self.url_env.as_deref().filter(|n| !n.trim().is_empty()) {
            return std::env::var(name.trim()).ok().filter(|v| !v.trim().is_empty());
        }
        self.url.clone()
    }

    /// 生效的密码：`password_env` 指向的环境变量 > `password`。
    pub fn effective_password(&self) -> String {
        if let Some(name) = self.password_env.as_deref().filter(|n| !n.trim().is_empty()) {
            return std::env::var(name.trim()).unwrap_or_default();
        }
        self.password.clone().unwrap_or_default()
    }
}

impl Default for GraphDbConfig {
    fn default() -> Self {
        Self {
            url: Some("bolt://localhost:7687".to_string()),
            url_env: None,
            user: Some("memgraph".to_string()),
            password_env: None,
            password: Some(String::new()),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct QdrantServiceConfig {
    #[serde(default)]
    pub url: Option<String>,
    /// 环境变量名：从环境变量读取 url（优先于 url 字段）。
    #[serde(default)]
    pub url_env: Option<String>,
}

impl QdrantServiceConfig {
    /// 生效的 gRPC url：`url_env` 指向的环境变量 > `url`。
    pub fn effective_url(&self) -> Option<String> {
        if let Some(name) = self.url_env.as_deref().filter(|n| !n.trim().is_empty()) {
            return std::env::var(name.trim()).ok().filter(|v| !v.trim().is_empty());
        }
        self.url.clone()
    }
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

// ---- 配置加载与解析 ----------------------------------------------

/// 解析 `~/.config/...`，无需引入 `dirs` crate。
pub fn dirs_like_home_config(suffix: &str) -> Option<PathBuf> {
    let home = crate::shared::home_dir()?;
    Some(home.join(suffix))
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

/// 将 config.yaml 的 `scanner` 段（若存在）与独立忽略文件
/// `~/.config/digital-twin/ignore.yaml`（若存在）合并为 `ScanConfig`。
///
/// 用户配置的列表与内置默认值**合并**（而非覆盖），确保常见噪音目录
/// 始终被忽略；`max_file_size` 未配置时用默认 500KB。
pub fn scan_config_from(cfg: &DaemonConfig) -> ScanConfig {
    let mut sc = ScanConfig::default();

    // 1. config.yaml 内联 scanner 段（旧式；已不推荐，保留兼容）
    merge_scanner(&mut sc, &cfg.scanner);

    // 2. 独立忽略文件 ~/.config/digital-twin/ignore.yaml（新式）
    if let Some(path) = dirs_like_home_config(".config/digital-twin/ignore.yaml") {
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_yaml::from_str::<ScannerFileConfig>(&content) {
                    Ok(file_cfg) => {
                        tracing::debug!("已加载忽略规则: {}", path.display());
                        merge_scanner(&mut sc, &file_cfg);
                    }
                    Err(e) => tracing::warn!("解析忽略规则失败 {}: {e}", path.display()),
                },
                Err(e) => tracing::warn!("读取忽略规则失败 {}: {e}", path.display()),
            }
        }
    }
    sc
}

/// 将一个 scanner 规则块并入 `ScanConfig`（列表取并集；max_file_size 取首个非零值）。
///
/// 归一化规则（新 `ignore` 条目直接进统一模型；旧三段式翻译）：
/// - `ignore`：条目原样加入（含通配 → glob 桶；纯名/路径 → 精确名桶）
/// - `ignore_dirs` / `ignore_files`：加入精确名桶
/// - `ignore_ext`：`.class` → `*.class` 通配条目
fn merge_scanner(sc: &mut ScanConfig, scanner: &ScannerFileConfig) {
    for entry in &scanner.ignore {
        sc.add_ignore(entry);
    }
    for d in &scanner.ignore_dirs {
        if !d.is_empty() {
            sc.add_ignore(d);
        }
    }
    for f in &scanner.ignore_files {
        if !f.is_empty() {
            sc.add_ignore(f);
        }
    }
    for e in &scanner.ignore_ext {
        let e = e.trim();
        if !e.is_empty() {
            // 归一化为 glob：`jpg` / `.jpg` → `*.jpg`
            let dot = if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            };
            if !sc.ignore_globs.iter().any(|g| *g == format!("*{dot}")) {
                sc.ignore_globs.push(format!("*{dot}"));
            }
        }
    }
    if let Some(m) = scanner.max_file_size {
        if m > 0 && sc.max_file_size == ScanConfig::default().max_file_size {
            sc.max_file_size = m;
        }
    }
}

/// 将 config.yaml 的 `roots` 段扁平化为 `(别名, 绝对路径)` 对。
///
/// 支持的写法（同一列表可混用）：
/// - 纯字符串：`- /data/myProject/digital-twin-v2`（别名 = 目录最后一段）
/// - 单根映射：`- label-center: /data/aflmProjects/aflm/uvp-label-center`
/// - 分组映射（共享 base 前缀，条目为相对路径；单条目可省略相对路径）：
///   ```yaml
///   - base: /data/aflmProjects/aflm
///     items: [archive-api, "copartner-h5: copartner/copartner-h5"]
///   ```
pub fn resolve_roots(cfg: &DaemonConfig) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for item in &cfg.roots {
        match item {
            serde_yaml::Value::String(s) => {
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                let path = PathBuf::from(s);
                let alias = default_alias(&path);
                out.push((alias, path));
            }
            serde_yaml::Value::Mapping(m) => {
                // 分组形态：base + items
                if let (Some(base), Some(items)) = (
                    m.get(serde_yaml::Value::String("base".into())),
                    m.get(serde_yaml::Value::String("items".into())),
                ) {
                    let base = base.as_str().unwrap_or("");
                    let base_path = PathBuf::from(base);
                    if let Some(items) = items.as_sequence() {
                        for it in items {
                            match it {
                                serde_yaml::Value::String(s) => {
                                    let s = s.trim();
                                    if s.is_empty() {
                                        continue;
                                    }
                                    // "别名: 相对路径" 或纯相对路径
                                    if let Some((alias, rel)) = s.split_once(':') {
                                        let alias = alias.trim();
                                        let rel = rel.trim();
                                        out.push((alias.to_string(), base_path.join(rel)));
                                    } else {
                                        let alias = default_alias(&base_path.join(s));
                                        out.push((alias, base_path.join(s)));
                                    }
                                }
                                serde_yaml::Value::Mapping(it_m) => {
                                    for (k, v) in it_m {
                                        let alias = k.as_str().unwrap_or("").to_string();
                                        let rel = v.as_str().unwrap_or(&alias).to_string();
                                        out.push((alias, base_path.join(rel)));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    continue;
                }
                // 单根映射：别名 → 绝对/相对路径
                for (k, v) in m {
                    let alias = k.as_str().unwrap_or("").to_string();
                    let rel = v.as_str().unwrap_or(&alias).to_string();
                    out.push((alias, PathBuf::from(rel)));
                }
            }
            _ => {}
        }
    }
    out
}

/// 根别名默认取路径最后一段目录名。
fn default_alias(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// 从 config.yaml `services.graph` 解析 Memgraph Bolt URI。
pub fn resolve_graph_bolt_url(cfg: &GraphDbConfig) -> String {
    match &cfg.effective_url() {
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
    let password = cfg.services.graph.effective_password();
    let password = password.as_str();

    match crate::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password).await
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
    let password = cfg.services.graph.effective_password();
    let password = password.as_str();

    match crate::infrastructure::memgraph::MemgraphClient::connect(&bolt_url, user, password).await
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
    let qdrant_uri_owned = cfg.services.qdrant.effective_url();
    let qdrant_uri = qdrant_uri_owned
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

/// 连接 Embed 路由(从 pipeline.yaml providers 端点池构建)。
///
/// 2026-09-06 起：embed 走 `providers.embed` 端点池（多厂商 × 多模型，
/// 失败自动顺延）；旧 `providers.siliconflow` 单块已移除。
pub async fn connect_embed() -> Option<Arc<dyn EmbedService>> {
    let pipeline_cfg = PipelineConfig::load().ok()?;
    let pcfg = pipeline_cfg.providers.as_ref()?;
    if pcfg.embed.is_empty() {
        tracing::warn!("pipeline.yaml providers.embed 为空，跳过 embed 服务");
        return None;
    }

    let svcs = crate::infrastructure::embedder::build_pooled_services(&pipeline_cfg);
    svcs.embed
}

pub async fn build_kg_bridge(
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    queue: Option<Arc<VectorQueue>>,
) -> Option<Arc<KgBridge>> {
    let g = graph?;
    let embed = queue.as_ref()?.embed_service().clone();
    let v = vector.unwrap_or_else(|| {
        Arc::new(crate::infrastructure::qdrant::repo::NoopVectorRepo) as Arc<dyn VectorRepository>
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
    /// 解析后的代码根清单：(别名, 绝对路径)。
    pub roots: Vec<(String, PathBuf)>,
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

        let queue = embed.clone().map(|e| Arc::new(VectorQueue::spawn(e)));
        let kg_bridge = build_kg_bridge(graph.clone(), vector.clone(), queue.clone()).await;
        let sync_acc = build_sync_acc(graph.clone(), vector.clone(), queue.clone()).await;

        let cfg = load_config();
        let roots = cfg.as_ref().map(resolve_roots).unwrap_or_default();
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
            roots,
            batch_config,
            scan_config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_roots_flat_string_uses_dir_name_as_alias() {
        let cfg: DaemonConfig = serde_yaml::from_str(
            r#"
roots:
- /data/myProject/digital-twin-v2
"#,
        )
        .unwrap();
        let roots = resolve_roots(&cfg);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].0, "digital-twin-v2");
        assert_eq!(roots[0].1, PathBuf::from("/data/myProject/digital-twin-v2"));
    }

    #[test]
    fn resolve_roots_single_mapping_alias_to_path() {
        let cfg: DaemonConfig = serde_yaml::from_str(
            r#"
roots:
- label-center: /data/aflmProjects/aflm/uvp-label-center
"#,
        )
        .unwrap();
        let roots = resolve_roots(&cfg);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].0, "label-center");
        assert_eq!(
            roots[0].1,
            PathBuf::from("/data/aflmProjects/aflm/uvp-label-center")
        );
    }

    #[test]
    fn resolve_roots_group_base_items() {
        let cfg: DaemonConfig = serde_yaml::from_str(
            r#"
roots:
- base: /data/aflmProjects/aflm
  items:
  - archive-api
  - copartner-h5: copartner/copartner-h5
"#,
        )
        .unwrap();
        let roots = resolve_roots(&cfg);
        assert_eq!(roots.len(), 2);
        // 纯相对路径：别名 = 路径最后一段
        assert_eq!(roots[0].0, "archive-api");
        assert_eq!(
            roots[0].1,
            PathBuf::from("/data/aflmProjects/aflm/archive-api")
        );
        // 显式别名
        assert_eq!(roots[1].0, "copartner-h5");
        assert_eq!(
            roots[1].1,
            PathBuf::from("/data/aflmProjects/aflm/copartner/copartner-h5")
        );
    }

    #[test]
    fn resolve_roots_mixed_forms() {
        let cfg: DaemonConfig = serde_yaml::from_str(
            r#"
roots:
- /data/doc/软件
- boss: /data/aflmProjects/aflm/boss/boss
- base: /data/myProject
  items:
  - digital-twin-v2
  - jcli: jenkins-cli-rs
"#,
        )
        .unwrap();
        let roots = resolve_roots(&cfg);
        assert_eq!(roots.len(), 4);
        assert_eq!(roots[0].0, "软件");
        assert_eq!(roots[1].0, "boss");
        assert_eq!(roots[2].0, "digital-twin-v2");
        assert_eq!(roots[2].1, PathBuf::from("/data/myProject/digital-twin-v2"));
        assert_eq!(roots[3].0, "jcli");
        assert_eq!(roots[3].1, PathBuf::from("/data/myProject/jenkins-cli-rs"));
    }

    #[test]
    fn scanner_merge_takes_union() {
        let sc = ScanConfig::default();
        let base_names = sc.ignore_names.len();
        let base_globs = sc.ignore_globs.len();

        let inline = ScannerFileConfig {
            ignore: vec![],
            ignore_dirs: vec!["node_modules".into(), "custom_dir".into()],
            ignore_ext: vec![".custom".into()],
            ignore_files: vec!["custom.lock".into()],
            max_file_size: Some(1024),
        };
        let mut merged = ScanConfig::default();
        merge_scanner(&mut merged, &inline);

        // 并集：内置默认仍在，新增条目已加入
        assert_eq!(merged.ignore_names.len(), base_names + 2); // custom_dir + custom.lock（node_modules 重复）
        assert_eq!(merged.ignore_globs.len(), base_globs + 1); // *.custom
        assert!(merged.is_ignored("custom_dir"));
        assert!(merged.is_ignored("custom.lock"));
        assert!(merged.is_ignored("a/x.custom"));
        assert_eq!(merged.max_file_size, 1024);
    }

    #[test]
    fn scanner_merge_keeps_default_size_when_absent() {
        let mut merged = ScanConfig::default();
        let no_size = ScannerFileConfig {
            ignore: vec![],
            ignore_dirs: vec!["x".into()],
            ignore_ext: vec![],
            ignore_files: vec![],
            max_file_size: None,
        };
        merge_scanner(&mut merged, &no_size);
        assert_eq!(merged.max_file_size, ScanConfig::default().max_file_size);
    }

    #[test]
    fn scanner_ext_normalizes_dot_prefix() {
        let mut merged = ScanConfig::default();
        let raw = ScannerFileConfig {
            ignore: vec![],
            ignore_dirs: vec![],
            ignore_ext: vec!["jpg".into(), ".png".into()],
            ignore_files: vec![],
            max_file_size: None,
        };
        merge_scanner(&mut merged, &raw);
        assert!(merged.is_ignored("x/y.jpg"));
        assert!(merged.is_ignored("x/y.png"));
    }

    #[test]
    fn scanner_merge_unified_ignore_list() {
        let mut merged = ScanConfig::default();
        let unified = ScannerFileConfig {
            ignore: vec![
                "*.class".into(),
                ".env*".into(),
                "custom/dir".into(),
                "build-*/".into(),
            ],
            ignore_dirs: vec![],
            ignore_ext: vec![],
            ignore_files: vec![],
            max_file_size: None,
        };
        merge_scanner(&mut merged, &unified);
        assert!(merged.is_ignored("deep/x.class"));
        assert!(merged.is_ignored(".env.production"));
        assert!(merged.is_ignored("custom/dir/file.rs"));
        assert!(!merged.is_ignored("custom/other"));
        // glob 条目进入 globs 桶
        assert!(merged.ignore_globs.iter().any(|g| g == ".env*"));
    }

    #[test]
    fn parse_ignore_yaml_shape() {
        // ignore.yaml 与 config.yaml 内联 scanner 段共用同一 schema；
        // 新式统一 `ignore` 列表优先。
        let yaml = r#"
ignore:
- node_modules
- "*.class"
- "**/generated/*.java"
ignore_dirs: [.cache]
max_file_size: 1048576
"#;
        let cfg: ScannerFileConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.ignore.len(), 3);
        assert_eq!(cfg.ignore_dirs, vec![".cache"]);
        assert_eq!(cfg.max_file_size, Some(1048576));
    }

    #[test]
    fn scanner_merge_glob_and_legacy_together() {
        let mut merged = ScanConfig::default();
        let cfg = ScannerFileConfig {
            ignore: vec!["*.tmp".into(), "vendor".into()],
            ignore_dirs: vec!["legacy_dir".into()],
            ignore_ext: vec![".bak".into()],
            ignore_files: vec![],
            max_file_size: None,
        };
        merge_scanner(&mut merged, &cfg);
        assert!(merged.is_ignored("a.tmp"));
        assert!(merged.is_ignored("vendor")); // ignore 纯名
        assert!(merged.is_ignored("x/legacy_dir")); // 旧 ignore_dirs
        assert!(merged.is_ignored("x/backup.bak")); // 旧 ignore_ext → *.bak
    }
}
