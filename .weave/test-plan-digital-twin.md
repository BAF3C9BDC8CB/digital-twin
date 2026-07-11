# Digital-Twin 完整功能测试计划 v2

> 当前环境: Neo4j ✅ | Qdrant ✅ | ~12 项目已索引 | ~13K KG 节点 | 40+ Labels
> 技能路径: `/home/luis/.config/opencode/skills/digital-twin/`
> 配置文件: `/data/myProject/digital-twin-v2/config.yaml`

---

## 测试覆盖矩阵

本计划覆盖 **28 个 CLI 命令** + **22 个 MCP Tool** + **11 份 Skill Guide 规则** 的完整验证。

---

## A. CLI 命令全覆盖（28 个）

### A1. 构建与索引 (3)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A1.1 | `dt build` | 构建单个项目 | 方法/类数>0, BELONGS_TO 关系完整, 无孤立节点 |
| A1.2 | `dt build` (增量) | 修改1个文件后重新构建 | 只处理变更文件, SHA1 hash 比对正确 |
| A1.3 | `dt build` (错误路径) | 传不存在的 --path | 不 crash, 友好报错 |
| A1.4 | `dt update` | 单文件更新 --file --project | 单文件正确更新到 KG, 不需要全量构建 |
| A1.5 | `dt update --type delete` | 删除单个文件的索引 | 该文件对应 Method/Class 从 KG 移除 |

### A2. 搜索与发现 (4)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A2.1 | `dt search` | 按项目搜索 | 返回结果含 name/file_path/start_line/signature/score |
| A2.2 | `dt search --path` | 按目录路径搜索 | 自动匹配路径下所有项目, 结果合并 |
| A2.3 | `dt search --expand` | 扩展搜索 | 结果≥普通搜索, 多变体去重合并 |
| A2.4 | `dt search --all` | 跨所有项目搜索 | 结果可能来自多个项目 |
| A2.5 | `dt search --json` | JSON 输出 | 格式正确可解析 |
| A2.6 | `dt search` (失败) | 不存在的项目/无意义关键词 | 不 crash, 友好返回 |
| A2.7 | `dt search-kg` | 语义搜索 KG 节点 | 返回 elementId, 可用 Cypher 验证 |
| A2.8 | `dt search-kg` (无结果) | 极冷门关键词 | 不 crash, 空结果 |

### A3. 知识图谱写入 (3)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A3.1 | `dt memorize` | --type KnowledgeAdded | 节点创建, 属性完整, 幂等 |
| A3.2 | `dt memorize` | --type Decision | Decision 节点 + BELONGS_TO Project |
| A3.3 | `dt memorize` | --type Environment/Dependencies | 对应类型节点创建 |
| A3.4 | `dt event` | --type Modification | Event 节点 + event_id 唯一 |
| A3.5 | `dt event` | --type ConfigChange/Deployment/BugFix | 各类型均可写入 |
| A3.6 | `dt event` | --type Conversation | Session-end 协议 |
| A3.7 | `dt event` (重复) | 同样参数执行两次 | 幂等, 不创建重复节点 |
| A3.8 | `dt learn` | --task --entities --success true | Knowledge+Experience+Playbook 创建 |
| A3.9 | `dt learn` | --success false | 失败场景也记录 |

### A4. 分析与规划 (5)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A4.1 | `dt context` | --task --worlds code,knowledge | 多世界聚合, JSON 输出 |
| A4.2 | `dt context` | --max-tokens 限制 | token 限制生效 |
| A4.3 | `dt plan` | --task | Playbook 匹配生成计划 |
| A4.4 | `dt domain` | --name + --depth + --include-code | 领域子图遍历, depth 限制 |
| A4.5 | `dt domain` (不存在) | 不存在的领域名 | 不 crash |
| A4.6 | `dt history` | --task --days --domain | 相似历史任务检索 |
| A4.7 | `dt dependency` | --target --direction both --depth --type code | 调用链分析 |
| A4.8 | `dt dependency` | --type config / service | 配置和服务依赖 |

### A5. 同步 (3)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A5.1 | `dt kg-sync` | 全量同步 | KG→Qdrant 同步, _kg_synced_at 更新 |
| A5.2 | `dt kg-sync --incremental` | 增量同步 | 仅同步变更节点 |
| A5.3 | `dt kg-sync --labels` | 按标签同步 | 只同步指定标签的节点 |
| A5.4 | `dt nacos-sync --env test` | Nacos test 同步 | NacosConfig/Service/Instance 节点生成 |
| A5.5 | `dt nacos-sync --env prod` | Nacos prod 同步 | production 环境同步 |
| A5.6 | `dt k8s-sync --dry-run` | K8s 干跑 | 预览模式不写数据 |
| A5.7 | `dt k8s-sync` | K8s 正式同步 | K8sDeployment/Pod/Service 节点生成 + 关联 NacosConfig |

### A6. Digital Thread (1)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A6.1 | `dt thread --action create` | 创建 Thread | Thread 节点创建 |
| A6.2 | `dt thread --action list` | 列出 | 所有 Thread 可见 |
| A6.3 | `dt thread --action add-session` | 追加 Session | Session 关联到 Thread |
| A6.4 | `dt thread --action add-decision` | 追加 Decision | Decision 关联到 Thread |
| A6.5 | `dt thread --action get` | 查看 | Thread 详情含 sessions/decisions |
| A6.6 | `dt thread --action close` | 关闭 | status 更新为 closed |

### A7. 运维与维护 (5)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A7.1 | `dt health` | 健康检查 | Neo4j/Qdrant 响应+耗时 |
| A7.2 | `dt backup --action backup` | 创建备份 | 备份文件生成 |
| A7.3 | `dt backup --action list` | 列出备份 | 显示备份日期 |
| A7.4 | `dt backup --action verify` | 校验备份 | checksum 验证通过 |
| A7.5 | `dt backup --action restore` | 恢复备份(需确认) | 数据恢复正确 |
| A7.6 | `dt cleanup --dry-run --targets all` | 预览清理 | reasoning/memory/snapshots 有输出 |
| A7.7 | `dt cleanup --dry-run --targets reasoning` | 仅预览 reasoning | 只显示 reasoning 清理 |
| A7.8 | `dt archive --dry-run --before <date>` | 预览归档 | Event 归档预览 |
| A7.9 | `dt archive --list` | 列出归档 | 显示已有归档 |
| A7.10 | `dt clean --dry-run` | 预览全量清理 | 显示待删除数据量 |
| A7.11 | `dt clean --dry-run --targets reasoning` | 仅清理 reasoning | 限制范围生效 |
| A7.12 | `dt verify --files` | 代码变更一致性 | 对指定文件校验 |
| A7.13 | `dt verify --check-config --check-api` | 全面校验 | 配置+API 一致性报告 |
| A7.14 | `dt schema init` | 初始化 Schema | uniqueness constraints + fulltext indexes 创建 |

### A8. 监控 (2)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A8.1 | `dt metrics` | 查询指标 | 返回实际数值(非 placeholder) |
| A8.2 | `dt metrics --filter "dt_*" --watch` | 过滤+监视 | filter 生效, watch 持续输出 |
| A8.3 | `dt watch --status` | 查看 watcher 状态 | PID/监控目录/事件数 |
| A8.4 | `dt watch` | 启动 watcher | 启动成功, 文件变更触发更新 |
| A8.5 | `dt watch --stop` | 停止 watcher | 正常退出 |

### A9. 外部工具 (3)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A9.1 | `dt kub pods --ns <ns>` | 列出 Pods | K8s namespace 连通 |
| A9.2 | `dt kub status pods --ns <ns>` | Pod 状态 | 返回 Pod 状态 |
| A9.3 | `dt kub status deploy --ns <ns>` | Deployment 状态 | 返回 Deployment |
| A9.4 | `dt kub status svc --ns <ns>` | Service 状态 | 返回 Service+Endpoints |
| A9.5 | `dt kub logs --ns <ns> --pod <pod>` | 实时日志 | 日志流正常 |
| A9.6 | `dt kub download --ns <ns> --pod <pod> -o /tmp/test.log` | 下载日志 | 文件生成,大小>0 |
| A9.7 | `dt jcli list` | 列出 Jenkins Job | Job 列表返回 |
| A9.8 | `dt jcli params --job <job>` | 查看参数 | 参数定义完整 |
| A9.9 | `dt jcli history --job <job> --limit 5` | 构建历史 | 历史列表返回 |
| A9.10 | `dt jcli log --job <job>` | 构建日志 | 日志内容返回 |

### A10. Daemon (1)

| # | 命令 | 测试场景 | 验证点 |
|---|------|---------|--------|
| A10.1 | `dt daemon status` | 查看状态 | 返回 running/stopped |
| A10.2 | `dt daemon start` | 启动 | gRPC 服务启动 |
| A10.3 | 启动后 `dt metrics` | metrics 可用 | 返回实际指标数据 |

---

## B. MCP Tool 全覆盖（22 个）

### B1. 核心功能 MCP Tool

| # | MCP Tool | 对应 CLI | 验证 |
|---|----------|---------|------|
| B1.1 | `dt_health` | `dt health` | MCP 与 CLI 返回一致 |
| B1.2 | `dt_search` | `dt search` | MCP 参数(--query/--project/--limit)生效 |
| B1.3 | `dt_search_expand` | `dt search --expand` | 扩展搜索多变体合并 |
| B1.4 | `dt_search_kg` | `dt search-kg` | MCP 返回 elementId 正确 |
| B1.5 | `dt_build` | `dt build` | --path + --name 构建 |
| B1.6 | `dt_context` | `dt context` | --task + --worlds + --max-tokens |
| B1.7 | `dt_plan` | `dt plan` | --task + --context + --thread-id |
| B1.8 | `dt_domain` | `dt domain` | --name + --depth + --include-code |
| B1.9 | `dt_dependency` | `dt dependency` | --target + --direction + --depth + --type |
| B1.10 | `dt_history` | `dt history` | --task + --domain + --days + --limit |
| B1.11 | `dt_verify` | `dt verify` | --files + --check-config + --check-api |
| B1.12 | `dt_kg_sync` | `dt kg-sync` | --incremental + --labels |

### B2. 写入 MCP Tool

| # | MCP Tool | 对应 CLI | 验证 |
|---|----------|---------|------|
| B2.1 | `dt_memorize` | `dt memorize` | --type + --entity-id + --details, 写入后能查回 |
| B2.2 | `dt_event` | `dt event` | --type + --entity-id + --entity-type + --details, 幂等 |
| B2.3 | `dt_learn` | `dt learn` | --task + --entities + --pattern + --success, Knowledge/Experience/Playbook |

### B3. 运维 MCP Tool

| # | MCP Tool | 对应 CLI | 验证 |
|---|----------|---------|------|
| B3.1 | `dt_backup` | `dt backup` | --action backup/list/verify/restore |
| B3.2 | `dt_cleanup` | `dt cleanup` | --dry-run + --targets |
| B3.3 | `dt_metrics` | `dt metrics` | --filter + --watch |
| B3.4 | `dt_thread` | `dt thread` | --action create/list/add-session/add-decision/get/close |

### B4. 外部集成 MCP Tool

| # | MCP Tool | 对应 CLI | 验证 |
|---|----------|---------|------|
| B4.1 | `nacos_sync` | `dt nacos-sync` | --env test/prod |
| B4.2 | `jcli_list` | `dt jcli list` | 列出 Jenkins Job |
| B4.3 | `jcli_params` | `dt jcli params` | 参数定义 |
| B4.4 | `jcli_history` | `dt jcli history` | 构建历史 |
| B4.5 | `jcli_build` | `dt jcli build` | 触发构建(仅测试环境) |
| B4.6 | `jcli_build_log` | `dt jcli log` | 构建日志 |
| B4.7 | `kublog_status` | `dt kub status` | pods/deploy/svc |
| B4.8 | `kublog_logs` | `dt kub logs` | 实时日志 |
| B4.9 | `kublog_download` | `dt kub download` | 日志下载 |
| B4.10 | `svc_list` | - | 列出本地微服务 |
| B4.11 | `svc_status` | - | 服务详细状态 |
| B4.12 | `svc_logs` | - | 服务日志 |
| B4.13 | `svc_start` | - | 编译+启动服务 |
| B4.14 | `svc_stop` | - | 停止服务 |
| B4.15 | `svc_restart` | - | 重启服务 |

---

## C. Skill 规范合规性验证

### C1. 架构与约定

| # | Skill 规则 | 验证方式 |
|---|-----------|---------|
| C1.1 | "优先使用 MCP Tool" | 每个 MCP Tool 都能成功调用且与 CLI 结果一致 |
| C1.2 | "MCP 不可用时降级为 CLI" | 停掉 MCP Server 后 CLI 命令仍可用（对比两者输出一致性） |
| C1.3 | Guide 文档可访问性 | 所有 guides 目录下的 .md 文件存在并可读 |
| C1.4 | config.yaml 路径正确 | `projects` 段每个 base+items 拼出的完整路径都存在 |
| C1.5 | ignored_dirs.yaml 逻辑 | 忽略的目录在扫描时被正确跳过 |

### C2. CODE-SEARCH.md 规范

| # | 规则 | 验证 |
|---|------|------|
| C2.1 | `dt search --path` 自动匹配项目 | 用 warehouse 目录测试, 自动匹配 goods-center/warehouse-center 等子项目 |
| C2.2 | `dt search --expand` 多变体合并 | 结果数≥普通搜索, 去重正确 |
| C2.3 | `dt search --all` 跨项目 | 多个 Project 的 Method 都可能出现 |
| C2.4 | `dt search --json` 输出规范 | 包含 method_id/name/file_path/start_line/end_line/signature/calls/language |
| C2.5 | 陷阱: 目录名≠项目名 | `--path` 比 `--project` 更健壮 -- 用 warehouse 路径(非项目名)测试 |
| C2.6 | 回退策略正确 | `dt health` 不通过时不 crash; 未索引项目有明确提示 |

### C3. KG-QUERY.md 规范

| # | 规则 | 验证 |
|---|------|------|
| C3.1 | 场景A: `dt search-kg` 向量语义搜索 | 返回 elementId, 能用于精确 Cypher 查询 |
| C3.2 | 场景B: 全文索引 `infra_search` | 全文索引存在且可用, 搜索返回正确类型 |
| C3.3 | 场景C: Cypher 兜底 | 多字段 LIKE 查询可执行 |
| C3.4 | 场景D: 项目分析型 | 不查 Method/Class 标签, 返回 Project/NacosService/Service 等高层信息 |

### C4. TRIGGER-RULES.md 规范

| # | 规则 | 验证 |
|---|------|------|
| C4.1 | Memorize: KnowledgeAdded | `dt memorize --type KnowledgeAdded` 成功写入 |
| C4.2 | Memorize: Decision → ArchitectureDecision | `dt memorize --type Decision --entity-type ArchitectureDecision` |
| C4.3 | Event: Modification (代码修改) | `dt event --type Modification --entity-type Method` |
| C4.4 | Event: ConfigChange (配置变更) | `dt event --type ConfigChange --entity-type NacosConfig` |
| C4.5 | Event: SoftwareInstalled (软件安装) | `dt event --type SoftwareInstalled` |
| C4.6 | Event: Deployment (仅生产) | `dt event --type Deployment --entity-type JenkinsJob` |
| C4.7 | Event: BugFix | `dt event --type BugFix --entity-type Method` |
| C4.8 | Event: Conversation (会话结束) | `dt event --type Conversation --entity-type Session` |
| C4.9 | Learn: 任务完成 | `dt learn --task --entities --success true/false` |
| C4.10 | dt update 由插件自动触发 | 验证 `dt update --file --project` 手动执行可工作 |
| C4.11 | dt remove | 验证可删除节点+索引 (需有 test 数据) |

### C5. WRITE-EVENTS.md 规范

| # | 规则 | 验证 |
|---|------|------|
| C5.1 | Event 去重: event_id SHA256 唯一约束 | 重复执行相同参数, 不创建重复 Event |
| C5.2 | Event 关联到实体 | 验证 INDEXED_METHOD/DEPLOYED_JOB 等关系存在 |
| C5.3 | Day→Session→Event 三级时间链 | Day 节点创建 + HAS_SESSION 关系 |
| C5.4 | Session-end Protocol | 执行 Conversation event, 生成摘要 |
| C5.5 | "写入后回复 `📝 已将 [XXX] 记录到知识图谱`" | 验证格式规范(非测试项,仅提醒) |

### C6. PROJECT-DISCOVERY.md 规范

| # | 规则 | 验证 |
|---|------|------|
| C6.1 | config.yaml vs KG 交叉对比 | 对比 Project 节点的项目名与 config.yaml 注册名 |
| C6.2 | 有源码文件即为候选 | 不依赖 pom.xml/package.json |
| C6.3 | ignored_dirs.yaml 过滤 | .git/.weave/node_modules 被正确跳过 |
| C6.4 | 发现候选用 question 工具提示 | 验证 skill 文档中的交互规范(非工具测试) |

### C7. 各集成指南规范

| # | 指南 | 验证 |
|---|------|------|
| C7.1 | JCLI-GUIDE: `jcli jobs` | 返回 Job 列表 |
| C7.2 | JCLI-GUIDE: Job 命名规范 | DEV-*/test-*/JAVA-*/VUE-*/PHP-* 前缀存在 |
| C7.3 | JCLI-GUIDE: 版本号规范 | yyyymmdd-0.0 格式 |
| C7.4 | K8S-LOGS: `kublog status pods` | 返回 Pod 列表 |
| C7.5 | K8S-LOGS: `kublog logs --pod` | 实时日志流正常(30min 稳定性) |
| C7.6 | K8S-LOGS: 禁止 kubectl logs | kublog 封装了 Kuboard 鉴权 |
| C7.7 | SVC-GUIDE: `svc list` | 列出本地微服务及状态 |
| C7.8 | SVC-GUIDE: 服务启停 | start/stop/restart 功能正常 |
| C7.9 | LONG-TASK: 记忆贯穿 | dt memorize/event/learn 全部可写入并查询 |
| C7.10 | EXECUTION-LEADER | guide 文档存在可读(非功能测试) |

### C8. Skill "禁止" 规则验证

| # | 禁止项 | 验证 |
|---|-------|------|
| C8.1 | "不要问要不要查 KG — 静默查询" | `dt search-kg` 执行不需要交互 |
| C8.2 | "禁止直接用 grep/glob/find" | `dt search` 能覆盖 grep 的查代码场景 |
| C8.3 | "禁止用 ls+read 浏览目录替代语义搜索" | `dt search --path` 按目录搜索替代浏览 |
| C8.4 | "禁止让用户去 Kuboard" | `kublog_logs` 可替代 Kuboard 网页 |
| C8.5 | "禁止用 kubectl logs 替代 kublog" | `kublog_logs` 功能覆盖 kubectl logs |

---

## D. 数据完整性深度校验

### D1. Schema 约束
```cypher
// 1. 所有 Method 必须有 project 和 file_path
// 2. 所有 Method 必须有 BELONGS_TO Project
// 3. 所有 NacosConfig 必须有 BELONGS_TO NacosGroup
// 4. 无重复 event_id
// 5. 无孤立 Event (无 HAS_EVENT 入边)
// 6. CALLS 边两端节点都存在
```
**期望**: 全部返回 0

### D2. 向量同步一致性
```bash
# 1. 写入一条 Knowledge → kg-sync --incremental → search-kg 搜到
# 2. KG 中 _kg_synced_at 为 NULL 的节点数 = 0 (全量同步后)
```
**期望**: 写入后可搜索, 同步后无遗漏

### D3. 配置-服务关系链完整性
```cypher
// NacosConfig ← BELONGS_TO ← NacosGroup
// NacosGroup ← BELONGS_TO ← NacosNamespace
// NacosService → HAS_INSTANCE → NacosInstance
// Service → REGISTERED_IN → NacosService
// K8sDeployment → CONFIGURED_BY → NacosConfig
```
**期望**: 关系链不断裂

---

## E. 跨系统一致性

| # | 校验 | 方式 |
|---|------|------|
| E1 | Neo4j 节点数 vs Qdrant 向量数 | `MATCH (n) RETURN count(n)` vs Qdrant collection info |
| E2 | CLI 输出 vs MCP Tool 输出 | 同一查询两种方式执行结果一致 |
| E3 | config.yaml project 数 vs KG Project 节点数 | 交叉对比找差异 |
| E4 | `dt search` file_path vs 实际文件系统 | 抽样验证文件路径存在 |

---

## F. 异常与边界

| # | 场景 | 期望 |
|---|------|------|
| F1 | Neo4j 断开时执行查询 | 不 crash, 友好报错 |
| F2 | Qdrant 断开时执行搜索 | 不 crash, 提示服务不可用 |
| F3 | 极大 limit 值 | 不 OOM, 有上限保护 |
| F4 | 中文/特殊字符查询 | 编码正确, 结果正常 |
| F5 | 空字符串查询 | 不 crash, 友好提示 |
| F6 | 并发写入(两个 event 同一 entity-id) | 幂等, 不创建重复节点 |
| F7 | 极深依赖分析 (depth=100) | 不 crash, 合理截断 |
| F8 | 不存在项目的 context/plan/domain | 不 crash, 返回合理信息 |

---

## G. 一键快速验证脚本

```bash
#!/bin/bash
# 快速验证: ~30s 覆盖核心功能
FAIL=0
ok() { echo "  ✅ $1"; }
fail() { echo "  ❌ $1"; FAIL=1; }

echo "=== Digital-Twin 快速验证 ==="

# 1. 健康
echo "--- Health ---"
dt health && ok "health" || fail "health"

# 2. 搜索
echo "--- Search ---"
RES=$(dt search "config" --project digital-twin-v2 --limit 3 2>&1)
echo "$RES" | grep -q "file_path" && ok "code search" || fail "code search"

# 3. KG搜索
echo "--- KG Search ---"
RES=$(dt search-kg "database" --limit 3 2>&1)
echo "$RES" | grep -q "elementId" && ok "kg search" || fail "kg search"

# 4. 写入
echo "--- Write ---"
TID="quick-verify-$(date +%s)"
dt memorize --type KnowledgeAdded --entity-id "$TID" \
  --entity-type Knowledge --project digital-twin-v2 \
  --details "title: 快速验证; content: 自动测试" 2>&1 && ok "memorize" || fail "memorize"

# 5. 读回
RES2=$(dt search-kg "快速验证" --limit 1 2>&1)
echo "$RES2" | grep -q "快速验证" && ok "read back" || fail "read back"

# 6. 上下文
dt context --task "config loading" --max-tokens 500 2>&1 && ok "context" || fail "context"

# 7. 历史
dt history --task "testing" --limit 2 2>&1 && ok "history" || fail "history"

# 8. 依赖
dt dependency --target "main" --direction both --depth 1 2>&1 && ok "dependency" || fail "dependency"

# 9. 同步
dt kg-sync --incremental 2>&1 && ok "kg-sync" || fail "kg-sync"

# 10. 线程
dt thread --action list 2>&1 && ok "thread list" || fail "thread list"

# 11. 备份
dt backup --action list 2>&1 && ok "backup list" || fail "backup list"

# 12. 清理(仅预览)
dt cleanup --dry-run --targets all 2>&1 && ok "cleanup preview" || fail "cleanup preview"

# 清理测试数据
MATCH (n {entity_id: "$TID"}) DETACH DELETE n 2>/dev/null

echo "=== 结果: $( [ $FAIL -eq 0 ] && echo '全部通过' || echo '有失败') ==="
```

---

## 测试优先级

| 优先级 | 范围 | 包含 |
|--------|------|------|
| P0 (立即) | A1-A3, A7.1, B1, C2-C4, D1, F | 核心功能+数据完整性+异常处理 |
| P1 (重要) | A4-A6, A7.2-A7.14, B2-B3, C5-C8, D2-D3, E | 分析+运维+Skill规范+跨系统一致性 |
| P2 (次要) | A8-A10, B4, C1, F2-F8 | 外部集成+监控+边界 |
| P3 (可选) | A7.5(restore), B4.5(构建), A9.5(日志), A10.2(daemon) | 有破坏性或影响运行环境的操作 |

---

## 执行记录

| 测试项 | 状态 | 备注 |
|--------|------|------|
| | | |

> 逐项执行时在右侧打勾记录。
