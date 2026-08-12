# Hermes 使用 KG 的会话审计 — 综合报告（三团队）

**审计对象**：im-center"消息撤回流程"测试会话 ×4（08:18 修复前对照组 + 08:35/08:38/08:42 修复后）
**方法**：会话实录（session_search）+ agent.log 注入记录 + 插件/mcp-server 源码 + Memgraph/Qdrant 实测
**团队**：合规审计员（deleg_f49344ec ✓）+ 价值发挥评估员（deleg_ef871b56 ✓）+ 方案设计员（deleg_53c6a1ab ✗ API 中断，由前两份综合替代）

---

## 一、核心结论

1. **修复总体生效**：修复后 3 会话全部使用 KG 且遵守"先 KG 定位再读源码"（08:18 对照组 0 次 KG，修复后 3/3 使用）。强信号简报 + 准则改写起了作用。
2. **KG 能力远超 agent 使用**：CALLS 关系 8903 条实测可遍历，一条 2-hop 查询即可复现三会话花 8-10 次 read_file 梳理的调用链。
3. **最大缺口是"沉淀闭环"**：撤回链路结论（回调未接入、标记体系、错误兜底不对称）零沉淀；knowledge 世界 8 个知识节点全为发送链路+群组管理。缺的不是能力，是"会话结束即沉淀"的习惯。
4. **次因是检索质量与工具可见性**：0.28 分 Builder 噪声占 limit 3/7；schema 可见性 = 合规最强杠杆（看过 schema 的 2/2 合规，没看的 1/1 违规）。

## 二、违规清单（合规审计员）

| # | 会话 | 违规 | 根因 |
|---|------|------|------|
| V1 | 会话2 | L1 查询 2 次（超 ≤1 约束） | 首查漏参→纠正性重查 |
| V2 | 会话2 | 首查未带 world=code/project | 简报"搜索触发"行裸示例矛盾 + 未 tool_describe |
| V3 | 会话2 | 纠正性重查=重复查询 | 同 V1 |
| V4 | 会话3 | tool_search+tool_describe 发现开销 2 步 | 工具未预加载（deferred） |
| V5 | 会话1 | 1×tool_describe（可辩护，学习 schema） | — |
| O1 | 三会话 | 均未用 L2（CALLS 图遍历）——错过的优化 | 准则未提 CALLS 可用 |
| O2 | 三会话 | calls 列表未用于构建调用链 | 同上 |
| O3 | 会话2 | 回调未接入发现未沉淀 | 无沉淀习惯 |
| O4 | 三会话 | 同任务未跨会话复用 | memory 世界 0 命中 |

## 三、行动方案（综合两份报告，按优先级）

### P0（低成本治本，建议立即做）

| # | 行动 | 改动文件 | 工作量 | 预期收益 |
|---|------|---------|--------|---------|
| P0-1 | **撤回链路 dt_learn 沉淀**（pattern+pitfalls，标题带"撤回 recall withdraw"关键词） | 无（1 次 CLI 调用） | S | 未来所有撤回/回调问题 L1 直接命中 0.9 分 pattern；第 2/3 次会话免全量重查 |
| P0-2 | **dt_search_kg 描述强约束**：查代码实体必须 world='code'+project，默认 knowledge 只含知识层会返回跨项目噪音 | mcp/mcp-server.py dt_search_kg description | S | 消灭 V2（漏参诱因） |
| P0-3 | **dt_search_kg/dt_sense/dt_search 预加载**（deferred → 常驻） | Hermes 工具配置 | S | 消灭 V4（发现开销）；schema 始终可见 |
| P0-4 | **简报"搜索触发"行改为带参示例**（消除与强信号行的矛盾先例） | plugins/dt-sense/__init__.py `_render_brief` | S | 消灭 V2 简报诱因 |
| P0-5 | **准则补丁**：SOUL.md 第 6 条追加"调用 dt_search_kg 前不确定参数先 tool_describe；漏参重查计入 L1"；AGENTS.md 场景 B 追加"项目名取简报 project 字段" | ~/.hermes/SOUL.md + AGENTS.md | S | 准则兜底 |
| P0-6 | **L2 CALLS 图遍历准则**：代码链路问题 L1 命中后可 L2 遍历 CALLS*1..2，内置噪音过滤（排除 getter/toString/success/fail/跨类优先） | AGENTS.md/SOUL.md 准则 | S | 单任务工具调用省 40-50% |
| P0-7 | **skill 过期文档修复**：kg-query-strategy.md/kg-usage-audit.md 中"dt_search_kg 硬编码 knowledge、无 project"改为"已支持 world/project（mcp-server.py L585-593 透传）" | ~/.hermes/skills/devops/digital-twin-ops/references/ | S | 防未来 agent 误判 |

### P1（中成本，全局收益）

| # | 行动 | 改动文件 | 工作量 |
|---|------|---------|--------|
| P1-1 | 索引端降权/排除 Builder 构造器、内部类、getter/setter 方法（synthetic 标记）；llm_analysis 构造器固定模板 | src/ 索引 pipeline + 重索引 | M |
| P1-2 | dt_search_kg 增加"按方法名精确检索"模式（如 name= 参数） | mcp/mcp-server.py + dt CLI | M |

### P2（可选，依赖习惯）

| # | 行动 | 改动文件 | 工作量 |
|---|------|---------|--------|
| P2-1 | 会话开始自动 session_search 重复问题检测（或 memory 世界按会话沉淀问答结论） | Hermes Hook | M |

## 四、3 个月内最值得做的 5 件事

1. **P0-1 撤回链路 dt_learn 沉淀**（今天做，1 分钟）——最大价值缺口，立即补上
2. **P0-2/3/4 工具层三连**（描述强约束 + 预加载 + 简报带参示例）——治本消灭漏参
3. **P0-6 CALLS 图遍历准则**——把 8903 条边变成可用资产
4. **P0-7 skill 文档修复**——防文档误导
5. **P1-1 构造器/内部类降权**——检索质量全局提升

## 五、验证方式（P0 实施后）

1. dt search "消息撤回" --world knowledge --project im-center → 应命中新沉淀的撤回链路 pattern（0.9 分）
2. dt search "groupMsgRecall" --world code --project im-center（不精确名）→ Builder 噪声应降权
3. 新会话问"消息撤回流程"→ 简报含带参示例；agent 首轮即带 world=code+project；可用 L2 CALLS 遍历
4. dt health 索引对账一致
