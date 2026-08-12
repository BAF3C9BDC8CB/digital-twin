# Hermes hook/plugin 注入 dt sense 上下文（2026-08-11 评审定案）

目标: 让 Hermes agent 自动感知当前项目环境(dt sense), 减少"凭记忆回答"。评审了 A(shell hook 每轮注入)/ B(Python plugin 首轮注入)/ C(pre_tool_call 拦截信息查询命令)/ D(组合) 四方案。

## 为什么旧方案（home 级 AGENTS.md）失败 — 根因（2026-08-11 源码级核实）

Hermes 的 AGENTS.md 注入规则：**AGENTS.md 只在 cwd 生效**（子目录和父目录副本均被忽略），
**home 级 `~/.hermes/AGENTS.md` 不跨项目注入**——只在以该目录为 cwd 时加载。官方文档明确：
跨项目规则应该用 `SOUL.md`（~/.hermes/SOUL.md，身份/行为准则，恒加载）或 skill。
→ 用户之前在个人目录 AGENTS.md 写"先查 KG"的软约束，agent 在 /data/aflmProjects 等目录工作时
根本看不到该规则，等于没约束。**软约束的正确载体是 SOUL.md + skill，不是 home AGENTS.md。**

## 实测性能(本机 2026-08-11, 均多次取样)
- `dt sense --json`(项目目录, 未索引): **0.01–0.04s**, ~19.5MB RSS, 输出 350B ≈ 100–150 tokens; 索引后估算 1–3KB ≈ 500–1500 tokens。
- 完整 shell hook 路径(bash 子进程 + dt sense + jq): **0.03–0.06s**。
- `dt search "query"`(对照): **1.38s, 13KB** ≈ 4000+ tokens —— **勿用于每轮注入**。
- dt sense 不依赖 LLM(与 `dt health` 里 SiliconFlow 489ms 检查无关)。
- 结论: 延迟不是问题(最坏 +0.06s/轮), 真成本是 token。

## dt sense 降级行为(源码确认 src/application/sense/mod.rs:84)
- 注释原话 "永不失败: 后端缺失走 degraded" —— Memgraph/Qdrant 挂掉时输出 `degraded: ["memgraph",...]` 且 **exit 0**, hook 仍拿到合法 JSON。
- **唯一非零退出条件: 路径不存在**(cli/sense.rs)。
- 支持 `dt sense <path>`; hook 必须用 stdin payload 的 `.cwd` 显式传目录(gateway 会话 cwd 不可靠, 可能是 HERMES_HOME)。

## 推荐实现: B 插件首轮注入(D 变体 = B + SOUL.md 软约束)
1. `~/.hermes/plugins/dt-sense/plugin.yaml` + `__init__.py`; `register(ctx)` 里 `ctx.register_hook("pre_llm_call", handler)`。
2. handler 用 kwargs **`is_first_turn`** + **`session_id`**(内存缓存) 实现"每会话只注入一次"; 非首轮 return None。
3. `subprocess.run(["dt","sense","--json",cwd], capture_output=True, timeout=5)`, 截断 2000 字符, 失败/空输出 return None。
4. 写日志文件(如 /tmp/dt-sense-inject.log)记触发次数供验证。
- 插件优点: 无 consent 弹窗(隐式信任)、CLI+Gateway 同路径生效、进程内省 bash+jq 子进程。
- SOUL.md 已有(~/.hermes/SOUL.md): 追加软约束"项目/环境信息优先调 dt_sense / dt_search_kg MCP 工具(已有 digital-twin MCP), 勿凭记忆"。

## Shell hook 替代(A)要点与坑
- 脚本放 **`~/.hermes/agent-hooks/`**(约定目录); **`~/.hermes/hooks/` 是 gateway-hooks 专用**(HOOK.yaml+handler.py, 仅 gateway 生效)——两个目录极易混淆。
- config.yaml:
  ```yaml
  hooks:
    pre_llm_call:
      - command: "~/.hermes/agent-hooks/dt-sense-inject.sh"
        timeout: 10
  ```
- **consent 坑(最易踩)**: gateway 非 TTY 不弹窗, 新 hook **静默不注册只打 warning**。需 `hooks_auto_accept: true` 或手写 `~/.hermes/shell-hooks-allowlist.json`:
  ```json
  {"approvals": [{"event": "pre_llm_call", "command": "/abs/path/dt-sense-inject.sh"}]}
  ```
- 脚本骨架: 读 stdin payload 取 `.cwd` → `dt sense --json "$cwd" | head -c 2000` → 有输出则 jq 包 `{"context":"[DT-SENSE]\n..."}`, 否则打印 `{}`。任何失败(坏 JSON/非零退出/超时) → 仅日志 warning, **绝不 crash agent**(文档+源码双重确认)。
- 多个 pre_llm_call 插件 context 按插件目录字母序拼接 → **A+B 混用会双份注入, 二选一**。

## ⚠️ 关键修正（用户指出, 2026-08-11）: dt sense 感知的是 cwd, 不是用户想操作的目录

- `dt sense`(无 path 参数) 感知 **当前进程 cwd**; Hermes 会话 cwd 通常是启动目录(如 /data/myProject/digital-twin-v2)或 HERMES_HOME, 而用户消息可能指向另一个项目(如 \"查 warehouse 的问题\") → **注入错项目简报**。
- **不能用 payload 的 `.cwd` 直接喂 dt sense**——那正是错误来源。正确做法: **从 user_message 提取目标项目**。
- 已验证事实:
  - `pre_llm_call` hook kwargs 含 **`user_message`**(turn_context.py:1058: `user_message=original_user_message`); shell hook 的 payload `extra` 里也带(`shell_hooks.py:541` 非顶层键全进 extra)。
  - `dt sense <path> --json` 支持显式传路径(已验证: `dt sense /data/aflmProjects/aflm/doctor-center --json` → 正确命中 doctor-center indexed)。
  - 项目注册表: `~/.config/digital-twin/config.yaml` 的 `projects:` 块(base + items) = 完整项目名清单, hook 可解析。
- **注入逻辑(修正版)**: 读 payload `extra.user_message` → 提取项目关键词 → 匹配注册表项目名 → 命中: `dt sense <该项目根路径> --json`; 未命中: 回退 `dt sense <cwd> --json`。输出统一截断 ≤2000 字符。
- 实施载体倾向 Python plugin(需解析 YAML 注册表 + user_message 匹配, Python 比 shell+jq 合适), 但**该修正尚未实施, 待用户确认**。

## 内置防 token 膨胀机制
- 注入进 **user message**(非 system prompt), 保住 provider prompt cache; 临时不落库。
- `hook_output_spill` 默认开启: >10K 字符 spill 到磁盘, 头尾各 500 字符预览(tools/hook_output_spill.py)。

## C 方案(pre_tool_call 拦截 grep/find/curl)不推荐
- `pre_tool_call` 的 block 是唯一能实质阻断 agent 的路径; 正则无法区分"信息查询"与正常开发操作(grep 查日志/curl 调 API/find 找文件), 高误伤 + 模型反复被打回。最多留极窄规则(如拦截重复 `dt search` 防刷屏)。

## 验证与回退
- `hermes hooks list` / `hermes hooks doctor`(exec 位/allowlist/mtime/JSON 有效性/耗时) / `hermes hooks test pre_llm_call --payload-file X`。
- 手动模拟: `echo '{"hook_event_name":"pre_llm_call","cwd":"/data/myProject/digital-twin-v2"}' | ~/.hermes/agent-hooks/dt-sense-inject.sh`。
- gateway 日志: `journalctl --user -u hermes-gateway -f`(systemd **用户**服务 hermes-gateway.service); agent 日志: `tail -f ~/.hermes/logs/agent.log`。
- 回退: A/C = 注释 config.yaml `hooks:` 块 + `hermes hooks revoke <command>`; B = `rm -rf ~/.hermes/plugins/dt-sense/`; 重启 `systemctl --user restart hermes-gateway`。CLI 新会话即时生效, **两端分别验证**(重启 gateway 不影响已开 CLI 会话)。
