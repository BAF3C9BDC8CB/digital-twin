# Hermes ↔ digital-twin 记忆 Provider 集成（2026-08-12 落地）

## 记忆分层结论（builtin vs KG 不可完全互替）

- builtin（MEMORY.md/USER.md）= 每轮全量注入，100% 可见保证，2200/1375 字符上限
- KG provider = 按用户消息语义召回 top-N（prefetch），省 token 但有召回失败风险（~90%）
- **用户偏好/行为准则是"准则"不是"知识"**：用户消息不相关时 prefetch 不会召回（如用户问"查 X 端口"不会召回"用中文回复"）→ 必须全量注入
- **存储可以统一到 KG，读取必须分层**：全量注入保证性 + 按需召回省 token，两种读取需求硬合并会牺牲其一
- 推荐形态：builtin 只存行为准则+必避工具坑（小、稳定、每轮必见），KG 存项目知识（大、按需召回）
- 写入通道：memory 工具 → builtin 文件；dt memorize → KG。两条通道独立，勿混

## Hermes 记忆机制源码要点

- `memory.memory_enabled` / `memory.user_profile_enabled` 控制 builtin 注入（agent_init.py:1680 + system_prompt.py:516）；内置 store 创建与开关独立（防 memory 工具 dispatch 失败）
- **memory 工具无 provider 路由**——tools/memory_tool.py 直接操作 MemoryStore（文件），不能写 KG
- `system_prompt_block()` 每次构建 system prompt 时动态调用（memory_manager.build_system_prompt 遍历 provider）——可实时查 KG，但内容变化会破坏 system prompt 前缀缓存
- prefetch 结果注入 user message（turn-varying），不破坏 system prompt 缓存——这是 prefetch 优于 system_prompt_block 全量注入的架构原因
- 外部 provider 的 system_prompt_block 是附加（additive），不替代 builtin
- "完全替代"（memory_enabled=false + provider 全量+召回）技术可行但代价：① 用户偏好语义召回触发不了 ② 全量注入放 KG 丢省 token 意义 + KG 单点风险 ③ 子代理/cron 上下文 provider 跳过 → 失忆

## 架构

Hermes 记忆是插件化 MemoryProvider 架构：
- **builtin**（文件 MEMORY.md/USER.md，~/.hermes/memories/，每轮全量注入 system prompt，memory 2200/user 1375 字符上限）
- **外部 provider 最多 1 个**（config.yaml `memory.provider` 选择），与 builtin 并存（双读双写）
- 外部 provider 接口（agent/memory_provider.py MemoryProvider ABC）：
  - `name` / `is_available()`（本地检查，不联网）/ `initialize(session_id, **kwargs)`
  - `prefetch(query, session_id="")` → 返回**纯文本**注入上下文（关键：按用户消息自动召回）
  - `sync_turn(...)` → 每轮写回（本插件留空，写入由 dt hook 负责）
  - `get_tool_schemas()` → 返回 []（纯 context 模式）
  - `system_prompt_block()` → 静态提示块
- 注册方式：插件模块须有 `register(ctx)` 函数，**调 `ctx.register_memory_provider(provider)`**（collector 模式，不是 return！）
- 发现路径：`~/.hermes/plugins/<name>/__init__.py`（源码含 "MemoryProvider" 字符串即识别为 memory provider）
- 超时：外部 provider prefetch 8s 上限（memory_manager `_EXTERNAL_PREFETCH_TIMEOUT_S`），失败非致命（返回空注入）

## 插件位置与配置

- 插件：`~/.hermes/plugins/digital-twin/__init__.py` + plugin.yaml
- 配置：`hermes config set memory.provider digital-twin`
- 生效：`systemctl --user restart hermes-gateway`
- 验证加载：`grep "Memory provider" ~/.hermes/logs/agent.log` → 应见
  `Memory provider 'digital-twin' registered (0 tools)` + `activated`

## 插件行为（prefetch-only 只读设计）

- 每轮 prefetch 调 `dt search "<query>" --world knowledge --limit 5 --json`（subprocess，6s 超时）
- 渲染 `[KG 记忆]` 文本块（每条：`(entity_type) title: [project] snippet[:110]`）
- project 从 hit 的 project 字段 / source_ref / file_path / id 提取：
  `dt://doc/{project}/...` 项目段 = split("/")[3]（parts[0]=dt:, [2]=world 段）
- entity_type 优先级排序：Decision > Knowledge > KnowledgeAdded > Standard > Api > Config > Service
- 跳过 subagent/cron/flush 上下文（不重复注入）
- fail-open：dt CLI 不可用/超时/解析失败 → 返回空串，绝不影响主流程
- 环境变量：DT_BIN（默认 ~/.local/bin/dt）、DT_PREFETCH_LIMIT（5）、DT_PREFETCH_MAX_CHARS（1500）

## 验证方法（无 request_dump 时）

1. `python -c "from plugins.memory import load_memory_provider; p=load_memory_provider('digital-twin'); print(p.is_available())"`
2. MemoryManager 模拟：`mm=MemoryManager(); mm.add_provider(p); mm.prefetch_all("查询", session_id="t")` → 打印注入文本
3. 真实会话：`hermes chat -q "<业务问题> 不要查任何工具"` → 若 0 工具调用且答对内部业务知识 = 注入生效（铁证）。⚠️ 模型**有时在 reasoning 显式引用 "[KG 记忆]" 块，有时直接使用不引用**——判定用回答内容（KG 私有数据如端口号），勿 grep "[KG 记忆]" 字样

## 坑

- **register(ctx) 必须调 ctx.register_memory_provider()**——返回实例不生效（collector 模式）
- Hermes venv 无 qdrant_client/neo4j——插件用 dt CLI subprocess（与 dt-sense 同模式），不要直连后端
- prefetch 是同步调用（manager 已包线程 + 8s 超时），插件内 subprocess 超时设 6s 留余量
- knowledge 世界命中无 project 字段——必须从 source_ref/file_path 提取（dt:// 格式）
- logger 用 debug 级，INFO 级 agent.log 看不到 prefetch 详情——验证用真实会话 reasoning 观察
- **dt memorize 写入格式**（关键坑）：details 必须 `name: X; title: X; summary: X; content: X` 标准 key:value，自定义键（如"项目:"）解析不出字段 → content 空 → 向量化无效 → 召回不到。写入后等 3-4s（异步向量化），验证查 kg_nodes（payload 键 type='knowledge' 小写 + business_id=entity_id）。详见 `references/dt-memorize-write-guide.md`
