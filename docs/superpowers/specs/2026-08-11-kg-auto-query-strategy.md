# Hermes 任务开头 KG 自动查询策略设计

- 日期: 2026-08-11
- 角色: 知识图谱数据/查询策略设计
- 关联: `docs/superpowers/specs/2026-08-03-dt-sense-design.md`、`AGENTS.md`(查询策略章节)、`mcp/mcp-server.py`
- 现状问题: AGENTS.md 软约束("先感知再查 KG")执行失败——约束无强制载体、无预算/节流、无降级语义。

## 实测数据(2026-08-11, live CLI)

| 查询 | 输出大小 | 约 token(中文 JSON) | 说明 |
|---|---|---|---|
| `dt sense --json`(已索引, 63 methods) | 1.65 KB | ~500-700 | dirs≤10 / langs≤5 / key_entities≤10, 天然有界 |
| `dt sense --json`(未索引/降级) | 350 B | ~120 | stats 全 0 + degraded 列表 |
| `dt search --world knowledge --limit 5 --json` | 7.2 KB | ~2.2-2.5K | 全字段: llm_analysis/relations/score_breakdown/evidence |
| `dt search --world knowledge --limit 10 --json`(推算) | ~14 KB | ~4-5K | 默认 limit 过重 |
| `dt health` | — | — | 1ms, 用于快速判别后端可用性 |

后端连接: Memgraph 连接失败→`connect_graph()` 返回 None→各服务用 noop 降级(不会 panic); bolt 连接无显式超时, MCP 层 `run_cmd` 对 sense/search 的兜底超时均为 120s。

---

## 查询策略设计

**三层漏斗模型: 项目级 → 实体级 → 属性级, 逐层下沉、每层有界。**

### L0 环境感知 — 每个任务开头必查(唯一强制层)
工具: `dt_sense(path)`(默认 cwd)。
- 一条命令完成三件事: 项目定位(注册匹配) + 索引状态三态(indexed / registered_not_indexed / unregistered) + 简报(统计/目录画像/语言/关键实体)。
- 粒度: **项目级摘要**。dirs 取 top10、langs 取 top5、key_entities 取 top10(后端已截断, 输出有界 ≤2KB)。
- 产出: 项目名、路径、注册状态、methods/classes/vectors 计数、last_build、degraded 列表。
- 已索引 → 简报; 未注册 → 候选发现报告(建议注册+构建命令)。

### L1 场景化检索 — 任务上下文相关才查(按需层)
工具: `dt_search_kg(query, limit=5)`(即 `dt search <q> --world knowledge`)。
- 触发条件: 任务涉及具体服务/配置/凭据引用/部署/历史决策时; 纯代码任务不触发。
- 粒度: **实体级命中**(Config/Server/Infrastructure/Knowledge 等), 带 `hop` 距离标注。
- 规则: **hop=0 当事实, hop≥1 只当线索**(防图扩展漂移到无关实体)。
- query 构造: 从任务提取 2-4 个具体关键词短语一次查完, 不碎查、不用宽泛词("系统/服务"→噪音)。

### L2 定向 Cypher — 仅当 L1 不够精确时(兜底层)
- 场景 B(已知确切命名, 如服务名/配置名): `dt search "<kw>" --world code|knowledge --project <项目> --limit 10`; Cypher 兜底用 `MATCH (n) WHERE n.project=... AND n.name CONTAINS ...`(本环境 Memgraph 不支持 db.index.fulltext 语法)。
- 场景 A 延伸(拿到 elementId 后): memgraph MCP `run_cypher_query` 精确取属性, **显式 RETURN 字段白名单**(auth_user/hostname/port/url/service_type...), 不 `RETURN n`。
- 场景 C(类型不确定): 探索性 MATCH, 显式排除 Method/Class/Interface/Enum/Package/Module 标签。
- 原则: **Cypher 是最后手段**, agent 第一选择永远是 dt_search_kg(与 AGENTS.md 一致)。

---

## 上下文预算与噪音控制

**任务开头注入预算: 总量 ≤3.5K tokens(≈12KB), 分三层。**

| 层 | 预算 | 控制手段 |
|---|---|---|
| L0 sense 简报 | ≤800 tokens | 后端已截断; agent 注入时压缩为状态行: `project/status/stats/last_build/degraded` + top5 目录 + top5 关键实体 |
| L1 search_kg | ≤2K tokens | limit=5(默认 10 太重); 只转述 top 3-5 命中 |
| L2 Cypher 结果 | ≤700 tokens | 字段白名单, 只取所需属性 |

**噪音控制清单**(针对 SearchHit JSON 全字段结构):
1. **limit 收缩**: 默认 10 → 5; 命中质量差(score 全低/无关)再扩到 8, 上限 10。
2. **字段裁剪**: 注入时只转述 `title + entity_type + snippet + hop + 关键属性(hostname/url/service_type/project)`; 丢弃 `score` 浮点、`score_breakdown`、`llm_analysis`(code 世界才有)、`evidence` 大段原文——这些是重噪音。
3. **hop 过滤**: hop=0 事实, hop=1/2 仅留实体名作线索。
4. **project 过滤**: 命中含 `project` 字段时按当前任务项目过滤, 无关项目命中直接丢弃(实测跨项目噪音明显: 查 user-center 时命中 doctor-center 的 Config)。
5. **去重**: RRF 融合可能跨 world 重复同一实体, 按 `{source_world}:{id}` 去重。
6. **凭据降噪**: `auth_password` 等敏感字段不自动注入; 需要时经 elementId 精确查询且不回显到日志。
7. **查询词聚焦**: 宽泛词不查; 一次任务最多 1 次 L1 自动查询, 更多按需手动。

---

## 缓存/节流方案

**时机结论: L0 sense 每任务一次(session 级缓存); L1 search 按需查 + 会话内缓存; L2 按需不缓存。禁止每轮 pre_llm_call 裸查。**

载体: Hermes `context_engine` hooks(pre_llm_call / session_start)挂 dt_sense, hook 内实现缓存与节流; 进程内存态, 不持久化。

| 项 | 设计 |
|---|---|
| 缓存键 | sense: `(project_name, cwd)`; search: `(project, query_normalized)` |
| TTL | sense 120s(任务内多次 LLM 调用共享一次); search 60s |
| 节流 | 同 session 同键 1 次/分钟; 失败后冷却 30s 不重试 |
| 失效 | 写操作(dt_memorize/dt_build/jcli_build/dt_learn)后, 相关键立即失效; 用户明示"重新感知/重新看"强制失效 |
| 跨会话 | 不缓存——新会话开头必须 fresh sense(环境可能已变) |
| 每轮注入 | pre_llm_call 每轮注入的不是完整 JSON, 而是缓存简报压缩成的 1-2 行状态摘要(project/status/last_build/degraded), 约 50-100 tokens |

---

## 降级策略

**失败语义三分**(agent 必须先区分):
- **连接失败/后端缺失** → `dt_sense` 不报错, 返回 `degraded: ["memgraph"/"qdrant"/"snapshot"]` + stats 全 0 + status=registered_not_indexed(源码: SenseService 永不失败)。
- **超时** → MCP run_cmd 120s 兜底(bolt 连接无显式超时); agent 侧对 sense/search 设 **10s 心理超时**, 超过即视为降级, 不等 120s。
- **空结果** → 正常语义(KG 确实无数据), 不是故障。

**降级阶梯**:
1. **感知降级**: sense 返回 degraded 或超时 → 不 panic, 回退本地目录侦察(读根目录 + config.yaml/pom.xml 等定位项目), 上下文开头标记 `⚠ KG degraded: [memgraph]`, 降低对全 0 统计的误判。
2. **查询降级**: dt_search_kg 失败 → ① Memgraph 活着但 Qdrant 挂: 走 memgraph MCP `run_cypher_query` 全文索引; ② MCP 层挂: CLI `dt search` 直连; ③ 全挂: 直接读磁盘(配置文件/README/部署脚本), 跳过 KG。
3. **写降级**: KG 不可达时 dt_memorize/dt_build 失败 → 不静默, 明示"知识图谱不可达, 本次决策未记录", 恢复后 `dt_learn` 批量补记。
4. **防雪崩**: 失败冷却 30s 内不重试同一查询; 每任务开头最多重试 1 次。
5. **恢复检测**: 上一任务 degraded 时, 本任务开头先 `dt_health`(1ms)确认恢复, 恢复则正常走 L0。

**决策原则: KG 是加速器不是单点依赖——查不到就查磁盘, 记录不了就明示, 绝不因 KG 故障阻塞任务主流程。**
