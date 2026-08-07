//! 文件类型（FileType）——按文件后缀划分的类别体系。
//!
//! 与内容类型（entity_type，LLM 语义分类）正交：
//! - **文件类型**：文件是什么，由后缀决定（`md` → 文档、`yaml` → 配置、`java` → 代码）。
//! - **内容类型**：内容是什么，由 LLM 语义分类（`Config`、`Service`、`Standard`…）或
//!   AST 结构（`Method`、`Class`、`Function`）决定。
//!
//! 类别命名注意：文档类别内部标识为 `document`（避免与 `world=doc` 混淆）。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 文件类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    /// 文档类（md、doc、docs、txt、rtf、pdf…）。内部标识 `document`。
    Document,
    /// 代码类（java、go、rs、php、py、js、ts、c、cpp…）。
    Code,
    /// 配置类（yaml、yml、properties、json、toml、ini…）。
    Config,
    /// Nacos 配置（`dt://nacos/` 来源，来源优先于后缀映射）。
    NacosConfig,
    /// 未知类别。
    Other,
}

impl FileCategory {
    /// 类别内部标识（用于 `--file-type` 参数匹配）。
    pub fn slug(self) -> &'static str {
        match self {
            FileCategory::Document => "document",
            FileCategory::Code => "code",
            FileCategory::Config => "config",
            FileCategory::NacosConfig => "nacos_config",
            FileCategory::Other => "other",
        }
    }

    /// 类别显示名（中文，用于搜索结果渲染）。
    pub fn label(self) -> &'static str {
        match self {
            FileCategory::Document => "文档",
            FileCategory::Code => "代码",
            FileCategory::Config => "配置",
            FileCategory::NacosConfig => "nacos配置",
            FileCategory::Other => "其他",
        }
    }
}

/// 后缀 → 类别 的静态映射表。
fn suffix_map() -> &'static HashMap<&'static str, FileCategory> {
    static MAP: OnceLock<HashMap<&'static str, FileCategory>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        // 文档类
        for s in [
            "md", "markdown", "doc", "docs", "docx", "txt", "rtf", "pdf", "rst", "adoc", "textile",
            "org",
        ] {
            m.insert(s, FileCategory::Document);
        }
        // 代码类
        for s in [
            "java", "go", "rs", "php", "py", "js", "ts", "jsx", "tsx", "c", "h", "cpp", "hpp",
            "cc", "cs", "rb", "kt", "kts", "swift", "scala", "sh", "bash", "zsh", "lua", "pl", "r",
            "m", "mm", "vue", "svelte", "sql", "dart", "zig", "nim", "ex", "exs", "erl", "hs",
            "ml", "clj", "groovy", "gradle", "tf", "proto",
        ] {
            m.insert(s, FileCategory::Code);
        }
        // 配置类
        for s in [
            "yaml",
            "yml",
            "properties",
            "json",
            "toml",
            "ini",
            "conf",
            "cfg",
            "config",
            "env",
            "xml",
            "tml",
            "editorconfig",
            "lock",
        ] {
            m.insert(s, FileCategory::Config);
        }
        m
    })
}

/// 根据文件路径/后缀推断文件类别。
pub fn categorize_path(path: &str) -> FileCategory {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    match ext {
        Some(e) => suffix_map()
            .get(e.as_str())
            .copied()
            .unwrap_or(FileCategory::Other),
        None => FileCategory::Other,
    }
}

/// 根据后缀字符串推断文件类别（`categorize_path` 的便捷包装，输入已是扩展名）。
pub fn categorize_ext(ext: &str) -> FileCategory {
    let e = ext.trim_start_matches('.').to_lowercase();
    suffix_map()
        .get(e.as_str())
        .copied()
        .unwrap_or(FileCategory::Other)
}

/// 将 `--file-type` 参数归一化为类别集合。
///
/// 支持两种写法：
/// - 类别名：`document` / `code` / `config`（亦接受 `doc`、`docs`、`text` → 文档类）。
/// - 具体后缀：`md`、`yaml`、`java`…
///
/// 返回匹配的类别列表；参数不匹配任何已知类别时返回空列表（由调用方决定报错或忽略）。
pub fn resolve_file_types(spec: &str) -> Vec<FileCategory> {
    let s = spec.trim().to_lowercase();
    if s.is_empty() {
        return Vec::new();
    }
    match s.as_str() {
        "document" | "doc" | "docs" | "text" | "txt" => vec![FileCategory::Document],
        "code" | "src" | "source" => vec![FileCategory::Code],
        "config" | "conf" | "cfg" | "配置" => vec![FileCategory::Config],
        "nacos_config" | "nacos" | "nacos配置" => vec![FileCategory::NacosConfig],
        "other" | "其他" => vec![FileCategory::Other],
        "all" | "*" => vec![
            FileCategory::Document,
            FileCategory::Code,
            FileCategory::Config,
            FileCategory::NacosConfig,
            FileCategory::Other,
        ],
        _ => {
            // 尝试按具体后缀解析
            match suffix_map().get(s.as_str()) {
                Some(cat) => vec![*cat],
                None => Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorizes_by_extension() {
        assert_eq!(categorize_path("docs/guide.md"), FileCategory::Document);
        assert_eq!(categorize_path("a/b/config.yaml"), FileCategory::Config);
        assert_eq!(categorize_path("src/main.rs"), FileCategory::Code);
        assert_eq!(categorize_path("README"), FileCategory::Other);
        assert_eq!(categorize_ext("yml"), FileCategory::Config);
        assert_eq!(categorize_ext(".java"), FileCategory::Code);
    }

    #[test]
    fn resolves_category_names() {
        assert_eq!(resolve_file_types("document"), vec![FileCategory::Document]);
        assert_eq!(resolve_file_types("code"), vec![FileCategory::Code]);
        assert_eq!(resolve_file_types("config"), vec![FileCategory::Config]);
        assert_eq!(resolve_file_types("md"), vec![FileCategory::Document]);
        assert_eq!(resolve_file_types("yaml"), vec![FileCategory::Config]);
        assert_eq!(resolve_file_types("rs"), vec![FileCategory::Code]);
        assert!(resolve_file_types("unknownxyz").is_empty());
    }

    #[test]
    fn labels_and_slugs() {
        assert_eq!(FileCategory::Document.slug(), "document");
        assert_eq!(FileCategory::Config.label(), "配置");
        assert_eq!(FileCategory::Code.label(), "代码");
    }

    #[test]
    fn nacos_config_category_mapping() {
        assert_eq!(FileCategory::NacosConfig.slug(), "nacos_config");
        assert_eq!(FileCategory::NacosConfig.label(), "nacos配置");
        // resolve 三形态：slug / 简写 / 中文
        assert_eq!(
            resolve_file_types("nacos_config"),
            vec![FileCategory::NacosConfig]
        );
        assert_eq!(resolve_file_types("nacos"), vec![FileCategory::NacosConfig]);
        assert_eq!(
            resolve_file_types("nacos配置"),
            vec![FileCategory::NacosConfig]
        );
        // all 集合包含新类别
        assert!(resolve_file_types("all").contains(&FileCategory::NacosConfig));
        // 后缀解析不受影响（yaml 仍属 Config，而非 NacosConfig）
        assert_eq!(resolve_file_types("yaml"), vec![FileCategory::Config]);
        assert_eq!(categorize_path("common.yaml"), FileCategory::Config);
        // dt://nacos 前缀路径本身按后缀解析不到 NacosConfig（来源判定在 infer_file_type）
        assert_eq!(
            categorize_path("dt://nacos/test/DEFAULT_GROUP/common.yaml"),
            FileCategory::Config
        );
    }
}
