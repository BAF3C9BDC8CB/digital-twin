# KG Behavior（dt_search 按需检索）

> 适用：非 Hermes 环境（OpenCode / 委派子代理任务书）。Hermes 下以 .hermes.md + dt-memory 插件注入为准。

## 规则

1. 需要项目知识时先用 MCP `dt_search` 按需检索，命中先读取确认，0 命中才读源码：
   - `dt_search(query=..., project=digital-twin-v2)` 查代码/文件/文档（world 默认 all，跨 code/doc/knowledge/memory）
   - 查记忆/配置/历史 → `dt_search(query=..., world=memory)`（记忆统一全局，不分项目）
   - 搜索命令已统一：前身 `dt_router` / `dt_search_kg` / 裸 `dt search` 全部并入 `dt search`（CLI）与 `dt_search`（MCP），不再有独立命令/工具。
2. 同任务检索 ≤1 次：先定 2-4 个关键词一次查完，不碎查；已有上下文/闲聊/无检索对象任务句（"帮我实现"）不查（search 会 L0 自动拦截）。
3. 委派 subagent：任务书写明同样规则——需要项目代码/文件用 dt_search(project=...)，查记忆用 dt_search(world=memory)；父代理把已命中结果随任务书附上，子代理不重复检索。
4. 禁止：伪造结果；输出 API key；hop≥1 当事实；跨项目命中直接采用；dt_sense 仅陌生目录/不确定项目名时用。
5. 用户说 "记忆"/"记一下" → 立即 `dt_memorize`（记忆统一全局，details 注明文件路径）。

## 项目

- 注册名：`digital-twin-v2`；路径：`/data/myProject/digital-twin-v2`；code=代码 / doc=文档 / memory=记忆。
