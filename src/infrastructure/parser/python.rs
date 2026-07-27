//! Python parser — regex-based extraction of functions and classes.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use regex::Regex;
use std::path::Path;

pub struct PythonParser;

impl PythonParser {
    fn extract_module(path: &Path) -> String {
        path.parent()
            .and_then(|p| p.to_str())
            .map(|s| s.replace(['/', '\\'], "."))
            .unwrap_or_default()
    }

    fn extract_calls(body: &str) -> Vec<String> {
        let re = Regex::new(r"\b([a-zA-Z_]\w*)\s*\(").unwrap();
        let mut calls: Vec<String> = re
            .captures_iter(body)
            .filter_map(|c| {
                let name = c[1].to_string();
                if matches!(
                    name.as_str(),
                    "if" | "elif" | "for" | "while" | "with" | "return"
                        | "raise" | "print" | "isinstance" | "is" | "in"
                        | "not" | "and" | "or" | "def" | "class" | "import"
                        | "from" | "try" | "except"
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
}

impl ParseStrategy for PythonParser {
    fn language(&self) -> Language {
        Language::Python
    }

    fn can_parse(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).map(|e| e == "py").unwrap_or(false)
    }

    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = Self::extract_module(path);

        let mut methods = Vec::new();
        let mut classes = Vec::new();

        // Python class definition
        let class_re = Regex::new(r"(?m)^\s*class\s+(\w+)\s*(\([^)]*\))?\s*:").unwrap();
        // Python function/method definition
        let func_re = Regex::new(r"(?m)^\s*def\s+(\w+)\s*\(([^)]*)\)\s*(->\s*[\w\[\],\s]+)?\s*:").unwrap();

        // Find classes first
        for caps in class_re.captures_iter(source) {
            let class_name = caps[1].to_string();
            let start_byte = caps.get(0).unwrap().start();
            let start_line = source[..start_byte].lines().count() + 1;

            let class_id = make_class_id(project, &module, &class_name);
            classes.push(ClassBlock {
                class_id,
                name: class_name.clone(),
                kind: ClassKind::Class,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                project: project.to_string(),
                start_line,
                end_line: 0,
                method_ids: Vec::new(),
            });
        }

        // Update class end lines
        let mut class_end_lines: Vec<usize> = classes.iter().map(|c| c.start_line).collect();
        class_end_lines.sort();
        let total_lines = source.lines().count();

        for c in &mut classes {
            let next_line = class_end_lines
                .iter()
                .filter(|&&l| l > c.start_line)
                .min()
                .copied()
                .unwrap_or(total_lines + 1);
            c.end_line = next_line.saturating_sub(1);
            if c.end_line == 0 {
                c.end_line = total_lines;
            }
        }

        // Find functions
        for caps in func_re.captures_iter(source) {
            let func_name = caps[1].to_string();
            let params = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            let return_annotation = caps.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
            let return_type = return_annotation.trim_start_matches("-> ").trim().to_string();
            let ret_type = if return_type.is_empty() { "None" } else { &return_type };

            let full_match = caps.get(0).unwrap();
            let start_byte = full_match.start();
            let start_line = {
                let raw_line = source[..start_byte].lines().count() + 1;
                // Skip blank lines that `(?m)^\s*` ambiguously matched before the def
                let src_lines: Vec<&str> = source.lines().collect();
                let mut adjusted = raw_line;
                while adjusted <= src_lines.len() && src_lines[adjusted - 1].trim().is_empty() {
                    adjusted += 1;
                }
                adjusted
            };

            // Find which class contains this function
            let current_class = classes
                .iter()
                .filter(|c| c.start_line < start_line && c.end_line >= start_line)
                .map(|c| c.name.clone())
                .next()
                .unwrap_or_else(|| "_module_".to_string());

            let method_id = make_method_id(project, &file_path, &current_class, &func_name, start_line);

            // Extract body
            let indent = source.lines().nth(start_line - 1).map(|l| l.len() - l.trim_start().len()).unwrap_or(4) + 4;
            let body = extract_python_body(source, start_line - 1, indent);
            let calls = Self::extract_calls(&body);

            let signature = format!("def {}({}){} -> {}", func_name, params, return_annotation, ret_type);

            let end_line = start_line + body.chars().filter(|&c| c == '\n').count();

            methods.push(MethodBlock {
                method_id,
                name: func_name,
                signature,
                params: params.to_string(),
                return_type: ret_type.to_string(),
                class_name: current_class,
                file_path: file_path.clone(),
                package_or_module: module.clone(),
                language: "python".to_string(),
                project: project.to_string(),
                start_line,
                end_line,
                calls,
                comment: String::new(),
                source_text: body,
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

/// Extract body of a Python function given start line index (0-based) and expected indent.
fn extract_python_body(source: &str, def_line_idx: usize, indent: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut body_lines: Vec<String> = Vec::new();
    for line in lines.iter().skip(def_line_idx) {
        let leading = line.len().saturating_sub(line.trim_start().len());
        // Include the def line itself, empty lines, or indented lines
        if body_lines.is_empty()
            || line.trim().is_empty()
            || leading >= indent
        {
            body_lines.push(line.to_string());
        } else {
            break;
        }
    }
    // Trim trailing blank lines so end_line points to the last actual code/comment line
    while body_lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        body_lines.pop();
    }
    body_lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_function_and_class() {
        let src = "def hello():\n    pass\n\nclass Foo:\n    def method(self, x: int) -> str:\n        return str(x)\n";
        let result = PythonParser.parse(src, &PathBuf::from("test.py"), "test").unwrap();
        assert!(result.methods.len() >= 2);
        assert!(result.classes.len() >= 1);
        assert_eq!(result.classes[0].name, "Foo");
    }

    #[test]
    fn can_parse_py() {
        assert!(PythonParser.can_parse(&PathBuf::from("main.py")));
        assert!(!PythonParser.can_parse(&PathBuf::from("main.java")));
    }

    #[test]
    fn check_payment_line_numbers() {
        let source = std::fs::read_to_string(
            "/data/myProject/digital-twin-v2/test/project/payment.py"
        ).expect("read payment.py");
        let result = PythonParser.parse(&source, &PathBuf::from("payment.py"), "test-pipeline")
            .expect("parse");
        let methods: Vec<String> = result.methods.iter()
            .map(|m| format!("{} L{}-{}", m.name, m.start_line, m.end_line))
            .collect();
        println!("Python methods: {:?}", methods);
        assert_eq!(result.methods.len(), 3, "Expected 3 methods");
        let pm = &result.methods[0];
        assert_eq!(pm.name, "process_payment");
        assert_eq!(pm.start_line, 9, "process_payment should start at line 9");
        assert_eq!(pm.end_line, 12, "process_payment should end at line 12");
        let cm = &result.methods[1];
        assert_eq!(cm.name, "_call_provider");
        assert_eq!(cm.start_line, 15, "_call_provider should start at line 15");
        assert_eq!(cm.end_line, 17, "_call_provider should end at line 17");
        let rm = &result.methods[2];
        assert_eq!(rm.name, "refund_payment");
        assert_eq!(rm.start_line, 20, "refund_payment should start at line 20");
        assert_eq!(rm.end_line, 22, "refund_payment should end at line 22");
    }
}
