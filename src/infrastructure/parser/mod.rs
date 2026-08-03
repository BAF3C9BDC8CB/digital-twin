//! 解析器注册表——多语言 AST 解析的策略模式。
//!
//! `ParserRegistry` 持有一组语言特定的解析器，每个解析器都实现了
//! `ParseStrategy` trait。当提交一个文件时，注册表会遍历其解析器
//! 以找到能处理该文件的那个。

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

/// 找到与 `open_byte` 处左花括号匹配的右花括号。
/// 返回右花括号的 1 起始行号。
/// `source` 是完整的文件源码文本，`open_byte` 是 `{` 的字节偏移。
pub fn find_brace_end_line(source: &str, open_byte: usize) -> usize {
    let bytes = source.as_bytes();
    if open_byte >= bytes.len() || bytes[open_byte] != b'{' {
        // 未找到花括号——回退为从 open_byte 起统计行数
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
    // 统计从左花括号到右花括号之间的换行数
    source[open_byte..=end_byte]
        .chars()
        .filter(|&c| c == '\n')
        .count()
}
use self::typescript::TypeScriptParser;

/// 将文件解析分派给相应语言解析器的注册表。
pub struct ParserRegistry {
    parsers: Vec<Box<dyn ParseStrategy>>,
}

impl ParserRegistry {
    /// 创建包含全部 7 种受支持语言解析器的新注册表。
    pub fn new() -> Self {
        Self {
            parsers: vec![
                // Tree-sitter 支撑的解析器（优先）
                Box::new(self::ts_java::TsJavaParser),
                Box::new(self::ts_python::TsPythonParser),
                Box::new(self::ts_javascript::TsJavaScriptParser),
                Box::new(self::ts_typescript::TsTypeScriptParser),
                Box::new(self::ts_go::TsGoParser),
                Box::new(self::ts_rust::TsRustParser),
                Box::new(self::ts_php::TsPhpParser),
                // 正则回退解析器
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

    /// 使用第一个匹配的语言解析器解析单个文件。
    ///
    /// 返回带有已抽取方法与类的 `ParseResult`，如果没有任何解析器
    /// 能处理该文件或解析失败则返回错误。
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
            "没有可用的解析器: {}",
            path.display()
        )))
    }

    /// 创建适合与 `Arc` 一起使用的注册表。
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
// 测试
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
