# KG 自动查询策略与实测数据 (2026-08-11)

设计文档(完整方案): `docs/superpowers/specs/2026-08-11-kg-auto-query-strategy.md`。
成本/收益决策模型(搜不搜的矩阵+预算规则): `docs/superpowers/specs/2026-08-11-kg-search-cost-benefit-model.md`(effective 成本 = raw × 重放放大 5-20×; 决策偏置: T1-T4 假阴性 10-20× 贵于假阳性 → 默认搜, T5-T7 默认浅搜/不搜; 单任务 raw ≤3K, 注入必裁剪)。
三层漏斗: L0 `dt_sense`(每任务开头必查, 项目级简报) → L1 `dt_search_kg(query, limit=5)`(按需, 实体级) → L2 Cypher(兜底, 属性级)。本文档是配套实测数据与源码语义。

## 实测 token 预算 (live CLI, 2026-08-11)

| 查询 | 大小 | ≈token | 结论 |
|---|---|---|---|
| `dt sense --json` 已索引(63 methods) | 1.65 KB | 500-700 | 有界: dirs≤10/langs≤5/entities≤10 |
| `dt sense --json` 未索引/降级 | 350 B | ~120 | stats 全 0 + degraded 列表 |
| `dt search --world knowledge --limit 5 --json` | 7.2 KB | ~2.2K | 全字段输出 |
| `dt search --world knowledge --limit 10 --json`(推算) | ~14 KB | 4-5K | **默认 limit=10 太重, 自动查询用 5** |
| `dt health` | — | 1ms | 快速判别后端可用性 |

开头注入总预算 ≤3.5K tokens; 每轮 pre_llm_call 注入压缩为 1-2 行状态摘要(~50-100 tok), 不裸查。

## 语义(源码事实, main.rs / sense/mod.rs / mcp/mcp-server.py)

- **dt sense "永不失败"**: Memgraph/Qdrant/SQLite 连不上 → `connect_graph()` 返回 None → 对应服务 noop → 输出 `degraded: ["memgraph", ...]` + stats 全 0 + status=`registered_not_indexed`, 进程退出码 0 不报错。**全 0 统计 ≠ 项目没代码, 是降级信号**。
- **MCP run_cmd 不检查 returncode**, stdout+stderr 合并返回; sense/search 兜底超时 120s(`run_cmd(timeout=120)`), bolt 连接无显式超时 → agent 侧对 sense/search 设 **10s 心理超时**, 超过即视为降级, 不等 120s。
- 失败语义三分: 连接失败(degraded 列表) vs 超时(MCP 报错) vs 空结果(正常, KG 确实无数据)。

## 查询策略要点

- 首选 `dt_search_kg`; **hop=0 当事实, hop≥1 只当线索**(防图扩展漂移); 按 `project` 字段过滤跨项目噪音(实测: 查 user-center 命中 doctor-center Config)。
- **代码实体检索用 world=code + project (2026-08-12 更新)**: `dt_search_kg` **已支持 world/project 参数**(mcp-server.py L585-593 透传 `--world`/`--project`; 修复前的"硬编码 --world knowledge 且无 --project 参数"已不成立, 勿按旧文档误判)。默认 world=knowledge 只含知识层: 无参查"消息撤回" top5 全 boss-center/message-center 配置键(0.26 分噪音), 带 `world=code, project=im-center` 精准命中 0.70+ 分。**正确处置**: ① 代码/方法/类定位一律 `dt_search_kg(q, world="code", project=<目标>, limit=5)`; ② 已带 project 仍全属他项目 → 换 `dt_search(world=code, project=<目标>)`; ③ 该入口也 0 命中才放弃 KG 转读源码; ④ 命中核验看 hits 的 `project`/`source_ref` 字段。⚠️ **精确方法名检索已内置(2026-08-12 确认)**: `is_identifier_query` 检测到标识符(如 `groupMsgRecall`)自动走 payload `name` 精确过滤 + 0.95 分置顶——**直接用精确方法名/类名当 query 比中文描述更准**(中文查询走纯向量通道才会出现 0.28 分 Builder 构造器噪声)。工具描述已引导 agent 优先传精确名。⚠️ **用户偏好(2026-08-12): 准则措辞用"推荐"不用"必须"**——`project=<项目名>` 是推荐项（上下文已知目标项目时才带），只有 world=code 是技术必然（knowledge 世界不索引代码实体）。**合规审计实测(2026-08-12 三会话)**: describe 过 schema 的 agent 2/2 带全参数, 没 describe 的 1/1 漏参——先 tool_describe 看 schema 是比简报更强的合规杠杆。⚠️ **Hermes Tool Search**: dt_* MCP 工具默认 deferred（藏在 tool_search/tool_describe/tool_call 桥接后，config `tools.tool_search.enabled: auto`）；勿全局 `enabled: off`（240+ MCP 工具占满上下文），用简报"可用 dt 工具"速查行免发现开销（详见 `references/kg-usage-audit.md`）。完整诊断方法论+Qdrant 对账+图 schema 事实见 `references/kg-search-verification.md`。
- 注入时字段裁剪: 只留 title/entity_type/snippet/hop/关键属性(hostname/url/service_type), 丢 score 浮点/score_breakdown/llm_analysis/evidence。
- Cypher 是最后手段: **勿用全文索引示例 `CALL db.index.fulltext.queryNodes("infra_search", ...)`(2026-08-12 实测无效)**: 那是 Neo4j 语法; 本环境 `infra_search` 索引不存在(`SHOW INDEXES` 只见 label+property 索引与 `mg_text_method` label_text), 且 `mg.text_search.query`/`text_search.query`/`QUERY ALL` 过程/语法均报不存在。L2 兜底一律改 **MATCH 属性查询**(`MATCH (c:Class {name:'GroupService'}) WHERE c.project='im-center'`)。**Memgraph 版本受限**: 不支持 `count{...}` 子查询(用 `WITH ... count(*)` 或 `UNION ALL` 代替)、不支持 `SHOW PROCEDURES`/`mg.version()`; MCP run_cypher_query 单次单语句。索引质量诊断方法论见 `references/kg-index-quality-audit.md`。elementId 精确查询显式 RETURN 字段白名单, 不 `RETURN n`。
- 缓存: sense `(project, cwd)` TTL 120s, search `(project, query)` 60s; 写操作(dt_memorize/dt_build/jcli_build)后失效; 跨会话不缓存。

## 坑: read_file 误判 binary

`read_file` 会把 `mcp/mcp-server.py`、`AGENTS.md` 等合法 UTF-8 文件报 "Binary file - cannot display as text"(`file` 命令确认是 text)。
→ 用 `python3 -c "print(open('<path>').read())"` 或 python 脚本读取。
(注: 与另一坑 read_file 显示层脱敏 `«redacted:sk-…»` 是两回事——脱敏是显示层, binary 误判是编码检测。)
