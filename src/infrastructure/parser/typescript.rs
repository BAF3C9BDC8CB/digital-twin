//! TypeScript/TSX parser — regex-based extraction of functions, methods, and classes.

use crate::domain::error::DtError;
use crate::domain::id::make_class_id;
use crate::domain::id::make_method_id;
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use regex::Regex;
use std::path::Path;

pub struct TypeScriptParser;

fn extract_calls(body: &str) -> Vec<String> {
    let re = Regex::new(r"\b([a-zA-Z_$][\w$]*)\s*\(").unwrap();
    let mut calls: Vec<String> = re
        .captures_iter(body)
        .filter_map(|c| {
            let name = c[1].to_string();
            if matches!(name.as_str(), "if" | "for" | "while" | "switch" | "return" | "throw"
                | "new" | "typeof" | "instanceof" | "catch" | "try" | "await" | "async"
                | "import" | "export" | "from" | "default" | "break") {
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
    path.parent().and_then(|p| p.to_str()).map(|s| s.replace(['/', '\\'], ".")).unwrap_or_default()
}

impl ParseStrategy for TypeScriptParser {
    fn language(&self) -> Language { Language::TypeScript }

    fn can_parse(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str())
            .map(|e| e == "ts" || e == "tsx").unwrap_or(false)
    }

    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = extract_module(path);
        let mut methods = Vec::new();
        let mut classes = Vec::new();

        // Class declarations
        let class_re = Regex::new(r"(?m)^\s*(export\s+)?(abstract\s+)?(class|interface|enum)\s+(\w+)\s*[^{]*\{").unwrap();
        // Function declarations (including methods)
        let func_re = Regex::new(r"(?m)^\s*(export\s+)?(async\s+)?function\s+(\w+)\s*\(([^)]*)\)\s*[:\w\s<>]*\s*\{").unwrap();
        // Arrow functions assigned to const/let
        let arrow_re = Regex::new(r"(?m)^\s*(export\s+)?(const|let|var)\s+(\w+)\s*=\s*(async\s+)?\([^)]*\)\s*[:\w\s<>]*\s*=>\s*\{").unwrap();
        // Method definitions inside class bodies
        let method_re = Regex::new(r"(?m)^\s*(public|private|protected|static|async|\s)*(\w+)\s*\(([^)]*)\)\s*[:\w\s<>]*\s*\{").unwrap();

        for caps in class_re.captures_iter(source) {
            let class_name = caps[4].to_string();
            let kind_str = caps[3].to_string();
            let kind = match kind_str.as_str() {
                "interface" => ClassKind::Interface,
                "enum" => ClassKind::Enum,
                _ => ClassKind::Class,
            };
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let class_id = make_class_id(project, &module, &class_name);
            classes.push(ClassBlock {
                class_id, name: class_name, kind,
                file_path: file_path.clone(), package_or_module: module.clone(),
                project: project.to_string(), start_line, end_line: 0, method_ids: Vec::new(),
            });
        }

        // Update class end lines
        let mut class_starts: Vec<usize> = classes.iter().map(|c| c.start_line).collect();
        class_starts.sort();
        let total = source.lines().count();
        for c in &mut classes {
            let next = class_starts.iter().filter(|&&l| l > c.start_line).min().copied().unwrap_or(total + 1);
            c.end_line = next.saturating_sub(1);
            if c.end_line == 0 { c.end_line = total; }
        }

        // Top-level functions
        for caps in func_re.captures_iter(source) {
            let name = caps[3].to_string();
            let params = caps[4].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let body = source.lines().skip(start_line - 1).take(20).collect::<Vec<_>>().join("\n");
            let calls = extract_calls(&body);
            let method_id = make_method_id(project, &file_path, "_module_", &name, start_line);
            methods.push(MethodBlock {
                method_id, name: name.clone(),
                signature: format!("function {}({})", name, params),
                params, return_type: "any".into(),
                class_name: "_module_".into(),
                file_path: file_path.clone(), package_or_module: module.clone(),
                language: "typescript".into(), project: project.to_string(),
                start_line, end_line: start_line + body.lines().count(),
                calls, comment: String::new(), source_text: body,
            });
        }

        // Arrow functions
        for caps in arrow_re.captures_iter(source) {
            let name = caps[3].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let body = source.lines().skip(start_line - 1).take(20).collect::<Vec<_>>().join("\n");
            let calls = extract_calls(&body);
            let method_id = make_method_id(project, &file_path, "_module_", &name, start_line);
            methods.push(MethodBlock {
                method_id, name: name.clone(),
                signature: format!("const {} = (...) => {{", name),
                params: "...".into(), return_type: "any".into(),
                class_name: "_module_".into(),
                file_path: file_path.clone(), package_or_module: module.clone(),
                language: "typescript".into(), project: project.to_string(),
                start_line, end_line: start_line + body.lines().count(),
                calls, comment: String::new(), source_text: body,
            });
        }

        // Class methods
        for caps in method_re.captures_iter(source) {
            let method_name = caps[2].to_string();
            let params = caps[3].to_string();
            if method_name == "class" || method_name == "interface" || method_name == "enum" || method_name == "export" { continue; }
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let current_class = classes.iter().filter(|c| c.start_line <= start_line && c.end_line >= start_line)
                .map(|c| c.name.clone()).next().unwrap_or_else(|| "_module_".to_string());
            let body = source.lines().skip(start_line - 1).take(20).collect::<Vec<_>>().join("\n");
            let calls = extract_calls(&body);
            let method_id = make_method_id(project, &file_path, &current_class, &method_name, start_line);
            methods.push(MethodBlock {
                method_id, name: method_name.clone(),
                signature: format!("{}({})", method_name, params),
                params, return_type: "any".into(),
                class_name: current_class,
                file_path: file_path.clone(), package_or_module: module.clone(),
                language: "typescript".into(), project: project.to_string(),
                start_line, end_line: start_line + body.lines().count(),
                calls, comment: String::new(), source_text: body,
            });
        }

        // Populate class method_ids
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
    fn parses_function_and_class() {
        let src = "export class App {\n  greet(name: string): string { return 'Hi ' + name; }\n}\n";
        let result = TypeScriptParser.parse(src, &PathBuf::from("App.ts"), "test").unwrap();
        assert!(result.classes.len() >= 1);
    }

    #[test]
    fn can_parse_ts() {
        assert!(TypeScriptParser.can_parse(&PathBuf::from("main.ts")));
        assert!(TypeScriptParser.can_parse(&PathBuf::from("main.tsx")));
        assert!(!TypeScriptParser.can_parse(&PathBuf::from("main.js")));
    }
}
