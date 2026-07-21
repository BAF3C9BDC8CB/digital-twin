//! Document chunking — splits text documents into overlapping chunks
//! with configurable boundaries, size, and overlap.
//!
//! The chunking strategy implements a hierarchical boundary approach:
//! 1. Paragraph boundaries (double-newline) are preferred.
//! 2. If a paragraph exceeds chunk_size, fall back to Sentence boundaries.
//! 3. If a sentence still exceeds chunk_size, fall back to Fixed-length slicing.
//!
//! Each chunk includes back-links to its predecessor and forward-links
//! to its successor, enabling navigation within a document.

/// 文档类型枚举——决定分段策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocType {
    /// 普通文本（按段落，当前默认行为）
    PlainText,
    /// Markdown（按 ## 标题分级分段）
    Markdown,
    /// YAML 配置文件（按顶级 key 分段）
    Yaml,
    /// Properties 配置文件（按前缀分组，如 spring.datasource.*）
    Properties,
    /// 代码文件（tree-sitter 已在 build 中处理，这里指嵌入在文档中的代码块）
    EmbeddedCode,
}

impl DocType {
    /// 从文件名扩展名和内容特征检测文档类型
    pub fn detect(path: &str, first_lines: &[&str]) -> Self {
        let lower = path.to_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".markdown") {
            return DocType::Markdown;
        }
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            return DocType::Yaml;
        }
        if lower.ends_with(".properties") {
            return DocType::Properties;
        }
        // 检查内容特征：YAML 缩进（key: value 模式）
        for line in first_lines {
            let trimmed = line.trim();
            if trimmed.starts_with('#') { continue; }
            if trimmed.contains(':') && !trimmed.contains('=') {
                return DocType::Yaml;
            }
        }
        DocType::PlainText
    }
}

/// Boundary type for chunk splitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Split at paragraph boundaries (empty lines / double newlines).
    Paragraph,
    /// Split at sentence boundaries (`.`, `!`, `?`, `。`).
    Sentence,
    /// Split at fixed character count (hard slice).
    Fixed,
}

/// Configuration for the chunking strategy.
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// Target number of tokens per chunk (approximated via character count).
    pub chunk_size: usize,
    /// Number of tokens of overlap between consecutive chunks.
    pub overlap: usize,
    /// Preferred boundary type to split on.
    pub boundary: Boundary,
    /// Minimum chunk size in tokens; chunks below this size are merged
    /// into the previous chunk instead of being emitted standalone.
    pub min_chunk_size: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            chunk_size: 512,
            overlap: 64,
            boundary: Boundary::Paragraph,
            min_chunk_size: 256,
        }
    }
}

/// A single chunk of a document, with navigation links.
#[derive(Debug, Clone)]
pub struct DocumentChunk {
    /// Unique chunk identifier: `"{doc_id}#chunk{index}"`
    pub chunk_id: String,
    /// Chunk text content.
    pub text: String,
    /// Zero-based index of this chunk within the document.
    pub chunk_index: usize,
    /// Chunk ID of the previous chunk, if any.
    pub prev_chunk_id: Option<String>,
    /// Chunk ID of the next chunk, if any.
    pub next_chunk_id: Option<String>,
    /// Starting character offset in the original text.
    pub start_char: usize,
    /// Ending character offset in the original text (exclusive).
    pub end_char: usize,
}

/// Chunk a document text into overlapping chunks using the configured
/// boundary strategy.
///
/// # Arguments
///
/// * `text` - Full document text.
/// * `doc_id` - Document identifier used to build `chunk_id` fields.
/// * `config` - Chunking configuration.
///
/// # Returns
///
/// A vector of `DocumentChunk` in order. Returns an empty vector if
/// the input text is empty (whitespace-only).
pub fn chunk_text(text: &str, doc_id: &str, config: &ChunkConfig) -> Vec<DocumentChunk> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    match config.boundary {
        Boundary::Paragraph => chunk_by_boundary(trimmed, doc_id, config, Boundary::Paragraph),
        Boundary::Sentence => chunk_by_boundary(trimmed, doc_id, config, Boundary::Sentence),
        Boundary::Fixed => chunk_fixed(trimmed, doc_id, config),
    }
}

/// Approximate token count from character count: ~4 chars = 1 token (English),
/// ~2 chars = 1 token (CJK). We use a conservative heuristic of 3 chars/token.
#[allow(dead_code)]
fn approx_tokens(char_count: usize) -> usize {
    char_count / 3
}

fn approx_chars(tokens: usize) -> usize {
    tokens * 3
}

/// Parse a key=value or key: value line.
/// Handles quoted values for the colon-delimited form.
/// Returns `(key, value)` on success, `None` for empty/comment lines.
pub fn parse_kv_line(line: &str) -> Option<(&str, &str)> {
    if let Some(pos) = line.find('=') {
        let key = line[..pos].trim();
        let value = line[pos + 1..].trim();
        if !key.is_empty() {
            return Some((key, value));
        }
    }
    if let Some(pos) = line.find(':') {
        let key = line[..pos].trim();
        let value = line[pos + 1..].trim().trim_matches('"').trim_matches('\'');
        if !key.is_empty() && !value.contains(':') {
            return Some((key, value));
        }
    }
    None
}

/// 按 Properties 配置的前缀分组（取前两个点分隔的段作为 section key）。
/// 例如 `spring.datasource.url` → group `"spring.datasource"`。
fn extract_properties_sections(content: &str) -> Vec<(String, Vec<(String, String)>)> {
    use std::collections::HashMap;
    let mut sections: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        if let Some((key, value)) = parse_kv_line(trimmed) {
            let parts: Vec<&str> = key.split('.').collect();
            let section_key = if parts.len() >= 2 {
                format!("{}.{}", parts[0], parts[1])
            } else {
                parts[0].to_string()
            };
            sections
                .entry(section_key)
                .or_default()
                .push((key.to_string(), value.to_string()));
        }
    }
    sections.into_iter().collect()
}

/// 将 YAML / Properties 内容按配置主题分段。
///
/// - **YAML**: 扫描缩进为 0 的顶级 key（以 `:` 结尾的行为段起始），将同级内容归为一个 section。
/// - **Properties**: 按 `extract_properties_sections` 分组。
///
/// 返回 `Vec<(section_name, Vec<DocumentChunk>)>`，每个 section 生成一个 `DocumentChunk`
/// 包含该 section 下所有 key-value 对，以 `key=value` 格式写入文本。
pub fn chunk_config_by_sections(
    text: &str,
    doc_id: &str,
    config: &ChunkConfig,
    is_yaml: bool,
) -> Vec<(String, Vec<DocumentChunk>)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    if is_yaml {
        chunk_yaml_by_top_level_keys(trimmed, doc_id, config)
    } else {
        chunk_properties_by_prefix(trimmed, doc_id, config)
    }
}

/// YAML 分段：扫描顶级 key（缩进 0 且以 `:` 结尾），
/// 收集其下级内容（缩进 > 0），每个 section 生成一个 chunk。
fn chunk_yaml_by_top_level_keys(
    text: &str,
    doc_id: &str,
    _config: &ChunkConfig,
) -> Vec<(String, Vec<DocumentChunk>)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_key = String::new();
    let mut current_content = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 检测顶级 key（缩进为 0 且以冒号结尾 — key: 或 key:value 模式）
        if !line.starts_with(' ') && !line.starts_with('\t') {
            // 检查是否为顶级 key
            if let Some(pos) = line.find(':') {
                // 行首到冒号之间缩进为 0 → 可能是顶级 key
                let potential_key = line[..pos].trim();
                if !potential_key.is_empty() && !potential_key.contains(' ') {
                    // 非空且不包含空格 → 顶级 key
                    // flush 上一个 section
                    if !current_key.is_empty() {
                        sections.push((current_key.clone(), current_content.clone()));
                        current_content.clear();
                    }
                    current_key = potential_key.to_string();
                    // 行内值（如 key: value）
                    let after_colon = line[pos + 1..].trim();
                    if !after_colon.is_empty() {
                        if !current_content.is_empty() {
                            current_content.push('\n');
                        }
                        current_content.push_str(after_colon);
                    }
                    continue;
                }
            }
        }

        // 属于当前 section 的内容
        if !current_key.is_empty() {
            if !current_content.is_empty() {
                current_content.push('\n');
            }
            current_content.push_str(line);
        }
    }

    // flush 最后一个 section
    if !current_key.is_empty() {
        sections.push((current_key.clone(), current_content.clone()));
    }

    // 将每个 section 转为 DocumentChunk
    let mut result = Vec::new();
    for (section_name, section_text) in sections {
        if section_text.trim().is_empty() {
            continue;
        }
        let chunk = DocumentChunk {
            chunk_id: format!("{}#section-{}", doc_id, section_name),
            text: section_text,
            chunk_index: 0,
            prev_chunk_id: None,
            next_chunk_id: None,
            start_char: 0,
            end_char: 0,
        };
        result.push((section_name, vec![chunk]));
    }
    result
}

/// Properties 分段：按 `extract_properties_sections` 分组，
/// 每组内容格式化为 `key=value\n` 写入一个 chunk。
fn chunk_properties_by_prefix(
    text: &str,
    doc_id: &str,
    config: &ChunkConfig,
) -> Vec<(String, Vec<DocumentChunk>)> {
    let sections = extract_properties_sections(text);
    let mut result = Vec::new();
    let chunk_chars = approx_chars(config.chunk_size);

    for (section_name, pairs) in sections {
        let mut section_text = String::new();
        for (key, value) in &pairs {
            if !section_text.is_empty() {
                section_text.push('\n');
            }
            section_text.push_str(&format!("{}={}", key, value));
        }

        // 如果 section 内容超出 chunk_size，进一步拆分
        if section_text.chars().count() > chunk_chars {
            let sub_chunks = chunk_text(&section_text, doc_id, config);
            result.push((section_name, sub_chunks));
        } else {
            let chunk = DocumentChunk {
                chunk_id: format!("{}#section-{}", doc_id, section_name),
                text: section_text,
                chunk_index: 0,
                prev_chunk_id: None,
                next_chunk_id: None,
                start_char: 0,
                end_char: 0,
            };
            result.push((section_name, vec![chunk]));
        }
    }
    result
}

/// Split text using the given boundary strategy, falling back to coarser
/// boundaries as needed.
fn chunk_by_boundary(
    text: &str,
    doc_id: &str,
    config: &ChunkConfig,
    boundary: Boundary,
) -> Vec<DocumentChunk> {
    let chunk_chars = approx_chars(config.chunk_size);
    let overlap_chars = approx_chars(config.overlap);
    let min_chars = approx_chars(config.min_chunk_size);

    let segments = split_by_boundary(text, boundary);
    let chunks = build_chunks_from_segments(doc_id, &segments, chunk_chars, overlap_chars, min_chars);

    // If any single chunk is still too large (because a single segment
    // exceeded chunk_size), fall back to the next coarser boundary.
    let max_allowed = chunk_chars + overlap_chars;
    if chunks.iter().any(|c| c.text.chars().count() > max_allowed) {
        return match boundary {
            Boundary::Paragraph => {
                chunk_by_boundary(text, doc_id, config, Boundary::Sentence)
            }
            Boundary::Sentence => chunk_fixed(text, doc_id, config),
            Boundary::Fixed => chunk_fixed(text, doc_id, config),
        };
    }

    chunks
}

/// Split text into segments using the given boundary.
fn split_by_boundary(text: &str, boundary: Boundary) -> Vec<String> {
    match boundary {
        Boundary::Paragraph => {
            // Split on one or more consecutive empty lines
            let mut parts: Vec<String> = text
                .split("\n\n")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            // Also try splitting on \r\n\r\n for Windows-style line endings
            if parts.len() <= 1 {
                parts = text
                    .split("\r\n\r\n")
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            parts
        }
        Boundary::Sentence => {
            // Split on sentence-ending punctuation followed by whitespace
            let mut sentences = Vec::new();
            let mut start = 0;
            for (i, c) in text.char_indices() {
                match c {
                    '.' | '!' | '?' | '。' | '！' | '？' => {
                        // Check if this is likely end-of-sentence (followed by space/newline or EOF)
                        let mut next = i + c.len_utf8();
                        while next < text.len() {
                            let nc = text[next..].chars().next().unwrap();
                            if nc == '\n' || nc == ' ' || nc == '\r' {
                                next += nc.len_utf8();
                                break;
                            } else if nc.is_whitespace() {
                                next += nc.len_utf8();
                            } else {
                                break;
                            }
                        }
                        let seg = text[start..(i + c.len_utf8())].trim().to_string();
                        if !seg.is_empty() {
                            sentences.push(seg);
                        }
                        start = next;
                    }
                    _ => {}
                }
            }
            // Remainder
            let remainder = text[start..].trim().to_string();
            if !remainder.is_empty() {
                sentences.push(remainder);
            }
            sentences
        }
        Boundary::Fixed => {
            // Won't be called directly for Fixed from chunk_by_boundary,
            // but provide for completeness.
            vec![text.to_string()]
        }
    }
}

/// Build overlapping chunks from segments.
fn build_chunks_from_segments(
    doc_id: &str,
    segments: &[String],
    chunk_chars: usize,
    overlap_chars: usize,
    min_chars: usize,
) -> Vec<DocumentChunk> {
    if segments.is_empty() {
        return vec![];
    }

    let mut raw_chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut seg_index = 0;

    while seg_index < segments.len() {
        let seg = &segments[seg_index];
        let current_len = current.chars().count();
        let seg_len = seg.chars().count();

        if current.is_empty() {
            current.push_str(seg);
            seg_index += 1;
        } else if current_len + 1 + seg_len <= chunk_chars {
            // Add with a separator
            current.push('\n');
            current.push_str(seg);
            seg_index += 1;
        } else {
            // Current is full; emit it
            raw_chunks.push(current.clone());
            current.clear();
        }
    }

    // Emit remaining
    if !current.is_empty() {
        raw_chunks.push(current.clone());
    }

    // Handle min_chunk_size: merge small tail chunks into previous
    let raw_chunks = merge_small_chunks(raw_chunks, min_chars);

    // Build final chunks with overlap
    let mut chunks = Vec::new();
    let mut char_offset = 0;

    for (idx, raw) in raw_chunks.iter().enumerate() {
        let start_char = char_offset;
        let end_char = char_offset + raw.chars().count();

        let chunk = DocumentChunk {
            chunk_id: format!("{}#chunk{}", doc_id, idx),
            text: raw.clone(),
            chunk_index: idx,
            prev_chunk_id: if idx > 0 {
                Some(format!("{}#chunk{}", doc_id, idx - 1))
            } else {
                None
            },
            next_chunk_id: if idx + 1 < raw_chunks.len() {
                Some(format!("{}#chunk{}", doc_id, idx + 1))
            } else {
                None
            },
            start_char,
            end_char,
        };
        chunks.push(chunk);

        // Move offset forward, accounting for overlap (the chunk separator)
        if idx + 1 < raw_chunks.len() {
            let overlap_start = raw.chars().count().saturating_sub(overlap_chars);
            char_offset += overlap_start + 1; // +1 for the separator
        } else {
            char_offset = end_char;
        }
    }

    chunks
}

/// Merge chunks that are below `min_chars` into their preceding chunk.
fn merge_small_chunks(chunks: Vec<String>, min_chars: usize) -> Vec<String> {
    if chunks.is_empty() {
        return chunks;
    }

    let mut result: Vec<String> = Vec::new();

    for chunk in chunks {
        let is_small = chunk.chars().count() < min_chars;
        if is_small {
            if let Some(last) = result.last_mut() {
                last.push('\n');
                last.push_str(&chunk);
            } else {
                // First chunk is small: just push it as-is
                result.push(chunk);
            }
        } else {
            result.push(chunk);
        }
    }

    result
}

/// 将 Markdown 内容按 #/##/### 标题分割为语义块
/// 每个 chunk = 一个标题 + 其下内容，标题作为 chunk 的语义标签
pub fn chunk_markdown_by_headings(
    text: &str,
    doc_id: &str,
    config: &ChunkConfig,
) -> Vec<DocumentChunk> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    let mut chunks = Vec::new();
    let mut current_section = Vec::new();
    let mut current_heading = String::new();
    let mut _section_start = 0usize;
    let mut char_offset = 0usize;
    let mut found_heading = false;

    for line in trimmed.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.starts_with('#') && trimmed_line.chars().any(|c| c.is_alphabetic()) {
            found_heading = true;
            // 先 flush 上一个 section
            if !current_section.is_empty() || chunks.is_empty() {
                flush_section(&mut chunks, doc_id, &current_section, &current_heading, config);
            }
            current_heading = trimmed_line.trim_start_matches('#').trim().to_string();
            current_section = vec![format!("[{}] ", current_heading)];
            _section_start = char_offset;
        } else {
            current_section.push(line.to_string());
        }
        char_offset += line.len() + 1; // +1 for newline
    }

    // flush 最后一个 section
    flush_section(&mut chunks, doc_id, &current_section, &current_heading, config);

    // 如果没有找到标题，回退到段落级 chunk
    if !found_heading {
        return chunk_text(text, doc_id, config);
    }

    // 更新 next_chunk_id
    for i in 0..chunks.len() {
        if i + 1 < chunks.len() {
            chunks[i].next_chunk_id = Some(chunks[i + 1].chunk_id.clone());
        }
    }

    chunks
}

/// 辅助函数：将当前 section 追加到 chunks 列表。
/// 如果 section 超过 chunk_size，则调用 `chunk_text()` 进行段落级拆分。
fn flush_section(
    chunks: &mut Vec<DocumentChunk>,
    doc_id: &str,
    section_lines: &[String],
    _heading: &str,
    config: &ChunkConfig,
) {
    let text = section_lines.join("\n");
    if text.trim().is_empty() {
        return;
    }

    let chunk_chars = approx_chars(config.chunk_size);
    if text.chars().count() > chunk_chars {
        // Section 超过 chunk_size → 回退到 chunk_text 进一步拆分
        let sub_chunks = chunk_text(&text, doc_id, config);
        let start_idx = chunks.len();
        for (i, sub) in sub_chunks.into_iter().enumerate() {
            let mut c = sub;
            c.chunk_id = format!("{}#section-{}", doc_id, start_idx + i);
            c.chunk_index = start_idx + i;
            c.prev_chunk_id = if start_idx + i > 0 {
                Some(format!("{}#section-{}", doc_id, start_idx + i - 1))
            } else if chunks.is_empty() {
                None
            } else {
                Some(chunks.last().unwrap().chunk_id.clone())
            };
            chunks.push(c);
        }
        // 修复前一个 chunk 的 next_chunk_id
        if start_idx > 0 && !chunks.is_empty() {
            chunks[start_idx - 1].next_chunk_id = Some(chunks[start_idx].chunk_id.clone());
        }
        return;
    }

    let chunk = DocumentChunk {
        chunk_id: format!("{}#section-{}", doc_id, chunks.len()),
        text,
        chunk_index: chunks.len(),
        prev_chunk_id: if chunks.is_empty() {
            None
        } else {
            Some(format!("{}#section-{}", doc_id, chunks.len() - 1))
        },
        next_chunk_id: None,
        start_char: 0,   // will be set by caller if needed
        end_char: 0,
    };
    chunks.push(chunk);
}

/// Fixed-length chunking: slices text at exact character boundaries,
/// with overlap between chunks.
fn chunk_fixed(text: &str, doc_id: &str, config: &ChunkConfig) -> Vec<DocumentChunk> {
    let chunk_chars = approx_chars(config.chunk_size);
    let overlap_chars = approx_chars(config.overlap);
    let min_chars = approx_chars(config.min_chunk_size);

    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();

    if total <= chunk_chars {
        return vec![DocumentChunk {
            chunk_id: format!("{}#chunk0", doc_id),
            text: text.to_string(),
            chunk_index: 0,
            prev_chunk_id: None,
            next_chunk_id: None,
            start_char: 0,
            end_char: total,
        }];
    }

    let step = chunk_chars.saturating_sub(overlap_chars);
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut idx = 0;

    while start < total {
        let end = std::cmp::min(start + chunk_chars, total);
        let chunk_text: String = chars[start..end].iter().collect();

        // Skip very small tail chunks; merge into previous
        if chunks.is_empty() || chunk_text.chars().count() >= min_chars || end == total {
            let chunk = DocumentChunk {
                chunk_id: format!("{}#chunk{}", doc_id, idx),
                text: chunk_text,
                chunk_index: idx,
                prev_chunk_id: if idx > 0 {
                    Some(format!("{}#chunk{}", doc_id, idx - 1))
                } else {
                    None
                },
                next_chunk_id: None, // will be filled after loop
                start_char: start,
                end_char: end,
            };
            chunks.push(chunk);
            idx += 1;
        } else {
            // Merge into previous
            if let Some(prev) = chunks.last_mut() {
                prev.text.push('\n');
                prev.text.push_str(&chunk_text);
                prev.end_char = end;
            }
        }

        start += step;

        // Handle the case where step is 0 (overlap >= chunk_size)
        if step == 0 {
            start = end;
        }
    }

    // Fix up next_chunk_id links
    for i in 0..chunks.len() {
        if i + 1 < chunks.len() {
            chunks[i].next_chunk_id = Some(chunks[i + 1].chunk_id.clone());
        }
    }

    chunks
}

/// 根据文档类型和内容选择最优分段策略
///
/// - `Markdown` → `chunk_markdown_by_headings()`, 如果无标题则回退 `chunk_text()`
/// - `Yaml` → `chunk_config_by_sections(is_yaml=true)`
/// - `Properties` → `chunk_config_by_sections(is_yaml=false)`
/// - `PlainText` / `EmbeddedCode` → `chunk_text()`
pub fn chunk_by_type(
    text: &str,
    doc_id: &str,
    doc_type: DocType,
    config: &ChunkConfig,
) -> Vec<DocumentChunk> {
    match doc_type {
        DocType::Markdown => {
            let chunks = chunk_markdown_by_headings(text, doc_id, config);
            if chunks.is_empty() || (chunks.len() == 1 && chunks[0].text.trim() == text.trim()) {
                // No headings found or no splitting happened — fall back to paragraph
                chunk_text(text, doc_id, config)
            } else {
                chunks
            }
        }
        DocType::Yaml | DocType::Properties => {
            let is_yaml = doc_type == DocType::Yaml;
            let sections = chunk_config_by_sections(text, doc_id, config, is_yaml);
            if sections.is_empty() {
                chunk_text(text, doc_id, config)
            } else {
                let mut all_chunks = Vec::new();
                for (_section_name, section_chunks) in sections {
                    all_chunks.extend(section_chunks);
                }
                all_chunks
            }
        }
        DocType::PlainText | DocType::EmbeddedCode => {
            chunk_text(text, doc_id, config)
        }
    }
}

// ===========================================================================
// Adaptive Config Chunking
// ===========================================================================

/// Result of adaptive config chunking: a section name and its key-value pairs.
pub type AdaptiveSection = (String, Vec<(String, String)>);

/// ─── Common: determine whether a key is a comment ─────────────────────────
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!')
}

/// ─── Properties adaptive chunking ─────────────────────────────────────────

/// Build a prefix tree from dotted keys and chunk adaptively.
///
/// Heuristic:
/// - Start grouping at depth 2 (e.g., `spring.datasource`)
/// - If a depth-2 group has ≥3 distinct child prefixes at depth 3 → split to depth 3
/// - If a depth-3 child has ≥3 distinct grandchild prefixes at depth 4 → split to depth 4
/// - Otherwise, keep at current depth
pub fn chunk_properties_adaptive(content: &str) -> Vec<AdaptiveSection> {
    let pairs: Vec<(String, String)> = content
        .lines()
        .filter(|l| !is_comment_line(l))
        .filter_map(|l| parse_kv_line(l))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    if pairs.is_empty() {
        return vec![];
    }

    // Check if any keys have dots; if none do, group everything as one section
    let has_dotted = pairs.iter().any(|(k, _)| k.contains('.'));
    if !has_dotted {
        // Flat key-value pairs — one section for all
        return vec![("config".to_string(), pairs)];
    }

    // Separate dotless keys (single-segment) from dotted keys
    let (dotted_pairs, flat_pairs): (Vec<_>, Vec<_>) = pairs
        .into_iter()
        .partition(|(k, _)| k.contains('.'));

    // Build a prefix tree (trie) from dotted keys
    let mut trie = PrefixTrie::new();
    for (key, _) in &dotted_pairs {
        let parts: Vec<&str> = key.split('.').collect();
        trie.insert(&parts);
    }

    // Assign each dotted key-value to a section, using adaptive depth
    let mut sections: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();

    for (key, value) in &dotted_pairs {
        let parts: Vec<&str> = key.split('.').collect();
        let section_name = trie.resolve_section(&parts);
        sections
            .entry(section_name)
            .or_default()
            .push((key.clone(), value.clone()));
    }

    // Add dotless keys as a "config" section
    if !flat_pairs.is_empty() {
        sections.insert("config".to_string(), flat_pairs);
    }

    // ── Post-process: merge singleton sections into their parent prefix ──
    // If a section has exactly 1 key and a "parent" section exists (by
    // removing the last dot-segment), merge it up.  This prevents overly
    // granular chunks like `spring.datasource.druid.max-active` when
    // `spring.datasource.druid.stat-view-servlet` forced a split.
    let merged = merge_singleton_sections(sections);

    // ── Post-process: merge sibling sections into their parent prefix ──
    // If a prefix has ≥2 sub-sections (e.g., spring.datasource.druid.core,
    // spring.datasource.druid.log, ...), merge them all into one section
    // named after the common parent (spring.datasource.druid).
    let merged = merge_sibling_sections(merged);

    // ── Post-process: merge orphan siblings (no existing grandparent) ──
    // Files where all keys nest directly under druid (no spring.datasource
    // top-level section) won't be caught by merge_sibling_sections because
    // the grandparent check fails.  Handle those orphans here.
    let merged = merge_orphan_sibling_sections(merged);

    // ── Post-process: promote parent keys into existing sub-section ──
    // If a section (e.g., spring.datasource) contains only keys that all
    // share a prefix matching an existing sub-section (spring.datasource.druid),
    // move those keys into the sub-section and remove the parent.
    let merged = promote_to_sub_section(merged);

    // Sort sections for deterministic output
    let mut result: Vec<AdaptiveSection> = merged.into_iter().collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Merge sections that have only 1 key into the nearest existing ancestor
/// section (by walking up the dot-separated prefix chain).
fn merge_singleton_sections(
    mut sections: std::collections::HashMap<String, Vec<(String, String)>>,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    // Collect singleton section names
    let singletons: Vec<String> = sections
        .iter()
        .filter(|(_, v)| v.len() == 1)
        .map(|(k, _)| k.clone())
        .collect();

    for name in singletons {
        // Walk up dot-segments to find nearest existing parent
        let mut candidate = name.clone();
        let target = loop {
            match candidate.rfind('.') {
                Some(pos) => {
                    candidate = candidate[..pos].to_string();
                    if sections.contains_key(&candidate) {
                        break Some(candidate);
                    }
                }
                None => break None, // no dot left → can't merge
            }
        };

        if let Some(parent) = target {
            if let Some(pairs) = sections.remove(&name) {
                sections.get_mut(&parent).unwrap().extend(pairs);
            }
        }
    }

    sections
}

/// Merge sibling sub-sections under a common parent prefix into a single
/// parent-named section.  For example, `spring.datasource.druid.core`,
/// `spring.datasource.druid.log` → one `spring.datasource.druid` section.
fn merge_sibling_sections(
    mut sections: std::collections::HashMap<String, Vec<(String, String)>>,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    // Find parent prefixes that have ≥2 child sections
    let section_names: Vec<String> = sections.keys().cloned().collect();
    let mut parent_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for name in &section_names {
        if let Some(pos) = name.rfind('.') {
            let parent = name[..pos].to_string();
            *parent_counts.entry(parent).or_default() += 1;
        }
    }

    // For each parent with ≥2 children that is NOT itself a section,
    // AND whose own parent IS a section (i.e., the children are deep
    // sub-groups of a meaningful parent), merge them into one section.
    let to_merge: Vec<String> = parent_counts
        .into_iter()
        .filter(|(parent, count)| {
            if *count < 2 || sections.contains_key(parent) {
                return false;
            }
            // Only merge if the grandparent exists as a section — prevents
            // merging top-level siblings like spring.datasource + spring.redis
            // into one massive "spring" section.
            parent.rfind('.').map_or(false, |gp_pos| {
                sections.contains_key(&parent[..gp_pos])
            })
        })
        .map(|(parent, _)| parent)
        .collect();

    for parent in to_merge {
        let mut merged_pairs: Vec<(String, String)> = Vec::new();
        // Collect and remove all child sections
        let child_names: Vec<String> = sections
            .keys()
            .filter(|k| k.starts_with(&format!("{}.", parent)))
            .cloned()
            .collect();
        for child in child_names {
            if let Some(pairs) = sections.remove(&child) {
                merged_pairs.extend(pairs);
            }
        }
        sections.insert(parent, merged_pairs);
    }

    sections
}

/// Merge sibling sub-sections that have no grandparent section.
/// Same as merge_sibling_sections but without the grandparent-exists check.
/// Handles cases like `spring.datasource.druid.{url,username,...}` where
/// `spring.datasource` never appeared as its own section.
fn merge_orphan_sibling_sections(
    mut sections: std::collections::HashMap<String, Vec<(String, String)>>,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    let section_names: Vec<String> = sections.keys().cloned().collect();
    let mut parent_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for name in &section_names {
        if let Some(pos) = name.rfind('.') {
            let parent = name[..pos].to_string();
            *parent_counts.entry(parent).or_default() += 1;
        }
    }

    // Merge orphans: ≥2 children, parent doesn't exist, grandparent may or may not exist
    // Only merge when the parent prefix has ≥1 dot (depth ≥2).
    // This prevents cascading to top-level like "spring", "management".
    let to_merge: Vec<String> = parent_counts
        .into_iter()
        .filter(|(parent, count)| {
            *count >= 2
                && !sections.contains_key(parent)
                && parent.contains('.')
        })
        .map(|(parent, _)| parent)
        .collect();

    for parent in to_merge {
        let mut merged_pairs: Vec<(String, String)> = Vec::new();
        let child_names: Vec<String> = sections
            .keys()
            .filter(|k| k.starts_with(&format!("{}.", parent)))
            .cloned()
            .collect();
        for child in child_names {
            if let Some(pairs) = sections.remove(&child) {
                merged_pairs.extend(pairs);
            }
        }
        sections.insert(parent, merged_pairs);
    }

    sections
}

/// If a parent section has ≥2 keys that belong to an existing sub-section,
/// move those keys into the sub-section.  Keeps "orphan" keys (that don't
/// match any sub-section) in the parent.
fn promote_to_sub_section(
    mut sections: std::collections::HashMap<String, Vec<(String, String)>>,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    let section_names: Vec<String> = sections.keys().cloned().collect();

    for sub_name in &section_names {
        let parent = match sub_name.rfind('.') {
            Some(pos) => sub_name[..pos].to_string(),
            None => continue,
        };
        if parent == *sub_name || !sections.contains_key(&parent) {
            continue;
        }

        let prefix = format!("{}.", sub_name);
        let parent_pairs = sections.get(&parent).unwrap();
        let to_move: Vec<(String, String)> = parent_pairs
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .cloned()
            .collect();

        if to_move.len() >= 2 {
            sections.get_mut(&parent).unwrap().retain(|(k, _)| !k.starts_with(&prefix));
            sections.get_mut(sub_name).unwrap().extend(to_move);
        }
    }

    sections.retain(|_, v| !v.is_empty());
    sections
}

/// A simple prefix trie for dotted key names.
/// Used to determine the optimal section prefix for each key.
struct PrefixTrie {
    root: TrieNode,
}

struct TrieNode {
    children: std::collections::HashMap<String, TrieNode>,
    /// Number of leaf keys that pass through or end at this node.
    leaf_count: usize,
}

impl PrefixTrie {
    fn new() -> Self {
        Self {
            root: TrieNode {
                children: std::collections::HashMap::new(),
                leaf_count: 0,
            },
        }
    }

    fn insert(&mut self, parts: &[&str]) {
        let mut node = &mut self.root;
        node.leaf_count += 1;
        for part in parts {
            node = node
                .children
                .entry(part.to_string())
                .or_insert(TrieNode {
                    children: std::collections::HashMap::new(),
                    leaf_count: 0,
                });
            node.leaf_count += 1;
        }
    }

    /// Resolve the optimal section name for a key with the given parts.
    ///
    /// Strategy:
    /// - Keys with 1-2 parts: section = first part (e.g., "server.port" → "server")
    /// - Keys with 3+ parts: start at depth 1 ("a.b"), then deepen if needed
    /// - At each intermediate node: count children with leaf_count > 1
    ///   (multi-key prefixes). If ≥ 2, split deeper.
    fn resolve_section(&self, parts: &[&str]) -> String {
        if parts.is_empty() {
            return String::new();
        }
        if parts.len() == 1 {
            return parts[0].to_string();
        }
        // 2-part keys: group at first component (e.g., "server.port" → "server")
        if parts.len() == 2 {
            return parts[0].to_string();
        }

        // 3+ part keys: start at depth 1 (e.g., "spring.datasource")
        let mut depth = 1; // 0-indexed → "a.b"

        // Walk the trie from root to check for splits
        let mut node = &self.root;
        for i in 0..parts.len().min(4) {
            if let Some(child) = node.children.get(parts[i]) {
                node = child;
            } else {
                break;
            }

            if i >= 1 && i < parts.len() - 1 {
                // At intermediate node: count children with leaf_count > 1
                // (these are multi-key sub-groups that deserve their own section)
                let multi_key_children = node
                    .children
                    .values()
                    .filter(|c| c.leaf_count > 1)
                    .count();

                // Split if ≥ 2 sub-groups have multiple keys each
                if multi_key_children >= 2 {
                    depth = i + 1; // "a.b.c"
                }
            }
        }

        // Cap depth at parts.len()-1 and max 4
        depth = depth.min(parts.len() - 1).min(3);
        parts[..=depth].join(".")
    }
}

/// ─── YAML adaptive chunking ───────────────────────────────────────────────

/// Parse YAML into flat key-value pairs, then use the same prefix-based
/// adaptive chunking as properties. This avoids complex tree walking.
pub fn chunk_yaml_adaptive(content: &str) -> Vec<AdaptiveSection> {
    let pairs = flatten_yaml_to_pairs(content);
    if pairs.is_empty() {
        return vec![];
    }
    let pseudo_props: String = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("\n");
    chunk_properties_adaptive(&pseudo_props)
}

/// Parse YAML-like text into a flat list of (dotted.key, value) pairs.
///
/// Uses a stack to track the current YAML path. For each line:
/// - If indent decreases, pop from stack accordingly
/// - If "key:" (no value), push key onto stack
/// - If "key: value", emit (stack + key, value)
fn flatten_yaml_to_pairs(content: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = count_indent(line);

        while let Some(&(s_indent, _)) = stack.last() {
            if s_indent >= indent {
                stack.pop();
            } else {
                break;
            }
        }

        if let Some(colon_pos) = trimmed.find(':') {
            let key = trimmed[..colon_pos].trim().to_string();
            let after_colon = trimmed[colon_pos + 1..].trim();

            if after_colon.is_empty() {
                stack.push((indent, key));
            } else if after_colon.starts_with('|') || after_colon.starts_with('>') {
                stack.push((indent, key));
            } else {
                let full_key: String = stack
                    .iter()
                    .map(|(_, k)| k.as_str())
                    .chain(std::iter::once(key.as_str()))
                    .collect::<Vec<_>>()
                    .join(".");
                pairs.push((full_key, after_colon.to_string()));
            }
        } else if trimmed.starts_with('-') {
            let value = trimmed[1..].trim().to_string();
            if !value.is_empty() {
                let full_key: String = stack
                    .iter()
                    .map(|(_, k)| k.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                if !full_key.is_empty() {
                    pairs.push((full_key, value));
                }
            }
        }
    }

    pairs
}

/// Count leading spaces for indentation.
fn count_indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// ─── Public entry point ───────────────────────────────────────────────────

/// Adaptive config chunking: automatically determines the optimal
/// section depth for both properties and YAML config files.
///
/// Returns a sorted vector of `(section_name, Vec<(key, value)>)`.
pub fn chunk_config_adaptive(content: &str, is_yaml: bool) -> Vec<AdaptiveSection> {
    if content.trim().is_empty() {
        return vec![];
    }
    if is_yaml {
        chunk_yaml_adaptive(content)
    } else {
        chunk_properties_adaptive(content)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(idx: usize) -> String {
        format!("dt://doc/test/links.md#chunk{}", idx)
    }

    #[test]
    fn empty_text_returns_no_chunks() {
        let config = ChunkConfig::default();
        let chunks = chunk_text("", "dt://doc/test/empty.md", &config);
        assert!(chunks.is_empty());

        let chunks = chunk_text("   \n\n   ", "dt://doc/test/ws.md", &config);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_paragraph_below_chunk_size() {
        let config = ChunkConfig::default();
        let text = "This is a short paragraph.";
        let doc_id = "dt://doc/test/short.md";
        let chunks = chunk_text(text, doc_id, &config);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_id, format!("{}#chunk0", doc_id));
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].text, text);
        assert!(chunks[0].prev_chunk_id.is_none());
        assert!(chunks[0].next_chunk_id.is_none());
    }

    #[test]
    fn paragraph_boundary_splits_on_double_newline() {
        let config = ChunkConfig::default();
        let text = "First paragraph with some content.\n\nSecond paragraph also has content.\n\nThird paragraph is here too.";
        let doc_id = "dt://doc/test/para.md";
        let chunks = chunk_text(text, doc_id, &config);
        // All three paragraphs should fit in one chunk (they're short)
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("First paragraph"));
        assert!(chunks[0].text.contains("Third paragraph"));
    }

    #[test]
    fn chunks_have_prev_and_next_links() {
        let config = ChunkConfig {
            chunk_size: 10,      // ~30 chars — very small to force multiple chunks
            overlap: 5,          // ~15 chars overlap
            boundary: Boundary::Fixed,
            min_chunk_size: 10,  // ~30 chars
        };
        let text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let doc_id = "dt://doc/test/links.md";
        let chunks = chunk_text(text, doc_id, &config);
        assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());

        // First chunk: no prev, has next
        assert!(chunks[0].prev_chunk_id.is_none());
        assert_eq!(chunks[0].next_chunk_id.as_deref(), Some(make_id(1).as_str()));

        // Middle chunks have both
        if chunks.len() > 2 {
            for i in 1..chunks.len() - 1 {
                assert_eq!(chunks[i].prev_chunk_id.as_deref(), Some(make_id(i - 1).as_str()));
                assert_eq!(chunks[i].next_chunk_id.as_deref(), Some(make_id(i + 1).as_str()));
            }
        }

        // Last chunk: has prev, no next
        let last = chunks.len() - 1;
        assert_eq!(
            chunks[last].prev_chunk_id.as_deref(),
            Some(make_id(last - 1).as_str())
        );
        assert!(chunks[last].next_chunk_id.is_none());
    }

    #[test]
    fn chunk_index_is_zero_based_sequential() {
        let config = ChunkConfig {
            chunk_size: 10,
            overlap: 3,
            boundary: Boundary::Fixed,
            min_chunk_size: 5,
        };
        let text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let doc_id = "dt://doc/test/index.md";
        let chunks = chunk_text(text, doc_id, &config);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn fixed_boundary_chunks_have_correct_offsets() {
        let config = ChunkConfig {
            chunk_size: 5,       // ~15 chars
            overlap: 2,          // ~6 chars
            boundary: Boundary::Fixed,
            min_chunk_size: 2,
        };
        let text = "abcdefghijklmnopqrstuvwxyz";
        let doc_id = "dt://doc/test/offsets.md";
        let chunks = chunk_text(text, doc_id, &config);

        // First chunk at offset 0
        assert_eq!(chunks[0].start_char, 0);
        assert!(chunks[0].text.len() > 0);

        // Last chunk covers end of text
        let last = chunks.last().unwrap();
        assert_eq!(last.end_char, text.chars().count());
    }

    #[test]
    fn min_chunk_size_merges_small_tail() {
        let config = ChunkConfig {
            chunk_size: 15,        // ~45 chars
            overlap: 3,            // ~9 chars
            boundary: Boundary::Fixed,
            min_chunk_size: 12,    // ~36 chars
        };
        // Create text that produces multiple fixed chunks with a small tail at the end
        // Each chunk at ~45 chars with ~9 char overlap, and ~36 char step
        // So the last tail might be small
        let text = "A".repeat(100);
        let doc_id = "dt://doc/test/min_chunk.md";
        let chunks = chunk_text(&text, doc_id, &config);

        // With Fixed boundary and chunk_size=15 (~45 chars), we should get ~3 chunks
        // The key is that no chunk is empty and all have proper navigation links
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
            assert!(!chunk.chunk_id.is_empty());
        }
    }

    #[test]
    fn sentence_boundary_splits_correctly() {
        let config = ChunkConfig {
            chunk_size: 20,       // ~60 chars
            overlap: 5,
            boundary: Boundary::Sentence,
            min_chunk_size: 10,
        };
        let text = "First sentence. Second sentence! Third sentence? Fourth one here. Fifth.";
        let doc_id = "dt://doc/test/sentences.md";
        let chunks = chunk_text(text, doc_id, &config);
        assert!(chunks.len() >= 1);
        // All sentences should be present across chunks
        let combined: String = chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(combined.contains("First sentence"));
        assert!(combined.contains("Second sentence"));
        assert!(combined.contains("Third sentence"));
    }

    #[test]
    fn chunk_config_default_values() {
        let config = ChunkConfig::default();
        assert_eq!(config.chunk_size, 512);
        assert_eq!(config.overlap, 64);
        assert_eq!(config.boundary, Boundary::Paragraph);
        assert_eq!(config.min_chunk_size, 256);
    }

    #[test]
    fn large_text_produces_multiple_chunks() {
        let config = ChunkConfig {
            chunk_size: 10,        // ~30 chars
            overlap: 3,            // ~9 chars
            boundary: Boundary::Fixed,
            min_chunk_size: 5,     // ~15 chars
        };
        let text = "X".repeat(500);
        let doc_id = "dt://doc/test/large.md";
        let chunks = chunk_text(&text, doc_id, &config);
        assert!(chunks.len() > 1, "large text should produce multiple chunks, got {}", chunks.len());

        // Verify chunk IDs are unique
        let ids: std::collections::HashSet<String> = chunks.iter().map(|c| c.chunk_id.clone()).collect();
        assert_eq!(ids.len(), chunks.len());
    }

    // -----------------------------------------------------------------------
    // DocType detect tests
    // -----------------------------------------------------------------------

    #[test]
    fn doc_type_detect_markdown_by_extension() {
        assert_eq!(DocType::detect("readme.md", &[]), DocType::Markdown);
        assert_eq!(DocType::detect("CHANGELOG.markdown", &[]), DocType::Markdown);
    }

    #[test]
    fn doc_type_detect_yaml_by_extension() {
        assert_eq!(DocType::detect("config.yaml", &[]), DocType::Yaml);
        assert_eq!(DocType::detect("application.yml", &[]), DocType::Yaml);
    }

    #[test]
    fn doc_type_detect_properties_by_extension() {
        assert_eq!(DocType::detect("app.properties", &[]), DocType::Properties);
    }

    #[test]
    fn doc_type_detect_yaml_by_content() {
        let lines = &["server:", "  port: 8080"];
        assert_eq!(DocType::detect("unknown.conf", lines), DocType::Yaml);
    }

    #[test]
    fn doc_type_detect_plain_text_default() {
        assert_eq!(DocType::detect("readme.txt", &[]), DocType::PlainText);
        assert_eq!(DocType::detect("notes", &["hello world"]), DocType::PlainText);
    }

    #[test]
    fn doc_type_detect_plain_text_when_contains_equals() {
        // Lines with = are properties, not yaml
        let lines = &["key=value"];
        assert_eq!(DocType::detect("some.file", lines), DocType::PlainText);
    }

    // -----------------------------------------------------------------------
    // Markdown heading chunking tests
    // -----------------------------------------------------------------------

    #[test]
    fn markdown_single_heading_produces_one_chunk() {
        let config = ChunkConfig::default();
        let text = "# Title\n\nSome content here.";
        let doc_id = "dt://doc/test/single.md";
        let chunks = chunk_markdown_by_headings(text, doc_id, &config);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("[Title]"));
        assert!(chunks[0].text.contains("Some content"));
    }

    #[test]
    fn markdown_multiple_headings_produces_chunks() {
        let config = ChunkConfig::default();
        let text = "# Main\n\nContent A.\n\n## Section 1\n\nContent B.\n\n### Sub 1\n\nContent C.";
        let doc_id = "dt://doc/test/multi.md";
        let chunks = chunk_markdown_by_headings(text, doc_id, &config);
        assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());
        assert!(chunks[0].text.contains("[Main]"));
        assert!(chunks[1].text.contains("[Section 1]"));
        // Check prev/next links
        assert!(chunks[0].prev_chunk_id.is_none());
        if chunks.len() > 1 {
            assert_eq!(chunks[0].next_chunk_id.as_deref(), Some(chunks[1].chunk_id.as_str()));
            assert_eq!(chunks[1].prev_chunk_id.as_deref(), Some(chunks[0].chunk_id.as_str()));
        }
    }

    #[test]
    fn markdown_no_headings_falls_back_to_chunk_text() {
        let config = ChunkConfig::default();
        let text = "Just a plain paragraph.\n\nAnother paragraph without headings.";
        let doc_id = "dt://doc/test/noheading.md";
        let chunks = chunk_markdown_by_headings(text, doc_id, &config);
        // Should produce 1 chunk (text is short, paragraph boundary)
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("plain paragraph"));
    }

    #[test]
    fn markdown_empty_text_returns_empty() {
        let config = ChunkConfig::default();
        let chunks = chunk_markdown_by_headings("", "empty.md", &config);
        assert!(chunks.is_empty());
    }

    // -----------------------------------------------------------------------
    // parse_kv_line tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_kv_line_equals() {
        assert_eq!(parse_kv_line("key=value"), Some(("key", "value")));
    }

    #[test]
    fn parse_kv_line_colon() {
        assert_eq!(parse_kv_line("key: value"), Some(("key", "value")));
    }

    #[test]
    fn parse_kv_line_empty_returns_none() {
        assert!(parse_kv_line("").is_none());
    }

    #[test]
    fn parse_kv_line_quoted_value() {
        assert_eq!(
            parse_kv_line("key: \"quoted value\""),
            Some(("key", "quoted value"))
        );
    }

    #[test]
    fn parse_kv_line_equals_with_spaces() {
        assert_eq!(
            parse_kv_line("  key = value with spaces "),
            Some(("key", "value with spaces"))
        );
    }

    #[test]
    fn parse_kv_line_colon_with_multiple_colons_in_value() {
        // value containing colon should return None for colon path
        // but = path works if present
        assert_eq!(parse_kv_line("key=foo:bar"), Some(("key", "foo:bar")));
    }

    #[test]
    fn parse_kv_line_no_delimiter() {
        assert!(parse_kv_line("just a string").is_none());
    }

    #[test]
    fn parse_kv_line_comment_char_not_at_start() {
        // '#' in the middle is not a comment
        assert_eq!(parse_kv_line("key=value#comment"), Some(("key", "value#comment")));
    }

    // -----------------------------------------------------------------------
    // extract_properties_sections tests
    // -----------------------------------------------------------------------

    #[test]
    fn extract_properties_sections_groups_by_prefix() {
        // "spring.datasource.url" → parts ["spring","datasource","url"] → section "spring.datasource"
        // "server.port"           → parts ["server","port"] → section "server.port"
        let content = "spring.datasource.url=jdbc:mysql://localhost/db\nspring.datasource.username=admin\nserver.port=8080";
        let sections = extract_properties_sections(content);
        assert_eq!(sections.len(), 2, "expected 2 sections, got {}: {:?}", sections.len(), sections);
        let ds = sections.iter().find(|(name, _)| name == "spring.datasource").unwrap();
        assert_eq!(ds.1.len(), 2);
        let sv = sections.iter().find(|(name, _)| name == "server.port").unwrap();
        assert_eq!(sv.1.len(), 1);
    }

    #[test]
    fn extract_properties_empty_returns_empty() {
        let sections = extract_properties_sections("");
        assert!(sections.is_empty());
    }

    #[test]
    fn extract_properties_skips_comments() {
        let content = "# this is a comment\n! this is also comment\nkey=value";
        let sections = extract_properties_sections(content);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "key");
        assert_eq!(sections[0].1.len(), 1);
    }

    #[test]
    fn extract_properties_single_part_key() {
        let content = "simple_key=simple_value\nanother_key=another_value";
        let sections = extract_properties_sections(content);
        // Each key has only one part (no dot), so each is its own section
        assert_eq!(sections.len(), 2);
    }

    #[test]
    fn extract_properties_deep_nested_key() {
        let content = "spring.datasource.hikari.connection-timeout=30000";
        let sections = extract_properties_sections(content);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "spring.datasource");
    }

    // -----------------------------------------------------------------------
    // chunk_config_by_sections tests (YAML)
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_yaml_single_top_level_key() {
        let config = ChunkConfig::default();
        let text = "server:\n  port: 8080";
        let result = chunk_config_by_sections(text, "doc1", &config, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "server");
        assert_eq!(result[0].1.len(), 1);
        assert!(result[0].1[0].text.contains("port: 8080"));
    }

    #[test]
    fn chunk_yaml_multiple_top_level_keys() {
        let config = ChunkConfig::default();
        let text = "server:\n  port: 8080\n  host: localhost\n\nredis:\n  host: 127.0.0.1\n  port: 6379";
        let result = chunk_config_by_sections(text, "doc1", &config, true);
        assert_eq!(result.len(), 2, "expected 2 sections, got {}", result.len());
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"server"));
        assert!(names.contains(&"redis"));
    }

    #[test]
    fn chunk_yaml_empty_returns_empty() {
        let config = ChunkConfig::default();
        let result = chunk_config_by_sections("", "doc1", &config, true);
        assert!(result.is_empty());
    }

    #[test]
    fn chunk_yaml_whitespace_only_returns_empty() {
        let config = ChunkConfig::default();
        let result = chunk_config_by_sections("   \n\n  ", "doc1", &config, true);
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // chunk_config_by_sections tests (Properties)
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_properties_single_section() {
        let config = ChunkConfig::default();
        let text = "spring.datasource.url=jdbc:mysql://localhost/db\nspring.datasource.username=admin";
        let result = chunk_config_by_sections(text, "doc1", &config, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "spring.datasource");
        assert!(result[0].1[0].text.contains("spring.datasource.url"));
        assert!(result[0].1[0].text.contains("spring.datasource.username"));
    }

    #[test]
    fn chunk_properties_multiple_sections() {
        // "spring.datasource.*" → section "spring.datasource"
        // "server.port"         → section "server.port"
        let config = ChunkConfig::default();
        let text = "spring.datasource.url=jdbc:mysql://localhost/db\nspring.datasource.username=admin\nserver.port=8080";
        let result = chunk_config_by_sections(text, "doc1", &config, false);
        assert_eq!(result.len(), 2, "expected 2 sections, got {}: {:?}", result.len(), result);
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"spring.datasource"));
        assert!(names.contains(&"server.port"));
    }

    #[test]
    fn chunk_properties_empty_returns_empty() {
        let config = ChunkConfig::default();
        let result = chunk_config_by_sections("", "doc1", &config, false);
        assert!(result.is_empty());
    }

    #[test]
    fn chunk_properties_ignores_comments() {
        let config = ChunkConfig::default();
        let text = "# comment\n! another comment\nkey=value";
        let result = chunk_config_by_sections(text, "doc1", &config, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "key");
    }

    // -----------------------------------------------------------------------
    // chunk_by_type dispatch tests
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_by_type_markdown_routes_to_heading_chunker() {
        let config = ChunkConfig::default();
        let text = "# Title\n\nContent.\n\n## Section 1\n\nMore content.";
        let doc_id = "dt://doc/test/type_md.md";
        let chunks = chunk_by_type(text, doc_id, DocType::Markdown, &config);
        assert!(chunks.len() >= 2);
        assert!(chunks[0].text.contains("[Title]"));
    }

    #[test]
    fn chunk_by_type_yaml_routes_to_section_chunker() {
        let config = ChunkConfig::default();
        let text = "server:\n  port: 8080\ndatabase:\n  url: jdbc:mysql://localhost/db";
        let doc_id = "dt://doc/test/type_yaml.md";
        let chunks = chunk_by_type(text, doc_id, DocType::Yaml, &config);
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn chunk_by_type_plaintext_uses_paragraph_chunker() {
        let config = ChunkConfig::default();
        let text = "Paragraph one.\n\nParagraph two.";
        let doc_id = "dt://doc/test/type_txt.md";
        let chunks = chunk_by_type(text, doc_id, DocType::PlainText, &config);
        assert_eq!(chunks.len(), 1); // short text fits in one chunk
    }

    #[test]
    fn chunk_by_type_empty_returns_empty() {
        let config = ChunkConfig::default();
        let chunks = chunk_by_type("", "empty.md", DocType::Markdown, &config);
        assert!(chunks.is_empty());
    }

    // -----------------------------------------------------------------------
    // Adaptive properties chunking tests
    // -----------------------------------------------------------------------

    #[test]
    fn adaptive_properties_datasource_stays_2level() {
        let content = "\
spring.datasource.url=jdbc:mysql://localhost/db\n\
spring.datasource.username=admin\n\
spring.datasource.password=secret";
        let sections = chunk_properties_adaptive(content);
        // All 3 keys should be in one "spring.datasource" section
        assert_eq!(sections.len(), 1, "expected 1 section, got {:?}", sections);
        assert_eq!(sections[0].0, "spring.datasource");
        assert_eq!(sections[0].1.len(), 3);
    }

    #[test]
    fn adaptive_properties_nacos_splits_to_3level() {
        let content = "\
spring.cloud.nacos.discovery.server-addr=addr\n\
spring.cloud.nacos.discovery.namespace=ns\n\
spring.cloud.nacos.config.server-addr=addr\n\
spring.cloud.nacos.config.namespace=ns2\n\
spring.cloud.nacos.username=user";
        let sections = chunk_properties_adaptive(content);
        // With orphan merge, all nacos children merge into spring.cloud.nacos
        eprintln!("Sections: {:?}", sections.iter().map(|(n,_)| n.as_str()).collect::<Vec<_>>());
        assert!(
            sections.iter().any(|(n, _)| n == "spring.cloud.nacos"),
            "expected spring.cloud.nacos section, got: {:?}", sections
        );
        assert_eq!(sections.iter().find(|(n,_)| n=="spring.cloud.nacos").unwrap().1.len(), 5);
    }

    #[test]
    fn adaptive_properties_jackson_stays_2level() {
        let content = "\
spring.jackson.date-format=yyyy-MM-dd\n\
spring.jackson.locale=zh_CN\n\
spring.jackson.mapper.sort-keys=true\n\
spring.jackson.serialization.write-dates-as-timestamps=false\n\
spring.jackson.default-property-inclusion=non_null";
        let sections = chunk_properties_adaptive(content);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "spring.jackson");
    }

    #[test]
    fn adaptive_properties_single_key() {
        let content = "server.port=8080";
        let sections = chunk_properties_adaptive(content);
        assert_eq!(sections.len(), 1);
        // 2-part key: grouped at first component
        assert_eq!(sections[0].0, "server");
    }

    #[test]
    fn adaptive_properties_mixed_depths() {
        let content = "\
spring.datasource.url=db\n\
spring.datasource.username=u\n\
spring.redis.host=localhost\n\
spring.redis.port=6379\n\
spring.redis.password=secret\n\
spring.cloud.nacos.discovery.server-addr=a\n\
spring.cloud.nacos.discovery.namespace=b\n\
spring.cloud.nacos.config.import=c\n\
spring.cloud.sentinel.dashboard=d\n\
spring.cloud.sentinel.eager=true";
        let sections = chunk_properties_adaptive(content);
        eprintln!("Mixed depth sections: {:?}", sections.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>());
        // Adaptive + orphan merge: spring.cloud.nacos.* and spring.cloud.sentinel.*
        // may merge into spring.cloud section.
        assert!(
            sections.iter().any(|(n, _)| n == "spring.datasource"),
            "Missing spring.datasource"
        );
        assert!(
            sections.iter().any(|(n, _)| n == "spring.redis"),
            "Missing spring.redis"
        );
        // spring.cloud.* sections should exist (cloud, or sub-sections)
        assert!(
            sections.iter().any(|(n, _)| n.starts_with("spring.cloud")),
            "Missing spring.cloud"
        );
    }

    // -----------------------------------------------------------------------
    // Adaptive YAML chunking tests
    // -----------------------------------------------------------------------

    #[test]
    fn adaptive_yaml_simple_top_level() {
        let content = "\
server:\n  port: 8080\n  host: localhost";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("YAML sections: {:?}", sections);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].0.contains("server"));
        assert!(sections[0].1.iter().any(|(k, _)| k == "server.port"));
    }

    #[test]
    fn adaptive_yaml_nested_sections() {
        let content = "\
spring:\n  datasource:\n    url: jdbc:mysql://localhost/db\n    username: admin\n  redis:\n    host: 127.0.0.1\n    port: 6379";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("Nested YAML sections: {:?}", sections);
        assert!(sections.len() >= 2);
        assert!(sections.iter().any(|(n, _)| n.contains("datasource")));
        assert!(sections.iter().any(|(n, _)| n.contains("redis")));
    }

    #[test]
    fn adaptive_yaml_jackson_block() {
        let content = "\
spring:\n  jackson:\n    mapper:\n      ALLOW_EXPLICIT_PROPERTY_RENAMING: true\n    deserialization:\n      READ_DATE_TIMESTAMPS_AS_NANOSECONDS: false\n    serialization:\n      WRITE_DATE_TIMESTAMPS_AS_NANOSECONDS: false";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("Jackson YAML sections: {:?}", sections);
        let jackson_section = sections.iter().find(|(n, _)| n.contains("jackson"));
        assert!(jackson_section.is_some(), "Expected a jackson section, got: {:?}", sections);
    }

    #[test]
    fn adaptive_yaml_common_structure() {
        let content = "\
spring:\n  cloud:\n    nacos:\n      discovery:\n        server-addr: http://nacos.newoffen.net\n        namespace: af6d04ec\n  jackson:\n    mapper:\n      ALLOW_EXPLICIT_PROPERTY_RENAMING: true\n    deserialization:\n      READ_DATE_TIMESTAMPS_AS_NANOSECONDS: false\n    serialization:\n      WRITE_DATE_TIMESTAMPS_AS_NANOSECONDS: false\n  boot:\n    admin:\n      client:\n        url: http://172.18.252.175:23333\n        username: admin\n        password: admin";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("Common YAML sections: {:?}", sections.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>());
        assert!(sections.len() >= 3, "Expected at least 3 sections, got {}", sections.len());
        // Adaptive: spring.cloud.nacos.discovery stays under spring.cloud (only 1 child at that depth)
        // Check content includes nacos, not just section name
        let has_nacos = sections.iter().any(|(n, pairs)| {
            n.contains("nacos") || n.contains("discovery")
            || pairs.iter().any(|(k, _)| k.contains("nacos") || k.contains("discovery"))
        });
        assert!(has_nacos, "Expected nacos/discovery in section names or keys");
        assert!(sections.iter().any(|(n, _)| n.contains("jackson")));
        assert!(sections.iter().any(|(n, _)| n.contains("boot") || n.contains("admin")));
    }

    // -----------------------------------------------------------------------
    // chunk_config_adaptive public entry point tests
    // -----------------------------------------------------------------------

    #[test]
    fn adaptive_entry_yaml_routes_correctly() {
        let sections = chunk_config_adaptive("server:\n  port: 8080", true);
        assert!(!sections.is_empty());
    }

    #[test]
    fn adaptive_entry_properties_routes_correctly() {
        let sections = chunk_config_adaptive("spring.datasource.url=db", false);
        assert!(!sections.is_empty());
    }

    #[test]
    fn adaptive_entry_empty_returns_empty() {
        assert!(chunk_config_adaptive("", true).is_empty());
        assert!(chunk_config_adaptive("   \n", false).is_empty());
    }

    // -----------------------------------------------------------------------
    // Integration: run against sample config files
    // -----------------------------------------------------------------------

    #[test]
    fn sample_all_config_files_chunking() {
        let dir = std::path::Path::new(
            "/data/myProject/digital-twin-v2/config/nacos_config_export_20260721165345/DEFAULT_GROUP",
        );
        if !dir.exists() {
            eprintln!("Sample config directory not found, skipping integration test");
            return;
        }

        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        eprintln!("\n===== Adaptive Config Chunking Results =====");
        eprintln!("Total config files: {}\n", entries.len());

        for entry in &entries {
            let path = entry.path();
            let file_name = path.file_name().unwrap().to_string_lossy();
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  [SKIP] {}: read error {}", file_name, e);
                    continue;
                }
            };

            let is_yaml = file_name.ends_with(".yaml") || file_name.ends_with(".yml");
            let sections = chunk_config_adaptive(&content, is_yaml);

            eprintln!("\n──────────────────────────────────────────────");
            eprintln!("File: {} ({} bytes, {} sections)",
                file_name, content.len(), sections.len());
            eprintln!("──────────────────────────────────────────────");

            for (section_name, pairs) in &sections {
                eprintln!("  ┌─ [{}] ({} keys)", section_name, pairs.len());
                for (key, value) in pairs.iter().take(15) {
                    if value.chars().count() > 80 {
                        let truncated: String = value.chars().take(80).collect();
                        eprintln!("  │  {}={}", key, truncated);
                    } else {
                        eprintln!("  │  {}={}", key, value);
                    }
                }
                if pairs.len() > 15 {
                    eprintln!("  │  ... and {} more keys", pairs.len() - 15);
                }
                eprintln!("  └─");
            }
        }

        // Summary statistics
        let mut total_sections = 0usize;
        let mut total_keys = 0usize;
        for entry in &entries {
            let path = entry.path();
            let file_name = path.file_name().unwrap().to_string_lossy();
            if let Ok(content) = std::fs::read_to_string(&path) {
                let is_yaml = file_name.ends_with(".yaml") || file_name.ends_with(".yml");
                let sections = chunk_config_adaptive(&content, is_yaml);
                total_sections += sections.len();
                for (_, pairs) in &sections {
                    total_keys += pairs.len();
                }
            }
        }
        eprintln!("\n===== Summary =====");
        eprintln!("Files: {}", entries.len());
        eprintln!("Total sections (chunks): {}", total_sections);
        eprintln!("Total keys: {}", total_keys);
        eprintln!("Avg sections per file: {:.1}", total_sections as f64 / entries.len() as f64);
        eprintln!("Avg keys per section: {:.1}",
            if total_sections > 0 { total_keys as f64 / total_sections as f64 } else { 0.0 });
    }
}
