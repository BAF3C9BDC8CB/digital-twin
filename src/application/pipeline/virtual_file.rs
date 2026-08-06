//! VirtualFile —— 统一输入抽象，将普通文件与远程数据源归约为同一结构。
//!
//! Pipeline 核心链只接收 VirtualFile / PipelineContext，不对具体来源做分支判断。
//! 普通文件（`Fs`）与 Nacos/Jenkins 等远程源均通过此结构送入流水线。

use serde::{Deserialize, Serialize};

/// 文件来源类型（开放枚举，可扩展）。
///
/// 序列化为字符串（`"Fs"` / `"Nacos"` / `"Jenkins"` / 任意自定义字符串）。
/// 反序列化时未知字符串自动归入 `Other(String)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSourceKind {
    /// 本地文件系统中的真实文件。
    Fs,
    /// Nacos 配置中心。
    Nacos,
    /// Jenkins 构建系统。
    Jenkins,
    /// 未来扩展或未识别的来源类型。
    Other(String),
}

impl Serialize for FileSourceKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FileSourceKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "Fs" => FileSourceKind::Fs,
            "Nacos" => FileSourceKind::Nacos,
            "Jenkins" => FileSourceKind::Jenkins,
            other => FileSourceKind::Other(other.to_string()),
        })
    }
}

impl FileSourceKind {
    /// 返回人类可读的来源类型名称。
    pub fn as_str(&self) -> &str {
        match self {
            FileSourceKind::Fs => "Fs",
            FileSourceKind::Nacos => "Nacos",
            FileSourceKind::Jenkins => "Jenkins",
            FileSourceKind::Other(s) => s.as_str(),
        }
    }

    /// 是否为本地文件来源。
    pub fn is_fs(&self) -> bool {
        matches!(self, FileSourceKind::Fs)
    }
}

impl Default for FileSourceKind {
    fn default() -> Self {
        FileSourceKind::Fs
    }
}

impl std::fmt::Display for FileSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 统一输入：普通文件与远程数据源都归约为此结构。
///
/// # 示例
///
/// ```ignore
/// let vf = VirtualFile {
///     virtual_path: "dt://nacos/prod/order-service.yaml".into(),
///     content: "server.port: 8080".into(),
///     project: "my-project".into(),
///     source: FileSourceKind::Nacos,
///     mtime: None,
///     content_hash: sha256("server.port: 8080"),
///     front_matter: None,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct VirtualFile {
    /// 虚拟路径（如 `dt://nacos/prod/order-service.yaml` 或真实文件路径）。
    pub virtual_path: String,
    /// 文件/配置的完整文本内容。
    pub content: String,
    /// 所属项目名。
    pub project: String,
    /// 来源类型。
    pub source: FileSourceKind,
    /// 修改时间（Fs 有真实 mtime；远程源为 `None`）。
    pub mtime: Option<f64>,
    /// 内容 SHA256 哈希（远程源必填，作为增量对比唯一依据）。
    pub content_hash: String,
    /// 可选的结构化 YAML front-matter（Jenkins 等来源使用）。
    pub front_matter: Option<String>,
}

impl VirtualFile {
    /// 创建新的 VirtualFile。
    pub fn new(
        virtual_path: impl Into<String>,
        content: impl Into<String>,
        project: impl Into<String>,
        source: FileSourceKind,
        mtime: Option<f64>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            virtual_path: virtual_path.into(),
            content: content.into(),
            project: project.into(),
            source,
            mtime,
            content_hash: content_hash.into(),
            front_matter: None,
        }
    }

    /// 为本地文件创建 VirtualFile（`source = Fs`）。
    pub fn from_fs(
        virtual_path: impl Into<String>,
        content: impl Into<String>,
        project: impl Into<String>,
        mtime: Option<f64>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self::new(
            virtual_path,
            content,
            project,
            FileSourceKind::Fs,
            mtime,
            content_hash,
        )
    }

    /// 设置 front_matter 并返回 self（构建器模式）。
    pub fn with_front_matter(mut self, front_matter: impl Into<String>) -> Self {
        self.front_matter = Some(front_matter.into());
        self
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_file_fs_defaults() {
        let vf = VirtualFile::from_fs(
            "src/main.rs",
            "fn main() {}",
            "my_project",
            Some(1723001234.0),
            "abc123",
        );
        assert_eq!(vf.virtual_path, "src/main.rs");
        assert_eq!(vf.content, "fn main() {}");
        assert_eq!(vf.project, "my_project");
        assert_eq!(vf.source, FileSourceKind::Fs);
        assert!(vf.source.is_fs());
        assert_eq!(vf.mtime, Some(1723001234.0));
        assert_eq!(vf.content_hash, "abc123");
        assert!(vf.front_matter.is_none());
    }

    #[test]
    fn virtual_file_nacos() {
        let vf = VirtualFile::new(
            "dt://nacos/prod/order-service.yaml",
            "server.port: 8080",
            "order-service",
            FileSourceKind::Nacos,
            None,
            "def456",
        );
        assert_eq!(vf.source, FileSourceKind::Nacos);
        assert!(!vf.source.is_fs());
        assert_eq!(vf.mtime, None);
        assert_eq!(vf.source.as_str(), "Nacos");
    }

    #[test]
    fn virtual_file_jenkins_with_front_matter() {
        let vf = VirtualFile::new(
            "dt://jenkins/order-service-deploy",
            "build log content",
            "order-service",
            FileSourceKind::Jenkins,
            None,
            "ghi789",
        )
        .with_front_matter("type: jenkins_build\njob_name: order-service-deploy");
        assert_eq!(vf.source, FileSourceKind::Jenkins);
        assert!(vf.front_matter.is_some());
        assert_eq!(
            vf.front_matter.unwrap(),
            "type: jenkins_build\njob_name: order-service-deploy"
        );
    }

    #[test]
    fn file_source_kind_default_is_fs() {
        let kind = FileSourceKind::default();
        assert_eq!(kind, FileSourceKind::Fs);
        assert!(kind.is_fs());
    }

    #[test]
    fn file_source_kind_display() {
        assert_eq!(FileSourceKind::Fs.to_string(), "Fs");
        assert_eq!(FileSourceKind::Nacos.to_string(), "Nacos");
        assert_eq!(FileSourceKind::Jenkins.to_string(), "Jenkins");
        assert_eq!(
            FileSourceKind::Other("k8s".into()).to_string(),
            "k8s"
        );
    }

    #[test]
    fn file_source_kind_serde_roundtrip() {
        let json = serde_json::to_string(&FileSourceKind::Nacos).unwrap();
        let kind: FileSourceKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, FileSourceKind::Nacos);

        // Other 变体
        let json = serde_json::to_string(&FileSourceKind::Other("k8s".into())).unwrap();
        let kind: FileSourceKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, FileSourceKind::Other("k8s".into()));
    }
}
