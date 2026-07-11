//! Core domain types for the Digital Twin system.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Health & Plugin
// ---------------------------------------------------------------------------

/// Health status of a plugin or service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// Fully operational.
    Healthy,
    /// Degraded but usable.
    Degraded(String),
    /// Not available.
    Unhealthy(String),
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }
}

/// Plugin-specific error type. The `From<PluginError> for tonic::Status`
/// conversion is implemented in `dt-plugins` where `tonic` is a dependency.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("plugin init failed: {0}")]
    InitFailed(String),
    #[error("gRPC registration failed: {0}")]
    GrpcRegistration(String),
    #[error("health check failed: {0}")]
    HealthCheck(String),
    #[error("shutdown failed: {0}")]
    Shutdown(String),
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Shared application configuration.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Data directory for the system.
    pub data_dir: PathBuf,
    /// Neo4j connection URI (bolt://).
    pub neo4j_uri: String,
    /// Neo4j username.
    pub neo4j_user: String,
    /// Neo4j password.
    pub neo4j_password: String,
    /// Qdrant gRPC endpoint.
    pub qdrant_uri: String,
    /// Embed server gRPC endpoint.
    pub embed_uri: String,
    /// Daemon gRPC listen address.
    pub listen_addr: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/digital-twin"),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "password".into(),
            qdrant_uri: "http://localhost:6334".into(),
            embed_uri: "http://localhost:50052".into(),
            listen_addr: "127.0.0.1:50051".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Logger
// ---------------------------------------------------------------------------

/// Logger handle for plugins (async-safe, no blocking I/O).
///
/// All messages are emitted through the `tracing` crate. The plugin name is
/// included in the message itself because `tracing` event macros require a
/// `&'static str` target. The JSON formatter picks up the Rust module path
/// as `target`, and the plugin name is embedded in the `message` field.
///
/// For key-value structured fields, use the `tracing::info!` macro directly:
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

/// Core context passed to all plugins at initialization.
pub struct PluginContext {
    pub graph: Arc<dyn crate::domain::traits::GraphRepository>,
    pub vector: Arc<dyn crate::domain::traits::VectorRepository>,
    pub config: Arc<AppConfig>,
    pub log: PluginLogger,
    pub data_dir: PathBuf,
}

// ---------------------------------------------------------------------------
// V2 Code Entity Types
// ---------------------------------------------------------------------------

/// Programming language.
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

/// Kinds of classes.
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

/// V2 Method node — a parsed method/function entity.
#[derive(Debug, Clone)]
pub struct MethodBlock {
    /// dt://entity/{project}/class/{className}/method/{name}@{line}
    pub method_id: String,
    /// Simple method name.
    pub name: String,
    /// Full signature line.
    pub signature: String,
    /// Parameter list string.
    pub params: String,
    /// Return type.
    pub return_type: String,
    /// Owning class name.
    pub class_name: String,
    /// File absolute path.
    pub file_path: String,
    /// Package or module name.
    pub package_or_module: String,
    /// Language.
    pub language: String,
    /// Project name.
    pub project: String,
    /// Start line (1-indexed).
    pub start_line: usize,
    /// End line (1-indexed).
    pub end_line: usize,
    /// Method names called within the body.
    pub calls: Vec<String>,
    /// Comment / docstring summary.
    pub comment: String,
    /// Full source text (for embedding).
    pub source_text: String,
}

/// V2 Class node — a parsed class/interface/enum/struct entity.
#[derive(Debug, Clone)]
pub struct ClassBlock {
    /// dt://entity/{project}/package/{package}/class/{name}
    pub class_id: String,
    /// Class name.
    pub name: String,
    /// Class kind.
    pub kind: ClassKind,
    /// File absolute path.
    pub file_path: String,
    /// Package or module name.
    pub package_or_module: String,
    /// Project name.
    pub project: String,
    /// Start line.
    pub start_line: usize,
    /// End line.
    pub end_line: usize,
    /// Method IDs contained in this class.
    pub method_ids: Vec<String>,
}

/// V2 Module node — auto-generated from package/module paths.
#[derive(Debug, Clone)]
pub struct ModuleBlock {
    /// dt://entity/{project}/module/{name}
    pub module_id: String,
    /// Module name.
    pub name: String,
    /// Project name.
    pub project: String,
}

// ---------------------------------------------------------------------------
// Qdrant / Vector Types
// ---------------------------------------------------------------------------

/// Information about a Qdrant collection.
#[derive(Debug, Clone)]
pub struct CollectionInfo {
    /// Collection name (e.g. "myproject_methods_v1").
    pub name: String,
    /// Total number of points stored.
    pub points_count: u64,
    /// Vector dimension (e.g. 1024 for BGE-M3).
    pub vector_dim: u32,
    /// Embedding model version used for this collection.
    pub model_version: String,
}

// ---------------------------------------------------------------------------
// Build Pipeline Types
// ---------------------------------------------------------------------------

/// Result of parsing a single file.
#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    pub methods: Vec<MethodBlock>,
    pub classes: Vec<ClassBlock>,
}

/// File snapshot for change detection.
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    /// Relative file path within the project.
    pub file_path: String,
    /// Project name.
    pub project: String,
    /// SHA1 hash of file contents.
    pub file_sha1: String,
    /// File modification time (unix epoch seconds as float).
    pub file_mtime: f64,
    /// Number of methods extracted.
    pub method_count: u32,
    /// Last update timestamp (ISO 8601).
    pub updated_at: String,
}

/// Scanner configuration.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Directories to ignore.
    pub ignore_dirs: HashSet<String>,
    /// Extensions to ignore (with dot prefix, e.g. ".class").
    pub ignore_ext: HashSet<String>,
    /// Maximum file size in bytes.
    pub max_file_size: u64,
    /// Document file extensions (without dot prefix, e.g. "md", "txt").
    pub document_extensions: HashSet<String>,
    /// Maximum document file size in bytes (default 5 MB).
    pub max_doc_file_size: u64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            ignore_dirs: [
                "node_modules", ".git", "target", "build", "__pycache__",
                ".venv", "dist", ".next", "vendor", ".idea", ".vscode",
                "coverage", ".nyc_output",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ignore_ext: [
                ".class", ".jar", ".war", ".so", ".dll", ".exe", ".bin",
                ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico",
                ".zip", ".tar", ".gz", ".bz2",
                ".pdf", ".lock",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            max_file_size: 524_288, // 500 KB
            document_extensions: [
                "md", "txt", "pdf",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            max_doc_file_size: 5_242_880, // 5 MB
        }
    }
}

/// Build completion report.
#[derive(Debug, Clone)]
pub struct BuildReport {
    /// Project name.
    pub project: String,
    /// Total files scanned.
    pub files_scanned: usize,
    /// Files that changed (new or modified).
    pub files_changed: usize,
    /// Total methods in graph for this project.
    pub methods_total: usize,
    /// New methods added in this build.
    pub methods_new: usize,
    /// Total classes in graph for this project.
    pub classes_total: usize,
    /// Elapsed time in milliseconds.
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Tests
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
