//! @knowledge annotation extraction.
//!
//! Provides functions to parse structured details strings from CLI input
//! into key-value HashMaps. This is the shared parsing logic used by the
//! KnowledgeService convenience constructors.
//!
//! Also defines [`KnowledgeAnnotation`] — the parsed representation of a
//! `@knowledge` code comment annotation — and the regex-based extraction
//! from source files in multiple languages.
//!
//! The `parse_details` function re-uses the same parsing strategy as the
//! memory world's `parse_key_values` in `memory::handlers::mod`, ensuring
//! consistent behaviour across the project.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// KnowledgeAnnotation — parsed @knowledge code comment
// ---------------------------------------------------------------------------

/// Parsed representation of an `@knowledge` code-comment annotation.
///
/// Supports attributes: `domain`, `concept`, `definition`, `pitfall`, `experience`.
///
/// # Comment style examples
///
/// **Java / TypeScript / JavaScript / PHP (Javadoc block):**
/// ```java
/// /**
///  * @knowledge domain="支付" concept="ifCode"
///  * 支付渠道编码，用于路由到不同支付平台。
///  */
/// private String ifCode;
/// ```
///
/// **Python (docstring / line comment):**
/// ```python
/// # @knowledge domain="部署" concept="healthcheck" definition="服务健康检查端点"
/// ```
///
/// **Go / Rust / JavaScript (line comment):**
/// ```go
/// // @knowledge domain="支付" concept="channelExtra"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeAnnotation {
    /// Domain classification (e.g. "支付", "部署").
    pub domain: Option<String>,
    /// Concept name being defined.
    pub concept: Option<String>,
    /// Formal definition of the concept.
    pub definition: Option<String>,
    /// A pitfall / gotcha to watch out for.
    pub pitfall: Option<String>,
    /// Experience / lesson learned title.
    pub experience: Option<String>,
    /// Line number in the source file where the annotation was found.
    pub line_number: usize,
    /// Relative file path.
    pub file_path: String,
    /// The descriptive text following the `@knowledge` tag line (comment body).
    pub description: String,
}

// ---------------------------------------------------------------------------
// @knowledge annotation extraction
// ---------------------------------------------------------------------------

/// Extract all `@knowledge` annotations from a source file.
///
/// Supports comment styles across Java, TypeScript, JavaScript, Python,
/// Go, Rust, and PHP:
///
/// | Language  | Block comments      | Line comments |
/// |-----------|---------------------|---------------|
/// | Java      | `/** ... */`, `/*`  | `//`          |
/// | TS/JS     | `/** ... */`, `/*`  | `//`          |
/// | PHP       | `/** ... */`, `/*`  | `//`, `#`     |
/// | Python    | `""" ... """`       | `#`           |
/// | Go        | `/* ... */`         | `//`          |
/// | Rust      | `/* ... */`         | `//`, `///`   |
///
/// The function scans the entire source as plain text — it does not require
/// an AST parser. Block comments and `"""` docstrings are treated as
/// contiguous regions; single-line comments are matched individually.
pub fn extract_knowledge_annotations(
    source: &str,
    file_path: &str,
    _project: &str,
) -> Vec<KnowledgeAnnotation> {
    let mut annotations = Vec::new();

    // Step 1: Find all @knowledge occurrences in block-comment regions.
    // We use a simple state machine: track whether we're inside a block comment.
    // Block comments: /* ... */, /** ... */ (Javadoc), """ ... """ (Python).
    let lines: Vec<&str> = source.lines().collect();
    let mut in_block = false;
    let mut block_lines: Vec<usize> = Vec::new();
    let mut block_text = String::new();

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !in_block {
            // Detect block comment start
            if trimmed.starts_with("/*") || trimmed.starts_with("/**") {
                in_block = true;
                block_lines.push(idx + 1); // 1-indexed
                block_text.push_str(trimmed);
                block_text.push('\n');
                // Check if single-line block comment
                if trimmed.ends_with("*/") || trimmed.contains("*/") {
                    in_block = false;
                    if block_text.contains("@knowledge") {
                        if let Some(ann) =
                            parse_annotation_from_text(&block_text, file_path, block_lines[0])
                        {
                            annotations.push(ann);
                        }
                    }
                    block_lines.clear();
                    block_text.clear();
                }
            } else if (trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''"))
                && trimmed.matches("\"\"\"").count() >= 2 || trimmed.matches("'''").count() >= 2
            {
                // Single-line Python docstring with """..."""
                if trimmed.contains("@knowledge") {
                    if let Some(ann) = parse_annotation_from_text(trimmed, file_path, idx + 1) {
                        annotations.push(ann);
                    }
                }
            } else if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
                // Multi-line Python docstring start
                in_block = true;
                block_lines.push(idx + 1);
                block_text.push_str(trimmed);
                block_text.push('\n');
            }
        } else {
            // Inside a block comment — accumulate lines
            block_text.push_str(trimmed);
            block_text.push('\n');

            // Check for block comment end
            if trimmed.ends_with("*/") || trimmed.contains("*/")
                || trimmed.ends_with("\"\"\"") || trimmed.contains("\"\"\"")
                || trimmed.ends_with("'''") || trimmed.contains("'''")
            {
                in_block = false;
                if block_text.contains("@knowledge") {
                    if let Some(ann) =
                        parse_annotation_from_text(&block_text, file_path, block_lines[0])
                    {
                        annotations.push(ann);
                    }
                }
                block_lines.clear();
                block_text.clear();
            }
        }
    }

    // Handle unterminated block comment (treat as if it ended at EOF)
    if in_block && block_text.contains("@knowledge") && !block_lines.is_empty() {
        if let Some(ann) = parse_annotation_from_text(&block_text, file_path, block_lines[0]) {
            annotations.push(ann);
        }
    }

    // Step 2: Find @knowledge in single-line comments.
    // Match: /// ... @knowledge ...   or  // ... @knowledge ...  or  # ... @knowledge ...
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//"))
            .or_else(|| trimmed.strip_prefix('#'))
        {
            let rest = rest.trim();
            if rest.starts_with("@knowledge") {
                if let Some(ann) = parse_annotation_from_line(rest, file_path, idx + 1) {
                    // Don't double-count if already captured in a block comment
                    if !annotations.iter().any(|a| a.line_number == idx + 1) {
                        annotations.push(ann);
                    }
                }
            }
        }
    }

    annotations
}

/// Parse a single `@knowledge ...` line (from a line comment).
fn parse_annotation_from_line(
    line: &str,
    file_path: &str,
    line_number: usize,
) -> Option<KnowledgeAnnotation> {
    // line looks like: "@knowledge domain="支付" concept="ifCode""
    // or: "@knowledge domain="支付" concept="ifCode"  some description"
    let content = line.strip_prefix("@knowledge").unwrap_or(line).trim();

    let (attrs, description) = split_attrs_and_description(content);

    Some(KnowledgeAnnotation {
        domain: attrs.get("domain").cloned(),
        concept: attrs.get("concept").cloned(),
        definition: attrs.get("definition").cloned(),
        pitfall: attrs.get("pitfall").cloned(),
        experience: attrs.get("experience").cloned(),
        line_number,
        file_path: file_path.to_string(),
        description,
    })
}

/// Parse @knowledge from accumulated block comment text.
///
/// Returns the annotation and the line offset (1-indexed) of the
/// `@knowledge` line within the block text.
fn parse_annotation_from_text(
    text: &str,
    file_path: &str,
    _block_start_line: usize,
) -> Option<KnowledgeAnnotation> {
    // Find the @knowledge line within the block and compute its line offset
    let mut knowledge_line_offset = 0usize;
    let knowledge_line = text
        .lines()
        .enumerate()
        .find(|(i, l)| {
            if l.trim().starts_with("@knowledge") || l.contains("@knowledge") {
                knowledge_line_offset = *i;
                true
            } else {
                false
            }
        })
        .map(|(_, l)| l)?;

    let line_number = _block_start_line + knowledge_line_offset;

    // Extract from the @knowledge marker onwards
    let at_pos = knowledge_line.find("@knowledge")?;
    let from_knowledge = &knowledge_line[at_pos..];

    // Strip comment prefix symbols: *, //, #, etc.
    let cleaned = from_knowledge
        .strip_prefix("@knowledge")
        .unwrap_or(from_knowledge)
        .trim_start()
        .trim_start_matches('*')
        .trim_start()
        .trim_start_matches("*/")
        .trim_start()
        .trim_end_matches("*/")
        .trim();

    let (attrs, description) = split_attrs_and_description(cleaned);

    // Also collect description from subsequent lines in the block
    let mut desc = description;
    let mut after_knowledge = false;
    for line in text.lines() {
        if after_knowledge {
            let t = line
                .trim()
                .trim_start_matches('*')
                .trim_start()
                .trim_start_matches("*/")
                .trim();
            if !t.is_empty() && !t.starts_with('@') && !t.starts_with("*/") {
                if !desc.is_empty() {
                    desc.push(' ');
                }
                desc.push_str(t);
            }
        }
        if line.contains("@knowledge") {
            after_knowledge = true;
        }
    }

    Some(KnowledgeAnnotation {
        domain: attrs.get("domain").cloned(),
        concept: attrs.get("concept").cloned(),
        definition: attrs.get("definition").cloned(),
        pitfall: attrs.get("pitfall").cloned(),
        experience: attrs.get("experience").cloned(),
        line_number,
        file_path: file_path.to_string(),
        description: desc,
    })
}

/// Split a @knowledge content string into key="value" attributes and a
/// trailing description.
///
/// Example input: `domain="支付" concept="ifCode" 支付渠道编码说明`
/// Returns: `({"domain": "支付", "concept": "ifCode"}, "支付渠道编码说明")`
fn split_attrs_and_description(content: &str) -> (HashMap<String, String>, String) {
    let mut attrs = HashMap::new();
    let mut remaining = content.to_string();

    // Parse key="value" pairs using a simple state machine.
    // Pattern: key="value"
    loop {
        let trimmed = remaining.trim_start().to_string();
        if trimmed.is_empty() {
            break;
        }

        // Find the first `=` sign preceded by a word
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_lowercase();
            let after_eq = trimmed[eq_pos + 1..].to_string();

            // Value must start with a quote
            if let Some(after_quote) = after_eq.trim_start().strip_prefix('"') {
                // Find closing quote
                if let Some(close_pos) = after_quote.find('"') {
                    let value = after_quote[..close_pos].to_string();
                    if !key.is_empty() {
                        attrs.insert(key, value);
                    }
                    remaining = after_quote[close_pos + 1..].to_string();
                    continue;
                } else {
                    // Unterminated quote — treat rest as description
                    break;
                }
            } else if let Some(after_quote) = after_eq.trim_start().strip_prefix('\'') {
                if let Some(close_pos) = after_quote.find('\'') {
                    let value = after_quote[..close_pos].to_string();
                    if !key.is_empty() {
                        attrs.insert(key, value);
                    }
                    remaining = after_quote[close_pos + 1..].to_string();
                    continue;
                } else {
                    break;
                }
            } else {
                // key without quoted value — not a valid attr, treat rest as description
                break;
            }
        } else {
            // No `=` found — rest is description
            break;
        }
    }

    let description = remaining.trim().to_string();
    (attrs, description)
}

/// Parse a semicolon-separated `key: value` details string into a HashMap.
///
/// Pairs are separated by `;`, `\n`, or `,`. Keys and values are split on
/// the first `=` or `:`. Leading/trailing whitespace is trimmed. Keys are
/// lowercased so callers can match case-insensitively.
///
/// # Example
///
/// ```ignore
/// let m = parse_details("decision: 选择银盛; reason: 费率低; scope: PayService");
/// assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
/// assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
/// assert_eq!(m.get("scope"), Some(&"PayService".to_string()));
/// ```
pub fn parse_details(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in raw.split([';', '\n', ',']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // Split on first `=` or `:` to allow both delimiters.
        if let Some(pos) = part.find(['=', ':']) {
            let key = part[..pos].trim().to_lowercase();
            let value = part[pos + 1..].trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }
    map
}

/// Parse a details string as a value list (semicolon-separated, no key prefix).
///
/// Each semicolon-separated segment is treated as a value (not a key:value pair).
/// This is useful for parsing fields like `tags: tag1; tag2; tag3`.
///
/// # Example
///
/// ```ignore
/// let tags = parse_value_list("tag1; tag2; tag3");
/// assert_eq!(tags, vec!["tag1", "tag2", "tag3"]);
/// ```
pub fn parse_value_list(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_colon_values() {
        let m = parse_details("decision: 选择银盛; reason: 费率低; scope: PayService");
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        assert_eq!(m.get("scope"), Some(&"PayService".to_string()));
    }

    #[test]
    fn parse_equals_values() {
        let m = parse_details("key=value; other=42");
        assert_eq!(m.get("key"), Some(&"value".to_string()));
        assert_eq!(m.get("other"), Some(&"42".to_string()));
    }

    #[test]
    fn parse_mixed_delimiters() {
        let m = parse_details("a: 1, b: 2; c: 3");
        assert_eq!(m.get("a"), Some(&"1".to_string()));
        assert_eq!(m.get("b"), Some(&"2".to_string()));
        assert_eq!(m.get("c"), Some(&"3".to_string()));
    }

    #[test]
    fn parse_newlines() {
        let m = parse_details("title: Payment;\ndomain: 支付;\nseverity: warning");
        assert_eq!(m.get("title"), Some(&"Payment".to_string()));
        assert_eq!(m.get("domain"), Some(&"支付".to_string()));
        assert_eq!(m.get("severity"), Some(&"warning".to_string()));
    }

    #[test]
    fn parse_trims_whitespace() {
        let m = parse_details("  decision  :  选择银盛  ");
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
    }

    #[test]
    fn parse_empty_string() {
        let m = parse_details("");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_keys_are_lowercased() {
        let m = parse_details("Decision: 选择银盛; Reason: 费率低");
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        // Original case keys are NOT stored.
        assert_eq!(m.get("Decision"), None);
    }

    #[test]
    fn parse_no_colon_treats_as_key_without_value() {
        let m = parse_details("nokey");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_duplicate_keys_last_wins() {
        let m = parse_details("a: 1; a: 2");
        assert_eq!(m.get("a"), Some(&"2".to_string()));
    }

    #[test]
    fn parse_value_list_basic() {
        let v = parse_value_list("tag1; tag2; tag3");
        assert_eq!(v, vec!["tag1", "tag2", "tag3"]);
    }

    #[test]
    fn parse_value_list_empty() {
        let v = parse_value_list("");
        assert!(v.is_empty());
    }

    #[test]
    fn parse_value_list_single() {
        let v = parse_value_list("only");
        assert_eq!(v, vec!["only"]);
    }

    #[test]
    fn parse_complex_details() {
        // Realistic details from a Memorize CLI call
        let details = "decision: 选择银盛; reason: 费率低; \
                       scope: PayService, BusinessService; \
                       confidence: 0.9";
        let m = parse_details(details);
        assert_eq!(m.get("decision"), Some(&"选择银盛".to_string()));
        assert_eq!(m.get("reason"), Some(&"费率低".to_string()));
        // Note: "scope" contains commas which are also delimiters, so the
        // value gets truncated. This is expected — the caller should use a
        // different delimiter strategy for multi-value fields.
        assert_eq!(m.get("confidence"), Some(&"0.9".to_string()));
    }

    #[test]
    fn parse_details_roundtrip_with_service_constructors() {
        // Verify the parse_details output is compatible with entity constructors.
        let details = "title: Redis超时; severity: critical; domain: 支付; project: test";
        let m = parse_details(details);
        assert_eq!(m.get("title"), Some(&"Redis超时".to_string()));
        assert_eq!(m.get("severity"), Some(&"critical".to_string()));
        assert_eq!(m.get("domain"), Some(&"支付".to_string()));
        assert_eq!(m.get("project"), Some(&"test".to_string()));
    }
}
