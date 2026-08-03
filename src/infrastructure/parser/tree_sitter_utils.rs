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
        } else if !comment_lines.is_empty() {
            // 已收集到注释但遇到非注释节点，停止
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
