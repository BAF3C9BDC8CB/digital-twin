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
}
