//! Shared helpers for tree-sitter parser implementations.
//!
//! Provides utilities for navigating AST trees, extracting node ranges,
//! walking class/method hierarchies, and building MethodBlock/ClassBlock
//! from tree-sitter nodes.

use crate::domain::types::{ClassBlock, ClassKind, MethodBlock};

/// Extract 1-based start/end line from a tree-sitter node.
pub fn node_range(node: &tree_sitter::Node) -> (usize, usize) {
    let start = node.start_position().row + 1;
    let end = node.end_position().row + 1;
    (start, end)
}

/// Get the source text for a node range.
pub fn node_text<'a>(source: &'a str, node: &tree_sitter::Node) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    &source[start..end]
}

/// Extract comment/docstring associated with a node by walking
/// backwards through previous siblings looking for comment nodes.
pub fn extract_comment(source: &str, node: &tree_sitter::Node) -> String {
    let mut comment_lines: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(sib) = prev {
        let kind = sib.kind();
        if kind.contains("comment") {
            comment_lines.push(node_text(source, &sib).to_string());
        } else if !comment_lines.is_empty() {
            // Stop if we collected comments but hit a non-comment
            break;
        }
        prev = sib.prev_sibling();
    }
    comment_lines.reverse();
    comment_lines.join(" ").chars().take(200).collect()
}

/// Extract method/function call names from a body node by walking
/// the subtree for call expressions.
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

    // Skip nested function/method/class declarations
    if (kind.contains("function") || kind.contains("method") || kind.contains("class"))
        && kind != "call_expression"
        && kind != "method_invocation"
    {
        return;
    }

    // Extract call name
    if kind == "call_expression" || kind == "method_invocation" {
        if let Some(name) = get_call_name(source, node) {
            if !calls.contains(&name) {
                calls.push(name);
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(source, &child, calls);
    }
}

/// Extract the function/method name from a call expression node.
fn get_call_name(source: &str, node: &tree_sitter::Node) -> Option<String> {
    // Try field name "name" (Java method_invocation uses this)
    if let Some(name_node) = node.child_by_field_name("name") {
        let text = node_text(source, &name_node).to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    // Try field name "function" (JS/TS/Python call_expression uses this)
    if let Some(func) = node.child_by_field_name("function") {
        let fkind = func.kind();
        // Simple identifier: `foo()`
        if fkind == "identifier" || fkind == "name" {
            return Some(node_text(source, &func).to_string());
        }
        // Method call: `obj.foo()` or `obj.method()`
        if fkind == "field_expression" || fkind == "member_expression" {
            if let Some(prop) = func.child_by_field_name("property") {
                return Some(node_text(source, &prop).to_string());
            }
        }
        // Chained or complex expression — use text
        return Some(node_text(source, &func).to_string());
    }

    // Fallback: try first child as function name
    if let Some(first) = node.child(0) {
        let text = node_text(source, &first).to_string();
        if !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some(text);
        }
    }
    None
}
