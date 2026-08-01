//! Python parser using tree-sitter AST.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

pub struct TsPythonParser;

impl TsPythonParser {
    fn collect_classes(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        classes: &mut Vec<ClassBlock>,
    ) {
        if node.kind() == "class_definition" {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = tree_sitter_utils::node_text(source, &name_node).to_string();
                let (start_line, end_line) = tree_sitter_utils::node_range(node);
                let class_id = make_class_id(project, module, &name);
                classes.push(ClassBlock {
                    class_id,
                    name,
                    kind: ClassKind::Class,
                    file_path: file_path.to_string(),
                    package_or_module: module.to_string(),
                    project: project.to_string(),
                    start_line,
                    end_line,
                    method_ids: Vec::new(),
                });
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_classes(source, &child, project, file_path, module, classes);
        }
    }

    fn collect_methods(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        class_name: &str,
    ) -> Vec<MethodBlock> {
        let mut methods = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "function_definition" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = tree_sitter_utils::node_text(source, &name_node).to_string();
                    let (start_line, end_line) = tree_sitter_utils::node_range(&child);
                    let params = Self::format_params(source, &child);
                    let return_type = Self::format_return_type(source, &child);
                    let sig_line = tree_sitter_utils::node_text(source, &child)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let comment = tree_sitter_utils::extract_comment(source, &child);
                    let body = Self::get_body(source, &child);
                    let calls = tree_sitter_utils::extract_calls_from_body(source, &child);
                    let method_id =
                        make_method_id(project, file_path, class_name, &name, start_line);
                    methods.push(MethodBlock {
                        method_id,
                        name,
                        signature: sig_line,
                        params,
                        return_type,
                        class_name: class_name.to_string(),
                        file_path: file_path.to_string(),
                        package_or_module: module.to_string(),
                        language: "python".to_string(),
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
        if let Some(ret) = node.child_by_field_name("return_type") {
            tree_sitter_utils::node_text(source, &ret).to_string()
        } else {
            "None".to_string()
        }
    }

    fn get_body(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(body) = node.child_by_field_name("body") {
            tree_sitter_utils::node_text(source, &body).to_string()
        } else {
            String::new()
        }
    }

    /// Build a MethodBlock from a module-level function_definition node directly.
    fn build_module_fn(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        methods: &mut Vec<MethodBlock>,
    ) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = tree_sitter_utils::node_text(source, &name_node).to_string();
            let (start_line, end_line) = tree_sitter_utils::node_range(node);
            let params = Self::format_params(source, node);
            let return_type = Self::format_return_type(source, node);
            let sig_line = tree_sitter_utils::node_text(source, node)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let comment = tree_sitter_utils::extract_comment(source, node);
            let body = Self::get_body(source, node);
            let calls = tree_sitter_utils::extract_calls_from_body(source, node);
            let method_id = make_method_id(project, file_path, "_module_", &name, start_line);
            methods.push(MethodBlock {
                method_id,
                name,
                signature: sig_line,
                params,
                return_type,
                class_name: "_module_".to_string(),
                file_path: file_path.to_string(),
                package_or_module: module.to_string(),
                language: "python".to_string(),
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

impl ParseStrategy for TsPythonParser {
    fn language(&self) -> Language {
        Language::Python
    }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "py")
            .unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let mut parser = Parser::new();
        let ts_lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        parser
            .set_language(&ts_lang)
            .map_err(|e| DtError::Repository(format!("tree-sitter python init: {e}")))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| DtError::Repository("tree-sitter parse returned None".into()))?;

        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = path
            .parent()
            .and_then(|p| p.to_str())
            .map(|s| s.replace(['/', '\\'], "."))
            .unwrap_or_default();
        let root = tree.root_node();

        // Collect classes
        let mut classes = Vec::new();
        {
            let mut cursor = root.walk();
            for child in root.children(&mut cursor) {
                Self::collect_classes(source, &child, project, &file_path, &module, &mut classes);
            }
        }

        // Collect methods from class bodies + top-level
        let mut methods = Vec::new();

        // Methods from class bodies
        for cls in &classes {
            // We need to re-find the class node in the tree
            let mut cursor = root.walk();
            find_class_methods(
                source,
                &root,
                &mut cursor,
                project,
                &file_path,
                &module,
                &cls.name,
                &mut methods,
            );
        }

        // Module-level (top-level) functions
        {
            let mut cursor = root.walk();
            for child in root.children(&mut cursor) {
                if child.kind() == "function_definition" {
                    Self::build_module_fn(
                        source,
                        &child,
                        project,
                        &file_path,
                        &module,
                        &mut methods,
                    );
                }
            }
        }

        // Populate class method_ids
        for c in &mut classes {
            for m in &methods {
                if m.class_name == c.name {
                    c.method_ids.push(m.method_id.clone());
                }
            }
        }

        Ok(ParseResult { methods, classes })
    }
}

fn find_class_methods(
    source: &str,
    _root: &tree_sitter::Node,
    cursor: &mut tree_sitter::TreeCursor,
    project: &str,
    file_path: &str,
    module: &str,
    class_name: &str,
    methods: &mut Vec<MethodBlock>,
) {
    loop {
        let node = cursor.node();
        if node.kind() == "class_definition" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if tree_sitter_utils::node_text(source, &name_node) == class_name {
                    if let Some(body) = node.child_by_field_name("body") {
                        let body_methods = TsPythonParser::collect_methods(
                            source, &body, project, file_path, module, class_name,
                        );
                        methods.extend(body_methods);
                    }
                    return;
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_payment_py() {
        let source =
            std::fs::read_to_string("/data/myProject/digital-twin-v2/test/project/payment.py")
                .expect("read payment.py");
        let parser = TsPythonParser;
        let result = parser
            .parse(&source, &PathBuf::from("payment.py"), "test-pipeline")
            .expect("parse");

        assert_eq!(result.methods.len(), 3, "Expected 3 methods");
        assert_eq!(result.classes.len(), 1, "Expected 1 class (PaymentGateway)");

        let pp = result
            .methods
            .iter()
            .find(|m| m.name == "process_payment")
            .unwrap();
        assert_eq!(pp.start_line, 9);
        assert_eq!(pp.end_line, 12);
        assert_eq!(pp.class_name, "PaymentGateway");

        let cp = result
            .methods
            .iter()
            .find(|m| m.name == "_call_provider")
            .unwrap();
        assert_eq!(cp.start_line, 15);
        assert_eq!(cp.end_line, 17);

        let rp = result
            .methods
            .iter()
            .find(|m| m.name == "refund_payment")
            .unwrap();
        assert_eq!(rp.start_line, 20);
        assert_eq!(rp.end_line, 22);
        assert_eq!(rp.class_name, "_module_");
    }
}
