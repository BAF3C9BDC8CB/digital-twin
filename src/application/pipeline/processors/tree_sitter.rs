//! Tree-sitter AST 处理器——包装 [`ParserRegistry`] 从源文件中提取代码
//! 实体（classes、methods、fields）。
//!
//! 产生一个带两个键的 [`ProcessorOutput`]：
//! - `"entities"`   —— 包含 `classes` 与 `methods` 数组的 JSON 对象
//! - `"imports"`    —— import/use 语句列表（裸结构，对没有完整导入树
//!   提取器的语言可能为空）

use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use crate::application::pipeline::context::PipelineContext;
use crate::application::pipeline::output::ProcessorOutput;
use crate::application::pipeline::processor::Processor;
use crate::domain::error::DtError;
use crate::infrastructure::parser::ParserRegistry;

/// 使用 tree-sitter 语法做基于 AST 的代码解析器。
///
/// 该处理器处理源代码文件扩展名（.java、.py、.rs、.go、.ts、.tsx、.js、
/// .jsx、.php），并使用共享的 `ParserRegistry` 产生结构化实体数据。
pub struct TreeSitterProcessor {
    registry: Arc<ParserRegistry>,
}

impl TreeSitterProcessor {
    /// 创建包装给定解析器注册表的新处理器。
    pub fn new(registry: Arc<ParserRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Processor for TreeSitterProcessor {
    fn name(&self) -> &str {
        "tree_sitter"
    }

    fn priority(&self) -> i32 {
        100
    }

    fn matches(&self, file_path: &Path) -> bool {
        matches!(
            file_path.extension().and_then(|e| e.to_str()),
            Some("java" | "py" | "rs" | "go" | "ts" | "tsx" | "js" | "jsx" | "php")
        )
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        let mut output = ProcessorOutput::new();

        // 使用注册表解析文件。
        let parse_result =
            self.registry
                .parse_file(&ctx.file_text, &ctx.file_path, &ctx.project_name)?;

        // 将实体存储为带 "classes" 与 "methods" 的 JSON 对象。
        let entities = serde_json::json!({
            "classes": parse_result.classes.iter().map(|c| {
                serde_json::json!({
                    "id": c.class_id,
                    "name": c.name,
                    "kind": c.kind.as_str(),
                    "file_path": c.file_path,
                    "package": c.package_or_module,
                    "start_line": c.start_line,
                    "end_line": c.end_line,
                })
            }).collect::<Vec<_>>(),
            "methods": parse_result.methods.iter().map(|m| {
                serde_json::json!({
                    "id": m.method_id,
                    "name": m.name,
                    "signature": m.signature,
                    "params": m.params,
                    "return_type": m.return_type,
                    "class_name": m.class_name,
                    "file_path": m.file_path,
                    "package": m.package_or_module,
                    "language": m.language,
                    "start_line": m.start_line,
                    "end_line": m.end_line,
                    "calls": m.calls,
                    "comment": m.comment,
                })
            }).collect::<Vec<_>>(),
        });
        output.set("entities", entities);

        // 基础导入提取——从解析出的类中收集唯一的 package/module 路径，
        // 作为简单列表。
        let import_paths: Vec<String> = {
            let mut paths: Vec<String> = parse_result
                .classes
                .iter()
                .map(|c| c.package_or_module.clone())
                .filter(|p| !p.is_empty())
                .collect();
            paths.sort();
            paths.dedup();
            paths
        };
        output.set("imports", import_paths);

        // 同时存储原始的 method 与 class 数量，便于引用。
        output.set("method_count", parse_result.methods.len());
        output.set("class_count", parse_result.classes.len());

        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_context(file_name: &str, text: &str) -> PipelineContext {
        PipelineContext::new(
            PathBuf::from(file_name),
            text.to_string(),
            "test_project".to_string(),
        )
    }

    #[tokio::test]
    async fn matches_code_extensions() {
        let processor = TreeSitterProcessor::new(Arc::new(ParserRegistry::new()));
        assert!(processor.matches(Path::new("main.rs")));
        assert!(processor.matches(Path::new("Main.java")));
        assert!(processor.matches(Path::new("app.py")));
        assert!(processor.matches(Path::new("server.go")));
        assert!(processor.matches(Path::new("component.ts")));
        assert!(processor.matches(Path::new("component.tsx")));
        assert!(processor.matches(Path::new("app.js")));
        assert!(processor.matches(Path::new("app.jsx")));
        assert!(processor.matches(Path::new("index.php")));
        assert!(!processor.matches(Path::new("README.md")));
        assert!(!processor.matches(Path::new("config.yaml")));
    }

    #[tokio::test]
    async fn executes_rust_file() {
        let processor = TreeSitterProcessor::new(Arc::new(ParserRegistry::new()));
        let ctx = make_context("lib.rs", "fn greet() -> &'static str { \"hello\" }");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.get("entities").is_some());
        assert!(output.get("imports").is_some());
    }

    #[tokio::test]
    async fn executes_java_file() {
        let processor = TreeSitterProcessor::new(Arc::new(ParserRegistry::new()));
        let ctx = make_context("Foo.java", "class Foo { }");
        let result = processor.execute(&ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.get("entities").is_some());
        assert!(output.get("class_count").is_some());
    }

    #[tokio::test]
    async fn rejects_unsupported_file() {
        let processor = TreeSitterProcessor::new(Arc::new(ParserRegistry::new()));
        let ctx = make_context("readme.md", "# Hello");
        let result = processor.execute(&ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn name_and_priority() {
        let processor = TreeSitterProcessor::new(Arc::new(ParserRegistry::new()));
        assert_eq!(processor.name(), "tree_sitter");
        assert_eq!(processor.priority(), 100);
    }
}
