# Tree-sitter Parser Replacement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace all 7 regex-based language parsers with real Tree-sitter AST parsers, providing accurate line numbers, reliable method/class extraction, and a foundation for adding more languages.

**Architecture:** Each language gets a new `ParseStrategy` implementation using `tree-sitter` with its language grammar. New parsers are registered in `ParserRegistry` alongside existing regex parsers, replacing them one by one. The `ParseResult` output format (MethodBlock/ClassBlock) stays unchanged, so the rest of the pipeline needs zero changes.

**Tech Stack:** `tree-sitter = "0.26"` + per-language grammar crates (`tree-sitter-java`, `tree-sitter-python`, etc.)

## Global Constraints

- All parsers must implement `ParseStrategy` trait (`fn language()`, `fn can_parse()`, `fn parse()`)
- Output type must be `Result<ParseResult, DtError>` with `MethodBlock`/`ClassBlock`
- Must handle Unicode identifiers (Chinese method names, etc.)
- Must handle nested generics, anonymous classes, string literals with braces
- Phase 2 LLM analysis depends on `source_text` field — must extract correct method body text
- Tests must pass after each parser replacement

---

### Task 0: Add tree-sitter dependencies

**Files:**
- Modify: `Cargo.toml`
- Modify: `build.rs`

**Interfaces:**
- Consumes: nothing (setup task)
- Produces: `tree_sitter::Parser` available to new parser implementations

- [ ] **Step 1: Add dependencies to Cargo.toml**

```toml
# In [dependencies] section:
tree-sitter = "0.25"
tree-sitter-java = "0.23"
tree-sitter-python = "0.25"
tree-sitter-javascript = "0.25"
tree-sitter-typescript = "0.23"
tree-sitter-go = "0.25"
tree-sitter-rust = "0.24"
tree-sitter-php = "0.24"
```

Note: version pins may need adjustment during implementation. The grammar crates (tree-sitter-java etc.) include their own C source + build.rs and export `language()` via `pub fn language() -> Language`.

- [ ] **Step 2: Verify compilation**

```bash
cargo check 2>&1 | grep "^error"
```

Expected: 0 errors (or only version-related warnings that need pin adjustment)

---

### Task 1: Create shared Tree-sitter helpers

**Files:**
- Create: `src/infrastructure/parser/tree_sitter_utils.rs`

**Interfaces:**
- Consumes: `tree_sitter::Tree`, `tree_sitter::Node`
- Produces: Shared helper functions for common AST operations

- [ ] **Step 1: Create the helper module**

```rust
//! Shared helpers for tree-sitter parser implementations.
//!
//! Provides utilities for navigating AST trees, extracting node ranges,
//! walking class/method hierarchies, and building MethodBlock/ClassBlock
//! from tree-sitter nodes.

use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock};

/// Extract start/end line (1-indexed) from a tree-sitter node.
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

/// Extract comment/docstring associated with a node.
pub fn extract_comment(source: &str, node: &tree_sitter::Node) -> String {
    // Walk previous siblings to find comment nodes
    let mut comment_lines: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(sib) = prev {
        if sib.kind().contains("comment") {
            comment_lines.push(node_text(source, &sib).to_string());
        } else if !sib.kind().contains("comment") && !comment_lines.is_empty() {
            break;
        }
        prev = sib.prev_sibling();
    }
    comment_lines.reverse();
    comment_lines.join(" ").chars().take(200).collect()
}

/// Extract method calls from a function body node.
pub fn extract_calls_from_body(source: &str, body: &tree_sitter::Node) -> Vec<String> {
    let mut calls = Vec::new();
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind().contains("call") || child.kind() == "call_expression" {
            // Try to get the function name from the call
            let name = get_call_name(source, &child);
            if let Some(n) = name {
                if !calls.contains(&n) {
                    calls.push(n);
                }
            }
        }
        // Recurse into sub-scopes (except nested functions)
        if child.child_count() > 0 && !child.kind().contains("function") && !child.kind().contains("method") {
            calls.extend(extract_calls_from_body(source, &child));
        }
    }
    calls.sort();
    calls.truncate(50);
    calls
}

/// Extract the method/function name from a call expression.
fn get_call_name(source: &str, node: &tree_sitter::Node) -> Option<String> {
    // call_expression: (function_name arguments)
    // For simple calls like `foo()`, function is the first child
    // For method calls like `obj.foo()`, function is field_expression
    if let Some(func) = node.child_by_field_name("function") {
        if func.kind() == "identifier" || func.kind() == "name" {
            return Some(node_text(source, &func).to_string());
        }
        // Handle method calls: obj.method() → get the method part
        if func.kind() == "field_expression" || func.kind() == "member_expression" {
            if let Some(prop) = func.child_by_field_name("property") {
                return Some(node_text(source, &prop).to_string());
            }
        }
    }
    None
}
```

- [ ] **Step 2: Register module in `mod.rs`**

Add to `src/infrastructure/parser/mod.rs`:
```rust
pub mod tree_sitter_utils;
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check 2>&1 | grep "^error"
```

Expected: 0 errors

---

### Task 2: Implement Java Tree-sitter parser

**Files:**
- Create: `src/infrastructure/parser/ts_java.rs`
- Modify: `src/infrastructure/parser/mod.rs` (register new parser)

**Interfaces:**
- Consumes: `tree_sitter_utils` helpers, `tree_sitter_java::language()`
- Produces: `ParseStrategy` impl for Java, registered in `ParserRegistry`

- [ ] **Step 1: Create the Java Tree-sitter parser**

```rust
//! Java parser using tree-sitter AST.
//!
//! Walks the CST to extract:
//! - Class/interface/enum declarations → ClassBlock
//! - Method/constructor declarations → MethodBlock
//! - Method calls within bodies → calls Vec
//! - Javadoc comments → comment String

use crate::domain::error::DtError;
use crate::domain::id::{make_class_id, make_method_id};
use crate::domain::traits::ParseStrategy;
use crate::domain::types::{ClassBlock, ClassKind, Language, MethodBlock, ParseResult};
use std::path::Path;
use tree_sitter::Parser;

use super::tree_sitter_utils::{extract_calls_from_body, node_range, node_text};

pub struct TsJavaParser;

impl TsJavaParser {
    fn parse_class(source: &str, node: &tree_sitter::Node, project: &str, file_path: &str, pkg: &str) -> Vec<ClassBlock> {
        let mut classes = Vec::new();
        if node.kind() == "class_declaration"
            || node.kind() == "interface_declaration"
            || node.kind() == "enum_declaration"
            || node.kind() == "record_declaration"
        {
            let name_node = node.child_by_field_name("name").unwrap();
            let name = node_text(source, &name_node).to_string();
            let (start_line, _) = node_range(node);
            let kind = match node.kind() {
                "interface_declaration" => ClassKind::Interface,
                "enum_declaration" => ClassKind::Enum,
                _ => ClassKind::Class,
            };
            let class_id = make_class_id(project, pkg, &name);
            classes.push(ClassBlock {
                class_id,
                name,
                kind,
                file_path: file_path.to_string(),
                package_or_module: pkg.to_string(),
                project: project.to_string(),
                start_line,
                end_line: 0, // Will be set below
                method_ids: Vec::new(),
            });
        }
        // Recursively walk children for nested classes
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            classes.extend(Self::parse_class(source, &child, project, file_path, pkg));
        }
        classes
    }

    fn parse_methods(
        source: &str,
        node: &tree_sitter::Node,
        project: &str,
        file_path: &str,
        pkg: &str,
        parent_class: &str,
    ) -> Vec<MethodBlock> {
        let mut methods = Vec::new();
        if node.kind() == "method_declaration"
            || node.kind() == "constructor_declaration"
        {
            let name_node = node.child_by_field_name("name").unwrap();
            let name = node_text(source, &name_node).to_string();
            let (start_line, end_line) = Self::method_range(source, node);
            let params = Self::format_params(source, node);
            let return_type = Self::format_return_type(source, node);
            let signature = Self::format_signature(source, node);
            let comment = super::tree_sitter_utils::extract_comment(source, node);
            let body = Self::get_body_text(source, node);
            let calls = extract_calls_from_body(source, &node);
            let method_id = make_method_id(project, file_path, parent_class, &name, start_line);

            methods.push(MethodBlock {
                method_id,
                name,
                signature,
                params,
                return_type,
                class_name: parent_class.to_string(),
                file_path: file_path.to_string(),
                package_or_module: pkg.to_string(),
                language: "java".to_string(),
                project: project.to_string(),
                start_line,
                end_line,
                calls,
                comment,
                source_text: body,
            });
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            methods.extend(Self::parse_methods(source, &child, project, file_path, pkg, parent_class));
        }
        methods
    }

    fn method_range(source: &str, node: &tree_sitter::Node) -> (usize, usize) {
        let start = node.start_position().row + 1;
        let end = node.end_position().row + 1;
        (start, end)
    }

    fn format_params(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(params) = node.child_by_field_name("parameters") {
            // Extract parameter text without parentheses
            let text = node_text(source, &params);
            if text.len() >= 2 {
                text[1..text.len()-1].to_string()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    }

    fn format_return_type(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(ret) = node.child_by_field_name("return_type") {
            node_text(source, &ret).to_string()
        } else if node.kind() == "constructor_declaration" {
            String::new()
        } else {
            "void".to_string()
        }
    }

    fn format_signature(source: &str, node: &tree_sitter::Node) -> String {
        let text = node_text(source, node);
        text.lines().next().unwrap_or("").trim().to_string()
    }

    fn get_body_text(source: &str, node: &tree_sitter::Node) -> String {
        if let Some(body) = node.child_by_field_name("body") {
            node_text(source, &body).to_string()
        } else {
            String::new()
        }
    }
}

impl ParseStrategy for TsJavaParser {
    fn language(&self) -> Language { Language::Java }
    fn can_parse(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).map(|e| e == "java").unwrap_or(false)
    }
    fn parse(&self, source: &str, path: &Path, project: &str) -> Result<ParseResult, DtError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_java::language())
            .map_err(|e| DtError::Repository(format!("tree-sitter java init: {e}")))?;

        let tree = parser.parse(source, None)
            .ok_or_else(|| DtError::Repository("tree-sitter parse failed".into()))?;

        let file_path = path.to_string_lossy().to_string().replace('\\', "/");
        let pkg = Self::extract_package(source);

        let root = tree.root_node();

        // Extract classes from the entire AST
        let mut classes = Vec::new();
        {
            let mut c = root.walk();
            for child in root.children(&mut c) {
                classes.extend(Self::parse_class(source, &child, project, &file_path, &pkg));
            }
        }

        // Update class end lines from the AST
        // Tree-sitter gives us the exact end position of each class declaration
        {
            let mut c = root.walk();
            for child in root.children(&mut c) {
                if child.kind() == "class_declaration"
                    || child.kind() == "interface_declaration"
                    || child.kind() == "enum_declaration"
                {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let class_name = node_text(source, &name_node);
                        if let Some(cls) = classes.iter_mut().find(|c| c.name == class_name) {
                            cls.end_line = child.end_position().row + 1;
                        }
                    }
                }
            }
        }

        // Extract methods from each class body
        let mut methods = Vec::new();
        {
            let mut c = root.walk();
            for child in root.children(&mut c) {
                if child.kind() == "class_declaration"
                    || child.kind() == "interface_declaration"
                {
                    let class_name = child.child_by_field_name("name")
                        .map(|n| node_text(source, &n).to_string())
                        .unwrap_or_default();
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut bc = body.walk();
                        for member in body.children(&mut bc) {
                            methods.extend(Self::parse_methods(source, &member, project, &file_path, &pkg, &class_name));
                        }
                    }
                }
            }
        }

        // Populate class method_ids
        for c in &mut classes {
            for m in &methods {
                if m.class_name == c.name && m.package_or_module == c.package_or_module {
                    c.method_ids.push(m.method_id.clone());
                }
            }
        }

        Ok(ParseResult { methods, classes })
    }
}

impl TsJavaParser {
    fn extract_package(source: &str) -> String {
        // Quick regex-free approach: find the package declaration
        let mut parser = Parser::new();
        if parser.set_language(&tree_sitter_java::language()).is_err() {
            return String::new();
        }
        if let Some(tree) = parser.parse(source, None) {
            let root = tree.root_node();
            let mut cursor = root.walk();
            for child in root.children(&mut cursor) {
                if child.kind() == "package_declaration" {
                    if let Some(name) = child.child_by_field_name("name") {
                        return node_text(source, &name).to_string();
                    }
                }
            }
        }
        String::new()
    }
}
```

- [ ] **Step 2: Register in `mod.rs`**

Add to `src/infrastructure/parser/mod.rs`:
```rust
pub mod ts_java;
```

Add `TsJavaParser` to `ParserRegistry::new()`:
```rust
Box::new(self::ts_java::TsJavaParser),
// Insert BEFORE existing JavaParser to take priority
```

- [ ] **Step 3: Write test for Java tree-sitter parser**

Add to `src/infrastructure/parser/ts_java.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_hello_service() {
        let source = std::fs::read_to_string(
            "/data/myProject/digital-twin-v2/test/project/HelloService.java"
        ).expect("read file");
        let parser = TsJavaParser;
        let result = parser.parse(&source, &PathBuf::from("HelloService.java"), "test-pipeline")
            .expect("parse");
        assert_eq!(result.methods.len(), 3, "Expected 3 methods (no phantom methods)");
        assert_eq!(result.classes.len(), 2, "Expected 2 classes");

        let create_order = result.methods.iter().find(|m| m.name == "createOrder").unwrap();
        assert_eq!(create_order.start_line, 11);
        assert_eq!(create_order.end_line, 15);
        assert!(create_order.calls.contains(&"saveToDb".to_string()));
        assert!(create_order.calls.contains(&"sendNotification".to_string()));
    }
}
```

- [ ] **Step 4: Build and run test**

```bash
cargo test --lib -- ts_java::tests --nocapture 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: test passes

---

### Task 3: Implement Python Tree-sitter parser

**Files:**
- Create: `src/infrastructure/parser/ts_python.rs`
- Modify: `src/infrastructure/parser/mod.rs`

**Interfaces:**
- Consumes: `tree_sitter_python::language()`
- Produces: `ParseStrategy` impl for Python

- [ ] **Step 1: Create `ts_python.rs`** (same pattern as Java but using Python AST node types)

Key Python AST node kinds:
- `class_definition` → ClassBlock
- `function_definition` → MethodBlock (check if inside class for class_name)
- `call` → extract calls (function name via `function` child)

- [ ] **Step 2: Register in `mod.rs`**

- [ ] **Step 3: Write test parsing `payment.py`**

```rust
assert_eq!(result.methods.len(), 3);
let pp = result.methods.iter().find(|m| m.name == "process_payment").unwrap();
assert_eq!(pp.start_line, 9);
assert_eq!(pp.end_line, 12);
```

- [ ] **Step 4: Build and test**

---

### Task 4: Implement JavaScript/TypeScript Tree-sitter parser

**Files:**
- Create: `src/infrastructure/parser/ts_javascript.rs`
- Create: `src/infrastructure/parser/ts_typescript.rs`
- Modify: `src/infrastructure/parser/mod.rs`

**Interfaces:**
- Consumes: `tree_sitter_javascript::language()`, `tree_sitter_typescript::language_tsx()`

**Note:** tree-sitter-typescript exports two language functions:
- `language()` for `.ts`
- `language_tsx()` for `.tsx`

Key JS AST node kinds:
- `class_declaration` → ClassBlock
- `method_definition` / `function_declaration` → MethodBlock
- `arrow_function` → MethodBlock (if assigned to variable)
- `call_expression` → extract calls

- [ ] **Step 1: Create `ts_javascript.rs`**

- [ ] **Step 2: Create `ts_typescript.rs`**

- [ ] **Step 3: Register both in `mod.rs`**

- [ ] **Step 4: Write tests for `app.js` and `utils.ts`**

- [ ] **Step 5: Build and test**

---

### Task 5: Implement Go/Rust/PHP Tree-sitter parsers

**Files:**
- Create: `src/infrastructure/parser/ts_go.rs`
- Create: `src/infrastructure/parser/ts_rust.rs`
- Create: `src/infrastructure/parser/ts_php.rs`
- Modify: `src/infrastructure/parser/mod.rs`

**Key Go AST node kinds:**
- `type_declaration` → check for `struct` → ClassBlock
- `function_declaration` / `method_declaration` → MethodBlock

**Key Rust AST node kinds:**
- `struct_item` → ClassBlock
- `function_item` → MethodBlock (check `visibility_modifier`)

**Key PHP AST node kinds:**
- `class_declaration` → ClassBlock
- `method_declaration` / `function_definition` → MethodBlock

- [ ] **Step 1: Create `ts_go.rs`**

- [ ] **Step 2: Create `ts_rust.rs`**

- [ ] **Step 3: Create `ts_php.rs`**

- [ ] **Step 4: Register all in `mod.rs`**

- [ ] **Step 5: Write tests and verify**

---

### Task 6: Register all new parsers in ParserRegistry (priority over regex)

**Files:**
- Modify: `src/infrastructure/parser/mod.rs`

**Changes:**
```rust
// In ParserRegistry::new(), add tree-sitter parsers BEFORE regex parsers:
pub fn new() -> Self {
    let mut parsers: Vec<Box<dyn ParseStrategy>> = vec![
        // Tree-sitter parsers (priority)
        Box::new(self::ts_java::TsJavaParser),
        Box::new(self::ts_python::TsPythonParser),
        Box::new(self::ts_javascript::TsJavaScriptParser),
        Box::new(self::ts_typescript::TsTypeScriptParser),
        Box::new(self::ts_go::TsGoParser),
        Box::new(self::ts_rust::TsRustParser),
        Box::new(self::ts_php::TsPhpParser),
        // Regex parsers (fallback)
        Box::new(self::java::JavaParser),
        Box::new(self::python::PythonParser),
        Box::new(self::javascript::JavaScriptParser),
        Box::new(self::typescript::TypeScriptParser),
        Box::new(self::go::GoParser),
        Box::new(self::rust_parser::RustParser),
        Box::new(self::php::PhpParser),
    ];
    // ...
}
```

This way tree-sitter parsers take priority. If they fail or don't parse correctly, the regex fallback is used.

- [ ] **Step 1: Update `ParserRegistry::new()`**

---

### Task 7: Integration test with `dt build --test`

- [ ] **Step 1: Full rebuild with `dt clean --test && dt build --test`**

```bash
cd /data/myProject/digital-twin-v2
dt clean --test
cargo run -- build --test 2>&1 | grep -E "Build complete|verify complete|passed="
```

Expected: 65 tests pass

- [ ] **Step 2: Verify line numbers in Qdrant**

```bash
curl -s -X POST http://localhost:6333/collections/test-pipeline_methods/points/scroll \
  -H 'Content-Type: application/json' \
  -d '{"limit": 30}' | python3 -c "..."  # verify all Lxxx match expected
```

- [ ] **Step 3: Verify search results**

```bash
cargo run -- search "createOrder" --world code --project test-pipeline --limit 5
```

Expected: correct line numbers

- [ ] **Step 4: Run existing unit tests**

```bash
cargo test --lib 2>&1 | grep "test result"
```

Expected: all tests pass

---

### Task 8: Update `expected.json` and clean up

- [ ] **Step 1: Update `expected.json` if any method counts changed**

Verify against actual parser output.

- [ ] **Step 2: (Optional) Remove old regex parsers**

Once all tree-sitter parsers are verified, remove the old regex `*.rs` files and their references from `mod.rs`.

- [ ] **Step 3: Run final `dt build --test`**

```bash
cd /data/myProject/digital-twin-v2 && dt clean --test && cargo run -- build --test
```

Expected: 65/65 passed
