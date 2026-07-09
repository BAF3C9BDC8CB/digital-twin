# Digital Twin v2 数据管道实现文档

> 状态：设计阶段 | 日期：2026-07-09

本文档说明每个世界数据的**具体更新方式、实现逻辑和所需组件**。对照 [数据全链路设计](./architecture-v2-data-pipeline.md) 和 [数据格式定义](./architecture-v2-data-schema.md) 阅读。

---

## 目录

1. [总体架构](#一总体架构)
2. [Reality World：代码实体更新](#二reality-world代码实体更新)
3. [Reality World：基础设施同步](#三reality-world基础设施同步)
4. [Knowledge World：知识写入](#四knowledge-world知识写入)
5. [Memory World：事件写入](#五memory-world事件写入)
6. [Semantic World：向量化管道](#六semantic-world向量化管道)
7. [Runtime World：实时查询](#七runtime-world实时查询)
8. [Reasoning World：推理缓存](#八reasoning-world推理缓存)
9. [Digital Thread：主线聚合](#九digital-thread主线聚合)
10. [所需组件清单](#十所需组件清单)

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
│  Neo4j   │ │  SQLite │  │  Qdrant  │  │  dt-embed      │
│ (Cypher) │ │(snapshot)│  │ (REST)   │  │ (UnixSocket)   │
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
                        │  │  neo4rs (Bolt driver)      │   │
                        │  │  qdrant-client (gRPC)      │   │
                        │  │  rusqlite (SQLite local)    │   │
                        │  └───────────────────────────┘   │
                        └──────┬──────┬───────┬────────────┘
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

统一要点：
- 全链路使用 protobuf 序列化，类型安全，编译期校验
- gRPC HTTP/2 多路复用，连接池，无需每次新建连接
- dt CLI 变为常驻 daemon（systemd socket activated），不再 subprocess 启动
- Neo4j 走 Bolt 二进制协议（原生驱动，无法用 gRPC 替代），持久连接池
- dt-embed 不再手写 Unix Socket 帧协议，改用标准 gRPC
- MCP Server 不再解析 stdout，改为结构化 gRPC 调用
```

**旧架构（V1）— 已废弃：**
```
Rust dt CLI ──HTTP POST──▶ Neo4j    (REST Cypher, 每次新建连接)
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
│    neo4j::health()  → 确保 Neo4j 可连接        │
│    neo4j::ensure_schema() → 创建约束/索引      │
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
│    neo4j::delete_methods_by_files(changed)    │
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
│    c. neo4j::write_methods_batch(batch=2000)  │
├──────────────────────────────────────────────┤
│ 8. 类关系 + 快照 + 调用图                      │
│    neo4j::write_classes_batch() → CONTAINS    │
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
  1. 读取文件 → 从 Qdrant + Neo4j 删除该文件的旧方法
  2. tree-sitter 解析 → 提取 MethodBlock + ClassBlock
  3. 向量嵌入 → 写入 Qdrant + Neo4j
  4. 写入类关系（CONTAINS）
  5. 增量重建该文件的调用图（CALLS）
  6. 更新 SQLite 快照
```

### 2.5 Neo4j 写入查询

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

**Collection 创建**：

```json
PUT /collections/{project}_methods
{
  "vectors": { "size": 1024, "distance": "Cosine" },
  "hnsw_config": { "m": 16, "ef_construct": 100 },
  "optimizers_config": { "indexing_threshold": 10000 }
}
```

**向量写入**（每批 1000 条）：

```json
PUT /collections/{project}_methods/points
{
  "points": [
    {
      "id": 12345678901234567890,     // SHA256(method_id)[0..8] → u64
      "vector": [0.123, -0.456, ...],  // 1024 维 float32
      "payload": {
        "entity_id": "abc123...",
        "name": "pay",
        "signature": "public String pay(...)",
        "class_name": "PayService",
        "file_path": "src/main/java/.../PayService.java",
        "language": "java",
        "start_line": 42,
        "end_line": 89,
        "project": "aflm-pay",
        "search_text": "Project: aflm-pay\nMethod: pay\nCalls: ..."
      }
    }
  ]
}
```

**按文件删除旧点**：

```json
POST /collections/{project}_methods/points/delete
{
  "filter": {
    "must": [{ "key": "file_path", "match": { "value": "src/.../PayService.java" } }]
  }
}
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
    └─ 交叉链接:
        ├─ NacosService.name ↔ K8sService.name → EXPOSED_BY
        └─ Environment ↔ K8s Namespace → DEPLOYS_TO
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
        │   └─ MERGE (:Deployment {name, namespace, image,
        │          replicas, available, strategy, condition})
        │   └─ (:Namespace)-[:HAS_DEPLOYMENT]->(:Deployment)
        │
        ├─ GET /api/v1/namespaces/{ns}/pods
        │   └─ MERGE (:K8sPod {name, namespace, ip, node, phase, restarts})
        │   └─ 通过 ownerReferences 关联到 Deployment
        │   └─ (:Namespace)-[:HAS_POD]->(:K8sPod)
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
        ├─ NacosInstance ↔ K8sPod (IP 匹配)
        └─ Deployment ↔ NacosConfig (名称前缀匹配)
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

`dt build-all` 时一并索引到 Neo4j。

---

## 四、Knowledge World：知识写入

### 4.1 六个来源的写入方式

| 来源 | 触发 | 写入方式 | 实现位置 |
|------|------|----------|----------|
| AI 会话自动提取 | 会话结束 | `dt event --type Conversation` → Knowledge 提取 | MCP `dt_event` |
| AI 任务主动沉淀 | 任务完成 | `dt memorize --type KnowledgeAdded` | MCP `dt_memorize` |
| 文档自动解析 | `dt build` 扫描 document_dirs | 同代码索引流程（tree-sitter → chunk → embed） | `dt build` |
| 代码注释标记 | `dt build` AST 解析 | 识别 `@knowledge` 注释 → MERGE Knowledge | `dt build` |
| 执行结果自动采集 | AI 执行工具后 | `dt memorize --type KnowledgeAdded --details "{工具名}: {结果摘要}"` | MCP `dt_memorize` |
| 用户口述 | 用户说"记住" | `dt memorize --type KnowledgeAdded` | MCP `dt_memorize` |

### 4.2 `dt memorize` 实现

**Rust 侧**（`knowledge.rs`）：

```rust
pub async fn write_knowledge(
    knowledge_type: &str,  // Decision | KnowledgeAdded | Environment | Dependencies
    entity_id: &str,
    entity_type: &str,     // ArchitectureDecision | Concept | ...
    project: &str,
    details: &str,         // "decision: ...; reason: ...; scope: ..."
) -> Result<()> {
    // 1. SHA256 生成唯一 ID（去重）
    let id = hex::encode(&sha256("knowledge::entity_id::ts")[..20]);

    // 2. 解析结构化字段
    //    "root_cause: X; fix: Y; decision: Z; reason: W"
    let (root_cause, fix_summary, decision, reason) = parse_details(details);

    // 3. MERGE Knowledge 节点
    run_cypher_raw("
        MERGE (k:Knowledge {id: $id})
        ON CREATE SET
            k.name = $name, k.title = $title,
            k.context = $knowledge_type, k.entity_type = $entity_type,
            k.details = $details, k.description = $description,
            k.project = $project,
            k.root_cause = $root_cause, k.fix_summary = $fix_summary,
            k.updatedAt = $ts, k.updatedBy = 'dt'
    ", params).await
}
```

### 4.3 执行结果自动采集

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

## 五、Memory World：事件写入

### 5.1 事件写入流程

所有事件通过 `dt event` 命令写入，由 MCP Tool 或 OpenCode Hook 自动触发：

```rust
// event.rs
pub async fn write_event(
    event_type: &str,    // Deploy | SoftwareInstalled | ConfigChange | Conversation
    entity_id: &str,     // 唯一标识
    entity_type: &str,   // JenkinsJob | Software | NacosConfig | Session
    project: &str,
    details: &str,       // JSON 或自然语言描述
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let event_id = hex::encode(&sha256(
        format!("{}::{}::{}", event_type, entity_id, ts)
    )[..20]);

    // MERGE Event 节点（event_id 唯一约束去重）
    run_cypher_raw("
        MERGE (e:Event {event_id: $event_id})
        SET e.type = $event_type, e.entity_id = $entity_id,
            e.entity_type = $entity_type, e.project = $project,
            e.details = $details, e.timestamp = $ts
        WITH e
        // 条件关联到已有实体
        FOREACH (_ IN CASE WHEN $event_type = 'Deploy' THEN [1] ELSE [] END |
            MERGE (j:JenkinsJob {name: $entity_id})
            MERGE (e)-[:DEPLOYED_JOB]->(j)
        )
        FOREACH (_ IN CASE WHEN $event_type = 'SoftwareInstalled' THEN [1] ELSE [] END |
            MERGE (s:Software {name: $entity_id})
            MERGE (e)-[:INSTALLED_SOFTWARE]->(s)
        )
        FOREACH (_ IN CASE WHEN $entity_type = 'Project' THEN [1] ELSE [] END |
            MERGE (p:Project {name: $project})
            MERGE (e)-[:INDEXED_PROJECT]->(p)
        )
    ", params).await
}
```

### 5.2 各事件类型的触发点

| 事件类型 | 触发条件 | 谁来触发 | entity_id 示例 |
|----------|----------|----------|----------------|
| `Deploy` | Jenkins 构建完成 | MCP `jcli_build` | `"aflm-pay-deploy"` |
| `SoftwareInstalled` | apt/pip/npm 安装后 | AI 自动调用 | `"redis-tools"` |
| `ConfigChange` | 修改 Nacos 配置后 | AI 自动调用 | `"app.yml"` |
| `Conversation` | 用户说"结束" | Loom 触发 | `"2026-07-09"` |
| `BugFix` | Bug 修复完成 | AI 自动调用 | `"fix-payment-timeout"` |
| `Modification` | edit/write 代码后 | OpenCode Hook → `dt update` | 文件路径 |

### 5.3 时间线组织

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

## 六、Semantic World：向量化管道

### 6.1 嵌入服务架构

```
┌──────────────┐     Unix Socket      ┌─────────────────────┐
│  Rust dt CLI │ ◄──── JSON ────────► │  Python dt-embed     │
│  (embed.rs)  │    /tmp/dt-embed.sock│  (cli.py daemon)     │
└──────────────┘                      └──────────┬──────────┘
                                                 │
                                        ┌────────▼──────────┐
                                        │  BGE-M3 模型       │
                                        │  (1024 维, FP16)   │
                                        │  GPU 推理           │
                                        └───────────────────┘
```

### 6.2 通信协议

**帧格式**：4 字节大端长度前缀 + JSON 负载

```
┌────────────────┬──────────────────────────┐
│ 4 bytes (BE)   │ N bytes                   │
│ payload length │ JSON payload              │
└────────────────┴──────────────────────────┘
```

**请求**：
```json
{"texts": ["public String pay(String orderId) { ... }", "..."]}
```

**响应**：
```json
{"vectors": [[0.123, -0.456, ...], [0.789, 0.012, ...]]}
```

### 6.3 Rust 客户端实现

```rust
// embed.rs
pub async fn embed_batch(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    tokio::task::spawn_blocking(move || {
        // 1. 连接 daemon（自动启动如果未运行）
        let mut stream = connect_daemon()?;

        // 2. 发送请求
        let request = json!({"texts": texts});
        let payload = serde_json::to_vec(&request)?;
        write_frame(&mut stream, &payload)?;  // 4字节长度 + JSON

        // 3. 读取响应
        let resp_bytes = read_frame(&mut stream)?;  // 读4字节长度 + payload
        let response: Value = serde_json::from_slice(&resp_bytes)?;

        Ok(response["vectors"].as_array()...)
    }).await
}

fn connect_daemon() -> Result<UnixStream> {
    match UnixStream::connect("/tmp/dt-embed.sock") {
        Ok(s) => Ok(s),
        Err(_) => {
            // 自动启动 daemon
            Command::new("dt-embed").arg("--daemon").spawn()?;
            // 轮询等待 socket 创建（最多 30 秒）
            for _ in 0..60 {
                if Path::new("/tmp/dt-embed.sock").exists() { break; }
                sleep(Duration::from_millis(500));
            }
            UnixStream::connect("/tmp/dt-embed.sock")
        }
    }
}
```

### 6.4 Python daemon 实现

```python
# cli.py cmd_daemon()
def cmd_daemon(engine, chunk_size=32):
    sock_path = "/tmp/dt-embed.sock"
    if os.path.exists(sock_path):
        os.unlink(sock_path)

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(sock_path)
    os.chmod(sock_path, 0o666)
    server.listen(5)

    while True:
        conn, _ = server.accept()
        try:
            raw = _recv_frame(conn)       # 读 4字节长度 + payload
            req = orjson.loads(raw)
            texts = req["texts"]

            # 批量推理
            all_vectors = []
            for i in range(0, len(texts), chunk_size):
                batch = texts[i:i + chunk_size]
                vectors = engine.encode(batch)  # BGE-M3 → 1024维
                all_vectors.extend(vectors.tolist())

            resp = orjson.dumps({"vectors": all_vectors}, option=orjson.OPT_SERIALIZE_NUMPY)
            _send_frame(conn, resp)
        finally:
            conn.close()
```

### 6.5 向量集合管理

每个项目有独立的 Qdrant Collection：

```
{project}_methods   ← 代码向量（Method 节点）
{project}_semantic  ← 文档向量（Document 节点）
kg_nodes            ← 知识图谱节点向量（KG → Qdrant 桥接）
```

### 6.6 KG ↔ Qdrant 桥接（`dt kg-sync`）

将 Neo4j 中非代码实体同步到 Qdrant，使 `dt search-kg` 能语义搜索 KG：

```
流程：
  1. MATCH (n) WHERE labels(n) IN [Infrastructure, Server, Database,
       Project, Environment, Knowledge, Software, Configuration,
       NacosConfig, NacosService, NacosNamespace]
     [AND n._kg_synced_at IS NULL]   ← 增量模式
  2. 构建搜索文本：拼接 service_type + description + auth_user + hostname + url ...
  3. embed_batch(texts) → BGE-M3 向量
  4. Qdrant upsert (point_id = SHA256(elementId)[0..16] → UUID)
  5. SET n._kg_synced_at = datetime()     ← 标记已同步
```

---

## 七、Runtime World：实时查询

Runtime 数据**不入库**，由 Context Builder 组装上下文时动态拉取。

### 7.1 Pod 状态（`dt kublog status`）

```bash
# 通过 Kuboard K8s API 实时查询
dt kublog status pods --namespace newoffen
```

返回当前 Pod 的 CPU/Memory/Phase/Restarts，不持久化。

### 7.2 Pod 日志（`dt kublog logs`）

```bash
# 通过 Kuboard WebSocket 流式拉取
dt kublog logs aflm-pay-7d8f9b6c-abcde --since 30m
```

MCP `kublog_logs` 封装了鉴权（Kuboard Bearer token）和 WebSocket 稳定性处理。

### 7.3 本地服务状态（`dt svc status`）

```bash
dt svc status aflm-pay
# → {"status": "running", "pid": 12345, "port": 8080, "uptime": "3d 12h"}
```

通过进程管理和端口检查实现，不入 Neo4j。

---

## 八、Reasoning World：推理缓存

### 8.1 写入时机

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

### 8.2 实现方式

目前 Reasoning World 由 AI（Loom/Agent）在会话中自行管理，写入方式与 Knowledge 共用 `dt memorize`：

```bash
dt memorize \
  --type Decision \
  --entity-id "analysis-pay-migration-20260709" \
  --entity-type Analysis \
  --project aflm \
  --details "hypothesis: 切换支付平台影响范围...; conclusion: 需改5处; confidence: 0.9"
```

### 8.3 生命周期管理

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

## 九、Digital Thread：主线聚合

### 9.1 创建时机

当 AI 开始一个**有明确业务目标的任务**时，创建 Thread：

```bash
dt memorize \
  --type Decision \
  --entity-id "thread-pay-migration-tonglian-to-yinsheng" \
  --entity-type Thread \
  --project aflm \
  --details "name: 支付平台迁移：通联→银盛; status: active"
```

### 9.2 关联方式

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

### 9.3 查询方式

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

## 十、所需组件清单

### 10.1 运行中的服务

| 服务 | 端口 | 用途 |
|------|------|------|
| Neo4j | 7687 (bolt), 7474 (HTTP) | 图谱存储和查询 |
| Qdrant | 6333 (HTTP), 6334 (gRPC) | 向量存储和搜索 |
| dt-embed daemon | Unix socket `/tmp/dt-embed.sock` | BGE-M3 向量化推理 |
| dt-sync 脚本 | cron 定时 | 定期同步 Nacos/K8s/KG |
| MCP Server | 子进程 | OpenCode MCP 协议适配 |

### 10.2 二进制文件

| 文件 | 来源 | 功能 |
|------|------|------|
| `/usr/local/bin/dt` | `engine-rust/` 编译 | 核心 CLI（build/event/search/sync） |
| `/usr/local/bin/dt-embed` | `services/embed-server/` | Python 嵌入服务入口 |
| `/home/luis/.local/bin/digital-twin-mcp` | `mcp-server.py` | MCP 协议适配器 |

### 10.3 配置文件

| 文件 | 位置 | 关键配置 |
|------|------|----------|
| `config.yaml` | `~/.config/opencode/skills/digital-twin/` | Neo4j/Qdrant/Nacos/K8s 连接，项目列表 |
| `opencode.json` | `~/.config/opencode/` | MCP 注册，Hook 配置 |
| `dt-build.js` | `~/.config/opencode/skills/digital-twin/.opencode/plugins/` | OpenCode 编辑钩子 |

### 10.4 数据目录

| 路径 | 内容 |
|------|------|
| `/var/lib/digital-twin/lazy.db` | SQLite 文件快照（变更检测） |
| `/var/lib/digital-twin/snapshots/` | 项目快照备份 |
| `/var/lib/digital-twin/last-sync` | 最后同步时间戳 |
| `/tmp/dt-embed.sock` | 嵌入服务 Unix socket |
| `/tmp/digital-twin-mcp.log` | MCP Server 日志 |
| `/tmp/dt-build-plugin.log` | OpenCode 插件日志 |

### 10.5 模型依赖

| 模型 | 用途 | 维度 |
|------|------|------|
| BAAI/bge-m3 | 文本向量化 | 1024 |
| tree-sitter (7 种语言) | AST 解析 | — |

### 10.6 `config.yaml` 结构

```yaml
server:
  hostname: "dev-server"

services:
  neo4j:
    url: "http://localhost:7474"
    user: "neo4j"
    password: "***"
  qdrant:
    url: "http://localhost:6333"
  embed_server:
    url: "unix:///tmp/dt-embed.sock"
    dim: 1024
    model: "BAAI/bge-m3"
  nacos:
    test: "http://nacos-test:8848"
    prod: "http://nacos-prod:8848"
  k8s:
    server: "https://kuboard.example.com"
    username: "admin"
    password: "***"
    cluster_id: "cluster-001"

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

### 10.7 定时任务（crontab）

```bash
# 每小时同步 Nacos 配置
0 * * * * dt nacos-sync --env test >> /var/log/dt-nacos-sync.log 2>&1

# 每小时同步 K8s 资源
30 * * * * dt k8s-sync >> /var/log/dt-k8s-sync.log 2>&1

# 每周日凌晨全量重建（修正增量遗漏）
0 3 * * 0 dt build-all >> /var/log/dt-build-all.log 2>&1

# 每天凌晨 KG→Qdrant 增量同步
0 2 * * * dt kg-sync --incremental >> /var/log/dt-kg-sync.log 2>&1
```
