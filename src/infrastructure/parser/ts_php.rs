//! PHP parser using tree-sitter AST.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

pub struct TsPhpParser;

impl ParseStrategy for TsPhpParser {
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
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
        parser
            .set_language(&lang)
            .map_err(|e| DtError::Repository(format!("ts php init: {e}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| DtError::Repository("ts php parse failed".into()))?;
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let root = tree.root_node();

        let mut classes = Vec::new();
        let mut methods = Vec::new();

        // Collect classes
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                if matches!(
                    ch.kind(),
                    "class_declaration" | "interface_declaration" | "trait_declaration"
                ) {
                    if let Some(nn) = ch.child_by_field_name("name") {
                        let name = tree_sitter_utils::node_text(source, &nn).to_string();
                        let (sl, el) = tree_sitter_utils::node_range(&ch);
                        let kind = match ch.kind() {
                            "interface_declaration" | "trait_declaration" => ClassKind::Interface,
                            _ => ClassKind::Class,
                        };
                        let ns = Self::extract_namespace(&root, source);
                        classes.push(ClassBlock {
                            class_id: make_class_id(project, &ns, &name),
                            name,
                            kind,
                            file_path: file_path.clone(),
                            package_or_module: ns.clone(),
                            project: project.to_string(),
                            start_line: sl,
                            end_line: el,
                            method_ids: Vec::new(),
                        });
                    }
                }
            }
        }

        fn collect_methods_from(
            source: &str,
            node: &tree_sitter::Node,
            project: &str,
            file_path: &str,
            ns: &str,
            class_name: &str,
        ) -> Vec<MethodBlock> {
            let mut methods = Vec::new();
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                if ch.kind() == "method_declaration" {
                    if let Some(nn) = ch.child_by_field_name("name") {
                        let name = tree_sitter_utils::node_text(source, &nn).to_string();
                        let (sl, el) = tree_sitter_utils::node_range(&ch);
                        let params = ch
                            .child_by_field_name("parameters")
                            .map(|p| tree_sitter_utils::node_text(source, &p).to_string())
                            .unwrap_or_default();
                        let ret = "mixed".to_string();
                        let sig = tree_sitter_utils::node_text(source, &ch)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let body = ch
                            .child_by_field_name("body")
                            .map(|b| tree_sitter_utils::node_text(source, &b).to_string())
                            .unwrap_or_default();
                        let calls = tree_sitter_utils::extract_calls_from_body(source, &ch);
                        let params_clean = if params.len() >= 2 {
                            params[1..params.len() - 1].to_string()
                        } else {
                            String::new()
                        };
                        methods.push(MethodBlock {
                            method_id: make_method_id(project, file_path, class_name, &name, sl),
                            name,
                            signature: sig,
                            params: params_clean,
                            return_type: ret,
                            class_name: class_name.to_string(),
                            file_path: file_path.to_string(),
                            package_or_module: ns.to_string(),
                            language: "php".into(),
                            project: project.to_string(),
                            start_line: sl,
                            end_line: el,
                            calls,
                            comment: String::new(),
                            source_text: body,
                        });
                    }
                }
            }
            methods
        }

        // Module-level namespace
        let ns = Self::extract_namespace(&root, source);

        // Collect class methods + module-level functions
        for cls in &classes {
            let mut c = root.walk();
            let _ = find_php_class_methods(
                source,
                &root,
                &mut c,
                project,
                &file_path,
                &ns,
                &cls.name,
                &mut methods,
            );
        }

        // Module-level function declarations (not inside a class)
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                if ch.kind().contains("function_definition") && !is_inside_class(&root, &ch) {
                    if let Some(nn) = ch.child_by_field_name("name") {
                        let name = tree_sitter_utils::node_text(source, &nn).to_string();
                        let (sl, el) = tree_sitter_utils::node_range(&ch);
                        let params = ch
                            .child_by_field_name("parameters")
                            .map(|p| tree_sitter_utils::node_text(source, &p).to_string())
                            .unwrap_or_default();
                        let sig = tree_sitter_utils::node_text(source, &ch)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let body = ch
                            .child_by_field_name("body")
                            .map(|b| tree_sitter_utils::node_text(source, &b).to_string())
                            .unwrap_or_default();
                        let calls = tree_sitter_utils::extract_calls_from_body(source, &ch);
                        let params_clean = if params.len() >= 2 {
                            params[1..params.len() - 1].to_string()
                        } else {
                            String::new()
                        };
                        methods.push(MethodBlock {
                            method_id: make_method_id(project, &file_path, "_module_", &name, sl),
                            name,
                            signature: sig,
                            params: params_clean,
                            return_type: "mixed".into(),
                            class_name: "_module_".into(),
                            file_path: file_path.clone(),
                            package_or_module: ns.clone(),
                            language: "php".into(),
                            project: project.to_string(),
                            start_line: sl,
                            end_line: el,
                            calls,
                            comment: String::new(),
                            source_text: body,
                        });
                    }
                }
            }
        }

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

impl TsPhpParser {
    fn extract_namespace(root: &tree_sitter::Node, source: &str) -> String {
        let mut c = root.walk();
        for ch in root.children(&mut c) {
            if ch.kind() == "namespace_definition" || ch.kind() == "namespace_declaration" {
                if let Some(nn) = ch.child_by_field_name("name") {
                    return tree_sitter_utils::node_text(source, &nn).to_string();
                }
            }
        }
        String::new()
    }
}

fn is_inside_class(root: &tree_sitter::Node, node: &tree_sitter::Node) -> bool {
    let start = node.start_position().row;
    let mut c = root.walk();
    for ch in root.children(&mut c) {
        if matches!(
            ch.kind(),
            "class_declaration" | "interface_declaration" | "trait_declaration"
        ) {
            let cs = ch.start_position().row;
            let ce = ch.end_position().row;
            if start > cs && start <= ce {
                return true;
            }
        }
    }
    false
}

fn find_php_class_methods(
    source: &str,
    _root: &tree_sitter::Node,
    cursor: &mut tree_sitter::TreeCursor,
    project: &str,
    file_path: &str,
    ns: &str,
    class_name: &str,
    methods: &mut Vec<MethodBlock>,
) {
    loop {
        let node = cursor.node();
        if matches!(
            node.kind(),
            "class_declaration" | "interface_declaration" | "trait_declaration"
        ) {
            if let Some(nn) = node.child_by_field_name("name") {
                if tree_sitter_utils::node_text(source, &nn) == class_name {
                    if let Some(body) = node.child_by_field_name("body") {
                        let ms =
                            collect_methods_from(source, &body, project, file_path, ns, class_name);
                        methods.extend(ms);
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

fn collect_methods_from(
    source: &str,
    node: &tree_sitter::Node,
    project: &str,
    file_path: &str,
    ns: &str,
    class_name: &str,
) -> Vec<MethodBlock> {
    let mut methods = Vec::new();
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        if ch.kind() == "method_declaration" {
            if let Some(nn) = ch.child_by_field_name("name") {
                let name = tree_sitter_utils::node_text(source, &nn).to_string();
                let (sl, el) = tree_sitter_utils::node_range(&ch);
                let params = ch
                    .child_by_field_name("parameters")
                    .map(|p| tree_sitter_utils::node_text(source, &p).to_string())
                    .unwrap_or_default();
                let sig = tree_sitter_utils::node_text(source, &ch)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let body = ch
                    .child_by_field_name("body")
                    .map(|b| tree_sitter_utils::node_text(source, &b).to_string())
                    .unwrap_or_default();
                let calls = tree_sitter_utils::extract_calls_from_body(source, &ch);
                let params_clean = if params.len() >= 2 {
                    params[1..params.len() - 1].to_string()
                } else {
                    String::new()
                };
                methods.push(MethodBlock {
                    method_id: make_method_id(project, file_path, class_name, &name, sl),
                    name,
                    signature: sig,
                    params: params_clean,
                    return_type: "mixed".into(),
                    class_name: class_name.to_string(),
                    file_path: file_path.to_string(),
                    package_or_module: ns.to_string(),
                    language: "php".into(),
                    project: project.to_string(),
                    start_line: sl,
                    end_line: el,
                    calls,
                    comment: String::new(),
                    source_text: body,
                });
            }
        }
    }
    methods
}
