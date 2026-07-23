# Digital Twin V2

**AI 辅助开发的持久记忆层**。结合 Memgraph 知识图谱 + Qdrant 向量数据库，为 AI Agent 提供跨会话上下文。

## 架构

单 crate DDD 分层 (src/domain → src/infrastructure → src/application → src/interfaces)，六世界模型：

```
src/
  domain/          # 领域层: types, traits, error, config, id
  infrastructure/  # 基础设施: memgraph, qdrant, sqlite, parser, scanner, embedder
  application/     # 应用层: build, sync, context, knowledge, plugins
  interfaces/      # 接口层: gRPC server, CLI
  shared/          # 横切: logging, metrics, coordinator, chunker, vectorizer
```

六世界聚合上下文：

| 世界 | 数据 | 存储 |
|------|------|------|
| Reality | 代码、配置、K8s 资源 | Memgraph + Qdrant |
| Knowledge | 概念、模式、Playbook、经验 | Memgraph |
| Memory | 事件、会话、时间线 | Memgraph |
| Semantic | 文档、API、日志模式向量 | Qdrant |
| Runtime | Pod 状态、服务运行态 | K8s API 实时查询 |
| Reasoning | 观察 → 分析 → 决策链路 | Memgraph (含 TTL) |

## 快速开始

### 依赖

- Memgraph 5.x (Bolt :7687)
- Qdrant (gRPC :6334)
- dt-inference-server (Python, BGE-M3 + BGE-reranker + Qwen3-4B; gRPC :50051, REST :50052)

### 构建

```bash
cargo build --release
./target/release/dt --help
```

## CLI 命令 (26 个)

### 管线

| 命令 | 功能 |
|------|------|
| `dt build` | 构建项目索引到知识图谱 |
| `dt build --test` | 管线集成测试 — 对真实项目运行 BuildCommand，验证 KG+Qdrant |
| `dt update` | 单文件增量更新 |
| `dt watch` | 文件监视 daemon |
| `dt nacos-sync` | 同步 Nacos 配置 |
| `dt k8s-sync` | 同步 K8s 资源 |
| `dt kg-sync` | KG 节点同步到 Qdrant |

### 搜索

| `dt search` | 跨世界语义搜索 |

### 知识

| `dt memorize` | 写入知识节点 |
| `dt event` | 写入事件节点 |
| `dt learn` | AI 任务后沉淀知识 |
| `dt thread` | 管理 Digital Thread |

### 分析

| `dt context` | 六世界聚合上下文 |
| `dt plan` | 匹配 Playbook 生成执行计划 |
| `dt domain` | 领域知识模型子图 |
| `dt history` | 历史相似任务检索 |
| `dt dependency` | 调用链与依赖分析 |
| `dt verify` | 修改后一致性验证 |

### 运维

| `dt backup` | 分层备份 (Memgraph + Qdrant + SQLite) |
| `dt archive` | Memory 数据归档 |
| `dt clean` | 清空所有数据 |
| `dt clean --test` | 清理 `test-` 前缀的测试数据 |
| `dt cleanup` | TTL 数据清理 |
| `dt schema` | Schema 管理 (`dt schema init`) |
| `dt health` | 健康检查 |
| `dt metrics` | 指标查询 |

## 项目结构

```
digital-twin-v2/
├── README.md
├── Cargo.toml                  # 单 crate (dt-daemon)
├── config.yaml                 # 集中配置
├── config/
│   ├── pipeline.yaml           # Pipeline 引擎配置 (inference server URL, processor toggles)
│   └── prompts/                # LLM prompt 模板 (YAML)
│       ├── code_with_ast.yaml
│       ├── document_with_nlp.yaml
│       └── raw_text.yaml
├── src/
│   ├── main.rs                 # CLI 入口 (clap) + gRPC server 启动
│   ├── lib.rs
│   ├── domain/
│   │   ├── config.rs           # AppConfig + SecretString
│   │   ├── error.rs            # DomainError
│   │   ├── id.rs               # 统一 ID 格式 (dt://entity/...)
│   │   ├── traits.rs           # GraphRepository, VectorRepository, EmbedService, ...
│   │   └── types.rs            # 六世界实体类型定义
│   ├── infrastructure/
│   │   ├── embedder.rs         # dt-inference-server gRPC client
│   │   ├── memgraph/           # Memgraph Bolt 驱动
│   │   ├── parser/             # tree-sitter 多语言解析器
│   │   ├── qdrant/             # Qdrant gRPC 驱动
│   │   ├── scanner.rs          # 项目文件扫描
│   │   └── sqlite/             # SQLite 快照缓存
│   ├── application/
│   │   ├── build/              # dt build / update / watch
│   │   ├── context/            # Context Builder 管道 (6 world)
│   │   ├── knowledge/          # memorize / learn / event
│   │   ├── pipeline/           # Pipeline Engine (processor orchestration)
│   │   │   ├── engine.rs       # ProcessorEngine: analyze_file / analyze_batch
│   │   │   ├── registry.rs     # ProcessorRegistry + priority ordering
│   │   │   ├── processor.rs    # Processor trait
│   │   │   ├── config.rs       # PipelineConfig
│   │   │   ├── context.rs      # PipelineContext (共享状态)
│   │   │   ├── infer_client.rs # InferClient (HTTP → inference-server)
│   │   │   ├── prompt.rs       # PromptRegistry (YAML templates)
│   │   │   ├── output.rs       # 输出类型
│   │   │   ├── processors/
│   │   │   │   ├── tree_sitter.rs   # AST 解析
│   │   │   │   ├── chunk.rs         # 文档分块
│   │   │   │   ├── hanlp_client.rs  # HanLP NLP 标注
│   │   │   │   ├── llm_client.rs    # LLM 摘要/标签
│   │   │   │   └── store.rs         # 写入 Memgraph + Qdrant
│   │   │   └── test/
│   │   │       ├── runner.rs   # 跑管线集成测试
│   │   │       ├── cleanup.rs  # 清理 test- 前缀数据
│   │   │       └── report.rs   # 测试报告生成
│   │   ├── plugins/            # Plugin trait + registry
│   │   └── sync/               # nacos-sync / k8s-sync / kg-sync
│   ├── interfaces/
│   │   ├── cli/                # CLI 辅助模块 (cleanup, backup, archive)
│   │   └── grpc/               # gRPC server + services + auth
│   └── shared/
│       ├── chunker.rs          # 文档分块策略
│       ├── coordinator.rs      # WriteCoordinator 并发写入协调
│       ├── logging/            # 统一日志 (tracing + JSON)
│       └── vectorizer.rs       # 文本向量化工具
├── docs/                       # 架构设计文档 (10 份)
│   ├── architecture-v3-single-crate-layered.md
│   ├── architecture-v2-six-worlds.md
│   ├── architecture-v2-data-schema.md
│   ├── architecture-v2-data-pipeline.md
│   ├── architecture-v2-pipeline-impl.md
│   ├── architecture-v2-project-structure.md
│   ├── architecture-v2-mcp-api-spec.md
│   └── superpowers/specs/
│       ├── 2026-07-22-unstructured-data-pipeline-design.md
│       ├── 2026-07-22-inference-server-refactor-design.md
│       └── 2026-07-22-build-test-design.md
├── services/
│   └── inference-server/       # dt-inference-server (Python, BGE-M3 + BGE-reranker + Qwen3-4B)
├── test/
│   └── fixtures/               # 测试夹具
│       ├── java/OrderController.java
│       ├── python/payment.py
│       ├── markdown/architecture.md
│       └── yaml/config.yaml
└── .weave/plans/               # 实施路线图
    └── v2-implementation-roadmap.md
```

## 架构文档

详见 [docs/](docs/) 目录 (10 份架构设计文档)：

| 文档 | 内容 |
|------|------|
| [architecture-v3-single-crate-layered.md](docs/architecture-v3-single-crate-layered.md) | 当前架构: 单 crate DDD 五层 |
| [architecture-v2-six-worlds.md](docs/architecture-v2-six-worlds.md) | 六世界模型设计 |
| [architecture-v2-data-schema.md](docs/architecture-v2-data-schema.md) | Memgraph Schema: 25 约束 + 全文索引 |
| [architecture-v2-data-pipeline.md](docs/architecture-v2-data-pipeline.md) | 数据采集管线设计 |
| [architecture-v2-pipeline-impl.md](docs/architecture-v2-pipeline-impl.md) | 管道实现细节 |
| [architecture-v2-project-structure.md](docs/architecture-v2-project-structure.md) | 项目结构设计 |
| [architecture-v2-mcp-api-spec.md](docs/architecture-v2-mcp-api-spec.md) | 12 个 MCP 工具 API 规范 |
| [2026-07-22-inference-server-refactor-design.md](docs/superpowers/specs/2026-07-22-inference-server-refactor-design.md) | 推理服务重构: dt-embed → dt-inference-server |
| [2026-07-22-unstructured-data-pipeline-design.md](docs/superpowers/specs/2026-07-22-unstructured-data-pipeline-design.md) | Pipeline Engine: 非结构化数据处理器编排 |
| [2026-07-22-build-test-design.md](docs/superpowers/specs/2026-07-22-build-test-design.md) | 管线集成测试设计 |

## dt-inference-server

统一模型推理服务，合并旧 `dt-embed` (Python + BGE-M3) 为单一服务，新增 Rerank (BGE-reranker) 和 LLM Chat (Qwen3-4B) 能力。

**端口：**
- `:50051` — gRPC Embed 服务 (旧版兼容，Rust dt-daemon 直连)
- `:50052` — REST API (chat/rerank/embed/health/metrics/nlp)

**启动：**
```bash
cd services/inference-server/src
python3 server.py --port 50051 --llm-port 50052
```

**核心组件：**
- **TaskRouter** — 三级优先级队列 (HIGH/NORMAL/LOW)，LOW 优先级支持自动攒批 (64条 或 0.5s)
- **ModelRegistry** — 模型懒加载 + 空闲自动卸载，支持 `BGE-M3` / `BGE-reranker-large` / `Qwen3-4B`
- **gRPC endpoint** — 兼容旧 `dt-embed` 协议，`dt build` 无需修改即可使用

详见 [services/inference-server/README.md](services/inference-server/README.md)。

## Pipeline Engine

处理器编排框架，将非结构化文件 (代码/文档/文本) 通过多阶段处理管道转换为结构化知识。

**架构：**
```
File → TreeSitterProcessor → ChunkProcessor → HanlpClientProcessor → LlmClientProcessor → StoreProcessor → KG+Qdrant
```

**核心组件：**
- **Processor trait** — 通用处理器接口，支持 `priority()` 排序自动编排
- **ProcessorEngine** — `analyze_file()` / `analyze_batch()` 入口，CPU/GPU 阶段分离
- **InferClient** — HTTP 客户端连接 dt-inference-server (embed/rerank/chat)
- **PromptRegistry** — YAML 模板管理 (code_with_ast, document_with_nlp, raw_text)
- **PipelineConfig** — `config/pipeline.yaml` 配置

**内置处理器 (按优先级)：**

| 处理器 | 优先级 | 阶段 | 功能 |
|--------|--------|------|------|
| TreeSitterProcessor | 0 | CPU | tree-sitter AST 解析 |
| ChunkProcessor | 1 | CPU | 文档分块 + 边界检测 |
| HanlpClientProcessor | 2 | GPU | HanLP NLP 标注 (分词/词性/命名实体) |
| LlmClientProcessor | 3 | GPU | LLM 摘要生成 + 标签提取 |
| StoreProcessor | 4 | CPU | 结果写入 Memgraph + Qdrant |

**测试命令：**
- `dt build --test` — 对真实项目运行 BuildCommand，验证 KG+Qdrant 写入
- `dt clean --test` — 删除所有 `test-` 前缀的测试数据

## AI 集成 (OpenCode)

`dt` CLI 通过 gRPC 与 OpenCode MCP Server 通信，实现以下自动触发：

| 触发操作 | 自动执行 |
|---------|---------|
| 源码编辑 | `dt update --file <path>` |
| 软件安装 | `dt event --type SoftwareInstalled ...` |
| Nacos 配置变更 | `dt event --type ConfigChange ...` |
| 架构决策 | `dt memorize --type Decision ...` |
| 生产部署 | `dt event --type Deploy ...` |
| 会话结束 | `dt event --type Conversation ...` |

## 许可证

MIT
