//! 使用 tree-sitter AST 的 TypeScript 解析器。

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils;

pub struct TsTypeScriptParser;

impl TsTypeScriptParser {
    fn collect_classes(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        classes: &mut Vec<ClassBlock>,
    ) {
        // 也在 export_statement 内部查找类/接口/枚举声明
        let target = if matches!(
            node.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration"
        ) {
            Some(*node)
        } else if node.kind() == "export_statement" {
            node.named_children(&mut node.walk()).find(|ch| {
                matches!(
                    ch.kind(),
                    "class_declaration" | "interface_declaration" | "enum_declaration"
                )
            })
        } else {
            None
        };
        if let Some(nn) = target.and_then(|n| n.child_by_field_name("name")) {
            let name = tree_sitter_utils::node_text(source, &nn).to_string();
            let (sl, el) = tree_sitter_utils::node_range(node);
            let kind = match node.kind() {
                "interface_declaration" => ClassKind::Interface,
                "enum_declaration" => ClassKind::Enum,
                _ => ClassKind::Class,
            };
            classes.push(ClassBlock {
                class_id: make_class_id(project, module, &name),
                name,
                kind,
                file_path: file_path.to_string(),
                package_or_module: module.to_string(),
                project: project.to_string(),
                start_line: sl,
                end_line: el,
                method_ids: Vec::new(),
            });
        }
        // 递归进入子节点（对 export_statement，这会找到内部声明）
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            Self::collect_classes(source, &ch, project, file_path, module, classes);
        }
    }

    fn build_method(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        class_name: &str,
        name: &str,
        sl: usize,
        el: usize,
        params: &str,
        sig: &str,
        body: &str,
        calls: Vec<String>,
    ) -> MethodBlock {
        MethodBlock {
            method_id: make_method_id(project, file_path, class_name, name, sl),
            name: name.to_string(),
            signature: sig.to_string(),
            params: params.to_string(),
            return_type: "any".into(),
            class_name: class_name.to_string(),
            file_path: file_path.to_string(),
            package_or_module: module.to_string(),
            language: "typescript".into(),
            project: project.to_string(),
            start_line: sl,
            end_line: el,
            calls,
            comment: String::new(),
            source_text: body.to_string(),
        }
    }

    /// 从可能是 function_declaration 或其包装的 export_statement 的节点中抽取函数。
    fn extract_fn(
        source: &str,
        node: &tree_sitter::Node,
    ) -> Option<(String, usize, usize, String, String, String, Vec<String>)> {
        let target = if node.kind() == "function_declaration" {
            Some(*node)
        } else if node.kind() == "export_statement" {
            node.named_children(&mut node.walk())
                .find(|ch| ch.kind() == "function_declaration")
        } else {
            None
        };
        target.and_then(|fn_node| {
            let nn = fn_node.child_by_field_name("name")?;
            let name = tree_sitter_utils::node_text(source, &nn).to_string();
            let (sl, el) = tree_sitter_utils::node_range(&fn_node);
            let params = Self::get_params(source, &fn_node);
            let sig = tree_sitter_utils::node_text(source, &fn_node)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let body = Self::get_body(source, &fn_node);
            let calls = tree_sitter_utils::extract_calls_from_body(source, &fn_node);
            Some((name, sl, el, params, sig, body, calls))
        })
    }
}

impl ParseStrategy for TsTypeScriptParser {
    fn language(&self) -> Language {
        Language::TypeScript
    }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e, "ts" | "tsx"))
            .unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let mut parser = Parser::new();
        // .tsx 文件使用 TSX 语言，.ts 文件使用 TypeScript
        let is_tsx = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "tsx")
            .unwrap_or(false);
        let lang: tree_sitter::Language = if is_tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        };
        parser
            .set_language(&lang)
            .map_err(|e| DtError::Repository(format!("ts ts init: {e}")))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| DtError::Repository("ts ts parse failed".into()))?;
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

        let mut methods = Vec::new();

        // 顶层函数（包括 export_statement 内部的）
        {
            let mut c = root.walk();
            for ch in root.children(&mut c) {
                // 处理直接或位于 export_statement 内部的 function_declaration
                if let Some((name, sl, el, params, sig, body, calls)) =
                    Self::extract_fn(source, &ch)
                {
                    methods.push(Self::build_method(
                        source, &ch, project, &file_path, &module, "_module_", &name, sl, el,
                        &params, &sig, &body, calls,
                    ));
                }

                // 箭头函数（包括 export_statement 内部的）
                if ch.kind() == "export_statement" {
                    let mut tmp_c = ch.walk();
                    let container = ch.children(&mut tmp_c).find(|c| {
                        matches!(c.kind(), "lexical_declaration" | "variable_declaration")
                    });
                    if let Some(container) = container {
                        let mut dc = container.walk();
                        for decl in container.children(&mut dc) {
                            if decl.kind() == "variable_declarator" {
                                Self::extract_arrow_fn(
                                    source,
                                    &decl,
                                    project,
                                    &file_path,
                                    &module,
                                    &mut methods,
                                );
                            }
                        }
                    }
                } else if ch.kind() == "lexical_declaration" || ch.kind() == "variable_declaration"
                {
                    let mut dc = ch.walk();
                    for decl in ch.children(&mut dc) {
                        if decl.kind() == "variable_declarator" {
                            Self::extract_arrow_fn(
                                source,
                                &decl,
                                project,
                                &file_path,
                                &module,
                                &mut methods,
                            );
                        }
                    }
                }
            }
        }

        // 类方法
        for cls in &classes {
            let mut c = root.walk();
            find_ts_class_methods(
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

impl TsTypeScriptParser {
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

    fn extract_arrow_fn(
        source: &str,
        decl: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        module: &str,
        methods: &mut Vec<MethodBlock>,
    ) {
        if let Some(init) = decl.child_by_field_name("value") {
            if init.kind().contains("arrow") || init.kind().contains("function") {
                if let Some(nn) = decl.child_by_field_name("name") {
                    let name = tree_sitter_utils::node_text(source, &nn).to_string();
                    let (sl, el) = tree_sitter_utils::node_range(&init);
                    let params = Self::get_params(source, &init);
                    let sig = format!("const {} = (...) => {{", name);
                    let body = Self::get_body(source, &init);
                    let calls = tree_sitter_utils::extract_calls_from_body(source, &init);
                    methods.push(Self::build_method(
                        source, &init, project, file_path, module, "_module_", &name, sl, el,
                        &params, &sig, &body, calls,
                    ));
                }
            }
        }
    }
}

fn find_ts_class_methods(
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
                                    let params = TsTypeScriptParser::get_params(source, &member);
                                    let sig = tree_sitter_utils::node_text(source, &member)
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                        .trim()
                                        .to_string();
                                    let body_text = TsTypeScriptParser::get_body(source, &member);
                                    let calls =
                                        tree_sitter_utils::extract_calls_from_body(source, &member);
                                    methods.push(TsTypeScriptParser::build_method(
                                        source, &member, project, file_path, module, class_name,
                                        &name, sl, el, &params, &sig, &body_text, calls,
                                    ));
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
