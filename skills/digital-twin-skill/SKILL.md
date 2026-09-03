---
name: digital-twin-skill
description: "如何使用 Digital Twin 知识图谱(基于 MCP 工具)定位代码、查询配置、管理记忆、检查系统。当用户提到 dt_sense、dt_search_kg、dt_memorize、dt_health、知识图谱查询、代码定位、项目索引、'记住/记一下'、记忆/回忆、服务健康检查或任何与本项目 dt 相关操作时，务必加载本 skill 按其流程执行，不要擅自绕开知识图谱直接读码或臆测结果。"
version: 3.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [digital-twin, dt, knowledge-graph, memory, code-lookup, index, health]
    requires_tools: [dt_search_kg, dt_search, dt_sense, dt_memorize, dt_event, dt_learn, dt_build, dt_health, dt_backup]
---

# 使用 Digital Twin 知识图谱

这个 skill 是一个操作手册：告诉你在什么时候、调用哪个 dt 工具、传什么参数。遵循它，AI 就能用知识图谱精确定位代码和查询历史，而不是盲猜或绕过图谱读盘。

## 什么时候用这个 skill

遇到以下任何情况，优先走本 skill 的流程：

- 需要精确**定位某个类/方法/函数/符号**在哪个文件、哪一行
- 用户用"我们之前设过 / 上次怎么配的 / 还记不记得"等**回忆历史、决策或配置**
- 用户明确说"**记住/记一下/记下来**"这类记忆意图
- 需要**查询配置、凭据位置、部署历史、环境变量**
- 想了解某**项目/目录的全貌**（统计、语言、关键实体）
- 需要**检查 dt 各服务（图库/向量/嵌入）健康状态**或**更新项目索引**
- 用户提到 dt_* 任何工具名或"知识图谱/KG/记忆"等关键词

> 原则：**定位先于读码，记忆优先于臆测，事实优于猜测**。能用图谱一次查到的，就不要靠猜或逐个文件翻。

## 十个工具速查

| 工具 | 用途 | 关键参数 |
|------|------|---------|
| `dt_search_kg` | 语义搜索图谱/代码/记忆（最常用） | `query`, `world`(code/doc/memory/knowledge), `project`, `limit` |
| `dt_search` | 全库搜索（world 默认 all） | `query`, `world`, `project`, `limit` |
| `dt_sense` | 探查项目/目录全貌与索引状态 | `path` |
| `dt_memorize` | 写入记忆/知识节点 | `type`, `entity_id`, `details`, `project`, `action` |
| `dt_event` | 记录事件节点 | `type`, `entity_id` |
| `dt_learn` | 批量写入知识（从执行结果提炼） | `type`, `content` 等 |
| `dt_build` | 构建/更新项目索引 | `path`, `full` |
| `dt_health` | 检查各服务健康状态 | 无 |
| `dt_backup` | 备份/恢复/校验 | `action`(backup/restore/list/verify), `date` |

## 三类核心任务流程

### A. 定位代码（三段序）

读取/分析**代码文件**之前，强制按顺序执行：

1. **探查项目**：`dt_sense(path=<项目或目录>)`，拿到 `project` 名和 `indexed` 状态。
   - 若 `indexed: false`，先 `dt_build` 再继续。
2. **图谱定位**：`dt_search_kg(query=<功能描述 + 符号名>, world="code", project=<项目名>, limit=5)`。
   - 拿到 `file_path`, `start_line`, `end_line`, `signature`, `score`。只信 `score > 0.7` 的结果。
3. **读码验证**：`read_file(path=<file_path>, offset=<start_line>, limit=<end_line - start_line + 数行>)` 确认实现。

这样做的原因：直接 `search_files` 全库 grep 会命中注释、测试和无关文件，不知道精确行号，浪费大量读取。先在图谱里定位能一次命中文件和行号。

### B. 查询配置 / 凭据 / 历史

查任何配置、凭据位置、部署历史、环境变量时，**先查记忆，命中即用**：

1. `dt_search_kg(query=<配置/凭据关键词>, world="memory", limit=5)`。
2. 命中 → 直接采用 `snippet`/`details` 给出的信息（含文件路径、环境变量名、配置位置）。
3. 0 命中 → 才允许 `read_file(<公开配置文件>)` 补充确认。

> 注意：**不读取 `.env`、`auth.json` 等敏感文件，不输出密钥原文**。只在记忆/配置里给"密钥在哪个环境变量、存于哪个文件"这样的**位置提示**。这是为了安全和防止泄露凭据。

### C. 写入记忆

用户说"记住/记一下/记下来"、分享了重要决策/知识/偏好时，**立即调用** `dt_memorize`：

1. 先想清楚 `entity_id`：稳定、可读、带日期标识（如 `rust-pipeline-rewrite-2026-09`）。
2. `type` 用工具接受的类型（如 `Decision`/`Knowledge`/`KnowledgeAdded`）。
3. `details` 必须用**结构化 key: value 格式**（每行一个键值对），必需字段 `name`、`content`，常用 `summary`/`type`/`confidence`/`source`/`tags`，并附上相关 `文件路径` 便于日后定位。

```python
dt_memorize(
    type="KnowledgeAdded",
    entity_id="rust-pipeline-rewrite-2026-09",
    project="digital-twin-v2",
    details="""name: Rust 管线重写决策
content: 决定用 Rust 重写数据管线，替换现有 Python 实现
summary: 技术选型：Rust 重写管线
type: decision
confidence: high
相关文件: src/pipeline/engine.rs
"""
)
```

## 维护系统健康

### 检查健康
`dt_health()` → 看 Graph / Vector / Embed 是否都可用。有红色则系统降级：`dt_search_kg` 可能只返回图结果（向量检索失败），或全部失败。

### 更新索引
- 新项目：先 `dt_sense` 确认 `indexed: false`，再 `dt_build(path=<项目>)`。
- 代码改动后：`dt_build`（增量）或 `dt_build --full`（全量重建对齐）。
- 单文件小改：增量/单文件更新即可，别每次全量重建（耗时）。

## 常见坑

- 不带 `world` 就查：`dt_search_kg` 默认 `knowledge` 世界，可能漏掉 code/memory。**按查询目标显式指定 world**。
- 记忆查询带 `project`：记忆统一全局，`dt_search_kg(world="memory")` 不要传 project（只作溯源返回，不过滤）。
- 读码前跳过 `dt_sense`/`dt_search_kg`：得到的是猜测位置，不是定位。先三段序。
- 用户说"记住"却只口头答应、不调 `dt_memorize`：记忆不会持久，下次就忘。立即执行。
- 输出密钥原文：违规。只给"存于哪个环境变量/文件"的位置提示。