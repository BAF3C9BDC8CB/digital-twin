# Digital Twin V2 分阶段实施路线图

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在完全清空 V1 数据的前提下，从零构建六世界模型、Context Builder 和 8 个高层 MCP 工具。

**Architecture:** 5 个 Phase，严格按依赖关系排序——Schema 先行，数据管线跟进，上层组件逐步叠加，最后集成验证。每个 Phase 产出可独立验证的能力增量。

**Tech Stack:** Rust (dt CLI), Python (MCP Server / dt-embed), Neo4j (Cypher), Qdrant (REST), BGE-M3 (1024-dim)

---

## TL;DR

> **Summary**: 5 个 Phase，总预估 XL（2-3 月）。Phase 1 落地新 Schema + 重写数据管道（最硬核），Phase 4 实现 Context Builder（最核心价值），Phase 5 集成验证上线。
> **Estimated Effort**: XL
> **Branch**: `feat/v2-architecture`

---

## Context

### Original Request
基于 V2 六世界模型设计文档，制定分阶段实施路线图。关键前提：完全不兼容 V1，所有数据清空重来。

### Key Findings
- 当前已有 22 个底层 MCP 工具（搜索/服务管理/K8s/Jenkins/管道/写入/运维），它们是 Phase 4 高层 MCP 的底层依赖
- dt CLI (Rust) 已具备 build/search/sync/event/memorize 能力，但都基于 V1 schema
- **通信架构需重构**：当前 5 条链路用了 4 种协议（HTTP REST/REST/自定义 Unix Socket/subprocess），V2 统一为 gRPC + Bolt（详见 Phase 1.0）
- Neo4j 当前 schema 是 V1 的单层扁平结构（Method/Class + Infrastructure/Server/Database + Event），V2 要变成六世界 + Digital Thread
- 设计文档完整覆盖：六世界模型 (185 lines) + 数据格式 (786 lines) + 全链路 (293 lines) + MCP API (1611 lines) + 管道实现 (921 lines)
- 底层基础设施（Neo4j/Qdrant/dt-embed/tree-sitter/Nacos同步/K8s同步）已就绪，需切换驱动方式

---

## Objectives

### Core Objective
在 `feat/v2-architecture` 分支上，分 5 个 Phase 完成从 Schema 到 Context Builder 到高层 MCP 的全链路实现。

### Deliverables
- [ ] Phase 1: 新 Neo4j Schema + Reality World 数据管线重写
- [ ] Phase 2: Memory + Knowledge World 写入管线
- [ ] Phase 3: Semantic + Reasoning World
- [ ] Phase 4: Context Builder + 8 个高层 MCP 工具
- [ ] Phase 5: 端到端集成测试 + 性能优化 + 文档

### Definition of Done
- [ ] `dt health` 全部服务绿灯
- [ ] `dt build --path /data/myProject/aflm-pay --name aflm-pay` 成功写入新 Schema 的 Method/Class/Module 节点
- [ ] `dt nacos-sync --env test` 成功写入新 Schema 的 NacosConfig/ConfigKey/Service
- [ ] `dt_event` 写入的 Memory Event 能沿 Day→Session→Event 时间线查询
- [ ] `dt_context --task "支付平台从通联切换到银盛"` 返回六世界聚合上下文
- [ ] `dt_verify` 能在修改后检测 config/db/api 不一致
- [ ] 全部 8 个高层 MCP 工具通过 MCP Server 可调用

### Guardrails (Must NOT)
- **不兼容 V1**：不考虑数据迁移，不保留旧 schema
- **不修改现有底层工具的参数签名**：22 个底层 MCP 的接口保持稳定
- **不影响生产环境**：所有操作在 feat/v2-architecture 分支，未完成前不合入 main

---

## TODOs

> **项目结构规范**：详见 [architecture-v2-project-structure.md](../docs/architecture-v2-project-structure.md)。所有实现必须遵守该文档定义的分层架构（Interface→Application→Domain→Infrastructure）、Trait 体系和设计模式。

### Phase 1: Core Infrastructure — 新 Schema + Reality World 数据管线重写

**目标**：落地新 Neo4j Schema，重写代码/配置/K8s 数据采集管线，确保 Reality World 能全量填充。本阶段同时完成插件系统、gRPC 通信和统一日志三大基础设施。

**依赖**：无（起点）

**预估工作量**：XL（最硬核的 Phase）

---

- [ ] 1.0a Workspace 初始化：Cargo workspace + 空 crate 骨架 + CI
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

- [ ] 1.0 插件系统 + gRPC 统一通信基础设施
  **What**: 设计插件系统骨架（`Plugin` trait），将 kub/svc/jcli 三个工具重构为插件，统一注册到 dt CLI daemon 的 gRPC server 上。所有服务间通信走 gRPC（Neo4j 用 Bolt），消除 HTTP REST、自定义帧协议和 subprocess 调用。
  
  **Plugin trait 定义**（`engine-rust/src/plugin/mod.rs`）：
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
  - 创建 `engine-rust/src/plugin/` (mod.rs, trait.rs, registry.rs, context.rs)
  - 创建 `engine-rust/src/plugins/k8s/` (从 /data/myProject/kub 重构，暴露 lib.rs)
  - 创建 `engine-rust/src/plugins/svc/` (从 /data/myProject/svc 重构)
  - 创建 `engine-rust/src/plugins/jenkins/` (从 /data/myProject/jenkins-cli-rs 重构)
  - 创建 `engine-rust/src/grpc/server.rs` (tonic server + plugin 注册)
  - 修改 `engine-rust/src/client/neo4j.rs` (HTTP REST → Bolt 驱动 neo4rs)
  - 修改 `engine-rust/src/client/qdrant.rs` (HTTP REST → gRPC)
  - 重写 `engine-rust/src/embed.rs` (Unix Socket → gRPC client)
  - 修改 `mcp-server.py` (subprocess → gRPC client 调用 dt daemon)
  - 修改 `engine-rust/Cargo.toml` (添加 tonic/neo4rs/prost 依赖，workspace 结构)

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

- [ ] 1.0b 统一日志系统（`dt-log` crate + gRPC LogService）
  **What**: 所有组件通过统一的日志管道输出，单一聚合文件可追踪跨进程调用链。日志系统覆盖 dt daemon、所有插件、dt-embed (Python) 和 MCP Server (Python) 四个进程。

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
  - 创建 `engine-rust/src/grpc/log_service.rs` (LogService 实现)
  - 创建 `python/dt_log.py` (Python gRPC log handler)
  - 修改 `engine-rust/Cargo.toml` (添加 dt-log + tracing 依赖)
  - 修改 `engine-rust/src/main.rs` (初始化 tracing-subscriber)
  - 修改 `engine-rust/src/plugin/context.rs` (添加 log 字段)
  - 修改 `mcp-server.py` (使用 GrpcLogHandler)
  - 修改 `services/embed-server/cli.py` (使用 GrpcLogHandler)
  - 修改 `engine-rust/src/plugins/*/` (所有 println! 改为 dt_log 宏)

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
  **What**: 在新 clean database 中创建所有 V2 约束、索引、节点标签。
  **Files**: 新建 `engine-rust/src/schema/mod.rs`，修改 `engine-rust/src/client/neo4j.rs`
  **Acceptance**:
  - 执行后 `CALL db.constraints()` 列出所有 V2 约束（method_id, class_id, module_id, server_id, database_id, config_id, endpoint_id, doc_id, service_id, knowledge_id, playbook_id, experience_id, concept_id, domain_id, day_id, session_id, modification_id, deployment_id, config_change_id, bugfix_id, decision_id, observation_id, analysis_id, thread_id, requirement_id）
  - `CALL db.indexes()` 列出全文索引和向量索引
  - 无 V1 残留标签（Infrastructure, Event, Project 等旧的 flat 标签）
  - `dt build --path <project>` 写入的节点使用新标签（Method/Class/Module，不是旧的 Method/Class）

- [ ] 1.2 `dt build` 重写为 V2 Schema
  **What**: 修改 `engine-rust/src/index/build.rs` 和 Neo4j 写入逻辑，产出 V2 Method/Class/Module 节点。
  **Files**: 修改 `engine-rust/src/index/build.rs`, `engine-rust/src/index/pipeline.rs`, `engine-rust/src/client/neo4j.rs`, `engine-rust/src/models.rs`
  **Acceptance**:
  - `dt build --path /data/myProject/aflm-pay --name aflm-pay` 完成后：
  - `MATCH (m:Method) RETURN count(m)` > 0
  - `MATCH (c:Class) RETURN count(c)` > 0
  - `MATCH (c:Class)-[:CONTAINS]->(m:Method) RETURN count(*)` > 0
  - `MATCH (m1:Method)-[:CALLS]->(m2:Method) RETURN count(*)` > 0
  - `MATCH (m:Module) RETURN count(m)` > 0
  - 每个 Method 节点的属性完全符合 V2 spec（method_id, name, signature, params, return_type, class_name, file_path, package_or_module, language, project, start_line, end_line, calls, comment）
  - Module 节点的 module_id 格式为 `dt://entity/{project}/module/{name}`

- [ ] 1.3 `dt nacos-sync` 重写为 V2 Schema
  **What**: 修改 `engine-rust/src/sync/nacos.rs`，产出 V2 NacosConfig/ConfigKey/Service/NacosService 节点。
  **Files**: 修改 `engine-rust/src/sync/nacos.rs`, `engine-rust/src/sync/mod.rs`
  **Acceptance**:
  - `dt nacos-sync --env test` 完成后：
  - `MATCH (n:NacosConfig) RETURN count(n)` > 0
  - `MATCH (k:ConfigKey) RETURN count(k)` > 0，且 k.name 包含如 `spring.datasource.url`
  - `MATCH (n:NacosConfig)-[:CONTAINS]->(k:ConfigKey) RETURN count(*)` > 0
  - `MATCH (s:Service) RETURN count(s)` > 0，且 s.service_id 格式为 `dt://entity/{env}/service/{name}`
  - NacosConfig.config_id 格式为 `dt://nacos/{ns}/{data_id}`
  - NacosConfig.content_hash 已计算 SHA256

- [ ] 1.4 `dt k8s-sync` 重写为 V2 Schema
  **What**: 修改 `engine-rust/src/sync/k8s.rs`，产出 V2 Server/Deployment/K8sPod/K8sService 节点。
  **Files**: 修改 `engine-rust/src/sync/k8s.rs`
  **Acceptance**:
  - `dt k8s-sync` 完成后：
  - `MATCH (s:Server) RETURN count(s)` > 0
  - `MATCH (s:Server {hostname: ...})` 节点有 server_id, name, hostname, environment 等属性
  - K8s 实体（Deployment, K8sPod, K8sService）节点按 V2 spec 创建
  - NacosService ↔ K8sService 交叉链接正确

- [ ] 1.5 `dt update` 单文件增量更新
  **What**: 新增 `engine-rust/src/index/update.rs` 实现单文件实时增量逻辑，替代当前 `dt build --file` 的简化流程。
  **Files**: 创建 `engine-rust/src/index/update.rs`，修改 `engine-rust/src/index/mod.rs`, `engine-rust/src/main.rs` 的命令注册
  **Acceptance**:
  - `dt update --file /data/myProject/aflm-pay/src/.../PayService.java` 执行后：
  - 该文件旧 Method 节点从 Neo4j + Qdrant 被删除
  - 新 Method 节点被 upsert
  - SQLite 快照更新（file_sha1 + mtime）
  - 返回变更摘要（`1 file, 5 methods updated, 2 classes`）
  - 幂等：重复执行不产生重复节点

- [ ] 1.6 `dt watch` 文件监视 daemon
  **What**: 新增文件监视守护进程，用 inotify 监听 config.yaml 中所有 projects 的源码变更。
  **Files**: 创建 `engine-rust/src/watch.rs`，修改 `engine-rust/src/main.rs`
  **Acceptance**:
  - `dt watch` 启动后常驻后台
  - 在任意项目中 `echo '// test' >> SomeService.java` 后，`dt update --file <path>` 被自动触发
  - `dt watch --status` 显示 daemon 运行状态和监听的目录数
  - `dt watch --stop` 安全退出

- [ ] 1.7 Schema 验证与数据清理
  **What**: 提供一键 clean 脚本，验证所有 V1 数据已清除，并执行 schema 初始化。
  **Files**: 创建 `engine-rust/src/schema/clean.rs`，修改 `engine-rust/src/main.rs`
  **Acceptance**:
  - `dt clean --confirm` 清空所有 Neo4j 节点和关系
  - `dt clean --confirm` 清空所有 Qdrant Collections
  - `dt clean --confirm` 重置 SQLite 快照
  - `dt schema init` 创建所有约束和索引
  - `dt health` 显示干净环境状态

---

### Phase 2: Memory + Knowledge Worlds

**目标**：实现 Memory World 时间线（Day→Session→Event）和 Knowledge World（Knowledge/Playbook/Experience/Concept/Domain）的写入与查询。

**依赖**：Phase 1（需要 Reality World 实体存在才能关联 AFFECTS/IMPLEMENTED_BY 等关系）

**预估工作量**：L

---

- [ ] 2.1 Memory World 核心结构：Day → Session → Event
  **What**: 实现 Day/Session 节点的自动创建，以及 Event 链的维护。
  **Files**: 修改 `engine-rust/src/event.rs`，创建 `engine-rust/src/memory.rs`
  **Acceptance**:
  - `dt event --type Conversation --entity-id "2026-07-09-test" --entity-type Session --project digital-twin --details "test session"` 执行后：
  - `MATCH (d:Day {day_id: "2026-07-09"}) RETURN d` 存在（首次自动创建）
  - `MATCH (d:Day)-[:HAS_SESSION]->(s:Session) RETURN s.session_id, s.summary` 返回对应 Session
  - 同一天多个 Session 正确链到同一个 Day 节点
  - Session 节点属性：session_id, summary, key_decisions, thread_id, started_at, ended_at

- [ ] 2.2 Memory Event 类型重写（Modification / Deployment / ConfigChange / BugFix / Decision）
  **What**: 重写 `dt_event` 的 Neo4j 写入逻辑，每种事件类型创建对应的 V2 节点标签和关系。
  **Files**: 修改 `engine-rust/src/event.rs`
  **Acceptance**:
  - `dt event --type Deploy --entity-id "aflm-pay-deploy-test" --entity-type JenkinsJob --project aflm --details "branch: master, env: test, version: v2.3.1"` 执行后：
    - `MATCH (d:Deployment {deploy_id: ...}) RETURN d.job, d.env, d.version` 返回正确值
    - `MATCH (d:Deployment)-[:DEPLOYS]->(s:Service) RETURN s.name` 返回关联服务
  - `dt event --type ConfigChange --entity-id "pay-datasource.yml" --entity-type NacosConfig --project aflm --details "key: spring.datasource.url, old: jdbc://10.0.1.50, new: jdbc://10.0.2.50"` 执行后：
    - `MATCH (c:ConfigChange)-[:AFFECTS]->(n:NacosConfig) RETURN c.key, c.old_value, c.new_value` 返回正确值
  - Modification/BugFix 节点同理验证

- [ ] 2.3 Auto-Event 触发集成（OpenCode Hook + AI 自动写入）
  **What**: 确保 OpenCode Hook (`tool.execute.after`) 触发代码修改时自动创建 Modification Event。
  **Files**: 修改 `.opencode/plugins/dt-build.js`，修改 `mcp-server.py` 的 post-execute 逻辑
  **Acceptance**:
  - AI edit 一个 Java 文件 → `dt update --file <path>` 被调用 → 自动创建 `(:Modification {file, diff_summary})` 节点
  - Jenkins 部署触发 → `(:Deployment)` 自动创建
  - 会话结束 → `(:Session {summary, key_decisions})` 自动创建

- [ ] 2.4 Knowledge World 核心实体：Knowledge / Experience / Concept / Domain / Playbook
  **What**: 重写 `dt memorize` 产出 V2 Knowledge 子图（Knowledge, Experience, Concept, Domain, Playbook 节点）。
  **Files**: 修改 `engine-rust/src/knowledge.rs`
  **Acceptance**:
  - `dt memorize --type Decision --entity-id "pay-migration-choice" --entity-type ArchitectureDecision --project aflm --details "decision: 选择银盛; reason: 费率低0.1%; scope: PayService, BusinessService"` 执行后：
    - `MATCH (k:Knowledge {knowledge_id: ...}) RETURN k.name, k.domain, k.content, k.confidence` 返回正确值
    - Knowledge 节点属性：knowledge_id, name, title, domain, summary, content, definition, source, project, confidence, verified_by, created_at, updated_at
  - `dt memorize --type KnowledgeAdded --entity-id "pitfall-channelExtra" --entity-type Experience --project aflm --details "title: 别忘了改channelExtra; severity: warning; domain: 支付"` 执行后：
    - `MATCH (e:Experience {experience_id: ...}) RETURN e.title, e.severity, e.domain` 返回正确值
  - Concept 节点属性包含 concept_id, name, definition, domain, summary
  - Domain 节点：`MATCH (d:Domain) RETURN d.name, d.description` 返回非空
  - Playbook 节点：steps 字段为 JSON 数组，每个 step 有 order/action/tool/target/expected/pitfall

- [ ] 2.5 `@knowledge` 代码注释提取
  **What**: `dt build` AST 解析时识别 Java/Python/TS/Go 中的 `@knowledge` 标记，自动创建 Knowledge/Concept 节点。
  **Files**: 修改 `engine-rust/src/parser.rs`，修改 `engine-rust/src/index/pipeline.rs`
  **Acceptance**:
  - 代码中包含 `@knowledge domain="支付" concept="ifCode"` 注释
  - `dt build` 后自动创建 `(:Concept {name: "ifCode", definition: "...", domain: "支付"})`
  - `MATCH (c:Concept)-[:IMPLEMENTED_BY]->(m:Method) RETURN c.name, m.name` 返回对应方法

- [ ] 2.6 `dt_learn` 基础版（高层 Knowledge 写入）
  **What**: 实现 `dt_learn` CLI 命令和 MCP 工具，接收 task/entities/pattern/pitfalls/decisions，批量写入 Knowledge World。
  **Files**: 创建 `engine-rust/src/learn.rs`，修改 `engine-rust/src/main.rs`, `mcp-server.py`
  **Acceptance**:
  - `dt learn --task "支付平台迁移" --entities "PayService,BusinessService,pay-channel.yml" --pattern "ifCode+wayCode+merchantNo+DB" --pitfalls "别忘了channelExtra,回调地址要改" --success true` 执行后：
    - `MATCH (k:Knowledge {name: "支付平台迁移模式", domain: "支付"}) RETURN k` 存在
    - `MATCH (e:Experience {title: "别忘了channelExtra"}) RETURN e` 存在
    - `MATCH (p:Playbook) RETURN p.steps` 非空
    - Playbook 从 pattern 和 pitfalls 自动合成 steps

---

### Phase 3: Semantic + Reasoning Worlds

**目标**：补全 Semantic World（文档/配置/API/经验/日志向量化管线）和 Reasoning World（Decision Graph 生命周期）。

**依赖**：Phase 1 + Phase 2（Science World 的向量化依赖 Neo4j 中有实体；Reasoning 的升级路径依赖 Knowledge World）

**预估工作量**：L

---

- [ ] 3.1 Semantic World：Qdrant Collection 体系建立
  **What**: 为每个项目创建标准 Qdrant Collection 矩阵（methods/semantic/kg_nodes），为 Semantic World 每类文本建立独立集合。
  **Files**: 修改 `engine-rust/src/client/qdrant.rs`，创建 `engine-rust/src/semantic.rs`
  **Acceptance**:
  - 执行 `dt build --path /data/myProject/aflm-pay --name aflm-pay` 后：
  - Qdrant collection `aflm-pay_methods` 存在且有向量
  - Qdrant collection `aflm-pay_semantic` 存在（文档向量）
  - 每个 vector point 的 payload 包含 `entity_id` 字段（可反查 Neo4j）
  - `dt search "支付回调" --path /data/myProject/aflm-pay --limit 5` 返回方法列表（与当前一致，但 entity_id 指向新 schema）

- [ ] 3.2 文档解析管道
  **What**: `dt build` 扫描 document_dirs 时，提取 md/pdf/txt 文档，chunk 分块，向量化，写入 Neo4j Document 节点 + Qdrant。
  **Files**: 修改 `engine-rust/src/parser.rs`（新增文档解析模块），修改 `engine-rust/src/index/pipeline.rs`
  **Acceptance**:
  - 在项目 `docs/` 目录放置一个 `.md` 文件
  - `dt build --path <project> --name <project>` 后：
  - `MATCH (d:Document) RETURN d.name, d.title, d.summary, d.doc_type` 返回文档节点
  - Qdrant `{project}_semantic` collection 中有对应的 vector points（payload 含 chunk_id, doc_name, text）
  - 文档节点有 `(:Document)-[:DESCRIBES]->(:Concept)` 关系（如果能从内容中提取概念）

- [ ] 3.3 Config/API/Experience/Log Pattern 向量化
  **What**: nacos-sync 后 ConfigKey 值自动向量化；dt_build 提取的 API Endpoint 描述向量化；Experience 写入时向量化；从 K8s 日志提取错误模式进入 Qdrant。
  **Files**: 修改 `engine-rust/src/sync/nacos.rs`（增加向量化步骤），修改 `engine-rust/src/semantic.rs`
  **Acceptance**:
  - nacos-sync 后，Qdrant 中有 config value 的向量（payload 含 key, value, namespace）
  - dt_build 后，Qdrant 中有 Endpoint 的向量（payload 含 method, path, description, controller）
  - dt_learn 写入 Experience 后，Qdrant 同步有对应向量
  - Log pattern 提取：`kublog_logs` 输出被解析出 ERROR 模板，进入 Qdrant pattern 集合

- [ ] 3.4 KG→Qdrant 桥接重写
  **What**: `dt kg-sync` 改为同步所有业务标签节点（Server, Database, NacosConfig, Service, Knowledge, Experience, Concept 等），而非旧的 Infrastructure 等标签。
  **Files**: 修改 `engine-rust/src/sync/kg.rs`
  **Acceptance**:
  - `dt kg-sync --incremental` 只同步 `_kg_synced_at IS NULL` 的节点
  - 同步后 `dt search-kg "支付数据库"` 能搜索到 Database/NacosConfig 节点
  - `dt search-kg "支付平台迁移"` 能搜索到 Knowledge/Experience 节点
  - 全量 `dt kg-sync` 覆盖所有业务标签节点

- [ ] 3.5 Reasoning World：Observation → Analysis → Decision
  **What**: 实现 Decision Graph 的写入、查询和生命周期管理。
  **Files**: 创建 `engine-rust/src/reasoning.rs`，修改 `engine-rust/src/main.rs`
  **Acceptance**:
  - `dt reason observe --description "Module A 和 Module B 结构高度相似" --evidence "都有 payChannel, merchant, callback 三层" --entities "ModuleA,ModuleB" --confidence 0.7` 执行后：
    - `MATCH (o:Observation {observation_id: ...}) RETURN o.description, o.evidence, o.confidence` 返回正确值
    - `MATCH (o:Observation)-[:ABOUT]->(m:Module) RETURN m.name` 返回关联模块
  - `dt reason analyze --question "切换支付平台影响哪些文件？" --hypothesis "影响 PayService 和 BusinessService" --conclusion "需改 5 处" --confidence 0.9` 执行后：
    - `MATCH (a:Analysis)-[:PRODUCED]->(d:Decision) RETURN d.choice, d.confidence` 返回决策节点
  - `dt reason confirm --decision-id "..."` 将 Decision 的 verified 设为 true，并创建对应 Knowledge 节点
  - 未 confirm 的 Reasoning Decision 在 session 结束时自动标记为 stale

- [ ] 3.6 Session 结束时的 Reasoning 清理
  **What**: 会话结束时，unverified 的 Observation/Analysis/Decision 节点被标记为 stale 或降级。
  **Files**: 修改 `mcp-server.py` 的 session-end 逻辑
  **Acceptance**:
  - 会话结束后，未验证的 Reasoning 节点的 `_stale_at` 属性被设置
  - `dt_context` 查询时不返回 stale 的 Reasoning 节点
  - 已验证的 Reasoning 节点被归档到 Knowledge/Memory（通过 `dt_learn`）

---

### Phase 4: Context Builder + 8 个高层 MCP 工具

**目标**：实现 V2 的核心价值组件——Context Builder（六世界聚合管道）和 8 个高层 MCP 工具。

**依赖**：Phase 1-3（所有六世界必须有数据，Context Builder 才能切出有意义的"世界切片"）

**预估工作量**：XL（最核心、最有价值的 Phase）

---

- [ ] 4.1 Context Builder 核心管道：Retriever → Ranker → Dedup → Resolver → Summarize
  **What**: 实现六世界并行查询后聚合压缩的完整管道。
  **Files**: 创建 `engine-rust/src/context/builder.rs`, `engine-rust/src/context/retriever.rs`, `engine-rust/src/context/ranker.rs`, `engine-rust/src/context/dedup.rs`, `engine-rust/src/context/resolver.rs`, `engine-rust/src/context/summarizer.rs`, `engine-rust/src/context/mod.rs`
  **Acceptance**:
  - 输入 `task="支付平台从通联切换到银盛"`：
    - Retriever：并行查询 Reality（代码/配置/服务）、Knowledge（模式/概念）、Memory（历史任务/踩坑）、Semantic（文档向量）、Runtime（Pod 状态）、Reasoning（之前的分析）
    - Ranker：按语义相关度排序，过滤低分结果（<0.5）
    - Dedup：合并重复信息（如同一文件的多次修改记录合并为一条）
    - Resolver：检测冲突（如配置中 ifCode=allinpay 但代码中 ifCode=ysf）
    - Summarize：对大规模结果做摘要（>8000 tokens 时触发压缩）
  - 最终输出为结构化 JSON，每个 world 有独立 section
  - Context Builder 的总耗时 < 5 秒（含 Neo4j+Qdrant+K8s API 查询）

- [ ] 4.2 `dt_context` MCP 工具
  **What**: 封装 Context Builder 为 MCP 工具，输入 task 描述，返回六世界聚合上下文。
  **Files**: 修改 `mcp-server.py`，创建 `engine-rust/src/context/cli.rs`
  **Acceptance**:
  - MCP 调用 `dt_context({"task": "支付平台从通联切换到银盛", "max_tokens": 6000})`
  - 返回 JSON 含 `reality`, `knowledge`, `memory`, `semantic`, `runtime`, `reasoning` 六个 section
  - `worlds` 参数可限定查询范围（如 `["reality", "knowledge"]`）
  - `thread_id` 参数关联 Digital Thread 时，返回该 thread 的历史摘要
  - 返回内容在 `max_tokens` 限制内

- [ ] 4.3 `dt_plan` MCP 工具
  **What**: 根据任务自动匹配 Playbook 生成执行计划。无匹配 Playbook 时基于历史推理生成。
  **Files**: 修改 `mcp-server.py`，创建 `engine-rust/src/plan.rs`
  **Acceptance**:
  - MCP 调用 `dt_plan({"task": "支付平台从通联切换到银盛"})`
  - 返回 JSON 含 `matched_playbook`（匹配到的 Playbook 及成功次数）、`plan`（有序步骤列表）
  - 每个 step 包含：`phase`（分析/修改/验证/沉淀）、`action`、`tool`、`target`、`detail`
  - 无匹配 Playbook 时，基于 Knowledge + Memory 中相似任务自动推理生成 steps
  - `estimated_impact` 包含 files/configs/database_changes/services_to_restart

- [ ] 4.4 `dt_domain` MCP 工具
  **What**: 返回某一业务领域的完整知识模型子图（概念→代码→配置→服务）。
  **Files**: 修改 `mcp-server.py`，创建 `engine-rust/src/domain_query.rs`
  **Acceptance**:
  - MCP 调用 `dt_domain({"domain": "支付", "depth": 2, "include_code": true})`
  - 返回 JSON 含 `concepts`（概念列表及定义、使用位置）、`services`、`databases`、`playbooks`、`relationships`
  - concepts 中每个概念有 `values`（如 ifCode 的枚举值）、`used_in`（引用该概念的代码文件+行号）
  - relationships 展示概念间关系（PAIRED_WITH/DEPENDS_ON 等）

- [ ] 4.5 `dt_history` MCP 工具
  **What**: 沿 Memory World 时间线检索历史相似任务与修改记录。
  **Files**: 修改 `mcp-server.py`，创建 `engine-rust/src/history.rs`
  **Acceptance**:
  - MCP 调用 `dt_history({"task": "支付平台切换", "domain": "支付", "days": 180, "limit": 3})`
  - 返回 JSON 含 `similar_tasks`（按相似度排序）
  - 每个任务包含：`date`, `task`, `similarity`, `thread`, `outcome`, `key_learnings`, `modified_files`, `pitfalls`
  - similarity 来自 Semantic World 向量匹配（task 描述 vs 历史 Session.summary）
  - 时间线查询走 `Day → Session → Event` 链，过滤 90 天内

- [ ] 4.6 `dt_dependency` MCP 工具
  **What**: 返回实体的调用链、依赖关系和影响范围分析。
  **Files**: 修改 `mcp-server.py`，创建 `engine-rust/src/dependency.rs`
  **Acceptance**:
  - MCP 调用 `dt_dependency({"target": "PayService", "direction": "both", "depth": 2, "type": "all"})`
  - 返回 JSON 含 `upstream`（callers + services）、`downstream`（callees + databases + configs + external）
  - `impact_analysis`：如果修改此实体，直接影响哪些实体、需改哪些配置、需重启哪些服务
  - 下游包含数据库表引用（`MATCH (m:Method)-[:REFERENCES]->(t:Table)`）
  - 下游包含 Nacos 配置依赖（`MATCH (m:Method)-[:DEPENDS_ON]->(c:ConfigKey)`）

- [ ] 4.7 `dt_verify` MCP 工具
  **What**: 修改完成后验证代码、配置、数据库、API 的一致性。
  **Files**: 修改 `mcp-server.py`，创建 `engine-rust/src/verify.rs`
  **Acceptance**:
  - MCP 调用 `dt_verify({"files": [".../PayService.java", ".../BusinessService.java"], "check_config": true, "check_db": true})`
  - 返回 JSON 含 `checks` 各个维度（code_consistency, config_consistency, database_consistency, api_consistency）
  - config_consistency：检测代码引用的配置项在 Nacos 中是否存在、值是否匹配
  - database_consistency：检测代码中引用的表/字段在数据库中是否存在
  - api_consistency：检测 API 签名是否变更、是否影响调用方
  - `overall` 汇总状态（✓/⚠/✗）和具体 warnings
  - `suggestions` 给出修复建议

- [ ] 4.8 `dt_search` MCP 工具（跨世界）
  **What**: 保留当前 `dt_search_expand` 的能力，增加 `world` 参数支持跨世界语义搜索。
  **Files**: 修改 `mcp-server.py`，修改 `engine-rust/src/search.rs`
  **Acceptance**:
  - MCP 调用 `dt_search({"query": "支付回调", "world": "code", "path": "/data/myProject/aflm-pay"})` 返回代码方法（与当前一致）
  - MCP 调用 `dt_search({"query": "支付回调", "world": "knowledge"})` 返回 Knowledge/Playbook/Experience 节点
  - MCP 调用 `dt_search({"query": "支付回调", "world": "doc"})` 返回文档 chunk
  - MCP 调用 `dt_search({"query": "支付回调", "world": "all"})` 返回所有世界的混合结果，按世界分组

- [ ] 4.9 `dt_learn` MCP 工具（高层版）
  **What**: 在 Phase 2.6 基础版上升级为完整的高层语义 dt_learn，接收 task/entities/pattern/pitfalls/decisions，批量写入 Knowledge World + 更新 Digital Thread。
  **Files**: 修改 `engine-rust/src/learn.rs`，修改 `mcp-server.py`
  **Acceptance**:
  - MCP 调用 `dt_learn({"task": "支付平台迁移", "entities": [...], "pattern": "...", "pitfalls": [...], "decisions": [...], "thread_id": "...", "success": true})`
  - 批量创建：1 Knowledge + N Experience + 1 Playbook + 1 Decision 节点
  - Playbook 的 `success_count` 自动递增（如果已存在则更新）
  - 关联 Digital Thread（HAS_KNOWLEDGE, HAS_PLAYBOOK, HAS_DECISION）
  - 返回写入摘要（"📝 已沉淀 1 个知识模式, 1 个 Playbook, 2 条踩坑经验, 1 个决策记录"）

- [ ] 4.10 Digital Thread 集成
  **What**: 实现 Thread 节点 + Requirement 节点的完整生命周期，以及跨六世界的关系关联。
  **Files**: 创建 `engine-rust/src/thread.rs`，修改 `engine-rust/src/main.rs`
  **Acceptance**:
  - `dt thread create --name "支付平台迁移：通联→银盛" --description "..."` 创建 Thread 节点
  - `dt thread add-session --thread-id "..." --session-id "..."` 关联会话
  - `dt thread add-decision --thread-id "..." --decision-id "..."` 关联决策
  - `MATCH (t:Thread {name: "支付平台迁移"})-[*]->(n) RETURN labels(n), count(*)` 展示完整演化链
  - `dt_context` 带 `thread_id` 参数时，返回 thread 历史摘要

---

### Phase 5: Integration, Testing & Polish

**目标**：端到端集成测试、性能优化、文档完善、生产部署准备。

**依赖**：Phase 1-4

**预估工作量**：M

---

- [ ] 5.1 端到端集成测试
  **What**: 编写完整的集成测试场景，覆盖从代码变更到 Context Builder 返回六世界聚合上下文的全程。
  **Files**: 创建 `engine-rust/tests/integration_v2.rs`
  **Acceptance**:
  - 测试场景 1：代码修改 → `dt update` → Memory Event 写入 → `dt_context` 返回包含该修改的上下文
  - 测试场景 2：Nacos 配置变更 → `nacos-sync` → `dt_verify` 检测到不一致 → `dt_plan` 生成修复计划
  - 测试场景 3：完整支付平台迁移模拟（修改代码 + Nacos + 验证 + dt_learn 沉淀）
  - 测试场景 4：Digital Thread 创建 → 多 session → 多 event → 完整演化链查询
  - 所有测试通过 `cargo test` 可运行

- [ ] 5.2 性能优化
  **What**: Context Builder 查询优化、批量写入优化、Qdrant 索引调优。
  **Files**: 修改 `engine-rust/src/context/builder.rs`，修改 `engine-rust/src/client/neo4j.rs`
  **Acceptance**:
  - `dt_context` 返回时间 < 3 秒（当前 < 5 秒目标）
  - `dt_build` 单个 300+ 文件的 Java 项目在 30 秒内完成（增量 < 5 秒）
  - `dt update --file` 单文件更新 < 2 秒
  - Neo4j Cypher 查询使用 PROFILE 验证索引命中
  - Qdrant HNSW 参数调优（m=16, ef_construct=100 → 根据数据量调整）

- [ ] 5.3 错误处理与降级
  **What**: 为所有 MCP 工具和 Context Builder 添加完善的错误处理和降级策略。
  **Files**: 修改 `mcp-server.py`，修改 `engine-rust/src/context/builder.rs`
  **Acceptance**:
  - Neo4j 不可达时，`dt_context` 跳过 Reality/Knowledge/Memory 查询，仍返回 Semantic + Runtime
  - Qdrant 不可达时，`dt_context` 跳过 Semantic 查询
  - K8s API 不可达时，Runtime section 标记为 `unavailable`
  - 所有错误有明确的 error message + error code
  - `dt_health` 能精确报告每个服务的状态和连接延迟

- [ ] 5.4 文档与 README 更新
  **What**: 更新用户文档、开发文档和 API 参考。
  **Files**: 修改 `README.md`, `README.zh.md`，创建 `docs/v2-migration-guide.md`
  **Acceptance**:
  - README 包含 V2 六世界模型概述和使用 quick start
  - V2 Migration Guide 说明如何从 V1 切换到 V2（清空数据 + 新 schema）
  - 8 个高层 MCP 工具的使用文档（含请求/响应示例）
  - Context Builder 架构图解

- [ ] 5.5 生产部署准备
  **What**: 确保 V2 在生产环境可部署，不影响现有服务。
  **Files**: 修改 `config.yaml`（增加 V2 配置段），修改 `setup.sh`
  **Acceptance**:
  - `dt health` 在生产 Neo4j 环境通过（需确认可访问生产集群）
  - `dt nacos-sync --env prod` 成功同步生产 Nacos 配置
  - `dt k8s-sync` 成功同步生产 K8s 资源
  - config.yaml 区分 test/prod 环境的 Neo4j/Qdrant 实例
  - 部署脚本（`setup.sh`）一键安装 V2 所有依赖

---

## Verification

- [ ] Phase 1-5 所有 checklist 项通过
- [ ] `dt health` 全部绿灯（Neo4j / Qdrant / Embed / KG Bridge / Fulltext）
- [ ] `dt_context --task "支付平台从通联切换到银盛"` 返回六世界聚合上下文（JSON 格式规范）
- [ ] `cargo test` 和 `cargo test --test integration_v2` 全部通过
- [ ] 0 regressions on 22 existing low-level MCP tools
- [ ] `dt clean --confirm && dt schema init && dt build-all` 一键可重建整个 V2 知识图谱
- [ ] MCP Server 注册的 30 个工具（22 底层 + 8 高层）在 OpenCode 中可调用

---

## 总览甘特图（文字）

```
                       Month 1              Month 2              Month 3
Phase   W1  W2  W3  W4  W5  W6  W7  W8  W9  W10 W11 W12
────────────────────────────────────────────────────────────────────────
P1:Core ██████████████████████████████░░░░░░░░░░░░░░░░░░░░░░░  XL
   1.0a workspace ██░░░░░░░░░░░░░░░
   1.0  plugin+grpc ░███░░░░░░░░░░░░░
   1.0b dt-log      ░░░██░░░░░░░░░░░░
   1.1  Schema       ░░░░████░░░░░░░░
   1.2  dt_build     ░░░░░░████████░░
   1.3  nacos-sync   ░░░░░░░░░░████░░
   1.4  k8s-sync     ░░░░░░░░░░░░███░
   1.5  dt_update    ░░░░░░░░░░░░░░██
   1.6  dt_watch     ░░░░░░░░░░░░░░░█
P2:Mem+Know ░░░░░░░░░░░░░░░████████████████░░░░░░░░░░░░░░░░░░░  L
  2.1  Day/Session ░░░░░░░░░░░░░████░░░░░░░░
  2.2  Event types ░░░░░░░░░░░░░░░████░░░░░░
  2.3  Auto-Event  ░░░░░░░░░░░░░░░░░███░░░░░
  2.4  Knowledge   ░░░░░░░░░░░░░░░░░░█████░░
  2.5  @knowledge  ░░░░░░░░░░░░░░░░░░░░██░░░
  2.6  dt_learn    ░░░░░░░░░░░░░░░░░░░░█████
P3:Sem+Reason ░░░░░░░░░░░░░░░░░░░░░░░░░░░████████████░░░░░░░░░  L
  3.1  Qdrant cols░░░░░░░░░░░░░░░░░░░░░░░░░█████░░░░░░░
  3.2  Doc pipeline░░░░░░░░░░░░░░░░░░░░░░░░░░░███░░░░░░
  3.3  Config/API  ░░░░░░░░░░░░░░░░░░░░░░░░░░░█████░░░
  3.4  KG bridge   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██░░░░
  3.5  Reasoning   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░█████
  3.6  Cleanup     ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
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
P5:Polish ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░████  M
  5.1  E2E tests  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
  5.2  Perf opt   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
  5.3  Error handl░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
  5.4  Docs       ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
  5.5  Prod ready ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░██
```

### 关键路径

```
Phase 1 (Schema+Reality) → Phase 2 (Memory+Knowledge) → Phase 4 (Context Builder+MCP)
                           ↘ Phase 3 (Semantic+Reasoning) ↗
                                                               → Phase 5 (Polish)
```

Phase 2 和 Phase 3 可以并行推进（Phase 3 仅依赖 Phase 1 的 Reality 数据存在，不依赖 Phase 2 的 Memory/Knowledge 写入管线完成）。

### 风险与缓解

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| Plugin trait 设计不完善，后期频繁修改 | 所有插件需要适配，Phase 1.0 延期 | 先基于 3 个已知插件（k8s/svc/jenkins）设计 trait，验证后再固化；预留 `PluginExt` 扩展 trait 供未来可选方法 |
| 重构 kub/svc/jcli 时遗漏功能 | 原 CLI 能力丢失 | 保留原仓库完整代码但标记 deprecated；1:1 映射原 subcommand 到 gRPC rpc；Phase 1.0 验收时逐条对比原 CLI --help 输出 |
| gRPC proto 定义不完整，后期频繁修改 | 接口不稳定 | Phase 1.0 完成所有 6 个 proto 定义（common/core/embed/k8s/svc/jenkins），后续只增不改；proto 版本化 |
| Neo4j Bolt 驱动 (neo4rs) 成熟度不足 | 连接池/事务处理有 bug | 评估 neo4rs 社区活跃度；备选方案：保留 HTTP 作为 fallback driver，用 feature flag 切换 |
| dt CLI daemon 进程崩溃影响全局 | 所有 MCP 工具不可用 | systemd `Restart=always` + `RestartSec=3`；MCP Server 检测 gRPC 不可用时自动 fallback 到 subprocess 模式 |
| 插件间状态泄漏（一个插件 panic 影响全局） | 其他插件连带不可用 | tonic 每个 service 独立 tokio task；panic 只影响当前请求不传播；plugin health check 独立运行 |
| Schema 设计返工 | Phase 1 延期 | Phase 1.1 完成前做一次完整 review，确保所有实体类型被后续 Phase 覆盖 |
| Qdrant gRPC API 与 REST API 行为差异 | Phase 3 写入/查询异常 | Phase 1.0 中做 A/B 对比测试，验证 gRPC 和 REST 返回结果一致 |
| Context Builder 延迟高 | Phase 4 体验差 | 并行查询六世界（Neo4j + Qdrant 并发 gRPC 调用），结果缓存 60s TTL |
| BGE-M3 向量维度不兼容 | Phase 3 无法写入 | V1 已用 BGE-M3 1024-dim，确认兼容；新 collection 需重新创建 |
