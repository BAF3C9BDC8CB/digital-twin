//! Java parser — regex-based extraction of methods and classes.
//!
//! Parses `.java` files to extract method declarations, constructors,
//! and class/interface/enum definitions.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use regex::Regex;
use std::path::Path;

/// Java source code parser.
pub struct JavaParser;

impl JavaParser {
    /// Extract the package name from a Java source file.
    fn extract_package(source: &str) -> String {
        let re = Regex::new(r"package\s+([a-zA-Z_][\w.]*)\s*;").unwrap();
        re.captures(source)
            .map(|c| c[1].to_string())
            .unwrap_or_default()
    }

    /// Extract method calls from source body text.
    fn extract_calls(body: &str) -> Vec<String> {
        let re = Regex::new(r"\b([a-zA-Z_]\w*)\s*\(").unwrap();
        let mut calls: Vec<String> = re
            .captures_iter(body)
            .filter_map(|c| {
                let name = c[1].to_string();
                // Skip keywords and common control flow
                if matches!(
                    name.as_str(),
                    "if" | "for" | "while" | "switch" | "catch" | "return"
                        | "throw" | "new" | "try" | "synchronized" | "assert"
                ) {
                    None
                } else {
                    Some(name)
                }
            })
            .collect();
        calls.sort();
        calls.dedup();
        calls.truncate(50);
        calls
    }

    /// Find Javadoc comment before a method.
    fn find_comment(lines: &[&str], method_line: usize) -> String {
        if method_line == 0 {
            return String::new();
        }
        let mut comments = Vec::new();
        let mut idx = method_line.saturating_sub(1);
        // Walk backwards from the line before the method
        loop {
            let line = lines.get(idx).map(|s| s.trim()).unwrap_or("");
            if line.starts_with("*/") || line.starts_with("*") || line.starts_with("//") {
                comments.push(line.to_string());
                if line.starts_with("/*") {
                    break;
                }
                if idx == 0 {
                    break;
                }
                idx -= 1;
            } else if line.is_empty() && idx > 0 {
                idx -= 1;
            } else {
                break;
            }
        }
        comments.reverse();
        comments.join(" ").chars().take(200).collect()
    }
}

impl ParseStrategy for JavaParser {
    fn language(&self) -> Language {
        Language::Java
    }

    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "java")
            .unwrap_or(false)
    }

    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let file_path = path.to_string_lossy().to_string();
        let file_path_no_slash = file_path.replace('\\', "/");
        let package = Self::extract_package(source);
        let lines: Vec<&str> = source.lines().collect();

        let mut methods = Vec::new();
        let mut classes = Vec::new();

        // Regex for method declarations (including constructors and interface methods).
        // `[;\{]` matches both interface methods (;) and concrete methods ({).
        let method_re = Regex::new(
            r"(?m)^\s*(public|private|protected|static|\s)*\s*([\w<>\[\],\s]+)\s+(\w+)\s*\(([^)]*)\)\s*(throws\s+[\w\s,]+)?\s*[;\{]"
        ).unwrap();

        // Regex for class declarations (also handles generics like Foo<E>)
        let class_re = Regex::new(
            r"(?m)^\s*(public\s+)?(abstract\s+)?(class|interface|enum)\s+(\w+)\s*(<[^>]+>)?\s*(extends\s+[\w\s,<>]+)?\s*(implements\s+[\w\s,<>.,]+)?\s*\{"
        ).unwrap();

        // Find classes first
        for caps in class_re.captures_iter(source) {
            let class_name = caps[4].to_string();
            let kind_str = caps[3].to_string();
            let kind = match kind_str.as_str() {
                "interface" => ClassKind::Interface,
                "enum" => ClassKind::Enum,
                _ => ClassKind::Class,
            };

            let class_match = caps.get(0).unwrap();
            let start_byte = class_match.start();
            let start_line = source[..start_byte].lines().count() + 1;
            let end_line = 0; // Will be estimated later

            let class_id = make_class_id(project, &package, &class_name);
            classes.push(ClassBlock {
                class_id,
                name: class_name.clone(),
                kind,
                file_path: file_path_no_slash.clone(),
                package_or_module: package.clone(),
                project: project.to_string(),
                start_line,
                end_line,
                method_ids: Vec::new(),
            });
        }

        // Find methods
        let mut current_class: String = String::new();
        for caps in method_re.captures_iter(source) {
            let modifiers = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            let return_type = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("void");
            let method_name = caps[3].to_string();
            let params = caps[4].to_string();

            // Skip if it's a keyword
            if method_name == "class" || method_name == "interface" || method_name == "enum" {
                continue;
            }

            let full_match = caps.get(0).unwrap();
            let start_byte = full_match.start();
            let start_line = {
                // Compute line from byte position
                let raw_line = source[..start_byte].lines().count() + 1;
                // Skip blank lines: when a method is preceded by a blank line, `(?m)^`
                // matches at the blank line's start, inflating start_line by 1.
                // Walk forward to the first non-blank line.
                let mut adjusted = raw_line;
                while adjusted <= lines.len() && lines[adjusted - 1].trim().is_empty() {
                    adjusted += 1;
                }
                adjusted
            };

            // Determine which class contains this method (crude heuristic)
            if current_class.is_empty() {
                for c in &classes {
                    if c.start_line < start_line {
                        current_class = c.name.clone();
                    }
                }
            } else {
                // Check if we've passed a class boundary
                for c in &classes {
                    if c.start_line < start_line && c.start_line > start_line.saturating_sub(50) {
                        current_class = c.name.clone();
                    }
                }
            }
            if current_class.is_empty() {
                current_class = "_top_level_".to_string();
            }

            let method_id = make_method_id(project, &file_path_no_slash, &current_class, &method_name, start_line);

            // Extract the method body for calls and source text.
            // Interface methods end with `;` — skip body extraction.
            let body = if full_match.as_str().trim_end().ends_with('{') {
                let body_start = full_match.end();
                find_matching_brace(source, body_start.saturating_sub(1))
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            };
            let calls = Self::extract_calls(&body);
            let comment = Self::find_comment(&lines, start_line - 1);

            let signature = format!(
                "{} {} {}({})",
                modifiers,
                return_type,
                method_name,
                params
            )
            .trim()
            .to_string();

            let end_line = if body.is_empty() {
                start_line
            } else {
                // Count newlines in body text (from { to }) to get actual end line
                start_line + body.chars().filter(|&c| c == '\n').count()
            };

            methods.push(MethodBlock {
                method_id,
                name: method_name,
                signature,
                params: params.to_string(),
                return_type: get_last_type_part(return_type),
                class_name: current_class.clone(),
                file_path: file_path_no_slash.clone(),
                package_or_module: package.clone(),
                language: "java".to_string(),
                project: project.to_string(),
                start_line,
                end_line,
                calls,
                comment,
                source_text: body,
            });

            current_class.clear();
        }

        // Filter out phantom methods caused by regex matching method body calls.
        // Build ranges for all methods that actually have a body ({ ... }), i.e. end_line > start_line.
        let concrete_ranges: Vec<(usize, usize)> = methods.iter()
            .filter(|m| m.end_line > m.start_line)
            .map(|m| (m.start_line, m.end_line))
            .collect();
        methods.retain(|m| {
            // A method that falls entirely within another method's body is a false positive
            // (e.g. `saveToDb(request);` matched as a `;`-terminated "method").
            !concrete_ranges.iter().any(|&(outer_start, outer_end)| {
                outer_start < m.start_line && outer_end >= m.end_line
            })
        });

        // Populate class method_ids by matching method.class_name to class.name
        for c in &mut classes {
            for m in &methods {
                if m.class_name == c.name && m.package_or_module == c.package_or_module {
                    c.method_ids.push(m.method_id.clone());
                }
            }
        }

        // Update class end lines
        // Collect start lines before mutating to avoid borrow conflict
        let class_start_lines: Vec<usize> = classes.iter().map(|c| c.start_line).collect();
        let total_lines = source.lines().count();
        for c in &mut classes {
            // End line is the line before the next class, or the end of file
            let next_class_line = class_start_lines
                .iter()
                .filter(|&&l| l > c.start_line)
                .min()
                .copied()
                .unwrap_or(total_lines + 1);
            c.end_line = next_class_line.saturating_sub(1);
            if c.end_line == 0 {
                c.end_line = source.lines().count();
            }
        }

        Ok(ParseResult { methods, classes })
    }
}

/// Get the last part of a type (e.g., "java.util.List" -> "List")
fn get_last_type_part(ty: &str) -> String {
    ty.rsplit('.').next().unwrap_or(ty).to_string()
}

/// Find the matching closing brace for an opening brace.
fn find_matching_brace(source: &str, open_pos: usize) -> Option<&str> {
    let bytes = source.as_bytes();
    if open_pos >= bytes.len() || bytes[open_pos] != b'{' {
        return None;
    }
    let mut depth = 1u32;
    for (i, &b) in bytes.iter().enumerate().skip(open_pos + 1) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[open_pos..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_simple_class_and_method() {
        let parser = JavaParser;
        let src = r#"
package com.example;

/**
 * A test class.
 */
public class HelloWorld {
    public String greet(String name) {
        log.info("hello");
        return "Hello " + name;
    }
}
"#;
        let result = parser.parse(src, &PathBuf::from("HelloWorld.java"), "test").unwrap();

        assert!(result.classes.len() >= 1);
        assert_eq!(result.classes[0].name, "HelloWorld");
        assert_eq!(result.classes[0].kind, ClassKind::Class);

        assert!(result.methods.len() >= 1);
        assert_eq!(result.methods[0].name, "greet");
        // Direct method calls extracted: `info` from `log.info("hello")`
        assert!(result.methods[0].calls.contains(&"info".to_string()));
    }

    #[test]
    fn parses_interface() {
        let parser = JavaParser;
        let src = "public interface PaymentService {\n    void pay(String orderId);\n}";
        let result = parser.parse(src, &PathBuf::from("PaymentService.java"), "test").unwrap();
        assert!(result.classes.len() >= 1);
        assert_eq!(result.classes[0].kind, ClassKind::Interface);
    }

    #[test]
    fn parses_interface_methods() {
        let parser = JavaParser;
        let src = r#"
package com.example;

public interface DaoBase {
    int insert(E entity);
    int updateById(E entity);
    List<E> selectList(E entity);
}
"#;
        let result = parser.parse(src, &PathBuf::from("DaoBase.java"), "test").unwrap();
        assert!(result.classes.len() >= 1);
        assert_eq!(result.classes[0].kind, ClassKind::Interface);
        // interface methods should be parsed
        assert!(result.methods.len() >= 3, "Expected >= 3 methods, got {}", result.methods.len());
        assert!(result.methods.iter().any(|m| m.name == "insert"));
        assert!(result.methods.iter().any(|m| m.name == "selectList"));
        // class method_ids should be populated
        assert!(!result.classes[0].method_ids.is_empty(),
            "Class method_ids should be populated");
    }

    #[test]
    fn can_parse_true_for_java() {
        assert!(JavaParser.can_parse(&PathBuf::from("Foo.java")));
        assert!(!JavaParser.can_parse(&PathBuf::from("Foo.py")));
    }

    #[test]
    fn language_returns_java() {
        assert_eq!(JavaParser.language(), Language::Java);
    }

    #[test]
    fn find_matching_brace_balanced() {
        let src = "{ foo { bar } baz }";
        let result = find_matching_brace(src, 0);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "{ foo { bar } baz }");
    }

    #[test]
    fn find_matching_brace_unbalanced() {
        let src = "{ foo { bar";
        let result = find_matching_brace(src, 0);
        assert!(result.is_none());
    }
}
