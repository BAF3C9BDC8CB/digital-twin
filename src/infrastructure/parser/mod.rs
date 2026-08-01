//! Parser registry — Strategy pattern for multi-language AST parsing.
//!
//! `ParserRegistry` holds a collection of language-specific parsers,
//! each implementing the `ParseStrategy` trait. When a file is submitted,
//! the registry iterates through its parsers to find one that can handle it.

pub mod document;
pub mod go;
pub mod java;
pub mod javascript;
pub mod php;
pub mod python;
pub mod rust_parser;
pub mod tree_sitter_utils;
pub mod ts_go;
pub mod ts_java;
pub mod ts_javascript;
pub mod ts_php;
pub mod ts_python;
pub mod ts_rust;
pub mod ts_typescript;
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

/// Find the closing brace that matches the opening brace at `open_byte`.
/// Returns the 1-based line number of the closing brace.
/// `source` is the full file source text, `open_byte` is the byte offset of `{`.
pub fn find_brace_end_line(source: &str, open_byte: usize) -> usize {
    let bytes = source.as_bytes();
    if open_byte >= bytes.len() || bytes[open_byte] != b'{' {
        // No brace found — fall back to counting lines from open_byte
        return source[open_byte..].lines().count();
    }
    let mut depth = 1u32;
    let mut end_byte = open_byte;
    for (i, &b) in bytes.iter().enumerate().skip(open_byte + 1) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end_byte = i;
                    break;
                }
            }
            _ => {}
        }
    }
    // Count newlines from the opening brace to the closing brace
    source[open_byte..=end_byte]
        .chars()
        .filter(|&c| c == '\n')
        .count()
}
use self::typescript::TypeScriptParser;

/// Registry that dispatches file parsing to the appropriate language parser.
pub struct ParserRegistry {
    parsers: Vec<Box<dyn ParseStrategy>>,
}

impl ParserRegistry {
    /// Create a new registry with all 7 supported language parsers.
    pub fn new() -> Self {
        Self {
            parsers: vec![
                // Tree-sitter backed parsers (priority)
                Box::new(self::ts_java::TsJavaParser),
                Box::new(self::ts_python::TsPythonParser),
                Box::new(self::ts_javascript::TsJavaScriptParser),
                Box::new(self::ts_typescript::TsTypeScriptParser),
                Box::new(self::ts_go::TsGoParser),
                Box::new(self::ts_rust::TsRustParser),
                Box::new(self::ts_php::TsPhpParser),
                // Regex fallback parsers
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

}
