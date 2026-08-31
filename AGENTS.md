# Knowledge Graph Behavior

This project uses a Memgraph knowledge graph for persistent memory.

## 执行顺序

1. **环境感知** — `dt_sense()`（CLI: `dt sense --json`）：项目定位/注册匹配/索引状态。已索引返回简报，未注册返回候选项目报告。
2. **按需搜索** — `dt_search_kg`（详见下方决策表）。
3. **深入探索** — 感知与搜索完成后再读代码/目录。

> dt_sense 不可用时（MCP 掉线且 dt 不在 PATH）才回退：读根目录 → 提取关键词 → 查 KG。
> 不强求"第一个动作就是 KG"——但要求在深度探索代码之前完成 KG 查询。

## 决策表（每任务先过一遍）

| 场景 | 动作 |
|------|------|
| 闲聊/元对话/本工具操作 | 不查 |
| [DT-SENSE] 已覆盖 | 不查 |
| 服务/配置/凭据/部署/历史决策 | `dt_search_kg(limit=5)`；hop=0 当事实，hop≥1 只当线索，跨项目丢弃 |
| 定位/理解代码 | 先 `dt_search_kg(world=code, project=<项目名>)` 定位再读源码验证；仅搜索不可用/超时才纯读源码并标 ⚠ |
| 精确属性（已知 elementId） | memgraph `run_cypher_query`，显式 RETURN 字段白名单 |
| 每任务 L1≤1 次 | 先定 2-4 个关键词一次查完，不碎查/宽泛词 |

**代码任务带 `world=code + project=<项目名>`**（knowledge 世界不索引代码实体）；project 限定消除跨项目噪音（KG 命中=事实，仅 dt_search_kg 不可用/超时才纯读源码并标注 ⚠）。委派子代理时任务书必须写明此规则（子代理不注入 DT-SENSE 简报，任务书是它唯一的 KG 准则来源）。

**降级与禁止**：
- dt_sense 永不失败：degraded=后端缺失，stats 全 0 ≠ 没代码；sense/search 10s 超时按降级
- 禁止：伪造结果；输出/提交 API key 或密码；hop≥1 当事实；直接采用跨项目命中；每轮重复查询
- 事件由 hook 自动入库，无需手动 `dt event`；用户说"记忆/记一下"立即 dt_memorize

> 详细查询策略（场景 A/B/C/D 的 Cypher 样例、token 预算、world/project 语义、坑）见 `skills/digital-twin-ops/references/kg-query-strategy.md`。

## Active Trigger: "记忆"

用户说 "记忆" / "记一下" / "记住这个" / "记下来" / "记住" 时——**必须立即写入 KG**，这是命令不是建议：

- 首选 MCP `dt_memorize(type="KnowledgeAdded", entity_id=..., entity_type=..., details=..., project=...)`
- 降级 CLI：`dt memorize --type KnowledgeAdded --entity-id ... --entity-type ... --details ... --project ...`
- 写入后回复：`📝 已将 [XXX] 记录到知识图谱`

## 事件与记忆写入（实际状态，2026-08-31 验证）

> ⚠️ 下表按**图库实证**标注：当前 Memgraph 中只有代码索引（Method/Class/Entity/Module/Document）
> 与记忆（:Knowledge）两类节点，**无任何事件标签节点**（:Modification/:Deployment/:ConfigChange/
> :Decision/:BugFix/:Conversation/:PodEvent/:K8sSyncEvent 均未创建）。AGENTS.md 历史版本宣称的
> "事件由 hook 自动入库"尚未落地——只有 dt-memory 插件在会话结束/每 N 轮做 LLM 记忆提取（写 :Knowledge）。

| 写入路径 | 状态 | 实际行为 |
|----------|------|---------|
| 代码索引 | ✅ 生效 | `dt build` 索引项目 → Method/Class/Entity/Module/Document 节点（world=code/doc） |
| 记忆提取（dt-memory v3） | ✅ 生效 | 会话每 N 轮/结束/压缩前，LLM 提炼 fact/decision/preference/convention → :Knowledge 节点（world=memory；项目记忆带 project，全局记忆 hermes-global）；显式 `dt_memorize` / `dt memorize` 同路径 |
| code_modified（dt build 插件）→ :Modification | ❌ 未落地 | 事件 hook 未实现，无此类节点 |
| jenkins_deploy_completed（jcli_build）→ :Deployment | ❌ 未落地 | 同上 |
| config_changed → :ConfigChange | ❌ 未落地 | 同上 |
| decision_made（dt memorize）→ :Decision | ❌ 未落地 | `dt memorize` 实际写 :Knowledge（带 --entity-type 时仅作标签参考），非 :Decision 事件节点 |
| bug_fix_recorded → :BugFix | ❌ 未落地 | 同上 |
| session_ended → :Conversation | ❌ 未落地 | 实际是 dt-memory `on_session_end` → LLM 记忆提取（:Knowledge），非事件节点 |
| pod_event_occurred / k8s_synced → :PodEvent / :K8sSyncEvent | ❌ 未落地 | 同上 |

**实际操作**：无需手动 `dt event`（事件路径未实现）；要留长期记忆用 `dt_memorize` / 说"记忆/记一下"。会话记录目前不进 KG——历史会话查询靠 memory 世界的 :Knowledge 记忆（llm_extract 自动提炼）。
