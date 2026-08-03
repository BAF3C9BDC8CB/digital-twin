//! PHP 解析器——基于正则抽取函数、方法与类。

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use regex::Regex;
use std::path::Path;

pub struct PhpParser;

fn extract_calls(body: &str) -> Vec<String> {
    let re = Regex::new(r"\b([a-zA-Z_]\w*)\s*\(").unwrap();
    let mut calls: Vec<String> = re
        .captures_iter(body)
        .filter_map(|c| {
            let name = c[1].to_string();
            if matches!(
                name.as_str(),
                "if" | "for"
                    | "foreach"
                    | "while"
                    | "return"
                    | "throw"
                    | "new"
                    | "echo"
                    | "print"
                    | "isset"
                    | "empty"
                    | "unset"
                    | "include"
                    | "require"
                    | "namespace"
                    | "use"
                    | "function"
                    | "class"
                    | "interface"
                    | "trait"
                    | "catch"
                    | "try"
                    | "finally"
                    | "switch"
                    | "case"
                    | "default"
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

fn extract_namespace(source: &str) -> String {
    let re = Regex::new(r"namespace\s+([\\\w]+)\s*;").unwrap();
    re.captures(source)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

impl ParseStrategy for PhpParser {
    fn language(&self) -> Language {
        Language::Php
    }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "php")
            .unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let ns = extract_namespace(source);
        let mut methods = Vec::new();
        let mut classes = Vec::new();

        let class_re =
            Regex::new(r"(?m)^\s*(abstract\s+)?(class|interface|trait)\s+(\w+)\s*[^{]*\{").unwrap();
        let func_re = Regex::new(r"(?m)^\s*(public\s+)?(private\s+)?(protected\s+)?(static\s+)?function\s+(\w+)\s*\(([^)]*)\)\s*[^{]*\{").unwrap();

        for caps in class_re.captures_iter(source) {
            let name = caps[3].to_string();
            let kind_str = caps[2].to_string();
            let kind = match kind_str.as_str() {
                "interface" => ClassKind::Interface,
                "trait" => ClassKind::Interface,
                _ => ClassKind::Class,
            };
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let class_id = make_class_id(project, &ns, &name);
            classes.push(ClassBlock {
                class_id,
                name,
                kind,
                file_path: file_path.clone(),
                package_or_module: ns.clone(),
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

        for caps in func_re.captures_iter(source) {
            let name = caps[5].to_string();
            let params = caps[6].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let current_class = classes
                .iter()
                .filter(|c| c.start_line <= start_line && c.end_line >= start_line)
                .map(|c| c.name.clone())
                .next()
                .unwrap_or_else(|| "_global_".to_string());
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
            let signature = format!("function {}({})", name, params);
            methods.push(MethodBlock {
                method_id,
                name,
                signature,
                params,
                return_type: "mixed".into(),
                class_name: current_class,
                file_path: file_path.clone(),
                package_or_module: ns.clone(),
                language: "php".into(),
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
    fn parses_class_and_method() {
        let src = "<?php\n\nclass Hello {\n  public function world(): string {\n    return 'hi';\n  }\n}\n";
        let result = PhpParser
            .parse(src, &PathBuf::from("Hello.php"), "test")
            .unwrap();
        assert!(result.classes.len() >= 1);
        assert!(result.methods.len() >= 1);
    }

    #[test]
    fn can_parse_php() {
        assert!(PhpParser.can_parse(&PathBuf::from("index.php")));
    }
}
