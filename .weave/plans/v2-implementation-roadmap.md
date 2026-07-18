# Digital Twin V2 分阶段实施路线图

> **状态**: Phase 1-4 代码完成 ✅ | Phase 5 文档 + skeleton 完成 ✅ | gRPC 服务已实现, CLI 已拆分, 实境测试待真实后端 ⚠️
> **架构**: 单 crate DDD 分层 (domain/infrastructure/application/interfaces/shared)，详见 [V3 架构文档](../docs/architecture-v3-single-crate-layered.md)

**Goal:** 在完全清空 V1 数据的前提下，从零构建六世界模型、Context Builder 和 12 个高层 MCP 工具。

**Architecture:** 单体 dt-daemon crate，内部 5 层模块分层。依赖 Neo4j/Qdrant/BGE-M3。

**Tech Stack:** Rust (dt CLI daemon + gRPC), Python (MCP Server / dt-embed), Neo4j (Bolt), Qdrant (gRPC), BGE-M3 (1024-dim)

---

## TL;DR

> **Summary**: 5 个 Phase，总预估 3-4 月。Phase 1 搭建全部基础设施（workspace + gRPC + 插件系统 + 安全 + 日志/指标 + Schema + 管线），Phase 4 实现 Context Builder（最核心价值），Phase 5 集成验证上线。
> **Estimated Effort**: XL
> **Branch**: `feat/v2-architecture`

---

## Context

### Original Request
基于 V2 六世界模型设计文档，制定分阶段实施路线图。关键前提：完全不兼容 V1，所有数据清空重来。

### Key Findings
- V1 代码库（engine-rust/ 34 文件）是面条代码：**0 个 trait、4 个 God File、70% 重复、无分层**——V2 需要完全重写，不兼容 V1
- **架构决策已就绪**（详见 docs/）：
  - [项目结构设计](../docs/architecture-v2-project-structure.md)：11 crate workspace，4 层架构，6 种设计模式
  - gRPC + Bolt 统一通信（消除 HTTP REST + 自定义帧 + subprocess）
  - 插件系统集成 kub/svc/jcli 三个工具
  - 统一日志系统（dt-log crate + gRPC LogService）
- **四步基础设施**先行（Phase 1.0a/1.0/1.0b/1.0c）：workspace 搭建 → gRPC + 插件 → 日志/指标 → 安全鉴权
- 设计文档完整覆盖：六世界模型 + 数据格式 + 全链路 + MCP API + 管道实现 + 项目结构
- 底层基础设施（Neo4j/Qdrant/dt-embed/tree-sitter/Nacos/K8s）已就绪，需切换驱动和通信协议

---

## Objectives

### Core Objective
在 `feat/v2-architecture` 分支上，分 5 个 Phase 完成从 Schema 到 Context Builder 到高层 MCP 的全链路实现。

### Deliverables
- [x] Phase 1: Workspace + gRPC/插件/日志/指标基础设施 + 安全鉴权 + 新 Neo4j Schema + Reality World 数据管线 + 并发写入协调
- [x] Phase 2: Memory + Knowledge World 写入管线（含知识版本管理 + 执行结果自动采集）
- [x] Phase 3: Semantic + Reasoning World（含 chunking 策略 + 模型迁移路径）
- [x] Phase 4: Context Builder + 12 个高层 MCP 工具 + 反馈闭环 + 备份恢复 + 数据清理
- [ ] Phase 5: 端到端集成测试 + 性能优化 + 文档

### Definition of Done
- [x] `cargo check && cargo test && cargo clippy` 全部通过 (494 tests)
- [x] gRPC mTLS 配置 + SecretString 脱敏（代码就绪，待生产证书）
- [x] `dt backup` Qdrant 备份+恢复已实测，Neo4j 需 neo4j-admin 配置
- [x] `dt build` 成功写入（实测: third-center 2790 Method + 301 Class）
- [x] `dt nacos-sync --env test` 成功写入（实测: 350 NacosConfig + 42 Service + 880 ConfigKey）
- [x] Memory Event 写入后能沿 Day→Session→Event 时间线查询（实测通过）
- [x] `dt_context` 返回六世界聚合上下文（实测: 23 items, ~190 tokens, 含 alerts）
- [x] `dt_verify` 实测通过（3 passed, 1 warned）
- [x] 全部 12 个高层 MCP 工具通过 gRPC 可调用（8 个核心六世界业务 + 4 个系统运维） — DtCore gRPC service 已实现 (6 RPCs: Build/Search/GetContext/RecordEvent/Memorize/Sync)
- [x] kub/svc/jcli 已融入 dt 插件系统（独立CLI不再需要）
- [x] `dt metrics` 可查询（占位 snapshot, 不暴露 HTTP）

### Guardrails (Must NOT)
- **不兼容 V1**：不考虑数据迁移，不保留旧 schema，旧 engine-rust/ 代码仅作参考不复用
- **不跳过基础设施**：必须先完成 workspace + gRPC + 日志 + 插件框架 + 安全鉴权，再编写业务代码
- **不违反分层架构**：上层不可直接依赖下层具体实现，全部通过 trait 注入
- **不影响生产环境**：所有操作在 feat/v2-architecture 分支，未完成前不合入 main
- **不暴露敏感信息**：密码/密钥走 SecretString（env/vault），日志输出自动脱敏；gRPC 生产环境必须 mTLS
- **不暴露 HTTP 监控端口**：所有指标走 gRPC MetricsService 或结构化日志，无独立 HTTP endpoint

---

## TODOs

> **项目结构规范**：详见 [architecture-v2-project-structure.md](../docs/architecture-v2-project-structure.md)。所有实现必须遵守该文档定义的分层架构（Interface→Application→Domain→Infrastructure）、Trait 体系和设计模式。

### Phase 1: Core Infrastructure — 新 Schema + Reality World 数据管线重写

**目标**：落地新 Neo4j Schema，重写代码/配置/K8s 数据采集管线，确保 Reality World 能全量填充。本阶段同时完成插件系统、gRPC 通信、统一日志、安全鉴权、并发写入协调器（WriteCoordinator）等全部基础设施。

**依赖**：无（起点）

**预估工作量**：XXL（包含 workspace 搭建 + gRPC 通信 + 插件系统 + 日志系统 + Schema + 管线重写，是整个 V2 最硬核的 Phase）

---

- [x] 1.0a Workspace 初始化：Cargo workspace + 空 crate 骨架 + CI
  **What**: 搭建 Cargo workspace 结构，创建所有 crates 的空骨架（仅 Cargo.toml + lib.rs + 依赖声明），配置 CI 门禁。
  **Files**: 
  - 根 `Cargo.toml`（workspace 定义 + 共享依赖）
  - 所有 `crates/dt-*/Cargo.toml`（仅依赖声明）
  - `crates/dt-common/src/lib.rs`（types/error/traits/id 模块声明）
  - 所有 crate 的 `src/lib.rs`（仅 `// TODO: Phase N` 占位）
  - `dt-daemon/src/main.rs`（空 tokio::main + gRPC server 骨架）
  - `rust-toolchain.toml`, `.rustfmt.toml`, `clippy.toml`
  - `.github/workflows/ci.yml`（check + test + clippy + fmt）
  **Acceptance**:
  - `cargo check --workspace` 0 errors
  - `cargo build --workspace` 成功
  - 依赖图无循环（`cargo tree --workspace` 验证）

- [x] 1.0 插件系统 + gRPC 统一通信基础设施
  **What**: 设计插件系统骨架（`Plugin` trait），将 kub/svc/jcli 三个工具重构为插件，统一注册到 dt CLI daemon 的 gRPC server 上。所有服务间通信走 gRPC（Neo4j 用 Bolt），消除 HTTP REST、自定义帧协议和 subprocess 调用。
  
  **Plugin trait 定义**（`crates/dt-plugins/src/lib.rs`）：
  ```rust
  #[async_trait]
  pub trait Plugin: Send + Sync + 'static {
      fn id(&self) -> &'static str;
      fn name(&self) -> &'static str;
      fn version(&self) -> &'static str;
      
      /// 注册 gRPC service 到 tonic Router
      fn register_grpc(&self, builder: tonic::transport::server::Router)
          -> Result<tonic::transport::server::Router, PluginError>;
      
      /// 初始化（在所有插件注册后、server 启动前调用）
      async fn init(&self, ctx: &PluginContext) -> Result<(), PluginError>;
      
      /// 健康检查（daemon 定期调用）
      async fn health(&self) -> Result<HealthStatus, PluginError>;
      
      /// 优雅关闭
      async fn shutdown(&self) -> Result<(), PluginError>;
  }
  
  pub struct PluginContext {
      pub neo4j: Arc<Neo4jClient>,     // Bolt 驱动
      pub qdrant: Arc<QdrantClient>,   // gRPC 驱动
      pub config: Arc<Config>,
      pub data_dir: PathBuf,           // 插件私有数据目录
  }
  ```

  **插件使用约束**（不可违反）：
  1. **禁止直接文件系统访问** — 所有路径操作走 `PluginContext` 提供的接口
  2. **禁止 subprocess 调用** — 所有外部交互通过 gRPC client traits（Neo4jClient/QdrantClient 等）
  3. **禁止阻塞 async runtime** — 所有 I/O 必须是 async，不允许 `std::thread::sleep` 或同步 IO
  4. **必须实现健康检查** — `health()` 返回值决定 daemon 是否标记该插件不可用
  5. **错误必须映射到 gRPC Status** — 插件内部不允许 `panic!`，所有错误走 `PluginError → tonic::Status`
  6. **proto 文件必须独立声明** — 每个插件的 gRPC service 定义在自己的 proto 文件中，不交叉引用

  **三个工具 → 三个插件**：
  ```
  kub ──重构──▶ plugin_k8s    (proto/plugin_k8s.proto)
  svc ──重构──▶ plugin_svc    (proto/plugin_svc.proto)
  jcli ──重构──▶ plugin_jenkins (proto/plugin_jenkins.proto)
  ```
  
  **Proto 结构**（`proto/` 目录）：
  ```
  proto/
  ├── common.proto           # 共享类型（HealthStatus, Error, Empty）
  ├── dt_core.proto          # dt CLI 核心 service（build/search/context/event/memorize/sync）
  ├── embed.proto            # dt-embed 嵌入 service
  ├── plugin_k8s.proto       # K8s 插件 service（pods/logs/download/status）
  ├── plugin_svc.proto       # 本地服务管理插件（list/start/stop/restart/logs/status）
  └── plugin_jenkins.proto   # Jenkins 插件（jobs/params/history/build/log）
  ```
  编译：Rust 用 `tonic-build`，Python 用 `grpcio-tools`
   
   **Safety Notice**：proto 文件新增 `proto/metrics.proto`、`proto/log.proto`（Phase 1.0b），`proto/common.proto` 增加 `HealthStatus` 扩展字段支持 metrics 快照。
  
  **dt CLI daemon 启动流程**：
  ```
  1. 加载 config.yaml
  2. 初始化 Neo4j Bolt client + 连接池
  3. 初始化 Qdrant gRPC client
  4. 构建 tonic Router
  5. 依次注册所有插件 → register_grpc()
  6. 并行调用所有插件 → init(ctx)
  7. 启动 gRPC server :50051
  8. 启动健康检查循环（每 30s 调所有插件的 health()）
  ```

  **架构变更**：
  ```
                         ┌──────────────────────────────────────┐
                         │       dt CLI daemon (Rust)           │
                         │       gRPC Server :50051             │
                         │                                      │
                         │  ┌──────────────────────────────┐    │
                         │  │        Plugin Registry        │    │
                         │  │  ┌────────┐ ┌──────┐ ┌─────┐ │    │
                         │  │  │plugin_ │ │plugin│ │plg_ │ │    │
                         │  │  │  k8s   │ │ _svc │ │jcli │ │    │
                         │  │  └───┬────┘ └──┬───┘ └──┬──┘ │    │
                         │  └──────┼─────────┼────────┼────┘    │
                         │         │         │        │         │
                         │  ┌──────┴─────────┴────────┴────┐    │
                         │  │      Host Client Traits       │    │
                         │  │  Neo4j(Bolt) Qdrant(gRPC)    │    │
                         │  └───────────────────────────────┘    │
                         └──────┬──────┬───────┬─────────────────┘
                 ┌──────────────┼──────┼───────┼──────────────┐
                 │ gRPC         │ gRPC │  Bolt │ gRPC          │
                 ▼              ▼      ▼        ▼              ▼
         ┌────────────┐ ┌────────────┐ ┌──────────┐ ┌──────────────┐
         │  dt-embed  │ │   Qdrant   │ │  Neo4j   │ │  MCP Server  │
         │  (Python)  │ │(gRPC:6334) │ │(Bolt:7687│ │  (Python)    │
         │ gRPC:50052 │ └────────────┘ └──────────┘ │ gRPC client  │
         └─────┬──────┘                             └──────┬───────┘
               │ in-process                                │ JSON-RPC
         ┌─────▼──────┐                             ┌──────▼───────┐
         │  BGE-M3    │                             │   OpenCode   │
         │  (GPU)     │                             │   (LLM)      │
         └────────────┘                             └──────────────┘
  ```

  **Files**: 
  - 创建 `proto/` 目录（全部 proto 定义）
  - 创建 `crates/dt-plugins/src/lib.rs` (Plugin trait 定义)
  - 创建 `crates/dt-plugins/src/registry.rs` (PluginRegistry + 生命周期)
  - 创建 `crates/dt-plugins/src/builtin/k8s/` (从 /data/myProject/kub 重构)
  - 创建 `crates/dt-plugins/src/builtin/svc/` (从 /data/myProject/svc 重构)
  - 创建 `crates/dt-plugins/src/builtin/jenkins/` (从 /data/myProject/jenkins-cli-rs 重构)
  - 创建 `crates/dt-daemon/src/server.rs` (tonic server + plugin 注册)
  - 创建 `crates/dt-storage/src/neo4j/client.rs` (Bolt 驱动 neo4rs)
  - 创建 `crates/dt-storage/src/qdrant/client.rs` (gRPC 驱动)
  - 重写 `crates/dt-pipeline/src/embedder.rs` (dt-embed gRPC client)
  - 修改 `python/mcp_server.py` (subprocess → gRPC client 调用 dt daemon)

  **Acceptance**:
  - `dt daemon` 启动后常驻后台（systemd socket activated），监听 :50051
  - `dt daemon --status` 显示所有插件和后端连接状态：
    ```
    插件                  版本      状态      延迟
    ──────────────────────────────────────────────
    dt_core              0.1.0    ✓ 正常     —
    plugin_k8s           0.1.0    ✓ 正常     12ms
    plugin_svc           0.1.0    ✓ 正常     5ms
    plugin_jenkins       0.1.0    ✓ 正常     8ms
    ──────────────────────────────────────────────
    后端                  状态      延迟
    Neo4j (Bolt:7687)    ✓ 正常    8ms
    Qdrant (gRPC:6334)   ✓ 正常    5ms
    dt-embed (gRPC:50052) ✓ 正常   3ms
    ```
  - MCP Server 通过 gRPC 调用所有工具（无 subprocess）
    - 原 `kublog_status` → gRPC `K8sService.GetPods`
    - 原 `svc_list` → gRPC `SvcService.ListServices`
    - 原 `jcli_list` → gRPC `JenkinsService.ListJobs`
  - 三个原独立 CLI 工具仍可独立运行（`kub`/`svc`/`jcli` 二进制保留），但它们现在是 dt CLI workspace 的成员
  - Neo4j 走 Bolt 驱动（连接池、事务重试、自动重连）
  - Qdrant 走 gRPC streaming（大批量向量写入）
  - dt-embed 提供 gRPC EmbedService
  - 所有 HTTP REST 端点从 dt CLI 代码中移除
  - 插件违反约束时编译报错（trait 方法签名强制 async、强制返回 Result、禁止 `std::process::Command`）
  - **硬性通信协议要求（不可降级）**：
    - ✅ dt-embed 必须提供 gRPC `EmbedService` (:50052)，代码库中不得残留 Unix Socket 帧协议代码
    - ✅ Qdrant 必须使用 gRPC 驱动 (`qdrant-client` crate)，不得残留 HTTP REST 调用
    - ✅ MCP Server 必须通过 gRPC 调用 dt daemon (:50051)，不得使用 subprocess 解析 stdout
    - ✅ Neo4j 走 Bolt 原生协议（合理例外，neo4rs crate）

- [x] 1.0b 统一日志系统 + gRPC 指标监控（`dt-log` crate + LogService + MetricsService）
  **What**: 所有组件通过统一的日志管道输出，单一聚合文件可追踪跨进程调用链。同时内置指标采集，通过 gRPC MetricsService 暴露（不开启 HTTP 端口）。日志系统覆盖 dt daemon、所有插件、dt-embed (Python) 和 MCP Server (Python) 四个进程。

  **日志格式**（结构化 JSON，一行一条）：
  ```json
  {"ts":"2026-07-09T14:30:00.123Z","level":"INFO","target":"dt::build","trace_id":"a1b2c3","span_id":"d4e5f6","plugin":"dt_core","path":"/data/...","message":"Starting project build","methods":312,"elapsed_ms":4200}
  ```

  **必选字段**（所有组件统一）：
  | 字段 | 类型 | 说明 |
  |------|------|------|
  | `ts` | RFC3339 | 时间戳，精确到毫秒 |
  | `level` | enum | TRACE / DEBUG / INFO / WARN / ERROR |
  | `target` | string | 日志来源模块，如 `dt::build`, `plugin_k8s::logs`, `dt_embed` |
  | `trace_id` | string | 全链路追踪 ID，跨进程通过 gRPC metadata `x-trace-id` 传递 |
  | `message` | string | 人类可读日志消息 |

  **可选字段**：`span_id`, `plugin`, `error`（错误详情+堆栈）, 业务自定义字段（如 `methods`, `elapsed_ms`）

  **日志输出目标**：
  | 目标 | 用途 |
  |------|------|
  | `/var/log/digital-twin/dt-daemon.log` | 主日志文件，每日轮转，保留 30 天 |
  | systemd journal | `journalctl -u dt-daemon -f` 实时查看 |
  | gRPC LogService stream | `dt logs --follow` 命令行实时查看（未来 MCP 工具） |

  **Rust 侧实现（`dt-log` crate）**：
  ```rust
  // 基于 tracing + tracing-subscriber
  // Cargo.toml: dt-log = { path = "crates/dt-log" }
  
  use dt_log::{info, warn, error, debug, instrument};

  #[instrument(skip(ctx), fields(project = %name))]
  async fn build_project(ctx: &Context, name: &str) -> Result<usize> {
      info!("Starting project build");
      let methods = do_build(ctx, name).await?;
      info!(methods = methods, "Build complete");
      Ok(methods)
  }
  // 自动产出：
  // {"ts":"...","level":"INFO","target":"dt::build","trace_id":"...","span_id":"...",
  //  "project":"aflm-pay","message":"Starting project build"}
  // {"ts":"...","level":"INFO","target":"dt::build","trace_id":"...","span_id":"...",
  //  "project":"aflm-pay","message":"Build complete","methods":312}
  ```

  **gRPC LogService**（dt daemon 内置）：
  ```protobuf
  // proto/log.proto
  service LogService {
      // 外部进程流式上报日志到 daemon
      rpc StreamLogs(stream LogEntry) returns (LogAck);
      // 查询历史日志（供 dt logs 命令）
      rpc QueryLogs(LogQuery) returns (stream LogEntry);
  }
  
  message LogEntry {
      string timestamp = 1;       // RFC3339
      string level = 2;           // TRACE|DEBUG|INFO|WARN|ERROR
      string target = 3;          // 来源模块
      string trace_id = 4;        // 全链路追踪 ID
      string span_id = 5;         // span ID
      string plugin = 6;          // 插件名称（dt_core|plugin_k8s|plugin_svc|plugin_jenkins）
      string message = 7;         // 日志消息
      string error = 8;           // 错误详情（JSON）
      map<string, string> fields = 9;  // 业务自定义字段
  }
  ```

  **Python 侧（dt-embed / MCP Server）**：
  ```python
  # dt_log.py — Python 日志桥接模块
  import logging, grpc
  from dt_log_pb2 import LogEntry
  from log_pb2_grpc import LogServiceStub

  class GrpcLogHandler(logging.Handler):
      """将 Python logging 记录流式发送到 dt daemon 的 LogService"""
      def __init__(self, daemon_addr="localhost:50051"):
          self.stub = LogServiceStub(grpc.aio.insecure_channel(daemon_addr))
          self._stream = None  # 延迟创建 gRPC stream
      
      async def emit(self, record: logging.LogRecord):
          entry = LogEntry(
              timestamp=datetime.utcnow().isoformat() + "Z",
              level=record.levelname,
              target=record.name,
              trace_id=get_trace_id(),  # 从 gRPC metadata 提取
              message=record.getMessage(),
          )
          # 流式发送到 daemon
  ```

  **PluginContext 集成**：
  ```rust
  pub struct PluginContext {
      // ... 已有字段
      pub log: dt_log::PluginLogger,  // 插件命名空间隔离的 logger
  }
  // 插件中使用：
  // ctx.log.info!("Pod {} status: {}", pod_name, status);
  // → {"ts":"...","level":"INFO","target":"plugin_k8s::status","plugin":"plugin_k8s",...}
  ```

  **Files**:
  - 创建 `crates/dt-log/` (Cargo.toml, src/lib.rs — tracing 封装)
  - 创建 `proto/log.proto` (LogService 定义)
  - 创建 `crates/dt-daemon/src/log_service.rs` (LogService 实现)
  - 创建 `python/dt_log.py` (Python gRPC log handler)
  - 修改 `crates/dt-daemon/Cargo.toml` (添加 dt-log + tracing 依赖)
  - 修改 `crates/dt-daemon/src/main.rs` (初始化 tracing-subscriber)
  - 修改 `crates/dt-plugins/src/registry.rs` (PluginContext 添加 log 字段)
  - 修改 `python/mcp_server.py` (使用 GrpcLogHandler)
  - 修改 `services/embed-server/cli.py` (使用 GrpcLogHandler)
  - 修改 `crates/dt-plugins/src/builtin/*/` (所有 println! 改为 dt_log 宏)

  **Acceptance**:
  - `dt daemon` 启动后 `/var/log/digital-twin/dt-daemon.log` 开始写入
  - 日志格式统一为单行 JSON，所有字段一致
  - 一次 `dt build` 调用的完整过程（扫描→解析→嵌入→写入）通过相同的 `trace_id` 串联
  - `journalctl -u dt-daemon -f` 实时看到结构化日志
  - dt-embed Python 进程的日志出现在同一个 log 文件中（通过 gRPC StreamLogs 上报）
  - MCP Server Python 进程的日志同上
  - `dt daemon --status` 显示 LogService 状态和最近日志条目数
  - 插件中无残留 `println!` / `eprintln!`，全部走 `ctx.log`
  - 日志文件每日 00:00 轮转，保留最近 30 天
  - ERROR 级别日志自动附带堆栈（Rust: `error!(error = %e, "msg")`，Python: `logger.exception()`）
  - `dt logs --level ERROR --since 1h` 可查询最近的错误日志（通过 gRPC QueryLogs）

  **gRPC MetricsService**（不暴露 HTTP 端口，复用 gRPC :50051）：
  ```protobuf
  // proto/metrics.proto
  service MetricsService {
      rpc GetMetrics(MetricsRequest) returns (MetricsResponse);
      rpc WatchMetrics(MetricsRequest) returns (stream MetricSnapshot);
  }

  message MetricSnapshot {
      string timestamp = 1;         // RFC3339
      map<string, double> gauges = 2;    // 瞬时值
      map<string, double> counters = 3;  // 累计值
      map<string, Histogram> histograms = 4;
  }
  ```

  **内置指标**（通过 `tracing` span 自动采集 + 手动 gauge/counter）：
  ```
  dt_build_duration_seconds{project, strategy}      histogram
  dt_embed_requests_total{status}                   counter
  dt_embed_queue_depth                               gauge
  dt_neo4j_connection_pool_size                      gauge
  dt_qdrant_write_bytes_total                        counter
  dt_plugin_health_status{plugin}                    gauge (0/1)
  dt_context_total_duration_seconds                  histogram
  dt_context_world_query_duration{world}             histogram
  dt_write_coordinator_active_locks                  gauge
  ```

  **查询方式**：
  ```bash
  dt metrics                          # 一次性快照
  dt metrics --watch --interval 5s    # 流式输出到终端
  ```
  指标同时以结构化日志形式每 60s 写入一次 snapshot 到 log 文件（供 log 聚合工具消费）。

  **实现**：dt-log crate 增加 `metrics` feature flag，基于 `tracing` 的 span 自动统计耗时，`counter!`/`gauge!` 宏手动埋点。无额外 HTTP 端点。

  **Files**（增量）：
  - 创建 `proto/metrics.proto`
  - 创建 `crates/dt-daemon/src/metrics_service.rs`
  - 修改 `crates/dt-log/src/lib.rs`（增加 metrics 宏：`counter!`, `gauge!`, `histogram!`）
  - 修改 `crates/dt-daemon/src/server.rs`（注册 MetricsService）
  - 修改 `crates/dt-daemon/Cargo.toml`（dt-log = { features = ["metrics"] }）

  **Acceptance**（增量）：
  - `dt metrics` 输出当前所有 gauge/counter/histogram 快照
  - `dt daemon --status` 显示 MetricsService 状态
  - 一次 `dt build` 后 `dt_build_duration_seconds` histogram 有数据
  - 指标以 JSON 行格式每 60s 写入日志文件（tag: `_metric_snapshot`）
  - 无 HTTP 端口暴露

- [x] 1.0c 安全与鉴权（SecretString + gRPC mTLS + 权限分级）
  **What**: 全链路安全加固。敏感凭证加密存储，gRPC 通信加密，破坏性操作权限控制。

  **SecretString 类型**（`dt-common/src/config.rs`）：
  ```rust
  enum SecretString {
      Env(String),     // "env:NEO4J_PASSWORD"
      Vault(String),   // "vault:secret/neo4j"
      Plain(String),   // 明文（仅 dev，生产拒绝启动）
  }
  ```
  - `SecretString::resolve()` 首次访问时解析，`Debug`/`Display` 输出为 `"***"`
  - config.yaml 中所有 auth_password 改为 `"env:DT_NEO4J_PASSWORD"` 格式

  **gRPC 三层认证**：
  ```
  外部调用者              dt daemon (:50051)         权限
  ─────────────────────────────────────────────────────
  OpenCode MCP Server ──▶  Unix Socket             全部（本地信任）
  用户 CLI (dt xxx)   ──▶  Unix Socket             全部（本地信任）
  远程/外部系统        ──▶  mTLS + JWT bearer       只读（拒绝 dt clean）
  ```

  **实现**：
  - tonik interceptor 检查 peer address：Unix socket 来源 → `AdminRole`；网络来源 → `ReadOnlyRole`
  - `dt clean` / `dt schema drop` 等破坏性操作仅 `AdminRole` 可调
  - config.yaml 新增 `security` 段：
    ```yaml
    security:
      tls:
        ca_cert: "/etc/dt/ca.pem"
        server_cert: "/etc/dt/server.pem"
        server_key: "/etc/dt/server.key"
      jwt_secret: "env:DT_JWT_SECRET"
    ```

  **Files**:
  - 修改 `crates/dt-common/src/config.rs`（新增 SecretString 类型）
  - 创建 `crates/dt-daemon/src/auth.rs`（tonic interceptor: role extraction）
  - 修改 `crates/dt-daemon/src/server.rs`（注册 auth interceptor）
  - 修改 `config/config.example.yaml`（增加 security 段）

  **Acceptance**:
  - `config.yaml` 中无明文密码，全部走 `env:` 前缀
  - 日志输出中 SecretString 字段显示为 `***`
  - gRPC Unix socket 连接可调所有操作
  - gRPC 网络连接调 `dt clean` 返回 `PERMISSION_DENIED`
  - 日志中自动脱敏：`password: "***"`

- [x] 1.1 Schema 初始化 + 数据保留策略
  **What**: 在新 clean database 中创建所有 V2 约束、索引、节点标签。同时定义数据 TTL 和自动清理规则。
  **Files**: 新建 `crates/dt-storage/src/neo4j/schema.rs`，修改 `crates/dt-storage/src/neo4j/repo.rs`
  **Acceptance**:
  - 执行后 `CALL db.constraints()` 列出所有 V2 约束（method_id, class_id, module_id, server_id, database_id, config_id, endpoint_id, doc_id, service_id, knowledge_id, playbook_id, experience_id, concept_id, domain_id, day_id, session_id, modification_id, deploy_id, change_id, fix_id, decision_id, observation_id, analysis_id, thread_id, requirement_id）
  - `CALL db.indexes()` 列出全文索引和向量索引
  - 无 V1 残留标签（Infrastructure, Event, Project 等旧的 flat 标签）
  - `dt build --path <project>` 写入的节点使用新标签（Method/Class/Module，不是旧的 Method/Class）

  **数据保留策略**（内置到 Schema 初始化脚本）：
  | 数据 | TTL | 超过后 |
  |------|-----|--------|
  | Memory.Event | 365 天 | 归档到 `/var/lib/dt/archive/` |
  | Reasoning (未验证) | 会话结束 | SET `_stale_at = timestamp()`（立即标记为过期）；`dt cleanup` 每夜 DELETE 所有 `_stale_at` 距今 > 30 天的节点 |
  | Reasoning (已验证) | 永久 | 升级为 Knowledge 或 Memory |
  | SQLite snapshot 旧行 | 仅保留最新 | `dt build` 后自动 `DELETE WHERE updated_at < latest_per_file` |
  | Qdrant orphan points | 跟随 Neo4j | entity 被删 → 对应 point 被 `dt kg-sync` 清理 |

  **自动化清理**：
  ```bash
  dt cleanup --dry-run    # 预览即将清理的数据
  dt cleanup --execute    # 执行清理
  # crontab: 0 4 * * 0 dt cleanup --execute   # 每周日凌晨自动清理
  ```

- [x] 1.2 `dt build` 重写为 V2 Schema
  **What**: 修改 `crates/dt-pipeline/src/builder.rs` 和 Neo4j 写入逻辑，产出 V2 Method/Class/Module 节点。
  **Files**: 修改 `crates/dt-pipeline/src/builder.rs`, `crates/dt-pipeline/src/pipeline.rs`, `crates/dt-storage/src/neo4j/repo.rs`, `crates/dt-common/src/types.rs`
  **Acceptance**:
  - `dt build --path /data/myProject/aflm-pay --name aflm-pay` 完成后：
  - `MATCH (m:Method) RETURN count(m)` > 0
  - `MATCH (c:Class) RETURN count(c)` > 0
  - `MATCH (c:Class)-[:CONTAINS]->(m:Method) RETURN count(*)` > 0
  - `MATCH (m1:Method)-[:CALLS]->(m2:Method) RETURN count(*)` > 0
  - `MATCH (m:Module) RETURN count(m)` > 0
  - 每个 Method 节点的属性完全符合 V2 spec（method_id, name, signature, params, return_type, class_name, file_path, package_or_module, language, project, start_line, end_line, calls, comment）
  - Module 节点的 module_id 格式为 `dt://entity/{project}/module/{name}`

- [x] 1.0e 并发写入协调器（WriteCoordinator）
  **What**: 三个写入源（OpenCode Hook → `dt update`、用户手动 `dt build`、cron `nacos-sync`/`k8s-sync`）可能并发写 Neo4j/Qdrant。WriteCoordinator 提供文件级锁和全局串行化选项。本组件属于基础设施层，必须在所有后续写入管线（1.3 nacos-sync、1.5 dt update 等）之前完成。

  **设计**：
  ```rust
  // crates/dt-pipeline/src/coordinator.rs
   pub struct WriteCoordinator {
       // 同一文件不能并发写（按 file_path 分片锁）
       file_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
       // 同一实体不能并发写（按 entity_id 分片锁，如 "file:/path/to/X.java", "nacos:ns/data_id"）
       entity_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
       // 全局写串行化（可选，默认关闭）
       global_lock: Option<tokio::sync::RwLock<()>>,
   }
  ```

  **集成方式**：CoordinatedBuildService / CoordinatedSyncService decorator wrapper。
  **Files**: 创建 `crates/dt-pipeline/src/coordinator.rs`，修改 `crates/dt-daemon/src/wiring.rs`

  **Acceptance**:
  - 两个并发 `dt update --file <same_file>` 串行执行，不产生重复节点
  - `dt build --path <project>` 与 `dt nacos-sync` 并行时无数据错乱
  - `dt watch` 触发的大量文件变更被正确排队

- [x] 1.3 `dt nacos-sync` 重写为 V2 Schema
  **What**: 修改 `crates/dt-sync/src/nacos/config_sync.rs`，产出 V2 NacosConfig/ConfigKey/Service/NacosService 节点。
  **Files**: 修改 `crates/dt-sync/src/nacos/config_sync.rs`, `crates/dt-sync/src/nacos/service_sync.rs`
  **Acceptance**:
  - `dt nacos-sync --env test` 完成后：
  - `MATCH (n:NacosConfig) RETURN count(n)` > 0
  - `MATCH (k:ConfigKey) RETURN count(k)` > 0，且 k.name 包含如 `spring.datasource.url`
  - `MATCH (n:NacosConfig)-[:CONTAINS]->(k:ConfigKey) RETURN count(*)` > 0
  - `MATCH (s:Service) RETURN count(s)` > 0，且 s.service_id 格式为 `dt://service/{name}`
  - NacosConfig.config_id 格式为 `dt://nacos/{ns}/{data_id}`
  - NacosConfig.content_hash 已计算 SHA256

- [x] 1.4 `dt k8s-sync` 重写为 V2 Schema
  **What**: 修改 `crates/dt-sync/src/k8s/resource_sync.rs`，产出 V2 Server/K8sDeployment/K8sService 节点（K8sPod 不入 Neo4j — 走 Runtime）。
  **Files**: 修改 `crates/dt-sync/src/k8s/resource_sync.rs`
  **Acceptance**:
  - `dt k8s-sync` 完成后：
  - `MATCH (s:Server) RETURN count(s)` > 0
  - `MATCH (s:Server {hostname: ...})` 节点有 server_id, name, hostname, environment 等属性
  - K8s 实体（K8sDeployment, K8sService）节点按 V2 spec 创建（K8sPod 不属于 Neo4j — 走 Runtime 实时查询）
  - NacosService ↔ K8sService 交叉链接正确

- [x] 1.5 `dt update` 单文件增量更新
  **What**: 新增 `crates/dt-pipeline/src/updater.rs` 实现单文件实时增量逻辑，替代当前 `dt build --file` 的简化流程。
  **Files**: 创建 `crates/dt-pipeline/src/updater.rs`，修改 `crates/dt-pipeline/src/lib.rs`, `crates/dt-daemon/src/main.rs` 的命令注册
  **Acceptance**:
  - `dt update --file /data/myProject/aflm-pay/src/.../PayService.java` 执行后：
  - 该文件旧 Method 节点从 Neo4j + Qdrant 被删除
  - 新 Method 节点被 upsert
  - SQLite 快照更新（file_sha1 + mtime）
  - 返回变更摘要（`1 file, 5 methods updated, 2 classes`）
  - 幂等：重复执行不产生重复节点

- [x] 1.6 `dt watch` 文件监视 daemon
  **What**: 新增文件监视守护进程，用 inotify 监听 config.yaml 中所有 projects 的源码变更。
  **Files**: 创建 `crates/dt-pipeline/src/watcher.rs`，修改 `crates/dt-daemon/src/main.rs`
  **Acceptance**:
  - `dt watch` 启动后常驻后台
  - 在任意项目中 `echo '// test' >> SomeService.java` 后，`dt update --file <path>` 被自动触发
  - `dt watch --status` 显示 daemon 运行状态和监听的目录数
  - `dt watch --stop` 安全退出

- [x] 1.7 Schema 验证与数据清理
  **What**: 提供一键 clean 脚本，验证所有 V1 数据已清除，并执行 schema 初始化。
  **Files**: 创建 `crates/dt-storage/src/neo4j/schema.rs`（含 clean 逻辑），修改 `crates/dt-daemon/src/main.rs`
  **Acceptance**:
  - `dt clean --confirm` 清空所有 Neo4j 节点和关系
  - `dt clean --confirm` 清空所有 Qdrant Collections
  - `dt clean --confirm` 重置 SQLite 快照
  - `dt schema init` 创建所有约束和索引
  - `dt health` 显示干净环境状态

**🔵 里程碑 M1 — "Hello World 管线"**（Phase 1 完成）
- ✅ `dt daemon --status` 显示所有插件+后端绿灯 + 安全鉴权 + 指标
- ✅ `dt build` 一个 Java 项目 → `MATCH (m:Method) RETURN count(m)` > 0
- ✅ `dt nacos-sync --env test` → `MATCH (n:NacosConfig) RETURN count(n)` > 0
- ✅ `dt update --file` 单文件增量更新幂等
- ✅ `dt watch` daemon 监听文件变更正常触发
- ✅ 并发写入时 WriteCoordinator 无数据错乱

---

### Phase 2: Memory + Knowledge Worlds

**目标**：实现 Memory World 时间线（Day→Session→Event）和 Knowledge World（Knowledge/Playbook/Experience/Concept/Domain）的写入与查询。

**依赖**：Phase 1（需要 Reality World 实体存在才能关联 AFFECTS/IMPLEMENTED_BY 等关系）

**预估工作量**：L

---

- [x] 2.1 Memory World 核心结构：Day → Session → Event
  **What**: 实现 Day/Session 节点的自动创建，以及 Event 链的维护。
  **Files**: 修改 `crates/dt-knowledge/src/memory/service.rs`，创建 `crates/dt-knowledge/src/memory/entities.rs`
  **Acceptance**:
  - `dt event --type Conversation --entity-id "2026-07-09-test" --entity-type Session --project digital-twin --details "test session"` 执行后：
  - `MATCH (d:Day {day_id: "2026-07-09"}) RETURN d` 存在（首次自动创建）
  - `MATCH (d:Day)-[:HAS_SESSION]->(s:Session) RETURN s.session_id, s.summary` 返回对应 Session
  - 同一天多个 Session 正确链到同一个 Day 节点
  - Session 节点属性：session_id, summary, key_decisions, thread_id, started_at, ended_at

- [x] 2.2 Memory Event 类型重写（Modification / Deployment / ConfigChange / BugFix / Decision）
  **What**: 重写 `dt_event` 的 Neo4j 写入逻辑，每种事件类型创建对应的 V2 节点标签和关系。
  **Files**: 修改 `crates/dt-knowledge/src/memory/dispatcher.rs`
  **Acceptance**:
  - `dt event --type Deploy --entity-id "aflm-pay-deploy-test" --entity-type JenkinsJob --project aflm --details "branch: master, env: test, version: v2.3.1"` 执行后：
    - `MATCH (d:Deployment {deploy_id: ...}) RETURN d.job, d.env, d.version` 返回正确值
    - `MATCH (d:Deployment)-[:DEPLOYS]->(si:ServiceInstance) RETURN si.host, si.port, si.environment` 返回关联服务实例
  - `dt event --type ConfigChange --entity-id "pay-datasource.yml" --entity-type NacosConfig --project aflm --details "key: spring.datasource.url, old: jdbc://10.0.1.50, new: jdbc://10.0.2.50"` 执行后：
    - `MATCH (c:ConfigChange)-[:AFFECTS]->(n:NacosConfig) RETURN c.key, c.old_value, c.new_value` 返回正确值
  - Modification/BugFix 节点同理验证

- [x] 2.3 Auto-Event 触发集成（OpenCode Hook + AI 自动写入）
  **What**: 确保 OpenCode Hook (`tool.execute.after`) 触发代码修改时自动创建 Modification Event。
  **Files**: 修改 `.opencode/plugins/dt-build.js`，修改 `python/mcp-server.py` 的 post-execute 逻辑
  **Acceptance**:
  - AI edit 一个 Java 文件 → `dt update --file <path>` 被调用 → 自动创建 `(:Modification {file, diff_summary})` 节点
  - Jenkins 部署触发 → `(:Deployment)` 自动创建
  - 会话结束 → `(:Session {summary, key_decisions})` 自动创建

- [x] 2.4 Knowledge World 核心实体：Knowledge / Experience / Concept / Domain / Playbook
  **What**: 重写 `dt memorize` 产出 V2 Knowledge 子图（Knowledge, Experience, Concept, Domain, Playbook 节点）。
  **Files**: 修改 `crates/dt-knowledge/src/knowledge/service.rs`
  **Acceptance**:
  - `dt memorize --type Decision --entity-id "pay-migration-choice" --entity-type ArchitectureDecision --project aflm --details "decision: 选择银盛; reason: 费率低0.1%; scope: PayService, BusinessService"` 执行后：
    - `MATCH (k:Knowledge {knowledge_id: ...}) RETURN k.name, k.domain, k.content, k.confidence` 返回正确值
    - Knowledge 节点属性：knowledge_id, name, title, domain, summary, content, definition, source, project, confidence, verified_by, created_at, updated_at
  - `dt memorize --type KnowledgeAdded --entity-id "pitfall-channelExtra" --entity-type Experience --project aflm --details "title: 别忘了改channelExtra; severity: warning; domain: 支付"` 执行后：
    - `MATCH (e:Experience {experience_id: ...}) RETURN e.title, e.severity, e.domain` 返回正确值
  - Concept 节点属性包含 concept_id, name, definition, domain, summary
  - Domain 节点：`MATCH (d:Domain) RETURN d.name, d.description` 返回非空
  - Playbook 节点：steps 字段为 JSON 数组，每个 step 有 order/action/tool/target/expected/pitfall

  **Knowledge 版本管理**（内置到写入逻辑）：
  - 每次更新 Knowledge 节点不是 UPDATE，而是：
    ```cypher
    CREATE (k2:Knowledge {knowledge_id: $id_v2, version: 2, ...})
    CREATE (k2)-[:EVOLVED_FROM]->(k1)
    CREATE (:KnowledgeVersion {
        version_id: $vid, knowledge_id: $id_v2,
        version: 2, diff: "新增 pitfall: ...",
        timestamp: $ts, session_id: $sid
    })
    ```
  - Context Builder 查询时默认取最新版本（无 `EVOLVED_FROM` 入边的节点）
  - Playbook 的 `success_count` 每次执行后递增；成功率低于 70% → `SET _needs_review = true`
  - 版本链可追溯："此知识已更新 3 次，最近一次在 2026-07-09"

- [x] 2.5 `@knowledge` 代码注释提取
  **What**: `dt build` AST 解析时识别 Java / TypeScript / Python / Go / Rust / PHP / JavaScript 中的 `@knowledge` 标记，自动创建 Knowledge/Concept 节点。
  **Files**: 修改 `crates/dt-pipeline/src/parser/mod.rs`，修改 `crates/dt-pipeline/src/pipeline.rs`
  **Acceptance**:
  - 代码中包含 `@knowledge domain="支付" concept="ifCode"` 注释
  - `dt build` 后自动创建 `(:Concept {name: "ifCode", definition: "...", domain: "支付"})`
  - `MATCH (c:Concept)-[:IMPLEMENTED_BY]->(m:Method) RETURN c.name, m.name` 返回对应方法

- [x] 2.6 `dt_learn` 基础版（高层 Knowledge 写入）
  **What**: 实现 `dt_learn` CLI 命令和 MCP 工具，接收 task/entities/pattern/pitfalls/decisions，批量写入 Knowledge World。
  **Files**: 创建 `crates/dt-knowledge/src/learn.rs`，修改 `crates/dt-daemon/src/main.rs`, `python/mcp-server.py`
  **Acceptance**:
  - `dt learn --task "支付平台迁移" --entities "PayService,BusinessService,pay-channel.yml" --pattern "ifCode+wayCode+merchantNo+DB" --pitfalls "别忘了channelExtra,回调地址要改" --success true` 执行后：
    - `MATCH (k:Knowledge {name: "支付平台迁移模式", domain: "支付"}) RETURN k` 存在
    - `MATCH (e:Experience {title: "别忘了channelExtra"}) RETURN e` 存在
    - `MATCH (p:Playbook) RETURN p.steps` 非空
    - Playbook 从 pattern 和 pitfalls 自动合成 steps

> **实现机制**：`dt learn` 自身是 Rust CLI，不具备 LLM 推理能力。Step 合成通过以下方式实现：
> 1. `dt learn --pattern "ifCode+wayCode+DB"` → 将 pattern 字符串解析为关键词列表
> 2. Rust 侧生成结构化的 `dt_learn` MCP 请求，委托给 MCP Server
> 3. MCP Server 调用 LLM 将 pattern + pitfalls 转化为 `[{order, action, tool, target, expected}]` 格式的 Step 列表
> 4. 生成的结构化 Step 通过 gRPC 回写 Neo4j Playbook 节点
> 
> 这是 `dt learn` 中唯一需要 LLM 辅助的环节——其余操作（Neo4j 写入、关系建立）均可在 Rust 侧完成。

- [x] 2.7 执行结果自动采集（Knowledge 来源 5）
  **What**: AI 执行 bash 等工具后，MCP Server 自动判断返回值是否有长期价值，有则写入 Knowledge World。黑名单跳过临时命令（ls/cat/echo/cd/pwd/grep/find）。
  **Files**: 修改 `python/mcp-server.py` 的 post-execute 逻辑，修改 `crates/dt-knowledge/src/learn.rs`
  **Acceptance**:
  - AI 执行 `mysql -e "show create table pay_order"` → 自动创建 `(:Knowledge {source: "execution_result"})` 含表结构 DDL
  - AI 执行 `docker inspect <container>` → 沉淀为 `(:Config)`
  - AI 执行 `ls /tmp` → 不触发采集（黑名单）
  - 同一实体的重复执行结果 → oversert（by entity_id 去重）

**🔵 里程碑 M2 — "记忆与知识"**（Phase 2 完成）
- ✅ `dt event --type Conversation` → 自动创建 Day → Session → Event 时间线
- ✅ AI 改代码 → 自动创建 `(:Modification)` 节点 + `[:AFFECTS]` 关系
- ✅ `dt memorize` → 写入 Knowledge/Playbook/Experience/Concept/Domain
- ✅ Knowledge 版本链可追溯（`MATCH (k)-[:EVOLVED_FROM*]->(old)`）
- ✅ `dt learn` → 批量写入模式 + pitfalls
- ✅ AI 执行 bash 工具 → 有价值结果自动沉淀为 Knowledge

---

### Phase 3: Semantic + Reasoning Worlds

**目标**：补全 Semantic World（文档/配置/API/经验/日志向量化管线）和 Reasoning World（Decision Graph 生命周期）。

**依赖**：Phase 1（全部）+ Phase 2（仅 3.3-3.6 需要 Knowledge/Experience 实体存在）

**预估工作量**：L

---

- [x] 3.1 Semantic World：Qdrant Collection 体系建立
  **What**: 为每个项目创建标准 Qdrant Collection 矩阵（methods/semantic/kg_nodes），为 Semantic World 每类文本建立独立集合。
  **Files**: 修改 `crates/dt-storage/src/qdrant/repo.rs`，创建 `crates/dt-storage/src/qdrant/collection.rs`
  **Acceptance**:
  - 执行 `dt build --path /data/myProject/aflm-pay --name aflm-pay` 后：
  - Qdrant collection `aflm-pay_methods` 存在且有向量
  - Qdrant collection `aflm-pay_semantic` 存在（文档向量）
  - 每个 vector point 的 payload 包含 `entity_id` 字段（可反查 Neo4j）
  - `dt search "支付回调" --path /data/myProject/aflm-pay --limit 5` 返回方法列表（与当前一致，但 entity_id 指向新 schema）

  **嵌入模型迁移路径**（内置到 Collection 命名）：
  - Qdrant collection 命名从 `{project}_methods` 改为 `{project}_methods_{model_version}`
  - `config.yaml` 中 `embed.model_version: "bge-m3-v1"` 驱动命名
  - 未来换模型时：
    ```bash
    # 1. 部署新 embed server（新模型）
    # 2. 修改 config.yaml model_version → 新建 collection 前缀
    # 3. dt build --reindex   # 重新生成所有向量
    # 4. 验证后旧 collection 保留 30 天可回滚
    ```
  - Collection 命名由 `VectorRepository` trait 内部拼接，上层无感知

- [x] 3.2 文档解析管道
  **What**: `dt build` 扫描 document_dirs 时，提取 md/pdf/txt 文档，chunk 分块，向量化，写入 Neo4j Document 节点 + Qdrant。
  **Files**: 修改 `crates/dt-pipeline/src/parser/mod.rs`（新增文档解析模块），修改 `crates/dt-pipeline/src/pipeline.rs`
  **Acceptance**:
  - 在项目 `docs/` 目录放置一个 `.md` 文件
  - `dt build --path <project> --name <project>` 后：
  - `MATCH (d:Document) RETURN d.name, d.title, d.summary, d.doc_type` 返回文档节点
  - Qdrant `{project}_semantic` collection 中有对应的 vector points（payload 含 chunk_id, doc_name, text）
  - 文档节点有 `(:Document)-[:DESCRIBES]->(:Concept)` 关系（如果能从内容中提取概念）

  **文档 Chunking 策略**（`dt-pipeline/src/chunker.rs`）：
  ```rust
  struct ChunkConfig {
      chunk_size: usize,       // 512 tokens
      overlap: usize,           // 64 tokens (12.5%)
      boundary: Boundary,       // Paragraph 优先，其次 Sentence，最后固定长度
      min_chunk_size: usize,    // 256 tokens（低于此合入前一个 chunk）
  }
  ```
  - 每个 chunk 的 Qdrant payload 增加 `chunk_index`、`prev_chunk_id`、`next_chunk_id`
  - 检索时支持上下文扩展：命中 chunk-3 → 可扩展返回 chunk-2 + chunk-3 + chunk-4

- [x] 3.3 Config/API/Experience/Log Pattern 向量化
  **What**: nacos-sync 后 ConfigKey 值自动向量化；dt_build 提取的 API Endpoint 描述向量化；Experience 写入时向量化；从 K8s 日志提取错误模式进入 Qdrant。
  **Files**: 修改 `crates/dt-sync/src/nacos/config_sync.rs`（增加向量化步骤），修改 `crates/dt-pipeline/src/embedder.rs`
  **Acceptance**:
  - nacos-sync 后，Qdrant 中有 config value 的向量（payload 含 key, value, namespace）
  - dt_build 后，Qdrant 中有 Endpoint 的向量（payload 含 method, path, description, controller）
  - dt_learn 写入 Experience 后，Qdrant 同步有对应向量
  - Log pattern 提取：`kublog_logs` 输出被解析出 ERROR 模板，进入 Qdrant pattern 集合

- [x] 3.4 KG→Qdrant 桥接重写
  **What**: `dt kg-sync` 改为同步所有业务标签节点（Server, Database, NacosConfig, Service, Knowledge, Experience, Concept 等），而非旧的 Infrastructure 等标签。
  **Files**: 修改 `crates/dt-sync/src/kg_bridge.rs`
  **Acceptance**:
  - `dt kg-sync --incremental` 只同步 `_kg_synced_at IS NULL` 的节点
  - 同步后 `dt search-kg "支付数据库"` 能搜索到 Database/NacosConfig 节点
  - `dt search-kg "支付平台迁移"` 能搜索到 Knowledge/Experience 节点
  - 全量 `dt kg-sync` 覆盖所有业务标签节点

- [x] 3.5 Reasoning World：Observation → Analysis → Decision
  **What**: 实现 Decision Graph 的写入、查询和生命周期管理。
  **Files**: 创建 `crates/dt-knowledge/src/reasoning/service.rs`，修改 `crates/dt-daemon/src/main.rs`
  **Acceptance**:
  - `dt reason observe --description "Module A 和 Module B 结构高度相似" --evidence "都有 payChannel, merchant, callback 三层" --entities "ModuleA,ModuleB" --confidence 0.7` 执行后：
    - `MATCH (o:Observation {observation_id: ...}) RETURN o.description, o.evidence, o.confidence` 返回正确值
    - `MATCH (o:Observation)-[:ABOUT]->(m:Module) RETURN m.name` 返回关联模块
  - `dt reason analyze --question "切换支付平台影响哪些文件？" --hypothesis "影响 PayService 和 BusinessService" --conclusion "需改 5 处" --confidence 0.9` 执行后：
    - `MATCH (a:Analysis)-[:PRODUCED]->(d:Decision) RETURN d.choice, d.confidence` 返回决策节点
  - `dt reason confirm --decision-id "..."` 将 Decision 的 verified 设为 true，并创建对应 Knowledge 节点
  - 未 confirm 的 Reasoning Decision 在 session 结束时自动标记为 stale

- [x] 3.6 Session 结束时的 Reasoning 清理
  **What**: 会话结束时，unverified 的 Observation/Analysis/Decision 节点被标记为 stale 或降级。

> **两级生命周期**：阶段一（弃用）：会话结束时 `SET _stale_at = timestamp()`，节点不可再被 Context Builder 查询。阶段二（删除）：`dt cleanup` 每夜检查，`_stale_at` 距今 > 30 天 → DETACH DELETE。两层设计确保近期弃用的节点在 30 天窗口内仍可被 `dt history` 审计回溯。

  **Files**: 修改 `python/mcp-server.py` 的 session-end 逻辑
  **Acceptance**:
  - 会话结束后，未验证的 Reasoning 节点的 `_stale_at` 属性被设置
  - `dt_context` 查询时不返回 stale 的 Reasoning 节点
  - 已验证的 Reasoning 节点被归档到 Knowledge/Memory（通过 `dt_learn`）

**🔵 里程碑 M3 — "语义可搜索"**（Phase 3 完成）
- ✅ `dt search "支付回调" --world all` → 返回代码+文档+配置混合结果
- ✅ 文档 chunking 按配置分块，chunk 间有 prev/next 上下文链
- ✅ Qdrant collection 命名包含 model_version，支持模型迁移
- ✅ `dt reason observe → analyze → confirm` 完整推理生命周期
- ✅ KG→Qdrant 桥接覆盖所有业务标签节点

---

### Phase 4: Context Builder + 12 个高层 MCP 工具（其中 8 个为核心六世界业务工具，4 个为系统运维工具：dt_cleanup, dt_backup, dt_archive, dt_metrics）

**目标**：实现 V2 的核心价值组件——Context Builder（六世界聚合管道）和 12 个高层 MCP 工具（其中 8 个为核心六世界业务工具，4 个为系统运维工具：dt_cleanup, dt_backup, dt_archive, dt_metrics）。

**依赖**：Phase 1-3（所有六世界必须有数据，Context Builder 才能切出有意义的"世界切片"）

**预估工作量**：XL（最核心、最有价值的 Phase）

---

- [x] 4.1 Context Builder 核心管道：Retriever → Ranker → Dedup → Resolver → Summarize
  **What**: 实现六世界并行查询后聚合压缩的完整管道。
  **Files**: 创建 `crates/dt-context/src/` (service.rs, pipeline.rs, stages/retriever.rs, stages/ranker.rs, stages/dedup.rs, stages/resolver.rs, stages/summarizer.rs, models.rs)
  **Acceptance**:
  - 输入 `task="支付平台从通联切换到银盛"`：
    - Retriever：并行查询 Reality（代码/配置/服务）、Knowledge（模式/概念）、Memory（历史任务/踩坑）、Semantic（文档向量）、Runtime（Pod 状态）、Reasoning（之前的分析）
    - Ranker：按语义相关度排序，过滤低分结果（<0.5）
    - Dedup：合并重复信息（如同一文件的多次修改记录合并为一条）
    - Resolver：检测冲突（如配置中 ifCode=allinpay 但代码中 ifCode=ysf）
    - Summarize：对大规模结果做摘要（>8000 tokens 时触发压缩）
  - 最终输出为结构化 JSON，每个 world 有独立 section
  - Context Builder 的总耗时 < 3 秒（含 Neo4j+Qdrant+K8s API 查询）

  **反馈闭环 — alerts 注入**（Summarizer 阶段内置）：
  - Retriever 从 Memory World 读取相关 Experience 节点（pitfalls）
  - Summarizer 阶段将高严重性 Experience 注入为 `alerts` 字段：
    ```json
    {
      "alerts": [
        {
          "type": "pitfall",
          "severity": "warning",
          "message": "别忘了同步修改 channelExtra",
          "source": "thread-pay-migration-2026-03"
        }
      ]
    }
    ```
  - `dt_verify` 检测到不一致时同样产出 alerts → `dt_plan` 生成的修复计划优先处理 alerts

- [x] 4.2 `dt_context` MCP 工具
  **What**: 封装 Context Builder 为 MCP 工具，输入 task 描述，返回六世界聚合上下文。
  **Files**: 修改 `python/mcp-server.py`，创建 `crates/dt-context/src/service.rs`
  **Acceptance**:
  - MCP 调用 `dt_context({"task": "支付平台从通联切换到银盛", "max_tokens": 6000})`
  - 返回 JSON 含 `reality`, `knowledge`, `memory`, `semantic`, `runtime`, `reasoning` 六个 section
  - `worlds` 参数可限定查询范围（如 `["reality", "knowledge"]`）
  - `thread_id` 参数关联 Digital Thread 时，返回该 thread 的历史摘要
  - 返回内容在 `max_tokens` 限制内

- [x] 4.3 `dt_plan` MCP 工具
  **What**: 根据任务自动匹配 Playbook 生成执行计划。无匹配 Playbook 时基于历史推理生成。
  **Files**: 修改 `python/mcp-server.py`，创建 `crates/dt-context/src/plan.rs`
  **Acceptance**:
  - MCP 调用 `dt_plan({"task": "支付平台从通联切换到银盛"})`
  - 返回 JSON 含 `matched_playbook`（匹配到的 Playbook 及成功次数）、`plan`（有序步骤列表）
  - 每个 step 包含：`phase`（分析/修改/验证/沉淀）、`action`、`tool`、`target`、`detail`
  - 无匹配 Playbook 时，基于 Knowledge + Memory 中相似任务自动推理生成 steps
  - `estimated_impact` 包含 files/configs/database_changes/services_to_restart

- [x] 4.4 `dt_domain` MCP 工具
  **What**: 返回某一业务领域的完整知识模型子图（概念→代码→配置→服务）。
  **Files**: 修改 `python/mcp-server.py`，创建 `crates/dt-context/src/domain_query.rs`
  **Acceptance**:
  - MCP 调用 `dt_domain({"domain": "支付", "depth": 2, "include_code": true})`
  - 返回 JSON 含 `concepts`（概念列表及定义、使用位置）、`services`、`databases`、`playbooks`、`relationships`
  - concepts 中每个概念有 `values`（如 ifCode 的枚举值）、`used_in`（引用该概念的代码文件+行号）
  - relationships 展示概念间关系（PAIRED_WITH/DEPENDS_ON 等）

- [x] 4.5 `dt_history` MCP 工具
  **What**: 沿 Memory World 时间线检索历史相似任务与修改记录。
  **Files**: 修改 `python/mcp-server.py`，创建 `crates/dt-context/src/history.rs`
  **Acceptance**:
  - MCP 调用 `dt_history({"task": "支付平台切换", "domain": "支付", "days": 180, "limit": 3})`
  - 返回 JSON 含 `similar_tasks`（按相似度排序）
  - 每个任务包含：`date`, `task`, `similarity`, `thread`, `outcome`, `key_learnings`, `modified_files`, `pitfalls`
  - similarity 来自 Semantic World 向量匹配（task 描述 vs 历史 Session.summary）
  - 时间线查询走 `Day → Session → Event` 链，过滤 90 天内

- [x] 4.6 `dt_dependency` MCP 工具
  **What**: 返回实体的调用链、依赖关系和影响范围分析。
  **Files**: 修改 `python/mcp-server.py`，创建 `crates/dt-context/src/dependency.rs`
  **Acceptance**:
  - MCP 调用 `dt_dependency({"target": "PayService", "direction": "both", "depth": 2, "type": "all"})`
  - 返回 JSON 含 `upstream`（callers + services）、`downstream`（callees + databases + configs + external）
  - `impact_analysis`：如果修改此实体，直接影响哪些实体、需改哪些配置、需重启哪些服务
  - 下游包含数据库表引用（`MATCH (t:Table)-[:REFERENCED_BY]->(m:Method {name: $method_name}) RETURN t.name, t.db`）
  - 下游包含 Nacos 配置依赖（`MATCH (m:Method {name: $method_name})<-[:CONTAINS]-(:Class)<-[:DEFINED_IN]-(ep:Endpoint)-[:DEPENDS_ON]->(nc:NacosConfig) RETURN DISTINCT nc.data_id, nc.group`）

- [x] 4.7 `dt_verify` MCP 工具
  **What**: 修改完成后验证代码、配置、数据库、API 的一致性。
  **Files**: 修改 `python/mcp-server.py`，创建 `crates/dt-context/src/verify.rs`
  **Acceptance**:
  - MCP 调用 `dt_verify({"files": [".../PayService.java", ".../BusinessService.java"], "check_config": true, "check_db": true})`
  - 返回 JSON 含 `checks` 各个维度（code_consistency, config_consistency, database_consistency, api_consistency）
  - config_consistency：检测代码引用的配置项在 Nacos 中是否存在、值是否匹配
  - database_consistency：检测代码中引用的表/字段在数据库中是否存在
  - api_consistency：检测 API 签名是否变更、是否影响调用方
  - `overall` 汇总状态（✓/⚠/✗）和具体 warnings
  - `suggestions` 给出修复建议

- [x] 4.8 `dt_search` MCP 工具（跨世界）
  **What**: 保留当前 `dt_search_expand` 的能力，增加 `world` 参数支持跨世界语义搜索。
  **Files**: 修改 `python/mcp-server.py`，修改 `crates/dt-pipeline/src/search.rs`
  **Acceptance**:
  - MCP 调用 `dt_search({"query": "支付回调", "world": "code", "path": "/data/myProject/aflm-pay"})` 返回代码方法（与当前一致）
  - MCP 调用 `dt_search({"query": "支付回调", "world": "knowledge"})` 返回 Knowledge/Playbook/Experience 节点
  - MCP 调用 `dt_search({"query": "支付回调", "world": "doc"})` 返回文档 chunk
  - MCP 调用 `dt_search({"query": "支付回调", "world": "all"})` 返回所有世界的混合结果，按世界分组

- [x] 4.9 `dt_learn` MCP 工具（高层版）
  **What**: 在 Phase 2.6 基础版上升级为完整的高层语义 dt_learn，接收 task/entities/pattern/pitfalls/decisions，批量写入 Knowledge World + 更新 Digital Thread。
  **Files**: 修改 `crates/dt-knowledge/src/learn.rs`，修改 `python/mcp-server.py`
  **Acceptance**:
  - MCP 调用 `dt_learn({"task": "支付平台迁移", "entities": [...], "pattern": "...", "pitfalls": [...], "decisions": [...], "thread_id": "...", "success": true})`
  - 批量创建：1 Knowledge + N Experience + 1 Playbook + 1 Decision 节点
  - Playbook 的 `success_count` 自动递增（如果已存在则更新）
  - 关联 Digital Thread（HAS_KNOWLEDGE, HAS_PLAYBOOK, HAS_DECISION）
  - 返回写入摘要（"📝 已沉淀 1 个知识模式, 1 个 Playbook, 2 条踩坑经验, 1 个决策记录"）

  **Playbook 成功率反馈**：
  - `dt_learn` 增加 `outcome: "success" | "partial" | "failure"` 字段
  - 成功 → `Playbook.success_count++`
  - 失败 → 自动创建 Experience 节点记录失败原因 → 关联到 Knowledge
  - Playbook 成功率 < 70% → `SET _needs_review = true` → `dt_context` alerts 中提示

- [x] 4.10 Digital Thread 集成
  **What**: 实现 Thread 节点 + Requirement 节点的完整生命周期，以及跨六世界的关系关联。
  **Files**: 创建 `crates/dt-knowledge/src/thread/service.rs`，修改 `crates/dt-daemon/src/main.rs`
  **Acceptance**:
  - `dt thread create --name "支付平台迁移：通联→银盛" --description "..."` 创建 Thread 节点
  - `dt thread add-session --thread-id "..." --session-id "..."` 关联会话
  - `dt thread add-decision --thread-id "..." --decision-id "..."` 关联决策
  - `MATCH (t:Thread {name: "支付平台迁移"})-[*]->(n) RETURN labels(n), count(*)` 展示完整演化链
  - `dt_context` 带 `thread_id` 参数时，返回 thread 历史摘要

- [x] 4.11 备份与灾难恢复（`dt backup`）
  **What**: 分层备份 Neo4j + Qdrant + SQLite，支持指定日期恢复。

  **备份策略**：
  | 存储 | 备份方式 | 频率 | 保留 |
  |------|----------|------|------|
  | Neo4j | `neo4j-admin database dump` | 每日 03:00 | 7 天滚动 |
  | Qdrant | Collection snapshot API | 每日 03:30 | 7 天滚动 |
  | SQLite | `cp lazy.db lazy.{date}.db` | 每次 `dt build` 前 | 30 天滚动 |

  **CLI**：
  ```bash
  dt backup                          # 全量备份所有存储
  dt backup --restore 2026-07-09     # 恢复到指定日期
  dt backup --list                   # 列出可用备份
  dt backup --verify 2026-07-09      # 验证备份完整性
  ```

  **Files**: 创建 `crates/dt-backup/` (Cargo.toml, src/lib.rs)，修改 `crates/dt-daemon/src/main.rs`

  **Acceptance**:
  - `dt backup` 执行后 `/var/lib/dt/backups/` 下生成带日期的备份文件
  - `dt backup --list` 列出所有备份及其大小
  - 清空 Neo4j 后 `dt backup --restore <date>` 恢复数据，节点数一致
  - 备份文件包含 SHA256 checksum，防止静默损坏
  - `dt health` 显示最近一次备份时间和状态
  - crontab: `0 3 * * * dt backup`

- [x] 4.12 数据归档（`dt archive`）
  **What**: Memory World 数据超过保留期后归档为压缩 JSON，释放 Neo4j 存储。

  **CLI**：
  ```bash
  dt archive --before 2026-01-01           # 归档指定日期之前的 Memory 数据
  dt archive --dry-run                      # 预览将被归档的数据量
  dt archive --list                         # 列出已有归档文件
  ```

  **归档格式**：`/var/lib/dt/archive/{date_range}.json.gz`，每行一个 Event JSON 对象

  **Files**: 修改 `crates/dt-storage/src/neo4j/repo.rs`（新增 `archive_events_before` 方法），创建 `crates/dt-daemon/src/archive.rs`

  **Acceptance**:
  - `dt archive --dry-run` 显示 "将归档 1,234 条 Event（2025-06 之前）"
  - 归档后 `MATCH (e:Event) WHERE e.timestamp < '2026-01-01' RETURN count(e)` = 0
  - 归档文件完整可读：`zcat archive/2025.json.gz | jq '.type' | sort | uniq -c`
  - `dt_context` / `dt_history` 不受归档影响（早期数据量小，性能无损）

- [x] 4.13 数据生命周期清理（`dt cleanup`）
  **What**: 按 TTL 策略自动清理过期数据：Memory.Event 超期归档后的残留、stale Reasoning 节点、旧 SQLite snapshot 行。
  **CLI**：
  ```bash
  dt cleanup --dry-run              # 预览即将清理的数据
  dt cleanup --execute              # 执行清理
  ```
  **Files**: 创建 `crates/dt-daemon/src/cleanup.rs`，修改 `crates/dt-daemon/src/main.rs`
  **Acceptance**:
  - `dt cleanup --dry-run` 显示各类数据将被清理的数量
  - 执行后 stale Reasoning 节点（`_stale_at > 30 days`）被删除
  - 旧 SQLite snapshot 行（`updated_at < latest_per_file`）被清理
  - Qdrant orphan points 被清理
  - 清理日志完整记录到 `/var/log/digital-twin/dt-daemon.log`

**🔵 里程碑 M4 — "六世界聚合"**（Phase 4 完成）
- ✅ `dt_context "切换支付平台"` → 返回六世界完整切片 + alerts 反馈（< 3 秒）
- ✅ `dt_plan "切换支付平台"` → 匹配 Playbook 生成执行计划
- ✅ `dt_verify` 修改后一致性检查产出具体 warnings + suggestions
- ✅ `dt_dependency PayService` → 上下游依赖 + 影响范围分析
- ✅ `dt_history` 沿时间线检索历史相似任务
- ✅ `dt backup` / `dt archive` / `dt cleanup` 备份归档清理正常
- ✅ Digital Thread 完整演化链可查询

---

### Phase 5: Integration, Testing & Polish

**目标**：端到端集成测试、性能优化、文档完善、生产部署准备。

**依赖**：Phase 1-4

**预估工作量**：M

---

- [ ] 5.1 端到端集成测试
  **What**: 编写完整的集成测试场景，覆盖从代码变更到 Context Builder 返回六世界聚合上下文的全程。
  **Files**: 创建 `tests/integration/` 目录
  **Acceptance**:
  - 测试场景 1：代码修改 → `dt update` → Memory Event 写入 → `dt_context` 返回包含该修改的上下文
  - 测试场景 2：Nacos 配置变更 → `nacos-sync` → `dt_verify` 检测到不一致 → `dt_plan` 生成修复计划
  - 测试场景 3：完整支付平台迁移模拟（修改代码 + Nacos + 验证 + dt_learn 沉淀）
  - 测试场景 4：Digital Thread 创建 → 多 session → 多 event → 完整演化链查询
  - 所有测试通过 `cargo test` 可运行

- [ ] 5.2 性能优化
  **What**: Context Builder 查询优化、批量写入优化、Qdrant 索引调优。
  **Files**: 修改 `crates/dt-context/src/`，修改 `crates/dt-storage/src/neo4j/`
  **Acceptance**:
  - `dt_context` 返回时间 < 2 秒（Phase 4 已达成 < 3s，此为 stretch goal）
  - `dt_build` 单个 300+ 文件的 Java 项目在 30 秒内完成（增量 < 5 秒）
  - `dt update --file` 单文件更新 < 2 秒
  - Neo4j Cypher 查询使用 PROFILE 验证索引命中
  - Qdrant HNSW 参数调优（m=16, ef_construct=100 → 根据数据量调整）

- [x] 5.3 错误处理与降级 (struct 骨架就绪)
  **What**: 为所有 MCP 工具和 Context Builder 添加完善的错误处理和降级策略。
  **Files**: 修改 `python/mcp-server.py`，修改 `crates/dt-context/src/pipeline.rs`
  **Acceptance**:
  - Neo4j 不可达时，`dt_context` 跳过 Reality/Knowledge/Memory 查询，仍返回 Semantic + Runtime
  - Qdrant 不可达时，`dt_context` 跳过 Semantic 查询
  - K8s API 不可达时，Runtime section 标记为 `unavailable`
  - 所有错误有明确的 error message + error code
  - `dt_health` 能精确报告每个服务的状态和连接延迟

- [x] 5.4 文档与 README 更新
   **What**: 更新用户文档、开发文档和 API 参考。
   **Files**: 修改 `README.md`, 创建 `docs/test-plan.md`
   **Acceptance**:
   - [x] README 包含 V2 六世界模型概述和 quick start（单 crate DDD 分层，24 个 CLI 命令）
   - [x] 项目结构完整展示（domain/infrastructure/application/interfaces/shared）
   - [x] `docs/test-plan.md` 测试方案（8 个测试场景，NoopRepo 模式验证通过）
   - Context Builder 架构图解

- [x] 5.5 生产部署准备 (skeleton 就绪)
   **What**: 确保 V2 在生产环境可部署，不影响现有服务。
   **Files**: 修改 `config.yaml`（增加 V2 配置段），修改 `setup.sh`
   **Acceptance**:
   - [x] `dt health` CLI 骨架通过（NoopRepo 模式，backend 健康检查正常）
   - [x] `dt schema init` 骨架通过（27 constraints + 1 index）
   - [x] `cargo build --release` 发布构建成功（53s）
   - [x] `cargo check && cargo test && cargo clippy -- -D warnings` 全部通过
   - [x] `dt nacos-sync --env test` 实测通过（350 configs）
   - [x] `dt k8s-sync` 实测通过（111 deployments, 123 services）
   - [x] config.yaml 已配置 test 环境 Neo4j/Qdrant/Nacos/K8s
   - [ ] 部署脚本（`setup.sh`）一键安装 V2 所有依赖（待环境就绪）

**🔵 里程碑 M5 — "生产就绪"**（Phase 5 完成）
- ✅ 全链路 E2E 测试通过（4 个场景）
- ✅ `dt_context` p99 < 3s, `dt_update` p99 < 2s
- ✅ Neo4j/Qdrant/K8s 任一不可达时优雅降级
- ✅ `dt backup --restore` 灾难恢复验证通过
- ✅ 安全审计：无明文密码、gRPC mTLS 生效、日志脱敏
- ✅ README + Migration Guide + API 文档完整

---

## Verification

- [ ] Phase 1-5 所有 checklist 项 + M1-M5 里程碑通过
- [x] `cargo check && cargo test && cargo clippy` 全部通过（509 tests, 0 fail）
- [x] `dt health` 全部绿灯（Neo4j 2ms, Qdrant v1.18.2, Embed gRPC :50052）
- [x] `dt --help` 显示所有命令（24个，实测通过）（build / search / sync / event / memorize / learn / context / plan / domain / history / dependency / verify / backup / archive / cleanup / metrics）
- [x] `dt_context` 返回六世界聚合上下文（实测: 23 items, ~190 tokens, 含 alerts）
- [x] MCP Server 通过 gRPC 调用 dt daemon — DtCore gRPC service 已注册到 server.rs (DtCoreServer)
- [x] kub/svc/jcli 已改为原生插件（非 subprocess, 非独立 CLI）
- [ ] 日志聚合：`/var/log/digital-twin/dt-daemon.log` 包含所有组件日志，`trace_id` 跨进程可串联
- [x] 安全: SecretString 脱敏 + auth interceptor 已实现, mTLS 待生产证书
- [x] 备份: Qdrant 备份恢复已实测, Neo4j 需 neo4j-admin 配置
- [x] 指标: `dt metrics` 可查询, 不暴露 HTTP

---

## 总览甘特图（文字）

```
                        Month 1              Month 2              Month 3
Phase   W1  W2  W3  W4  W5  W6  W7  W8  W9  W10 W11 W12
────────────────────────────────────────────────────────────────────────
P1:Core ██████████████████████████████░░░░░░░░░░░░░░░░░░░░░░░  XL
   1.0a workspace ██░░░░░░░░░░░░░░░
   1.0  plugin+grpc ░███░░░░░░░░░░░░░
   1.0b log+metrics ░░░███░░░░░░░░░░░
   1.0c security    ░░░░██░░░░░░░░░░░
   1.1  Schema      ░░░░░███░░░░░░░░░
    1.2  dt_build     ░░░░░░████████░░
    1.0e coordinator  ░░░░░░░░░░██░░░░
    1.3  nacos-sync   ░░░░░░░░░░████░░
   1.4  k8s-sync     ░░░░░░░░░░░░███░
   1.5  dt_update    ░░░░░░░░░░░░░░██
   1.6  dt_watch     ░░░░░░░░░░░░░░░█
    1.7  clean        ░░░░░░░░░░░░░░░░█
    M1 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██  ← Hello World 管线
P2:Mem+Know ░░░░░░░░░░░░░░░████████████████░░░░░░░░░░░░░░░░░░░  L
   2.1  Day/Session ░░░░░░░░░░░░░████░░░░░░░░
   2.2  Event types ░░░░░░░░░░░░░░░████░░░░░░
   2.3  Auto-Event  ░░░░░░░░░░░░░░░░░███░░░░░
   2.4  Knowledge   ░░░░░░░░░░░░░░░░░░█████░░
   2.5  @knowledge  ░░░░░░░░░░░░░░░░░░░░██░░░
    2.6  dt_learn    ░░░░░░░░░░░░░░░░░░░░█████
    2.7  exec_collect ░░░░░░░░░░░░░░░░░░░░░░███
    M2 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██  ← 记忆与知识
P3:Sem+Reason ░░░░░░░░░░░░░░░░░░░░░░░░░░░████████████░░░░░░░░░  L
   3.1  Qdrant cols░░░░░░░░░░░░░░░░░░░░░░░░░█████░░░░░░░
   3.2  Doc pipeline░░░░░░░░░░░░░░░░░░░░░░░░░░░███░░░░░░
   3.3  Config/API  ░░░░░░░░░░░░░░░░░░░░░░░░░░░█████░░░
   3.4  KG bridge   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██░░░░
   3.5  Reasoning   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░█████
   3.6  Cleanup     ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   M3 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██  ← 语义可搜索
P4:CtxBuilder ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░████████████  XL
   4.1  Pipeline   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░████░░
   4.2  dt_context ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░███░
   4.3  dt_plan    ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██░
   4.4  dt_domain  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   4.5  dt_history ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   4.6  dt_depend  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░████
   4.7  dt_verify  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░████
   4.8  dt_search  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   4.9  dt_learn v2░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   4.10 Thread     ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   4.11 backup     ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
    4.12 archive    ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
    4.13 dt_cleanup  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
    M4 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██  ← 六世界聚合
P5:Polish ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░████  M
   5.1  E2E tests  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   5.2  Perf opt   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   5.3  Error handl░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   5.4  Docs       ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   5.5  Prod ready ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
   M5 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██  ← 生产就绪
```

### 关键路径

```
Phase 1 (Schema+Reality) → Phase 2 (Memory+Knowledge) → Phase 4 (Context Builder+MCP)
                           ↘ Phase 3 (Semantic+Reasoning) ↗
                                                               → Phase 5 (Polish)
```

Phase 2 和 Phase 3 可以部分并行推进（Phase 3 的 Qdrant 集合创建和文档管道 3.1-3.2 不依赖 Phase 2，可在 Phase 1 完成后立即开始；Phase 3.3-3.6 的 KG 桥接和 Reasoning 升级路径依赖 Phase 2 的 Knowledge/Experience 实体写入管线）。

### 风险与缓解

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| Plugin trait 设计不完善，后期频繁修改 | 所有插件需要适配，Phase 1.0 延期 | 先基于 3 个已知插件（k8s/svc/jenkins）设计 trait，验证后再固化；预留 `PluginExt` 扩展 trait 供未来可选方法 |
| gRPC proto 定义不完整，后期频繁修改 | 接口不稳定 | Phase 1.0 完成所有 6+2 个 proto 定义，后续只增不改；proto 版本化 |
| Neo4j Bolt 驱动 (neo4rs) 成熟度不足 | 连接池/事务处理有 bug | 评估 neo4rs 社区活跃度；备选方案：保留 HTTP 作为 fallback driver，用 feature flag 切换 |
| gRPC 安全配置遗漏 | 生产环境明文传输、密码泄露 | Phase 1.0c 作为独立 task 强制验收；mTLS cert 过期自动告警 |
| 并发写入导致数据不一致 | Neo4j/Qdrant 脏数据 | WriteCoordinator (Phase 1.0e) + Cypher MERGE 唯一约束双重保障 |
| Schema 设计返工 | Phase 1 延期 | Phase 1.1 完成前做一次完整 review，确保所有实体类型被后续 Phase 覆盖 |
| Knowledge 版本覆盖后无法追溯 | 历史决策链断裂 | Phase 2.4 内置 EVOLVED_FROM 版本链，写入逻辑永不覆盖 |
| Qdrant gRPC API 与 REST API 行为差异 | Phase 3 写入/查询异常 | Phase 1.0 中做 A/B 对比测试，验证 gRPC 和 REST 返回结果一致 |
| Context Builder 延迟高 | Phase 4 体验差 | 并行查询六世界（Neo4j + Qdrant 并发 gRPC 调用），结果缓存 60s TTL |
| BGE-M3 向量维度不兼容 | Phase 3 无法写入 | V1 已用 BGE-M3 1024-dim，确认兼容；新 collection 通过 model_version 命名隔离 |
| Neo4j 数据损坏无法恢复 | 所有世界数据丢失 | `dt backup` 每日自动备份 + `dt backup --restore` 验证流程（Phase 4.11） |
| Memory World 无限膨胀 | Neo4j 存储成本指数增长 | 数据 TTL（Phase 1.1）+ `dt archive` 归档（Phase 4.12）
