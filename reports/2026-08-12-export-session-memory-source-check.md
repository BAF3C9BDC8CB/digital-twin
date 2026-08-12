# 导出会话记忆来源检查报告（2026-08-12 14:30）

**导出文件**: /data/aflmProjects/aflm/uvp-im-center/default.tar.gz
**解压目录**: /tmp/default-export3/（87 个 request_dump，最新 20260812_085209）

## 结论速览

| 检查项 | 结果 |
|--------|------|
| 是否读取记忆 | ✅ 读取了（builtin 文件记忆，56/87 会话注入 system prompt）|
| 记忆是否来自知识图谱 | ❌ **不是**——全部来自 MEMORY.md/USER.md 文件 |
| KG provider prefetch 注入 | ❌ 0 个会话（[KG 记忆] 块 0 命中）|
| dt_search_kg 工具实际调用 | ❌ **0 次**（87 个 dump 中无一次实际调用）|
| KG 私有数据出现 | 28 个文件，但全部来自 system prompt 的 MEMORY.md 旧内容，非工具返回 |

## 关键证据

### ① builtin 文件记忆注入（确认）
request_dump_20260806_225721_d6b6b3（8月6-7日会话，706 条消息）system prompt 内：
```
MEMORY (your personal notes) [99% — 2,194/2,200 chars]
══════════════════════════════════════════════
User's Hermes conventions: complex tasks → kimi-k3...
§
Hermes v0.20.0 本地部署:源码+venv 在 ~/.hermes/hermes-agent...
```
- 内容是**当时的旧版 MEMORY.md**（2194/2200 字符，与现在 37% 版本不同）
- 证明：builtin 记忆确实是 md 文件每轮全量注入 system prompt

### ② KG 数据来自 MEMORY.md 而非 KG 检索
28 个含 KG 数据词（8095/IsPlaceMsg/msgWithdraw/GroupController）的文件中，**全部**出现在 `role=system` 消息里
- 即：这些数据是 MEMORY.md 里本来就有的旧内容，随 system prompt 注入
- 没有任何一条来自 dt 工具返回（tool 消息里 0 命中）

### ③ dt_search_kg 从未被实际调用
- 87 个 dump 中 tools 数组都含 dt_search_kg 定义（MCP 注册），但
- assistant 消息 tool_calls 里 **dt 系列工具实际调用 = 0 次**
- 佐证了此前元测试发现："agent 不查 KG" 问题（当时插件修复尚未落地）

## 为什么看不到 KG provider 效果

**时间不匹配**：
- 导出内容最新会话：20260812 08:52（上午）
- digital-twin memory provider 安装时间：2026-08-12 12:54（今天下午）
- → 这份导出里**不可能**有 [KG 记忆] prefetch 注入（插件尚未存在）

## 如何验证今天的 KG provider

重新导出（现在 14:30，导出应包含 12:58+ 的记忆测试会话），然后检查：
1. 新 dump 里 grep "[KG 记忆]" → 应有命中（provider prefetch 注入）
2. 模型回答含 KG 私有数据（8095/IsPlaceMsg=2）且 0 工具调用 → 注入生效
3. agent.log: "Memory provider 'digital-twin' registered/activated"

或直接跑测试脚本：
bash ~/.hermes/plugins/digital-twin/tests/test-recall.sh "warehouse 项目的端口和测试库"
