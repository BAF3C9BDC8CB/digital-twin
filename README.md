# Digital Twin V2

**AI 辅助开发的持久记忆层**。结合 Memgraph 知识图谱 + Qdrant 向量数据库，为 AI Agent 提供跨会话上下文。

## 架构

单 crate DDD 分层 (`src/domain/ → src/infrastructure/ → src/application/ → src/interfaces/`)，六世界模型：

```
src/
  domain/          # 领域层: types, traits, error, config, id (零内部依赖)
  infrastructure/  # 基础设施: memgraph, qdrant, sqlite, parser, scanner, embedder, hanlp
  application/     # 应用层: build, sync, context, knowledge, pipeline, hooks, plugins
  interfaces/      # 接口层: gRPC server, CLI command handlers
  shared/          # 横切: logging, coordinator, chunker, vectorizer, collections
```

六世界模型：

| 世界 | 数据 | 存储 |
|------|------|------|
| Reality | 代码、配置、K8s 资源 | Memgraph + Qdrant (`code_methods`/`config_chunks`) |
| Knowledge | 概念、模式、Playbook、经验 | Memgraph + Qdrant (`kg_nodes`，经 `dt kg-sync` 桥接) |
| Memory | 事件、会话、时间线 | Memgraph（事件标签，无向量，纯关键词检索） |
| Semantic | 文档、API、日志模式向量 | Qdrant (`doc_chunks`) |
| Runtime | Pod 状态、服务运行态 | K8s API 实时查询 |
| Reasoning | 观察 → 分析 → 决策链路 | Memgraph（session-end 时标记 stale） |

## 快速开始

### 依赖

- Memgraph 5.x (Bolt `:7688`，默认 `:7687`)
- Qdrant (gRPC `:6334`)
- SiliconFlow API (BGE-M3 embed + BGE-reranker + Qwen3.5 LLM)
- HanLP (可选，本地 NLP 服务 `:8765`)

### 构建

```bash
cargo build --release
./target/release/dt --help
```

配置从 `~/.config/digital-twin/config.yaml` 加载。

## CLI 命令 (17 个)

> **AI 助手使用时优先走 MCP 工具**（`mcp/mcp-server.py`，25 个工具，见文末「AI 集成」）；CLI 是降级/运维入口。

### 管线

| 命令 | 功能 |
|------|------|
| `dt build` | 构建项目索引到知识图谱（增量模式，基于 SQLite 哈希缓存） |
| `dt build --test` | 管线集成测试 — 对真实项目运行 BuildCommand，验证 KG+Qdrant |
| `dt build --file <path>` | 单文件增量更新 |
| `dt daemon` | 启动 gRPC daemon 服务端 |
| `dt nacos-sync` | 同步 Nacos 配置到知识图谱 |
| `dt k8s-sync` | 同步 K8s 资源到知识图谱 |
| `dt kg-sync` | KG 节点同步到 Qdrant 向量库 |
| `dt jc-sync` | 同步 Jenkins Views/Jobs/Builds 到知识图谱 |

### 搜索

| 命令 | 功能 |
|------|------|
| `dt sense` | 环境感知：项目定位 + 索引状态 + 目录内容简报（会话开始第一个动作） |
| `dt search` | 跨世界语义搜索（CrossWorldSearch 统一入口） |

### 知识

| 命令 | 功能 |
|------|------|
| `dt memorize` | 写入知识节点（Knowledge/Experience/Concept/Domain/Playbook） |
| `dt event` | 写入事件节点（Hook 驱动） |
| `dt learn` | AI 任务后沉淀结构化知识到 Knowledge World |

### 运维

| 命令 | 功能 |
|------|------|
| `dt backup` | 分层备份 (Memgraph + Qdrant + SQLite) |
| `dt clean` | 清空所有数据 |
| `dt schema init` | Schema 初始化（约束+索引） |
| `dt health` | 后端服务健康检查 |

### 插件

| 命令 | 功能 |
|------|------|
| `dt kub` | K8s 操作（pods/logs/download/status） |
| `dt jcli` | Jenkins CI/CD 操作 |

## 搜索架构

### CrossWorldSearch — 统一搜索入口

所有搜索走 `CrossWorldSearch` 服务（`src/application/context/search_mcp.rs`），CLI / MCP / gRPC 三入口共享同一栈，按 `world` 参数分派：

```
CLI dt search / MCP dt_search / gRPC Search → CrossWorldSearch.search()
    ├─ world=code      → Qdrant code_methods (全局集合)
    ├─ world=knowledge → Memgraph GraphRAG (向量召回+图扩展+rerank)
    ├─ world=doc       → Qdrant 文档块
    ├─ world=config    → Qdrant config_chunks (+ QueryRewriter)
    ├─ world=memory    → Memgraph 事件标签
    └─ world=all (默认) → code+knowledge+doc RRF 融合
```

每条命中自带 `llm_analysis`（方法用途/逻辑）与精确位置（`file_path`/`start_line`/`end_line`）。

### 三个 Qdrant 全局集合

| Collection | 数据来源 | 内容 | 搜索工具 |
|------------|---------|------|---------|
| `code_methods` | `dt build` 代码索引 | 方法级源码向量（含 start_line/end_line/calls/llm_analysis） | `dt_search` (world=code) / `dt search` |
| `doc_chunks` | 文档分块 | 文档段落向量 | `dt_search` (world=doc) |
| `kg_nodes` | `dt kg-sync` KG 节点同步 | 业务实体向量（Server/DB/NacosConfig/Knowledge/Decision 等） | `dt_search_kg` / `dt search --world knowledge` |

**关键：三者职责分离，不交叉。** 代码搜索走 `code_methods`，文档搜索走 `doc_chunks`，KG 搜索走 `kg_nodes`。

### 代码搜索流程

1. embed query → 向量
2. 搜索 `code_methods` 全局集合（可选按 `project` payload 过滤）
3. 读完整 payload（name/file_path/start_line/end_line/signature/calls/llm_analysis）
4. score < `DT_SEARCH_MIN_SCORE`（默认 0.3）过滤
5. 兜底：vector 不可用时用 Memgraph 全文索引 `db.index.fulltext.queryNodes("infra_search", ...)`

## 管线引擎

处理器编排框架，将非结构化文件（代码/文档/文本）通过多阶段处理管道转换为结构化知识。

**架构：**
```
File → TreeSitterProcessor → ChunkProcessor → HanlpClientProcessor → LlmClientProcessor → StoreProcessor → KG+Qdrant
```

**核心组件：**
- **Processor trait** — 通用处理器接口，支持 `priority()` 排序自动编排
- **ProcessorEngine** — `analyze_file()` / `analyze_batch()` 入口，CPU/GPU 阶段分离（阈值 85）
- **SiliconFlowChatClient** — HTTP 客户端连接 SiliconFlow API (chat/embed)
- **PromptRegistry** — YAML 模板管理 (code_with_ast, document_with_nlp, raw_text)
- **PipelineConfig** — `config/pipeline.yaml` 配置

**内置处理器（按优先级）：**

| 处理器 | 优先级 | 阶段 | 功能 |
|--------|--------|------|------|
| TreeSitterProcessor | 100 | CPU | tree-sitter AST 解析（7 种语言） |
| ChunkProcessor | 90 | CPU | 文档分块 + 边界检测 |
| HanlpClientProcessor | 80 | GPU | HanLP NLP 标注 (NER/关键词/摘要) |
| LlmClientProcessor | 60 | GPU | LLM 摘要生成 + 标签提取 |
| StoreProcessor | 10 | CPU | 结果写入 Memgraph + Qdrant (始终运行) |

**测试命令：**
- `dt build --test` — 对真实项目运行 BuildCommand，验证 KG+Qdrant 输出与 `test/expected.json` 一致
- `dt clean --test` — 删除所有 `test-` 前缀的测试数据

## Build 策略

`src/application/build/strategy/` 下两种策略：

- **IncrementalStrategy**（默认）：SHA-256 差异比较，基于 SQLite 快照，仅处理变更文件
- **FullRebuildStrategy**：清除全部项目数据后完全重建

# SiliconFlow API 集成

所有模型推理（embed/rerank/chat）通过 SiliconFlow 云 API 完成，同时可配置本地 XInference 作为备选。

**模型配置**（`~/.config/digital-twin/config.yaml` `services.siliconflow`）：
- `model_embed`: BAAI/bge-m3（1024 维）
- `model_reranker`: BAAI/bge-reranker-v2-m3
- `model_llm`: Qwen3-14B（SiliconFlow）/ Qwen3.5-9B（默认）

**双 Provider 路由**（`services.embed`）：
- `embed_provider`: 指定 embed 用哪个 provider（siliconflow / xinference）
- `rerank_provider`: 指定 rerank 用哪个 provider
- `llm_provider`: 指定 LLM 用哪个 provider

**Rust 客户端**：
- `SiliconFlowClient`（`src/infrastructure/siliconflow.rs`）— 实现 EmbedService/LlmService/RerankService，含 3 次重试
- `XInferenceClient`（`src/infrastructure/xinference.rs`）— 与 SiliconFlow 同接口，面向本地推理
- `EmbedProviderRouter`（`src/infrastructure/provider_router.rs`）— 路由到配置的 provider
- `SiliconFlowChatClient`（`src/application/pipeline/infer_client.rs`）— Pipeline 引擎专用 HTTP 客户端

## Hook 事件系统

`config/event-hooks.yaml` 配置的事件驱动写入系统（`src/application/hooks/`），自动将外部操作（代码修改/部署/配置变更/决策/Bug 修复/会话结束/K8s 事件）写入知识图谱。

| Hook | 触发条件 | 写入标签 |
|------|---------|---------|
| `code_modified` | 代码修改 | `:Modification` |
| `jenkins_deploy_completed` | Jenkins 部署完成 | `:Deployment` + JenkinsJob/Build/ServiceInstance |
| `config_changed` | Nacos 配置变更 | `:ConfigChange` |
| `decision_made` | 架构决策 | `:Decision` |
| `session_ended` | 会话结束 | `:Conversation` |

## 项目结构

```
digital-twin-v2/
├── README.md
├── Cargo.toml                  # 单 crate (dt-daemon)
├── src/
│   ├── main.rs                 # 入口 (clap CLI + tokio gRPC server)
│   ├── lib.rs                  # 模块声明
│   ├── domain/                 # 领域层 (5 文件)
│   │   ├── config.rs           # AppConfig + SecretString
│   │   ├── error.rs            # DtError 枚举
│   │   ├── id.rs               # dt://entity/... ID 生成
│   │   ├── traits.rs           # GraphRepository, VectorRepository, EmbedService, ...
│   │   └── types.rs            # 实体类型定义
│   ├── infrastructure/         # 基础设施层 (11 模块)
│   │   ├── memgraph/           # Memgraph Bolt 客户端 + schema
│   │   ├── qdrant/             # Qdrant gRPC 驱动
│   │   ├── sqlite/             # SQLite 快照缓存
│   │   ├── embedder.rs         # NoopEmbedService (fallback)
│   │   ├── siliconflow.rs      # SiliconFlow 云 API 客户端
│   │   ├── xinference.rs       # XInference 本地推理客户端
│   │   ├── provider_router.rs  # 双 Provider 路由
│   │   ├── hanlp.rs            # HanLP NLP 客户端
│   │   ├── scanner.rs          # 文件扫描 + 变更检测
│   │   └── parser/             # tree-sitter 解析器 (7 语言)
│   ├── application/            # 应用层 (8 模块)
│   │   ├── build/              # dt build (strategy/pipeline/watcher)
│   │   ├── context/            # CrossWorldSearch 统一检索 (search_mcp/search_config/search_memory/fusion)
│   │   ├── hooks/              # Hook 事件驱动系统
│   │   ├── knowledge/          # knowledge/memory/reasoning/thread
│   │   ├── pipeline/           # Pipeline Engine (processor orchestration)
│   │   ├── plugins/            # K8s/Svc/Jenkins 插件
│   │   └── sync/               # nacos/k8s/jenkins/kg 同步
│   ├── interfaces/             # 接口层
│   │   ├── cli/                # CLI 命令处理模块
│   │   └── grpc/               # gRPC server + 8 个 service 实现
│   └── shared/                 # 横切关注点
│       ├── logging/            # JSON 日志 (tracing)
│       ├── coordinator.rs      # WriteCoordinator 并发写入协调
│       ├── chunker.rs          # 文档分块策略
│       ├── vectorizer.rs       # 配置/API/日志向量化
│       └── collections.rs      # Qdrant 集合命名约定
├── config/
│   ├── config.yaml.example     # 配置模板
│   ├── pipeline.yaml           # Pipeline 引擎配置
│   │── prompts/                # LLM prompt 模板
│   └── event-hooks.yaml        # Hook 事件定义
├── docs/
│   └── superpowers/            # 设计文档 (specs/) 与实施计划 (plans/)
├── skill/
│   ├── SKILL.md                # digital-twin 技能入口
│   └── guides/                 # 操作指南 (13 份)
├── mcp/                        # MCP 服务器
│   ├── mcp-server.py           # DT MCP Server V2 (25 工具)
│   └── mcp-session-hooks.py    # 会话生命周期钩子 (旧版)
├── scripts/
│   ├── build-all.sh            # 全项目构建
│   ├── setup.sh                # 一键部署
│   ├── check_claude.sh         # Claude 环境检查
│   └── fixes/                  # 一次性修复脚本 (已过时)
├── logs/                       # 会话日志
├── test/
│   ├── expected.json           # 回归基线 (19 文件)
│   ├── fixtures/               # 测试夹具 (10 文件)
│   └── project/                # 集成测试项目 (13 文件)
└── .claude/                    # AI 团队配置
    └── agents/                 # 5 个 AI Agent 角色定义
```

## 架构文档

详见 [docs/superpowers/](docs/superpowers/) 目录：

| 目录 | 内容 |
|------|------|
| [docs/superpowers/specs/](docs/superpowers/specs/) | 设计规格（含知识管线主文档、S5 知识混合检索、统一检索设计） |
| [docs/superpowers/plans/](docs/superpowers/plans/) | 实施计划 |

## HanLP NLP 集成

本地 HanLP 服务（`:8765`）提供中文文本分析能力：

- **处理器**: `HanlpClientProcessor`（优先级 80，GPU 阶段）
- **输出**: `entities`（NER 实体列表，含 text/tag/frequency）、`keywords`（关键词数组）、`summary`（摘要）
- **配置**: `~/.config/digital-twin/config.yaml` 中 `services.hanlp`
- **测试**: `test/expected.json` 已覆盖 19 个文件的 HanLP keyword 基线

## 外部依赖

- **Memgraph 5.x** (Bolt `:7688` 或 `:7687`)
- **Qdrant** (gRPC `:6334`)
- **SiliconFlow API** — embed (BGE-M3)、rerank (BGE-reranker)、chat (Qwen3-14B)
- **HanLP** (可选，`:8765`) — 中文 NLP (NER、关键词)
- **tree-sitter** — 多语言 AST 解析 (Java/Python/TypeScript/Go/Rust/PHP/JavaScript)

## AI 集成 (OpenCode / Claude Code)

AI 助手通过 **MCP 协议**调用 `mcp/mcp-server.py`（25 个工具），检索/记忆类操作优先用 MCP Tool（返回结构化 JSON），CLI 仅作降级：

| 场景 | MCP Tool（首选） | CLI 降级 |
|------|-----------------|---------|
| 环境感知（会话开始） | `dt_sense` | `dt sense --json` |
| 统一搜索（代码/知识/文档/配置/事件） | `dt_search` | `dt search` |
| KG GraphRAG 搜索 | `dt_search_kg` | `dt search --world knowledge` |
| 写知识/事件 | `dt_memorize` / `dt_event` / `dt_learn` | `dt memorize` / `dt event` / `dt learn` |
| 索引构建 | `dt_build` | `dt build` |
| KG/Nacos 同步 | `dt_kg_sync` / `nacos_sync` | `dt kg-sync` / `dt nacos-sync` |
| Jenkins / 微服务 / K8s 日志 | `jcli_*` / `svc_*` / `kublog_*` | `dt jcli` / `dt kub` |
| 健康检查/备份 | `dt_health` / `dt_backup` | 同名 CLI |

事件写入由 Hook 系统自动完成（AI 无需手动调 `dt_event`）：

| 触发操作 | 自动执行 |
|---------|---------|
| 源码编辑 | Hook: `code_modified`（插件自动触发 `dt_build`） |
| Nacos 配置变更 | Hook: `config_changed` |
| 架构决策 | Hook: `decision_made` |
| Bug 修复 | Hook: `bug_fix_recorded` |
| 生产部署 | Hook: `jenkins_deploy_completed` |
| 会话结束 | Hook: `session_ended` |
| K8s Pod 异常 | Hook: `pod_event_occurred` |

> 软件安装/手动记忆等主动写入仍需调 `dt_event` / `dt_memorize`（见 `skill/guides/WRITE-EVENTS.md`）。

## 许可证

MIT
