# CLAUDE.md

本文件为 Claude Code（claude.ai/code）在本仓库中处理代码时提供指引。

## 构建与测试命令

```bash
# 构建项目
cargo build
cargo build --release

# 运行全部单元测试（源码内联的 #[cfg(test)] 模块）
cargo test

# 运行单个测试
cargo test test_name
cargo test <module>::<test_name>  # 例如：cargo test domain::id::tests::method_id_structure

# 运行指定模块中的测试
cargo test domain::id
cargo test application::build

# 集成测试（需要 Memgraph + Qdrant 运行中）
# 通过 `dt` CLI 二进制运行，而非 cargo test：
dt build --test        # BuildCommand 集成测试——构建 test-pipeline 项目，验证 KG+Qdrant
dt clean --test        # 删除所有 test- 前缀的数据

# Lint 检查
cargo clippy --all-targets

# 格式化
cargo fmt
```

## 核心架构

**单 crate DDD 分层架构**（`src/lib.rs` 为 crate 根）：

```
src/
  domain/          # 领域层：类型、traits、error、config、id（零内部依赖）
  infrastructure/  # 基础设施：Memgraph、Qdrant、SQLite、tree-sitter 解析器、scanner、embedder
  application/     # 应用层：build、sync、context、knowledge、plugins（编排）
  interfaces/      # 接口层：gRPC server、CLI 命令处理
  shared/          # 横切关注点：logging、coordinator、chunker、vectorizer
```

**六世界模型**——系统将数据划分为六个世界：

| 世界 | 数据 | 存储 |
|------|------|------|
| Reality | 代码、配置、K8s 资源 | Memgraph + Qdrant（`code_methods`/`config_chunks`） |
| Knowledge | 概念、模式、Playbook、经验 | Memgraph + Qdrant（`kg_nodes`，经 `dt kg-sync` 桥接） |
| Memory | 事件、会话、时间线 | 仅 Memgraph（关键词检索，无向量） |
| Semantic | 文档、API、日志模式向量 | Qdrant（`doc_chunks`） |
| Runtime | Pod 状态、服务运行态 | K8s API（实时查询） |
| Reasoning | 观察 → 分析 → 决策链路 | Memgraph（会话结束时标记 stale） |

**CLI 二进制**（`src/main.rs`）：`dt` 共 17 个命令（含 `dt sense` 环境感知），双模式：服务端（gRPC daemon）或 CLI 子命令。

## 管线引擎

将非结构化文件转换为结构化知识的处理器编排框架：

```
File → TreeSitterProcessor → ChunkProcessor → {HanlpClientProcessor → LlmClientProcessor} → StoreProcessor → KG+Qdrant
```

- **CPU 阶段**（优先级 ≥ 85）：tree_sitter（100）、chunk（90）——全并行执行
- **GPU 阶段**（优先级 < 85）：hanlp（80）、llm（60）——信号量限流并发
- 配置：`config/pipeline.yaml`
- 处理器：`src/application/pipeline/processors/`

## Qdrant 集合

全局集合，严格分离：
- `code_methods` ——代码搜索（方法级，来自 `dt build`；单个全局集合，`project` payload 字段用于过滤）
- `kg_nodes` ——知识图谱实体向量（来自 `dt kg-sync`）
- `doc_chunks` ——文档块向量（内部）
- `config_chunks` ——Nacos/配置块（world=config 搜索）

## CrossWorldSearch（`src/application/context/search_mcp.rs`）

统一搜索入口——CLI `dt search`、MCP `dt_search`/`dt_search_kg`、gRPC `Search` 背后的单一搜索栈，按 `world` 参数分派：
- `world=code` → Qdrant `code_methods`
- `world=knowledge` → 基于 `kg_nodes` 向量召回的 GraphRAG + Memgraph 图扩展 + rerank
- `world=doc` → Qdrant `doc_chunks`
- `world=config` → Qdrant `config_chunks`+`doc_chunks`（+ QueryRewriter），Cypher 关键词兜底
- `world=memory` → Memgraph 事件标签（关键词 CONTAINS，无向量）
- `world=all`（默认）→ 仅对 code+knowledge+doc 做 RRF 融合（config/memory 需显式查询）

合法的 world 取值：`all`、`code`、`knowledge`、`doc`、`config`、`memory`。没有 `reality` 取值——"Reality World" 是 code 世界的内部名称。

每条命中都携带 `llm_analysis`（方法用途/逻辑）与精确位置（`file_path`/`start_line`/`end_line`）。

## 外部依赖

- **Memgraph 5.x**（Bolt :7688）——知识图谱
- **Qdrant**（gRPC :6334）——向量存储
- **SiliconFlow API** ——embed（BGE-M3）、rerank、chat（Qwen2.5-14B）
- **tree-sitter** ——多语言 AST 解析（Java、Python、JS、TS、Go、Rust、PHP）

## 代码风格

- `rust-toolchain.toml`：stable 通道
- `rustfmt`：max_width=100、tab_spaces=4、edition=2021
- `clippy.toml`：cognitive-complexity-threshold=30、too-many-arguments-threshold=8
- 错误处理：应用层用 `anyhow`，领域错误用 `thiserror`（`DtError`）
- 异步：`tokio` + `async-trait` 用于异步 trait 方法
- 实体 ID：`dt://entity/{project}/...` URI 方案（见 `src/domain/id.rs`）

## 构建策略（`src/application/build/strategy/`）

- **Incremental**（默认）：与 SQLite 快照做 SHA1 差异比对——只处理变更的文件
- **FullRebuild**：清空全部数据后从头重建

## 测试基础设施

- **单元测试**：源码文件中的内联 `#[cfg(test)]` 模块，通过 `cargo test` 运行
- **集成测试**：`dt build --test` ——对真实 test-pipeline 项目运行，验证 Memgraph + Qdrant 输出与 `test/expected.json` 一致
- **测试运行器**：`src/application/pipeline/test/runner.rs` ——独立的 verify 函数
- **测试夹具**：`test/fixtures/`（Java、Python、Markdown、YAML）
- **测试项目**：`test/project/` ——用于集成测试的真实项目

## 多 Agent 团队系统

项目使用正式的多 Agent 团队流水线处理代码变更，每次变更都经过：

```
Change Request → Architect Guard → [Implementer + Tester] → Reviewer → Integrator → Done
```

### Agent 角色

| Agent | 文件 | 角色 |
|-------|------|------|
| **Architect** | `.claude/agents/architect.md` | DDD 分层边界守护——按分层规则检查 `use crate::*` 导入 |
| **Implementer** | `.claude/agents/implementer.md` | 代码实现——TDD、cargo fmt、cargo clippy |
| **Tester** | `.claude/agents/tester.md` | 测试编写——单元测试、边界情况、错误路径 |
| **Reviewer** | `.claude/agents/reviewer.md` | 代码审查——质量、安全、性能 |
| **Integrator** | `.claude/agents/integrator.md` | 集成——完整构建、测试套件、clippy、fmt 检查 |

### DDD 分层规则（由 Architect 强制执行）

| 层 | 可导入 | 禁止导入 |
|-------|----------------|---------------------|
| `src/domain/` | `crate::domain::*` | `infrastructure/`、`application/`、`interfaces/` |
| `src/infrastructure/` | `domain/`、`shared/` | `application/`、`interfaces/` |
| `src/application/` | `domain/`、`infrastructure/`、`shared/` | `interfaces/` |
| `src/interfaces/` | 所有层 | 无 |
| `src/shared/` | `domain/` | `infrastructure/`、`application/`、`interfaces/` |

**例外**：`src/main.rs`（组合根）可引用所有层。

### 工作流

- **`change-workflow`**：完整变更流水线——architect 守卫 → 实现 + 测试 → 审查 → 集成
- **`arch-guard-workflow`**：独立的架构检查（只读）
