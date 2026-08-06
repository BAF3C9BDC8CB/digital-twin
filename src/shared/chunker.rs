//! 文档分块——将文本文档切分为带重叠的分块，
//! 支持可配置的边界、大小与重叠度。
//!
//! 分块策略采用分层边界方案：
//! 1. 优先使用段落边界（双换行）。
//! 2. 若段落超过 chunk_size，则回退到句子边界。
//! 3. 若句子仍超过 chunk_size，则回退到固定长度切片。
//!
//! 每个分块包含指向前驱与后继的链接，支持在文档内导航。

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
            if trimmed.starts_with('#') {
                continue;
            }
            if trimmed.contains(':') && !trimmed.contains('=') {
                return DocType::Yaml;
            }
        }
        DocType::PlainText
    }
}

/// 分块时使用的边界类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// 在段落边界处切分（空行 / 双换行）。
    Paragraph,
    /// 在句子边界处切分（`.`、`!`、`?`、`。`）。
    Sentence,
    /// 按固定字符数切分（硬切片）。
    Fixed,
}

/// 分块策略的配置。
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// 每个分块的目标 token 数（通过字符数近似估算）。
    pub chunk_size: usize,
    /// 相邻分块之间的重叠 token 数。
    pub overlap: usize,
    /// 首选的分割边界类型。
    pub boundary: Boundary,
    /// 分块的最小 token 数；低于此大小的分块会合并到
    /// 前一个分块中，而不是独立输出。
    pub min_chunk_size: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            chunk_size: 256,
            overlap: 0,
            boundary: Boundary::Paragraph,
            min_chunk_size: 128,
        }
    }
}

/// 文档的单个分块，带有导航链接。
#[derive(Debug, Clone)]
pub struct DocumentChunk {
    /// 唯一分块标识：`"{doc_id}#chunk{index}"`
    pub chunk_id: String,
    /// 分块文本内容。
    pub text: String,
    /// 该分块在文档中的从零开始的索引。
    pub chunk_index: usize,
    /// 前一个分块的 ID（如果有）。
    pub prev_chunk_id: Option<String>,
    /// 下一个分块的 ID（如果有）。
    pub next_chunk_id: Option<String>,
    /// 在原始文本中的起始字符偏移。
    pub start_char: usize,
    /// 在原始文本中的结束字符偏移（不含）。
    pub end_char: usize,
}

/// 使用配置的边界策略将文档文本切分为带重叠的分块。
///
/// # 参数
///
/// * `text` - 完整文档文本。
/// * `doc_id` - 用于构建 `chunk_id` 字段的文档标识。
/// * `config` - 分块配置。
///
/// # 返回值
///
/// 按顺序返回 `DocumentChunk` 向量。若输入文本为空（仅空白字符），
/// 则返回空向量。
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

/// 根据字符数近似估算 token 数：约 4 字符 = 1 token（英文），
/// 约 2 字符 = 1 token（中日韩文）。采用保守的启发式：3 字符/token。
#[allow(dead_code)]
fn approx_tokens(char_count: usize) -> usize {
    char_count / 3
}

fn approx_chars(tokens: usize) -> usize {
    tokens * 3
}

/// 解析 key=value 或 key: value 行。
/// 冒号分隔形式支持带引号的值。
/// 成功时返回 `(key, value)`；空行/注释行返回 `None`。
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

/// YAML 树遍历分段：按多分支 key 拆分，单链合并。
///
/// 规则：
/// - 构建 YAML 树，每个 key 是一个节点
/// - 如果父节点有 ≥2 个子节点：每个子节点拆成独立 section
/// - 如果父节点只有 ≤1 个子节点：子节点合并到父 section
/// - section 包含从该节点到所有后代的内容
fn chunk_yaml_by_top_level_keys(
    text: &str,
    doc_id: &str,
    _config: &ChunkConfig,
) -> Vec<(String, Vec<DocumentChunk>)> {
    // -- 解析行 --
    struct Item {
        indent: usize,
        is_key: bool,
        key_name: String,
        raw: String,
    }
    let items: Vec<Item> = text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let indent = line.len() - trimmed.len();
            let is_key = if let Some(pos) = trimmed.find(':') {
                let before = trimmed[..pos].trim();
                !before.is_empty() && !before.contains(' ')
            } else {
                false
            };
            let key_name = if is_key {
                trimmed.split(':').next().unwrap_or("").trim().to_string()
            } else {
                String::new()
            };
            Some(Item {
                indent,
                is_key,
                key_name,
                raw: line.to_string(),
            })
        })
        .collect();

    // -- 构建树 --
    struct Node {
        parent: Option<usize>,
        key_name: String,
        indent: usize,
        item_idx: usize,      // 本节点在 items 中的索引
        children: Vec<usize>, // 子节点索引
    }
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<usize> = Vec::new(); // 当前路径的 node 索引

    for (idx, item) in items.iter().enumerate() {
        if !item.is_key {
            continue;
        }
        while let Some(&top) = stack.last() {
            if nodes[top].indent < item.indent {
                break;
            }
            stack.pop();
        }
        let parent = stack.last().copied();
        nodes.push(Node {
            parent,
            key_name: item.key_name.clone(),
            indent: item.indent,
            item_idx: idx,
            children: Vec::new(),
        });
        let node_idx = nodes.len() - 1;
        if let Some(p) = parent {
            nodes[p].children.push(node_idx);
        }
        stack.push(node_idx);
    }

    // -- 收集每个节点下属的所有 item 索引 --
    fn collect_descendants(items: &[Item], nodes: &[Node], node_idx: usize) -> Vec<usize> {
        let node = &nodes[node_idx];
        let mut result = vec![node.item_idx];
        // 本节点到下一个同层或更浅层 key 之间的所有 item
        // 使用 `<=` 而非 `==` 确保不会越出父节点的作用域
        let my_indent = node.indent;
        let my_pos = node.item_idx;
        // 找到下一个同层或更浅层 key 的位置（同层 sibling 或祖先的 sibling，用于限定范围）
        let end = items
            .iter()
            .enumerate()
            .skip(my_pos + 1)
            .find(|(_, it)| it.is_key && it.indent <= my_indent)
            .map(|(i, _)| i)
            .unwrap_or(items.len());
        // 从当前 key 到下一个同层/更浅层 key（不含）之间的所有 item
        for i in (my_pos + 1)..end {
            result.push(i);
        }
        // 子节点
        for &child in &node.children {
            let child_items = collect_descendants(items, nodes, child);
            for &ci in &child_items {
                if !result.contains(&ci) {
                    result.push(ci);
                }
            }
        }
        result.sort();
        result
    }

    // -- 递归提取 section --
    /// `ancestor_ids`：从根节点到当前节点父节点的祖先节点索引。
    /// 用于在 section 内容中包含父级 key 行，以提供完整的 YAML 上下文。
    fn collect_sections(
        items: &[Item],
        nodes: &[Node],
        node_idx: usize,
        parent_path: &str,
        ancestor_ids: &[usize],
        result: &mut Vec<(String, Vec<usize>)>,
    ) {
        let node = &nodes[node_idx];
        let path = if parent_path.is_empty() {
            node.key_name.clone()
        } else {
            format!("{}.{}", parent_path, node.key_name)
        };

        let mut new_ancestors: Vec<usize> = ancestor_ids.to_vec();
        new_ancestors.push(node_idx);

        let has_complex_child = node.children.iter().any(|&c| !nodes[c].children.is_empty());

        if node.children.len() > 1 && has_complex_child {
            // 多分支且至少一个是复杂节点 → 子节点各自独立成 section
            for &child_idx in &node.children {
                collect_sections(items, nodes, child_idx, &path, &new_ancestors, result);
            }
        } else if node.children.is_empty() || !has_complex_child {
            // 叶子节点，或子节点均为简单值 → 收集完整内容作为一个 section
            // 包含祖先 key 行 + 本节点及后代行，提供完整的 YAML 层级上下文
            let desc = collect_descendants(items, nodes, node_idx);
            let mut all_idx: Vec<usize> = Vec::new();
            // 添加祖先 key 的 item 索引（不包含当前 node 自身，new_ancestors 已包含它）
            for &aid in &new_ancestors {
                all_idx.push(nodes[aid].item_idx);
            }
            // 添加后代条目
            for &di in &desc {
                if !all_idx.contains(&di) {
                    all_idx.push(di);
                }
            }
            all_idx.sort();
            result.push((path, all_idx));
        } else {
            // 单链（唯一子节点）→ 合并到当前路径
            collect_sections(
                items,
                nodes,
                node.children[0],
                &path,
                &new_ancestors,
                result,
            );
        }
    }

    let mut result: Vec<(String, Vec<usize>)> = Vec::new();
    for root_idx in 0..nodes.len() {
        if nodes[root_idx].parent.is_none() {
            // 有多个顶级 key → 每个顶级 key 独立 section
            if nodes.iter().filter(|n| n.parent.is_none()).count() > 1 {
                collect_sections(&items, &nodes, root_idx, "", &[], &mut result);
            } else {
                // 只有一个顶级 key
                let node = &nodes[root_idx];
                if node.children.len() > 1 {
                    // 多个子分支 → 每个子分支独立
                    // 父节点（根 key）作为祖先上下文传递给子分支
                    for &child_idx in &node.children {
                        collect_sections(
                            &items,
                            &nodes,
                            child_idx,
                            &node.key_name,
                            &[root_idx],
                            &mut result,
                        );
                    }
                } else {
                    // 无分支 → 整篇一个 section
                    let desc = collect_descendants(&items, &nodes, root_idx);
                    result.push((node.key_name.clone(), desc));
                }
            }
        }
    }

    // -- 转为 DocumentChunk --
    let mut output = Vec::new();
    for (section_name, desc_indices) in result {
        let content_lines: Vec<String> =
            desc_indices.iter().map(|&i| items[i].raw.clone()).collect();
        let text = content_lines.join("\n");
        if text.trim().is_empty() {
            continue;
        }
        let chunk = DocumentChunk {
            chunk_id: format!("{}#section-{}", doc_id, section_name),
            text,
            chunk_index: 0,
            prev_chunk_id: None,
            next_chunk_id: None,
            start_char: 0,
            end_char: 0,
        };
        output.push((section_name, vec![chunk]));
    }
    output
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

/// 使用给定的边界策略切分文本，必要时回退到更粗的边界。
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
    let chunks =
        build_chunks_from_segments(doc_id, &segments, chunk_chars, overlap_chars, min_chars);

    // 若单个分块仍然过大（因为单个 segment 超过了 chunk_size），
    // 则回退到下一个更粗的边界。
    let max_allowed = chunk_chars + overlap_chars;
    if chunks.iter().any(|c| c.text.chars().count() > max_allowed) {
        return match boundary {
            Boundary::Paragraph => chunk_by_boundary(text, doc_id, config, Boundary::Sentence),
            Boundary::Sentence => chunk_fixed(text, doc_id, config),
            Boundary::Fixed => chunk_fixed(text, doc_id, config),
        };
    }

    chunks
}

/// 使用给定的边界将文本切分为多个段（segment）。
fn split_by_boundary(text: &str, boundary: Boundary) -> Vec<String> {
    match boundary {
        Boundary::Paragraph => {
            // 按一个或多个连续空行切分
            let mut parts: Vec<String> = text
                .split("\n\n")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            // 同时尝试按 \r\n\r\n 切分，以支持 Windows 风格的行结束符
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
            // 按句末标点后跟空白进行切分
            let mut sentences = Vec::new();
            let mut start = 0;
            for (i, c) in text.char_indices() {
                match c {
                    '.' | '!' | '?' | '。' | '！' | '？' => {
                        // 判断是否可能是句末（后跟空格/换行或文件结束）
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
            // 剩余部分
            let remainder = text[start..].trim().to_string();
            if !remainder.is_empty() {
                sentences.push(remainder);
            }
            sentences
        }
        Boundary::Fixed => {
            // 从 chunk_by_boundary 不会直接以 Fixed 调用此分支，
            // 仅为完整性保留。
            vec![text.to_string()]
        }
    }
}

/// 从段构建带重叠的分块。
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

    // 将相邻段合并至 chunk_chars，但仅限于段边界处。
    // 这样可保持分块的语义完整（不会在句子/段落中间切断）。
    let mut raw_chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for seg in segments {
        if seg.chars().count() > chunk_chars {
            // 先写出当前缓冲区
            if !current.is_empty() {
                raw_chunks.push(current.clone());
                current.clear();
            }
            // 单个段也过大——原样压入（由调用方回退处理）
            raw_chunks.push(seg.clone());
        } else if current.is_empty() {
            current.push_str(seg);
        } else if current.chars().count() + 1 + seg.chars().count() <= chunk_chars {
            // 还有空间容纳该段
            current.push('\n');
            current.push_str(seg);
        } else {
            // 将超过 chunk_chars → 写出当前内容，开始新分块
            raw_chunks.push(current.clone());
            current = seg.clone();
        }
    }
    if !current.is_empty() {
        raw_chunks.push(current);
    }

    // 注意：此处有意跳过 min_chunk_size 合并——在当前边界层级下，
    // 每个分块已经是完整的语义单元。

    // 构建带重叠的最终分块
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

        // 前移偏移量，计入重叠部分（即分块分隔符）
        if idx + 1 < raw_chunks.len() {
            let overlap_start = raw.chars().count().saturating_sub(overlap_chars);
            char_offset += overlap_start + 1; // 分隔符 +1
        } else {
            char_offset = end_char;
        }
    }

    chunks
}

/// 将低于 `min_chars` 的分块合并到前一个分块。
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
                // 首个分块较小：原样压入
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
                flush_section(
                    &mut chunks,
                    doc_id,
                    &current_section,
                    &current_heading,
                    config,
                );
            }
            current_heading = trimmed_line.trim_start_matches('#').trim().to_string();
            current_section = vec![format!("[{}] ", current_heading)];
            _section_start = char_offset;
        } else {
            current_section.push(line.to_string());
        }
        char_offset += line.len() + 1; // 换行符 +1
    }

    // flush 最后一个 section
    flush_section(
        &mut chunks,
        doc_id,
        &current_section,
        &current_heading,
        config,
    );

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
        start_char: 0, // 如需要将由调用方设置
        end_char: 0,
    };
    chunks.push(chunk);
}

/// 固定长度分块：按精确的字符边界切片，分块之间带重叠。
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

        // 跳过非常小的尾部块；合并到前一个块
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
                next_chunk_id: None, // 循环结束后填充
                start_char: start,
                end_char: end,
            };
            chunks.push(chunk);
            idx += 1;
        } else {
            // 合并到前一个块
            if let Some(prev) = chunks.last_mut() {
                prev.text.push('\n');
                prev.text.push_str(&chunk_text);
                prev.end_char = end;
            }
        }

        start += step;

        // 处理 step 为 0（overlap >= chunk_size）的情况
        if step == 0 {
            start = end;
        }
    }

    // 修复 next_chunk_id 链接
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
                // 未找到标题或未发生切分——回退到段落级
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
        DocType::PlainText | DocType::EmbeddedCode => chunk_text(text, doc_id, config),
    }
}

// ===========================================================================
// 自适应配置分块
// ===========================================================================

/// 自适应配置分块的结果：section 名称及其键值对。
pub type AdaptiveSection = (String, Vec<(String, String)>);

/// ─── 通用：判断一行是否为注释 ─────────────────────────────────────
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!')
}

/// ─── Properties 自适应分块 ─────────────────────────────────────────────

/// 从点分隔的 key 构建前缀树并自适应分块。
///
/// 启发式规则：
/// - 从第 2 层开始分组（例如 `spring.datasource`）
/// - 若第 2 层分组在第 3 层有 ≥3 个不同的子前缀 → 拆分到第 3 层
/// - 若第 3 层子节点在第 4 层有 ≥3 个不同的孙前缀 → 拆分到第 4 层
/// - 否则，保持当前层级
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

    // 检查是否有 key 包含点；若都没有，则全部合并为一个 section
    let has_dotted = pairs.iter().any(|(k, _)| k.contains('.'));
    if !has_dotted {
        // 扁平键值对——全部归为一个 section
        return vec![("config".to_string(), pairs)];
    }

    // 将无点 key（单段）与点分隔 key 分开
    let (dotted_pairs, flat_pairs): (Vec<_>, Vec<_>) =
        pairs.into_iter().partition(|(k, _)| k.contains('.'));

    // 从点分隔 key 构建前缀树（trie）
    let mut trie = PrefixTrie::new();
    for (key, _) in &dotted_pairs {
        let parts: Vec<&str> = key.split('.').collect();
        trie.insert(&parts);
    }

    // 使用自适应层级将每个点分隔键值对分配到 section
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

    // 将无点 key 作为 "config" section 加入
    if !flat_pairs.is_empty() {
        sections.insert("config".to_string(), flat_pairs);
    }

    // ── 后处理：将单键 section 合并到其父前缀 ──
    // 若某个 section 恰好只有 1 个 key，且存在"父"section（去掉最后一个
    // 点分隔段后得到），则向上合并。这可以避免在
    // `spring.datasource.druid.stat-view-servlet` 强制拆分时产生
    // 过于细碎的分块，如 `spring.datasource.druid.max-active`。
    let merged = merge_singleton_sections(sections);

    // ── 后处理：将兄弟 section 合并到其父前缀 ──
    // 若某前缀有 ≥2 个子 section（例如 spring.datasource.druid.core、
    // spring.datasource.druid.log 等），则全部合并为一个以公共父前缀
    // （spring.datasource.druid）命名的 section。
    let merged = merge_sibling_sections(merged);

    // ── 后处理：合并孤儿兄弟（不存在祖父前缀）──
    // 所有 key 都直接嵌套在 druid 之下（没有 spring.datasource 顶级
    // section）的文件不会被 merge_sibling_sections 捕获，因为祖父前缀
    // 检查不通过。在此处理这些孤儿。
    let merged = merge_orphan_sibling_sections(merged);

    // ── 后处理：将父级 key 提升到已存在的子 section ──
    // 若某个 section（例如 spring.datasource）中的 key 全部共享一个与
    // 已有子 section（spring.datasource.druid）匹配的前缀，则将这些
    // key 移入子 section 并删除父级。
    let merged = promote_to_sub_section(merged);

    // 对 section 排序以获得确定性输出
    let mut result: Vec<AdaptiveSection> = merged.into_iter().collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// 将只有 1 个 key 的 section 合并到最近的已存在祖先 section
/// （沿点分隔前缀链向上查找）。
fn merge_singleton_sections(
    mut sections: std::collections::HashMap<String, Vec<(String, String)>>,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    // 收集单键 section 的名称
    let singletons: Vec<String> = sections
        .iter()
        .filter(|(_, v)| v.len() == 1)
        .map(|(k, _)| k.clone())
        .collect();

    for name in singletons {
        // 沿点分段向上查找最近的已存在父级
        let mut candidate = name.clone();
        let target = loop {
            match candidate.rfind('.') {
                Some(pos) => {
                    candidate = candidate[..pos].to_string();
                    if sections.contains_key(&candidate) {
                        break Some(candidate);
                    }
                }
                None => break None, // 没有剩余点 → 无法合并
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

/// 将公共父前缀下的兄弟子 section 合并为单个以父级命名的 section。
/// 例如，`spring.datasource.druid.core`、`spring.datasource.druid.log`
/// → 一个 `spring.datasource.druid` section。
fn merge_sibling_sections(
    mut sections: std::collections::HashMap<String, Vec<(String, String)>>,
) -> std::collections::HashMap<String, Vec<(String, String)>> {
    // 找出拥有 ≥2 个子 section 的父前缀
    let section_names: Vec<String> = sections.keys().cloned().collect();
    let mut parent_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for name in &section_names {
        if let Some(pos) = name.rfind('.') {
            let parent = name[..pos].to_string();
            *parent_counts.entry(parent).or_default() += 1;
        }
    }

    // 对每个拥有 ≥2 个子节点、且其自身不是 section、
    // 而其父级是 section 的父前缀（即子节点是某个有意义父级的
    // 深层子组），将它们合并为一个 section。
    let to_merge: Vec<String> = parent_counts
        .into_iter()
        .filter(|(parent, count)| {
            if *count < 2 || sections.contains_key(parent) {
                return false;
            }
            // 仅当祖父前缀已作为 section 存在时才合并——避免将
            // spring.datasource + spring.redis 等顶级兄弟合并为
            // 一个庞大的 "spring" section。
            parent
                .rfind('.')
                .map_or(false, |gp_pos| sections.contains_key(&parent[..gp_pos]))
        })
        .map(|(parent, _)| parent)
        .collect();

    for parent in to_merge {
        let mut merged_pairs: Vec<(String, String)> = Vec::new();
        // 收集并移除所有子 section
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

/// 合并没有祖父 section 的兄弟子 section。
/// 与 merge_sibling_sections 相同，但省略祖父存在性检查。
/// 处理类似 `spring.datasource.druid.{url,username,...}` 的场景——
/// 此时 `spring.datasource` 从未作为独立 section 出现。
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

    // 合并孤儿：≥2 个子节点、父前缀不存在，祖父前缀可有可无
    // 仅当父前缀至少含 1 个点（深度 ≥2）时才合并。
    // 这可以防止级联合并到 "spring"、"management" 等顶级前缀。
    let to_merge: Vec<String> = parent_counts
        .into_iter()
        .filter(|(parent, count)| {
            *count >= 2 && !sections.contains_key(parent) && parent.contains('.')
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

/// 若父 section 有 ≥2 个 key 属于某个已存在的子 section，
/// 则将这些 key 移入子 section。不匹配任何子 section 的
/// "孤儿" key 保留在父级。
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
            sections
                .get_mut(&parent)
                .unwrap()
                .retain(|(k, _)| !k.starts_with(&prefix));
            sections.get_mut(sub_name).unwrap().extend(to_move);
        }
    }

    sections.retain(|_, v| !v.is_empty());
    sections
}

/// 用于点分隔 key 名称的简单前缀树（trie）。
/// 用于确定每个 key 的最优 section 前缀。
struct PrefixTrie {
    root: TrieNode,
}

struct TrieNode {
    children: std::collections::HashMap<String, TrieNode>,
    /// 经过或终止于该节点的叶子 key 数量。
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
            node = node.children.entry(part.to_string()).or_insert(TrieNode {
                children: std::collections::HashMap::new(),
                leaf_count: 0,
            });
            node.leaf_count += 1;
        }
    }

    /// 为给定 parts 的 key 解析最优 section 名称。
    ///
    /// 策略：
    /// - 1-2 段的 key：section = 第一段（例如 "server.port" → "server"）
    /// - 3 段及以上的 key：从第 1 层（"a.b"）开始，必要时继续加深
    /// - 在每个中间节点：统计 leaf_count > 1 的子节点
    ///   （多 key 前缀）。若 ≥ 2，则进一步拆分。
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

        // 从根节点遍历 trie 以检查是否需要拆分
        let mut node = &self.root;
        for i in 0..parts.len().min(4) {
            if let Some(child) = node.children.get(parts[i]) {
                node = child;
            } else {
                break;
            }

            if i >= 1 && i < parts.len() - 1 {
                // 在中间节点：统计 leaf_count > 1 的子节点
                //（这些是多 key 子组，应拥有独立 section）
                let multi_key_children =
                    node.children.values().filter(|c| c.leaf_count > 1).count();

                // 若 ≥ 2 个子组各含多个 key，则拆分
                if multi_key_children >= 2 {
                    depth = i + 1; // "a.b.c"
                }
            }
        }

        // 将深度限制在 parts.len()-1 且最大为 4
        depth = depth.min(parts.len() - 1).min(3);
        parts[..=depth].join(".")
    }
}

/// ─── YAML 自适应分块 ─────────────────────────────────────────────────────

/// 将 YAML 解析为扁平的键值对，然后使用与 Properties 相同的前缀
/// 自适应分块。这避免了复杂的树遍历。
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

/// 将类 YAML 文本解析为扁平的 (dotted.key, value) 对列表。
///
/// 使用栈跟踪当前 YAML 路径。对每行：
/// - 若缩进减小，相应地弹出栈
/// - 若是 "key:"（无值），将 key 压入栈
/// - 若是 "key: value"，输出（栈路径 + key, value）
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

/// 统计用于缩进的前导空格数。
fn count_indent(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// ─── 公共入口 ─────────────────────────────────────────────────────────────

/// 自适应配置分块：自动为 Properties 与 YAML 配置文件
/// 确定最优的 section 层级。
///
/// 返回排序后的 `(section_name, Vec<(key, value)>)` 向量。
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
// 测试
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
        // 三个短段落合并为一个分块（在 chunk_size 范围内）
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("First paragraph"));
        assert!(chunks[0].text.contains("Third paragraph"));
    }

    #[test]
    fn chunks_have_prev_and_next_links() {
        let config = ChunkConfig {
            chunk_size: 10, // ~30 字符——非常小，用于强制产生多个分块
            overlap: 5,     // ~15 字符重叠
            boundary: Boundary::Fixed,
            min_chunk_size: 10, // ~30 字符
        };
        let text =
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let doc_id = "dt://doc/test/links.md";
        let chunks = chunk_text(text, doc_id, &config);
        assert!(
            chunks.len() >= 2,
            "预期至少 2 个分块，实际得到 {}",
            chunks.len()
        );

        // 第一个分块：无 prev，有 next
        assert!(chunks[0].prev_chunk_id.is_none());
        assert_eq!(
            chunks[0].next_chunk_id.as_deref(),
            Some(make_id(1).as_str())
        );

        // 中间的分块两者都有
        if chunks.len() > 2 {
            for i in 1..chunks.len() - 1 {
                assert_eq!(
                    chunks[i].prev_chunk_id.as_deref(),
                    Some(make_id(i - 1).as_str())
                );
                assert_eq!(
                    chunks[i].next_chunk_id.as_deref(),
                    Some(make_id(i + 1).as_str())
                );
            }
        }

        // 最后一个分块：有 prev，无 next
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
            chunk_size: 5, // ~15 字符
            overlap: 2,    // ~6 字符
            boundary: Boundary::Fixed,
            min_chunk_size: 2,
        };
        let text = "abcdefghijklmnopqrstuvwxyz";
        let doc_id = "dt://doc/test/offsets.md";
        let chunks = chunk_text(text, doc_id, &config);

        // 第一个分块偏移为 0
        assert_eq!(chunks[0].start_char, 0);
        assert!(chunks[0].text.len() > 0);

        // 最后一个分块覆盖文本末尾
        let last = chunks.last().unwrap();
        assert_eq!(last.end_char, text.chars().count());
    }

    #[test]
    fn min_chunk_size_merges_small_tail() {
        let config = ChunkConfig {
            chunk_size: 15, // ~45 字符
            overlap: 3,     // ~9 字符
            boundary: Boundary::Fixed,
            min_chunk_size: 12, // ~36 字符
        };
        // 构造文本以产生多个固定分块，末尾带一个较小的尾部
        // 每个分块约 45 字符、重叠约 9 字符、步长约 36 字符
        // 因此最后的尾部可能较小
        let text = "A".repeat(100);
        let doc_id = "dt://doc/test/min_chunk.md";
        let chunks = chunk_text(&text, doc_id, &config);

        // 使用 Fixed 边界和 chunk_size=15（约 45 字符），应得到约 3 个分块
        // 关键是没有任何分块为空，且都带有正确的导航链接
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
            assert!(!chunk.chunk_id.is_empty());
        }
    }

    #[test]
    fn sentence_boundary_splits_correctly() {
        let config = ChunkConfig {
            chunk_size: 20, // ~60 字符
            overlap: 5,
            boundary: Boundary::Sentence,
            min_chunk_size: 10,
        };
        let text = "First sentence. Second sentence! Third sentence? Fourth one here. Fifth.";
        let doc_id = "dt://doc/test/sentences.md";
        let chunks = chunk_text(text, doc_id, &config);
        assert!(chunks.len() >= 1);
        // 所有句子都应出现在各分块中
        let combined: String = chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("First sentence"));
        assert!(combined.contains("Second sentence"));
        assert!(combined.contains("Third sentence"));
    }

    #[test]
    fn chunk_config_default_values() {
        let config = ChunkConfig::default();
        assert_eq!(config.chunk_size, 256);
        assert_eq!(config.overlap, 0);
        assert_eq!(config.boundary, Boundary::Paragraph);
        assert_eq!(config.min_chunk_size, 128);
    }

    #[test]
    fn large_text_produces_multiple_chunks() {
        let config = ChunkConfig {
            chunk_size: 10, // ~30 字符
            overlap: 3,     // ~9 字符
            boundary: Boundary::Fixed,
            min_chunk_size: 5, // ~15 字符
        };
        let text = "X".repeat(500);
        let doc_id = "dt://doc/test/large.md";
        let chunks = chunk_text(&text, doc_id, &config);
        assert!(
            chunks.len() > 1,
            "大文本应产生多个分块，实际得到 {}",
            chunks.len()
        );

        // 验证分块 ID 唯一
        let ids: std::collections::HashSet<String> =
            chunks.iter().map(|c| c.chunk_id.clone()).collect();
        assert_eq!(ids.len(), chunks.len());
    }

    // -----------------------------------------------------------------------
    // DocType 检测测试
    // -----------------------------------------------------------------------

    #[test]
    fn doc_type_detect_markdown_by_extension() {
        assert_eq!(DocType::detect("readme.md", &[]), DocType::Markdown);
        assert_eq!(
            DocType::detect("CHANGELOG.markdown", &[]),
            DocType::Markdown
        );
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
        assert_eq!(
            DocType::detect("notes", &["hello world"]),
            DocType::PlainText
        );
    }

    #[test]
    fn doc_type_detect_plain_text_when_contains_equals() {
        // 含 = 的行是 properties，而非 yaml
        let lines = &["key=value"];
        assert_eq!(DocType::detect("some.file", lines), DocType::PlainText);
    }

    // -----------------------------------------------------------------------
    // Markdown 标题分块测试
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
        assert!(
            chunks.len() >= 2,
            "预期至少 2 个分块，实际得到 {}",
            chunks.len()
        );
        assert!(chunks[0].text.contains("[Main]"));
        assert!(chunks[1].text.contains("[Section 1]"));
        // 检查 prev/next 链接
        assert!(chunks[0].prev_chunk_id.is_none());
        if chunks.len() > 1 {
            assert_eq!(
                chunks[0].next_chunk_id.as_deref(),
                Some(chunks[1].chunk_id.as_str())
            );
            assert_eq!(
                chunks[1].prev_chunk_id.as_deref(),
                Some(chunks[0].chunk_id.as_str())
            );
        }
    }

    #[test]
    fn markdown_no_headings_falls_back_to_chunk_text() {
        let config = ChunkConfig::default();
        let text = "Just a plain paragraph.\n\nAnother paragraph without headings.";
        let doc_id = "dt://doc/test/noheading.md";
        let chunks = chunk_markdown_by_headings(text, doc_id, &config);
        // 两个短段落合并为一个分块（在 chunk_size 范围内）
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
    // parse_kv_line 测试
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
        // 值中包含冒号时，冒号路径应返回 None
        // 但若存在 = 路径则仍可工作
        assert_eq!(parse_kv_line("key=foo:bar"), Some(("key", "foo:bar")));
    }

    #[test]
    fn parse_kv_line_no_delimiter() {
        assert!(parse_kv_line("just a string").is_none());
    }

    #[test]
    fn chunk_long_document_paragraph_first() {
        // 模拟包含多个段落的长文档
        let mut paragraphs: Vec<String> = Vec::new();
        for i in 0..10 {
            paragraphs.push(format!("这是第{}段。这一段描述了架构设计中的重要概念和相关实现细节。每个段落应该独立成为文档块。\n技术细节包括多个方面。", i));
        }
        let text = paragraphs.join("\n\n");

        // 使用较小的 chunk_size 以强制产生多个分块
        let config = ChunkConfig {
            chunk_size: 100, // 较小以触发合并限制
            overlap: 0,
            boundary: Boundary::Paragraph,
            min_chunk_size: 128,
        };
        let doc_id = "dt://doc/test/long.md";
        let chunks = chunk_text(&text, doc_id, &config);

        // 由段落合并出多个分块（每段约 50 字符，10 段 ≈ 500 字符）
        // 在 chunk_size=100（约 300 字符）下，约 2-3 个分块
        assert!(
            chunks.len() >= 2 && chunks.len() <= 5,
            "段落合并后预期 2-5 个分块，实际得到 {}",
            chunks.len()
        );

        // 验证分块不会在段落中间切断
        for chunk in &chunks {
            let text = &chunk.text;
            assert!(
                !text.starts_with('第') || text.contains("段。"),
                "分块可能在段落中间被切断：{}",
                text.chars().take(30).collect::<String>()
            );
        }
    }

    #[test]
    fn chunk_long_paragraph_falls_back_to_sentence() {
        // 单个超长段落（无 \n\n）应回退到句子切分
        let mut long_text = String::new();
        for i in 0..20 {
            long_text.push_str(&format!(
                "这是第{}个句子。它描述了系统架构中的某个重要方面。",
                i
            ));
        }
        let config = ChunkConfig {
            chunk_size: 100, // 较小以强制拆分
            overlap: 16,
            boundary: Boundary::Paragraph,
            min_chunk_size: 32,
        };
        let doc_id = "dt://doc/test/long_para.md";
        let chunks = chunk_text(&long_text, doc_id, &config);

        // 应产生多个分块（按句子，而非固定大小）
        assert!(
            chunks.len() >= 2,
            "句子切分后预期至少 2 个分块，实际得到 {}",
            chunks.len()
        );
        // 验证分块不会在句子中间切断
        for chunk in &chunks {
            let text = &chunk.text;
            if let Some(last_char) = text.chars().last() {
                // 每个分块应以句子边界结尾
                assert!(
                    matches!(last_char, '。' | '！' | '？' | '）' | '"' | '\n'),
                    "分块应以句子边界结尾，实际以 '{}' 结尾：{}",
                    last_char,
                    text.chars().take(30).collect::<String>()
                );
            }
        }
    }

    #[test]
    fn parse_kv_line_comment_char_not_at_start() {
        // 中间的 '#' 不是注释
        assert_eq!(
            parse_kv_line("key=value#comment"),
            Some(("key", "value#comment"))
        );
    }

    // -----------------------------------------------------------------------
    // extract_properties_sections 测试
    // -----------------------------------------------------------------------

    #[test]
    fn extract_properties_sections_groups_by_prefix() {
        // "spring.datasource.url" → parts ["spring","datasource","url"] → section "spring.datasource"
        // "server.port"           → parts ["server","port"] → section "server.port"
        let content = "spring.datasource.url=jdbc:mysql://localhost/db\nspring.datasource.username=admin\nserver.port=8080";
        let sections = extract_properties_sections(content);
        assert_eq!(
            sections.len(),
            2,
            "expected 2 sections, got {}: {:?}",
            sections.len(),
            sections
        );
        let ds = sections
            .iter()
            .find(|(name, _)| name == "spring.datasource")
            .unwrap();
        assert_eq!(ds.1.len(), 2);
        let sv = sections
            .iter()
            .find(|(name, _)| name == "server.port")
            .unwrap();
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
        // 每个 key 只有一段（无点），因此各自独立成 section
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
    // chunk_config_by_sections 测试（YAML）
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_yaml_single_top_level_key() {
        let config = ChunkConfig::default();
        let text = "server:\n  port: 8080";
        let result = chunk_config_by_sections(text, "doc1", &config, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "server");
        assert_eq!(result[0].1.len(), 1);
        assert!(result[0].1[0].text.contains("server:"));
        assert!(result[0].1[0].text.contains("port: 8080"));
    }

    #[test]
    fn chunk_yaml_multiple_top_level_keys() {
        let config = ChunkConfig::default();
        let text =
            "server:\n  port: 8080\n  host: localhost\n\nredis:\n  host: 127.0.0.1\n  port: 6379";
        let result = chunk_config_by_sections(text, "doc1", &config, true);
        assert_eq!(
            result.len(),
            2,
            "预期 2 个 section，实际得到 {}",
            result.len()
        );
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
    // chunk_config_by_sections 测试（Properties）
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_properties_single_section() {
        let config = ChunkConfig::default();
        let text =
            "spring.datasource.url=jdbc:mysql://localhost/db\nspring.datasource.username=admin";
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
        assert_eq!(
            result.len(),
            2,
            "预期 2 个 section，实际得到 {}: {:?}",
            result.len(),
            result
        );
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
    // YAML section 父级上下文包含测试
    // -----------------------------------------------------------------------

    #[test]
    fn chunk_yaml_single_chain_includes_parent_context() {
        let config = ChunkConfig::default();
        let yaml = "\
spring:
  boot:
    admin:
      client:
        instance:
          service-url: http://doctor-center";
        let result = chunk_config_by_sections(yaml, "doc1", &config, true);
        // 单链应合并为一个 section
        assert_eq!(result.len(), 1, "单链应产生 1 个 section");
        let text = &result[0].1[0].text;
        eprintln!("=== 单链 section ===\n{}", text);
        assert!(text.contains("spring:"), "必须包含 spring: 祖先");
        assert!(text.contains("boot:"), "必须包含 boot: 祖先");
        assert!(text.contains("admin:"), "必须包含 admin: 祖先");
        assert!(text.contains("client:"), "必须包含 client: 祖先");
        assert!(text.contains("instance:"), "必须包含 instance: 祖先");
        assert!(
            text.contains("service-url: http://doctor-center"),
            "必须包含叶子值"
        );
    }

    #[test]
    fn chunk_yaml_multi_branch_includes_full_hierarchy() {
        let config = ChunkConfig::default();
        let yaml = "\
server:
  port: 8080

spring:
  datasource:
    url: jdbc:mysql://localhost:3306/order_db
    username: root
    password: secret

pay:
  service:
    url: http://pay-service:8081";
        let result = chunk_config_by_sections(yaml, "doc2", &config, true);
        eprintln!("\n=== 多个顶级 key 的 section ===");
        for (name, chunks) in &result {
            eprintln!(
                "[{}] -> {} 字符\n{}\n---",
                name,
                chunks[0].text.len(),
                chunks[0].text
            );
        }
        // 应有 3 个顶级 section
        assert_eq!(result.len(), 3, "预期 3 个 section（server、spring、pay）");

        // spring section 应包含完整层级
        let spring = result
            .iter()
            .find(|(n, _)| n.as_str() == "spring.datasource")
            .expect("spring.datasource section 应存在");
        assert!(
            spring.1[0].text.contains("spring:"),
            "spring section 必须包含 spring:"
        );
        assert!(
            spring.1[0].text.contains("datasource:"),
            "spring section 必须包含 datasource:"
        );
        assert!(
            spring.1[0].text.contains("url: jdbc:mysql://localhost"),
            "spring section 必须包含 url"
        );
        // 不应包含 pay 内容（作用域修复）
        assert!(
            !spring.1[0].text.contains("pay:"),
            "spring section 不应包含 pay 内容"
        );
    }

    #[test]
    fn chunk_yaml_doctor_center_style_proper_context() {
        let config = ChunkConfig::default();
        let yaml = "\
spring:
  boot:
    admin:
      client:
        instance:
          service-url: http://doctor-center
  datasource:
    dynamic:
      enable: true
    druid:
      core:
        url: jdbc:mysql://10.12.7.22:3308/db1
        username: user1
        password: pass1
        driver-class-name: com.mysql.cj.jdbc.Driver
      log:
        url: jdbc:mysql://10.12.7.22:3308/db2
        username: user2
        password: pass2
        driver-class-name: com.mysql.cj.jdbc.Driver";
        let result = chunk_config_by_sections(yaml, "doc3", &config, true);
        eprintln!("\n=== Doctor-Center 风格 section ===");
        for (name, chunks) in &result {
            eprintln!(
                "[{}] -> {} 字符\n{}\n---",
                name,
                chunks[0].text.len(),
                chunks[0].text
            );
        }

        // 应包含 section：spring.boot.admin.client.instance、spring.datasource.dynamic、
        // spring.datasource.druid.core、spring.datasource.druid.log
        assert!(
            result.len() >= 3,
            "应至少有 3 个 section，实际得到 {}",
            result.len()
        );

        // 检查 core section 是否包含完整父链
        let core = result.iter().find(|(n, _)| n.contains("core"));
        assert!(core.is_some(), "core section 应存在");
        let core = core.unwrap();
        eprintln!("core section 名称：{}", core.0);
        assert!(
            core.1[0].text.contains("spring:"),
            "core section 必须包含 spring: 祖先"
        );
        assert!(
            core.1[0].text.contains("datasource:"),
            "core section 必须包含 datasource: 祖先"
        );
        assert!(
            core.1[0].text.contains("druid:"),
            "core section 必须包含 druid: 祖先"
        );
        assert!(
            core.1[0].text.contains("core:"),
            "core section 必须包含 core: 自身"
        );
        assert!(
            core.1[0].text.contains("jdbc:mysql://10.12.7.22:3308/db1"),
            "core section 必须包含其值"
        );

        // 检查 log section
        let log = result
            .iter()
            .find(|(n, _)| n.contains("log") && !n.contains("dynamic") && !n.contains("boot"));
        assert!(log.is_some(), "log section 应存在");
        let log = log.unwrap();
        assert!(
            log.1[0].text.contains("spring:"),
            "log section 必须包含 spring: 祖先"
        );
        assert!(
            log.1[0].text.contains("jdbc:mysql://10.12.7.22:3308/db2"),
            "log section 必须包含其值"
        );
    }

    // -----------------------------------------------------------------------
    // chunk_by_type 分发测试
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
        assert_eq!(chunks.len(), 1); // 短段落合并为一个分块
    }

    #[test]
    fn chunk_by_type_empty_returns_empty() {
        let config = ChunkConfig::default();
        let chunks = chunk_by_type("", "empty.md", DocType::Markdown, &config);
        assert!(chunks.is_empty());
    }

    // -----------------------------------------------------------------------
    // Properties 自适应分块测试
    // -----------------------------------------------------------------------

    #[test]
    fn adaptive_properties_datasource_stays_2level() {
        let content = "\
spring.datasource.url=jdbc:mysql://localhost/db\n\
spring.datasource.username=admin\n\
spring.datasource.password=secret";
        let sections = chunk_properties_adaptive(content);
        // 3 个 key 应都在一个 "spring.datasource" section 中
        assert_eq!(
            sections.len(),
            1,
            "预期 1 个 section，实际得到 {:?}",
            sections
        );
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
        // 经孤儿合并后，所有 nacos 子 key 合并到 spring.cloud.nacos
        eprintln!(
            "Section 列表：{:?}",
            sections.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()
        );
        assert!(
            sections.iter().any(|(n, _)| n == "spring.cloud.nacos"),
            "预期 spring.cloud.nacos section，实际得到：{:?}",
            sections
        );
        assert_eq!(
            sections
                .iter()
                .find(|(n, _)| n == "spring.cloud.nacos")
                .unwrap()
                .1
                .len(),
            5
        );
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
        // 2 段 key：按第一段分组
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
        eprintln!(
            "混合深度 section：{:?}",
            sections.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()
        );
        // 自适应 + 孤儿合并：spring.cloud.nacos.* 和 spring.cloud.sentinel.*
        // 可能合并到 spring.cloud section。
        assert!(
            sections.iter().any(|(n, _)| n == "spring.datasource"),
            "缺少 spring.datasource"
        );
        assert!(
            sections.iter().any(|(n, _)| n == "spring.redis"),
            "缺少 spring.redis"
        );
        // spring.cloud.* section 应存在（cloud 或其子 section）
        assert!(
            sections.iter().any(|(n, _)| n.starts_with("spring.cloud")),
            "缺少 spring.cloud"
        );
    }

    // -----------------------------------------------------------------------
    // YAML 自适应分块测试
    // -----------------------------------------------------------------------

    #[test]
    fn adaptive_yaml_simple_top_level() {
        let content = "\
server:\n  port: 8080\n  host: localhost";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("YAML section：{:?}", sections);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].0.contains("server"));
        assert!(sections[0].1.iter().any(|(k, _)| k == "server.port"));
    }

    #[test]
    fn adaptive_yaml_nested_sections() {
        let content = "\
spring:\n  datasource:\n    url: jdbc:mysql://localhost/db\n    username: admin\n  redis:\n    host: 127.0.0.1\n    port: 6379";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("嵌套 YAML section：{:?}", sections);
        assert!(sections.len() >= 2);
        assert!(sections.iter().any(|(n, _)| n.contains("datasource")));
        assert!(sections.iter().any(|(n, _)| n.contains("redis")));
    }

    #[test]
    fn adaptive_yaml_jackson_block() {
        let content = "\
spring:\n  jackson:\n    mapper:\n      ALLOW_EXPLICIT_PROPERTY_RENAMING: true\n    deserialization:\n      READ_DATE_TIMESTAMPS_AS_NANOSECONDS: false\n    serialization:\n      WRITE_DATE_TIMESTAMPS_AS_NANOSECONDS: false";
        let sections = chunk_yaml_adaptive(content);
        eprintln!("Jackson YAML section：{:?}", sections);
        let jackson_section = sections.iter().find(|(n, _)| n.contains("jackson"));
        assert!(
            jackson_section.is_some(),
            "预期 jackson section，实际得到：{:?}",
            sections
        );
    }

    #[test]
    fn adaptive_yaml_common_structure() {
        let content = "\
spring:\n  cloud:\n    nacos:\n      discovery:\n        server-addr: http://nacos.newoffen.net\n        namespace: af6d04ec\n  jackson:\n    mapper:\n      ALLOW_EXPLICIT_PROPERTY_RENAMING: true\n    deserialization:\n      READ_DATE_TIMESTAMPS_AS_NANOSECONDS: false\n    serialization:\n      WRITE_DATE_TIMESTAMPS_AS_NANOSECONDS: false\n  boot:\n    admin:\n      client:\n        url: http://172.18.252.175:23333\n        username: admin\n        password: admin";
        let sections = chunk_yaml_adaptive(content);
        eprintln!(
            "常见 YAML section：{:?}",
            sections.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>()
        );
        assert!(
            sections.len() >= 3,
            "预期至少 3 个 section，实际得到 {}",
            sections.len()
        );
        // 自适应：spring.cloud.nacos.discovery 保留在 spring.cloud 下（该深度只有 1 个子节点）
        // 检查内容包含 nacos，而不只是 section 名称
        let has_nacos = sections.iter().any(|(n, pairs)| {
            n.contains("nacos")
                || n.contains("discovery")
                || pairs
                    .iter()
                    .any(|(k, _)| k.contains("nacos") || k.contains("discovery"))
        });
        assert!(
            has_nacos,
            "预期在 section 名称或 key 中包含 nacos/discovery"
        );
        assert!(sections.iter().any(|(n, _)| n.contains("jackson")));
        assert!(sections
            .iter()
            .any(|(n, _)| n.contains("boot") || n.contains("admin")));
    }

    // -----------------------------------------------------------------------
    // chunk_config_adaptive 公共入口测试
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
    // 集成：针对示例配置文件运行
    // -----------------------------------------------------------------------

    #[test]
    fn sample_all_config_files_chunking() {
        let dir = std::path::Path::new(
            "/data/myProject/digital-twin-v2/config/nacos_config_export_20260721165345/DEFAULT_GROUP",
        );
        if !dir.exists() {
            eprintln!("未找到示例配置目录，跳过集成测试");
            return;
        }

        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        eprintln!("\n===== 自适应配置分块结果 =====");
        eprintln!("配置文件总数：{}\n", entries.len());

        for entry in &entries {
            let path = entry.path();
            let file_name = path.file_name().unwrap().to_string_lossy();
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  [SKIP] {}: 读取错误 {}", file_name, e);
                    continue;
                }
            };

            let is_yaml = file_name.ends_with(".yaml") || file_name.ends_with(".yml");
            let sections = chunk_config_adaptive(&content, is_yaml);

            eprintln!("\n──────────────────────────────────────────────");
            eprintln!(
                "文件：{}（{} 字节，{} 个 section）",
                file_name,
                content.len(),
                sections.len()
            );
            eprintln!("──────────────────────────────────────────────");

            for (section_name, pairs) in &sections {
                eprintln!("  ┌─ [{}]（{} 个 key）", section_name, pairs.len());
                for (key, value) in pairs.iter().take(15) {
                    if value.chars().count() > 80 {
                        let truncated: String = value.chars().take(80).collect();
                        eprintln!("  │  {}={}", key, truncated);
                    } else {
                        eprintln!("  │  {}={}", key, value);
                    }
                }
                if pairs.len() > 15 {
                    eprintln!("  │  ... 以及另外 {} 个 key", pairs.len() - 15);
                }
                eprintln!("  └─");
            }
        }

        // 汇总统计
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
        eprintln!("\n===== 汇总 =====");
        eprintln!("文件数：{}", entries.len());
        eprintln!("section（分块）总数：{}", total_sections);
        eprintln!("key 总数：{}", total_keys);
        eprintln!(
            "每个文件的平均 section 数：{:.1}",
            total_sections as f64 / entries.len() as f64
        );
        eprintln!(
            "每个 section 的平均 key 数：{:.1}",
            if total_sections > 0 {
                total_keys as f64 / total_sections as f64
            } else {
                0.0
            }
        );
    }
}
