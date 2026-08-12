//! 使用 tree-sitter AST 的 JavaScript 解析器。

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

pub struct TsJavaScriptParser;

impl TsJavaScriptParser {
    fn collect_classes(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        classes: &mut Vec<ClassBlock>,
    ) {
        if node.kind() == "class_declaration" {
            if let Some(nn) = node.child_by_field_name("name") {
                let name = tree_sitter_utils::node_text(source, &nn).to_string();
                let (sl, el) = tree_sitter_utils::node_range(node);
                classes.push(ClassBlock {
                    class_id: make_class_id(project, module, &name),
                    name,
                    kind: ClassKind::Class,
                    file_path: file_path.to_string(),
                    package_or_module: module.to_string(),
                    project: project.to_string(),
                    start_line: sl,
                    end_line: el,
                    method_ids: Vec::new(),
                description: String::new(),
                });
            }
        }
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            Self::collect_classes(source, &ch, project, file_path, module, classes);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_methods(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
    ) -> Vec<MethodBlock> {
        let mut methods = Vec::new();
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            let kind = ch.kind();
            let is_method = kind == "method_definition"
                || kind == "function_declaration"
                || (kind == "arrow_function"
                    && ch
                        .parent()
                        .map(|p| p.kind() == "variable_declarator")
                        .unwrap_or(false));
            if is_method {
                if let Some(nn) = ch.child_by_field_name("name").or_else(|| {
                    // 赋给变量的箭头函数：名字在父节点上
                    ch.parent().and_then(|p| p.child_by_field_name("name"))
                }) {
                    let name = tree_sitter_utils::node_text(source, &nn).to_string();
                    let (sl, el) = tree_sitter_utils::node_range(&ch);
                    let params = Self::get_params(source, &ch);
                    let sig = tree_sitter_utils::node_text(source, &ch)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let body = Self::get_body(source, &ch);
                    let calls = tree_sitter_utils::extract_calls_from_body(source, &ch);
                    methods.push(MethodBlock {
                        method_id: make_method_id(project, file_path, module, &name, sl),
                        name,
                        signature: sig,
                        params,
                        return_type: "any".into(),
                        class_name: "_module_".into(),
                        file_path: file_path.to_string(),
                        package_or_module: module.to_string(),
                        language: "javascript".into(),
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

    fn get_params(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(p) = node.child_by_field_name("parameters") {
            let t = tree_sitter_utils::node_text(source, &p);
            if t.len() >= 2 {
                t[1..t.len() - 1].to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    }

    fn get_body(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(b) = node.child_by_field_name("body") {
            tree_sitter_utils::node_text(source, &b).to_string()
        } else {
            String::new()
        }
    }
}

impl ParseStrategy for TsJavaScriptParser {
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
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        parser
            .set_language(&lang)
            .map_err(|e| DtError::Repository(format!("ts js init: {e}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| DtError::Repository("ts js parse failed".into()))?;
        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let module = path
            .parent()
            .and_then(|p| p.to_str())
            .map(|s| s.replace(['/', '\\'], "."))
            .unwrap_or_default();
        let root = tree.root_node();

        let mut classes = Vec::new();
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                Self::collect_classes(source, &ch, project, &file_path, &module, &mut classes);
            }
        }

        // 收集类方法
        let mut methods = Vec::new();
        for cls in &classes {
            // 遍历树以查找类方法体
            let mut c = root.walk();
            find_class_methods_js(
                source,
                &root,
                &mut c,
                project,
                &file_path,
                &module,
                &cls.name,
                &mut methods,
            );
        }

        // 模块级函数
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                if ch.kind() == "function_declaration" {
                    if let Some(nn) = ch.child_by_field_name("name") {
                        let name = tree_sitter_utils::node_text(source, &nn).to_string();
                        let (sl, el) = tree_sitter_utils::node_range(&ch);
                        let params = Self::get_params(source, &ch);
                        let sig = tree_sitter_utils::node_text(source, &ch)
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let body = Self::get_body(source, &ch);
                        let calls = tree_sitter_utils::extract_calls_from_body(source, &ch);
                        methods.push(MethodBlock {
                            method_id: make_method_id(project, &file_path, "_module_", &name, sl),
                            name,
                            signature: sig,
                            params,
                            return_type: "any".into(),
                            class_name: "_module_".into(),
                            file_path: file_path.clone(),
                            package_or_module: module.clone(),
                            language: "javascript".into(),
                            project: project.to_string(),
                            start_line: sl,
                            end_line: el,
                            calls,
                            comment: String::new(),
                            source_text: body,
                        });
                    }
                }
                // 模块级箭头函数
                if ch.kind() == "lexical_declaration" || ch.kind() == "variable_declaration" {
                    let mut cc = ch.walk();
                    for decl in ch.children(&mut cc) {
                        if decl.kind() == "variable_declarator" {
                            if let Some(init) = decl.child_by_field_name("value") {
                                if init.kind() == "arrow_function" {
                                    if let Some(nn) = decl.child_by_field_name("name") {
                                        let name =
                                            tree_sitter_utils::node_text(source, &nn).to_string();
                                        let (sl, el) = tree_sitter_utils::node_range(&init);
                                        let params = Self::get_params(source, &init);
                                        let sig = format!("const {} = (...) => {{", name);
                                        let body = Self::get_body(source, &init);
                                        let calls = tree_sitter_utils::extract_calls_from_body(
                                            source, &init,
                                        );
                                        methods.push(MethodBlock {
                                            method_id: make_method_id(
                                                project, &file_path, "_module_", &name, sl,
                                            ),
                                            name,
                                            signature: sig,
                                            params,
                                            return_type: "any".into(),
                                            class_name: "_module_".into(),
                                            file_path: file_path.clone(),
                                            package_or_module: module.clone(),
                                            language: "javascript".into(),
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
                    }
                }
            }
        }

        // 填充类的 method_ids
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

fn find_class_methods_js(
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
        if node.kind() == "class_declaration" {
            if let Some(nn) = node.child_by_field_name("name") {
                if tree_sitter_utils::node_text(source, &nn) == class_name {
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut bc = body.walk();
                        for member in body.children(&mut bc) {
                            if member.kind() == "method_definition" {
                                if let Some(mn) = member.child_by_field_name("name") {
                                    let name =
                                        tree_sitter_utils::node_text(source, &mn).to_string();
                                    let (sl, el) = tree_sitter_utils::node_range(&member);
                                    let params = TsJavaScriptParser::get_params(source, &member);
                                    let sig = tree_sitter_utils::node_text(source, &member)
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                        .trim()
                                        .to_string();
                                    let body_text = TsJavaScriptParser::get_body(source, &member);
                                    let calls =
                                        tree_sitter_utils::extract_calls_from_body(source, &member);
                                    methods.push(MethodBlock {
                                        method_id: make_method_id(
                                            project, file_path, class_name, &name, sl,
                                        ),
                                        name,
                                        signature: sig,
                                        params,
                                        return_type: "any".into(),
                                        class_name: class_name.to_string(),
                                        file_path: file_path.to_string(),
                                        package_or_module: module.to_string(),
                                        language: "javascript".into(),
                                        project: project.to_string(),
                                        start_line: sl,
                                        end_line: el,
                                        calls,
                                        comment: String::new(),
                                        source_text: body_text,
                                    });
                                }
                            }
                        }
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
