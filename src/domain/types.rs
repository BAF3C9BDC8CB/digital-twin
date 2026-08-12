//! 数字孪生系统的核心领域类型。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// 健康状态与插件
// ---------------------------------------------------------------------------

/// 插件或服务的健康状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// 完全正常运行。
    Healthy,
    /// 已降级但仍可用。
    Degraded(String),
    /// 不可用。
    Unhealthy(String),
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }
}

/// 插件专用错误类型。
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("插件未找到：{0}")]
    NotFound(String),
    #[error("插件初始化失败：{0}")]
    InitFailed(String),
    #[error("gRPC 注册失败：{0}")]
    GrpcRegistration(String),
    #[error("健康检查失败：{0}")]
    HealthCheck(String),
    #[error("关闭失败：{0}")]
    Shutdown(String),
    #[error("内部错误：{0}")]
    Internal(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 共享的应用配置。
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// 系统数据目录。
    pub data_dir: PathBuf,
    /// Memgraph 连接 URI（bolt://）。
    pub memgraph_uri: String,
    /// Memgraph 用户名。
    pub memgraph_user: String,
    /// Memgraph 密码。
    pub memgraph_password: String,
    /// Qdrant gRPC 端点。
    pub qdrant_uri: String,
    /// Embed 服务 gRPC 端点。
    pub embed_uri: String,
    /// Daemon gRPC 监听地址。
    pub listen_addr: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/digital-twin"),
            memgraph_uri: "bolt://localhost:7687".into(),
            memgraph_user: "memgraph".into(),
            memgraph_password: "password".into(),
            qdrant_uri: "http://localhost:6334".into(),
            embed_uri: "https://api.siliconflow.cn/v1".into(),
            listen_addr: "127.0.0.1:50051".into(),
        }
    }
}

/// 构建流水线与 upsert 操作的批处理大小。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BatchConfig {
    /// 向 Memgraph 写入节点时每个 UNWIND 批次包含的条目数。
    /// 统一适用于 Method、Class 和 Module 节点。
    #[serde(default = "default_unwind_batch")]
    pub unwind: usize,
    /// 每次 embedding gRPC 调用处理的文本条目数。
    #[serde(default = "default_embed_batch")]
    pub embed: usize,
    /// 每次 Qdrant upsert 调用处理的向量点数。
    #[serde(default = "default_upsert_batch")]
    pub upsert: usize,
    /// 并发的 embedding gRPC 流数量。
    #[serde(default = "default_embed_concurrency")]
    pub embed_concurrency: usize,
}

const fn default_unwind_batch() -> usize {
    200
}
const fn default_embed_batch() -> usize {
    512
}
const fn default_upsert_batch() -> usize {
    1000
}
const fn default_embed_concurrency() -> usize {
    3
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            unwind: default_unwind_batch(),
            embed: default_embed_batch(),
            upsert: default_upsert_batch(),
            embed_concurrency: default_embed_concurrency(),
        }
    }
}

// ---------------------------------------------------------------------------
// 日志器
// ---------------------------------------------------------------------------

/// 插件的日志器句柄（异步安全，无阻塞 I/O）。
///
/// 所有消息都通过 `tracing` crate 发出。插件名包含在消息本身中，因为
/// `tracing` 事件宏要求 `&'static str` 作为 target。JSON 格式化器将 Rust
/// 模块路径作为 `target`，插件名则内嵌在 `message` 字段中。
///
/// 如需键值结构字段，请直接使用 `tracing::info!` 宏：
/// ```ignore
/// tracing::info!(plugin = "k8s", pods = 12, "pod listing complete");
/// ```
#[derive(Clone)]
pub struct PluginLogger {
    pub target: String,
}

impl PluginLogger {
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }

    pub fn info(&self, msg: &str) {
        tracing::info!("[{}] {}", self.target, msg);
    }

    pub fn warn(&self, msg: &str) {
        tracing::warn!("[{}] {}", self.target, msg);
    }

    pub fn error(&self, msg: &str) {
        tracing::error!("[{}] {}", self.target, msg);
    }

    pub fn debug(&self, msg: &str) {
        tracing::debug!("[{}] {}", self.target, msg);
    }

    pub fn trace(&self, msg: &str) {
        tracing::trace!("[{}] {}", self.target, msg);
    }
}

/// 初始化时传递给所有插件的核心上下文。
pub struct PluginContext {
    pub graph: Arc<dyn crate::domain::traits::GraphRepository>,
    pub vector: Arc<dyn crate::domain::traits::VectorRepository>,
    pub config: Arc<AppConfig>,
    pub log: PluginLogger,
    pub data_dir: PathBuf,
}

// ---------------------------------------------------------------------------
// V2 代码实体类型
// ---------------------------------------------------------------------------

/// 编程语言。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Language {
    Java,
    TypeScript,
    Python,
    Go,
    Rust,
    Php,
    JavaScript,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Java => "java",
            Language::TypeScript => "typescript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Php => "php",
            Language::JavaScript => "javascript",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "java" => Some(Language::Java),
            "ts" | "tsx" => Some(Language::TypeScript),
            "py" => Some(Language::Python),
            "go" => Some(Language::Go),
            "rs" => Some(Language::Rust),
            "php" => Some(Language::Php),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            _ => None,
        }
    }
}

/// 类的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Interface,
    Enum,
    Struct,
    Trait,
}

impl ClassKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClassKind::Class => "Class",
            ClassKind::Interface => "Interface",
            ClassKind::Enum => "Enum",
            ClassKind::Struct => "Struct",
            ClassKind::Trait => "Interface",
        }
    }
}

/// V2 Method 节点——一个已解析的方法/函数实体。
#[derive(Debug, Clone)]
pub struct MethodBlock {
    /// dt://entity/{project}/class/{className}/method/{name}@{line}
    pub method_id: String,
    /// 简单方法名。
    pub name: String,
    /// 完整签名行。
    pub signature: String,
    /// 参数列表字符串。
    pub params: String,
    /// 返回类型。
    pub return_type: String,
    /// 所属类名。
    pub class_name: String,
    /// 文件绝对路径。
    pub file_path: String,
    /// 包名或模块名。
    pub package_or_module: String,
    /// 语言。
    pub language: String,
    /// 项目名。
    pub project: String,
    /// 起始行（从 1 开始）。
    pub start_line: usize,
    /// 结束行（从 1 开始）。
    pub end_line: usize,
    /// 方法体内调用的方法名列表。
    pub calls: Vec<String>,
    /// 注释 / docstring 摘要。
    pub comment: String,
    /// 完整源代码文本（用于 embedding）。
    pub source_text: String,
}

/// V2 Class 节点——一个已解析的类/接口/枚举/结构体实体。
#[derive(Debug, Clone)]
pub struct ClassBlock {
    /// dt://entity/{project}/package/{package}/class/{name}
    pub class_id: String,
    /// 类名。
    pub name: String,
    /// 类的种类。
    pub kind: ClassKind,
    /// 文件绝对路径。
    pub file_path: String,
    /// 包名或模块名。
    pub package_or_module: String,
    /// 项目名。
    pub project: String,
    /// 起始行。
    pub start_line: usize,
    /// 结束行。
    pub end_line: usize,
    /// 该类包含的方法 ID 列表。
    pub method_ids: Vec<String>,
    /// 类级 javadoc / 注释摘要（源码有注释时由解析器提取，否则为空）。
    pub description: String,
}

/// V2 Module 节点——由包/模块路径自动生成。
#[derive(Debug, Clone)]
pub struct ModuleBlock {
    /// dt://entity/{project}/module/{name}
    pub module_id: String,
    /// 模块名。
    pub name: String,
    /// 项目名。
    pub project: String,
}

// ---------------------------------------------------------------------------
// Qdrant / 向量类型
// ---------------------------------------------------------------------------

/// 关于 Qdrant 集合的信息。
#[derive(Debug, Clone)]
pub struct CollectionInfo {
    /// 集合名称（例如 "myproject_methods_v1"）。
    pub name: String,
    /// 已存储的点总数。
    pub points_count: u64,
    /// 向量维度（例如 BGE-M3 为 1024）。
    pub vector_dim: u32,
    /// 该集合使用的 embedding 模型版本。
    pub model_version: String,
}

// ---------------------------------------------------------------------------
// 构建流水线类型
// ---------------------------------------------------------------------------

/// 解析单个文件的结果。
#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    pub methods: Vec<MethodBlock>,
    pub classes: Vec<ClassBlock>,
}

/// 用于变更检测的文件快照。
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    /// 项目内的相对文件路径。
    pub file_path: String,
    /// 项目名。
    pub project: String,
    /// 文件内容的 SHA1 哈希。
    pub file_sha1: String,
    /// 文件修改时间（Unix 纪元秒，浮点数）。
    pub file_mtime: f64,
    /// 提取的方法数量。
    pub method_count: u32,
    /// 最后更新时间戳（ISO 8601）。
    pub updated_at: String,
}

/// 扫描器配置。
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// 需要忽略的目录（单段目录名或相对路径前缀，例如 "node_modules"、"target/debug"）。
    pub ignore_dirs: HashSet<String>,
    /// 需要忽略的文件名（精确匹配，例如 "Cargo.lock"、"composer.lock"）。
    pub ignore_files: HashSet<String>,
    /// 需要忽略的扩展名（带点前缀，例如 ".class"）。
    pub ignore_ext: HashSet<String>,
    /// 最大文件大小（字节）。
    pub max_file_size: u64,
    /// 文档文件扩展名（不带点前缀，例如 "md"、"txt"）。
    pub document_extensions: HashSet<String>,
    /// 最大文档文件大小（字节，默认 5 MB）。
    pub max_doc_file_size: u64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            ignore_dirs: [
                "node_modules",
                ".git",
                "target",
                "build",
                "__pycache__",
                ".venv",
                "dist",
                ".next",
                "vendor",
                ".idea",
                ".vscode",
                "coverage",
                ".nyc_output",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ignore_files: [
                "composer.lock",
                "Gemfile.lock",
                "Cargo.lock",
                "poetry.lock",
                "Pipfile.lock",
                "mix.lock",
                "yarn-error.log",
                "npm-debug.log",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ignore_ext: [
                ".class", ".jar", ".war", ".so", ".dll", ".exe", ".bin", ".png", ".jpg", ".jpeg",
                ".gif", ".svg", ".ico", ".zip", ".tar", ".gz", ".bz2", ".pdf", ".lock",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            max_file_size: 524_288, // 500 KB
            document_extensions: ["md", "txt", "pdf", "yaml", "yml", "properties"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_doc_file_size: 5_242_880, // 5 MB
        }
    }
}

/// 构建完成报告。
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// 项目名。
    pub project: String,
    /// 扫描的文件总数。
    pub files_scanned: usize,
    /// 发生变更的文件数（新增或修改）。
    pub files_changed: usize,
    /// 该项目在图中累计的方法总数。
    pub methods_total: usize,
    /// 本次构建新增的方法数。
    pub methods_new: usize,
    /// 该项目在图中累计的类总数。
    pub classes_total: usize,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_extension() {
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        assert_eq!(Language::from_extension("php"), Some(Language::Php));
        assert_eq!(Language::from_extension("js"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("jsx"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("cjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_extension("txt"), None);
    }

    #[test]
    fn language_as_str() {
        assert_eq!(Language::Java.as_str(), "java");
        assert_eq!(Language::Python.as_str(), "python");
    }

    #[test]
    fn class_kind_as_str() {
        assert_eq!(ClassKind::Class.as_str(), "Class");
        assert_eq!(ClassKind::Interface.as_str(), "Interface");
        assert_eq!(ClassKind::Enum.as_str(), "Enum");
        assert_eq!(ClassKind::Struct.as_str(), "Struct");
    }

    #[test]
    fn method_block_debug() {
        let m = MethodBlock {
            method_id: "dt://entity/test/class/Foo/method/bar@10".into(),
            name: "bar".into(),
            signature: "fn bar()".into(),
            params: "".into(),
            return_type: "void".into(),
            class_name: "Foo".into(),
            file_path: "/tmp/test.rs".into(),
            package_or_module: "test_crate".into(),
            language: "rust".into(),
            project: "test".into(),
            start_line: 10,
            end_line: 15,
            calls: vec!["baz".into()],
            comment: "does stuff".into(),
            source_text: "fn bar() { baz(); }".into(),
        };
        assert_eq!(m.method_id, "dt://entity/test/class/Foo/method/bar@10");
        assert_eq!(m.name, "bar");
    }

    #[test]
    fn scan_config_default_ignores_dirs() {
        let cfg = ScanConfig::default();
        assert!(cfg.ignore_dirs.contains("node_modules"));
        assert!(cfg.ignore_dirs.contains(".git"));
        assert!(!cfg.ignore_dirs.contains("src"));
    }

    #[test]
    fn build_report_default() {
        let r = BuildReport {
            project: "test".into(),
            files_scanned: 10,
            files_changed: 2,
            methods_total: 50,
            methods_new: 5,
            classes_total: 8,
            elapsed_ms: 42,
        };
        assert_eq!(r.project, "test");
        assert_eq!(r.files_scanned, 10);
    }

    #[test]
    fn health_status_is_healthy() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(!HealthStatus::Degraded("slow".into()).is_healthy());
        assert!(!HealthStatus::Unhealthy("down".into()).is_healthy());
    }
}
