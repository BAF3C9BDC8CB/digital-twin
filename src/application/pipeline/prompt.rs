//! Prompt template system.
//!
//! YAML‑based prompt templates stored under `config/prompts/` are loaded at
//! startup into a [`PromptRegistry`].  Each template may contain
//! `${variable}` placeholders that are substituted at render time from a
//! JSON context value.
//!
//! # Example
//!
//! ```ignore
//! let registry = PromptRegistry::load("config/prompts")?;
//! let ctx = serde_json::json!({
//!     "file_path": "src/main.rs",
//!     "file_text": "fn main() {}",
//! });
//! let (system, user) = registry.render("code_with_ast", &ctx)?;
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------

/// A single named prompt template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    /// Machine‑friendly name (e.g. `"code_with_ast"`).
    pub name: String,

    /// Human‑readable description of what this prompt does.
    pub description: String,

    /// System‑level instruction (passed as the `system` role message).
    pub system: String,

    /// User prompt template with optional `${variable}` placeholders.
    pub prompt: String,

    /// Optional JSON Schema describing the expected output structure.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// PromptRegistry
// ---------------------------------------------------------------------------

/// A registry of named prompt templates loaded from a directory of YAML
/// files.
pub struct PromptRegistry {
    prompts: HashMap<String, Prompt>,
}

impl PromptRegistry {
    /// Load all `*.yaml` / `*.yml` prompt files from `prompts_dir`.
    ///
    /// Each file must contain a single [`Prompt`] struct.  File names are
    /// used as the key after stripping the extension, but the `name` field
    /// inside the file takes precedence for lookups via [`get`](Self::get)
    /// and [`render`](Self::render).
    pub fn load(prompts_dir: &Path) -> Result<Self, String> {
        if !prompts_dir.is_dir() {
            return Err(format!("prompts directory not found: {:?}", prompts_dir));
        }

        let mut prompts = HashMap::new();

        let dir_entries =
            std::fs::read_dir(prompts_dir).map_err(|e| format!("cannot read prompts dir: {e}"))?;

        for entry in dir_entries {
            let entry = entry.map_err(|e| format!("bad entry in prompts dir: {e}"))?;
            let path = entry.path();

            // Only accept .yaml / .yml files
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }

            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {:?}: {e}", path))?;

            let prompt: Prompt = serde_yaml::from_str(&content)
                .map_err(|e| format!("parse error in {:?}: {e}", path))?;

            let key = prompt.name.clone();
            prompts.insert(key, prompt);
        }

        Ok(Self { prompts })
    }

    /// Retrieve a prompt by name.
    pub fn get(&self, name: &str) -> Option<&Prompt> {
        self.prompts.get(name)
    }

    /// Render a named prompt by substituting `${variable}` placeholders
    /// with values from the JSON `context`.
    ///
    /// # Returns
    ///
    /// A tuple `(system_prompt, rendered_user_prompt)`.
    ///
    /// # Errors
    ///
    /// Returns an error if `prompt_name` is not in the registry.
    ///
    /// Unknown variables (those not present in `context`) are left as-is
    /// in the output — they are **not** silently removed so that callers
    /// can detect missing keys.
    pub fn render(
        &self,
        prompt_name: &str,
        context: &serde_json::Value,
    ) -> Result<(String, String), String> {
        let prompt = self
            .prompts
            .get(prompt_name)
            .ok_or_else(|| format!("prompt not found: {prompt_name}"))?;

        let rendered = render_template(&prompt.prompt, context);

        Ok((prompt.system.clone(), rendered))
    }
}

// ---------------------------------------------------------------------------
// Template rendering
// ---------------------------------------------------------------------------

/// Replace `${variable}` or `${nested.key}` placeholders with values from
/// `context`.
///
/// The lookup walks the JSON value tree: `"${file_path}"` becomes
/// `context["file_path"]`, and `"${tree_sitter.entities}"` becomes
/// `context["tree_sitter"]["entities"]`.
///
/// Placeholders whose paths do not exist in `context` are left unchanged.
fn render_template(template: &str, context: &serde_json::Value) -> String {
    // Matches `${...}` but not `$${...}` (escaped).
    let re = regex::Regex::new(r"\$\{([^}]+)\}").expect("hard-coded regex is valid");

    re.replace_all(template, |caps: &regex::Captures<'_>| {
        let key_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        resolve_json_path(context, key_path)
            .unwrap_or_else(|| caps.get(0).unwrap().as_str().to_string())
    })
    .to_string()
}

/// Walk a dotted path (e.g. `"tree_sitter.entities"`) into a JSON value.
///
/// Returns `None` when any segment of the path does not exist.
fn resolve_json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('.').collect();
    let mut current = value;

    for segment in &segments {
        // Try object field first, then array index.
        if let Some(obj) = current.as_object() {
            current = obj.get(*segment)?;
        } else if let Some(arr) = current.as_array() {
            let idx: usize = segment.parse().ok()?;
            current = arr.get(idx)?;
        } else {
            return None;
        }
    }

    match current {
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_simple_variable() {
        let ctx = json!({"file_path": "src/main.rs"});
        let result = render_template("File: ${file_path}", &ctx);
        assert_eq!(result, "File: src/main.rs");
    }

    #[test]
    fn render_nested_variable() {
        let ctx = json!({"tree_sitter": {"entities": "Class,Method"}});
        let result = render_template("AST: ${tree_sitter.entities}", &ctx);
        assert_eq!(result, "AST: Class,Method");
    }

    #[test]
    fn render_missing_variable_left_intact() {
        let ctx = json!({});
        let result = render_template("Hello ${unknown}", &ctx);
        assert_eq!(result, "Hello ${unknown}");
    }

    #[test]
    fn render_array_access() {
        let ctx = json!({"items": ["a", "b", "c"]});
        let result = render_template("First: ${items.0}", &ctx);
        assert_eq!(result, "First: a");
    }

    #[test]
    fn render_multiple_variables() {
        let ctx = json!({"a": "X", "b": "Y"});
        let result = render_template("${a} + ${b} = ${a}", &ctx);
        assert_eq!(result, "X + Y = X");
    }

    #[test]
    fn render_non_string_value() {
        let ctx = json!({"count": 42});
        let result = render_template("Count: ${count}", &ctx);
        assert_eq!(result, "Count: 42");
    }

    #[test]
    fn prompt_deserialize() {
        let yaml = r#"
name: test_prompt
description: A test
system: You are a test assistant.
prompt: "Analyze: ${file_path}"
"#;
        let prompt: Prompt = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(prompt.name, "test_prompt");
        assert_eq!(prompt.system, "You are a test assistant.");
        assert_eq!(prompt.prompt, "Analyze: ${file_path}");
        assert!(prompt.output_schema.is_none());
    }

    #[test]
    fn prompt_with_output_schema() {
        let yaml = r#"
name: structured
description: Structured output test
system: Return JSON
prompt: "Analyze: ${file_path}"
output_schema:
  type: object
  properties:
    entities:
      type: array
"#;
        let prompt: Prompt = serde_yaml::from_str(yaml).unwrap();
        assert!(prompt.output_schema.is_some());
        let schema = prompt.output_schema.unwrap();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn registry_roundtrip() {
        // Write a temporary prompt file, load it, verify.
        let dir = std::env::temp_dir().join("dt_prompt_test");
        let _ = std::fs::create_dir_all(&dir);

        let yaml = r#"
name: test_prompt
description: A test
system: You are a test
prompt: "File: ${file_path}
"
"#;
        let file_path = dir.join("test.yaml");
        std::fs::write(&file_path, yaml).unwrap();

        let registry = PromptRegistry::load(&dir).unwrap();
        assert!(registry.get("test_prompt").is_some());

        let ctx = json!({"file_path": "hello.rs"});
        let (system, user) = registry.render("test_prompt", &ctx).unwrap();
        assert_eq!(system, "You are a test");
        assert!(user.contains("hello.rs"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
