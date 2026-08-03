//! Rust 解析器——基于正则抽取函数、结构体、trait 与枚举。

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use regex::Regex;
use std::path::Path;

pub struct RustParser;

fn extract_calls(body: &str) -> Vec<String> {
    let re = Regex::new(r"\b([a-zA-Z_]\w*)\s*(::\s*[a-zA-Z_]\w*)?\s*\(").unwrap();
    let mut calls: Vec<String> = re
        .captures_iter(body)
        .filter_map(|c| {
            let name = c[1].to_string();
            if matches!(
                name.as_str(),
                "if" | "for"
                    | "while"
                    | "loop"
                    | "match"
                    | "return"
                    | "break"
                    | "continue"
                    | "let"
                    | "mut"
                    | "fn"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "impl"
                    | "pub"
                    | "use"
                    | "mod"
                    | "crate"
                    | "self"
                    | "super"
                    | "where"
                    | "unsafe"
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

fn extract_module(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.to_str())
        .map(|s| s.replace(['/', '\\'], "::"))
        .unwrap_or_default()
}

impl ParseStrategy for RustParser {
    fn language(&self) -> Language {
        Language::Rust
    }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "rs")
            .unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = extract_module(path);
        let mut methods = Vec::new();
        let mut classes = Vec::new();

        let struct_re =
            Regex::new(r"(?m)^\s*(pub(?:\s*\(\s*crate\s*\))?\s+)?struct\s+(\w+)\s*[^{;]*(?:\{|;)")
                .unwrap();
        let trait_re = Regex::new(r"(?m)^\s*(pub\s+)?trait\s+(\w+)\s*\{").unwrap();
        let enum_re = Regex::new(r"(?m)^\s*(pub\s+)?enum\s+(\w+)\s*\{").unwrap();
        let fn_re = Regex::new(r"(?m)^\s*(pub(?:\s*\(\s*crate\s*\))?\s+)?(async\s+)?fn\s+(\w+)\s*(<[^>]*>)?\s*\(([^)]*)\)\s*(->\s*[\w<>\[\]*\s+()]+)?\s*(where\s+[^{]+)?\s*\{").unwrap();

        // 将 struct/enum/trait 作为"类"抽取
        for caps in struct_re.captures_iter(source) {
            let name = caps[2].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let class_id = make_class_id(project, &module, &name);
            classes.push(ClassBlock {
                class_id,
                name,
                kind: ClassKind::Struct,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                project: project.to_string(),
                start_line,
                end_line: 0,
                method_ids: Vec::new(),
            });
        }
        for caps in trait_re.captures_iter(source) {
            let name = caps[2].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let class_id = make_class_id(project, &module, &name);
            classes.push(ClassBlock {
                class_id,
                name,
                kind: ClassKind::Trait,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                project: project.to_string(),
                start_line,
                end_line: 0,
                method_ids: Vec::new(),
            });
        }
        for caps in enum_re.captures_iter(source) {
            let name = caps[2].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let class_id = make_class_id(project, &module, &name);
            classes.push(ClassBlock {
                class_id,
                name,
                kind: ClassKind::Enum,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                project: project.to_string(),
                start_line,
                end_line: 0,
                method_ids: Vec::new(),
            });
        }

        let total = source.lines().count();
        let mut class_starts: Vec<usize> = classes.iter().map(|c| c.start_line).collect();
        class_starts.sort();
        for c in &mut classes {
            let next = class_starts
                .iter()
                .filter(|&&l| l > c.start_line)
                .min()
                .copied()
                .unwrap_or(total + 1);
            c.end_line = next.saturating_sub(1);
            if c.end_line == 0 {
                c.end_line = total;
            }
        }

        // 函数
        for caps in fn_re.captures_iter(source) {
            let name = caps[3].to_string();
            let params = caps
                .get(5)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let return_type = caps
                .get(6)
                .map(|m| m.as_str().trim().trim_start_matches("-> ").to_string())
                .unwrap_or_default();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let current_class = classes
                .iter()
                .filter(|c| c.start_line <= start_line && c.end_line >= start_line)
                .map(|c| c.name.clone())
                .next()
                .unwrap_or_else(|| "_module_".to_string());
            let brace_pos = caps.get(0).unwrap().end() - 1;
            let end_line_calc =
                start_line + crate::infrastructure::parser::find_brace_end_line(source, brace_pos);
            let body = source
                .lines()
                .skip(start_line - 1)
                .take(end_line_calc - start_line + 1)
                .collect::<Vec<_>>()
                .join("\n");
            let calls = extract_calls(&body);
            let method_id = make_method_id(project, &file_path, &current_class, &name, start_line);
            let signature = format!("fn {}({}) -> {}", name, params, return_type);
            methods.push(MethodBlock {
                method_id,
                name,
                signature,
                params,
                return_type,
                class_name: current_class,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                language: "rust".into(),
                project: project.to_string(),
                start_line,
                end_line: end_line_calc,
                calls,
                comment: String::new(),
                source_text: body,
            });
        }

        // 填充类的 method_ids
        for c in &mut classes {
            for m in &methods {
                if m.class_name == c.name && m.package_or_module == c.package_or_module {
                    c.method_ids.push(m.method_id.clone());
                }
            }
        }

        Ok(ParseResult { methods, classes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_fn_and_struct() {
        let src = "pub struct Bar;\n\nimpl Bar {\n  pub fn new() -> Self { Bar }\n}\n\nfn main() {\n  println!(\"hi\");\n}\n";
        let result = RustParser
            .parse(src, &PathBuf::from("main.rs"), "test")
            .unwrap();
        assert!(result.classes.len() >= 1);
        assert!(result.methods.len() >= 2);
    }

    #[test]
    fn can_parse_rs() {
        assert!(RustParser.can_parse(&PathBuf::from("main.rs")));
        assert!(!RustParser.can_parse(&PathBuf::from("main.go")));
    }
}
