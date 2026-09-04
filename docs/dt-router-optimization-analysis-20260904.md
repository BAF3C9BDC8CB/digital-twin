# dt router 命令优化分析报告

> 日期：2026-09-04
> 范围：`dt router` 命令（智能路由搜索）现状分析、问题根因与优化建议
> 相关代码：`src/interfaces/cli/router.rs` / `src/application/context/fusion.rs` / `src/application/context/search_mcp.rs` / `plugins/dt-router/__init__.py`

## 背景

`dt router` 是 `dt search` 的升级版智能路由搜索，宣称多层路由：

| 层级 | 职责 | 说明 |
|---|---|---|
| L0 提前拦截（early-exit） | 纯规则判断查询是否值得检索，寒暄/算术/闲聊直接返回「无需检索」 | 开关 `kg_router.early_exit.enabled` |
| 第一层 意图识别 | 分析查询，识别 code/knowledge/doc/config 意图 | `analyze_query_intent()` |
| 第二层 策略路由 | 根据意图与 world 决定检索参数 | `build_route()` |
| 第三层 结果过滤 | 复用 LLM（kg_router 已接入）判断每个命中相关性 | 开关 `kg_router.result_filter.enabled`，默认关 |

在 Hermes 环境下由 `plugins/dt-router` 在每 turn 开头（`pre_llm_call`）以
`dt router <user_message> --json --limit 5` 调用，命中压缩为 `<knowledge_context>` 简报注入。

用户反馈两大问题：

1. 简单闲聊拦截无法阻止「没有必要查询知识图谱」的消息（如：帮我实现、分析某个文件的内容、给我一些建议等）；LLM 过滤似乎并没有起作用，没有过滤掉无效内容。
2. 默认接入 Hermes 后，每次查询都是全局的（world=all），想查询 code / 知识 / 记忆等也没有区分。

## 一、闲聊拦截失效 + LLM 过滤无效

### 1.1 实测复现

| 查询 | world | 命中 |
|---|---|---|
| `你好` | none | （拦住了） |
| `帮我实现` | all | yijianbao 的 help / execute / activity / getHelp / applySuccess 等 |
| `分析某个文件的内容` | all | FileAnalysis / VerifyFileResult / "09-47个内置工具全解.md" 等 |
| `给我一些建议` | all | recommend / Tips / 2026-09-04-withdrawal-sync-fix.md / 候选注册建议 等 |
| `帮忙实现一个xx功能` | all | 同 `帮我实现`，10 条原始命中 |

开启 `--filter true` 后（`--explain` 可见 filter 已启用、阈值 0.60），以上三例
5 条命中几乎未被移除——LLM 过滤没有移除这些「无效内容」。

### 1.2 根因 1：L0 是「闲聊分词器」，不是「任务性小查询拦截器」

见 `src/interfaces/cli/router.rs:242`（`should_search` / `is_casual_query`）：

- 设计：查询文本能用闲聊词表**从头到尾完整贪心切分** → 判为闲聊返回「无需检索」；
  只要混入一个词表外的字词（含"实现/分析/建议"这类）即放行。
- 局限：`帮我实现`、`分析某个文件的内容`、`给我一些建议` 这类**任务性口头语**
  因带"实现/分析/建议"等词被放行，但这些词并不携带具体检索锚点（符号名、文件路径、
  配置项、图实体），进入检索也几乎只能抓到噪声。
- 结论：L0 需要从「闲聊分类」升级为「**值不值得检索**」判断——区分
  「帮我实现轮询功能 / 分析 Wxapp.php」 与 空泛的 「帮我实现 / 给我一些建议」。
  前者有锚点放行，后者直接短路。

### 1.3 根因 2（核心）：world=all 的 RRF 融合把所有分数压成 0.01x

`dt router` 默认 world=all（`src/main.rs` Router 默认 `--world all`；插件调用未传 world），
而 world=all 的检索走跨世界 RRF 融合：

- `src/application/context/search_mcp.rs:1133`：world == "all" 时
  code + knowledge + doc 三路各自召回后进入 `rrf_hits()`。
- `src/application/context/fusion.rs:60`：RRF 分数 = `1.0 / (60 + rank)`，
  **任何普通查询 top-1 恒 ≈ 0.0164**，与真实相关度断钩。
- code 世界内部本来有强信号：标识符精确通道给 0.95、关键词通道给 0.90、
  向量语义分 > 0.3（见 `search_mcp.rs` `search_code` / `search_doc`）——
  融合后这些差异被平均成等值，code/knowledge/doc 各占一个 rank 位。

这就是为什么三例命中 score 全部显示 0.016 的原因：RRF 分只表达「在多少个世界排第几」，
不表达相关度。LLM 过滤器拿这种分数做不了门槛，只能看 title+snippet 硬判。

### 1.4 根因 3：LLM 过滤的上下文过窄

`src/interfaces/cli/router.rs:774`（`judge_relevance`）：

- 对 code 命中只喂 `title`（方法名/类名）+ `snippet`（仅 `file_path: L行号-行号`），
  没有 signature / llm_analysis / 所属类 / project 等上下文；
  人在没有这些信息时也难以判断「这个方法是否有用」。
- 判断提示词偏向「严格相关」会误杀有效 code 命中，偏向宽松则无法过滤——
  现有实现取了保守的宽松分支（包含"不相关"才删，命中失败则保留）。

### 1.5 修改建议

按代价/收益排序：

1. **L0 增加任务性 gate**（Rust 侧统一实现）：
   - 在 `should_search` 之上叠一层：先用统一关键词提取（复用
     `keywords_of`，见 `retrieve.rs:245`，jieba 切词 + 虚词过滤）得到内容锚点；
   - 若锚点为空（查询只由 帮/给我/一下/建议/分析/介绍 等指令性/虚词构成），
     且不含文件后缀/标识符/路径/注册项目名 → 直接「无需检索」。
   - 样例：`帮我实现` / `给我建议` / `介绍一下` → 短路；
     `帮我实现轮询` / `分析 Wxapp.php` / `wxpay 退款怎么配置` → 放行。
   - 好处：复用现有关键词基础设施，与 code/doc 的 keyword 通道行为一致。

2. **world=all 保留原始 score 信号**：
   - `rrf_hits` 目前只把 RRF 分写回 `item.score`，导致 0.016 恒值。
   - 可选：保留各来源的语义分（如 `score_breakdown` / 单独字段），
     或在融合分上叠加 max(各世界原始语义分) 作为"排序用分"，
     让 LLM 过滤阈值/用户一眼看到有效 code 命中真实分 0.9x，而不是 0.016。
   - 注意：LOW_SCORE_THRESHOLD 等展示层逻辑在 world=all 时已主动跳过
     （见 `search_render.rs:216` 注释，作者已知 RRF 分数不可用）。

3. **LLM 过滤喂真实上下文**：
   - 传 `signature` / `llm_analysis`（code 命中 payload 直取）/ `summary`
     （knowledge 命中）/ `project` / 所属类 等，让判断有据。
   - code 命中的 snippet 替换为 llm_analysis 或 file_path + signature。

4. （可选）**过滤统计放进 JSON**：`--json` 输出里带 removed_count /
   各条 remove 理由，便于插件侧调试。

## 二、Hermes 下每次查询都是全局（world=all）、world 不区分

### 2.1 现状链路

- `plugins/dt-router/__init__.py` `_run_router()`（L259）：
  `dt router <query> --json --limit 5 [--project P]`，**不传 --world**。
- `dt` CLI Router 子命令（`src/main.rs`）默认 `world="all"` → 每次都全库三世界混排。

### 2.2 已有但不够的部分

- 插件 `_resolve_project()`（L234）会从 cwd / 消息文本 / 容器推断 project 并加
  `--project`，跨项目噪音已有缓解；project 聚类的 code 命中是当前项目，并不真"全库"。
- 用户感知的"没区分"，本质是 **knowledge/code/doc 三种世界混在一条结果里**，
  与 project 无关。而 world=all 的 RRF 融合又把 code 与 doc/knowledge 拉平，
  让"查代码"的意图被 doc/knowledge 命中稀释。

### 2.3 修改建议

1. **调用端负责路由（首选）**：
   - 路由入参从"自然语言 user_message 默认 all"改为**意图显式**：
     插件在 `pre_llm_call` / `subagent_start` / `delegate_task` 里根据消息类型
     决定 `--world code|knowledge|doc|memory`（或交给 Hermes 的
     `dt_search_kg(world=...)` / `dt_search(world=...)` 工具，而非 subprocess router）。
   - 参照 AGENTS.md 决策表：
     - 定位/理解代码 → world=code（KG 只索引 code 的代码实体）
     - 查记忆/配置/部署历史 → world=memory（`search_memory`，见 `search_memory.rs`）
     - 查文档 → world=doc
     - 查业务知识 → world=knowledge
   - Rust 侧保持"显式 world 优先"，不必用自然语言猜单世界——误猜世界比跨世界更贵。

2. **world=all 只作为兜底**：对确实跨域的问题（如"支付超时怎么排查"既涉及代码也涉及
   配置/记忆）才走 all；其余由调用端显式指定。

3. **memory 的语义澄清**：KG 记忆是 `:Knowledge` 节点 + `dt_memorize` 写入，
   在 Hermes 侧区分"查代码 / 查记忆"靠的是**选哪个工具、传什么 world**，
   而不是 router 的默认值。让 dt-router 插件与 MCP `dt_search_kg` 对齐同一套决策。

## 三、涉及改动文件与边界

| 文件 | 改动 | 注意 |
|---|---|---|
| `src/application/context/fusion.rs` | RRF 融合时保留/叠加原始语义分，避免 0.016 恒值 | 影响 dt search 展示？需确认 world=all 排序仍稳定 |
| `src/interfaces/cli/router.rs` | L0 加任务性 gate；LLM 过滤喂真实上下文 | 不改变 dt search 行为（router 独立） |
| `plugins/dt-router/__init__.py` | 调用前粗滤无锚点任务语；world 显式化 | 需配套 Rust 侧 world 解析 |
| `config/casual-words.txt` | （可选）补充任务性口语词 | 只增不减语义 |

边界注意：

- `dt router` 是 `dt search` 的升级入口，两者共享 `CrossWorldSearch` 后端与渲染；
  改动 router 的默认行为时不要让 `dt search` 行为随之改变。
- world=all 的 RRF 分数已有展示层规避（`search_render.rs` 低分警告在 all 时跳过），
  若在 fusion 层改动需同步检查该逻辑。

## 四、结论

1. L0 只管闲聊/算术，需要**叠加"任务性小查询"识别**：用统一 keywords_of 判断
   查询是否携带检索锚点（符号/路径/配置/实体），无锚点即短路。
2. LLM 过滤"不起作用"的根因不是过滤器本身，而是 **world=all 的 RRF 把
   所有命中分数压成 0.016 恒值 + 过滤上下文过窄**：先修分数/上下文，再谈阈值。
3. Hermes 下的 world 不区分是**调用端默认值问题**：dt-router 插件未传 --world，
   CLI 默认 all；应改为插件按消息类型显式传 world，router 的 all 只作兜底。
