//! 贯穿所有流水线阶段的共享数据容器。
//!
//! [`PipelineContext`] 携带正在处理的文件、其原始文本、项目名，
//! 以及迄今为止每个已运行处理器累积的输出。`resolve` 方法支持对模板
//! 字符串做简单的变量插值。

use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::virtual_file::FileSourceKind;
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;

/// 缓存的模板变量正则表达式 `${...}`，避免重复编译。
static TEMPLATE_VAR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$\{([^}]+)\}").expect("硬编码的正则表达式必须有效"));

/// 针对单个文件流过整个流水线的共享上下文。
#[derive(Debug, Clone)]
pub struct PipelineContext {
    /// 正在处理的文件的绝对或相对路径。
    pub file_path: PathBuf,
    /// 文件的完整文本内容。
    pub file_text: String,
    /// 该文件所属的项目。
    pub project_name: String,
    /// 文件来源类型（Fs / Nacos / Jenkins / 自定义）。
    pub source_kind: FileSourceKind,
    /// 文件修改时间（Fs 有真实 mtime；远程源为 None）。
    pub mtime: Option<f64>,
    /// 内容哈希（远程源必填，作为增量对比唯一依据）。
    pub content_hash: Option<String>,
    /// 按处理器名索引的输出（例如 `"tree_sitter"`、`"chunk"`）。
    pub outputs: HashMap<String, ProcessorOutput>,
}

impl PipelineContext {
    /// 为给定文件创建新的流水线上下文。
    pub fn new(
        file_path: impl Into<PathBuf>,
        file_text: String,
        project_name: String,
        source_kind: FileSourceKind,
        mtime: Option<f64>,
        content_hash: Option<String>,
    ) -> Self {
        Self {
            file_path: file_path.into(),
            file_text,
            project_name,
            source_kind,
            mtime,
            content_hash,
            outputs: HashMap::new(),
        }
    }

    /// 插入（或覆盖）某处理器产生的输出。
    pub fn add_output(&mut self, processor_name: &str, output: ProcessorOutput) {
        self.outputs.insert(processor_name.to_string(), output);
    }

    /// 获取指定处理器产生的输出。
    pub fn get_output(&self, processor_name: &str) -> Option<&ProcessorOutput> {
        self.outputs.get(processor_name)
    }

    /// 针对当前上下文将字符串中的模板变量解析出来。
    ///
    /// # 语法
    ///
    /// - `${processor_name.field}` —— 处理器输出的顶层字段，
    ///   通过 `serde_json::Value::get` 解析
    /// - `${processor_name.field.subfield}` —— 嵌套子字段访问
    ///
    /// 未知变量在输出字符串中原样保留。
    ///
    /// # 示例
    ///
    /// ```ignore
    /// ctx.resolve("Language: ${lang_detector.language}");
    /// // -> "Language: Rust"
    ///
    /// ctx.resolve("First method: ${tree_sitter.entities.methods[0]}");
    /// // -> "First method: parse_file"
    /// ```
    pub fn resolve(&self, template: &str) -> String {
        let mut result = template.to_string();

        // 使用缓存的正则表达式匹配 `${...}` 占位符。
        for cap in TEMPLATE_VAR_RE.captures_iter(template) {
            let full_match = cap.get(0).unwrap().as_str().to_string();
            let inner = cap.get(1).unwrap().as_str();

            // 在第一个点处分割，将处理器名与其余键路径分开。
            if let Some(dot_pos) = inner.find('.') {
                let processor_name = &inner[..dot_pos];
                let key_path = &inner[dot_pos + 1..];

                if let Some(output) = self.outputs.get(processor_name) {
                    let value = self.resolve_json_path(output.as_inner(), key_path);
                    if let Some(v) = value {
                        // 将 JSON 值转换为其字符串表示。
                        // 字符串不带引号返回；其他类型使用默认 Display
                        //（与 JSON 序列化一致）。
                        let replacement = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        result = result.replace(&full_match, &replacement);
                    }
                }
            }
        }

        result
    }

    /// 沿点分 / 括号记法的路径在 JSON map 中遍历。
    ///
    /// 支持：
    /// - `field` —— 简单键查找
    /// - `field.subfield` —— 嵌套对象遍历
    /// - `arr[0]` —— 数组索引访问（仅限段末尾）
    /// - `field.subfield[1].name` —— 混合嵌套
    fn resolve_json_path<'a>(
        &'a self,
        root: &'a HashMap<String, serde_json::Value>,
        path: &str,
    ) -> Option<&'a serde_json::Value> {
        let segments = split_path_segments(path);
        let mut current: Option<&'a serde_json::Value> = None;

        for (i, seg) in segments.iter().enumerate() {
            let (key, idx) = parse_segment(seg);

            let next = if i == 0 {
                root.get(key)
            } else {
                current?.as_object()?.get(key)
            };

            current = match idx {
                Some(index) => next?.as_array()?.get(index),
                None => next,
            };
        }

        current
    }
}

/// 将点分路径（如 `"entities.methods[0].name"`）分割为段
/// `["entities", "methods[0]", "name"]`。
fn split_path_segments(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;

    for ch in path.chars() {
        match ch {
            '.' if depth == 0 => {
                if !current.is_empty() {
                    segments.push(current.clone());
                    current.clear();
                }
            }
            '[' => {
                depth += 1;
                current.push(ch);
            }
            ']' => {
                depth -= 1;
                current.push(ch);
            }
            _ => {
                current.push(ch);
            }
        }
    }

    if !current.is_empty() {
        segments.push(current);
    }

    segments
}

/// 将类似 `"methods[0]"` 的段解析为键 `"methods"` 与
/// 可选索引 `Some(0)`。
fn parse_segment(seg: &str) -> (&str, Option<usize>) {
    if let Some(bracket_pos) = seg.find('[') {
        let key = &seg[..bracket_pos];
        let idx_str = &seg[bracket_pos + 1..seg.len() - 1]; // 去掉 ']'
        let idx = idx_str.parse::<usize>().ok();
        (key, idx)
    } else {
        (seg, None)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_context() -> PipelineContext {
        let mut ctx = PipelineContext::new(
            PathBuf::from("src/main.rs"),
            "fn main() {}".to_string(),
            "my_project".to_string(),
            FileSourceKind::Fs,
            Some(1723001234.0),
            Some("abc123".into()),
        );

        let mut lang_out = ProcessorOutput::new();
        lang_out.set("language", "Rust");
        lang_out.set("version", "2021");
        ctx.add_output("lang_detector", lang_out);

        let mut ts_out = ProcessorOutput::new();
        ts_out.set(
            "entities",
            json!({
                "methods": ["parse_file", "compile"],
                "classes": ["Compiler"],
            }),
        );
        ctx.add_output("tree_sitter", ts_out);

        ctx
    }

    #[test]
    fn context_new() {
        let ctx = PipelineContext::new(
            "/tmp/test.rs",
            "content".into(),
            "p".into(),
            FileSourceKind::Fs,
            None,
            None,
        );
        assert_eq!(ctx.file_path.to_string_lossy(), "/tmp/test.rs");
        assert_eq!(ctx.file_text, "content");
        assert_eq!(ctx.project_name, "p");
        assert!(ctx.outputs.is_empty());
    }

    #[test]
    fn add_and_get_output() {
        let ctx = sample_context();
        let out = ctx.get_output("lang_detector").unwrap();
        assert_eq!(out.get("language").and_then(|v| v.as_str()), Some("Rust"));
    }

    #[test]
    fn get_output_missing() {
        let ctx = sample_context();
        assert!(ctx.get_output("nonexistent").is_none());
    }

    #[test]
    fn resolve_simple_field() {
        let ctx = sample_context();
        let result = ctx.resolve("Language: ${lang_detector.language}");
        assert_eq!(result, "Language: Rust");
    }

    #[test]
    fn resolve_nested_array_field() {
        let ctx = sample_context();
        let result = ctx.resolve("First method: ${tree_sitter.entities.methods[0]}");
        assert_eq!(result, "First method: parse_file");
    }

    #[test]
    fn resolve_unknown_variable_left_as_is() {
        let ctx = sample_context();
        let result = ctx.resolve("${unknown.key}");
        assert_eq!(result, "${unknown.key}");
    }

    #[test]
    fn resolve_missing_processor_left_as_is() {
        let ctx = sample_context();
        let result = ctx.resolve("${missing.field}");
        assert_eq!(result, "${missing.field}");
    }

    #[test]
    fn resolve_no_template() {
        let ctx = sample_context();
        let result = ctx.resolve("plain string without variables");
        assert_eq!(result, "plain string without variables");
    }

    #[test]
    fn resolve_multiple_variables() {
        let ctx = sample_context();
        let result =
            ctx.resolve("${lang_detector.language} project: ${tree_sitter.entities.classes[0]}");
        assert_eq!(result, "Rust project: Compiler");
    }

    #[test]
    fn split_path_segments_simple() {
        let segs = split_path_segments("language");
        assert_eq!(segs, vec!["language"]);
    }

    #[test]
    fn split_path_segments_nested() {
        let segs = split_path_segments("entities.methods");
        assert_eq!(segs, vec!["entities", "methods"]);
    }

    #[test]
    fn split_path_segments_with_brackets() {
        let segs = split_path_segments("entities.methods[0].name");
        assert_eq!(segs, vec!["entities", "methods[0]", "name"]);
    }

    #[test]
    fn parse_segment_no_index() {
        let (key, idx) = parse_segment("language");
        assert_eq!(key, "language");
        assert!(idx.is_none());
    }

    #[test]
    fn parse_segment_with_index() {
        let (key, idx) = parse_segment("methods[2]");
        assert_eq!(key, "methods");
        assert_eq!(idx, Some(2));
    }
}
