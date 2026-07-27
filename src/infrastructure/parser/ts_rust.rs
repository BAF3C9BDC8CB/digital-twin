//! Rust parser using tree-sitter AST.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

pub struct TsRustParser;

impl TsRustParser {
    /// Recursively collect struct/enum/trait as classes from any level (handles nesting in mod).
    fn collect_classes_recursive(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        classes: &mut Vec<ClassBlock>,
    ) {
        if matches!(node.kind(), "struct_item" | "enum_item" | "trait_item") {
            if let Some(nn) = node.child_by_field_name("name") {
                let name = tree_sitter_utils::node_text(source, &nn).to_string();
                let (sl, el) = tree_sitter_utils::node_range(node);
                let kind = match node.kind() {
                    "trait_item" => ClassKind::Interface,
                    "enum_item" => ClassKind::Enum,
                    _ => ClassKind::Struct,
                };
                classes.push(ClassBlock {
                    class_id: make_class_id(project, module, &name),
                    name, kind, file_path: file_path.to_string(), package_or_module: module.to_string(),
                    project: project.to_string(), start_line: sl, end_line: el, method_ids: Vec::new(),
                });
            }
        }
        // Recurse into children to handle nested items (e.g. inside mod_item)
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            Self::collect_classes_recursive(source, &ch, project, file_path, module, classes);
        }
    }

    /// Walk up from a function_item to find the type name of the enclosing impl_item (if any).
    fn find_impl_type(source: &str, node: &tree_sitter::Node) -> Option<String> {
        let mut parent = node.parent();
        while let Some(p) = parent {
            if p.kind() == "impl_item" {
                // Extract the type from the impl_item's "type" field
                if let Some(ty) = p.child_by_field_name("type") {
                    let text = tree_sitter_utils::node_text(source, &ty);
                    // Handle generics like Result<T> — take the base name
                    let base = text.split('<').next().unwrap_or(text).trim().to_string();
                    if !base.is_empty() {
                        return Some(base);
                    }
                }
                return None;
            }
            parent = p.parent();
        }
        None
    }

    /// Recursively collect function_item nodes at any nesting level (handles impl_item, mod_item).
    fn collect_funcs_recursive(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        classes: &[ClassBlock],
        methods: &mut Vec<MethodBlock>,
    ) {
        if node.kind() == "function_item" {
            if let Some(nn) = node.child_by_field_name("name") {
                let name = tree_sitter_utils::node_text(source, &nn).to_string();
                let (sl, el) = tree_sitter_utils::node_range(node);
                let params = node.child_by_field_name("parameters")
                    .map(|p| tree_sitter_utils::node_text(source, &p).to_string())
                    .unwrap_or_default();
                let ret = node.child_by_field_name("return_type")
                    .map(|r| tree_sitter_utils::node_text(source, &r).to_string())
                    .unwrap_or_default();
                let sig = format!("fn {}({}) -> {}", name, 
                    if params.len() >= 2 { &params[1..params.len()-1] } else { "" }, ret);
                let body = node.child_by_field_name("body")
                    .map(|b| tree_sitter_utils::node_text(source, &b).to_string())
                    .unwrap_or_default();
                let calls = tree_sitter_utils::extract_calls_from_body(source, node);

                // Determine class: check for enclosing impl_item, then fall back to line range
                let class_name = Self::find_impl_type(source, node)
                    .and_then(|impl_type| {
                        classes.iter().find(|c| c.name == impl_type).map(|c| c.name.clone())
                    })
                    .or_else(|| {
                        classes.iter()
                            .find(|c| c.start_line <= sl && c.end_line >= el)
                            .map(|c| c.name.clone())
                    })
                    .unwrap_or_else(|| "_module_".to_string());

                let params_clean = if params.len() >= 2 { params[1..params.len()-1].to_string() } else { String::new() };

                methods.push(MethodBlock {
                    method_id: make_method_id(project, file_path, &class_name, &name, sl),
                    name, signature: sig, params: params_clean, return_type: ret,
                    class_name, file_path: file_path.to_string(), package_or_module: module.to_string(),
                    language: "rust".into(), project: project.to_string(),
                    start_line: sl, end_line: el, calls, comment: String::new(), source_text: body,
                });
            }
        }
        // Recurse into children to handle nested items
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            Self::collect_funcs_recursive(source, &ch, project, file_path, module, classes, methods);
        }
    }
}

impl ParseStrategy for TsRustParser {
    fn language(&self) -> Language { Language::Rust }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).map(|e| e == "rs").unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).map_err(|e| DtError::Repository(format!("ts rust init: {e}")))?;
        let tree = parser.parse(source, None).ok_or_else(|| DtError::Repository("ts rust parse failed".into()))?;
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = path.parent().and_then(|p| p.to_str()).map(|s| s.replace(['/', '\\'], ".")).unwrap_or_default();
        let root = tree.root_node();

        let mut classes = Vec::new();
        // Recursively collect classes (walks into mod_item etc.)
        Self::collect_classes_recursive(source, &root, project, &file_path, &module, &mut classes);

        let mut methods = Vec::new();
        // Recursively collect functions (walks into impl_item, mod_item etc.)
        Self::collect_funcs_recursive(source, &root, project, &file_path, &module, &classes, &mut methods);

        for c in &mut classes {
            for m in &methods {
                if m.class_name == c.name { c.method_ids.push(m.method_id.clone()); }
            }
        }

        Ok(ParseResult { methods, classes })
    }
}
