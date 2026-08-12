//! tree-sitter 解析器实现的共享辅助函数。
//!
//! 提供遍历 AST 树、抽取节点区间、遍历类/方法层级，
//! 以及从 tree-sitter 节点构建 MethodBlock/ClassBlock 的工具。

use crate::domain::types::{ClassBlock, ClassKind, MethodBlock};

/// 从 tree-sitter 节点抽取 1 起始的起始/结束行。
pub fn node_range(node: &tree_sitter::Node) -> (usize, usize) {
    let start = node.start_position().row + 1;
    let end = node.end_position().row + 1;
    (start, end)
}

/// 获取节点区间的源码文本。
pub fn node_text<'a>(source: &'a str, node: &tree_sitter::Node) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    &source[start..end]
}

/// 通过向前遍历前驱兄弟节点查找注释节点，抽取与节点关联的
/// 注释/文档字符串。
pub fn extract_comment(source: &str, node: &tree_sitter::Node) -> String {
    let mut comment_lines: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(sib) = prev {
        let kind = sib.kind();
        if kind.contains("comment") {
            comment_lines.push(node_text(source, &sib).to_string());
        } else {
            // 遇到非注释节点立即停止。
            // 关键：tree-sitter 中空白/空行不产生节点，方法声明的
            // 前兄弟要么是紧邻的注释，要么是前一个成员（方法/字段）。
            // 旧逻辑在 comment_lines 为空时不 break，会跨过上一个
            // 方法节点继续向前，误取其 Javadoc 作为本方法注释。
            break;
        }
        prev = sib.prev_sibling();
    }
    comment_lines.reverse();
    comment_lines.join(" ").chars().take(200).collect()
}

/// 通过遍历子树中的调用表达式，从方法体节点抽取方法/函数调用名。
pub fn extract_calls_from_body(source: &str, node: &tree_sitter::Node) -> Vec<String> {
    let mut calls = Vec::new();
    collect_calls(source, node, &mut calls);
    calls.sort();
    calls.dedup();
    calls.truncate(50);
    calls
}

fn collect_calls(source: &str, node: &tree_sitter::Node, calls: &mut Vec<String>) {
    let kind = node.kind();

    // 跳过嵌套的函数/方法/类声明
    if (kind.contains("function") || kind.contains("method") || kind.contains("class"))
        && kind != "call_expression"
        && kind != "method_invocation"
    {
        return;
    }

    // 抽取调用名
    if kind == "call_expression" || kind == "method_invocation" {
        if let Some(name) = get_call_name(source, node) {
            if !calls.contains(&name) {
                calls.push(name);
            }
        }
    }

    // 递归进入子节点
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(source, &child, calls);
    }
}

/// 从调用表达式节点抽取函数/方法名。
fn get_call_name(source: &str, node: &tree_sitter::Node) -> Option<String> {
    // 尝试字段名 "name"（Java 的 method_invocation 使用此字段）
    if let Some(name_node) = node.child_by_field_name("name") {
        let text = node_text(source, &name_node).to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    // 尝试字段名 "function"（JS/TS/Python 的 call_expression 使用此字段）
    if let Some(func) = node.child_by_field_name("function") {
        let fkind = func.kind();
        // 简单标识符：`foo()`
        if fkind == "identifier" || fkind == "name" {
            return Some(node_text(source, &func).to_string());
        }
        // 方法调用：`obj.foo()` 或 `obj.method()`
        if fkind == "field_expression" || fkind == "member_expression" {
            if let Some(prop) = func.child_by_field_name("property") {
                return Some(node_text(source, &prop).to_string());
            }
        }
        // 链式或复杂表达式——使用文本
        return Some(node_text(source, &func).to_string());
    }

    // 回退：尝试将第一个子节点作为函数名
    if let Some(first) = node.child(0) {
        let text = node_text(source, &first).to_string();
        if !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_java(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn collect_methods<'tree>(
        node: tree_sitter::Node<'tree>,
        out: &mut Vec<tree_sitter::Node<'tree>>,
    ) {
        if node.kind() == "method_declaration" || node.kind() == "constructor_declaration" {
            out.push(node);
        }
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            collect_methods(ch, out);
        }
    }

    #[test]
    fn comment_not_stolen_from_prev_method() {
        let src = r#"public class Test {
    /**
     * 删除群成员消息
     */
    public void deleteGroupMsgBySender() {
    }

    public void groupMsgGetSimple() {
    }
}"#;
        let tree = parse_java(src);
        let mut methods = Vec::new();
        collect_methods(tree.root_node(), &mut methods);
        assert!(methods.len() >= 2, "methods={}", methods.len());

        let c1 = extract_comment(src, &methods[0]);
        assert!(c1.contains("删除群成员消息"), "method0 comment: {c1:?}");

        // 回归：无注释方法不得偷取前一个方法的 javadoc
        let c2 = extract_comment(src, &methods[1]);
        assert!(!c2.contains("删除群成员消息"), "method1 stolen: {c2:?}");
    }

    #[test]
    fn adjacent_comment_still_extracted() {
        let src = r#"public class Test {
    public void a() {
    }

    /**
     * 紧邻注释
     */
    public void b() {
    }
}"#;
        let tree = parse_java(src);
        let mut methods = Vec::new();
        collect_methods(tree.root_node(), &mut methods);
        assert!(methods.len() >= 2);
        let c2 = extract_comment(src, &methods[1]);
        assert!(c2.contains("紧邻注释"), "adjacent comment: {c2:?}");
    }

    #[test]
    fn class_level_javadoc_extracted() {
        // 类级 javadoc：extract_comment 对 class_declaration 节点应提取到类注释。
        let src = r#"/**
 * 群组管理服务，封装腾讯云 IM 群组操作
 */
public class GroupService {
    public void groupMsgRecall() {
    }
}"#;
        let tree = parse_java(src);
        let root = tree.root_node();
        // 找到 class_declaration 节点
        let mut class_node = None;
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "class_declaration" {
                class_node = Some(child);
                break;
            }
        }
        let cn = class_node.expect("class_declaration 节点应存在");
        let c = extract_comment(src, &cn);
        assert!(c.contains("群组管理服务"), "class comment: {c:?}");
    }

    #[test]
    fn class_without_comment_stays_empty() {
        // 标准 Java 文件：文件头注释在 package 之前，class 之前有
        // package/import 节点挡隔——extract_comment 遇非注释节点即停，
        // 不得跨过 package/import 偷取文件头注释。
        let src = r#"/**
 * 文件级版权注释
 */
package com.example;

import java.util.List;

public class PlainService {
    public void doWork() {
    }
}"#;
        let tree = parse_java(src);
        let mut cursor = tree.root_node().walk();
        let mut class_node = None;
        for child in tree.root_node().children(&mut cursor) {
            if child.kind() == "class_declaration" {
                class_node = Some(child);
                break;
            }
        }
        let cn = class_node.expect("class_declaration 节点应存在");
        let c = extract_comment(src, &cn);
        // 文件头注释在 package/import 之前，不应被偷取
        assert!(!c.contains("文件级版权注释"), "class comment stolen from file header: {c:?}");
    }
}
