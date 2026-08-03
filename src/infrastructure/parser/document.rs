//! 文档解析器——从文档文件中抽取结构化信息。
//!
//! 处理：
//! - Markdown（`.md`）：从首个 H1 提取标题，剥离格式得到纯文本
//! - Text（`.txt`）：原始文本，用文件名作为标题
//! - PDF（`.pdf`）：桩实现——仅返回基于文件名的元数据（完整 PDF 解析
//!   需要 `pdf-extract` 或 `lopdf` 等外部依赖）

use std::path::{Path, PathBuf};

/// 解析出的文档内容。
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    /// 文档标识符：`dt://doc/{project}/{rel_path}`
    pub doc_id: String,
    /// 文件名（不含路径）。
    pub name: String,
    /// 文档标题（来自 H1、文件名或首行）。
    pub title: String,
    /// 完整纯文本内容（已剥离 markdown）。
    pub content: String,
    /// 简短摘要（内容的前 200 个字符）。
    pub summary: String,
    /// 磁盘上的绝对文件路径。
    pub file_path: PathBuf,
    /// 相对项目根目录的路径。
    pub rel_path: String,
    /// 项目名。
    pub project: String,
    /// 文档类型："markdown"、"text"、"pdf"。
    pub doc_type: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 文件修改时间（RFC 3339）。
    pub modified: String,
}

/// 解析文档文件，返回结构化的文档元数据。
pub fn parse_document(path: &Path, project: &str, root: &Path) -> Result<ParsedDocument, String> {
    let rel_path = crate::infrastructure::scanner::rel_path(root, path);
    let doc_id = crate::domain::id::make_document_id(project, &rel_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.clone());

    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let metadata = std::fs::metadata(path).map_err(|e| format!("io error: {e}"))?;
    let size = metadata.len();
    let modified = {
        metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos())
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    };

    // 尝试按文本读取。二进制文档（PDF、DOCX 等）会解码 UTF-8 失败——
    // 此时回退到仅元数据的桩结果，而不是报错。
    let raw_content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            return parse_binary_stub(path, project, &name, &rel_path, &doc_id, size, &modified)
        }
    };

    let (doc_type, title, content) = match extension.as_str() {
        "md" | "markdown" => parse_markdown(&raw_content, &name),
        "txt" | "text" => ("text".to_string(), name.clone(), raw_content),
        "yaml" | "yml" => ("yaml".to_string(), name.clone(), raw_content),
        "properties" => ("properties".to_string(), name.clone(), raw_content),
        other => return Err(format!("unsupported document type: .{}", other)),
    };

    let summary = if content.len() > 200 {
        let truncated: String = content.chars().take(200).collect();
        format!("{}…", truncated)
    } else {
        content.clone()
    };

    Ok(ParsedDocument {
        doc_id,
        name,
        title,
        content,
        summary,
        file_path: path.to_path_buf(),
        rel_path,
        project: project.to_string(),
        doc_type,
        size,
        modified,
    })
}

/// 解析 markdown 文档：从首个 H1（`# Title`）提取标题，
/// 剥离常见格式以得到纯文本内容。
fn parse_markdown(raw: &str, filename: &str) -> (String, String, String) {
    let doc_type = "markdown".to_string();

    let title = raw
        .lines()
        .find(|line| line.trim_start().starts_with("# "))
        .map(|line| {
            line.trim_start()
                .trim_start_matches("# ")
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| {
            // 尝试用首个非空行作为回退标题
            raw.lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| filename.to_string())
        });

    let content = strip_markdown(raw);

    (doc_type, title, content)
}

/// 基础 markdown 剥离：移除 ATX 标题（`#`）、粗体/斜体标记、
/// 行内代码、链接（保留文本）、引用块与水平分割线。
fn strip_markdown(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let lines: Vec<&str> = raw.lines().collect();

    for line in &lines {
        let trimmed = line.trim();

        // 跳过水平分割线
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            continue;
        }

        // 跳过 HTML 注释
        if trimmed.starts_with("<!--") {
            continue;
        }

        let mut processed = line.to_string();

        // 剥离 ATX 标题："## " 或 "# " → 保留文本
        if let Some(stripped) = processed.trim_start().strip_prefix("# ") {
            processed = stripped.to_string();
        } else if let Some(stripped) = processed.trim_start().strip_prefix("## ") {
            processed = stripped.to_string();
        } else if let Some(stripped) = processed.trim_start().strip_prefix("### ") {
            processed = stripped.to_string();
        } else if let Some(stripped) = processed.trim_start().strip_prefix("#### ") {
            processed = stripped.to_string();
        } else if let Some(stripped) = processed.trim_start().strip_prefix("##### ") {
            processed = stripped.to_string();
        } else if let Some(stripped) = processed.trim_start().strip_prefix("###### ") {
            processed = stripped.to_string();
        }

        // 剥离粗体/斜体：**text**、*text*、__text__、_text_
        processed = strip_markers(&processed, "**");
        processed = strip_markers(&processed, "__");
        processed = strip_markers(&processed, "*");
        processed = strip_markers(&processed, "_");

        // 剥离行内代码：`text`
        processed = strip_markers(&processed, "`");

        // 剥离引用块前缀 "> "
        if processed.trim_start().starts_with("> ") {
            processed = processed.trim_start().replacen("> ", "", 1);
        }

        // 剥离图片语法：![alt](url) → alt
        processed = strip_markdown_images(&processed);

        // 剥离链接语法：[text](url) → text
        processed = strip_markdown_links(&processed);

        // 处理后跳过空行
        let trimmed_processed = processed.trim();
        if trimmed_processed.is_empty() {
            // 不添加多个连续空行
            if !result.ends_with('\n') {
                result.push('\n');
            }
        } else {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&processed);
        }
    }

    result.trim().to_string()
}

/// 剥离成对标记（如 `**bold**` → `bold`）。
fn strip_markers(text: &str, marker: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find(marker) {
        let after_start = &result[start + marker.len()..];
        let end = match after_start.find(marker) {
            Some(e) => start + marker.len() + e,
            None => break,
        };
        if start < end {
            let inner = &result[start + marker.len()..end];
            result = format!(
                "{}{}{}",
                &result[..start],
                inner,
                &result[end + marker.len()..]
            );
        } else {
            break;
        }
    }
    result
}

/// 剥离 markdown 图片语法：`![alt](url)` → `alt`
fn strip_markdown_images(text: &str) -> String {
    let re = regex::Regex::new(r"!\[([^\]]*)\]\([^)]*\)").unwrap();
    re.replace_all(text, "$1").to_string()
}

/// 剥离 markdown 链接语法：`[text](url)` → `text`
fn strip_markdown_links(text: &str) -> String {
    let re = regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap();
    re.replace_all(text, "$1").to_string()
}

/// 二进制文档的桩解析器：仅元数据，不抽取内容。
///
/// 当 `read_to_string` 在二进制文件（PDF、DOCX 等）上失败时调用。
/// 记录文件名、大小与修改时间，不尝试内容解析。
fn parse_binary_stub(
    path: &Path,
    project: &str,
    name: &str,
    rel_path: &str,
    doc_id: &str,
    size: u64,
    modified: &str,
) -> Result<ParsedDocument, String> {
    let doc_type = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();

    Ok(ParsedDocument {
        doc_id: doc_id.to_string(),
        name: name.to_string(),
        title: name.to_string(),
        content: String::new(),
        summary: String::new(),
        file_path: path.to_path_buf(),
        rel_path: rel_path.to_string(),
        project: project.to_string(),
        doc_type,
        size,
        modified: modified.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_markdown_extracts_h1_title() {
        let md = "# My Document\n\nSome content here.";
        let (doc_type, title, content) = parse_markdown(md, "readme.md");
        assert_eq!(doc_type, "markdown");
        assert_eq!(title, "My Document");
        assert!(content.contains("Some content here"));
    }

    #[test]
    fn parse_markdown_fallback_to_first_line() {
        let md = "No H1 here\n\nJust regular text.";
        let (doc_type, title, content) = parse_markdown(md, "file.md");
        assert_eq!(doc_type, "markdown");
        assert_eq!(title, "No H1 here");
        assert!(content.contains("Just regular text"));
    }

    #[test]
    fn strip_markdown_removes_bold_and_italic() {
        let md = "**bold** and *italic* text";
        let result = strip_markdown(md);
        assert_eq!(result, "bold and italic text");
    }

    #[test]
    fn strip_markdown_removes_links() {
        let md = "Visit [Google](https://google.com) now";
        let result = strip_markdown(md);
        assert_eq!(result, "Visit Google now");
    }

    #[test]
    fn strip_markdown_removes_images() {
        let md = "![Logo](logo.png)";
        let result = strip_markdown(md);
        assert_eq!(result, "Logo");
    }

    #[test]
    fn strip_markdown_removes_inline_code() {
        let md = "Use the `println!` macro";
        let result = strip_markdown(md);
        assert_eq!(result, "Use the println! macro");
    }

    #[test]
    fn strip_markdown_skips_horizontal_rules() {
        let md = "Before\n\n---\n\nAfter";
        let result = strip_markdown(md);
        // 水平分割线被移除；周围空行被折叠
        assert!(result.starts_with("Before"));
        assert!(result.ends_with("After"));
        // 确保 Before 与 After 均存在，且被适当分隔
        assert!(!result.contains("---"), "水平分割线应被移除");
    }
}
