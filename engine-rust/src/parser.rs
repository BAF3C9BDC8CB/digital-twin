use crate::models::{ClassBlock, EntityKind, MethodBlock};
use anyhow::Result;
use std::path::Path;
use regex::Regex;

pub struct Parser {
    parsers: Vec<tree_sitter::Parser>,
}

impl Parser {
    pub fn new() -> Result<Self> {
        let langs: [tree_sitter::Language; 7] = [
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_php::LANGUAGE_PHP.into(),
            tree_sitter_javascript::LANGUAGE.into(),
        ];
        let parsers = langs.into_iter().map(|lang| {
            let mut p = tree_sitter::Parser::new();
            p.set_language(&lang).expect("set language");
            p
        }).collect();
        Ok(Parser { parsers })
    }

    pub fn parse_file(&mut self, file_path: &str, project: &str, root: &str) -> Result<ParsedFile> {
        let ext = Path::new(file_path).extension().and_then(|e| e.to_str()).unwrap_or("");
        let (idx, lang_name) = match ext {
            "java" => (0, "java"),
            "ts" | "tsx" => (1, "typescript"),
            "py" => (2, "python"),
            "go" => (3, "go"),
            "rs" => (4, "rust"),
            "php" => (5, "php"),
            "js" | "jsx" | "mjs" | "cjs" => (6, "javascript"),
            _ => return Ok(ParsedFile::default()),
        };
        let parser = &mut self.parsers[idx];

        let content = std::fs::read_to_string(file_path).unwrap_or_default();
        let tree = match parser.parse(&content, None) { Some(t) => t, None => return Ok(ParsedFile::default()) };
        let rel_path = Path::new(file_path).strip_prefix(root).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| file_path.to_string());

        let mut methods = Vec::new();
        let mut classes = Vec::new();
        walk_node(&tree.root_node(), &content, project, &rel_path, lang_name, &mut methods, &mut classes, "");
        Ok(ParsedFile { methods, classes })
    }
}

#[derive(Default)]
pub struct ParsedFile { pub methods: Vec<MethodBlock>, pub classes: Vec<ClassBlock> }

fn walk_node(node: &tree_sitter::Node, content: &str, project: &str, path: &str, lang: &str,
    methods: &mut Vec<MethodBlock>, classes: &mut Vec<ClassBlock>, current_class: &str) {

    let is_method = matches!(node.kind(), "method_declaration" | "constructor_declaration"
        | "function_declaration" | "function_item" | "method_definition"
        | "function_definition" | "arrow_function");
    let is_class = matches!(node.kind(), "class_declaration" | "class_definition" | "interface_declaration"
        | "enum_declaration" | "enum_item" | "struct_item" | "trait_item"
        | "union_item" | "trait_declaration");

    if is_method {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = name_node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
            let sig = node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
            let start = node.start_position().row + 1;
            let end = node.end_position().row + 1;
            let source = content[node.start_byte()..node.end_byte()].to_string();
            let calls = extract_calls(&source);
            let mid = make_method_id(project, path, current_class, &name);
            let search_text = format!("Project: {}\nMethod: {}\nCalls: {}", project, name,
                calls.iter().take(10).map(|s|s.as_str()).collect::<Vec<_>>().join(","));

            methods.push(MethodBlock {
                method_id: mid, project: project.into(), file_path: path.into(),
                language: lang.into(), package_or_module: String::new(),
                class_name: current_class.to_string(), name, signature: sig,
                params: vec![], return_type: String::new(),
                source_code: source, search_text, summary: String::new(),
                start_line: start, end_line: end,
                comment: String::new(), calls,
            });
        }
    } else if is_class {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = name_node.utf8_text(content.as_bytes()).unwrap_or("").to_string();
            let kind = match node.kind() {
                "class_declaration" => EntityKind::Class,
                "interface_declaration" => EntityKind::Interface,
                "enum_declaration" | "enum_item" => EntityKind::Enum,
                "struct_item" => EntityKind::Struct,
                "trait_item" | "trait_declaration" => EntityKind::Trait,
                _ => EntityKind::Class,
            };
            let start = node.start_position().row + 1;
            let end = node.end_position().row + 1;
            let cid = make_class_id(project, path, &name);
            let search_text = format!("Project: {}\nClass: {}", project, name);
            classes.push(ClassBlock {
                class_id: cid, project: project.into(), file_path: path.into(),
                language: lang.into(), package_or_module: String::new(),
                name, kind, summary: String::new(), search_text,
                start_line: start, end_line: end,
                extends: vec![], implements: vec![], method_ids: vec![],
            });
        }
    }

    // Recurse into ALL children (named + anonymous)
    let class_for_children = if is_class {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(content.as_bytes()).ok())
            .unwrap_or("")
    } else {
        current_class
    };

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            walk_node(&child, content, project, path, lang, methods, classes, class_for_children);
        }
    }
}

fn make_method_id(project: &str, path: &str, class_name: &str, name: &str) -> String {
    crate::common::hash::sha1_hex(&format!("{}::{}::{}::{}", project, path, class_name, name))
}
fn make_class_id(project: &str, path: &str, name: &str) -> String {
    crate::common::hash::sha1_hex(&format!("{}::{}::class::{}", project, path, name))
}
fn extract_calls(source: &str) -> Vec<String> {
    let re = Regex::new(r"\b([a-zA-Z_]\w*)\s*\(").unwrap();
    let keywords = ["if","for","while","switch","return","throw","new","catch","try"];
    let mut calls: Vec<String> = re.captures_iter(source).filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|n| !keywords.contains(&n.as_str()) && n.len() >= 2)
        .collect();
    calls.sort(); calls.dedup(); calls.truncate(50); calls
}
