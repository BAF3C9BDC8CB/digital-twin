# pre_llm_call 注入模板 + SOUL.md 增补（最终交付物, 2026-08-11）

关联: `references/hermes-hook-dt-sense-injection.md`（评审定案: B 插件首轮注入 + SOUL.md 软约束 + MCP 执行层, 不启用 pre_tool_call）、`docs/superpowers/specs/2026-08-11-kg-auto-query-strategy.md`（三层漏斗 L0/L1/L2 + token 预算）、`skill/SKILL.md`（禁止事项）。

## 0. 分工（两件交付物的边界）

| 载体 | 内容 | 生命周期 | 触发 |
|---|---|---|---|
| **注入 context**（本文件 §1） | 当前环境事实: 项目简报 + KG 健康 + 压缩搜索规则 + 禁止事项 | 动态, 每会话开头由插件渲染一次, ≤1.5KB, 进 user message | pre_llm_call 首轮 |
| **SOUL.md 增补**（本文件 §2） | 静态行为准则: 三层漏斗 + 搜索时机决策清单 + 降级/禁止 | 恒生效, 跨会话 | 每轮都在 system prompt |

规则: 注入回答"现在是什么环境", SOUL.md 回答"任何时候该怎么判断"——两者规则一致, 注入版是压缩速查, SOUL.md 版是完整决策逻辑。**规则冲突时以 SOUL.md 为准**（注入内容可能因降级而缺失, 准则不能缺）。

---

## 1. hook 注入 context 模板（完整文本）

```text
[DT-SENSE] {project_name} | {status} | KG {kg_status}
path: {project_path}
stats: {methods}m {classes}c {vectors}v | build: {last_build}
{cwd_brief}
{projects_hint}
{candidates_brief}
{degraded_brief}

搜索触发: 服务/配置/凭据/部署/历史决策→dt_search_kg(q,limit=5); hop0=事实,hop1+=线索; 按project过滤; 纯代码→读源码或code世界; 闲聊→不查; 每任务L1≤1次; 10s超时=降级
禁止: 凭记忆答项目事实; 伪造结果; 输出key/密码; KG故障阻塞任务→读磁盘并标⚠; 重复/碎查
```

实测体积: 模板 0.53KB / 常规渲染 0.66KB / 最坏（degraded+candidates 全填）≈1.0KB —— **恒 ≤1.5KB**。

### 占位符契约（插件渲染规则）

| 占位符 | 来源 | 渲染规则 | 降级默认 |
|---|---|---|---|
| `{project_name}` | `dt sense --json` `.project.name` | 直接取 | `?` |
| `{project_path}` | `.project.path` | 直接取 | 会话 cwd |
| `{status}` | `.status` | `indexed` / `registered_not_indexed` / `unregistered` | 同上 |
| `{kg_status}` | `.degraded` | 空→`healthy`; 非空→`degraded:[memgraph,…]` | `unknown` |
| `{methods}{classes}{vectors}` | `.stats.*` | 整数 | `0` |
| `{last_build}` | `.stats.last_build` | ISO 时间截到分钟; 空→`never` | `never` |
| `{cwd_brief}` | `.dirs`(top5) `.languages`(top5) `.key_entities`(top5) | 一行: `dirs: src:63 | langs: java:100% | 关键实体: queryById(m,11),…`; 实体格式 `name(kind,in_degree)`; 空→`-` | `-` |
| `{projects_hint}` | 注册表 `~/.config/digital-twin/config.yaml` `projects:` | `注册项目: aflm/doctor-center,user-center,…共N个`(取前5+计数) | 空行 |
| `{candidates_brief}` | `.candidates` | `unregistered` 时: `候选: <前3> 未注册, 建议 dt build --full` | 空行 |
| `{degraded_brief}` | `.degraded` | 非空时: `⚠ KG degraded: [memgraph] 查询可能为空, 降级读磁盘` | 空行 |

### 插件实现要点（沿用评审定案）

- 每会话一次: `is_first_turn` + `session_id` 内存缓存; 非首轮 `return None`。
- 路径解析: 从 `extra.user_message` 匹配注册表项目名（含别名, 如 `user-center`→`uvp-user-center`）→ 命中 `dt sense <根路径> --json`; 未命中回退 `dt sense <cwd> --json`。
- 渲染: 用上表填占位符, 输出整体截断 ≤2000 字符; 任何失败（非零退出/坏 JSON/超时 5s）`return None`, 绝不 crash。
- 可选优化: `user_message` 无项目关键词 且 cwd 未注册 → 跳过注入（纯闲聊省 token）, 默认关闭。
- 注入进 **user message**（非 system prompt）保住 provider prompt cache。

---

## 2. SOUL.md 增补段落（完整文本, 直接追加到 ~/.hermes/SOUL.md 末尾）

```markdown
## 知识图谱(KG)感知准则

本机数字孪生知识图谱（Memgraph+Qdrant, 经 MCP 工具 dt_sense / dt_search_kg / dt_memorize 与 CLI dt 访问）
是项目/服务/配置/历史决策的记忆层。**KG 是加速器, 不是单点依赖：查得到就用, 查不到就读磁盘, 绝不因 KG 阻塞任务。**

### 三层漏斗（按需下沉, 每层有界）
- L0 感知: 每任务开头由插件注入 [DT-SENSE] 简报（项目名/索引状态/统计/关键实体/KG 健康）。注入即已感知, 不重复手动查。
- L1 检索: 任务涉及具体服务/配置/凭据/部署/历史决策时才查 `dt_search_kg(query, limit=5)`（knowledge 世界）。
  hop=0 当事实, hop≥1 只当线索; 命中含 project 字段时按当前项目过滤, 丢弃跨项目噪音。
- L2 定向: 仅当 L1 不够精确; 已知 elementId 走 memgraph `run_cypher_query`, 显式 RETURN 字段白名单, 不 RETURN n。

### 搜索时机决策清单（每次收到任务先过一遍）
1. 任务与项目无关（闲聊/元对话/本工具操作）？ → 不查 KG。
2. 已注入的 [DT-SENSE] 是否已覆盖所需事实？ → 覆盖则不查。
3. 需要服务地址/配置/凭据/部署信息/历史决策？ → dt_search_kg。
4. 需要定位/理解代码逻辑？ → 读源码, 或 dt_search_kg(code 世界), 不查 knowledge 世界。
5. 需要精确属性（已有 elementId/确切命名）？ → L2 白名单查询。
6. 每任务 L1 自动查询 ≤1 次; 查询前先定 2-4 个具体关键词一次查完, 不碎查、不用宽泛词。

### 降级与禁止
- dt_sense 永不失败: degraded 列表=后端缺失, stats 全 0 ≠ 项目没代码。sense/search 设 10s 心理超时, 超时按降级处理, 不等 120s 兜底。
- KG 不可达 → 读磁盘（配置文件/README/部署脚本）完成任务, 上下文标 ⚠, 恢复后 dt_learn 补记。
- 禁止: 伪造 sense/search 结果; 输出或提交 API key/密码; 把 hop≥1 当事实; 直接采用跨项目命中; 每轮重复查询。
- 事件记录已自动化（代码修改/部署/配置变更/决策由 hook 写库）, 无需手动 dt event; 用户说"记忆/记一下"是命令, 立即 dt_memorize。
```

---

## 3. 三种典型任务示例

### 示例 1：改代码 bug

**用户消息**: `doctor-center 的 queryById 返回顺序不对，帮我定位并修一下`

**注入效果**（user_message 命中注册表 `doctor-center` → 渲染）:

```text
[DT-SENSE] doctor-center | indexed | KG healthy
path: /data/aflmProjects/aflm/doctor-center
stats: 63m 13c 63v | build: 2026-08-11T14:34
dirs: src:63 | langs: java:100% | 关键实体: queryById(m,11),success(m,7),selectDoctorIdByGroup(m,6),selectAllByPage(m,6),update(m,5),…
注册项目: aflm/doctor-center,user-center,…共23个

搜索触发: 服务/配置/凭据/部署/历史决策→dt_search_kg(q,limit=5); hop0=事实,hop1+=线索; 按project过滤; 纯代码→读源码或code世界; 闲聊→不查; 每任务L1≤1次; 10s超时=降级
禁止: 凭记忆答项目事实; 伪造结果; 输出key/密码; KG故障阻塞任务→读磁盘并标⚠; 重复/碎查
```

**Agent 行为**:
1. 注入已给 project + `queryById` 实体线索（in_degree=11 是热点方法）→ **不重复 dt_sense**。
2. 决策清单第 4 条: 纯代码任务 → **不查 knowledge 世界**; `dt_search_kg("queryById", limit=5)`（code 世界）定位方法 → hop=0 命中当事实, 拿到 file_path/start_line。
3. 读源码确认排序逻辑（注入的 `success/insert/update` 实体提示该模块是 CRUD 风格, 排序多半在 SQL 或 Service 层）。
4. 修复 → 保存文件 → **hook 自动触发 code_modified 单文件增量 build**, 不手动全量构建。
5. 验证: `dt search --world code "queryById"` 回读 + 报告（动作/项目/文件/结果/下一步）。

### 示例 2：查服务配置

**用户消息**: `user-center 连的数据库和 nacos 地址是什么？`

**注入效果**（命中别名 `user-center`→`uvp-user-center`, 假设已索引）:

```text
[DT-SENSE] user-center | indexed | KG healthy
path: /data/aflmProjects/aflm/uvp-user-center
stats: 210m 42c 210v | build: 2026-08-11T09:12
dirs: src:210 | langs: java:100% | 关键实体: UserController(m,18),UserService(m,14),login(m,9),…
注册项目: aflm/doctor-center,user-center,…共23个

…（搜索触发/禁止两行同示例 1, 固定文本）
```

**Agent 行为**:
1. 决策清单第 3 条: 涉及服务/配置/凭据 → **触发 L1**。
2. `dt_search_kg("user-center nacos database", limit=5)`（knowledge 世界, 一次查完）。
3. hop=0 命中 `NacosConfig`/`Configuration` 实体 → 当事实; hop≥1 仅留实体名作线索; 按 `project` 字段过滤（实测会混入 doctor-center 的 Config, 直接丢弃）。
4. 拿 elementId → memgraph `run_cypher_query`: `RETURN n.auth_user, n.hostname, n.port, n.url, n.service_type`（字段白名单, **不回显 auth_password**, 不 RETURN n）。
5. 回答 hostname/port/url; 密码只告知存在性不给出值。
6. 若 KG degraded（注入里有 `⚠ KG degraded` 行）→ 降级读 `uvp-user-center` 的 application.yml / nacos 拉取脚本, 不阻塞。

### 示例 3：纯闲聊

**用户消息**: `在吗？今天有点累`

**注入效果**（无项目关键词 → 回退 cwd `/data/myProject/digital-twin-v2` → 未索引）:

```text
[DT-SENSE] digital-twin-v2 | registered_not_indexed | KG not_indexed
path: /data/myProject/digital-twin-v2
stats: 0m 0c 0v | build: never
cwd_brief: -
注册项目: aflm/doctor-center,user-center,…共23个

…（搜索触发/禁止两行同示例 1, 固定文本）
```

**Agent 行为**:
1. 决策清单第 1 条: 闲聊 → **不查 KG**; 不调 dt_search_kg、不 build、不 dt_learn。
2. 注入简报仅作背景, **不主动展开项目话题**; 简短共情回复。
3. `0m 0c | build: never` 按 not_indexed 理解（未索引/降级信号）, 不误读为"这个项目没代码", 本次也无需处理。
4. 成本说明: 每会话一次约 0.6KB 注入是设计内开销; 若希望纯闲聊连注入都省, 启用插件"无项目关键词+未注册 cwd → 跳过注入"优化项。

---

## 4. 落地清单

1. 按 §1 模板 + 占位符契约实现 `~/.hermes/plugins/dt-sense/`（plugin.yaml + __init__.py, 逻辑沿用评审定案）。
2. §2 文本追加到 `~/.hermes/SOUL.md` 末尾。
3. 验证: `hermes hooks list` / `hermes hooks doctor` / `hermes hooks test pre_llm_call --payload-file X`; 三种任务各开新会话实测注入效果与 agent 行为。
4. 回退: `rm -rf ~/.hermes/plugins/dt-sense/` + 从 SOUL.md 删除本段; CLI 新会话即时生效, gateway 需 `systemctl --user restart hermes-gateway`。
