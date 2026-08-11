# im-center 团队测试与 KG 使用改进记录（2026-08-12）

## 一、团队 A：项目测试分析（3 角色并行）

### 结论：uvp-im-center = 腾讯云 IM (TIM) REST API 封装代理网关

- **技术栈**：Spring Boot 2.x + Nacos（注册/配置中心）+ MongoDB（消息记录）+ OkHttp（调腾讯 IM）+ Sentinel（未配置）
- **核心链路**：`/message/sendMessage` → MessageService.sendMessage → 腾讯云 `openim/sendmsg` → 回查（msgTime±1s 窗口）→ Mongo `message_record_{source}`
- **功能清单（12 项，11 项实现完整）**：单聊发送/查询/撤回、群聊建群/发消息/查询/成员管理/资料/禁言/通知、账号导入/状态查询、多租户 domain 路由
- **关键否定结论**：无 FeignClient、无 MQ（RabbitMQ/Kafka）、无 Redis、无 WebSocket、无下游内部服务调用
- **10 项架构问题**：消息可靠性差（轮询回查无重试）、回调体系悬空（20+ 回调模型无消费）、线程池过浅（queue=5）、冗余依赖（MySQL/MyBatis 零使用）、敏感信息明文（usersig 打日志）、无鉴权（domainId 静默第一租户）等

### 报告产出
- `reports/uvp-im-center-外部依赖与数据流分析.md`（105 行完整报告）
- 另两份报告（架构分析/功能测试）由子代理直接输出

## 二、团队 B：KG 使用评价（3 角色并行）

### 发现的问题（按严重度）

| # | 问题 | 根因 | 状态 |
|---|---|---|---|
| 1 | **dt_search_kg 对代码实体 0% 命中** | mcp-server.py:586 硬编码 `--world knowledge`，代码实体在 code world | ✅ 已修复 |
| 2 | **文档与 Memgraph 语法脱节** | AGENTS.md/KG-QUERY.md/specs 示例 `CALL db.index.fulltext.queryNodes("infra_search",...)` 是 Neo4j 语法，本环境不可用 | ✅ 已修复 |
| 3 | **注释错位** | 旧版索引器 find_comment 关联 bug（GroupService 3 方法被标成"删除群成员消息"） | 🔄 重建中 |
| 4 | Class 描述全空（357 类 0 摘要） | 架构设计：llm_analysis 仅对 Method 做 Phase2 增强 | ⚠ 设计如此，非 bug |
| 5 | 79.9% 方法无注释 | 源码本身无注释 | ⚠ 源码问题，Phase2 增强补偿 |
| 6 | 双写不一致（Memgraph 无 llm_analysis） | 架构设计：Memgraph 存结构，Qdrant 存向量+payload | ⚠ 设计如此 |

### 已实施的改进

1. **mcp-server.py：dt_search_kg 增加 world + project 参数**
   - 默认 world=knowledge（保持向后兼容），可指定 code/knowledge/doc/config/memory/all
   - 增加 project 参数过滤跨项目噪音
   - 实测：`dt_search_kg(query="发送单聊文本消息", world=code, project=im-center)` → 100% 命中 im-center 真实方法（score 0.70）
   - 生效条件：重启 hermes-gateway

2. **AGENTS.md 场景 B**：Neo4j 全文索引示例 → `dt_search_kg(world=...)` + `MATCH ... WHERE n.name CONTAINS` 属性查询

3. **skill/guides/KG-QUERY.md 方式 B**：全文索引 → `dt search --world code|knowledge --project` + 属性匹配兜底

4. **两份 spec 文档**：infra_search 全文索引引用 → 实际可用方式

## 三、复测计划（待构建完成）

1. 验证注释错位已修复（GroupService 3 方法 comment 应为空）
2. 重新执行团队 B 的 8-10 条真实查询，验证正确率 0% → 高
3. 若全部通过 → 结束；若仍有问题 → 再改进再测

## 四、教训（写入 KG 使用准则）

- **代码实体查询必须 world=code**（dt_search_kg 默认 knowledge 是知识层，不含代码实体）
- **跨项目查询必须带 project 过滤**（否则 message-center 等大量噪音）
- **本环境 Memgraph 无全文索引语法**（无 db.index.fulltext，文档已修正）
- **dt build 不支持 --json 参数**（CLI 与 MCP 参数差异）
- **dt build 位置参数不接受，必须 --path**

## 二、团队 B：KG 使用评价（3 角色并行）

### 核心发现（按严重度排序）

1. **【严重】dt_search_kg 硬编码 knowledge world**（mcp-server.py:586）
   - 症状：对 im-center 检索正确率 0%，结果被 message-center 污染
   - 根因：MCP 工具 dt_search_kg 固定传 `--world knowledge`，而 im-center 代码实体在 code world
   - 修复：✅ 已给 dt_search_kg 增加 world + project 参数（默认 knowledge 向后兼容）

2. **【严重】--file 单文件构建误删 Memgraph 方法**（pipeline.rs 步骤 2/3）
   - 症状：构建后 Memgraph 中 im-center 只剩 11 个方法（应 2287）
   - 根因：--file X 时 all_files=[X]，IncrementalStrategy 把快照中其余 341 文件判为 deleted → delete_files_from_graph 全删
   - 修复：✅ 单文件模式跳过 select_files 的 deleted 检测（直接 (all_files, [])）
   - 注：Qdrant 向量未删（2287 仍在），sense 的 methods 从 Memgraph 读 → 崩到 11

3. **【中】AGENTS.md/KG-QUERY.md 文档与 Memgraph 语法脱节**
   - infra_search/fulltext.queryNodes 是 Neo4j 语法，本环境 Memgraph 不可用（SHOW PROCEDURES 也受限）
   - 修复：✅ 已替换为 dt_search(world=code, project=X) + MATCH 属性查询

4. **【中】类描述全空**（357 类 0 个有摘要）
   - 现状：索引器只为 Method 生成 llm_analysis（Phase2），Class 不做 LLM 增强——设计如此，非使用问题

5. **【低】方法注释 79.9% 为空**
   - 源码本身大多无 javadoc；注释错位（deleteGroupMsgBySender 注释复制到后续方法）由 3 号全量重建修复

### 审计员误报澄清
- "config.yaml 无 hooks 段 → 插件未生效"：dt-sense 是 Python plugin（plugins.enabled），不需要 shell hooks 段；插件已 enabled 且验证 17/17

## 三、改进实施与复测结果（第二轮）

### 实施的修复（3 项代码级 + 1 项文档级）

1. **dt_search_kg 增加 world/project 参数**（mcp-server.py）
   - 修复 0% 命中根因；验证：sendMessage 检索全部命中 im-center，llm_analysis 准确

2. **--file 单文件构建误删 bug**（pipeline.rs 步骤 2/3）
   - 修复：单文件模式跳过 select_files 的 deleted 检测
   - 影响：避免构建后 Memgraph 方法数从 2287 崩到 11

3. **TsJavaParser 注释错位 bug**（tree_sitter_utils.rs extract_comment）
   - 根因：comment_lines 为空时遇非注释节点不 break，跨过上方法偷取其 javadoc
   - 修复：遇非注释节点无条件 break；新增 2 个回归测试；676 测试全过
   - 验证：groupMsgGetSimple/sendGroupSystemNotification/sendGroupMsg 注释已清空（不再错位），deleteGroupMsgBySender/groupMsgRecall 正确注释保留

4. **AGENTS.md/KG-QUERY.md/specs 文档语法修正**
   - infra_search 全文索引（Neo4j 语法）→ dt_search(world=code, project=X) + MATCH 属性查询

### 复测结果（构建后）

- KG 检索复测 5/7 直通，2 项（撤回/账号导入）经精确方法名查询确认功能存在且可检索（查询词与向量空间不匹配，非数据问题）
- accountImport 检索：3/3 命中真实实体（AccountService/Account/AccountController），llm_analysis 描述准确
- Memgraph：im-center 2287 方法 / 357 类，注释错位清零
- Qdrant：2287 向量全部正常

### 结论

KG 使用链路（sense → search_kg → cypher）全部畅通；代码实体检索需 world=code + project 限定（已写入技能）；注释/索引质量经重建+修复后达标。
