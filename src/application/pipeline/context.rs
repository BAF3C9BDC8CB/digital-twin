//! Shared data container passed through all pipeline stages.
//!
//! [`PipelineContext`] carries the file being processed, its raw text, the
//! project name, and the accumulated outputs from every processor that has
//! run so far.  The [`resolve`](PipelineContext::resolve) method supports
//! simple variable interpolation for template strings.

use crate::application::pipeline::output::ProcessorOutput;
use std::collections::HashMap;
use std::path::PathBuf;

/// Shared context that flows through the entire pipeline for one file.
#[derive(Debug, Clone)]
pub struct PipelineContext {
    /// Absolute or relative path of the file being processed.
    pub file_path: PathBuf,
    /// Full text content of the file.
    pub file_text: String,
    /// The project this file belongs to.
    pub project_name: String,
    /// Processor outputs keyed by processor name (e.g. `"tree_sitter"`,
    /// `"hanlp"`).
    pub outputs: HashMap<String, ProcessorOutput>,
}

impl PipelineContext {
    /// Create a new pipeline context for the given file.
    pub fn new(file_path: impl Into<PathBuf>, file_text: String, project_name: String) -> Self {
        Self {
            file_path: file_path.into(),
            file_text,
            project_name,
            outputs: HashMap::new(),
        }
    }

    /// Insert (or overwrite) the output produced by a processor.
    pub fn add_output(&mut self, processor_name: &str, output: ProcessorOutput) {
        self.outputs.insert(processor_name.to_string(), output);
    }

    /// Retrieve the output produced by a named processor.
    pub fn get_output(&self, processor_name: &str) -> Option<&ProcessorOutput> {
        self.outputs.get(processor_name)
    }

    /// Resolve template variables in a string against the current context.
    ///
    /// # Syntax
    ///
    /// - `${processor_name.field}` — top-level field from a processor's
    ///   output, resolved via `serde_json::Value::get`
    /// - `${processor_name.field.subfield}` — nested subfield access
    ///
    /// Unknown variables are left as-is in the output string.
    ///
    /// # Examples
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

        // Match `${...}` placeholders.
        let re = regex::Regex::new(r"\$\{([^}]+)\}").unwrap();

        for cap in re.captures_iter(template) {
            let full_match = cap.get(0).unwrap().as_str().to_string();
            let inner = cap.get(1).unwrap().as_str();

            // Split on the FIRST dot to separate processor name from the
            // remaining key path.
            if let Some(dot_pos) = inner.find('.') {
                let processor_name = &inner[..dot_pos];
                let key_path = &inner[dot_pos + 1..];

                if let Some(output) = self.outputs.get(processor_name) {
                    let value = self.resolve_json_path(output.as_inner(), key_path);
                    if let Some(v) = value {
                        // Convert the JSON value to its string representation.
                        // Strings are returned without quotes; other types use
                        // the default Display (which matches JSON serialization).
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

    /// Walk a dotted / bracket-notation path into a JSON map.
    ///
    /// Supports:
    /// - `field` — simple key lookup
    /// - `field.subfield` — nested object traversal
    /// - `arr[0]` — array index access (only at the end of a segment)
    /// - `field.subfield[1].name` — mixed nesting
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

/// Split a dotted path like `"entities.methods[0].name"` into segments
/// `["entities", "methods[0]", "name"]`.
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

/// Parse a segment like `"methods[0]"` into a key `"methods"` and an
/// optional index `Some(0)`.
fn parse_segment(seg: &str) -> (&str, Option<usize>) {
    if let Some(bracket_pos) = seg.find('[') {
        let key = &seg[..bracket_pos];
        let idx_str = &seg[bracket_pos + 1..seg.len() - 1]; // strip ']'
        let idx = idx_str.parse::<usize>().ok();
        (key, idx)
    } else {
        (seg, None)
    }
}

// ---------------------------------------------------------------------------
// Tests
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
        let ctx = PipelineContext::new("/tmp/test.rs", "content".into(), "p".into());
        assert_eq!(ctx.file_path.to_string_lossy(), "/tmp/test.rs");
        assert_eq!(ctx.file_text, "content");
        assert_eq!(ctx.project_name, "p");
        assert!(ctx.outputs.is_empty());
    }

    #[test]
    fn add_and_get_output() {
        let ctx = sample_context();
        let out = ctx.get_output("lang_detector").unwrap();
        assert_eq!(
            out.get("language").and_then(|v| v.as_str()),
            Some("Rust")
        );
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
        let result = ctx.resolve("${lang_detector.language} project: ${tree_sitter.entities.classes[0]}");
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
