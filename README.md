# Digital Twin V2

**AI 辅助开发的持久记忆层**。结合 Neo4j 知识图谱 + Qdrant 向量数据库，为 AI Agent 提供跨会话上下文。

## 架构

单 crate DDD 分层 (src/domain → src/infrastructure → src/application → src/interfaces)，六世界模型：

```
src/
  domain/          # 领域层: types, traits, error, config, id
  infrastructure/  # 基础设施: neo4j, qdrant, sqlite, parser, scanner, embedder
  application/     # 应用层: build, sync, context, knowledge, plugins
  interfaces/      # 接口层: gRPC server, CLI
  shared/          # 横切: logging, metrics, coordinator, chunker, vectorizer
```

六世界聚合上下文：

| 世界 | 数据 | 存储 |
|------|------|------|
| Reality | 代码、配置、K8s 资源 | Neo4j + Qdrant |
| Knowledge | 概念、模式、Playbook、经验 | Neo4j |
| Memory | 事件、会话、时间线 | Neo4j |
| Semantic | 文档、API、日志模式向量 | Qdrant |
| Runtime | Pod 状态、服务运行态 | K8s API 实时查询 |
| Reasoning | 观察 → 分析 → 决策链路 | Neo4j (含 TTL) |

## 快速开始

### 依赖

- Neo4j 5.x (Bolt :7687)
- Qdrant (gRPC :6334)
- dt-embed (Python, BGE-M3 :50052)

### 构建

```bash
cargo build --release
./target/release/dt --help
```

## CLI 命令 (24 个)

### 管线

| 命令 | 功能 |
|------|------|
| `dt build` | 构建项目索引到知识图谱 |
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

| `dt backup` | 分层备份 (Neo4j + Qdrant + SQLite) |
| `dt archive` | Memory 数据归档 |
| `dt clean` | 清空所有数据 |
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
│   │   ├── embedder.rs         # dt-embed gRPC client
│   │   ├── neo4j/              # Neo4j Bolt 驱动 (neo4rs)
│   │   ├── parser/             # tree-sitter 多语言解析器
│   │   ├── qdrant/             # Qdrant gRPC 驱动
│   │   ├── scanner.rs          # 项目文件扫描
│   │   └── sqlite/             # SQLite 快照缓存
│   ├── application/
│   │   ├── build/              # dt build / update / watch
│   │   ├── context/            # Context Builder 管道 (6 world)
│   │   ├── knowledge/          # memorize / learn / event
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
├── docs/                       # 架构设计文档 (7 份)
│   ├── architecture-v3-single-crate-layered.md
│   ├── architecture-v2-six-worlds.md
│   ├── architecture-v2-data-schema.md
│   ├── architecture-v2-data-pipeline.md
│   ├── architecture-v2-pipeline-impl.md
│   ├── architecture-v2-project-structure.md
│   └── architecture-v2-mcp-api-spec.md
├── services/
│   └── embed-server/           # dt-embed (Python + BGE-M3)
└── .weave/plans/               # 实施路线图
    └── v2-implementation-roadmap.md
```

## 架构文档

详见 [docs/](docs/) 目录 (7 份架构设计文档)：

| 文档 | 内容 |
|------|------|
| [architecture-v3-single-crate-layered.md](docs/architecture-v3-single-crate-layered.md) | 当前架构: 单 crate DDD 五层 |
| [architecture-v2-six-worlds.md](docs/architecture-v2-six-worlds.md) | 六世界模型设计 |
| [architecture-v2-data-schema.md](docs/architecture-v2-data-schema.md) | Neo4j Schema: 25 约束 + 全文索引 |
| [architecture-v2-data-pipeline.md](docs/architecture-v2-data-pipeline.md) | 数据采集管线设计 |
| [architecture-v2-pipeline-impl.md](docs/architecture-v2-pipeline-impl.md) | 管道实现细节 |
| [architecture-v2-project-structure.md](docs/architecture-v2-project-structure.md) | 项目结构设计 |
| [architecture-v2-mcp-api-spec.md](docs/architecture-v2-mcp-api-spec.md) | 12 个 MCP 工具 API 规范 |

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
