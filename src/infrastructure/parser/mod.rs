//! Parser registry — Strategy pattern for multi-language AST parsing.
//!
//! `ParserRegistry` holds a collection of language-specific parsers,
//! each implementing the `ParseStrategy` trait. When a file is submitted,
//! the registry iterates through its parsers to find one that can handle it.
//!
//! Also re-exports [`extract_knowledge_annotations`] from `dt-knowledge`
//! for use during the pipeline's annotation-extraction step.

pub mod document;
pub mod go;
pub mod java;
pub mod javascript;
pub mod php;
pub mod python;
pub mod rust_parser;
pub mod typescript;

use crate::domain::error::DtError;
use crate::domain::traits::ParseStrategy;
use crate::domain::types::ParseResult;
use std::path::Path;
use std::sync::Arc;

use self::go::GoParser;
use self::java::JavaParser;
use self::javascript::JavaScriptParser;
use self::php::PhpParser;
use self::python::PythonParser;
use self::rust_parser::RustParser;
use self::typescript::TypeScriptParser;

// Re-export knowledge annotation extraction for pipeline use.
pub use crate::application::knowledge::knowledge::annotation::{
    extract_knowledge_annotations, KnowledgeAnnotation,
};

/// Registry that dispatches file parsing to the appropriate language parser.
pub struct ParserRegistry {
    parsers: Vec<Box<dyn ParseStrategy>>,
}

impl ParserRegistry {
    /// Create a new registry with all 7 supported language parsers.
    pub fn new() -> Self {
        Self {
            parsers: vec![
                Box::new(JavaParser),
                Box::new(TypeScriptParser),
                Box::new(PythonParser),
                Box::new(GoParser),
                Box::new(RustParser),
                Box::new(PhpParser),
                Box::new(JavaScriptParser),
            ],
        }
    }

    /// Parse a single file using the first matching language parser.
    ///
    /// Returns `ParseResult` with extracted methods and classes, or an
    /// error if no parser could handle the file or parsing failed.
    pub fn parse_file(
        &self,
        source: &str,
        path: &Path,
        project: &str,
    ) -> Result<ParseResult, DtError> {
        for parser in &self.parsers {
            if parser.can_parse(path) {
                return parser.parse(source, path, project);
            }
        }
        Err(DtError::General(format!(
            "no parser available for: {}",
            path.display()
        )))
    }

    /// Create a registry suitable for use with `Arc`.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn registry_detects_java() {
        let reg = ParserRegistry::new();
        let path = PathBuf::from("src/Foo.java");
        let result = reg.parse_file("class Foo {}", &path, "test");
        assert!(result.is_ok());
        assert!(result.unwrap().classes.len() >= 1);
    }

    #[test]
    fn registry_detects_python() {
        let reg = ParserRegistry::new();
        let path = PathBuf::from("foo.py");
        let result = reg.parse_file("def hello(): pass\nclass Bar:\n  pass", &path, "test");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert!(r.methods.len() >= 1);
        assert!(r.classes.len() >= 1);
    }

    #[test]
    fn registry_rejects_unknown_ext() {
        let reg = ParserRegistry::new();
        let path = PathBuf::from("README.md");
        let result = reg.parse_file("# Hello", &path, "test");
        assert!(result.is_err());
    }

    #[test]
    fn registry_detects_typescript() {
        let reg = ParserRegistry::new();
        let path = PathBuf::from("app.ts");
        let result = reg.parse_file("function main() {}\nclass App {}", &path, "test");
        assert!(result.is_ok());
    }

    // -------------------------------------------------------------------
    // @knowledge annotation extraction tests
    // -------------------------------------------------------------------

    #[test]
    fn extract_java_javadoc_block_comment() {
        let source = r#"/**
 * @knowledge domain="支付" concept="ifCode"
 * 支付渠道编码，用于路由到不同支付平台。
 */
private String ifCode;"#;

        let anns = extract_knowledge_annotations(source, "src/PayService.java", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("支付"));
        assert_eq!(a.concept.as_deref(), Some("ifCode"));
        assert!(a.pitfall.is_none());
        assert!(a.definition.is_none());
        assert!(a.description.contains("支付渠道编码"));
        assert_eq!(a.line_number, 2); // /** starts at line 2 (1-indexed)
        assert_eq!(a.file_path, "src/PayService.java");
    }

    #[test]
    fn extract_python_single_line_hash() {
        let source = r#"# @knowledge domain="部署" concept="healthcheck" definition="服务健康检查端点"
def health():
    return "ok""#;

        let anns = extract_knowledge_annotations(source, "app.py", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("部署"));
        assert_eq!(a.concept.as_deref(), Some("healthcheck"));
        assert_eq!(a.definition.as_deref(), Some("服务健康检查端点"));
        assert!(a.pitfall.is_none());
        assert!(a.description.is_empty());
        assert_eq!(a.file_path, "app.py");
    }

    #[test]
    fn extract_go_line_comment_with_pitfall() {
        let source = r#"package pay

// @knowledge domain="支付" concept="channelExtra" pitfall="修改ifCode时容易遗漏channelExtra配置"
func getChannelExtra() string {
    return ""
}"#;

        let anns = extract_knowledge_annotations(source, "pay/channel.go", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("支付"));
        assert_eq!(a.concept.as_deref(), Some("channelExtra"));
        assert_eq!(a.pitfall.as_deref(), Some("修改ifCode时容易遗漏channelExtra配置"));
        assert!(a.definition.is_none());
        assert!(a.description.is_empty());
    }

    #[test]
    fn extract_rust_doc_comment() {
        let source = r#"/// @knowledge domain="配置" concept="timeout" definition="RPC超时配置（秒）"
const TIMEOUT: u64 = 30;"#;

        let anns = extract_knowledge_annotations(source, "src/config.rs", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("配置"));
        assert_eq!(a.concept.as_deref(), Some("timeout"));
        assert_eq!(a.definition.as_deref(), Some("RPC超时配置（秒）"));
    }

    #[test]
    fn extract_javascript_single_line_comment() {
        let source = r#"// @knowledge domain="前端" concept="tokenRefresh" definition="Access Token自动刷新机制"
const refreshToken = () => {};"#;

        let anns = extract_knowledge_annotations(source, "auth.js", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("前端"));
        assert_eq!(a.concept.as_deref(), Some("tokenRefresh"));
        assert_eq!(a.definition.as_deref(), Some("Access Token自动刷新机制"));
    }

    #[test]
    fn extract_python_docstring() {
        let source = r#""""
@knowledge domain="AI" concept="rag-retrieval" definition="RAG检索增强生成管道"
"""
def retrieve():
    pass"#;

        let anns = extract_knowledge_annotations(source, "rag.py", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("AI"));
        assert_eq!(a.concept.as_deref(), Some("rag-retrieval"));
        assert_eq!(a.definition.as_deref(), Some("RAG检索增强生成管道"));
        assert_eq!(a.file_path, "rag.py");
    }

    #[test]
    fn extract_php_block_comment() {
        let source = r#"<?php
/**
 * @knowledge domain="Web" concept="csrf-guard" definition="CSRF防护中间件"
 */
class CsrfGuard {}"#;

        let anns = extract_knowledge_annotations(source, "middleware.php", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("Web"));
        assert_eq!(a.concept.as_deref(), Some("csrf-guard"));
        assert_eq!(a.definition.as_deref(), Some("CSRF防护中间件"));
        assert_eq!(a.file_path, "middleware.php");
    }

    #[test]
    fn extract_typescript_single_line_jsdoc_style() {
        let source = r#"/** @knowledge domain="类型" concept="Result<T>" definition="泛型Result包装" */
type Result<T> = { data: T; error?: string };"#;

        let anns = extract_knowledge_annotations(source, "types.ts", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("类型"));
        assert_eq!(a.concept.as_deref(), Some("Result<T>"));
        assert_eq!(a.definition.as_deref(), Some("泛型Result包装"));
    }

    #[test]
    fn no_knowledge_annotation_returns_empty() {
        let source = r#"class Foo {
    // This is a regular comment
    private int x;
}
"#;
        let anns = extract_knowledge_annotations(source, "Foo.java", "test");
        assert!(anns.is_empty());
    }

    #[test]
    fn knowledge_with_experience_field() {
        let source = r#"# @knowledge domain="支付" concept="channelSwitch" experience="支付渠道切换导致回滚"
class ChannelSwitch:"#;

        let anns = extract_knowledge_annotations(source, "switch.py", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("支付"));
        assert_eq!(a.concept.as_deref(), Some("channelSwitch"));
        assert_eq!(a.experience.as_deref(), Some("支付渠道切换导致回滚"));
        assert!(a.definition.is_none());
        assert!(a.pitfall.is_none());
    }

    #[test]
    fn knowledge_with_single_quotes() {
        let source = r#"// @knowledge domain='数据库' concept='connectionPool' definition='连接池配置'
let pool = initPool();"#;

        let anns = extract_knowledge_annotations(source, "db.js", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("数据库"));
        assert_eq!(a.concept.as_deref(), Some("connectionPool"));
        assert_eq!(a.definition.as_deref(), Some("连接池配置"));
    }

    #[test]
    fn knowledge_single_line_block_with_description() {
        let source = r#"/* @knowledge domain="日志" concept="structuredLog" 使用JSON格式的结构化日志 */
func log() {}"#;

        let anns = extract_knowledge_annotations(source, "log.go", "test");
        assert_eq!(anns.len(), 1);
        let a = &anns[0];
        assert_eq!(a.domain.as_deref(), Some("日志"));
        assert_eq!(a.concept.as_deref(), Some("structuredLog"));
        assert!(a.description.contains("使用JSON格式"));
    }

    #[test]
    fn multiple_knowledge_in_one_file() {
        let source = r#"# @knowledge domain="支付" concept="ifCode" definition="支付渠道编码"
# @knowledge domain="支付" concept="channelExtra" definition="渠道扩展参数"
class PayService:
    pass"#;

        let anns = extract_knowledge_annotations(source, "pay.py", "test");
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].concept.as_deref(), Some("ifCode"));
        assert_eq!(anns[1].concept.as_deref(), Some("channelExtra"));
    }
}
