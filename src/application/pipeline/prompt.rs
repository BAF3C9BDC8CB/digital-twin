//! 提示词模板系统。
//!
//! 存放在 `config/prompts/` 下的基于 YAML 的提示词模板在启动时加载到
//! [`PromptRegistry`] 中。每个模板可包含 `${variable}` 占位符，在渲染时
//! 从 JSON 上下文值中替换。
//!
//! # 示例
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
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------

/// 单个命名提示词模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    /// 机器友好名称（例如 `"code_with_ast"`）。
    pub name: String,

    /// 该提示词用途的人类可读描述。
    pub description: String,

    /// 系统级指令（作为 `system` 角色的消息传入）。
    pub system: String,

    /// 带可选 `${variable}` 占位符的用户提示词模板。
    pub prompt: String,

    /// 描述预期输出结构的可选 JSON Schema。
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// PromptRegistry
// ---------------------------------------------------------------------------

/// 从 YAML 文件目录加载的命名提示词模板注册表。
pub struct PromptRegistry {
    prompts: HashMap<String, Prompt>,
}

impl PromptRegistry {
    /// 从 `prompts_dir` 加载所有 `*.yaml` / `*.yml` 提示词文件。
    ///
    /// 每个文件必须包含一个 [`Prompt`] 结构。去掉扩展名后文件名用作键，
    /// 但文件内部的 `name` 字段优先用于 [`get`](Self::get)
    /// 与 [`render`](Self::render) 的查找。
    pub fn load(prompts_dir: &Path) -> Result<Self, String> {
        if !prompts_dir.is_dir() {
            return Err(format!("提示词目录不存在: {:?}", prompts_dir));
        }

        let mut prompts = HashMap::new();

        let dir_entries =
            std::fs::read_dir(prompts_dir).map_err(|e| format!("无法读取提示词目录: {e}"))?;

        for entry in dir_entries {
            let entry = entry.map_err(|e| format!("提示词目录中的条目异常: {e}"))?;
            let path = entry.path();

            // 仅接受 .yaml / .yml 文件
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }

            let content =
                std::fs::read_to_string(&path).map_err(|e| format!("无法读取 {:?}: {e}", path))?;

            let prompt: Prompt = serde_yaml::from_str(&content)
                .map_err(|e| format!("解析 {:?} 时出错: {e}", path))?;

            let key = prompt.name.clone();
            prompts.insert(key, prompt);
        }

        Ok(Self { prompts })
    }

    /// 默认提示词目录加载：多路径搜索，按优先级取第一个存在者。
    ///
    /// 查找顺序：
    /// 1. 环境变量 `DT_PROMPTS_DIR`
    /// 2. 当前工作目录的 `config/prompts`
    /// 3. `~/.config/digital-twin/prompts`
    /// 4. 可执行文件所在目录的 `config/prompts`
    ///
    /// 所有候选均不存在时返回错误。
    pub fn load_default() -> Result<Self, String> {
        let mut candidates: Vec<PathBuf> = Vec::new();

        // 1) 环境变量
        if let Ok(dir) = std::env::var("DT_PROMPTS_DIR") {
            candidates.push(PathBuf::from(dir));
        }

        // 2) CWD 的 config/prompts
        candidates.push(PathBuf::from("config/prompts"));

        // 3) 用户级固定路径（与 pipeline.yaml 一致约定）
        if let Some(home) = crate::shared::home_dir() {
            candidates.push(home.join(".config").join("digital-twin").join("prompts"));
        }

        // 4) 可执行文件所在目录的 config/prompts
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                candidates.push(exe_dir.join("config").join("prompts"));
            }
        }

        for dir in &candidates {
            if dir.is_dir() {
                tracing::debug!(path = %dir.display(), "PromptRegistry 使用提示词目录");
                return Self::load(dir);
            }
        }

        Err(format!(
            "未找到提示词目录（已尝试: {}）",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// 按名称获取提示词。
    pub fn get(&self, name: &str) -> Option<&Prompt> {
        self.prompts.get(name)
    }

    /// 通过用 JSON `context` 中的值替换 `${variable}` 占位符来渲染
    /// 命名提示词。
    ///
    /// # 返回值
    ///
    /// 元组 `(system_prompt, 渲染后的用户提示词)`。
    ///
    /// # 错误
    ///
    /// 如果 `prompt_name` 不在注册表中则返回错误。
    ///
    /// 未知变量（`context` 中不存在的变量）在输出中原样保留——它们
    /// **不会**被静默删除，以便调用方能够发现缺失的键。
    pub fn render(
        &self,
        prompt_name: &str,
        context: &serde_json::Value,
    ) -> Result<(String, String), String> {
        let prompt = self
            .prompts
            .get(prompt_name)
            .ok_or_else(|| format!("提示词不存在: {prompt_name}"))?;

        let rendered = render_template(&prompt.prompt, context);

        Ok((prompt.system.clone(), rendered))
    }
}

// ---------------------------------------------------------------------------
// 模板渲染
// ---------------------------------------------------------------------------

/// 用 `context` 中的值替换 `${variable}` 或 `${nested.key}` 占位符。
///
/// 查找遍历 JSON 值树：`"${file_path}"` 变为 `context["file_path"]`，
/// `"${tree_sitter.entities}"` 变为 `context["tree_sitter"]["entities"]`。
///
/// 路径在 `context` 中不存在的占位符保持原样。
fn render_template(template: &str, context: &serde_json::Value) -> String {
    // 匹配 `${...}` 但不匹配 `$${...}`（转义）。
    let re = regex::Regex::new(r"\$\{([^}]+)\}").expect("硬编码的正则必须有效");

    re.replace_all(template, |caps: &regex::Captures<'_>| {
        let key_path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        resolve_json_path(context, key_path)
            .unwrap_or_else(|| caps.get(0).unwrap().as_str().to_string())
    })
    .to_string()
}

/// 沿点分路径（例如 `"tree_sitter.entities"`）在 JSON 值中遍历。
///
/// 当路径的任意段不存在时返回 `None`。
fn resolve_json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('.').collect();
    let mut current = value;

    for segment in &segments {
        // 先尝试对象字段，再尝试数组索引。
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
// 测试
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
        // 写一个临时提示词文件，加载并验证。
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

        // 清理
        let _ = std::fs::remove_dir_all(&dir);
    }
}
