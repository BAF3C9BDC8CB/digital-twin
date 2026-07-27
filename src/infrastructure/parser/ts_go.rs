//! Go parser using tree-sitter AST.

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

pub struct TsGoParser;

impl ParseStrategy for TsGoParser {
    fn language(&self) -> Language { Language::Go }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).map(|e| e == "go").unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
        parser.set_language(&lang).map_err(|e| DtError::Repository(format!("ts go init: {e}")))?;
        let tree = parser.parse(source, None).ok_or_else(|| DtError::Repository("ts go parse failed".into()))?;
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = path.parent().and_then(|p| p.to_str()).map(|s| s.replace(['/', '\\'], ".")).unwrap_or_default();
        let root = tree.root_node();

        let mut classes = Vec::new();
        let mut methods = Vec::new();

        // Collect struct types as classes
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                if ch.kind() == "type_declaration" {
                    let mut cc = ch.walk();
                    for td in ch.children(&mut cc) {
                        if td.kind() == "type_spec" {
                            if let Some(nn) = td.child_by_field_name("name") {
                                if let Some(ty) = td.child_by_field_name("type") {
                                    if ty.kind() == "struct_type" {
                                        let name = tree_sitter_utils::node_text(source, &nn).to_string();
                                        let (sl, el) = tree_sitter_utils::node_range(&td);
                                        classes.push(ClassBlock {
                                            class_id: make_class_id(project, &module, &name),
                                            name, kind: ClassKind::Struct,
                                            file_path: file_path.clone(), package_or_module: module.clone(),
                                            project: project.to_string(), start_line: sl, end_line: el, method_ids: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Collect functions and methods
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                if ch.kind() == "function_declaration" || ch.kind() == "method_declaration" {
                    let (sl, el) = tree_sitter_utils::node_range(&ch);
                    let name = ch.child_by_field_name("name")
                        .map(|n| tree_sitter_utils::node_text(source, &n).to_string())
                        .unwrap_or_default();
                    let params = ch.child_by_field_name("parameters")
                        .map(|p| tree_sitter_utils::node_text(source, &p).to_string())
                        .unwrap_or_default();
                    let ret = ch.child_by_field_name("result")
                        .map(|r| tree_sitter_utils::node_text(source, &r).to_string())
                        .unwrap_or_default();
                    let sig = tree_sitter_utils::node_text(source, &ch).lines()
                        .next().unwrap_or("").trim().to_string();
                    let body = ch.child_by_field_name("body")
                        .map(|b| tree_sitter_utils::node_text(source, &b).to_string())
                        .unwrap_or_default();
                    let calls = tree_sitter_utils::extract_calls_from_body(source, &ch);

                    // Find class name for methods
                    let class_name = if ch.kind() == "method_declaration" {
                        ch.child_by_field_name("receiver")
                            .and_then(|r| {
                                let text = tree_sitter_utils::node_text(source, &r);
                                // Extract type name after `(recv Type)` or `(recv *Type)`
                                let trimmed = text.trim().trim_start_matches('(').trim_end_matches(')');
                                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                                parts.last().map(|s| s.trim_start_matches('*').to_string())
                            })
                            .unwrap_or_else(|| "_module_".to_string())
                    } else {
                        // For regular functions (constructors), check if return type matches a struct
                        let ret_trimmed = ret.trim().trim_start_matches('*');
                        classes.iter()
                            .find(|c| c.name == ret_trimmed)
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "_module_".to_string())
                    };

                    let params_clean = if params.len() >= 2 { params[1..params.len()-1].to_string() } else { String::new() };

                    methods.push(MethodBlock {
                        method_id: make_method_id(project, &file_path, &class_name, &name, sl),
                        name, signature: sig, params: params_clean, return_type: ret,
                        class_name, file_path: file_path.clone(), package_or_module: module.clone(),
                        language: "go".into(), project: project.to_string(),
                        start_line: sl, end_line: el, calls, comment: String::new(), source_text: body,
                    });
                }
            }
        }

        for c in &mut classes {
            for m in &methods {
                if m.class_name == c.name { c.method_ids.push(m.method_id.clone()); }
            }
        }

        Ok(ParseResult { methods, classes })
    }
}
