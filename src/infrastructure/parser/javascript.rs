//! JavaScript 解析器——基于正则抽取函数、方法与类。
//! 使用与 TypeScript 类似的模式，但没有类型注解。

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use regex::Regex;
use std::path::Path;

pub struct JavaScriptParser;

fn extract_calls(body: &str) -> Vec<String> {
    let re = Regex::new(r"\b([a-zA-Z_$][\w$]*)\s*\(").unwrap();
    let mut calls: Vec<String> = re
        .captures_iter(body)
        .filter_map(|c| {
            let name = c[1].to_string();
            if matches!(
                name.as_str(),
                "if" | "for"
                    | "while"
                    | "switch"
                    | "return"
                    | "throw"
                    | "new"
                    | "typeof"
                    | "instanceof"
                    | "catch"
                    | "try"
                    | "await"
                    | "async"
                    | "import"
                    | "export"
                    | "from"
                    | "default"
                    | "break"
                    | "continue"
                    | "function"
                    | "class"
                    | "var"
                    | "let"
                    | "const"
                    | "require"
                    | "module"
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
        .map(|s| s.replace(['/', '\\'], "."))
        .unwrap_or_default()
}

impl ParseStrategy for JavaScriptParser {
    fn language(&self) -> Language {
        Language::JavaScript
    }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e, "js" | "jsx" | "mjs" | "cjs"))
            .unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = extract_module(path);
        let mut methods = Vec::new();
        let mut classes = Vec::new();

        let class_re =
            Regex::new(r"(?m)^\s*(export\s+)?class\s+(\w+)\s*(extends\s+\w+)?\s*\{").unwrap();
        let func_re =
            Regex::new(r"(?m)^\s*(export\s+)?(async\s+)?function\s+(\w+)\s*\(([^)]*)\)\s*\{")
                .unwrap();
        let arrow_re = Regex::new(
            r"(?m)^\s*(export\s+)?(const|let|var)\s+(\w+)\s*=\s*(async\s+)?\([^)]*\)\s*=>\s*\{",
        )
        .unwrap();
        let method_re =
            Regex::new(r"(?m)^\s*(static\s+)?(async\s+)?(\w+)\s*\(([^)]*)\)\s*\{").unwrap();

        for caps in class_re.captures_iter(source) {
            let name = caps[2].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
            let class_id = make_class_id(project, &module, &name);
            classes.push(ClassBlock {
                class_id,
                name,
                kind: ClassKind::Class,
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

        // 顶层函数
        for caps in func_re.captures_iter(source) {
            let name = caps[3].to_string();
            let params = caps[4].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
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
            let method_id = make_method_id(project, &file_path, "_module_", &name, start_line);
            methods.push(MethodBlock {
                method_id,
                name: name.clone(),
                signature: format!("function {}({})", name, params),
                params,
                return_type: "any".into(),
                class_name: "_module_".into(),
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                language: "javascript".into(),
                project: project.to_string(),
                start_line,
                end_line: end_line_calc,
                calls,
                comment: String::new(),
                source_text: body,
            });
        }

        // 箭头函数
        for caps in arrow_re.captures_iter(source) {
            let name = caps[3].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;
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
            let method_id = make_method_id(project, &file_path, "_module_", &name, start_line);
            methods.push(MethodBlock {
                method_id,
                name: name.clone(),
                signature: format!("const {} = (...) => {{", name),
                params: "...".into(),
                return_type: "any".into(),
                class_name: "_module_".into(),
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                language: "javascript".into(),
                project: project.to_string(),
                start_line,
                end_line: end_line_calc,
                calls,
                comment: String::new(),
                source_text: body,
            });
        }

        // 类方法
        for caps in method_re.captures_iter(source) {
            let method_name = caps[3].to_string();
            let params = caps[4].to_string();
            if method_name == "class" || method_name == "function" || method_name == "if" {
                continue;
            }
            let start_byte = caps.get(0).unwrap().start();
            let start_line = {
                let raw = source[..start_byte].lines().count() + 1;
                // 跳过空行：`(?m)^\s*(\w+)` 可能从空行开始匹配，
                // 因为 `\s*` 会贪婪地吞掉空行的换行符。
                let src_lines: Vec<&str> = source.lines().collect();
                let mut adj = raw;
                while adj <= src_lines.len() && src_lines[adj - 1].trim().is_empty() {
                    adj += 1;
                }
                adj
            };
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
            let method_id = make_method_id(
                project,
                &file_path,
                &current_class,
                &method_name,
                start_line,
            );
            methods.push(MethodBlock {
                method_id,
                name: method_name.clone(),
                signature: format!("{}({})", method_name, params),
                params,
                return_type: "any".into(),
                class_name: current_class,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                language: "javascript".into(),
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
    fn parses_function_and_class() {
        let src = "function hello() { return 'hi'; }\n\nclass Greeter {\n  greet(name) { return `Hi ${name}`; }\n}\n";
        let result = JavaScriptParser
            .parse(src, &PathBuf::from("app.js"), "test")
            .unwrap();
        assert!(result.classes.len() >= 1);
        assert!(result.methods.len() >= 2);
    }

    #[test]
    fn can_parse_js_variants() {
        assert!(JavaScriptParser.can_parse(&PathBuf::from("app.js")));
        assert!(JavaScriptParser.can_parse(&PathBuf::from("app.jsx")));
        assert!(JavaScriptParser.can_parse(&PathBuf::from("app.mjs")));
        assert!(JavaScriptParser.can_parse(&PathBuf::from("app.cjs")));
        assert!(!JavaScriptParser.can_parse(&PathBuf::from("app.ts")));
    }
}
