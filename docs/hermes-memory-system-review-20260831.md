# Hermes 记忆体系问题总结与待办（2026-08-31）

> 背景：排查"银盛支付手续费"新会话（session 20260831_174636_3ab765）完全没效果的问题，
> 顺带把 Hermes ↔ digital-twin KG 记忆体系整体过了一遍。
> 结论：KG 里其实有全部答案（project=offen-pay 下 8 命中），但注入/引导链路有缺陷导致 agent 盲查。
> 参考导出：`/data/aflmProjects/others/pay/yinsheng-payrate.json`

---

## 一、已修复（本会话已完成）

### 1. memory 世界检索能力（dt 侧 Rust）
- `src/application/context/search_memory.rs`：
  - world=memory 支持 `project` 过滤（原来传了也被丢弃）
  - 标签集合补 `:Knowledge`（dt memorize 写入的节点，原来 world=memory 查不到——隐藏 bug）
  - 检索字段补 `content`（Knowledge 节点存 content 不存 details）
  - 多词查询从"整串 CONTAINS"改为"关键词拆分 AND 语义"（"净盘 分账"这种原来 0 命中）
- `src/application/context/search_mcp.rs`：调用点把 project 传给 search_memory
- 测试：678→679 全过

### 2. dt-memory 插件 v3（Hermes 侧，plugins/dt-memory/）
- **prefetch 默认零注入**：原来每轮无条件灌最多 8 条记忆原文（1600 字符/token 爆炸）；现在只有显式记忆意图词（记住/记得/上次/之前说/remember 等）才定向检索项目+全局各 4 条
- **system_prompt_block 改为检索方式引导**：注入"怎么用 dt_search_kg 查记忆"，不注入记忆本体
- **新增 `llm_extract.py`（LLM 主动整理）**：
  - 复用 Hermes LLM 配置（config.yaml model 段 + env key + /models 自动发现真实模型，解决 `auto` 无渠道问题）
  - 每 3 轮 / 会话结束 / 压缩前自动提炼 fact/decision/preference/convention，importance<3 丢弃，scope 区分 project/global
  - 不依赖用户说"记住"
- **去重合并**：写入前检索相似，score≥0.82 复用 entity_id 更新（MERGE 覆盖），防记忆膨胀
- **写路径 project 区分**：项目记忆 `--project <当前项目>`，全局记忆 `hermes-global`
- 测试：29 个全过（新增 v3 行为测试 + llm_extract 解析测试）

### 3. 内置记忆禁用（~/.hermes/config.yaml，经 `hermes config set`）
- `memory.memory_enabled: false`
- `memory.user_profile_enabled: false`
- MEMORY.md / USER.md 不再注入 system prompt，KG（world=memory）成为唯一长期记忆
- 验证：`hermes config get` 返回 false/false/dt-memory

### 4. dt-sense 容器目录引导（plugins/dt-sense/）
- `_match_cwd` 对"注册容器目录"（如 /data/aflmProjects/others/pay 含 offen-pay/offenpay-ui 子项目）不再返回 None，注入容器简报
- 新增 `_is_container_of_registered()` 判断
- 容器简报明确列出子项目名 + 警告"不要用目录名(pay)当 project，会过滤掉全部命中"
- 测试：12→14 个全过（新增 2 个容器测试）

---

## 一·5. dt-sense 透传改造（第二轮，用户要求"注入内容 = dt sense 输出 + 去硬编码"）

### 5a. 移除硬编码项目名（ALIASES 表）
- 删除 15 项硬编码别名（user-center→uvp-user-center、pay-center→uvp-pay-center、digital-twin→digital-twin-v2 等）
- 项目名来源**唯一 = registry**（~/.config/digital-twin/config.yaml）；新增项目只需注册，插件自动可见
- 新增测试约束：逻辑代码（ast 解析去 docstring）不含任何具体项目名

### 5b. 注入内容 = dt sense 原生文本输出
- 删除自拼渲染层（_render_brief / _fmt_ts 约 90 行）
- `_run_sense` 改跑 `dt sense <path>`（文本模式），stdout 原样注入
- 效果：注入内容始终与 dt CLI 一致，新状态/新项目自动反映

### 5c. 追加一行 [KG] 检索引导（弥补透传后丢掉的工具指引）
- `_search_guidance()`：按 sense 输出追加一行
  - 容器场景 → `dt_search_kg(project=<子项目名>)` + "不要用目录名当 project"
  - indexed 场景 → `dt_search_kg(project=<项目名>, limit=5)`
  - unregistered → "可先 dt build 注册"
- 原因：透传后 dt sense 原生输出不含工具指引，18:08 会话实测 agent 只翻磁盘不用 dt_search_kg

### 5d. 无匹配时注入最小简报（替代静默跳过）
- `_minimal_brief()`：≤200 字符，列出注册项目数 + 提示先 dt_sense 确认项目名
- 修掉"no project match, skip briefing"导致 agent 完全无引导的问题

### 5e. 18:08 新会话验证（session 20260831_180752_5e40d5）
- 日志铁证注入成功：`18:08:10,509 dt-sense: injected briefing for /data/aflmProjects/others/pay`
- 注入机制澄清：**pre_llm_call 返回值注入 user message，不是 system prompt**（plugins.py:5088-5092 "Context is ALWAYS injected into the user message, never the system prompt... ephemeral — never persisted"）。所以 JSON 导出的 system_prompt 字段里看不到 [DT-SENSE] 是**正常设计**，不能据此判断未注入
- 行为改善：agent 不再 curl 百度/Bing，直接引用"知识图谱记忆 0.38%" + 读设计文档 + 确认代码常量（vs 旧会话 9 轮磁盘翻找）
- **遗留问题**：该会话仍没用 dt_search_kg 工具（靠读 yinsheng-payrate.json 间接获得 KG 信息）——5c 的 [KG] 引导正是为此

### 5f. 修复 cwd 解析根因：用会话级 cwd 而非进程 cwd（第三轮）
- **症状**：用户在验证时，dt-sense 注入的是 digital-twin-v2 简报，而不是用户实际操作的 pay 目录
- **根因**：dt-sense 用 `Path.cwd()`（Hermes 进程启动目录）。gateway 进程 cwd=/home/luis/.hermes（`/proc/<pid>/cwd` 实证），环境无 TERMINAL_CWD → gateway 模式下永远匹配不到用户操作的项目
- **修复**：新增 `_resolve_cwd()`，用 Hermes 官方 `agent.runtime_cwd.resolve_agent_cwd()`（优先级：**会话级 cwd（contextvar）** → TERMINAL_CWD → 进程 cwd），import 失败才兜底 Path.cwd()
- **机制依据**：
  - gateway/session_context.py:280-282 会话开始时 `set_session_cwd(cwd)`（contextvar）
  - agent/turn_context.py:1171-1187 pre_llm_call 在会话主循环内触发，contextvar 同任务传播
  - 所以 dt-sense hook 能拿到会话真实 cwd
- **验证**：模拟 set_session_cwd(pay) → `_resolve_cwd()` 返回 pay → 注入 pay 容器简报（含 offen-pay），不再注入 digital-twin-v2
- 测试：21 个全过（新增 TestResolveCwd 3 个）

---

## 二、待修复（本会话分析发现，尚未动手）

### A. dt-sense 的 cwd 盲区（最严重）
- **症状**：会话 20260831_174636_3ab765 日志铁证：
  `dt-sense: no project match, skip briefing (session=20260831_174636_3ab765)`
  system_prompt 无 [DT-SENSE] 块（JSON 实证，6187 字符里只有 [DT-MEMORY]）
- **根因**：`pre_llm_call` hook 字段里**没有 cwd**（hermes_cli/hooks.py:147-154 只传 session_id/user_message/conversation_history/is_first_turn/model/platform）。dt-sense 只能用 `Path.cwd()`（Hermes 进程启动目录）。用户从别处启动 hermes、会话内 terminal cd 到项目 → 简报在首轮就判定跳过（工具环境 session cwd 是 17:46:50 才创建，dt-sense 17:46:42 判定时看不到）
- **影响**：agent 不知道项目名 → 盲查 KG / 带错 project / curl 百度/Bing 浪费 token
- **修法**：dt-sense 增加降级链：
  1. user_message 里的 @file 路径（成功会话 20260830_174654_2890e4 靠这个命中了 offenpay-ui）
  2. 进程 cwd
  3. **无匹配时注入"最小简报"**（列出注册项目名 + 提示先 dt_sense），而不是静默跳过。最小简报 ≤200 字符，成本极低，收益是 agent 永远有引导

### B. AGENTS.md 决策表缺少"先 KG 后外部搜索"顺序约束
- **症状**：会话 msg1/msg4/msg7 先 curl 百度/Bing 再查 KG（Bing 甚至被引导到"银价"无关内容），浪费 3 次 API 调用
- **修法**：AGENTS.md 决策表加一条"服务/配置/业务知识 → 先 dt_search_kg；KG 明确 0 命中才允许外部搜索/翻磁盘"

### C. dt-memory system_prompt_block 引导不完整
- **症状**：当前引导只提 memory 世界，且出现错误的 `project=pay`（_infer_project 从 cwd 推断，该会话 cwd=None 推断出 "pay"）
- **修法**：
  - 引导补 knowledge/doc 世界
  - project 名不确定时提示"不带 project 全局查 或 先 dt_sense 确认"
  - 避免引导里出现可能错误的 project 名（宁可不写 project，写"用 dt_sense 确认"）

### D. dt_search_kg 结果缺乏"可信度提示"
- **症状**：会话里 final_score=0.948 的高置信命中（hop=0，snippet 直接给出 0.38%），agent 仍去 Bing 搜——不信任 KG 结果
- **修法**：dt_search_kg 返回里 hop=0 且 final_score≥阈值时加"✅ 高置信命中，可直接采用"标记；或在 AGENTS.md 决策表强化 hop=0 当事实

---

## 三、设计取舍（建议，需拍板）

### E. 记忆作用域：project 推断不可靠
- `_infer_project` 靠 git root basename，但容器目录/无 git 目录会推断出错误项目名（如 pay）
- 选项：
  - (a) 推断失败时一律写全局（hermes-global），宁宽勿错
  - (b) 会话内首次 dt_sense 成功后再定 project
  - (c) 保持现状

### F. 主动记忆整理的频率与成本
- 现在每 3 轮 + 会话结束 + 压缩前调 LLM 提取，deepseek-v4-flash 一次约 1-2k token
- 长会话成本可控，但频繁短会话（cron/子代理）会有开销
- 选项：
  - (a) 保持
  - (b) 提高阈值到每 5 轮
  - (c) 只在会话结束提取（最省）

---

## 建议执行顺序（更新于 5e 之后）

1. ~~**A（dt-sense 最小简报）**~~ ✅ 已完成（5d，`_minimal_brief` + 容器匹配）
2. **B（AGENTS.md 顺序约束）**——一行文字，立竿见影省 token，仍未做
3. **C（dt-memory 引导补全）**——改 system_prompt_block 文案，仍未做
4. ~~**D（高置信标记）**~~ ✅ 部分完成（5c 的 [KG] 引导覆盖了"命中直接采用"；dt 侧返回格式标记未做）
5. **E/F**——看记忆作用域和成本偏好

剩余：B、C、E/F 为小改动（一行 md + 插件文案）。

---

## 附：关键证据速查

| 证据 | 位置 |
|------|------|
| 会话无 [DT-SENSE] 块（正常，见注入机制） | yinsheng-payrate.json system_prompt（6187 字符，只含 [DT-MEMORY]）；注入进 user message 不落库 |
| pre_llm_call 注入机制 | hermes_cli/plugins.py:5088-5092 "Context is ALWAYS injected into the user message, never the system prompt... ephemeral — never persisted" |
| 18:08 会话注入成功 | ~/.hermes/logs/agent.log: `18:08:10,509 ... dt-sense: injected briefing for /data/aflmProjects/others/pay (session=20260831_180752_5e40d5)` |
| 18:08 会话行为改善 | 无 curl 百度/Bing；msg7 推理直接引用"知识图谱记忆 0.38%"；读设计文档+代码常量 |
| 18:08 会话遗留：未用 dt_search_kg | 工具调用全为 search_files/read_file（靠读 yinsheng-payrate.json 间接获得 KG 信息）→ 5c [KG] 引导 |
| dt-sense 跳过日志（旧） | ~/.hermes/logs/agent.log: `2026-08-31 17:46:42,274 ... dt-sense: no project match, skip briefing (session=20260831_174636_3ab765)` |
| 工具环境 cwd 晚于判定 | 同日志 `17:46:50,555 ... Session snapshot created (session=2008ad1d7865, cwd=/data/aflmProjects/others/pay)` |
| dt_search_kg 实际命中 | 旧会话 msg8：`dt://entity/offen-pay/Concept/银盛支付手续费` final_score=0.948，snippet "费率0.38%" |
| agent 无视命中继续 Bing | 旧会话 msg9：curl bing.com 搜"银盛支付 手续费是多少" |
| KG 完整答案 | `dt search "银盛 手续费" --world all --project offen-pay` → 8 命中（知识+文档+代码） |
| 错误 project 过滤致 0 命中 | 旧会话：`dt_search_kg(project=pay)` 全 0（银盛知识挂在 offen-pay 下） |
| pre_llm_call 无 cwd 字段 | hermes_cli/hooks.py:147-154 字段列表 |
| dt-memory 引导含错误 project=pay | yinsheng-payrate.json system_prompt [DT-MEMORY] 块 |

---

## 四、2026-08-31 晚间验证：注入闭环复查（yinsheng-payrate.json 溯源）

> 背景：用户要求"再验证一下，确认是否注入"，拿 18:16 会话导出 `yinsheng-payrate.json`
> （session 20260831_181633_cd820a，问"银盛支付手续费是多少"）追查该会话结论是否真正进了 KG。

### 验证结论

| # | 检查项 | 结果 | 证据 |
|---|--------|------|------|
| 1 | 银盛手续费知识是否在 KG | ✅ 在 | `dt_search_kg(project=offen-pay)` 命中 `Concept/银盛支付手续费`(0.38%)、`Config/转账手续费`(0.02%)、`Config/提现手续费`(1元)、`Concept/手续费` 等，hop=0 高置信；源头是设计文档索引（2026-08-21-fee-rule-v2.md），**非会话记忆** |
| 2 | 18:16 会话结论是否注入 KG | ❌ 未注入 | Memgraph `:Knowledge` 节点 10:xx-11:xx UTC（=18:xx-19:xx CST）期间无该会话写入；该会话工具调用全是 search_files/read_file，没调 dt_search_kg / dt_memorize，靠读 yinsheng-payrate.json 间接获得答案 |
| 3 | "会话自动入库" hook 是否存在 | ❌ 不存在 | 图库无 `:Conversation/:Session/:Event` 标签（全库标签只有 Method/Entity/Class/Module/Document/Knowledge/Project/Experience/Playbook）；AGENTS.md 表格宣称的 session_ended 事件自动化未落地——实际只有 dt-memory 的 `on_session_end` 做 LLM 记忆提取（写 :Knowledge），不是事件节点 |
| 4 | memory 世界检索为何此前 0 命中 | ✅ 已修复 | search_memory.rs 已补 :Knowledge 标签 + project 过滤 + content 字段 + 多词 AND 语义；实测 `world=memory` 检索命中 |
| 5 | `dt memorize` 写入链路是否通 | ✅ 通（但见下方 G 截断 bug） | 实测写入 `yinsheng-fee-rules-20260831` 后 Memgraph 可见、memory 检索命中 |

### 本次实测补写的记忆

- `yinsheng-fee-rules-20260831`（project=offen-pay）：银盛渠道手续费 4 项完整规则
  （支付 0.38% 商户承担结算侧扣 / 分账 0.02% 君乐承担 / 二段转账 0.02% 接收方承担 / 提现固定 1 元用户承担 +
  实付口径、净盘公式、CG_PAY 免支付费）

---

## 五、新增优化建议（2026-08-31 晚验证发现）

### G. 【已修复 2026-08-31】`parse_details` 按 `,`/`;`/`:` 分隔导致记忆 content 截断（真 bug）

- **症状**（Memgraph 实证）：
  - `mem-af5cc4d200d8`「变量拆分可绕过Hermes重启拦截」content=`通过变量拆分服务名（如 S=restart` ← 被 `,`/`(` 截断
  - `auto-c9fcdd2365ac`「4. Hermes 侧还有两层依赖注入」content 同样截断
  - 本次实测写入 `yinsheng-fee-rules-20260831` content 只存到第一个 `,`（`银盛渠道手续费4项: 1.支付手续费0.38%(商户承担`）
  - 大量 `auto-*` 节点 name=`KnowledgeAdded`（没传 `name:` 键，fallback 到 knowledge_type）
- **根因**：`src/application/knowledge/knowledge/annotation.rs` `parse_details()` 按 `[';', '\n', ',']` 分隔再按首个 `:`/`=` 拆分；而 dt-memory 插件（`handle_tool_call` / `_write_with_dedup` / llm_extract 链路）构造的 details 是 `name: xxx; content: <自由文本>`，自由文本里几乎必含中文逗号/分号/冒号 → content 被拦腰截断，summary/name 也可能碎
- **✅ 修复方案（已实施）**：
  - `parse_details` 改为两层解析：先按 `;`/`\n` 分「段」，段内再处理
  - 新增 `WHOLE_VALUE_KEYS`（content/details/description/definition/body/text）：全文键的值**可跨段吞并**，直到遇到「形如新键的段」（`is_key_value_segment`：段首是纯 ascii 小写字母/数字/`_`/`-` 组成的 `<key>:`/`<key>=`）才停
  - 非全文键保持旧行为（按 `,` 拆多值，兼容 `scope: A, B`）
  - 中文 `=`/`:` 片段（如正文里的 `净盘=订单金额-支付手续费`）不会被误判为新键边界（is_key_value_segment 只认纯 ascii 键）
  - 新增 3 个测试（中文标点全文保留、`=` 分隔、非全文键多值兼容），annotation 模块 17 测试全过，lib 682 全过
- **端到端验证**（release 构建后实测）：`dt memorize` 写入含中文逗号/分号/冒号/括号/百分号的自由文本 content → Memgraph 中 content **完整保留**，未被截断；测试节点已清理

### H. 过时认知记忆需清理/更新

- `decision-dt-hermes-memory-provider`（project=hermes）content 写死"memory世界无写入CLI入口，恒空不可用"——**已被 search_memory.rs 修复推翻**，且会误导后续检索（hop=0 当事实用）
- 建议：更新该记忆或在文档标注；llm_extract 去重阈值 0.82 对这类"同一主题新旧矛盾"无解，需要人工清理一次

### I. 【已修复 2026-08-31】AGENTS.md「事件已自动化」表格与实现不符

- 表格声称 code_modified/jenkins_deploy_completed/config_changed/decision_made/bug_fix_recorded/session_ended/pod_event_occurred/k8s_synced 由 hook 自动入库，但：
  - 图库**没有任何事件标签节点**（:Modification/:Deployment/:ConfigChange/:BugFix/:Conversation/:Session/:PodEvent/:K8sSyncEvent 全不存在）
  - 现有写入实际只有两类：代码索引（Method/Class/Entity）和记忆（:Knowledge）
- **✅ 已修复**：AGENTS.md「事件已自动化」表格改为「事件与记忆写入（实际状态）」——区分 ✅ 生效（代码索引、dt-memory 记忆提取）与 ❌ 未落地（全部事件标签），并注明"无需手动 dt event（事件路径未实现）"

### J. 验证过程中的其他发现

- `dt memorize` 打印"知识已写入"但 details 是自由文本时（不带 name:/content: 键），节点是空壳（name=KnowledgeAdded、content 空）——CLI 无解析失败提示，**建议加校验/警告**（G 修复后 name/content 都能正确解析，但裸自由文本仍会产生空壳，建议后续加警告）
- `dt search --world memory` CLI 与 MCP dt_search_kg 行为一致（都命中新写入），链路无差异
- 索引漂移：`dt health` 报 Memgraph 106735 方法 ≠ Qdrant 107165 向量 → **已执行 `dt build --full` 重建**（2026-08-31 晚，后台运行）
- **新发现：auto-build 增量构建 600s 超时**（`/tmp/dt-build-queue/auto-build.log` 实证 19:41、20:52 两次 `构建失败 digital-twin-v2: timed out after 600 seconds`）。文件监听器每次文件变更触发增量构建，但单项目构建在 LLM backfill 阶段就超过 10 分钟超时被杀。与全量重建并发时更易触发。建议：增量构建加 `--no-llm-backfill` 或提高超时

### K. 【已修复 2026-08-31】`--no-llm-backfill` 关不掉 Phase 2 方法级 LLM 分析

- **症状**：`dt build --full --no-llm-backfill` 仍逐方法调 LLM（日志 `LLM 方法分析开始` 持续），全量重建数万方法 × 20-30s/方法 → 单项目数小时
- **根因**：`src/application/build/pipeline.rs` Phase 2 触发条件是 `llm_client.is_some() && snapshot_repo.is_some()`，无视 `llm_backfill` 开关；该开关只控制 Phase 2.5/2.6（末尾缺口补偿）
- **✅ 修复**：`phase2_enabled = self.llm_backfill`，Phase 2 与 2.5/2.6 统一受 `--no-llm-backfill` 控制
- **验证**：修复后 archive-api 644 方法 50 秒完成（此前数小时）

### L. 【已修复 2026-08-31·索引漂移根因】full_rebuild 漏清 CODE_METHODS 集合

- **症状**：`dt health` 持续报索引漂移；全量重建后仍在（实证：Memgraph 101254 ≠ Qdrant 107302）
- **根因**：`src/application/build/strategy/full_rebuild.rs` 的 `prepare` 清空向量只清 `KG_NODES/DOC_CHUNKS/CODE_CLASSES`，**漏掉 `CODE_METHODS`** → 旧方法向量残留，Memgraph 方法节点已删但 Qdrant 向量还在
- **✅ 修复**：清理循环加入 `CODE_METHODS`
- **验证**：修复后对 digital-twin-v2 重新 full 构建，code_methods 4231→1947（清掉 2284 残留）；offen-pay 5811→2899（与 Memgraph 完全一致）；yijianbao 29112→（重建中）
- **提速手段**（本次重建采用）：
  - `config.yaml` `batch.embed_concurrency: 1 → 8`（embed 并发 8 倍）
  - 去代理直连 SiliconFlow（`env -u http_proxy...`，0.33s→0.15s/请求，快 2.3 倍）
  - `--no-llm-backfill` 关 phase2（见 K）
  - 效果：单项目从数小时降到 1-4 分钟

### M. 【已修复 2026-09-01·yijianbao 构建"死锁"根因】UNWIND MERGE 无 method_id 索引 → 全表扫描

- **症状**：yijianbao 全量构建（8 万文件/2.1GB，含 3.3 万 JS）反复"死锁"——日志停滞、23 线程全 futex park、Memgraph 有 UNWIND $methods 事务 running 数分钟不返回
- **根因**（双重）：
  1. **Memgraph 唯一约束不隐式创建索引**：`CREATE CONSTRAINT method_id_unique` 存在，但 `SHOW INDEX INFO` 无 `Method.method_id` 索引。`UNWIND $methods MERGE (n:Method {method_id})` 每次 MERGE 全表扫描（8 万方法）→ 500 批 × 全表扫描 = 事务挂起数分钟，且**持有存储锁**（`Cannot get read-only access to the storage`），阻塞所有其他事务 → 表现为死锁
  2. **写入并发**：`tokio::join!(write_methods, write_classes, write_modules)` 三个写入流并行，放大锁竞争（串行化后仍慢，说明主因是索引）
- **✅ 修复**：
  - `schema.rs` INDEX_STATEMENTS 补 4 个索引：`Method.method_id`、`Class.class_id`、`Module.module_id`、`Project.name`
  - `pipeline.rs` 三个写入流改为顺序 await（消除并发锁竞争）
  - 手动对当前实例执行 CREATE INDEX（幂等）
- **验证**：建索引后 yijianbao 全量构建方法写入完成，Memgraph 29112 = Qdrant 29112；全局 `dt health` = **Memgraph 101633 = Qdrant 101633 完全一致**
- **遗留**：yijianbao 有 3497 个 md 文件，文档 LLM 分析（processors.llm）仍需较长时间（每 chunk 20-30s），非死锁、会自然完成
