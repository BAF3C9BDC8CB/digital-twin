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
