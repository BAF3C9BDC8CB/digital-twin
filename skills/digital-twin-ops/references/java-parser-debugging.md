# Java 解析器调试：TsJavaParser 优先于 JavaParser（2026-08-12 im-center 会话）

## 核心事实：.java 文件实际由 TsJavaParser 解析
`ParserRegistry`（src/infrastructure/parser/mod.rs）解析器顺序：7 个 tree-sitter 解析器在前（ts_java/ts_python/ts_javascript/ts_typescript/ts_go/ts_rust/ts_php），正则回退解析器（JavaParser 等）在后。因此 **.java 由 TsJavaParser（tree-sitter）解析**，JavaParser 只是回退。

→ 调试 Java 解析/注释问题必须看 `ts_java.rs` + `tree_sitter_utils.rs`，**不是 java.rs**！在 JavaParser 加 eprintln 调试永远不触发（0 输出），会误导排查方向。

## 注释错位 bug（已修复）
症状：KG 中多个无注释方法显示前一个方法的 javadoc（如 groupMsgGetSimple / sendGroupSystemNotification / sendGroupMsg 全显示"删除群成员消息"，其实只有 deleteGroupMsgBySender 有该注释）。

根因：`tree_sitter_utils.rs::extract_comment` 循环向前扫 `prev_sibling()`：
- 旧逻辑：`if kind.contains("comment")` 收集；`else if !comment_lines.is_empty()` 才 break → comment_lines 为空时遇到非注释节点（上一个方法节点）**不 break**，继续向前跨过它偷取其 javadoc
- 修复：遇到非注释节点**无条件 break**
- 原理：tree-sitter 中空白/空行不产生节点，方法的前兄弟要么是紧邻注释要么是前一个成员——跨过非注释节点即说明无注释

回归测试（tree_sitter_utils.rs 的 tests 模块，676 测试全过）：
- `comment_not_stolen_from_prev_method`：无注释方法不得偷取前方法的 javadoc
- `adjacent_comment_still_extracted`：紧邻注释仍正确提取（修复不破坏正常路径）

## 调试工作流（有效路径）
1. **增量构建不会触发 parser**：日志"增量跳过 342 个完全未变更文件, 0 个文件有步骤待执行"→ JavaParser 从未被调用，DBG 无效 → 必须 `--full` 强制全量。
2. **决定性证据 = 单测调真实 parser**：在 parser 文件 tests 模块写 Rust 单元测试，`std::fs::read_to_string` 读真实文件，直接调真实 `parse()`，打印 start_line/comment，与 Memgraph 数据对比——能区分"解析错了"vs"写入错了"。
3. **dt build 输出 daemon 化 + 并发日志交错**：后台进程 stdout 被 Hermes 捕获但 eprintln 可能不出现；多个并发 dt build 进程日志交错（日志里出现 `file=某.java` 但自己没传 --file，是残留进程的日志，别被误导）。
4. **Memgraph 验证**：`MATCH (m:Method {project:'X', name:'Y', start_line:N}) RETURN m.comment`；`count(m)` vs `count(DISTINCT m.method_id)` 判断是否重复节点（全量重建后应相等）。

## dt build CLI 坑
- `dt build <path>` 位置参数报错 `unexpected argument '<path>' found` → 必须 `--path <PATH>` 或 `--name <NAME>`
- `dt build ... --json` 不支持（`unexpected argument '--json' found`）
- 构建必须用 `--name im-center`（注册表名）；`--path` 会用目录名（uvp-im-center）作 project 名产生重复节点污染检索

## tree-sitter 测试写法坑
- `parser.set_language(&lang)` 需要引用（`&Language`）
- 收集节点的递归函数需生命周期标注：`fn collect_methods<'tree>(node: tree_sitter::Node<'tree>, out: &mut Vec<tree_sitter::Node<'tree>>)`
- Java grammar 获取：`let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();`
