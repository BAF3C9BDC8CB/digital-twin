# Hermes 实际使用 KG 的验证与审计（2026-08-12）

用户会亲自开新会话测试「Hermes 做任务时是否真的用 dt 搜索」（元测试）。
本文件记录：验证方法、根因（agent 为何跳过 KG）、修复、以及三次测试会话的审计发现。

## 1. 验证方法（用户实测流程）

1. 开**新** Hermes 会话（插件与 SOUL.md 均会话启动时加载，旧会话仍是旧代码）
2. 问一个需要项目事实的任务，如「im-center 的消息撤回流程是怎样的？」
3. 观察两个信号：
   - **信号 A（开场简报）**：`[DT-SENSE]` 行 + 强信号行「✅ 本项目已索引 N 方法——代码问题先用 dt_search_kg(world=code, project=X) 定位」
   - **信号 B（工具调用）**：agent 是否调用 dt_search_kg(world=code, project=X) 或 dt search --world code --project X
4. 事后量化：`bash scripts/check-dt-usage.sh <小时数>`（统计 agent.log 中 dt_sense/dt_search_kg/run_cypher 次数）
   - 判定：dt_sense ≥1 且 dt_search_kg ≥1 = 用了 KG；全 0 = 插件/准则未生效

## 2. 根因（agent 跳过 KG 的两层原因，2026-08-12 实测）

测试发现 agent 做代码问答时 0 次 dt_search_kg（3 次 search_files + 1 次 terminal）：

- **层 1 准则漏洞**：SOUL.md/AGENTS.md 原第 4 条「读源码, 或 dt_search_kg(code 世界)」——「或」给了完全跳过 KG 的合法出口
- **层 2 插件信号弱**：dt-sense 简报只报 cwd 项目状态（如 digital-twin-v2 未索引时显示 0m 0c 0v），未提示「目标项目已索引 2287 方法可用」，agent 误判 KG 无内容

## 3. 修复（commit f7b0af9）

- **插件强信号**（plugins/dt-sense/__init__.py `_render_brief`）：status=indexed 且 methods>0 时注入
  `✅ 本项目已索引 N 方法——代码问题先用 dt_search_kg(world=code, project=X, limit=5) 定位, 再读源码验证; 禁止只读源码跳过 KG`
- **准则收紧**（SOUL.md + AGENTS.md 第 4 条）：
  代码逻辑任务**先 dt_search_kg(world=code, project=<项目名>) 定位再读源码验证**；
  仅 KG 不可用/超时才纯读源码并标注 ⚠
- **digital-twin-v2 自身从未索引**（0m 0c 0v）→ 触发 `dt build --name digital-twin-v2 --full` 让 KG 覆盖自身

## 4. 三次测试会话审计（session_search 读取 state.db）

| 会话 | 行为 | 评价 |
|------|------|------|
| 083552（"im-center 的消息撤回"） | 1 次 dt_search_kg(world=code, project=im-center, limit=8) → read_file 验证 | 规范 ✓ |
| 083804（"消息撤回流程"） | 首次查询**漏 world/project** → knowledge 噪音 5 条 → 自己纠正后再查 code | ⚠ 浪费 1 次 L1 |
| 084247（"消息撤回流程"） | tool_search + tool_describe 发现工具 → 1 次规范查询 | 合规但工具发现开销 |

### 审计发现的优化点（A-G，详见 reports/2026-08-12-hermes-kg-usage-audit-material.md）
- A. 会话 2 首次漏 world/project → 工具描述需更强调「代码任务必带 world=code+project」
- B. 会话 3 tool_search+tool_describe 开销 → dt_search_kg 建议预加载（deferred tool 机制）
- C. KG 返回的 calls 列表（如 groupMsgRecall 调 groupMsgRecallUpdate）未被用于构建调用链，agent 只当位置线索
- D. 三会话都只用 dt_search_kg 一层，未用 run_cypher_query 查 CALLS 关系做图遍历
- E. llm_analysis 质量：Builder 构造器/内部类占命中且 0.28 分噪声（GroupMsgRecallRequest 构造器分析有偏差）
- F. 撤回链路发现（AfterMsgWithdrawCallback 无消费方）未沉淀回 knowledge 世界，后续会话无法复用
- G. 三会话重复问同一问题，未利用会话记忆复用第 1 次结论

## 5. 关键工具
- `session_search(session_id=...)` 读完整会话轨迹（state.db 实时库；导出目录 sessions/ 可能滞后不含当天会话）
- `grep "20260812_XXXXXX" ~/.hermes/logs/agent.log` 查会话工具调用统计
- `scripts/check-dt-usage.sh` 统计 dt 工具调用次数
- 审计材料模板：`reports/2026-08-12-hermes-kg-usage-audit-material.md`（工具序列 + KG 返回 + 对比表 + 优化点清单）
