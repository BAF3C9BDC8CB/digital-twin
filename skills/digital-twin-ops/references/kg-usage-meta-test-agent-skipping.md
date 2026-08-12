# Hermes 实际使用 KG 的元测试：agent 不主动查 KG 的根因与修复

**日期**: 2026-08-12
**触发**: 用户实测——开新 Hermes 会话问 "im-center 的消息撤回流程"，观测 agent 是否主动用 dt 搜索。结果：全程未查 KG（3 次 search_files + 1 次 terminal，0 次 dt_search_kg/run_cypher_query）。

## 根因（两层，缺一不可）

1. **准则漏洞**：SOUL.md/AGENTS.md 原第 4 条 "需要定位/理解代码逻辑？→ 读源码, 或 dt_search_kg(code 世界)"——"或"给了 agent 完全跳过 KG 的合法出口。agent 事后解释"故意不查按准则"只是合理化，实际它不知道目标项目已索引。
2. **插件信号弱**：dt-sense 简报只报 cwd 项目状态。当 cwd 未索引（如 digital-twin-v2 自身 0m 0c 0v）而任务目标项目已索引（如 im-center 2287 方法）时，agent 读到 "registered_not_indexed | 0m 0c 0v" 会合理推断"KG 没内容可查"——简报反而误导放弃 KG。

## 修复（三管齐下）

1. **dt-sense 插件强信号**（plugins/dt-sense/__init__.py `_render_brief`）：status=="indexed" 且 methods>0 时注入：
   ```
   ✅ 本项目已索引 N 方法/M 类——代码问题先用 dt_search_kg(world=code, project=X, limit=5) 定位, 再读源码验证; 禁止只读源码跳过 KG
   ```
2. **准则收紧**（SOUL.md + AGENTS.md 第 4 条）：代码逻辑任务**先 dt_search_kg(world=code, project=<项目名>) 定位再读源码验证**；仅 KG 不可用/超时才纯读源码并标注 ⚠。
3. **digital-twin-v2 自身从未索引**（注册表有但 0m 0c 0v）→ 触发 `dt build --name digital-twin-v2 --full` 让 KG 覆盖自身，避免 cwd=项目根时简报误导。

## 验证方法（复测 agent 是否真用 KG）

- 脚本 `scripts/check-dt-usage.sh [小时数]`：统计 agent.log 中 dt_sense / dt_search_kg / dt search / run_cypher_query 调用次数与 [DT-SENSE] 注入次数。
- 判定：dt_sense ≥1 且 (dt_search_kg/dt search ≥1) = 用了；全 0 = 插件/准则未生效。
- 注意：日志里 "dt-sense: injected briefing for <path>" 一行可确认注入路径到底是 cwd 还是 user_message 匹配到的目标项目（曾经发现注入的是 uvp-im-center 路径而非 digital-twin-v2）。

## 教训（可复用）

- **给 agent 的"或"选项 = 给跳过 KG 的出口**。准则要用"先 X 再 Y"的强制顺序，不是"X 或 Y"。
- **插件简报必须报"目标项目已索引"这一强信号**，不能只报 cwd 状态。agent 不会主动 cross-check 项目是否已索引。
- 元测试本身是验证 KG 接入是否真正生效的手段：让用户在真实会话做任务，事后查日志统计 dt_* 调用，比任何插件单测都真实。
