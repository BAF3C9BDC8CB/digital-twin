# Digital Twin v2 数据全链路设计

> ⚠️ **DEPRECATED**: 本文档已被 [V3 单 Crate 分层架构](./architecture-v3-single-crate-layered.md) 替代。
> V2 多 crate workspace 方案已废弃，实际实现采用单 crate 内部模块分层。
> 保留本文档仅供历史参考。

> 状态：设计阶段 | 日期：2026-07-09（已刷新：新增 WriteCoordinator、安全鉴权、指标采集、备份归档管道）

## 一、总览：数据流全景

```
                     ┌──────────────────────────────────────┐
                     │            触发层（多通道）            │
                     ├──────────┬──────────┬────────────────┤
                     │ OpenCode │ inotify  │ 定时 / 手动     │
                     │ Hook     │ daemon   │ CLI / cron     │
                     └────┬─────┴────┬─────┴───────┬────────┘
                          │          │             │
                          └──────────┼─────────────┘
                                     │
                          ┌──────────▼──────────┐
                          │  WriteCoordinator    │
                          │  并发写入串行化       │
                          └──────────┬──────────┘
                                     │
              ┌──────────────────────┼──────────────────────┐
              │                      │                      │
              ▼                      ▼                      ▼
    ┌─────────────────┐   ┌─────────────────┐   ┌─────────────────┐
    │  Entity Extract │   │  Embedding      │   │  Event Handler  │
    │  (tree-sitter)  │   │  (BGE-M3 1024)  │   │  (AI 操作事件)   │
    └────────┬────────┘   └────────┬────────┘   └────────┬────────┘
             │                     │                     │
             ▼                     ▼                     ▼
    ┌─────────────────────────────────────────────────────────────┐
    │                      Neo4j + Qdrant                         │
    │  Reality / Knowledge / Memory / Reasoning → Neo4j            │
    │  Semantic → Qdrant                                          │
    └─────────────────────────────────────────────────────────────┘
                                     │
                          ┌──────────▼──────────┐
                          │   Context Builder   │
                          └──────────┬──────────┘
                                     │
                          ┌──────────▼──────────┐
                          │   MCP Interface     │
                          │ (+ backup/archive/  │
                          │  cleanup/metrics)   │
                          └─────────────────────┘
```

---

## 二、统一更新机制：`dt update`

不再依赖 Git Hook，改为**多通道触发器 + 统一脚本**。

```
dt update --file <abs_path> [--type create|modify|delete] [--project <name>]

执行流程：
  1. 解析文件 → 提取 Entity（tree-sitter AST）
  2. SHA1 比对 → 变更检测（SQLite 快照）
  3. Neo4j upsert（Entity 节点 + 关系）
  4. Qdrant upsert（增量嵌入，仅变更 chunk）
  5. 更新 SQLite 快照
```

### 通道 1：OpenCode Hook（主要通道）

```json
// ~/.config/opencode/opencode.json
{
  "hooks": {
    "tool.execute.after": {
      "edit": "dt update --file $FILE_PATH &",
      "write": "dt update --file $FILE_PATH &",
      "bash": {
        "command_patterns": {
          "rm *": "dt update --file $FILE_PATH --type delete &"
        }
      }
    }
  }
}
```

LLM 每次 `edit`/`write` 后**异步触发**，不阻塞会话。

### 通道 2：文件监视器 Daemon（通用兜底）

```bash
dt watch    # 启动后台 daemon
            # inotify/fswatch 监听所有 config.yaml projects 路径
            # 检测 .java/.py/.ts/.go/.rs/.php 等源码文件变更
            # 变更后自动调用 dt update --file <path>
```

适用场景：
- 用户在 IDE/终端手动编辑文件
- 非 OpenCode 环境（VSCode、IntelliJ、vim 等）

### 通道 3：手动 CLI

```bash
dt update --file /path/to/PayService.java
dt update --project aflm --all          # 全量重建某项目
dt update --path /data/aflmProjects     # 批量扫描目录
```

### 通道 4：定时全量兜底

```bash
# crontab 每周日凌晨全量比对，修正增量遗漏
0 3 * * 0 dt build-all
```

---

## 三、各世界数据来源

### Reality World（事实世界）

| 实体类型 | 来源 | 采集方式 |
|----------|------|----------|
| Code (Method/Class/Module/Package) | 源码文件系统 | `dt update` → tree-sitter AST 解析 → SHA1 增量检测 |
| Database (MySQL/Redis/Kafka) | Nacos 配置中心 / K8s ConfigMap | `nacos-sync` 解析连接串；手动注册 |
| Server (物理机/Pod/Container) | K8s API (Kuboard) | `k8s-sync` 扫描 Node/Deployment |
| Config (NacosCfg/EnvVar) | Nacos / Apollo / 本地 .env | `nacos-sync` 解析配置项，关联到 Service |
| API (Endpoint/RPC) | 源码注解扫描 | tree-sitter 提取 @RestController/@GetMapping/FeignClient |
| Document (Markdown/PDF) | 本地文件系统 document_dirs | `dt build` 扫描 → 文本提取 → chunk 分块 → Embedding |

**更新机制：**
- 代码变更 → OpenCode Hook / inotify daemon → `dt update`
- 配置变更 → 定时 `nacos-sync`（每小时）
- 基础设施变更 → 定时 `k8s-sync`（每小时）
- 手动 → `dt build --full` 全量重建

---

### Knowledge World（知识世界）

六个来源，按优先级：

| 优先级 | 来源 | 触发条件 | 说明 |
|--------|------|----------|------|
| ⭐1 | **AI 会话自动提取** | 会话结束自动触发 | 从 Session 中提取关键发现、新概念、决策 |
| ⭐2 | **AI 任务后主动沉淀** | `dt_learn` MCP | 任务完成后 AI 主动调用，记录模式、经验、踩坑 |
| ⭐3 | **文档自动解析** | `dt build` 扫描 document_dirs | md/pdf → 提取领域术语、架构概念 |
| ⭐4 | **代码注释提取** | `dt build` AST 解析 | `@knowledge` 标记 → 自动创建 Knowledge 节点 |
| ⭐5 | **执行结果自动采集** | AI 执行工具后自动判断 | `kubectl`/`mysql`/`curl`/`docker` 等命令的返回中有长期价值的部分。黑名单跳过（ls/cat/echo/cd/pwd/grep/find） |
| ⭐6 | **用户口述** | 用户说"记住" | `dt memorize` 兜底方案 |

#### 来源 1+2 详细流程（AI 自主采集，核心）

```
AI 完成 "支付平台从通联切银盛" 任务
  ↓
修改了 5 文件 + 2 Nacos 配置 + 1 数据库
  ↓
dt_learn({
  task:        "支付平台迁移：通联 → 银盛",
  entities:    [PayService, BusinessService, NacosCfg],
  pattern:     "ifCode + wayCode + merchantNo + DB",
  pitfalls:    ["别忘了 channelExtra", "回调地址要改"],
  success:     true
})
  ↓
生成:
  (:Knowledge {name: "支付平台迁移模式", domain: "支付"})
  (:Experience {pitfall: "别忘了 channelExtra"})
  (:Playbook {steps: [查经验→改ifCode→改wayCode→改merchantNo→...]})
```

**关键设计：AI 决定记什么。Skill 只要求"任务完成后必须调用 dt_learn"，内容由 AI 自主判断。**

#### 来源 4：代码注释标记

```java
/**
 * @knowledge domain="支付" concept="ifCode"
 * 支付渠道编码，用于路由到不同支付平台。
 * 通联=allinpay, 银盛=ysf, 微信=wechat, 支付宝=alipay
 */
private String ifCode;
```

`dt build` 解析到 `@knowledge` → 创建 `(:Concept {name:"ifCode", definition:"...", domain:"支付"})`

#### 来源 5：执行结果自动采集

AI 执行工具命令后，将返回结果中有长期价值的结构化信息自动沉淀为 Knowledge。

| 执行示例 | 采集内容 | 沉淀到 |
|----------|----------|--------|
| `mysql -e "show create table"` | 表结构 DDL | Reality → Schema Entity |
| `docker inspect <container>` | 容器配置 | Reality → Config Entity |
| `curl <API>` | OpenAPI / Swagger | Knowledge → API Concept |
| `kubectl describe` | K8s 资源详情 | Reality → Resource Entity |
| `git log --oneline` | 提交历史上下文 | Knowledge → Context |

**触发方式：**
- AI 执行 bash 等工具后，自动判断返回值是否有长期价值
- 有价值 → 写入 Knowledge（`dt_learn` 或事件写入）
- 无价值（临时查询、一次性调试）→ 不入库

**去重机制：** 同一实体（如表、API、容器）的新执行结果覆盖旧结果（upsert by entity_id），保持 Reality 最新。

---

### Memory World（记忆世界）

全部由 AI 操作自动触发，事件溯源模式。

| 触发操作 | 生成的 Memory 节点 |
|----------|-------------------|
| edit/write 代码 | `(:Modification {file, diff_summary, reason})` |
| Jenkins 部署 | `(:Deployment {job, env, version})` |
| Nacos 配置变更 | `(:ConfigChange {key, old_val, new_val})` |
| Bug 修复 | `(:BugFix {issue, root_cause, solution})` |
| 会话结束 | `(:Session {summary, key_decisions})` |
| 架构决策 | `(:Decision {context, choice, rationale})` |

**生命周期（两级设计）：**
- **Memory Event**：所有 Event 节点 TTL 365 天。超期 → `dt archive` 导出为 `.json.gz`，级联清理孤儿 Session/Day。
- **Reasoning**：两级生命周期：阶段一（弃用）— 会话结束时 `SET _stale_at = timestamp()`，节点不可被 Context Builder 查询；阶段二（删除）— `dt cleanup` 每夜清理 `_stale_at` 距今 > 30 天的节点，30 天窗口内仍可被 `dt history` 审计回溯。

**归档级联清理策略：** `dt archive` 执行后，自动清理孤立的父节点：
1. 删除超期 Event 节点及其 `[:HAS_EVENT]` 关系
2. `MATCH (s:Session) WHERE NOT (s)-[:HAS_EVENT]->()` DETACH DELETE 所有无事件的孤儿 Session
3. `MATCH (d:Day) WHERE NOT (d)-[:HAS_SESSION]->()` DETACH DELETE 所有无 Session 的孤儿 Day
4. 归档文件名包含清理统计：`{date_range}_{event_count}_{session_count}_{day_count}.json.gz`

**更新机制：**
- 只增不删（immutable），完整审计日志
- 按时间线组织：`Day → Session → Event` 链
- `dt_context` 查询时沿时间线聚合最近 N 天

---

### Semantic World（语义世界）

所有可向量化的文本，存入 Qdrant。

| 文本类型 | 来源 | 关联 |
|----------|------|------|
| Code snippets | tree-sitter 提取的方法体+签名 | `entity_id` → Neo4j Code Entity |
| Documents | document_dirs 中的 md/pdf/txt | `entity_id` → Neo4j Document Entity |
| Config values | Nacos 配置项的值 | `entity_id` → Neo4j Config Entity |
| API descriptions | 接口注解 + Javadoc | `entity_id` → Neo4j API Entity |
| Log patterns | K8s 日志中提取的错误模板 | 独立向量 |
| Experience | Memory World 中的经验节点 | `entity_id` → Neo4j Experience Entity |

**生成方式：** 文本 chunk → BGE-M3 1024维 → Qdrant Collection `{project}_semantic_{model_version}`

**更新机制：** 与 Reality World 联动 —— 代码变 → 对应 chunk 重新 embedding → Qdrant upsert（by entity_id）

---

### Runtime World（实时世界）

**不入库，实时查询。** `dt_context` 组装上下文时动态拉取。

| 数据 | 来源 | 查询方式 |
|------|------|----------|
| CPU/Memory/Pod 状态 | K8s Metrics API | `kublog_status` |
| Pod 日志 | Kuboard | `kublog_logs` |
| 服务运行状态 | 本地进程管理 | `svc_status` |
| JVM Heap/Thread | Spring Actuator（未来） | HTTP API |
| Redis 连接数 | Redis INFO（未来） | 直连查询 |

---

### Reasoning World（推理世界）

AI 推理过程缓存，提高重复任务效率。实体类型：Observation → Analysis → Decision（三层递进），详见 [数据格式文档](./architecture-v2-data-schema.md#六reasoning-world推理世界)。

**生成方式：**
```
AI 分析出 "支付平台切换需要改 5 个地方"
  ↓
逐层沉淀：

1. (:Observation {                              ← 发现现象
     description:  "PayService 和 BusinessService 结构高度相似",
     evidence:     "两者都有 payChannel、merchant、callback 三层",
     confidence:   0.7
   })

2. (:Analysis {                                 ← 分析过程
     question:     "切换支付平台影响哪些文件？",
     hypothesis:   "需改 5 处",
     conclusion:   "ifCode + wayCode + merchantNo + channelExtra + DB",
     confidence:   0.9,
     session_id:   "2026-07-09-001"
   })
     └─[:PRODUCED]→ (:Decision {               ← 最终决策
          choice:       "切换到银盛",
          rationale:    "费率低 0.1%，API 兼容好",
          confidence:   0.9,
          verified:     false
        })
```

**更新机制：**
- 会话内有效
- AI 确认结论正确 → `dt_learn` 升级为永久 Knowledge
- 未验证的推理在会话结束后降级或丢弃

---

## 四、关键脚本清单

| 脚本 | 功能 | 触发方式 |
|------|------|----------|
| `dt update` | 单文件增量更新 | Hook / daemon / CLI |
| `dt watch` | 启动文件监视 daemon | 手动启动，常驻后台 |
| `dt build` | 项目全量/增量构建 | CLI / cron |
| `dt build-all` | 所有项目批量构建 | CLI / cron |
| `dt nacos-sync` | 同步 Nacos 配置 | 定时 / 手动 |
| `dt k8s-sync` | 同步 K8s 资源 | 定时 / 手动 |
| `dt kg-sync` | KG 节点同步到 Qdrant | 增量自动 / 手动 |
| `dt backup` | Neo4j/Qdrant/SQLite 分层备份 | CLI / cron |
| `dt archive` | Memory.Event 超期归档 | CLI / cron |
| `dt cleanup` | 按 TTL 策略清理过期数据 | CLI / cron |
| `dt metrics` | gRPC 指标查询（无 HTTP） | CLI |
