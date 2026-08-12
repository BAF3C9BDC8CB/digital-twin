# TsJavaParser 注释提取 bug — 完整调试记录（2026-08-12）

## 症状

im-center 全量重建后，KG 中多个**无注释**方法显示相同错位注释：
- `GroupService.groupMsgGetSimple`(L204)、`sendGroupSystemNotification`(L271)、`sendGroupMsg`(L291)
  全部被标成 "删除群成员消息"（实为前一个方法 `deleteGroupMsgBySender` 的 javadoc）。
- 有正确 javadoc 的方法（groupMsgRecall、deleteGroupMsgBySender）注释正常。

## 根因

`src/infrastructure/parser/tree_sitter_utils.rs` 的 `extract_comment`：

```rust
let mut prev = node.prev_sibling();
while let Some(sib) = prev {
    let kind = sib.kind();
    if kind.contains("comment") {
        comment_lines.push(...);
    } else if !comment_lines.is_empty() {
        break;   // ← 旧逻辑：comment_lines 为空时不 break！
    }
    prev = sib.prev_sibling();
}
```

tree-sitter 中**空白/空行不产生节点**。方法声明的 prev_sibling 链：
`groupMsgGetSimple` → prev = `deleteGroupMsgBySender`（method_declaration 节点，非注释）
→ 旧逻辑 comment_lines 为空 → 不 break → 继续向前 → prev = deleteGroupMsgBySender 的 javadoc
→ **错误收集**。

## 修复

遇到非注释节点**无条件 break**：

```rust
} else {
    // 空白不产生节点：方法的前兄弟要么是紧邻注释，要么是前一个成员。
    break;
}
```

## ⚠️ 关键教训：Java 文件实际由 TsJavaParser 解析，不是 JavaParser！

`ParserRegistry::new()`（src/infrastructure/parser/mod.rs）中 **TsJavaParser（tree-sitter）优先**，
JavaParser（正则回退）排在其后。parse_file 用**第一个 can_parse 命中的解析器**。

调试 Java 解析问题：
- ✅ 看 `ts_java.rs`（collect_methods 调用 extract_comment）+ `tree_sitter_utils.rs`
- ❌ 不要只看 `java.rs`（正则回退解析器，正常构建根本不会走它）

## 调试路径复盘（为什么走了弯路）

1. 在 `java.rs` 的 `find_comment` 加 `eprintln!` DBG → **无输出**
2. 直接调用 `find_comment` 的独立 Rust 测试 → 返回空（正确！）
3. 结论：解析正确但 KG 有错位 → 怀疑写入路径/并发
4. 排查了 FullRebuildStrategy 清空、Consolidator、流水线 StoreProcessor…全排除
5. 用 `cargo test` 直接调用 `JavaParser::parse()` 解析真实文件 → 输出空注释（正确）
6. 此时才意识到：**构建根本没走 JavaParser**，而是 TsJavaParser
7. 在 `extract_comment` 找到 bug，修复后 2 个回归测试通过

**教训**：加 DBG 日志无效时，先确认代码路径真的被执行了（哪个解析器在跑），
再怀疑写入层。用 cargo test 直接调用真实解析器是最快的定位手段。

## 回归测试

`tree_sitter_utils::tests`：
- `comment_not_stolen_from_prev_method` — 无注释方法不偷前方法 javadoc
- `adjacent_comment_still_extracted` — 紧邻注释仍正确提取

测试注意：`tree_sitter::Language` 用 `tree_sitter_java::LANGUAGE.into()`；
`set_language` 需要 `&lang`；递归收集节点的函数要带 `'tree` 生命周期参数。

## 验证方法（构建后查 Memgraph）

```cypher
MATCH (m:Method {project:'im-center', file_path:'src/main/java/com/nextai/im/servie/GroupService.java'})
RETURN m.name, m.start_line, m.comment ORDER BY m.start_line
```
期望：groupMsgGetSimple/sendGroupSystemNotification/sendGroupMsg → comment=""，
deleteGroupMsgBySender/groupMsgRecall → 保留各自 javadoc。
