//! Java parser using tree-sitter AST.
//!
//! Walks the CST to extract class/interface/enum declarations and
//! method/constructor declarations with accurate line numbers.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

/// Java parser backed by tree-sitter AST.
pub struct TsJavaParser;

impl TsJavaParser {
    fn extract_package_from_tree<'a>(source: &'a str, root: &tree_sitter::Node) -> String {
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "package_declaration" {
                if let Some(name) = child.child_by_field_name("name") {
                    return tree_sitter_utils::node_text(source, &name).to_string();
                }
            }
        }
        String::new()
    }

    /// Recursively collect class/interface/enum declarations.
    fn collect_classes(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        pkg: &str,
        classes: &mut Vec<ClassBlock>,
    ) {
        let kind = node.kind();
        if kind == "class_declaration"
            || kind == "interface_declaration"
            || kind == "enum_declaration"
            || kind == "record_declaration"
        {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = tree_sitter_utils::node_text(source, &name_node).to_string();
                let (start_line, end_line) = tree_sitter_utils::node_range(node);
                let cls_kind = match kind {
                    "interface_declaration" => ClassKind::Interface,
                    "enum_declaration" => ClassKind::Enum,
                    _ => ClassKind::Class,
                };
                let class_id = make_class_id(project, pkg, &name);
                classes.push(ClassBlock {
                    class_id,
                    name,
                    kind: cls_kind,
                    file_path: file_path.to_string(),
                    package_or_module: pkg.to_string(),
                    project: project.to_string(),
                    start_line,
                    end_line,
                    method_ids: Vec::new(),
                });
            }
        }
        // Recurse into children for nested types
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_classes(source, &child, project, file_path, pkg, classes);
        }
    }

    /// Collect method declarations from a class/interface body.
    fn collect_methods(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        pkg: &str,
        class_name: &str,
    ) -> Vec<MethodBlock> {
        let mut methods = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "method_declaration" || child.kind() == "constructor_declaration" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = tree_sitter_utils::node_text(source, &name_node).to_string();
                    let (start_line, end_line) = tree_sitter_utils::node_range(&child);
                    let params = Self::format_params(source, &child);
                    let return_type = Self::format_return_type(source, &child);
                    let signature = Self::format_signature(source, &child);
                    let comment = tree_sitter_utils::extract_comment(source, &child);
                    let calls = if let Some(body_node) = child.child_by_field_name("body") {
                        tree_sitter_utils::extract_calls_from_body(source, &body_node)
                    } else {
                        Vec::new()
                    };
                    let body = Self::get_body(source, &child);
                    let method_id = make_method_id(project, file_path, class_name, &name, start_line);
                    methods.push(MethodBlock {
                        method_id,
                        name,
                        signature,
                        params,
                        return_type,
                        class_name: class_name.to_string(),
                        file_path: file_path.to_string(),
                        package_or_module: pkg.to_string(),
                        language: "java".to_string(),
                        project: project.to_string(),
                        start_line,
                        end_line,
                        calls,
                        comment,
                        source_text: body,
                    });
                }
            }
        }
        methods
    }

    fn format_params(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(params) = node.child_by_field_name("parameters") {
            let text = tree_sitter_utils::node_text(source, &params);
            if text.len() >= 2 {
                text[1..text.len() - 1].to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    }

    fn format_return_type(source: &str, node: &tree_sitter::Node) -> String {
        if node.kind() == "constructor_declaration" {
            return String::new();
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            tree_sitter_utils::node_text(source, &ret).to_string()
        } else {
            "void".to_string()
        }
    }

    fn format_signature(source: &str, node: &tree_sitter::Node) -> String {
        let text = tree_sitter_utils::node_text(source, node);
        text.lines().next().unwrap_or("").trim().to_string()
    }

    fn get_body(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(body) = node.child_by_field_name("body") {
            tree_sitter_utils::node_text(source, &body).to_string()
        } else {
            String::new()
        }
    }
}

impl ParseStrategy for TsJavaParser {
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
        let mut parser = Parser::new();
        let ts_lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
        parser
            .set_language(&ts_lang)
            .map_err(|e| DtError::Repository(format!("tree-sitter java init: {e}")))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| DtError::Repository("tree-sitter parse returned None".into()))?;

        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let root = tree.root_node();
        let pkg = Self::extract_package_from_tree(source, &root);

        // ---- Collect classes ----
        let mut classes = Vec::new();
        {
            let mut cursor = root.walk();
            for child in root.children(&mut cursor) {
                Self::collect_classes(source, &child, project, &file_path, &pkg, &mut classes);
            }
        }

        // ---- Collect methods from each class body ----
        let mut methods = Vec::new();
        {
            let mut cursor = root.walk();
            for child in root.children(&mut cursor) {
                let kind = child.kind();
                if kind == "class_declaration"
                    || kind == "interface_declaration"
                    || kind == "record_declaration"
                {
                    let class_name = child
                        .child_by_field_name("name")
                        .map(|n| tree_sitter_utils::node_text(source, &n).to_string())
                        .unwrap_or_default();
                    if let Some(body) = child.child_by_field_name("body") {
                        let body_methods =
                            Self::collect_methods(source, &body, project, &file_path, &pkg, &class_name);
                        methods.extend(body_methods);
                    }
                }
            }
        }

        // ---- Populate class method_ids ----
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
    fn parses_hello_service() {
        let source = std::fs::read_to_string(
            "/data/myProject/digital-twin-v2/test/project/HelloService.java",
        )
        .expect("read HelloService.java");
        let parser = TsJavaParser;
        let result = parser
            .parse(&source, &PathBuf::from("HelloService.java"), "test-pipeline")
            .expect("parse");
        // Should have 3 methods: createOrder, saveToDb, sendNotification (no phantoms)
        assert_eq!(
            result.methods.len(),
            3,
            "Expected 3 methods, got {}",
            result.methods.len()
        );
        // Should have 2 classes: HelloService, OrderRequest
        assert_eq!(result.classes.len(), 2, "Expected 2 classes");

        // Verify createOrder
        let co = result
            .methods
            .iter()
            .find(|m| m.name == "createOrder")
            .expect("createOrder method");
        assert_eq!(co.start_line, 11);
        assert_eq!(co.end_line, 15);
        eprintln!("createOrder calls: {:?}", co.calls);
        assert!(
            co.calls.contains(&"saveToDb".to_string()),
            "calls should contain saveToDb, got {:?}",
            co.calls
        );
        assert!(
            co.calls.contains(&"sendNotification".to_string()),
            "calls should contain sendNotification, got {:?}",
            co.calls
        );

        // Verify saveToDb
        let sd = result
            .methods
            .iter()
            .find(|m| m.name == "saveToDb")
            .expect("saveToDb method");
        assert_eq!(sd.start_line, 17);
        assert_eq!(sd.end_line, 19);

        // Verify sendNotification
        let sn = result
            .methods
            .iter()
            .find(|m| m.name == "sendNotification")
            .expect("sendNotification method");
        assert_eq!(sn.start_line, 21);
        assert_eq!(sn.end_line, 23);

        // Verify classes
        let hs = result.classes.iter().find(|c| c.name == "HelloService").unwrap();
        assert_eq!(hs.start_line, 6);
        let or = result.classes.iter().find(|c| c.name == "OrderRequest").unwrap();
        assert_eq!(or.start_line, 26);
    }

    #[test]
    fn can_parse_java_file() {
        assert!(TsJavaParser.can_parse(&PathBuf::from("Foo.java")));
        assert!(!TsJavaParser.can_parse(&PathBuf::from("Foo.py")));
    }

    #[test]
    fn language_is_java() {
        assert_eq!(TsJavaParser.language(), Language::Java);
    }
}
