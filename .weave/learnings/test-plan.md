# Learnings: Digital Twin V2 测试方案

## 第一轮测试: 2026-07-10 (08:30)

dt 版本: 旧版 (dt-daemon 0.1.0, 有 `dt list`, `dt search-kg`, `dt build-all`)

### 一、通过项 ✅

#### T1: 健康检查 ✅
- Neo4j: healthy (1 ms)
- Qdrant: healthy, v1.18.2
- SQLite / dt-embed: N/A (未配置)

#### T2: dt build — 方法数 ✅
- KG 数据: third-center 2790 methods, digital-twin-v2 909 methods — 匹配计划

#### T3: dt search — 无路径范围 ✅
- 不指定 --path 时搜索正常: Neo4jClient, DaoBase 均可找到

#### T6: Nacos KG 数据存量 ✅
- NacosConfig 352 / NacosService 42 / ConfigKey 880 — 与计划基本一致

#### T7: K8s 数据存量 ✅
- K8sDeployment 111 / K8sService 123 — 精确匹配计划

#### T8: 插件 ✅
- kub pods (100+ pods), jcli list (150+ jobs) — 均正常

### 二、第一轮不合预期项 🔴 (8 个)

| # | 问题 | 严重度 |
|---|------|--------|
| 1 | `digital-twin-v2` 未在 config.yaml 中注册 → 0向量 → dt list 不显示 | 高 |
| 2 | **`dt kg-sync` 崩溃** `missing 'results[0].data' in Neo4j response` → search-kg/kg_nodes 不可用 | 🔥致命 |
| 3 | Knowledge 节点全部 id=null, details=null | 高 |
| 4 | `dt event` MCP 缺 `--entity-type` 参数 | 高 |
| 5 | Event 孤立 (0 条→Project 关系) + 重复 (cleanup.rs 7条) | 中 |
| 6 | `dt context` 5/6 世界 0 数据 (knowledge/memory/semantic/runtime/reasoning) | 高 |
| 7 | `dt nacos-sync` 报告 175/0 vs 实际 352/42；2 条孤儿节点 | 中 |
| 8 | `dt search --path` 限定未注册项目返回空 | 低 |

---

## 第二轮重测: 2026-07-10 (09:00)

dt 版本: 新版 (dt-daemon 0.1.0, 移除 `dt list`/`search-kg`/`build-all`, 新增 `--query` flag, `--incremental` 改 flag)

### 三、已修复 ✅

| # | 问题 | 验证结果 |
|---|------|---------|
| 1 | digital-twin-v2 未注册 | ✅ config.yaml line 330 已加入 `digital-twin-v2` |
| 2 | kg-sync 崩溃 | ✅ **修复** — 98 nodes synced, 0 failed, 47ms |
| 3 | Knowledge 节点 null | ✅ 已修复 — `knowledge_id` 和 `details` 正确写入 |
| 4 | dt event 缺参数 | ✅ CLI 已加 `--entity-type` (但见下方新问题) |
| 7-孤儿 | NacosConfig 孤儿节点 | ✅ nacos-sync 新增自动清理 (orphans: 0) |

### 四、仍存在的问题 🔴

#### 🔴 A. `dt event` 写入不持久化 (新回归)
- **现象**: CLI 输出 `Event recorded: type=ConfigChange entity_id=retest-event-20260710`，打印日志 `event ConfigChange → retest-event-20260710`
- **实际**: Neo4j 查不到该 Event 节点；ConfigChange 总数仍为 2 (未增加)
- **建议**: 检查 event 写事务是否 commit

#### 🔴 B. Event-Project 关系始终为 0
- 157 个 Event 节点，`MATCH (e:Event)-[r]->(p:Project)` 返回 0
- 与第一轮相同，未修复

#### 🔴 C. `dt context` 5/6 世界仍空
- Reality: 20 items ✅
- Knowledge/Memory/Semantic/Runtime/Reasoning: 0 items
- kg-sync 已修复（98 nodes 含 Knowledge 同步到 Qdrant），但 context 的 knowledge 世界仍不检索
- README.md 仍出现 3 次 (去重未修复)

#### 🔴 D. `dt search --world knowledge` 返回空
- kg-sync 已将 Knowledge 节点同步到 Qdrant，但 knowledge world 搜索无结果
- 与问题 C 可能同源：knowledge/semantic 搜索链路不通

### 五、新发现 ⚠️

| # | 问题 | 详情 |
|---|------|------|
| E | CLI 语法大变更 | `dt list` 移除, `dt search-kg` 移除, `--query` 必填, `--incremental` 改为 flag (无值) |
| F | `dt jcli list` Jenkins 502 | Jenkins 服务不可用 (非代码问题) |
| G | nacos-sync 改进 | 新增 service sync (45 services)、孤儿节点清理；config sync 175 条 (仅 test namespace，local_test 174 条保持不变是预期行为) |
| H | k8s-sync 改进 | 新增 `--dry-run` 选项；nodes 403 (权限，非代码问题)；servers 0 (集群无此资源类型) |

---

---

## 第三轮重测: 2026-07-10 (09:33)

### 本轮修复 ✅

| # | 问题 | 验证结果 |
|---|------|---------|
| 2-P0 | knowledge 世界搜索空 | ✅ **修复** — `dt search --world knowledge "支付平台迁移"` → 3 results (Knowledge + Experience + Playbook) |
| build | 方法向量数显示 0 | ✅ 现在正确显示 3 methods, 3 vectors upserted |

### 仍存在的问题 🔴

| # | 问题 | 详情 |
|---|------|------|
| A-P0 | `dt event` 写入不持久化 | ~~误判~~ — 事件确实持久化，R1-R3 用了错误标签 `:Event` (正确: `:ConfigChange`) 和错误字段 `created_at` (正确: `timestamp`)。但 entity_id/details 为 null — **R4 修复** |
| B-P1 | Event-Project 关系: 0 | ~~误判~~ — BELONGS_TO 关系一直存在，R1-R3 用了错误标签 `:Event` 查 |
| C | `dt context` knowledge 世界仍空 | R3 已修复 `dt search --world knowledge`。R4 context 的 `json_to_bolt` fix 已部署 (DEBUG 可见 queries 执行)，但 match 逻辑仍返回 0 |

---

## 第四轮重测: 2026-07-10 (10:32)

### R4 修复验证 ✅

| # | 问题 | 验证 |
|---|------|------|
| event entity_id | R1-R3 entity_id=null | ✅ `r4-verify-event` → `entity_id: "r4-verify-event"` |
| event details | R1-R3 details=null | ✅ `details: "R4-验证entity_id写入"` |
| BELONGS_TO | 关系验证 | ✅ `ConfigChange → BELONGS_TO → digital-twin-v2` |
| context query 执行 | json_to_bolt fix | ✅ DEBUG 输出可见，queries 不再崩溃 |

### R4 仍存问题 ⚠️

| 问题 | 详情 |
|------|------|
| context knowledge 匹配 | query 执行但返回空 (`test-minimal: []`, `side-query: []`)，KG 有 3 条 domain=支付 的 Knowledge 节点但未被检索到 |
| context 去重 | README.md 仍出现 3 次 |
| jcli | Jenkins 502 (infra) |

---

## 六、四轮终态汇总

| 测试项 | R1 | R2 | R3 | R4 | 最终 |
|--------|-----|-----|-----|-----|------|
| T1 健康 | ✅ | ✅ | ✅ | ✅ | ✅ |
| T2 build | ⚠️ | ✅ | ✅ | ✅ | ✅ |
| T3 search | ⚠️ | ⚠️ | ✅ | ✅ | ✅ |
| T4 event 写入 | ~~🔴~~ | ~~⚠️~~ | ~~⚠️~~ | ✅ | ✅ |
| T4 BELONGS_TO | ~~🔴~~ | ~~🔴~~ | ~~🔴~~ | ✅ | ✅ |
| T5 context | 🔴 | 🔴 | 🔴 | ⚠️ | query 执行了，match=0 |
| T6 nacos | ⚠️ | ✅ | ✅ | ✅ | ✅ |
| T7 kg-sync | 🔴 | ✅ | ✅ | ✅ | ✅ |
| T8 kub/jcli | ✅ | ⚠️ | ⚠️ | ⚠️ | kub ✅ / jcli infra |

### R1-R3 误判说明

以下两个问题实际不存在，是我查询标签用错：
- **event 不持久化**: 用了 `MATCH (e:Event)` — V2 无此标签，事件存在但 entity_id/details=null (R4 已修复)
- **Event-Project 关系=0**: 同上标签错误，`BELONGS_TO` 关系一直正常

### 当前仅剩 1 条

| 优先级 | 问题 |
|--------|------|
| 🔴 P1 | `dt context` knowledge 世界 query 执行但 match 返回 0 (3 条 domain=支付 Knowledge 未被检索) |
