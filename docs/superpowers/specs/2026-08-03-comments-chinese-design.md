# 设计文档：项目英文内容中文化（注释/日志/文档）

日期：2026-08-03
状态：已批准

## 1. 目标

将 digital-twin-v2 项目中所有英文内容转为中文：

- 源码注释（`//`、`///`、`//!`、`/* */`）
- 日志/错误/提示字符串（`tracing::info!/warn!/error!/debug!`、`anyhow!`、`bail!`、`println!`、`eprintln!` 等）
- 文档（CLAUDE.md 全文中译；README/docs/ 残留英文清理）

## 2. 现状盘点（已实测）

| 范围 | 规模 |
|---|---|
| src/ Rust 英文注释行 | ~2264 行（173 文件） |
| src/ 日志宏调用 | main.rs 64 处、CLI 文件 20-43 处等 |
| python/proto/mcp/scripts 英文注释 | ~82 行 |
| CLAUDE.md | 全英文 |
| docs/ 文档 | 已 99% 中文，仅零星英文残留 |
| README.md / AGENTS.md / config/*.yaml 描述 | 已中文 |

## 3. 翻译规范

1. 注释正文、文档正文、日志/错误消息全部转为中文，语义保真。
2. 保留以下内容原样：
   - 代码标识符、变量名、函数名、类型名、字段名
   - URL、路径、CLI 命令名、子命令名
   - 英文专有名词：Jenkins、K8s/Kubernetes、Nacos、Memgraph、MCP、Rust 等
   - License/版权法律文本
   - 注释内嵌的代码示例、配置片段
3. 日志字符串转为中文时，测试断言中依赖的英文文本同步改为中文断言。

## 4. 范围排除

- `target/`、`logs/`、`.git/`、`.weave/runtime/`（会话数据）
- `test/` fixtures 中的 JSON 数据文件、`Cargo.lock`
- `.claude/` 工具配置、`.idea/`

## 5. 实施方式（方案 A）

按模块分片委托 Shuttle 翻译，无文件重叠的片可并行，每片完成即验证：

1. `src/domain/` — 类型、trait、实体
2. `src/shared/` — 工具与公共组件
3. `src/interfaces/` — CLI 层（含大量日志字符串）
4. `src/application/` — 业务层（按子模块可再分片）
5. `src/infrastructure/` — 基础设施
6. `src/main.rs`、`src/lib.rs`、`src/proto.rs`
7. `python/`、`proto/`、`mcp/`、`scripts/`、`build.rs`
8. 文档：CLAUDE.md 中译 + docs/ 残留清理

## 6. 验证标准

- `cargo build` 通过
- `cargo clippy` 无新增告警
- `cargo test` 全绿（测试断言已同步中文化）
- 抽查翻译后注释与原文语义一致、无标识符被破坏

## 附录：测试断言清单

> **基线（Task 1 实测）**：`cargo check` 通过（仅既有 unused_variables 等 warning）；`cargo test --lib` = **675 passed / 2 failed**（`infrastructure::parser::ts_java::tests::parses_hello_service`、`interfaces::cli::backup_sqlite::tests::copy_database_writes_file`，均为既有失败，与本计划无关）。
>
> **扫描命令**：`rg -n 'expect\(|assert!\(|assert_eq!\(' tests/*.rs src/application/pipeline/test/`
> 覆盖文件：tests/extract_real_docs.rs、tests/s5_knowledge_search.rs、tests/unified_search.rs、src/application/pipeline/test/{runner.rs,cleanup.rs,report.rs,mod.rs}
>
> **分类定义**：
> - **内部提示语** — 测试断言失败时的诊断提示，仅面向开发者 → 可译为中文；
> - **功能性字符串** — 断言所依赖的程序输出、格式标记、文件名等 → 禁止翻译。
>
> 翻译时按 §3 规范保留消息中的代码标识符、CLI 命令名、专有名词、路径与 world 名（如 `dt`、`ifCode`、`Memgraph`、`config/prompts`、`knowledge`）。

### A. 内部提示语（可译为中文）

| 文件 | 消息 | 分类 | 处理 |
|---|---|---|---|
| tests/s5_knowledge_search.rs | "search must succeed"（×6：L111/186/245/288/326/360） | 内部提示语 | 可译为中文 |
| tests/s5_knowledge_search.rs | "knowledge world must return hits"（L115） | 内部提示语 | 可译为中文（保留 world 名 "knowledge"） |
| tests/unified_search.rs | "dt binary"（L10） | 内部提示语 | 可译为中文（保留命令名 "dt"） |
| tests/unified_search.rs | "dt search failed: {stderr}"（L20） | 内部提示语 | 可译为中文（保留命令名 "dt search"） |
| tests/unified_search.rs | "knowledge world empty"（L46） | 内部提示语 | 可译为中文（保留 world 名 "knowledge"） |
| tests/unified_search.rs | "ifCode not in top3: {top3:?}"（L52） | 内部提示语 | 可译为中文（保留标识符 "ifCode"） |
| tests/unified_search.rs | "memgraph connect"（L70） | 内部提示语 | 可译为中文（保留专名 "Memgraph"） |
| tests/unified_search.rs | "seed probe event"（L84） | 内部提示语 | 可译为中文 |
| tests/unified_search.rs | "seeded event not found: {hits:?}"（L91） | 内部提示语 | 可译为中文 |
| tests/unified_search.rs | "cleanup probe event"（L99） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "brief requires >= 5 real documents, found {}"（L84） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "LLM endpoint unreachable — start the model server first"（L96） | 内部提示语 | 可译为中文（保留专名 "LLM"） |
| tests/extract_real_docs.rs | "config/prompts must load"（L101） | 内部提示语 | 可译为中文（保留路径 "config/prompts"） |
| tests/extract_real_docs.rs | "chunk processor failed"（L122） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "llm processor failed"（L125） | 内部提示语 | 可译为中文（保留专名 "LLM"） |
| tests/extract_real_docs.rs | "graphs missing"（L127） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "graphs must deserialize"（L128） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "metric 1 failed: parse success {:.1}% < 90%"（L212） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "metric 2 failed: head/tail coverage {:.1}% < 95%"（L217） | 内部提示语 | 可译为中文 |

### B. 功能性字符串（禁止翻译）

| 文件 | 消息 | 分类 | 处理 |
|---|---|---|---|
| tests/unified_search.rs | "unrecognized subcommand"（L116，断言目标：clap 错误输出子串） | 功能性字符串 | 禁止翻译 |
| tests/unified_search.rs | "[Method] createApp"（L124，断言目标：stdout 输出格式标记） | 功能性字符串 | 禁止翻译 |
| tests/unified_search.rs | "app.js"（L36，断言目标：fixture 文件名） | 功能性字符串 | 禁止翻译 |
| tests/unified_search.rs | "app.js:L32-36"（L126，断言目标：文件:行号 输出格式） | 功能性字符串 | 禁止翻译 |

### C. 补充：panic! 断言失败提示（同类，超出 rg 扫描模式，翻译时一并处理）

| 文件 | 消息 | 分类 | 处理 |
|---|---|---|---|
| tests/unified_search.rs | "stdout is not pure JSON: {e}\n--- stdout ---\n{stdout}"（L22） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "cannot read {FIXTURE_DIR}: {e}"（L47） | 内部提示语 | 可译为中文 |
| tests/extract_real_docs.rs | "cannot read {}: {e}"（L119） | 内部提示语 | 可译为中文 |

### D. 无消息 / 无需处理

- **src/application/pipeline/test/report.rs**：全部 `assert_eq!`（L231-277）均无消息文本，无英文提示语可译。
- **src/application/pipeline/test/runner.rs / cleanup.rs / mod.rs**：无 `expect!`/`assert!`/`assert_eq!` 调用。
- **已为中文的断言消息**无需处理，未列入（如 s5_knowledge_search.rs 中 "ifCode 应被语义召回…"、"rerank 正常时不得打 rerank_unavailable: {:?}"、"doc 世界应返回证据块" 等）；其内部英文 token（`rerank_unavailable`、`dt://doc/`、`hop=1` 等）属功能性标识，按 §3 规范保留不译。
