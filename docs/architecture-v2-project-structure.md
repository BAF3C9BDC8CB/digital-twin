# Digital Twin V2 项目结构设计

> ⚠️ **DEPRECATED**: 本文档已被 [V3 单 Crate 分层架构](./architecture-v3-single-crate-layered.md) 替代。
> V2 多 crate workspace 方案已废弃，实际实现采用单 crate 内部模块分层。
> 保留本文档仅供历史参考。

> 状态：设计阶段 | 日期：2026-07-09（已刷新：新增 dt-backup crate、WriteCoordinator、auth 模块、metrics proto）

---

## 一、V1 现状与 V2 目标

### V1 现状（engine-rust/）

```
engine-rust/ (单体 crate, 34 文件)
  ├── 0 个 trait → 零可扩展性
  ├── 4 个 God Files (config 531行, pipeline 353行, k8s 763行, nacos 467行)
  ├── build.rs/full.rs 70% 重复
  ├── 无 Service/Repository 分层
  └── 硬编码依赖链，无法 mock/测试
```

### V2 目标

| 维度 | V1 | V2 |
|------|-----|-----|
| 架构 | 单体面条代码 | 分层架构 (Interface→Application→Domain→Infrastructure) |
| 可测试性 | 0（无法 mock） | 全部 trait 抽象，单元测试覆盖 |
| 可扩展性 | 0（0 个 trait） | 策略模式、模板方法、插件系统 |
| 模块边界 | 文件级，职责模糊 | crate 级，依赖方向单向 |
| 代码复用 | 多处重复 | Template Method 消除 |

---

## 二、分层架构

```
┌──────────────────────────────────────────────────────────────┐
│  Interface Layer (接口层)                                     │
│  gRPC services, CLI commands                                  │
│  dt-daemon/src/server.rs  dt-daemon/src/main.rs              │
├──────────────────────────────────────────────────────────────┤
│  Application Layer (应用层)                                   │
│  用例编排、业务流程、事务管理                                    │
│  BuildService, SyncService, SearchService, ContextService     │
│  MemoryService, KnowledgeService, LearnService                │
├──────────────────────────────────────────────────────────────┤
│  Domain Layer (领域层)                                        │
│  实体、值对象、领域 trait、业务规则                              │
│  dt-common: types + traits + error                            │
│  dt-pipeline: parse strategies, embed abstraction             │
│  dt-knowledge: knowledge/memory/reasoning/thread entities     │
├──────────────────────────────────────────────────────────────┤
│  Infrastructure Layer (基础设施层)                             │
│  Repository 实现、外部 API Client、文件系统                     │
│  dt-storage: MemgraphRepo, QdrantRepo, SqliteRepo                │
│  dt-sync: NacosClient, K8sClient                              │
│  dt-log: LogService, formatter                                │
└──────────────────────────────────────────────────────────────┘
```

**依赖方向（单向，无循环）：**

```
dt-common ──────────────────────────── 零依赖，定义所有 trait + 类型
  ↑
dt-log ─────────────────────────────── 依赖 dt-common
  ↑
dt-storage ─────────────────────────── 依赖 dt-common + dt-log，实现 repository trait
  ↑
dt-pipeline, dt-sync, dt-knowledge ─── 依赖 dt-storage (通过 trait，非具体类型)
  ↑
dt-context ─────────────────────────── 依赖 dt-pipeline + dt-knowledge
  ↑
dt-plugins ─────────────────────────── 依赖 dt-common (Plugin trait + trait objects)
  ↑
dt-daemon ──────────────────────────── 组合根，依赖所有 crate，负责 DI 装配
```

---

## 三、Cargo Workspace 结构

```
digital-twin-v2/
├── Cargo.toml                          # [workspace] members = [...]
├── rust-toolchain.toml                 # channel = "stable"
├── .rustfmt.toml
├── clippy.toml
│
├── proto/                              # 所有 gRPC proto 定义
│   ├── buf.yaml                        # buf 配置 (lint + breaking change check)
│   ├── common.proto
│   ├── dt_core.proto
│   ├── embed.proto
│   ├── log.proto
│   ├── metrics.proto                   # MetricsService (gRPC 指标，无 HTTP)
│   ├── plugin_k8s.proto
│   ├── plugin_svc.proto
│   └── plugin_jenkins.proto
│
├── crates/
│   ├── dt-common/                      # 共享内核 (0 外部依赖)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # re-export 所有子模块
│   │       ├── types.rs                # Method, Class, Config, EntityId, Project...
│   │       ├── error.rs                # DtError 枚举层级
│   │       ├── traits.rs               # 所有核心 trait (repository, service, plugin)
│   │       └── id.rs                   # EntityId 生成规则 (dt://entity/...)
│   │
│   ├── dt-log/                         # 统一日志基础设施
│   │   ├── Cargo.toml                  # [dependencies] tracing, tracing-subscriber, chrono
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── formatter.rs            # JSON 结构化格式器
│   │       ├── context.rs              # PluginLogger (命名空间隔离)
│   │       └── init.rs                 # tracing-subscriber 初始化
│   │
│   ├── dt-storage/                     # Repository 实现层
│   │   ├── Cargo.toml                  # [dependencies] bolt-driver, tonic(qdrant), rusqlite
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── memgraph/
│   │       │   ├── mod.rs
│   │       │   ├── client.rs           # Memgraph Bolt 连接池
│   │       │   ├── repo.rs             # impl GraphRepository for MemgraphRepository
│   │       │   ├── queries.rs          # Cypher 模板常量
│   │       │   └── schema.rs           # V2 Schema 初始化
│   │       ├── qdrant/
│   │       │   ├── mod.rs
│   │       │   ├── client.rs           # Qdrant gRPC client
│   │       │   ├── repo.rs             # impl VectorRepository for QdrantRepository
│   │       │   └── collection.rs       # Collection 管理
│   │       └── sqlite/
│   │           ├── mod.rs
│   │           ├── repo.rs             # impl SnapshotRepository for SqliteRepository
│   │           └── migrations.rs       # Schema 迁移
│   │
│   ├── dt-pipeline/                    # 数据摄取管道
│   │   ├── Cargo.toml                  # [dependencies] tree-sitter, dt-common, dt-storage
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── service.rs              # BuildService (编排层)
│   │       ├── pipeline.rs             # PipelineTemplate (Template Method 模式)
│   │       ├── strategy/
│   │       │   ├── mod.rs
│   │       │   ├── incremental.rs      # IncrementalStrategy
│   │       │   └── full_rebuild.rs     # FullRebuildStrategy
│   │       ├── coordinator.rs          # WriteCoordinator (并发写入控制)
│   │       ├── chunker.rs              # 文档 Chunking 策略配置
│   │       ├── scanner.rs              # 文件扫描 + 变更检测
│   │       ├── parser/
│   │       │   ├── mod.rs              # ParserRegistry (Strategy 模式)
│   │       │   ├── java.rs
│   │       │   ├── python.rs
│   │       │   ├── typescript.rs
│   │       │   ├── go.rs
│   │       │   ├── rust.rs
│   │       │   ├── php.rs
│   │       │   └── javascript.rs
│   │       ├── embedder.rs             # EmbedService (封装 dt-embed gRPC)
│   │       ├── builder.rs              # BuildCommand
│   │       ├── updater.rs              # UpdateCommand (单文件增量)
│   │       └── watcher.rs              # inotify daemon
│   │
│   ├── dt-sync/                        # 外部系统同步
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── service.rs              # SyncService (编排层)
│   │       ├── traits.rs               # SyncSource trait
│   │       ├── nacos/
│   │       │   ├── mod.rs
│   │       │   ├── client.rs           # Nacos HTTP Client
│   │       │   ├── config_sync.rs      # ConfigSyncSource
│   │       │   └── service_sync.rs     # ServiceSyncSource
│   │       └── k8s/
│   │           ├── mod.rs
│   │           ├── client.rs           # Kuboard K8s API Client
│   │           ├── resource_sync.rs    # ResourceSyncSource
│   │           └── timeline_sync.rs    # K8s Event Timeline
│   │
│   ├── dt-knowledge/                   # 知识/记忆/推理领域逻辑
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── knowledge/
│   │       │   ├── mod.rs
│   │       │   ├── service.rs          # KnowledgeService
│   │       │   ├── entities.rs         # Knowledge, Concept, Domain, Playbook
│   │       │   └── annotation.rs       # @knowledge 注释提取
│   │       ├── memory/
│   │       │   ├── mod.rs
│   │       │   ├── service.rs          # MemoryService
│   │       │   ├── entities.rs         # Day, Session, Event 类型层级
│   │       │   ├── dispatcher.rs       # EventDispatcher (Observer 模式)
│   │       │   └── handlers/           # 各事件处理器
│   │       │       ├── modification.rs
│   │       │       ├── deployment.rs
│   │       │       ├── config_change.rs
│   │       │       └── bug_fix.rs
│   │       ├── reasoning/
│   │       │   ├── mod.rs
│   │       │   ├── service.rs          # ReasoningService
│   │       │   └── lifecycle.rs        # Observation→Analysis→Decision→Knowledge
│   │       ├── thread/
│   │       │   ├── mod.rs
│   │       │   └── service.rs          # ThreadService (Digital Thread)
│   │       └── learn.rs                # LearnService (dt_learn 高层语义)
│   │
│   ├── dt-context/                     # Context Builder (Phase 4)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── service.rs              # ContextService
│   │       ├── pipeline.rs             # ContextPipeline (Chain of Responsibility)
│   │       ├── stages/
│   │       │   ├── mod.rs              # ContextStage trait
│   │       │   ├── retriever.rs        # 并行查询六世界
│   │       │   ├── ranker.rs           # 语义相关度排序
│   │       │   ├── dedup.rs            # 信息去重合并
│   │       │   ├── resolver.rs         # 冲突检测与消解
│   │       │   └── summarizer.rs       # Token 预算压缩
│   │       └── models.rs               # AggregatedContext, WorldSlice
│   │
│   ├── dt-plugins/                     # 内置插件系统
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                  # Plugin trait 定义 + PluginRegistry
│   │       ├── registry.rs             # 插件注册与生命周期
│   │       └── builtin/
│   │           ├── mod.rs
│   │           ├── k8s/                # K8s 操作插件 (kub)
│   │           │   ├── mod.rs
│   │           │   ├── service.rs      # K8sService gRPC impl
│   │           │   ├── logs.rs         # Pod 日志流
│   │           │   └── status.rs       # 资源状态查询
│   │           ├── svc/                # 本地服务管理插件 (svc)
│   │           │   ├── mod.rs
│   │           │   ├── service.rs      # SvcService gRPC impl
│   │           │   ├── manager.rs      # 进程管理
│   │           │   └── logs.rs         # 本地日志查看
│   │           └── jenkins/            # Jenkins CI/CD 插件 (jcli)
│   │               ├── mod.rs
│   │               ├── service.rs      # JenkinsService gRPC impl
│   │               ├── client.rs       # Jenkins REST client
│   │               └── build.rs        # 构建触发与监控
│   │
│   ├── dt-backup/                      # 备份与归档
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── memgraph.rs                # memgraph-admin dump/restore 封装
│   │       ├── qdrant.rs               # Qdrant snapshot API
│   │       ├── sqlite.rs               # SQLite 文件备份
│   │       └── verify.rs               # SHA256 checksum 验证
│   │
│   └── dt-daemon/                      # 组合根 (二进制)
│       ├── Cargo.toml                  # depends on all crates
│       ├── build.rs                    # tonic-build proto 编译
│       └── src/
│           ├── main.rs                 # 入口点 (tokio::main)
│           ├── server.rs               # gRPC server 装配 (tonic Router)
│           ├── wiring.rs               # DI 装配 (创建所有具体实例 + 注入)
│           ├── config.rs               # config.yaml 加载
│           ├── auth.rs                 # gRPC auth interceptor (mTLS + 角色)
│           ├── archive.rs              # Memory 数据归档
│           └── signal.rs               # SIGTERM/SIGINT 优雅关闭
│
├── bin/                                # 独立 CLI 入口 (thin wrapper)
│   ├── kub/                            # kub → dt-daemon gRPC client
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── svc/                            # svc → dt-daemon gRPC client
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── jcli/                           # jcli → dt-daemon gRPC client
│       ├── Cargo.toml
│       └── src/main.rs
│
├── python/
│   ├── mcp_server.py                   # MCP Server (gRPC client to dt-daemon)
│   ├── dt_log.py                       # Python log bridge
│   └── requirements.txt
│
├── services/
│   └── embed-server/                   # dt-embed Python daemon
│       ├── cli.py
│       ├── server.py                   # gRPC EmbedService
│       └── requirements.txt
│
├── config/
│   ├── config.yaml                     # 主配置
│   └── config.example.yaml
│
├── docs/                               # 架构文档
│   ├── architecture-v2-six-worlds.md
│   ├── architecture-v2-data-pipeline.md
│   ├── architecture-v2-data-schema.md
│   ├── architecture-v2-mcp-api-spec.md
│   ├── architecture-v2-pipeline-impl.md
│   ├── architecture-v2-project-structure.md  ← 本文件
│   └── v2-migration-guide.md           # V1→V2 迁移指南（Phase 5）
│
├── tests/                              # 集成测试
│   ├── integration/
│   │   ├── build_pipeline.rs
│   │   ├── sync_nacos.rs
│   │   ├── sync_k8s.rs
│   │   └── context_builder.rs
│   └── fixtures/                       # 测试用 fixture 文件
│
└── .weave/                             # OpenCode 工作流
    └── plans/
        └── v2-implementation-roadmap.md
```

---

## 四、核心 Trait 体系

### 4.1 Repository Traits（dt-common/src/traits.rs）

```rust
// ──── 图存储 Repository ────
#[async_trait]
pub trait GraphRepository: Send + Sync {
    /// 批量 upsert 方法节点
    async fn upsert_methods(&self, project: &str, methods: &[MethodNode]) -> Result<u64>;
    /// 批量 upsert 类节点 + CONTAINS 关系
    async fn upsert_classes(&self, project: &str, classes: &[ClassNode]) -> Result<u64>;
    /// 按文件路径删除旧节点
    async fn delete_by_files(&self, project: &str, files: &[&str]) -> Result<u64>;
    /// 重建调用图 (CALLS 关系)
    async fn rebuild_calls(&self, project: &str, files: &[&str]) -> Result<u64>;
    /// Cypher 查询
    async fn query(&self, cypher: &str, params: HashMap<&str, Value>) -> Result<Vec<Row>>;
    /// 执行写事务
    async fn execute_batch(&self, statements: Vec<Statement>) -> Result<()>;
}

// ──── 向量存储 Repository ────
#[async_trait]
pub trait VectorRepository: Send + Sync {
    async fn ensure_collection(&self, name: &str, dim: u32) -> Result<()>;
    async fn upsert_points(&self, collection: &str, points: Vec<Point>) -> Result<()>;
    async fn delete_by_filter(&self, collection: &str, filter: &Filter) -> Result<()>;
    async fn search(&self, collection: &str, vector: &[f32], limit: u32)
        -> Result<Vec<ScoredPoint>>;
}

// ──── 快照 Repository（变更检测） ────
#[async_trait]
pub trait SnapshotRepository: Send + Sync {
    async fn get_snapshot(&self, project: &str, path: &str) -> Result<Option<FileSnapshot>>;
    async fn save_snapshot(&self, project: &str, snapshots: &[FileSnapshot]) -> Result<()>;
    async fn delete_project(&self, project: &str) -> Result<u64>;
    async fn list_snapshots(&self, project: &str) -> Result<Vec<FileSnapshot>>;
}
```

### 4.2 Service Traits（dt-pipeline/src/service.rs 等）

```rust
// ──── 构建服务 ────
#[async_trait]
pub trait BuildService: Send + Sync {
    async fn build(&self, project: &str, path: &Path) -> Result<BuildReport>;
    async fn update_file(&self, project: &str, path: &Path) -> Result<UpdateReport>;
    async fn delete_project(&self, project: &str) -> Result<()>;
    async fn list_projects(&self) -> Result<Vec<ProjectStatus>>;
}

// ──── 同步服务 ────
#[async_trait]
pub trait SyncService: Send + Sync {
    async fn sync_nacos(&self, env: &str) -> Result<SyncReport>;
    async fn sync_k8s(&self) -> Result<SyncReport>;
    async fn sync_kg_to_qdrant(&self, incremental: bool) -> Result<SyncReport>;
}

// ──── 搜索服务 ────
#[async_trait]
pub trait SearchService: Send + Sync {
    async fn search_code(&self, query: &str, scope: &SearchScope) -> Result<Vec<CodeResult>>;
    async fn search_kg(&self, query: &str, limit: u32) -> Result<Vec<KgResult>>;
    async fn search_cross_world(&self, query: &str, worlds: &[World]) -> Result<CrossWorldResult>;
}

// ──── 上下文服务 ────
#[async_trait]
pub trait ContextService: Send + Sync {
    async fn build_context(&self, task: &str, options: &ContextOptions)
        -> Result<AggregatedContext>;
}
```

### 4.3 Embed Service Trait

```rust
// ──── 嵌入服务（dt-pipeline/src/embedder.rs） ────
#[async_trait]
pub trait EmbedService: Send + Sync {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    async fn health(&self) -> Result<HealthStatus>;
}
```

### 4.4 Plugin Trait（dt-plugins/src/lib.rs）

```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;

    fn register_grpc(&self, builder: tonic::transport::server::Router)
        -> Result<tonic::transport::server::Router, PluginError>;

    async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError>;
    async fn health(&self) -> Result<HealthStatus, PluginError>;
    async fn shutdown(&self) -> Result<(), PluginError>;
}

pub struct PluginContext {
    pub graph: Arc<dyn GraphRepository>,
    pub vector: Arc<dyn VectorRepository>,
    pub config: Arc<AppConfig>,
    pub log: PluginLogger,
    pub data_dir: PathBuf,
}
```

---

## 五、关键设计模式

### 5.1 Template Method — 消除 build/full/update 的 70% 重复

```
BuildStrategy (trait)
  ├── select_files() → 子类决定处理哪些文件
  ├── prepare()      → 子类决定如何清理旧数据
  │
  ├── IncrementalStrategy: diff compare → incremental delete
  ├── FullRebuildStrategy: wipe all → full insert
  └── SingleFileStrategy: single file → targeted delete/insert

PipelineTemplate::execute()  ← 模板方法，定义固定流程
  1. select_files(strategy)
  2. prepare(strategy)
  3. parse_files()          [固定]
  4. embed_batch()          [固定]
  5. upsert_vectors()       [固定]
  6. upsert_graph()         [固定]
  7. rebuild_calls()        [固定]
  8. update_snapshots()     [固定]
```

### 5.2 Strategy — 多语言解析器

```
ParseStrategy (trait)
  ├── language() → Language
  ├── can_parse(path) → bool
  └── parse(source, path) → Vec<MethodBlock>

  ├── JavaParser      implements ParseStrategy
  ├── PythonParser    implements ParseStrategy
  ├── TypeScriptParser implements ParseStrategy
  ├── GoParser        implements ParseStrategy
  ├── RustParser      implements ParseStrategy
  ├── PhpParser       implements ParseStrategy
  └── JavaScriptParser implements ParseStrategy

ParserRegistry::parse_file(path, source)
  → 遍历 strategies，找到 can_parse() 返回 true 的
  → 委托给该策略的 parse()
```

### 5.3 Observer — Memory World 事件系统

```
EventHandler (trait)
  ├── event_type() → EventType
  └── handle(event, graph) → Result<()>

  ├── ModificationHandler  → creates (:Modification)-[:AFFECTS]->(:Method)
  ├── DeploymentHandler    → creates (:Deployment)-[:DEPLOYS]->(:ServiceInstance)
  ├── ConfigChangeHandler  → creates (:ConfigChange)-[:AFFECTS]->(:NacosConfig)
  └── BugFixHandler        → creates (:BugFix)-[:FIXES]->(:Method)

EventDispatcher::dispatch(event)
  → 查找匹配 event_type 的 handler
  → 调用 handler.handle()
  → 失败不影响其他 handlers
```

### 5.4 Chain of Responsibility — Context Builder 管道

```
ContextStage (trait)
  └── process(state: ContextState) → ContextState

  Retriever → Ranker → Deduper → Resolver → Summarizer
       ↓          ↓        ↓         ↓           ↓
    并行查询    相关度   去重合并   冲突检测    token压缩
    六世界      排序     保留来源   标记冲突    摘要生成

ContextPipeline::execute(task)
  → state = ContextState::new(task)
  → for stage in stages: state = stage.process(state)
  → state.into_aggregated()
```

### 5.5 Repository — 存储抽象

```
         ┌─────────────────────────────┐
         │   Application Layer (trait) │
         │   GraphRepository           │
         │   VectorRepository          │
         │   SnapshotRepository        │
         └──────────┬──────────────────┘
                    │ implements
         ┌──────────▼──────────────────┐
         │   Infrastructure Layer      │
         │   MemgraphRepository (Bolt)    │
         │   QdrantRepository (gRPC)   │
         │   SqliteRepository (local)  │
         └─────────────────────────────┘
```

### 5.6 Dependency Injection — dt-daemon 作为组合根

```rust
// crates/dt-daemon/src/wiring.rs
pub struct AppComponents {
    pub graph: Arc<dyn GraphRepository>,
    pub vector: Arc<dyn VectorRepository>,
    pub snapshot: Arc<dyn SnapshotRepository>,
    pub embed: Arc<dyn EmbedService>,
    pub build: Arc<dyn BuildService>,
    pub sync: Arc<dyn SyncService>,
    pub search: Arc<dyn SearchService>,
    pub knowledge: Arc<dyn KnowledgeService>,
    pub memory: Arc<dyn MemoryService>,
    pub context: Arc<dyn ContextService>,
    pub plugins: PluginRegistry,
}

pub async fn wire(config: &AppConfig) -> Result<AppComponents> {
    // 1. 基础设施层
    let graph = Arc::new(MemgraphRepository::connect(&config.memgraph).await?);
    let vector = Arc::new(QdrantRepository::connect(&config.qdrant).await?);
    let snapshot = Arc::new(SqliteRepository::open(&config.snapshot_path)?);
    let embed = Arc::new(GrpcEmbedService::connect(&config.embed).await?);

    // 2. 服务层（注入 Repository）
    let build = Arc::new(BuildServiceImpl::new(
        graph.clone(), vector.clone(), snapshot.clone(), embed.clone()
    ));
    let sync = Arc::new(SyncServiceImpl::new(config, graph.clone()));
    let search = Arc::new(SearchServiceImpl::new(vector.clone(), graph.clone()));
    // ...

    // 3. 插件层
    let plugins = PluginRegistry::new(PluginContext {
        graph: graph.clone(),
        vector: vector.clone(),
        config: Arc::new(config.clone()),
        log: PluginLogger::new("plugin"),
        data_dir: config.plugin_dir.clone(),
    });
    plugins.register(Box::new(K8sPlugin::new(config)))?;
    plugins.register(Box::new(SvcPlugin::new(config)))?;
    plugins.register(Box::new(JenkinsPlugin::new(config)))?;

    Ok(AppComponents { graph, vector, snapshot, embed, build, sync, search, plugins, ... })
}
```

---

## 六、Crate 依赖关系图

```
                          dt-daemon (组合根)
                         /    |    |    \
                        /     |    |     \
              dt-plugins  dt-context  dt-sync  dt-knowledge
                  |           |  \       |        |
                  |     dt-pipeline |     |        |
                  |           |     |     |        |
                  +-----------+-----+-----+--------+
                              |
                          dt-storage
                          /    |    \
                         /     |     \
                    dt-common  dt-log  (bolt-driver, tonic, rusqlite)
```

---

## 七、与 V1 的迁移映射

| V1 文件 | V2 位置 | 重构内容 |
|---------|---------|----------|
| `models.rs` (24行) | `dt-common/src/types.rs` | 扩展为完整实体定义 |
| `error.rs` | `dt-common/src/error.rs` | DtError 层级化 |
| `config.rs` (531行 God File) | `dt-daemon/src/config.rs` | 仅保留加载逻辑，拆出 ScannerConfig |
| `client/memgraph.rs` (266行) | `dt-storage/src/memgraph/` | HTTP→Bolt + trait 实现 + 分文件 |
| `client/qdrant.rs` (152行) | `dt-storage/src/qdrant/` | HTTP→gRPC + trait 实现 + 分文件 |
| `client/embed.rs` (140行) | `dt-pipeline/src/embedder.rs` | 封装为 trait + gRPC client |
| `index/build.rs` (192行) | `dt-pipeline/src/pipeline.rs` + `strategy/incremental.rs` | Template Method |
| `index/full.rs` (126行) | `dt-pipeline/src/strategy/full_rebuild.rs` | 策略实现 |
| `index/pipeline.rs` (353行 God File) | 拆入 `scanner.rs` + `parser/` + `embedder.rs` + `pipeline.rs` | 单一职责 |
| `index/update.rs` (99行) | `dt-pipeline/src/updater.rs` | UpdateCommand |
| `parser.rs` (144行) | `dt-pipeline/src/parser/` | Strategy 模式拆分 |
| `search.rs` (317行) | `dt-pipeline/src/search.rs` (暂) / 未来 `dt-search` crate | trait 抽象 |
| `event.rs` (65行) | `dt-knowledge/src/memory/dispatcher.rs` + `handlers/` | Observer 模式 |
| `knowledge.rs` (88行) | `dt-knowledge/src/knowledge/` | KnowledgeService |
| `sync/nacos.rs` (467行 God File) | `dt-sync/src/nacos/` | 拆 client + config_sync + service_sync |
| `sync/k8s.rs` (763行 God File) | `dt-sync/src/k8s/` | 拆 client + resource_sync + timeline_sync |
| `sync/kg.rs` (178行) | `dt-sync/` (保留为 kg_bridge) | 重写查询标签 |
| `main.rs` (285行) | `dt-daemon/src/main.rs` | gRPC server 入口 |
| (新增) | `dt-daemon/src/auth.rs` | gRPC auth interceptor |
| (新增) | `dt-pipeline/src/coordinator.rs` | 并发写入控制 |
| (新增) | `crates/dt-backup/` | 分层备份与恢复 |

---

## 八、编译与构建

### Workspace Cargo.toml

```toml
[workspace]
resolver = "2"
members = [
    "crates/dt-common",
    "crates/dt-log",
    "crates/dt-storage",
    "crates/dt-pipeline",
    "crates/dt-sync",
    "crates/dt-knowledge",
    "crates/dt-context",
    "crates/dt-plugins",
    "crates/dt-backup",
    "crates/dt-daemon",
    "bin/kub",
    "bin/svc",
    "bin/jcli",
]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
tonic = "0.12"
prost = "0.13"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
chrono = "0.4"
async-trait = "0.1"
# ... shared across crates

[workspace.metadata]
# CI: cargo check --workspace
# CI: cargo test --workspace
# CI: cargo clippy --workspace -- -D warnings
# CI: cargo fmt --check
```

### Proto 编译（dt-daemon/build.rs）

```rust
fn main() -> Result<()> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/generated")
        .compile_protos(
            &[
                "../proto/common.proto",
                "../proto/dt_core.proto",
                "../proto/embed.proto",
                "../proto/log.proto",
                "../proto/metrics.proto",
                "../proto/plugin_k8s.proto",
                "../proto/plugin_svc.proto",
                "../proto/plugin_jenkins.proto",
            ],
            &["../proto"],
        )?;
    Ok(())
}
```

---

## 九、质量门禁

| 检查 | 工具 | 通过标准 |
|------|------|----------|
| 编译 | `cargo check --workspace` | 0 errors |
| 测试 | `cargo test --workspace` | 全部通过 |
| Lint | `cargo clippy --workspace -- -D warnings` | 0 warnings |
| 格式 | `cargo fmt --check` | 无变更 |
| Proto | `buf lint proto/` | 0 violations |
| 依赖审计 | `cargo deny check` | 0 advisories |
| 文档 | `cargo doc --no-deps --workspace` | 0 warnings |
