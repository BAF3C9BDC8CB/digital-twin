# Digital Twin v2 数据管道实现文档

> ⚠️ **DEPRECATED**: 本文档已被 [V3 单 Crate 分层架构](./architecture-v3-single-crate-layered.md) 替代。
> V2 多 crate workspace 方案已废弃，实际实现采用单 crate 内部模块分层。
> 保留本文档仅供历史参考。

> 状态：设计阶段 | 日期：2026-07-09（已刷新：新增 WriteCoordinator、安全鉴权、gRPC 指标、备份归档、chunking 策略、知识版本管理）

本文档说明每个世界数据的**具体更新方式、实现逻辑和所需组件**。对照 [数据全链路设计](./architecture-v2-data-pipeline.md) 和 [数据格式定义](./architecture-v2-data-schema.md) 阅读。

---

## 目录

1. [总体架构](#一总体架构)
2. [Reality World：代码实体更新](#二reality-world代码实体更新)
3. [Reality World：基础设施同步](#三reality-world基础设施同步)
4. [Security：安全鉴权与 SecretString](#四security安全鉴权与-secretstring)
5. [Concurrency：WriteCoordinator 并发写入协调](#五concurrencywritecoordinator-并发写入协调)
6. [Knowledge World：知识写入](#六knowledge-world知识写入)
7. [Memory World：事件写入](#七memory-world事件写入)
8. [Semantic World：向量化管道](#八semantic-world向量化管道)
9. [Runtime World：实时查询](#九runtime-world实时查询)
10. [Reasoning World：推理缓存](#十reasoning-world推理缓存)
11. [Digital Thread：主线聚合](#十一digital-thread主线聚合)
12. [Metrics：gRPC 指标监控](#十二metricsgrpc-指标监控)
13. [Backup & Archive：备份与归档](#十三backup--archive备份与归档)
14. [所需组件清单](#十四所需组件清单)

---

## 一、总体架构

```
┌─────────────────────────────────────────────────────────────┐
│                      触发器层                                 │
│   OpenCode Plugin (JS)  │  inotify daemon  │  cron / CLI     │
└─────────────┬────────────────┬──────────────────┬───────────┘
              │                │                  │
              ▼                ▼                  ▼
┌─────────────────────────────────────────────────────────────┐
│                     dt CLI (Rust binary)                     │
│                                                              │
│  dt build    dt update     dt nacos-sync    dt k8s-sync     │
│  dt event    dt memorize   dt kg-sync       dt search       │
└──────┬──────────┬──────────────┬───────────────┬────────────┘
       │          │              │               │
       ▼          ▼              ▼               ▼
┌──────────┐ ┌─────────┐  ┌──────────┐  ┌───────────────┐
│  Memgraph   │ │  SQLite │  │  Qdrant  │  │  dt-embed      │
│ (Cypher) │ │(snapshot)│  │ (gRPC)   │  │ (gRPC)         │
└──────────┘ └─────────┘  └──────────┘  └──────┬────────┘
                                                │
                                         ┌──────▼────────┐
                                         │  BGE-M3 GPU    │
                                         │  (1024-dim)    │
                                         └────────────────┘
```

**核心组件通信方式（V2 统一为 gRPC + Bolt）：**

```
                        ┌─────────────────────────────────┐
                        │     dt CLI (Rust daemon)         │
                        │     gRPC Server :50051           │
                        │                                  │
                        │  ┌───────────────────────────┐   │
                        │  │  tonic (gRPC framework)    │   │
                        │  │  bolt-driver (Bolt protocol)      │   │
                        │  │  qdrant-client (gRPC)      │   │
                        │  │  rusqlite (SQLite local)    │   │
                        │  └───────────────────────────┘   │
                        └──────┬──────┬───────┬────────────┘
                ┌──────────────┼──────┼───────┼──────────────┐
                │ gRPC         │ gRPC │  Bolt │ gRPC          │
                ▼              ▼      ▼        ▼              ▼
        ┌────────────┐ ┌────────────┐ ┌──────────┐ ┌──────────────┐
        │  dt-embed  │ │   Qdrant   │ │  Memgraph   │ │  MCP Server  │
        │  (Python)  │ │(gRPC:6334) │ │(Bolt:7687│ │  (Python)    │
        │ gRPC:50052 │ └────────────┘ └──────────┘ │ gRPC client  │
        └─────┬──────┘                             └──────┬───────┘
              │ in-process                                │ JSON-RPC
        ┌─────▼──────┐                             ┌──────▼───────┐
        │  BGE-M3    │                             │   OpenCode   │
        │  (GPU)     │                             │   (LLM)      │
        └────────────┘                             └──────────────┘

统一要点：
- 全链路使用 protobuf 序列化，类型安全，编译期校验
- gRPC HTTP/2 多路复用，连接池，无需每次新建连接
- dt CLI 变为常驻 daemon（systemd socket activated），不再 subprocess 启动
- Memgraph 走 Bolt 二进制协议（原生驱动，无法用 gRPC 替代），持久连接池
- dt-embed 不再手写 Unix Socket 帧协议，改用标准 gRPC
- MCP Server 不再解析 stdout，改为结构化 gRPC 调用
```

**旧架构（V1）— 已废弃：**
```
Rust dt CLI ──HTTP POST──▶ Memgraph    (REST Cypher, 每次新建连接)
Rust dt CLI ──HTTP PUT ──▶ Qdrant   (REST, JSON 序列化开销大)
Rust dt CLI ──SQLite─────▶ lazy.db  (本地文件, ✅ 保留)
Rust dt CLI ──UnixSocket─▶ dt-embed (自定义4字节帧+JSON, ❌ 无类型安全)
dt-embed    ──GPU推理───▶ BGE-M3   (进程内, ✅ 保留)
MCP Server  ──subprocess─▶ dt CLI   (stdout解析, ❌ 错误处理脆弱)
```

---

## 二、Reality World：代码实体更新

### 2.1 更新流程（`dt build`）

这是整个管道中最核心的流程。每次代码文件变更后触发：

```
触发（OpenCode Hook / inotify / CLI）
    │
    ▼
┌──────────────────────────────────────────────┐
│ 1. 项目解析                                   │
│    resolve_project(file_path)                 │
│    → 找到文件所属的项目名和根路径               │
├──────────────────────────────────────────────┤
│ 2. 健康检查                                   │
│    embed::health()  → 确保 dt-embed 运行      │
│    memgraph::health()  → 确保 Memgraph 可连接        │
│    memgraph::ensure_schema() → 创建约束/索引      │
│    qdrant::ensure_collection() → 创建向量集合  │
├──────────────────────────────────────────────┤
│ 3. 文件扫描                                   │
│    scanner::collect_files(root)               │
│    → WalkDir 遍历项目目录                      │
│    → 过滤：忽略目录/扩展名/文件名/大小>500KB    │
├──────────────────────────────────────────────┤
│ 4. 变更检测                                   │
│    compute_hashes_parallel(files)             │
│    → 多线程 SHA1(file_content) + mtime        │
│    → SQLite 比对：changed / deleted / unchanged│
├──────────────────────────────────────────────┤
│ 5. 删除旧数据                                 │
│    memgraph::delete_methods_by_files(changed)    │
│    qdrant::delete_points_by_files(changed)    │
├──────────────────────────────────────────────┤
│ 6. AST 解析                                   │
│    parse_files_parallel(changed_files)        │
│    → 每线程一个 tree-sitter Parser            │
│    → walk_node() 递归遍历语法树                │
│    → 提取 MethodBlock + ClassBlock            │
├──────────────────────────────────────────────┤
│ 7. 向量化 + 写入                              │
│    embed_and_write_all(methods)               │
│    a. embed::embed_batch(texts) → BGE-M3      │
│    b. qdrant::upsert_points(batch=1000)       │
│    c. memgraph::write_methods_batch(batch=2000)  │
├──────────────────────────────────────────────┤
│ 8. 类关系 + 快照 + 调用图                      │
│    memgraph::write_classes_batch() → CONTAINS    │
│    write_sqlite_snapshots() → INSERT OR REPLACE│
│    rebuild_calls_for_files() → CALLS 关系      │
└──────────────────────────────────────────────┘
```

### 2.2 变更检测实现

**SQLite 快照表**（`/var/lib/digital-twin/lazy.db`）：

```sql
CREATE TABLE IF NOT EXISTS file_snapshots (
    file_path    TEXT NOT NULL,
    project      TEXT NOT NULL,
    file_sha1    TEXT NOT NULL,
    file_mtime   REAL NOT NULL,
    method_count INTEGER DEFAULT 0,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (file_path, project)
);
```

**比对逻辑**（Rust）：

```rust
fn detect_changes(db, project, file_hashes) -> (changed, deleted) {
    for (rel_path, hash, mtime) in file_hashes {
        let prev = db.query_row(
            "SELECT file_sha1 FROM file_snapshots
             WHERE file_path = ?1 AND project = ?2",
            [rel_path, project],
        ).ok();
        match prev {
            Some(prev_hash) if prev_hash == hash => { /* unchanged */ }
            _ => changed.push(rel_path),  // 新增或变更
        }
    }
    // 在数据库中但不在文件系统中的 → 已删除
    for f in db_files {
        if !current_files.contains(&f) { deleted.push(f); }
    }
}
```

### 2.3 AST 解析实现

**支持 7 种语言**：

| 语言 | 扩展名 | tree-sitter 库 | 方法节点类型 |
|------|--------|----------------|-------------|
| Java | `.java` | `tree_sitter_java` | `method_declaration`, `constructor_declaration` |
| TypeScript | `.ts`, `.tsx` | `tree_sitter_typescript` | `function_declaration`, `method_definition`, `arrow_function` |
| Python | `.py` | `tree_sitter_python` | `function_definition` |
| Go | `.go` | `tree_sitter_go` | `function_declaration`, `method_declaration` |
| Rust | `.rs` | `tree_sitter_rust` | `function_item` |
| PHP | `.php` | `tree_sitter_php` | `method_declaration`, `function_definition` |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | `tree_sitter_javascript` | 同 TypeScript |

**提取内容**：
- 方法名、签名（完整方法体）、参数列表
- 行号范围（start_line, end_line）
- 调用关系（正则 `\b([a-zA-Z_]\w*)\s*\(` 提取，去重，前50个）
- 类名、包名（从文件路径推导）

**并行化**：每个线程持有独立的 tree-sitter Parser 实例，互不阻塞。

### 2.4 单文件增量更新（`dt update`）

对应 OpenCode Hook 的实时触发场景：

```
dt update --file /path/to/PayService.java

流程：
  1. 读取文件 → 从 Qdrant + Memgraph 删除该文件的旧方法
  2. tree-sitter 解析 → 提取 MethodBlock + ClassBlock
  3. 向量嵌入 → 写入 Qdrant + Memgraph
  4. 写入类关系（CONTAINS）
  5. 增量重建该文件的调用图（CALLS）
  6. 更新 SQLite 快照
```

### 2.5 Memgraph 写入查询

**方法节点**（批量，每批 2000 条）：

```cypher
UNWIND $methods AS m
MERGE (n:Method {method_id: m.method_id})
SET n.project = m.project,
    n.file_path = m.file_path,
    n.language = m.language,
    n.package_or_module = m.package_or_module,
    n.class_name = m.class_name,
    n.name = m.name,
    n.signature = m.signature,
    n.params = m.params,
    n.return_type = m.return_type,
    n.start_line = m.start_line,
    n.end_line = m.end_line,
    n.calls = m.calls
```

**类节点 + 关系**：

```cypher
UNWIND $classes AS c
MERGE (n:Class {class_id: c.class_id})
SET n.name = c.name, n.file_path = c.file_path, ...
WITH n, c
UNWIND c.method_ids AS mid
MATCH (m:Method {method_id: mid})
MERGE (n)-[:CONTAINS]->(m)
```

**调用图关系**：

```cypher
MATCH (caller:Method {project: $project})
WHERE caller.file_path IN $changed_files
UNWIND caller.calls AS called_name
MATCH (callee:Method {project: $project, name: called_name})
WHERE callee.method_id <> caller.method_id
MERGE (caller)-[:CALLS]->(callee)
```

### 2.6 Qdrant 写入

**Collection 创建**（gRPC，通过 qdrant-client crate）：

```rust
use qdrant_client::qdrant::{
    CreateCollectionBuilder, VectorParamsBuilder, HnswConfigDiffBuilder,
    OptimizersConfigDiffBuilder, Distance,
};

client.create_collection(CreateCollectionBuilder::new(&format!("{project}_methods"))
    .vectors_config(VectorParamsBuilder::new(1024, Distance::Cosine))
    .hnsw_config(HnswConfigDiffBuilder::default().m(16).ef_construct(100))
    .optimizers_config(OptimizersConfigDiffBuilder::default()
        .indexing_threshold(10000))
).await?;
```

**向量写入**（每批 1000 条，gRPC upsert）：

```rust
use qdrant_client::qdrant::{PointStruct, UpsertPointsBuilder};

client.upsert_points(UpsertPointsBuilder::new(
    &format!("{project}_methods"),
    points.iter().map(|p| PointStruct::new(
        p.id,         // SHA256(method_id)[0..8] → u64
        p.vector,     // 1024 维 float32
        p.payload,    // {entity_id, name, signature, class_name, ...}
    )).collect(),
).wait(true)).await?;
```

**按文件删除旧点**（gRPC delete）：

```rust
use qdrant_client::qdrant::{Condition, Filter, DeletePointsBuilder};

client.delete_points(DeletePointsBuilder::new(&format!("{project}_methods"))
    .points_selector(
        Filter::must([Condition::matches("file_path", "src/.../PayService.java")])
    )
).await?;
```

---

## 三、Reality World：基础设施同步

### 3.1 Nacos 配置同步（`dt nacos-sync`）

**触发方式**：定时（cron 每小时）或手动

**实现**（`sync/nacos.rs`，467 行）：

```
dt nacos-sync --env test
    │
    ├─ GET /v1/console/namespaces → 获取所有命名空间
    │     └─ 写入: (:Environment)-[:HAS_NAMESPACE]->(:NacosNamespace)
    │
    ├─ 对每个 namespace:
    │   ├─ GET /v1/cs/configs?pageNo=1&pageSize=100&tenant={ns}
    │   │   └─ 分页获取所有配置
    │   │   └─ 写入: (:NacosNamespace)-[:HAS_GROUP]->(:NacosGroup)
    │   │          (:NacosGroup)-[:HAS_CONFIG]->(:NacosConfig)
    │   │          (:NacosNamespace)-[:CONTAINS]->(:NacosConfig)
    │   │
    │   │   对每个配置：
    │   │   ├─ GET /v1/cs/configs?dataId=...&show=all → 获取完整内容
    │   │   ├─ SHA256(content) 比对 → 仅变更时更新
    │   │   └─ MERGE (:NacosConfig {config_id, data_id, group, content,
    │   │              content_hash, config_type, updated_at})
    │   │
    │   └─ GET /v1/ns/catalog/services?namespaceId={ns} → 获取服务列表
    │       └─ 写入: (:NacosNamespace)-[:REGISTERS]->(:NacosService)
    │              (:NacosService)-[:HAS_INSTANCE]->(:NacosInstance)
    │
    │
    │   对每个配置内容：
    │   ├─ 正则匹配 JDBC URL：`jdbc:(\w+)://([\w.-]+):(\d+)/(\w+)`
    │   │   └─ MERGE (:Database {host, port, db_type, name})
    │   │   └─ (:Service)-[:DEPENDS_ON]->(:Database)
    │   │   同样匹配 Redis/Kafka 连接串
    │   │
    └─ 交叉链接:
        ├─ NacosService.name ↔ K8sService.name → EXPOSED_BY
        └─ K8sDeployment ↔ NacosConfig (名称前缀匹配)
```

**API 调用示例**：

```bash
# 获取命名空间
curl "${NACOS_URL}/v1/console/namespaces"

# 获取配置列表
curl "${NACOS_URL}/v1/cs/configs?tenant=ns-id&pageNo=1&pageSize=100"

# 获取完整配置内容
curl "${NACOS_URL}/v1/cs/configs?dataId=app.yml&group=DEFAULT&tenant=ns-id&show=all"

# 获取服务列表
curl "${NACOS_URL}/v1/ns/catalog/services?namespaceId=ns-id"
```

### 3.2 K8s 资源同步（`dt k8s-sync`）

**触发方式**：定时（cron 每小时）或手动

**实现**（`sync/k8s.rs`，763 行）：

```
dt k8s-sync
    │
    ├─ 认证: POST {kuboard_url}/api/login.kuboard.cn/v4/login
    │   └─ 获取 Bearer token
    │
    └─ 对每个 namespace (newoffen, newoffen-test):
        │
        ├─ GET /apis/apps/v1/namespaces/{ns}/deployments
        │   └─ MERGE (:K8sDeployment {name, namespace, image,
        │          replicas, available, strategy, condition})
        │   └─ (:Namespace)-[:HAS_DEPLOYMENT]->(:K8sDeployment)
        │
        ├─ GET /api/v1/namespaces/{ns}/services
        │   └─ MERGE (:K8sService {name, namespace, cluster_ip, type})
        │   └─ (:Namespace)-[:HAS_SERVICE]->(:K8sService)
        │
        ├─ GET /api/v1/namespaces/{ns}/configmaps
        │   └─ MERGE (:ConfigMap {name, namespace, data_keys})
        │
        ├─ GET /apis/networking.k8s.io/v1/namespaces/{ns}/ingresses
        │   └─ MERGE (:Ingress {name, namespace})
        │   └─ (:Ingress)-[:PROXIES_TO]->(:K8sService)
        │
        └─ GET /api/v1/namespaces/{ns}/persistentvolumeclaims
            └─ MERGE (:PersistentVolumeClaim {name, namespace, phase})
    │
    └─ 交叉链接:
        ├─ NacosService ↔ K8sService (同名匹配)
        ├─ NacosInstance ↔ ServiceInstance (服务名 + IP 匹配)
        └─ K8sDeployment ↔ NacosConfig (名称前缀匹配)
```

**Kuboard K8s API 代理路径**：

```
{kuboard_server}/k8s-api/{cluster_id}/api/v1/namespaces/{ns}/pods
{kuboard_server}/k8s-api/{cluster_id}/apis/apps/v1/namespaces/{ns}/deployments
```

### 3.3 手动注册

对于无法自动发现的实体（如特定的数据库、外部 API），通过 `config.yaml` 手动注册：

```yaml
servers:
  - server_id: "prod-mysql-01"
    hostname: "10.0.1.50"
    port: 3306
    service_type: "mysql"
    environment: "prod"
    auth_user: "app_user"
    auth_password: "encrypted..."
```

`dt build-all` 时一并索引到 Memgraph。

---

## 四、Security：安全鉴权与 SecretString

### 4.1 SecretString 凭证管理

所有敏感配置（密码、token、密钥）通过 `SecretString` 枚举类型管理，不再在 config.yaml 明文存储：

```rust
// dt-common/src/config.rs
pub enum SecretString {
    Env(String),      // "env:MEMGRAPH_PASSWORD"
    Vault(String),    // "vault:secret/memgraph"
    Plain(String),    // 明文（仅 dev，生产拒绝启动）
}
```

- `resolve()` 首次访问时解析
- `Debug`/`Display` 实现输出 `"***"`，日志自动脱敏
- config.yaml 改造：
  ```yaml
  memgraph:
    password: "env:MEMGRAPH_PASSWORD"     # 从环境变量读取
  qdrant:
    api_key: "vault:secret/qdrant"     # 从外部密钥管理
  ```

### 4.2 gRPC 三层认证

```
外部调用者              dt daemon (:50051)         权限
────────────────────────────────────────────────────────
OpenCode MCP Server ──▶  Unix Socket             全部（本地信任）
用户 CLI (dt xxx)   ──▶  Unix Socket             全部（本地信任）
远程/外部系统        ──▶  mTLS + JWT bearer       只读（拒绝 dt clean）
```

**实现：**
- tonic interceptor 检查 peer address：Unix socket → `AdminRole`；网络 → `ReadOnlyRole`
- `dt clean` / `dt schema drop` 仅 `AdminRole` 可调用
- config.yaml `security` 段：
  ```yaml
  security:
    tls:
      ca_cert: "/etc/dt/ca.pem"
      server_cert: "/etc/dt/server.pem"
      server_key: "/etc/dt/server.key"
    jwt_secret: "env:DT_JWT_SECRET"
  ```

### 4.3 文件

- `dt-common/src/config.rs` — SecretString 类型
- `dt-daemon/src/auth.rs` — tonic interceptor，角色提取
- `dt-daemon/src/server.rs` — 注册 auth interceptor

---

## 五、Concurrency：WriteCoordinator 并发写入协调

### 5.1 问题

三个独立写入源可能同时写 Memgraph/Qdrant，Cypher MERGE 不保证事务隔离：

| 写入源 | 触发 | 频率 |
|--------|------|------|
| OpenCode Hook → `dt update` | AI 编辑文件 | 实时，异步 |
| 用户 CLI → `dt build` | 手动执行 | 随意 |
| cron → `nacos-sync` / `k8s-sync` | 定时 | 每小时 |

> **daemon 内聚原则**：`dt update` / `dt build` / `dt memorize` 等 CLI 命令必须是 **Thin Client**——不包含任何业务逻辑或数据库连接。CLI 通过 Unix Socket gRPC 将请求转发至 `dt daemon`，所有写操作（Memgraph/Qdrant/SQLite）由 daemon 内唯一的 `WriteCoordinator` 实例串行化。`dt_core.proto` 需补充 `UpdateFile`、`BuildProject` 等 RPC 定义，确保无 CLI 绕过 daemon 直接写库的路径。

### 5.2 设计

```rust
// dt-pipeline/src/coordinator.rs
pub struct WriteCoordinator {
    // 同一文件不能并发写（按 file_path 分片锁）
    file_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    // 实体锁（按 entity_id 分片，用于 Knowledge 版本写入等并发控制）
    entity_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    // 全局写串行化（可选，默认关闭）
    global_lock: Option<tokio::sync::RwLock<()>>,
}
```

### 5.3 集成方式

Wrap 模式——不改变现有 trait 签名：

```rust
struct CoordinatedBuildService {
    inner: Arc<dyn BuildService>,
    coordinator: Arc<WriteCoordinator>,
}

impl BuildService for CoordinatedBuildService {
    async fn update_file(&self, project: &str, path: &Path) -> Result<UpdateReport> {
        let _guard = self.coordinator.acquire_file(path).await;
        self.inner.update_file(project, path).await
    }
}
```

### 5.4 定时任务冲突处理

```rust
// nacos-sync / k8s-sync 启动前
if coordinator.has_active_writes().await {
    info!("skipped: {} active writes in progress", count);
    return Ok(SyncReport::skipped());
}
// 连续跳过 3 次 → WARN 日志
```

### 5.5 文件

- `dt-pipeline/src/coordinator.rs` — WriteCoordinator 实现
- `dt-daemon/src/wiring.rs` — DI 装配时用 CoordinatedXxx 包装所有 Service

### 5.6 全量构建时的锁降级策略

`dt build --full` 扫描整个项目时，逐文件加锁会带来性能问题：

```rust
// dt build --full 时采用全局锁模式
pub async fn build_full(&self, project: &str) -> Result<BuildReport> {
    // 获取全局写锁（阻塞所有 file_locks）
    let _global_guard = self.coordinator.acquire_global().await;
    // 内部不再逐文件加锁，直接批量写入
    self.inner.build_full(project).await
}
```

- 全量构建时，`WriteCoordinator` 启用 `global_lock`（`RwLock::write()`），阻塞所有并发写入
- 此时 `file_locks` 退化为 no-op（全局锁已覆盖所有文件）
- 定时任务（nacos-sync / k8s-sync）检测到 `has_active_writes()` → 自动跳过
- 全量构建完成后释放全局锁，恢复逐文件并发锁模式

---

## 六、Knowledge World：知识写入

### 6.1 六个来源的写入方式

| 来源 | 触发 | 写入方式 | 实现位置 |
|------|------|----------|----------|
| AI 会话自动提取 | 会话结束 | `dt event --type Conversation` → Knowledge 提取 | MCP `dt_event` |
| AI 任务主动沉淀 | 任务完成 | `dt memorize --type KnowledgeAdded` | MCP `dt_memorize` |
| 文档自动解析 | `dt build` 扫描 document_dirs | 同代码索引流程（tree-sitter → 按 ChunkConfig（512 tokens + 64 overlap + 段落边界优先）分块 → embed） | `dt build` |
| 代码注释标记 | `dt build` AST 解析 | 识别 `@knowledge` 注释 → MERGE Knowledge | `dt build` |
| 执行结果自动采集 | AI 执行工具后 | `dt memorize --type KnowledgeAdded --details "{工具名}: {结果摘要}"` | MCP `dt_memorize` |
| 用户口述 | 用户说"记住" | `dt memorize --type KnowledgeAdded` | MCP `dt_memorize` |

### 6.2 `dt memorize` 实现

**Rust 侧**（`knowledge.rs`）：

```rust
pub async fn write_knowledge(
    knowledge_type: &str,
    entity_id: &str,
    entity_type: &str,
    project: &str,
    details: &str,
) -> Result<()> {
    let id = hex::encode(&sha256("knowledge::entity_id::ts")[..20]);

    let (root_cause, fix_summary, decision, reason) = parse_details(details);

    // 查询是否已存在同名 Knowledge 节点
    let existing = run_cypher_read("
        MATCH (k:Knowledge {entity_id: $entity_id})
        WHERE NOT (()-[:EVOLVED_FROM]->(k))
        RETURN k.version AS version, elementId(k) AS old_eid
        ORDER BY k.version DESC LIMIT 1
    ", params! {"entity_id": entity_id}).await?;

    if let Some((old_version, old_eid)) = existing {
        // 已存在 → 创建新版本节点 + EVOLVED_FROM 关系
        let new_version = old_version + 1;
        run_cypher_raw("
            CREATE (k2:Knowledge {knowledge_id: $id_v2, version: $new_version, ...})
            WITH k2
            MATCH (k1:Knowledge) WHERE elementId(k1) = $old_eid
            CREATE (k2)-[:EVOLVED_FROM]->(k1)
            CREATE (kv:KnowledgeVersion {
                version_id: $vid, knowledge_id: $id_v2,
                version: $new_version, diff: $details,
                timestamp: $ts, session_id: $sid
            })
            CREATE (kv)-[:RECORDS]->(k2)
        ", params).await?;
    } else {
        // 新知识 → 创建 v1
        run_cypher_raw("
            CREATE (k:Knowledge {knowledge_id: $id, version: 1, ...})
        ", params).await?;
    }
}
```

> **并发保护**：`dt memorize` 的版本写入需在 WriteCoordinator 中增加 `entity_id` 级别的锁（而非仅 file_path）。两个并发请求对同一 `entity_id` 写入时，按 `entity_id` 互斥，避免版本链分叉。实现方式：在 `WriteCoordinator` 中增加 `entity_locks: DashMap<String, Arc<Mutex<()>>>`。

### 6.3 Knowledge 版本管理

更新 Knowledge 节点时不 UPDATE 旧节点，而是创建新版本：

```cypher
CREATE (k2:Knowledge {knowledge_id: $id_v2, version: 2, ...})
CREATE (k2)-[:EVOLVED_FROM]->(k1)
CREATE (kv:KnowledgeVersion {
    version_id: $vid, knowledge_id: $id_v2,
    version: 2, diff: $diff, timestamp: $ts, session_id: $sid
})
CREATE (kv)-[:RECORDS]->(k2)
```

Context Builder 查询时取最新版本（无 `EVOLVED_FROM` 入边的节点）。版本链可追溯：`MATCH (k)-[:EVOLVED_FROM*]->(old)`。

### 6.4 执行结果自动采集

AI 执行 bash 后，MCP Server 判断返回值是否有长期价值：

```python
# mcp-server.py 中的逻辑（伪代码）
def after_tool_execute(tool_name: str, result: str):
    # 黑名单：无需采集的命令
    SKIP_COMMANDS = ["ls", "cat", "echo", "cd", "pwd", "grep", "find"]

    if any(tool_name.startswith(cmd) for cmd in SKIP_COMMANDS):
        return  # 跳过临时查询

    # 判断是否为结构化输出
    if is_structured_output(result):
        run_cmd(["dt", "memorize",
            "--type", "KnowledgeAdded",
            "--entity-id", f"exec-{tool_name}-{timestamp}",
            "--entity-type", "ExecutionResult",
            "--project", project,
            "--details", f"tool: {tool_name}\nresult_summary: {summarize(result)}"
        ])
```

---

## 七、Memory World：事件写入

### 7.1 事件写入流程

所有事件通过 `dt event` 命令写入，由 MCP Tool 或 OpenCode Hook 自动触发：

```rust
// event.rs
pub async fn write_event(
    event_type: &str,    // Deploy | SoftwareInstalled | ConfigChange | Conversation
    entity_id: &str,     // 唯一标识
    entity_type: &str,   // ServiceInstance | Software | NacosConfig | Session
    project: &str,
    details: &str,       // JSON 或自然语言描述
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let event_id = hex::encode(&sha256(
        format!("{}::{}::{}", event_type, entity_id, ts)
    )[..20]);

    // 按事件类型写入对应标签（V2: 不再使用泛化 :Event 标签）
    match event_type {
        "Deploy" => run_cypher_raw("
            MERGE (d:Deployment {deploy_id: $entity_id})
            SET d.job = $entity_id, d.env = $env, d.branch = $branch,
                d.version = $version, d.params = $params, d.status = $status,
                d.session_id = $session_id, d.timestamp = $ts
            WITH d
            MATCH (si:ServiceInstance {name: $service_name})
            MERGE (d)-[:DEPLOYS]->(si)
        ", params).await,

        "ConfigChange" => run_cypher_raw("
            MERGE (c:ConfigChange {change_id: $entity_id})
            SET c.data_id = $data_id, c.key = $key,
                c.old_value = $old_val, c.new_value = $new_val,
                c.session_id = $session_id, c.timestamp = $ts
            WITH c
            MATCH (n:NacosConfig {data_id: $data_id})
            MERGE (c)-[:AFFECTS]->(n)
        ", params).await,

        "Modification" => run_cypher_raw("
            MERGE (m:Modification {mod_id: $entity_id})
            SET m.file = $file, m.entity_type = $entity_type, m.entity_id = $target_id,
                m.change_type = $change_type, m.diff_summary = $diff,
                m.reason = $reason, m.session_id = $session_id, m.timestamp = $ts
            WITH m
            // AFFECTS 关系在调用方按具体实体类型关联
        ", params).await,

        "BugFix" => run_cypher_raw("
            MERGE (b:BugFix {fix_id: $entity_id})
            SET b.issue = $issue, b.root_cause = $root_cause,
                b.solution = $solution, b.files_changed = $files,
                b.session_id = $session_id, b.timestamp = $ts
        ", params).await,

        "Decision" => run_cypher_raw("
            MERGE (d:Decision {decision_id: $entity_id})
            SET d.title = $title, d.context = $context, d.alternatives = $alts,
                d.evidence = $evidence, d.choice = $choice, d.rationale = $rationale,
                d.consequences = $consequences, d.confidence = $conf,
                d.verified = $verified, d.session_id = $session_id, d.timestamp = $ts
        ", params).await,

        "PodEvent" => run_cypher_raw("
            MERGE (p:PodEvent {event_id: $entity_id})
            SET p.pod_name = $pod, p.namespace = $ns, p.phase = $phase,
                p.reason = $reason, p.message = $msg, p.node = $node,
                p.container = $container, p.restart_count = $restarts,
                p.session_id = $session_id, p.timestamp = $ts
            WITH p
            MATCH (si:ServiceInstance)  // 按 pod_name 前缀匹配
            WHERE $pod STARTS WITH si.name
            MERGE (p)-[:AFFECTS]->(si)
        ", params).await,

        "SoftwareInstalled" => run_cypher_raw("
            MERGE (s:Software {name: $entity_id})
            SET s.version = $version, s.installed_at = $ts
        ", params).await,

        "Conversation" => run_cypher_raw("
            MERGE (day:Day {day_id: $day_id})
            ON CREATE SET day.date = $day_id
            MERGE (s:Session {session_id: $entity_id})
            SET s.summary = $summary, s.key_decisions = $decisions,
                s.thread_id = $thread_id, s.started_at = $ts, s.ended_at = $ts
            MERGE (day)-[:HAS_SESSION]->(s)
        ", params).await,

        _ => warn!("unknown event_type: {}", event_type),
    }
}
```

### 7.2 各事件类型的触发点

| 事件类型 | 触发条件 | 谁来触发 | entity_id 示例 |
|----------|----------|----------|----------------|
| `Deploy` | Jenkins 构建完成 | MCP `jcli_build` | `"aflm-pay-deploy"` |
| `SoftwareInstalled` | apt/pip/npm 安装后 | AI 自动调用 | `"redis-tools"` |
| `ConfigChange` | 修改 Nacos 配置后 | AI 自动调用 | `"app.yml"` |
| `Conversation` | 用户说"结束" | Loom 触发 | `"2026-07-09"` |
| `BugFix` | Bug 修复完成 | AI 自动调用 | `"fix-payment-timeout"` |
| `Modification` | edit/write 代码后 | OpenCode Hook → `dt update` | 文件路径 |

### 7.3 时间线组织

```
(:Day {day_id: "2026-07-09"})
  └─[:HAS_SESSION]-> (:Session {session_id: "2026-07-09-001"})
       ├─[:HAS_EVENT]-> (:Modification)
       ├─[:HAS_EVENT]-> (:ConfigChange)
       ├─[:HAS_EVENT]-> (:Decision)
       └─[:HAS_EVENT]-> (:BugFix)
```

Session 节点由 `dt event --type Conversation` 创建，并关联当天 Day 节点。查询时沿时间线聚合最近 N 天的历史。

---

## 八、Semantic World：向量化管道

### 8.1 嵌入服务架构（V2 gRPC）

```
┌──────────────┐        gRPC         ┌─────────────────────┐
│  Rust dt CLI │ ◄──── proto ──────► │  Python dt-embed     │
│  (embedder)  │    localhost:50052  │  (gRPC server)       │
└──────────────┘                     └──────────┬──────────┘
                                                │
                                       ┌────────▼──────────┐
                                       │  BGE-M3 模型       │
                                       │  (1024 维, FP16)   │
                                       │  GPU 推理           │
                                       └───────────────────┘
```

### 8.2 通信协议（V2 gRPC）

dt-embed 通过标准 gRPC EmbedService 暴露 `Embed(batch) -> vectors`，替代 V1 自定义 Unix Socket 帧协议。proto 定义见 `proto/embed.proto`。

> **V1 遗留**：V1 使用 Unix Socket + 4 字节长度前缀帧 + JSON。V2 全部迁移到 gRPC，消除自定义协议。

**请求**：
```json
{"texts": ["public String pay(String orderId) { ... }", "..."]}
```

**响应**：
```json
{"vectors": [[0.123, -0.456, ...], [0.789, 0.012, ...]]}
```

### 8.3 Rust 客户端实现（V2 gRPC）

```rust
// embed.rs
use proto::embed::embed_service_client::EmbedServiceClient;
use proto::embed::EmbedRequest;
use tonic::transport::Channel;

pub struct EmbedClient {
    client: EmbedServiceClient<Channel>,
}

impl EmbedClient {
    pub async fn connect() -> Result<Self> {
        let channel = Channel::from_static("http://[::1]:50052")
            .connect()
            .await?;
        let client = EmbedServiceClient::new(channel);
        Ok(Self { client })
    }

    pub async fn embed_batch(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let request = tonic::Request::new(EmbedRequest { texts });
        let response = self.client.embed(request).await?;
        Ok(response.into_inner().vectors)
    }
}
```

**V2 变化：** 不再手写 Unix Socket + 4字节帧协议，改用标准 gRPC + protobuf 序列化。连接管理由 tonic channel 连接池自动处理，无需手动轮询 socket 文件。

### 8.4 Python daemon 实现（V2 gRPC server）

```python
# embed_server.py
import grpc
from concurrent import futures
from proto import embed_pb2, embed_pb2_grpc

class EmbedService(embed_pb2_grpc.EmbedServiceServicer):
    def __init__(self, engine, chunk_size=32):
        self.engine = engine
        self.chunk_size = chunk_size

    def Embed(self, request, context):
        all_vectors = []
        texts = request.texts
        for i in range(0, len(texts), self.chunk_size):
            batch = texts[i:i + self.chunk_size]
            vectors = self.engine.encode(batch)  # BGE-M3 → 1024维
            all_vectors.extend(vectors.tolist())
        return embed_pb2.EmbedResponse(vectors=all_vectors)

def serve(engine, port=50052):
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=4))
    embed_pb2_grpc.add_EmbedServiceServicer_to_server(
        EmbedService(engine), server
    )
    server.add_insecure_port(f"[::]:{port}")
    server.start()
    server.wait_for_termination()
```

**V2 变化：** 不再手写 Unix `socket.accept()` + `_recv_frame` / `_send_frame`，改用标准 gRPC server。protobuf 定义见 `proto/embed.proto`。

### 8.5 向量集合管理

每个项目有独立的 Qdrant Collection：

```
{project}_methods_{model_version}   ← 代码向量
{project}_semantic_{model_version}  ← 文档向量
kg_nodes_{model_version}            ← 知识图谱节点向量
```

### 8.6 KG ↔ Qdrant 桥接（`dt kg-sync`）

将 Memgraph 中非代码实体同步到 Qdrant，使 `dt search-kg` 能语义搜索 KG：

```
流程：
  1. MATCH (n) WHERE labels(n) IN [Server, Database,
        K8sDeployment, Service, ServiceInstance,
        Knowledge, Concept, Playbook, Experience, Domain,
        NacosConfig, NacosService, NacosNamespace, NacosGroup, NacosInstance,
        Document, Endpoint, ConfigKey, Table,
        Deployment, ConfigChange, BugFix, Decision, PodEvent,
        Thread, Requirement]
     [AND n._kg_synced_at IS NULL]   ← 增量模式
  2. 构建搜索文本：拼接 service_type + description + auth_user + hostname + url ...
  3. embed_batch(texts) → BGE-M3 向量
  4. Qdrant upsert (point_id = SHA256(elementId)[0..16] → UUID)
   5. SET n._kg_synced_at = datetime()     ← 标记已同步
```

### 8.7 增量同步的变更检测

`_kg_synced_at` 时间戳驱动增量同步，需在各实体写入路径中加入重置逻辑：

```rust
// dt-pipeline/src/coordinator.rs — 在所有实体写入完成后
pub async fn mark_entity_dirty(entity_id: &str) -> Result<()> {
    run_cypher("MATCH (n) WHERE elementId(n) = $eid
                REMOVE n._kg_synced_at", params! {eid: entity_id}).await
}
```

触发点：
- `dt nacos-sync` 写入/更新 NacosConfig → 先 `REMOVE n._kg_synced_at`
- `dt memorize` 创建 Knowledge 新版本 → 在旧节点上 `REMOVE _kg_synced_at`
- `dt k8s-sync` 更新 K8sDeployment → `REMOVE _kg_synced_at`
- 手动 `SET` 操作 → 同步重置

> ⚠️ 不重置 `_kg_synced_at` 会导致 Qdrant 向量数据陈旧，`dt search-kg` 返回已过时的语义搜索结果。

---

## 九、Runtime World：实时查询

Runtime 数据**不入库**，由 Context Builder 组装上下文时动态拉取。

### 9.1 Pod 状态（`dt kublog status`）

```bash
# 通过 Kuboard K8s API 实时查询
dt kublog status pods --namespace newoffen
```

返回当前 Pod 的 CPU/Memory/Phase/Restarts，不持久化。

### 9.2 Pod 日志（`dt kublog logs`）

```bash
# 通过 Kuboard WebSocket 流式拉取
dt kublog logs aflm-pay-7d8f9b6c-abcde --since 30m
```

MCP `kublog_logs` 封装了鉴权（Kuboard Bearer token）和 WebSocket 稳定性处理。

### 9.3 本地服务状态（`dt svc status`）

```bash
dt svc status aflm-pay
# → {"status": "running", "pid": 12345, "port": 8080, "uptime": "3d 12h"}
```

通过进程管理和端口检查实现，不入 Memgraph。

---

## 十、Reasoning World：推理缓存

### 10.1 写入时机

AI 在分析过程中产生中间结论时，自动写入 Reasoning：

```json
{
  "hypothesis": "切换支付平台可能影响 PayService 和 BusinessService",
  "entities": ["PayService", "BusinessService"],
  "method": "dependency_graph",
  "conclusion": "需改 ifCode + wayCode + merchantNo + channelExtra + DB",
  "confidence": 0.9,
  "session_id": "2026-07-09-001"
}
```

### 10.2 实现方式

目前 Reasoning World 由 AI（Loom/Agent）在会话中自行管理，写入方式与 Knowledge 共用 `dt memorize`：

```bash
dt memorize \
  --type Decision \
  --entity-id "analysis-pay-migration-20260709" \
  --entity-type Analysis \
  --project aflm \
  --details "hypothesis: 切换支付平台影响范围...; conclusion: 需改5处; confidence: 0.9"
```

### 10.3 生命周期管理

```
Observation (confidence < 0.7)
    ↓ 补充证据
Decision (confidence >= 0.7, verified = false)
    ↓ 执行确认
Knowledge (verified = true)  ← 升级为永久知识
    ↓
Memory.Decision (confirmed)  ← 归档为记忆
```

---

## 十一、Digital Thread：主线聚合

### 11.1 创建时机

当 AI 开始一个**有明确业务目标的任务**时，创建 Thread：

```bash
dt memorize \
  --type Decision \
  --entity-id "thread-pay-migration-tonglian-to-yinsheng" \
  --entity-type Thread \
  --project aflm \
  --details "name: 支付平台迁移：通联→银盛; status: active"
```

### 11.2 关联方式

后续所有操作通过 `thread_id` 字段关联：

```cypher
// 会话关联
MATCH (t:Thread {thread_id: "thread-pay-migration-..."})
MATCH (s:Session {session_id: "2026-07-09-001"})
MERGE (t)-[:HAS_SESSION]->(s)

// 决策关联
MATCH (t:Thread), (d:Decision {decision_id: "..."})
WHERE t.thread_id = "thread-pay-migration-..."
MERGE (t)-[:HAS_DECISION]->(d)

// 以此类推：HAS_MODIFICATION, HAS_DEPLOYMENT, HAS_KNOWLEDGE, HAS_PLAYBOOK
```

### 11.3 查询方式

```cypher
// 查询某条主线的完整演化链
MATCH (t:Thread {name: "支付平台迁移"})
OPTIONAL MATCH (t)-[:HAS_SESSION]->(s:Session)
OPTIONAL MATCH (t)-[:HAS_DECISION]->(d:Decision)
OPTIONAL MATCH (t)-[:HAS_MODIFICATION]->(m:Modification)
OPTIONAL MATCH (t)-[:HAS_DEPLOYMENT]->(dep:Deployment)
OPTIONAL MATCH (t)-[:HAS_KNOWLEDGE]->(k:Knowledge)
OPTIONAL MATCH (t)-[:HAS_PLAYBOOK]->(p:Playbook)
RETURN t, s, d, m, dep, k, p
```

---

## 十二、Metrics：gRPC 指标监控

### 12.1 设计原则

- **不暴露 HTTP 端口**：所有指标走 gRPC MetricsService，复用 :50051
- **无外部依赖**：基于 `tracing` crate 的 span 自动统计，无需 Prometheus exporter
- **双通道输出**：gRPC 实时查询 + 结构化日志 60s 快照

### 12.2 gRPC MetricsService

```protobuf
// proto/metrics.proto
service MetricsService {
    rpc GetMetrics(MetricsRequest) returns (MetricsResponse);
    rpc WatchMetrics(MetricsRequest) returns (stream MetricSnapshot);
}
```

### 12.3 内置指标

```
dt_build_duration_seconds{project, strategy}      histogram
dt_embed_requests_total{status}                   counter
dt_embed_queue_depth                               gauge
dt_memgraph_connection_pool_size                      gauge
dt_qdrant_write_bytes_total                        counter
dt_plugin_health_status{plugin}                    gauge (0/1)
dt_context_total_duration_seconds                  histogram
dt_context_world_query_duration{world}             histogram
dt_write_coordinator_active_locks                  gauge
dt_backup_last_success_timestamp                    gauge
```

### 12.4 日志快照

每 60s 将当前指标以 JSON 行写入日志文件，tag `_metric_snapshot`：
```json
{"ts":"...","level":"INFO","target":"dt::metrics","type":"_metric_snapshot","gauges":{...},"counters":{...}}
```

### 12.5 文件

- `proto/metrics.proto`
- `dt-daemon/src/grpc/metrics_service.rs`
- `dt-log/src/metrics.rs` — counter!/gauge!/histogram! 宏

---

## 十三、Backup & Archive：备份与归档

### 13.1 分层备份策略

| 存储 | 备份方式 | 频率 | 保留 |
|------|----------|------|------|
| Memgraph | `memgraph-admin database dump` | 每日 03:00 | 7 天滚动 |
| Qdrant | Collection snapshot API | 每日 03:30 | 7 天滚动 |
| SQLite | `cp lazy.db lazy.{date}.db` | 每次 `dt build` 前 | 30 天滚动 |

### 13.2 CLI

```bash
dt backup                          # 全量备份
dt backup --restore 2026-07-09     # 恢复到指定日期
dt backup --list                   # 列出可用备份
dt backup --verify 2026-07-09      # 验证完整性
```

### 13.3 Memory World 归档

```bash
dt archive --before 2026-01-01     # 归档旧 Event
dt archive --dry-run                # 预览
dt archive --list                   # 列出归档文件
```

归档格式：`/var/lib/dt/archive/{date_range}.json.gz`，每行一个 Event JSON。

### 13.4 文件

- `crates/dt-backup/` — 备份 crate
- `dt-daemon/src/archive.rs` — 归档逻辑
- `dt-storage/src/memgraph/repo.rs` — `archive_events_before()` 方法

---

## 十四、所需组件清单

### 14.1 运行中的服务

| 服务 | 端口 | 用途 |
|------|------|------|
| Memgraph | 7687 (bolt), 7474 (HTTP) | 图谱存储和查询 |
| Qdrant | 6333 (HTTP), 6334 (gRPC) | 向量存储和搜索 |
| dt-embed daemon | gRPC :50052 | BGE-M3 向量化推理 |
| dt-sync 脚本 | cron 定时 | 定期同步 Nacos/K8s/KG |
| MCP Server | gRPC client → dt daemon :50051 | OpenCode MCP 协议适配 |

### 14.2 二进制文件

| 文件 | 来源 | 功能 |
|------|------|------|
| `/usr/local/bin/dt-daemon` | `digital-twin-v2/` Cargo workspace | 核心 daemon（build/event/search/sync，gRPC :50051） |
| `/usr/local/bin/dt-embed` | `services/embed-server/` | Python gRPC 嵌入服务入口（:50052） |
| `/home/luis/.local/bin/digital-twin-mcp` | `mcp-server.py` | MCP 协议适配器（gRPC client） |

### 14.3 配置文件

| 文件 | 位置 | 关键配置 |
|------|------|----------|
| `config.yaml` | `/etc/digital-twin/` | Memgraph/Qdrant/Nacos/K8s 连接，项目列表 |
| `opencode.json` | `~/.config/opencode/` | MCP 注册，Hook 配置 |
| `dt-build.js` | `~/.config/opencode/skills/digital-twin/.opencode/plugins/` | OpenCode 编辑钩子 |

### 14.4 数据目录

| 路径 | 内容 |
|------|------|
| `/var/lib/digital-twin/lazy.db` | SQLite 文件快照（变更检测） |
| `/var/lib/digital-twin/snapshots/` | 项目快照备份 |
| `/var/lib/digital-twin/last-sync` | 最后同步时间戳 |
| `/var/log/digital-twin/` | 统一日志目录 |
| `/tmp/digital-twin-mcp.log` | MCP Server 日志 |
| `/tmp/dt-build-plugin.log` | OpenCode 插件日志 |

### 14.5 模型依赖

| 模型 | 用途 | 维度 |
|------|------|------|
| BAAI/bge-m3 | 文本向量化 | 1024 |
| tree-sitter (7 种语言) | AST 解析 | — |

### 14.6 `config.yaml` 结构

```yaml
server:
  hostname: "dev-server"

services:
  memgraph:
    url: "bolt://localhost:7687"
    user: "memgraph"
    password: "env:DT_MEMGRAPH_PASSWORD"
  qdrant:
    url: "grpc://localhost:6334"
  embed_server:
    url: "grpc://localhost:50052"
    dim: 1024
    model: "BAAI/bge-m3"
  nacos:
    test: "http://nacos-test:8848"
    prod: "http://nacos-prod:8848"
  k8s:
    server: "https://kuboard.example.com"
    username: "env:DT_K8S_USERNAME"
    password: "env:DT_K8S_PASSWORD"
    cluster_id: "cluster-001"

daemon:
  listen_addr: "[::]:50051"
  log_dir: "/var/log/digital-twin"

security:
  tls:
    ca_cert: "/etc/dt/ca.pem"
    server_cert: "/etc/dt/server.pem"
    server_key: "/etc/dt/server.key"
  jwt_secret: "env:DT_JWT_SECRET"

snapshot_dir: "/var/lib/digital-twin/snapshots"

scanner:
  ignore_dirs: ["node_modules", ".git", "target", "build", "__pycache__", ".venv"]
  ignore_ext: [".class", ".jar", ".war", ".so", ".dll", ".exe", ".bin", ".png", ".jpg"]
  max_file_size: 524288  # 500KB

projects:
  - base: /data/myProject
    items:
      - digital-twin-v2
      - aflm-pay
      - aflm-admin
```

### 14.7 定时任务（crontab）

```bash
# 每小时同步 Nacos 配置
0 * * * * dt nacos-sync --env test >> /var/log/dt-nacos-sync.log 2>&1

# 每小时同步 K8s 资源
30 * * * * dt k8s-sync >> /var/log/dt-k8s-sync.log 2>&1

# 每周日凌晨全量重建（修正增量遗漏）
0 3 * * 0 dt build-all >> /var/log/dt-build-all.log 2>&1

# 每天凌晨 KG→Qdrant 增量同步
0 2 * * * dt kg-sync --incremental >> /var/log/dt-kg-sync.log 2>&1

# 每天凌晨备份
0 3 * * * dt backup >> /var/log/dt-backup.log 2>&1

# 每周日凌晨自动清理过期数据
0 4 * * 0 dt cleanup --execute >> /var/log/dt-cleanup.log 2>&1

# 每月 1 号归档 365 天前的 Memory 数据
0 5 1 * * dt archive --before $(date -d '365 days ago' +%Y-%m-%d) >> /var/log/dt-archive.log 2>&1
```
