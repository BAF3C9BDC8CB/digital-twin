# Hermes Profile 导出分析工作流（2026-08-12 三轮实践）

用户反复导出 default profile tarball（/data/aflmProjects/aflm/uvp-im-center/default.tar.gz）
要求"分析是否还能优化 KG 使用效率"。三轮下来沉淀出可复用方法论。

## 分析步骤（按序）

### 1. 解压 + 基线对比
```bash
tar -xzf <file>.tar.gz -C /tmp/default-exportN
diff <(ls /tmp/default-exportN/default/ | sort) <(ls /tmp/default-export/旧版 | sort)
```
- 顶层差异先看：sessions/ skills/ memories/ SOUL.md cron/
- **会话文件大概率无新增**（导出含的是 API request dump，Hermes 当前会话不在内）——不要浪费时间逐会话翻
- 重点 diff：`SOUL.md`、`memories/MEMORY.md`（零差异=配置已同步）、`skills/` 文件清单

### 2. skills 新增 = 子代理沉淀（最有价值信号）
`for f in $(find 新版/skills -type f); do [ -f 旧版/$f ] || echo $f; done`
- 新增引用通常是子代理审查后写回的（如 qdrant-collections-backfill-review.md、code-classes-retrieval-gap.md）
- **检查本机 ~/.hermes/skills 是否已同步**——子代理写回的是导出快照，本机可能滞后
- **检查引用内容是否过时**：子代理沉淀的是它审查时的状态，后续主流程修复后引用可能记录"未修复缺口"（实测 code-classes-retrieval-gap.md 记"精确类名未命中"，P1-4 修复后需更新）

### 3. 从导出挖未落地方向
导出里可能含"优化方案设计"类任务书（子代理任务上下文），列出 N 层优化方向——
逐项对照实施状态，**找出唯一未落地的项**（本轮找到：数据层 Builder 噪声，其余 4 层已实施）。

### 4. 量化验证（勿停在定性）
对疑似噪声/问题做可复现量化：
- Builder 噪声：Qdrant scroll + filter 统计（见 builder-noise-optimization.md）
- 注意 scroll limit 5000 截断坑——必须带 scroll_filter 按 project 过滤

## 记忆来源审计（"是否读取记忆 / 记忆是否来自 KG"）

用户导出会话问"记忆是否从知识图谱来的"时，按三类痕迹审计（2026-08-12 实测）：

### 痕迹检查（全文件扫）
| 痕迹 | 含义 | 判定 |
|------|------|------|
| `[KG 记忆]` | digital-twin provider prefetch 注入 | 出现在 user 消息 = provider 生效 |
| `MEMORY (your personal notes)` | builtin 文件记忆 | 出现在 system prompt = MEMORY.md 全量注入 |
| assistant 消息 tool_calls 里 `mcp__digital_twin_*` / `dt_search_kg` | **实际**工具调用 | 非零 = agent 主动查 KG |

### 关键坑
- **grep 工具名 ≠ 实际调用**：tools 数组（工具 schema 定义）在 system prompt 里每个会话都有 → grep 命中 ~86/87；必须精确统计 assistant 消息的 `tool_calls`（实测 0 次调用）。这是"工具注册了但 agent 没用"的证据。
- **KG 数据词来源判定**：在文件里搜 KG 私有数据（如 8095/IsPlaceMsg），定位出现消息的 role——
  - `role=system` → 来自 MEMORY.md 旧内容注入（builtin，非 KG 检索）
  - `role=tool` → 来自 dt 工具返回（真正的 KG 检索）
  - 实测 28 个文件含 8095 等词**全部在 system** → 记忆来自 md 文件，非 KG
- **时间差验证先行**：先看导出最新会话时间戳（文件名 `request_dump_YYYYMMDD_HHMM...`）vs 插件/功能安装时间——导出可能不含安装后的会话（实测：插件 12:54 装，导出最新 08:52 → 不可能有 provider 注入，无需再逐文件翻）

### 证据抽取（写报告用）
- builtin 注入铁证：system prompt 里 `MEMORY (your personal notes) [NN% — X/X chars]` + 首条内容（旧版本内容与当前不同 → 证明是 md 快照注入）
- request_dump 结构：顶层 `timestamp/session_id/reason/request/error`；`request.body` 是 JSON 字符串需二次 json.loads；`body.messages` 为消息数组

## 关键教训
- request_dump 是 API 请求转储，`dt_search_kg` 出现次数 ≠ 实际调用（工具 schema 也计入）——别用 count 断言使用率
- 导出不含当前活跃会话（Hermes 会话在 state.db）——"最新会话"仍可能是失败的旧会话
- 用户三次导出内容高度重叠时，报告重点放在"新增了什么 + 哪些还没做"，不要重复已交付的分析

## 真实使用盘点（哪些 MCP/skill/插件在用）——2026-08-12

用户问"整理 skill/mcp/插件哪些真实使用到"时，用 state.db 做权威统计，**不要 grep agent.log**：

### 工具调用权威统计（state.db）
```sql
SELECT tool_name, COUNT(*) FROM messages
WHERE tool_name IS NOT NULL AND tool_name != '' AND role='tool'
GROUP BY tool_name ORDER BY cnt DESC
```
- `~/.hermes/state.db` messages 表每行一条真实消息，role='tool' 的 tool_name = 实际调用
- **agent.log grep 会假计数**：MCP 注册/工具 schema 重复出现（实测每个工具 16 次噪音）→ 数字虚高
- 归类：`mcp__<server>__<tool>` 前缀拆分 server；无前缀（terminal/read_file/patch）= 内置工具
- 零调用的 MCP server = 配置了但没用（实测 httptoolkit/idalib/js-reverse/stitch 全 0）
- qdrant server enabled=False（config.yaml mcp_servers 每项有 enabled 标记）

### Skill 使用统计（skills/.usage.json）
- 每个 skill 记录 `last_used_at`（有值=用过）、`created_at`、`patch_count`——没有 count 字段
- 区分"使用"与"近亲冗余"：sillytavern 用过但 sillytavern-ops 没用、feishu-messaging 用过但 feishu-bot-events 没用 → 未用的常是冗余副本，建议"留 1 删 2"

### 报告结构
按 MCP（9 个→4 用/4 零/1 禁用）、插件（state.db 调用 + agent.log registered/activated 双证）、Skill（51 用/14 未用+冗余表）三层给表，附核心使用链路图（每轮 DT-SENSE + builtin + KG prefetch → 任务中 dt_search_kg/memgraph/chrome-devtools/ocr/kanban）
