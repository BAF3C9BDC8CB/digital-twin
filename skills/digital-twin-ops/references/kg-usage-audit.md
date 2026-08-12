# KG 使用审计方法（2026-08-12 im-center 首轮实测）

> 关联: 审计完看价值缺口与优化操作（CALLS 图遍历/知识沉淀/残留清理/索引对账）→ `references/kg-value-optimization.md`。SKILL.md 已达 100K 上限无法加指针，本文件是入口。

触发: 评价某项目"是否用 dt 知识图谱 / 使用方式规范性 / KG 是否正确返回结果"。用户/父会话常派 3 并行角色: 使用审计、检索质量验证(8-10 真实查询)、索引诊断(cypher 统计)。本文件是审计角色的四步法+证据位置+实测坑位。

## 审计四步
1. **项目内关键字搜索**: `digital-twin|知识图谱|memgraph|qdrant|dt_sense|dt_search|dt_kg|dt_learn|dt_memorize`（含独立 `\bdt\b`，排除 target）；检查 readme/AGENTS.md/CLAUDE.md 是否存在及内容。
2. **Hermes 侧调用记录**: `~/.hermes/logs/agent.log{,.1,.2,.3}` grep `tool mcp__digital_twin__dt_` 按会话归类，区分主会话(cli)与子代理(subagent)；
   - 子代理每次调用的 query 原文与 hits id 在 `/home/luis/.hermes/cache/delegation/live/<deleg_id>/task-*.log`（live transcript，含 tool 参数与结果片段）——agent.log 只有"completed (耗时, chars)"无参数，判定查询质量必须看 transcript。
   - 索引写入行为: 查 dt_learn/dt_memorize/dt_build/dt_kg_sync 的实际调用（注意 MCP 注册行会大量出现，勿误计）。
3. **规范声明**: `~/.hermes/SOUL.md`「知识图谱(KG)感知准则」(L0 注入/L1 search_kg/L2 cypher + 决策清单 + 降级禁止)；skill `references/kg-search-decision-rules.md`；项目内 AGENTS.md。⚠️ 声明≠生效: SOUL.md 写"由插件注入"但 config.yaml 可能无 hooks 段——必须验证机制（config hooks 段 + agent.log 注入/hook 日志）再下结论。
4. **行为比对**: 主会话是否先 dt_sense(**目标项目 path 而非 cwd**) → 子代理是否各自 L0 → L1 查询词是否具体/带项目名前缀/一次查完 → 跨项目噪音是否按 project 字段过滤 → 是否 KG+源码双通道验证(不凭记忆答项目事实) → L2 cypher 是否仅诊断角色使用。

## 2026-08-12 im-center 实测坑位（基线）
- **dt_search_kg 已支持 world/project 参数（2026-08-12 修复后）**: mcp-server.py L585-593 透传 `--world`/`--project`，勿再按旧文"schema 仅 query+limit"判断。跨项目噪音仍可能出现（未带 project / 查询词语义跨项目），处置: ① 先确认调用是否带 `world=code`+`project`——**漏参是首查噪音最常见原因**（实测无参查"消息撤回" top5 全他项目配置键 0.26 分，带参后 0.70+ 分精准命中）; ② 结果侧仍按命中 project 字段过滤; ③ 委派任务书必须写明"代码任务带 world=code+project、漏参纠正性重查计入 L1 次数"（本次团队 A 任务书遗漏，子代理未过滤）。
- **委派子代理不各自 dt_sense（无 L0）**: 6/6 子代理直接 L1/L2。dt-sense 插件 pre_llm_call 注入未生效（config.yaml 无 hooks 段、无注入日志）时，主会话手动 sense 并把 key_entities/索引统计写进 delegation context 是现行有效补位。
- **readme 过短 → KG doc/entity 覆盖缺失**: im-center readme 仅"腾讯im"4 字节，检索"项目 readme 介绍"落空。需补 readme（作用/技术栈/功能域）后 dt build 增量重建。
- **合规基线（本次为范本）**: 主会话 sense→indexed(2287 方法/357 类/2287 向量, key_entities: getGroupId/getToAccount/getActionStatus) → 任务书强制 dt search_kg → 检索定位+读源码验证 → 无凭记忆断言；单角色 L1≤2 次、查询词带项目名前缀；dt_health 先行确认链路。
- 结果正确性判定: 归 bc0e2a 式验证员（8-10 查询 × ✓/⚠/✗ + cypher 节点统计），审计员不重复执行，只在报告中引用其结论。

## 2026-08-12 Hermes 三会话合规审计基线（live CLI, deepseek-v4-flash）

材料: `reports/2026-08-12-hermes-kg-usage-audit-material.md`（注意: 材料对会话1工具序列的描述与实际 transcript 有出入——多写了 3 步不存在的调用，以 transcript 为准）。任务: "im-center 消息撤回流程"（2287 方法已索引）。

**审计证据链（四查，缺一不可）**:
1. **会话原文**: `session_search(session_id=<id>)` 拉完整 transcript——核实工具调用参数原文、agent 是否引用 [DT-SENSE] 简报、查询词是否一次查完。材料/总结文件可能美化或遗漏调用。
2. **注入证明**: `grep "dt-sense: injected briefing" ~/.hermes/logs/agent.log` → 逐会话确认简报注入时间与目标项目（本次 08:35:58/08:38:11/08:42:54 三次全注入 im-center）。修复前对照组(08:18)同样有注入但简报无强信号、SOUL.md 还是"读源码,或 dt_search_kg"旧文——**注入存在≠强信号在场**，须看第 3 步。
3. **简报模板**: `plugins/dt-sense/__init__.py` 的 `_render_brief()` 是权威模板——核对强信号行"✅ 本项目已索引 N 方法——先用 dt_search_kg(world=code, project=X) 定位"是否在场。⚠️ 模板内"搜索触发: …→dt_search_kg(q,limit=5)"裸示例与强信号行并存、互相矛盾，是 agent 漏参的诱因之一。
4. **能力边界实测**（只读，不改配置）: `dt search "<q>" --world code --project im-center --json`（验证 world/project 过滤真实生效）; python neo4j 驱动 `bolt://127.0.0.1:7688` 跑 `MATCH (a:Method)-[r:CALLS]->(b:Method) WHERE a.project='im-center' RETURN count(*)`（验证 L2 图遍历可行——本次 8903 条边、方法有 elementId）。

**结论基线**: 会话1/3 合规（L1 各 1 次、带 world/project、先 KG 定位再读源码、查询词具体无碎查）；会话2 轻微违规（L1 2 次、首查漏 world/project 落 knowledge 世界得 5 条跨项目噪音、纠正性重查触发"重复查询"禁止项），但自我纠偏快、答案质量优（还发现回调未接入）。

**会话2 漏参根因（按权重）**: ① 模型行为——同模型同简报同任务，会话3 首轮即带全参、会话2 裸查（采样随机性）; ② 简报表述——"搜索触发"行裸示例 `dt_search_kg(q,limit=5)` 与强信号行矛盾，模型选了错的那个; ③ 工具可见性——会话2 未 tool_describe，schema 里 world/project 不可见，仅凭简报猜参数。**实测合规杠杆: 看过 schema 的 agent 2/2 带全参数，没看的 1/1 漏参 → schema 可见性 > 简报强信号 > 准则文本**。

**改进建议落地点**（审计报告已给出文件+句子级）: SOUL.md 第 6 条追加"漏参纠正性重查计入 L1 次数"；`plugins/dt-sense/__init__.py` "搜索触发"行示例改为带参形式；mcp-server.py dt_search_kg description 推荐标注"代码实体推荐 world=code+project"；dt_search_kg 预加载为常驻工具；本文件与 `kg-query-strategy.md` 的旧 schema 描述已同步。

✅ **P0 六项已于 2026-08-12 全部落地（commit 7a9c9a3，勿重复实施）**：
1. 撤回链路 `dt learn` 沉淀（pattern 0.876 分命中，含回调未接入/标记体系/兜底不对称）
2. mcp-server.py dt_search_kg description 改推荐语气（"推荐用法：查询代码实体时推荐 world='code'；上下文已知目标项目时推荐带 project=…"）
3. [DT-SENSE] 简报加"可用dt工具: dt_search_kg(query,world=code|knowledge,project=<项目名>,limit≤5) — … run_cypher_query; dt_health; dt_sense"速查行 + 搜索触发行改带参形式 + "漏参重查计入"
4. SOUL.md 第 6 条追加查参习惯（先 tool_describe）；AGENTS.md 场景 B 改"推荐"语气
5. AGENTS.md 新增场景 D：CALLS 图遍历准则（含噪音过滤规则）
6. skill 文档标注已修复（build-params-search-world-pitfalls / kg-retrieval-quality-fixes）
验证：11/11 ad-hoc + 676 cargo 测试全过。**未做（留待评估）**：dt_search_kg 常驻预加载（用简报速查行替代，勿全局 tools.tool_search.enabled: off）、构造器/内部类索引降权（P1-1）。

⚠️ **用户偏好（2026-08-12 纠正，优先于上面任何"必须"表述）**: 用户明确要求 KG 使用准则**不要用"必须/强制"措辞**——"不应该约束太强，只是如果有项目或目标文件的类型才增加，推荐增加，而不是必须增加"。落地规则: ① `world=code` 是技术必然（knowledge 世界不索引代码实体，查代码只能用 code 世界）可保留事实性表述; ② **`project=<项目名>` 一律"推荐"语气**——上下文（[DT-SENSE] 简报 project 字段/用户消息）已知目标项目时才带，未知时不强求; ③ 修改 SOUL.md/AGENTS.md/简报/工具描述时检查是否有"必须/禁止只读源码"类过强措辞，改为"推荐/建议"。技术事实（如"knowledge 世界不索引代码"）与行为约束（"必须带 project"）是两回事，后者必须软化。

**Hermes Tool Search 机制（deferred 工具背景，2026-08-12 实测）**: 所有 MCP 工具（含 dt_search_kg/dt_sense/dt_search 等 24 个 digital-twin 工具）默认**延迟加载**——不在模型可见工具数组里，藏在 `tool_search/tool_describe/tool_call` 三个桥接工具后面（config.yaml `tools.tool_search.enabled: auto`，tiered disclosure）。agent 用 dt 工具的实际开销路径: 会话3 实测 `tool_search("digital twin search kg code") → tool_describe → tool_call` 两步发现开销。⚠️ **勿用全局预加载方案**（`tools.tool_search.enabled: off` 会让 240+ 个 MCP 工具全部常驻占满上下文，得不偿失）。正确做法: 在 [DT-SENSE] 简报里加"可用 dt 工具: dt_search_kg(query,world=code|knowledge,project=<项目名>,limit≤5)"速查行，让 agent 直接 tool_call 免发现开销。
