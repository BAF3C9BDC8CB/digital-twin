//! Document parser — extracts structured information from document files.
//!
//! Handles:
//! - Markdown (`.md`): extracts title from first H1, strips formatting for plain text
//! - Text (`.txt`): raw text, uses filename as title
//! - PDF (`.pdf`): stub — returns filename-based metadata only (full PDF parsing
//!   requires external dependencies like `pdf-extract` or `lopdf`)

use std::path::{Path, PathBuf};

/// Parsed document content.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    /// Document identifier: `dt://doc/{project}/{rel_path}`
    pub doc_id: String,
    /// File name (without path).
    pub name: String,
    /// Document title (from H1, filename, or first line).
    pub title: String,
    /// Full plain-text content (markdown stripped).
    pub content: String,
    /// Brief summary (first 200 characters of content).
    pub summary: String,
    /// Absolute file path on disk.
    pub file_path: PathBuf,
    /// Relative path from project root.
    pub rel_path: String,
    /// Project name.
    pub project: String,
    /// Document type: "markdown", "text", "pdf".
    pub doc_type: String,
    /// File size in bytes.
    pub size: u64,
    /// File modification time (RFC 3339).
    pub modified: String,
}

/// Parse a document file, returning structured document metadata.
pub fn parse_document(
    path: &Path,
    project: &str,
    root: &Path,
) -> Result<ParsedDocument, String> {
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

    let raw_content = std::fs::read_to_string(path)
        .map_err(|e| format!("read error: {e}"))?;

    let (doc_type, title, content) = match extension.as_str() {
        "md" | "markdown" => parse_markdown(&raw_content, &name),
        "txt" | "text" => ("text".to_string(), name.clone(), raw_content),
        "pdf" => return parse_pdf_stub(path, project, root),
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

/// Parse a markdown document: extract title from first H1 (`# Title`),
/// strip common formatting for plain text content.
fn parse_markdown(raw: &str, filename: &str) -> (String, String, String) {
    let doc_type = "markdown".to_string();

    let title = raw
        .lines()
        .find(|line| line.trim_start().starts_with("# "))
        .map(|line| line.trim_start().trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| {
            // Try first non-empty line as fallback title
            raw.lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| filename.to_string())
        });

    let content = strip_markdown(raw);

    (doc_type, title, content)
}

/// Basic markdown stripping: remove ATX headers (`#`), bold/italic markers,
/// inline code, links (keeping text), blockquotes, and horizontal rules.
fn strip_markdown(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let lines: Vec<&str> = raw.lines().collect();

    for line in &lines {
        let trimmed = line.trim();

        // Skip horizontal rules
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            continue;
        }

        // Skip HTML comments
        if trimmed.starts_with("<!--") {
            continue;
        }

        let mut processed = line.to_string();

        // Strip ATX headers: "## " or "# " → keep text
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

        // Strip bold/italic: **text**, *text*, __text__, _text_
        processed = strip_markers(&processed, "**");
        processed = strip_markers(&processed, "__");
        processed = strip_markers(&processed, "*");
        processed = strip_markers(&processed, "_");

        // Strip inline code: `text`
        processed = strip_markers(&processed, "`");

        // Strip blockquote prefix "> "
        if processed.trim_start().starts_with("> ") {
            processed = processed.trim_start().replacen("> ", "", 1);
        }

        // Strip image syntax: ![alt](url) → alt
        processed = strip_markdown_images(&processed);

        // Strip link syntax: [text](url) → text
        processed = strip_markdown_links(&processed);

        // Skip empty lines after processing
        let trimmed_processed = processed.trim();
        if trimmed_processed.is_empty() {
            // Don't add multiple consecutive empty lines
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

/// Strip paired markers (e.g., `**bold**` → `bold`).
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
            result = format!("{}{}{}", &result[..start], inner, &result[end + marker.len()..]);
        } else {
            break;
        }
    }
    result
}

/// Strip markdown image syntax: `![alt](url)` → `alt`
fn strip_markdown_images(text: &str) -> String {
    let re = regex::Regex::new(r"!\[([^\]]*)\]\([^)]*\)").unwrap();
    re.replace_all(text, "$1").to_string()
}

/// Strip markdown link syntax: `[text](url)` → `text`
fn strip_markdown_links(text: &str) -> String {
    let re = regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap();
    re.replace_all(text, "$1").to_string()
}

/// Stub PDF parser: returns filename-based metadata without PDF content.
fn parse_pdf_stub(
    path: &Path,
    project: &str,
    root: &Path,
) -> Result<ParsedDocument, String> {
    let rel_path = crate::infrastructure::scanner::rel_path(root, path);
    let doc_id = crate::domain::id::make_document_id(project, &rel_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.clone());

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

    Ok(ParsedDocument {
        doc_id,
        name: name.clone(),
        title: name,
        content: String::new(),
        summary: String::new(),
        file_path: path.to_path_buf(),
        rel_path,
        project: project.to_string(),
        doc_type: "pdf".to_string(),
        size,
        modified,
    })
}

// ---------------------------------------------------------------------------
// Tests
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
        // Horizontal rule is removed; surrounding blank lines are collapsed
        assert!(result.starts_with("Before"));
        assert!(result.ends_with("After"));
        // Ensure Before and After are present, separated appropriately
        assert!(!result.contains("---"), "horizontal rule should be removed");
    }
}
