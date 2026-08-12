# Hermes 记忆机制 + KG 集成方案（2026-08-12 源码探查）

## Hermes 自带记忆实现机制

```
MemoryStore (tools/memory_tool.py)           ← 内置 builtin（当前使用）
  • 文件: ~/.hermes/memories/MEMORY.md + USER.md
  • § 分隔条目（每条一记忆卡片）
  • 字符上限: memory 2200 / user 1375（配置 memory.memory_char_limit / user_char_limit）
  • 每轮全量注入 system prompt（冻结快照 _system_prompt_snapshot，保 prefix cache 稳定）
  • 操作: add / replace / remove（consolidation 失败上限 3 次/轮防循环）

MemoryManager (agent/memory_manager.py)      ← 可插拔 Provider 编排
  • MemoryProvider ABC (agent/memory_provider.py): name / is_available / initialize /
    system_prompt_block / prefetch(query→召回注入) / sync_turn(写回)
  • 仅允许 1 个外部 Provider（防工具 schema 膨胀），与 builtin 并存
  • 选择: config.yaml memory.provider = <name>

外部 Provider 插件（bundled 9 种 + 用户安装）:
  plugins/memory/{mem0,honcho,hindsight,supermemory,retaindb,byterover,holographic,openviking,...}
  用户安装: $HERMES_HOME/plugins/<name>/__init__.py（合成包 _hermes_user_memory 防 sys.modules 冲突）

关键: prefetch(查询) 按需召回 + sync(写回) —— 正是"需要时搜索"模式。
外部 Provider 的 prefetch 是**补充**，builtin 文件记忆仍全量注入（_memory_enabled 控制）。
```

## dt 侧现状

- dt 已有 memory 世界：`dt search --world memory`（search_mcp.rs:1061，search_memory 实现）
- dt learn / dt memorize 写 KG knowledge 世界
- 记忆文件位置：~/.hermes/memories/（不是 profiles/<name>/memories/，注意目录）

## 集成路径（用户问题：记忆可否存 KG、按需搜索）

**路径 A（零改动，推荐先做）**：项目/技术知识迁 KG
- 把 memory 中的项目部署/凭据/技能要点 → `dt learn` 批量写入 KG（knowledge 世界）
- 查询走 `dt search --world knowledge` / dt_search_kg
- 收益：给 Hermes 个人记忆腾空间（当前 96% 满 2119/2200）

**路径 B（深度集成）**：实现 dt 记忆 Provider 插件
- 按 MemoryProvider ABC 实现：prefetch() 调 dt_search_kg 按需召回 + sync() 写回 KG
- 放 ~/.hermes/plugins/<name>/，config.yaml 设 memory.provider
- 突破 2200 字符上限（不再全量注入，改为按查询召回）
- 注意：builtin 文件记忆仍注入，外部 Provider 是补充；只允许 1 个外部 Provider

## 源码位置备忘

- agent/memory_manager.py — 编排 + build_memory_context_block
- agent/memory_provider.py — MemoryProvider ABC
- tools/memory_tool.py — MemoryStore（load_from_disk:203 / save_to_disk:363 / format_for_system_prompt:682）
- agent/agent_init.py:1663 — 启动加载 + provider 选择（mem_config.get("provider")）
- plugins/memory/__init__.py — discover_memory_providers / load_memory_provider
