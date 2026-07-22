# Digital Twin V2 架构设计：单 Crate 分层架构

> 状态：实现完成 | 日期：2026-07-10
> 替代 V2 workspace 多 crate 方案，改为单 crate 内部模块分层

---

## 为什么从多 Crate 改为单 Crate

原 V2 设计将 10 个功能模块拆成独立 crate，导致：
- 依赖关系复杂（dt-pipeline → dt-storage → dt-common，链路长）
- 跨 crate 重构成本高（改一个 trait 要改 5 个 Cargo.toml）
- 编译时间长（每个 crate 独立编译）
- 模块边界实际上不需要 crate 级隔离

**新方案**：单个 `dt-daemon` crate，内部通过 Rust module 实现 DDD 四层分层。

---

## 目录结构

```
dt-daemon/
├── Cargo.toml                    # 单一 crate，所有依赖在此声明
├── build.rs                      # tonic-build proto 编译
├── src/
│   ├── main.rs                   # 入口点 (tokio::main)
│   ├── lib.rs                    # crate 根，声明所有模块
│   │
│   ├── domain/                   # ─── 领域层 ───
│   │   ├── mod.rs                #    实体、值对象、领域 trait
│   │   ├── types.rs              #    所有实体类型 (Method, Class, Service...)
│   │   ├── error.rs              #    DtError 枚举层级
│   │   ├── traits.rs             #    核心 trait (GraphRepository, VectorRepository...)
│   │   ├── id.rs                 #    EntityId 生成规则
│   │   └── config.rs             #    SecretString, AppConfig
│   │
│   ├── infrastructure/           # ─── 基础设施层 ───
│   │   ├── mod.rs                #    外部系统交互、存储实现
│   │   ├── memgraph/                #    Memgraph 图存储
│   │   │   ├── mod.rs
│   │   │   ├── client.rs         #    Bolt 连接池
│   │   │   ├── repo.rs           #    impl GraphRepository（含内联 Cypher 模板）
│   │   │   └── schema.rs         #    V2 Schema 初始化
│   │   ├── qdrant/               #    Qdrant 向量存储
│   │   │   ├── mod.rs
│   │   │   ├── client.rs         #    gRPC client
│   │   │   ├── repo.rs           #    impl VectorRepository
│   │   │   └── collection.rs     #    Collection 管理
│   │   ├── sqlite/               #    SQLite 快照
│   │   │   ├── mod.rs
│   │   │   └── repo.rs
│   │   ├── embedder.rs           #    dt-embed gRPC client
│   │   ├── scanner.rs            #    文件扫描 + 变更检测
│   │   └── parser/               #    AST 解析器 (Strategy 模式)
│   │       ├── mod.rs            #    ParserRegistry
│   │       ├── java.rs
│   │       ├── python.rs
│   │       ├── typescript.rs
│   │       ├── go.rs
│   │       ├── rust_parser.rs        #    避免与 Rust 关键字冲突
│   │       ├── php.rs
│   │       ├── javascript.rs
│   │       └── document.rs       #    文档解析
│   │
│   ├── application/              # ─── 应用层 ───
│   │   ├── mod.rs                #    用例编排、业务流程
│   │   ├── build/                #    dt build 命令
│   │   │   ├── mod.rs
│   │   │   ├── service.rs        #    BuildService
│   │   │   ├── pipeline.rs       #    PipelineTemplate (Template Method)
│   │   │   ├── strategy.rs       #    BuildStrategy + 增量/全量策略
│   │   │   ├── builder.rs        #    BuildCommand CLI
│   │   │   ├── updater.rs        #    UpdateCommand (单文件增量)
│   │   │   └── watcher.rs        #    FileWatcher daemon
│   │   ├── sync/                 #    外部系统同步
│   │   │   ├── mod.rs
│   │   │   ├── service.rs        #    SyncService
│   │   │   ├── nacos/            #    Nacos 配置同步
│   │   │   │   ├── mod.rs
│   │   │   │   ├── client.rs
│   │   │   │   ├── config_sync.rs
│   │   │   │   └── service_sync.rs
│   │   │   ├── k8s/              #    K8s 资源同步
│   │   │   │   ├── mod.rs
│   │   │   │   ├── client.rs
│   │   │   │   ├── resource_sync.rs
│   │   │   │   ├── timeline_sync.rs
│   │   │   │   └── types.rs
│   │   │   └── kg_bridge.rs      #    KG→Qdrant 桥接
│   │   ├── context/              #    Context Builder
│   │   │   ├── mod.rs
│   │   │   ├── service.rs
│   │   │   ├── context_service.rs
│   │   │   ├── pipeline.rs
│   │   │   ├── models.rs
│   │   │   ├── stages/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── retriever.rs
│   │   │   │   ├── ranker.rs
│   │   │   │   ├── dedup.rs
│   │   │   │   ├── resolver.rs
│   │   │   │   └── summarizer.rs
│   │   │   ├── plan.rs           #    dt_plan
│   │   │   ├── domain_query.rs   #    dt_domain
│   │   │   ├── history.rs        #    dt_history
│   │   │   ├── dependency.rs     #    dt_dependency
│   │   │   ├── verify.rs         #    dt_verify
│   │   │   └── search_mcp.rs     #    dt_search 跨世界
│   │   ├── knowledge/            #    知识/记忆/推理
│   │   │   ├── mod.rs
│   │   │   ├── knowledge/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── service.rs
│   │   │   │   ├── entities.rs
│   │   │   │   └── annotation.rs
│   │   │   ├── memory/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── service.rs
│   │   │   │   ├── entities.rs
│   │   │   │   ├── dispatcher.rs
│   │   │   │   └── handlers/
│   │   │   │       ├── mod.rs
│   │   │   │       ├── modification.rs
│   │   │   │       ├── deployment.rs
│   │   │   │       ├── config_change.rs
│   │   │   │       ├── bug_fix.rs
│   │   │   │       └── decision.rs
│   │   │   ├── reasoning/
│   │   │   │   ├── mod.rs
│   │   │   │   ├── service.rs
│   │   │   │   └── lifecycle.rs
│   │   │   ├── learn.rs
│   │   │   └── thread/
│   │   │       ├── mod.rs
│   │   │       └── service.rs
│   │   └── plugins/              #    插件系统
│   │       ├── mod.rs            #    Plugin trait
│   │       ├── registry.rs       #    PluginRegistry
│   │       ├── k8s/              #    K8s 操作插件
│   │       │   ├── mod.rs
│   │       │   ├── service.rs
│   │       │   ├── logs.rs
│   │       │   └── status.rs
│   │       ├── svc/              #    本地服务管理插件
│   │       │   ├── mod.rs
│   │       │   ├── service.rs
│   │       │   ├── manager.rs
│   │       │   └── logs.rs
│   │       └── jenkins/          #    Jenkins CI/CD 插件
│   │           ├── mod.rs
│   │           ├── service.rs
│   │           ├── client.rs
│   │           └── build.rs
│   │
│   ├── interfaces/               # ─── 接口层 ───
│   │   ├── mod.rs                #    gRPC server + CLI
│   │   ├── grpc/
│   │   │   ├── mod.rs
│   │   │   ├── server.rs         #    tonic server 装配
│   │   │   ├── auth.rs           #    auth interceptor (mTLS + 角色)
│   │   │   ├── wiring.rs         #    DI 装配 (AppComponents)
│   │   │   └── services/         #    gRPC service 实现
│   │   │       ├── mod.rs
│   │   │       ├── dt_core_service.rs
│   │   │       ├── build_service.rs
│   │   │       ├── context_service.rs
│   │   │       ├── sync_service.rs
│   │   │       ├── knowledge_service.rs
│   │   │       ├── memory_service.rs
│   │   │       ├── log_service.rs
│   │   │       └── metrics_service.rs
│   │   └── cli/                  #    CLI command handlers
│   │       ├── mod.rs
│   │       ├── build.rs
│   │       ├── sync.rs
│   │       ├── event.rs
│   │       ├── memorize.rs
│   │       ├── learn.rs
│   │       ├── context.rs
│   │       ├── thread.rs
│   │       ├── kub.rs
│   │       ├── jcli.rs
│   │       ├── backup.rs
│   │       ├── backup_memgraph.rs
│   │       ├── backup_qdrant.rs
│   │       ├── backup_sqlite.rs
│   │       ├── backup_verify.rs
│   │       ├── archive.rs
│   │       └── cleanup.rs
│   │
│   └── shared/                   # ─── 共享/横切 ───
│       ├── mod.rs
│       ├── logging/              #    统一日志 (dt-log)
│       │   ├── mod.rs
│       │   ├── formatter.rs
│       │   ├── context.rs        #    PluginLogger
│       │   ├── init.rs
│       │   └── metrics.rs        #    指标采集
│       ├── coordinator.rs        #    WriteCoordinator
│       ├── chunker.rs            #    文档分块
│       └── vectorizer.rs         #    Config/API/Log 向量化
│
├── proto/                        # gRPC proto 定义 (不变)
├── python/                       # MCP Server + dt_log.py (不变)
├── services/embed-server/        # dt-embed Python daemon (不变)
├── config/                       # 配置文件
├── docs/                         # 架构文档
└── tests/                        # 集成测试
```

---

## 依赖方向

```
interfaces/ (gRPC + CLI)
    │
    ▼
application/ (用例编排)
    │
    ├──▶ domain/ (实体 + trait)
    │       ▲
    └──▶ infrastructure/ (存储 + 外部API)
            │
            ▼
        shared/ (横切工具)
```

**规则**：
- `interfaces/` 可以 import `application/`, `domain/`, `shared/`
- `application/` 可以 import `domain/`, `infrastructure/`, `shared/`
- `infrastructure/` 可以 import `domain/`, `shared/`
- `domain/` **不可以** import 其他任何模块（纯领域层，零外部依赖）
- `shared/` **不可以** import `application/` 或 `infrastructure/`
- **禁止循环依赖**

---

## 与原多 Crate 方案的映射

| 原 Crate | 新位置 |
|----------|--------|
| dt-common | domain/ + shared/ |
| dt-log | shared/logging/ |
| dt-storage | infrastructure/ (memgraph + qdrant + sqlite) |
| dt-pipeline | application/build/ + infrastructure/parser/ + infrastructure/scanner.rs |
| dt-sync | application/sync/ |
| dt-knowledge | application/knowledge/ |
| dt-context | application/context/ |
| dt-plugins | application/plugins/ |
| dt-backup | interfaces/cli/backup.rs + 骨架代码 |
| dt-daemon | interfaces/ + main.rs |

---

## 关键设计模式 (不变)

- **Template Method**：BuildService PipelineTemplate
- **Strategy**：7 种语言 Parser、BuildStrategy (增量/全量)
- **Observer**：Memory EventDispatcher
- **Chain of Responsibility**：Context Builder 5 阶段管道
- **Repository**：GraphRepository / VectorRepository trait
- **Plugin**：Plugin trait + PluginRegistry

---

## 编译与构建

```toml
[package]
name = "dt-daemon"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "dt"
path = "src/main.rs"
```

单一 `cargo build` 编译所有代码，不再需要 `cargo check --workspace`。

---

## 实现状态

> 日期：2026-07-10

| 指标 | 数值 |
|------|------|
| main.rs 行数 | 1566 行（从 V2 workspace 的 2625 行缩减） |
| gRPC services | `DtCoreServiceImpl` 已实现，含 6 个 RPC（Build/Search/GetContext/RecordEvent/Memorize/Sync） |
| wiring.rs | 连接真实 Memgraph/Qdrant 后端（从 config.yaml 读取连接配置） |
| Thread service | 已移动至 `application/knowledge/thread/`（含 `mod.rs` + `service.rs`） |
| CLI 命令 | 已提取 17 个模块到 `interfaces/cli/`（build, sync, event, memorize, learn, context, thread, kub, jcli, backup 等） |
| parser | Rust 解析器实际文件名为 `rust_parser.rs`（避免与 `rust` 关键字冲突） |
| Memgraph Cypher | 模板内联在 `infrastructure/memgraph/repo.rs` 中，无独立 `queries.rs` 文件 |
| metrics | 位于 `shared/logging/metrics.rs`（不在 `shared/` 根目录） |
| search | 实际文件名为 `context/search_mcp.rs`
