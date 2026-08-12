# 测试会话审计材料：Hermes 是否使用 dt 知识图谱

## 背景
用户测试 Hermes 做代码问答时是否主动使用 digital-twin 知识图谱（dt_search_kg / dt search / run_cypher_query）。
三次测试均在 CLI 平台、模型 deepseek-v4-flash。任务都是"消息撤回流程是怎样的"（im-center 项目，腾讯云 IM 封装网关，Java，2287 方法已索引）。
dt-sense 插件已启用（开场注入 [DT-SENSE] 简报）。修复后的简报含强信号"✅ 本项目已索引 2287 方法/357 类——代码问题先用 dt_search_kg(world=code, project=X) 定位, 再读源码验证; 禁止只读源码跳过 KG"。

## 三层漏斗准则（SOUL.md/AGENTS.md）
- L0 感知：任务开头注入 [DT-SENSE] 简报（已注入即不重复查）
- L1 检索：需要服务/配置/凭据/部署/历史决策 → dt_search_kg(q, limit=5)
- L2 定向：已知 elementId 走 memgraph run_cypher_query 白名单查询
- 代码逻辑任务：**先 dt_search_kg(world=code, project=<项目名>, limit=5) 定位, 再读源码验证**（KG 命中=事实, 跨项目/低分=线索; 只有 dt_search_kg 不可用/超时才纯读源码, 且须标注 ⚠ KG 未用）
- 每任务 L1 自动查询 ≤1 次; 查询前先定 2-4 个具体关键词一次查完
- 10s 超时=降级; KG 不可达→读磁盘并标 ⚠

---

## 会话 1（20260812_083552_96d48d, 08:35, 22 条消息）
用户："im-center 的消息撤回流程是怎样的？"

工具调用序列（按时间序）：
1. tool_describe(mcp__digital_twin__dt_search_kg) — 先查工具 schema
2. dt_search_kg(query="消息撤回 recall revoke 撤回消息", world=code, project=im-center, limit=8)
   → 命中 7 条，全 im-center 代码实体，分数 0.28-0.73：
   - groupMsgRecallUpdate (MessageRecordMongoService.java L237-260, 0.73) 带 calls 列表
   - groupMsgRecall (GroupController.java L115-118, 0.72)
   - groupMsgRecall (GroupService.java L156-176, 0.69) 带 calls 列表（含 groupMsgRecallUpdate!）
   - msgWithdraw (MessageController.java L55-60, 0.68)
   - GroupMsgRecallRequest 构造器 ×2
   - groupMsgRecall@115 (0.28)
3. read_file ×4（GroupController L110-149, GroupService L145-189, MessageRecordMongoService L230-269, MessageController L45-74）— 用 KG 给的行号直接读
4. read_file(GroupMsgRecallRequest.java 全文) + search_files(msgWithdraw in servie/)
5. read_file(MessageService.java L140-184)
6. search_files(updateMessageRecord in MessageRecordMongoService, context=6)
7. read_file(MessageRecordMongoService.java L83-127)
8. search_files(AfterMsgWithdrawCallback 引用) + read_file(Message.java msgWithdraw 上下文) + read_file(AdminMsgWithdrawRequest.java)
9. 输出最终答案：两条链路（群撤回/单聊撤回）完整梳理，含 sleep 延迟、IsPlaceMsg=2/MsgFlagBits=8 标记、错误兜底集合、差异对比
用户追问："以上过程中，是否使用到了本地知识图谱？"
agent 详细解释：用了，L1 检索定位 5 个关键方法 → read_file 验证语义，KG 提供位置线索+签名+calls，源码提供业务细节。符合准则。

评价：规范。一次 L1 检索，带 world=code + project=im-center，命中后精准 read_file，无碎查。

---

## 会话 2（20260812_083804_4662a0, 08:38, 29 条消息）
用户："消息撤回流程是怎样的？"（注意：未提项目名）

工具调用序列：
1. dt_search_kg(query="消息撤回 recall revoke withdraw", limit=5) — **未带 world/project！**
   → 命中 knowledge 世界 5 条，全是跨项目噪音：boss-center 的 user.logout.success 国际化键(0.27)、message-center 消息模板(0.27)、boss-center no.delete.permission(0.26)、copartner main.js(0.26)、boss-center user.password.delete(0.26)。**与撤回无关**
2. agent 自述："knowledge 世界全是跨项目噪音。这是纯代码问题，改查 code 世界定位撤回相关代码"
3. dt_search_kg(query="撤回 revoke recall withdraw message", world=code, project=im-center, limit=5)
   → 命中 5 条：AdminMsgWithdrawRequest(0.71)、groupMsgRecallUpdate(0.67)、groupMsgRecall(0.66)、msgWithdraw(0.66)、GroupMsgRecallRequest(0.28)
4. read_file(MessageController.java 全文) + read_file(GroupController.java 全文) + search_files(withdraw|Withdraw|recall|Recall files_only, 13 文件)
5. search_files(msgWithdraw in MessageService, context=3) + search_files(groupMsgRecall in GroupService, context=3) + read_file(MessageRecordMongoService L200-289) + read_file(AfterMsgWithdrawCallback.java 全文)
6. read_file(MessageService.java L148-207) + search_files(AfterMsgWithdrawCallback 引用) + search_files(Message.java msgWithdraw, context=5)
7. search_files(updateMessageRecord context=10) + read_file(GroupMsgRecallRequest.java) + search_files(CallbackCommand|AfterMsgWithdraw, 20 个回调模型)
8. read_file(MessageRecordMongoService L83-112) + search_files(callback|Callback in controller, files_only) → TRTCCallBackController
9. search_files(Withdraw|Recall in TRTCCallBackController) → 0 命中（确认撤回回调未接入）
10. read_file(AdminMsgWithdrawRequest.java)
11. 输出最终答案：完整梳理 + 发现 AfterMsgWithdrawCallback 模型定义了但无消费方（回调未接入）

评价：功能正确但**第一次 L1 查询漏带 world/project**（浪费一次调用，返回 5 条无关噪音），靠 agent 自己判断纠正。若简报/工具描述更强调"代码必带 world=code+project"，可避免。L1 实际 2 次（违反 ≤1 次约束）。

---

## 会话 3（20260812_084247_6db944, 08:42, 23 条消息）
用户："消息撤回流程是怎样的？"

工具调用序列：
1. tool_search("digital twin search kg code") — 找 dt 检索工具（说明该会话的工具列表里 dt_search_kg 未直接列出/需按需加载）
2. tool_describe(mcp__digital_twin__dt_search_kg)
3. dt_search_kg(query="消息撤回 recall revoke withdraw 撤回消息", world=code, project=im-center, limit=5)
   → 命中 5 条（与会话 1 相似：groupMsgRecallUpdate 0.73, groupMsgRecall 0.72, msgWithdraw 0.70, GroupMsgRecallRequest ×2 0.28）
4. read_file ×3（GroupController L95-139, MessageController L30-79, MessageRecordMongoService L210-288）
5. search_files(groupMsgRecall\(|msgWithdraw\( content, 6 文件命中)
6. read_file ×4（GroupService L140-184, MessageService L140-184, Message.java L95-134, Group.java L290-359）
7. search_files(updateMessageRecord|IsPlaceMsg|PlaceMsg) → 3 命中
8. read_file(MessageRecordMongoService L70-109)
9. 输出最终答案：完整梳理 + IsPlaceMsg=1/2 与 MsgFlagBits=8 标记体系对比 + 异步延迟设计意图

评价：规范。一次 L1 检索带 world+project。但开头多了 tool_search + tool_describe 两步工具发现开销（工具未预加载，按需加载机制导致）。

---

## 对比总结
| 维度 | 会话1 (08:35) | 会话2 (08:38) | 会话3 (08:42) |
|------|--------------|--------------|--------------|
| 是否用 KG | ✓ | ✓（纠正后） | ✓ |
| L1 次数 | 1 | 2（首次漏参） | 1 |
| world+project | ✓ | 首次 ✗ 后纠正 | ✓ |
| 工具发现开销 | 1 (describe) | 0 | 2 (search+describe) |
| read_file 次数 | ~10 | ~10 | ~8 |
| 最终答案质量 | 优 | 优（+回调未接入发现） | 优 |
| 准则合规 | 合规 | 轻微违规(碎查) | 合规 |

## 可观察的优化点（供团队审计）
A. 会话 2 首次查询漏 world/project → 工具描述/简报是否足够强调"代码任务必带 world=code+project"？
B. 会话 3 tool_search+tool_describe 开销 → dt_search_kg 是否应预加载（deferred tool 机制）？
C. KG 返回的 calls 列表（如 groupMsgRecall 调 groupMsgRecallUpdate）是否被充分利用？agent 只用来确认位置，没用它直接构建调用链
D. 三会话都只用了 dt_search_kg 一层，未用 run_cypher_query（L2）查关系图；未用 CALLS 关系做图遍历（hop 扩展）
E. KG 的 llm_analysis 摘要质量：GroupMsgRecallRequest 构造器 llm_analysis="定义群消息撤回请求对象" 与真实用途（Builder 模式）有偏差；Builder/构造器/内部类方法占大量命中（0.28 分噪声）
F. 会话 2 中 AfterMsgWithdrawCallback 无消费方、TRTCCallBackController 无撤回分支——KG knowledge 世界是否有这个发现可复用？没有（本轮 learn 只补了发送链路+群组管理）
G. 用户测试重复 3 次同任务 → 知识图谱/会话记忆是否可让第 2/3 次直接复用第 1 次结论？
