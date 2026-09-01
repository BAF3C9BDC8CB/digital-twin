---
name: digital-twin-ops
description: Digital-twin usage decisions — when to search the KG, which world/project to use, memory rules, and fallback paths. Tool mechanics live in dt-mcp tool descriptions; this skill covers what the MCP descriptions cannot: judgment.
version: 6.0.0
platforms: [linux, macos, windows]
metadata:
  hermes:
    tags: [digital-twin, dt, search, memory, knowledge-graph, mcp]
---

# digital-twin-ops 使用决策

## 0. 与 dt-mcp 的分工

- **dt-mcp**(已启用)= 工具执行层: 参数、world 枚举、默认值都在各工具描述里, 直接看工具描述即可
- **本 skill** = 决策层: 只回答 MCP 描述不覆盖的「该不该查 / 怎么解读 / 预算与降级」

**选择原则**:
- 调用工具前先 `dt_sense` 确认项目与索引状态
- 搜索优先 `dt_search_kg`(world 语义见工具描述)
- dt-mcp 不可用时降级 CLI: `dt search` / `dt sense --json` / `dt memorize` / `dt health` / `dt build`(命令签名以 `dt --help` 为准)
- `dt_kg_sync` 已弃用 → 用 `dt_build --source knowledge`

## 1. 该不该查(搜索决策)

### 任务类型 → 搜索要求

| 类型 | L0 sense | L1 search_kg | L2 Cypher | 边界 |
|------|----------|--------------|-----------|------|
| A 配置/凭据/基础设施排查 | 必查 | **必查** | 按需(elementId 白名单) | L1≤2 L2≤2 |
| B 部署/运维操作 | 必查 | 建议 1 次(确认操作对象) | 按需 | L1≤1 L2≤1 |
| C 代码修复/开发 | 必查(防改错项目) | 条件必查(涉跨实体引用) | 否 | L1≤1 |
| D 信息询问(项目相关) | 必查 | **必查** | 按需 | L1≤2 |
| E 泛知识问答 / F 纯对话 / G 记忆写入 | 否 | 否 | 否 | 0 |
| H 跨项目/全局对比 | 必查 | 必查(--world) | 按需 | L1≤2 |

分类模糊时取更严格一侧。**豁免的是 L1 不是 L0**——除 E/F/G 外 L0 仍执行。

### 触发信号(命中必搜 L1)

- 服务/基础设施名、配置/凭据(token/host/端口/域名/环境名)、部署/生命周期(部署/发布/回滚/构建/jenkins)、历史决策(为什么/上次/历史/变更/原因)、注册表项目名(精确匹配 user_message, 非子串猜测)、状态健康词(状态/日志/告警/挂了)

query=2-4 个具体短语一次查完, 禁宽泛词("系统/服务/项目" → 噪音)。

### 豁免信号(可不搜)

- 纯对话/泛知识/记忆写入(E/F/G); 纯文件内代码逻辑(无跨实体引用); 信息已在当前上下文(刚查过/贴出/刚 build 过)
- **KG 无数据**——sense 未索引/未注册 → 跳过 L1 直接磁盘 + 候选注册建议
- L1 空结果 = 正常语义(KG 确实无数据), 停止不重试

### 禁止信号

- 每轮 pre_llm_call 裸查 sense/search(hook 只注入缓存摘要); 同 query 60s 内重复; degraded 反复重试(冷却 30s, 每任务开头最多重试 1 次); L1≥3/L2≥3 预算外追加(须显式说明理由); 敏感凭据自动注入/回显; 宽泛词查询; 未按 project 过滤的跨项目结果

### 优先级与跳层

顺序固定 L0→L1→L2, 无跳层默认。**跳过 L1 直达 L2 需同时满足**: ①已知确切命名 ②sense 已索引 ③需求是精确属性。

L2 用 **MATCH 属性查询**(`MATCH (c:Class {name:'GroupService'}) WHERE c.project='im-center'`), 勿用全文索引 `infra_search`/`db.index.fulltext.queryNodes`(Neo4j 语法, 本环境不存在)。elementId 精确查询显式 RETURN 白名单, 不 `RETURN n`。

sense 三态: indexed → L1 可行; 未注册/未索引 → 磁盘直查; degraded → 降级 + 上下文标 `⚠ KG degraded`。

## 2. 结果解读

- **hop=0 当事实, hop≥1 只当线索**(防图扩展漂移)
- **命中核验**: 看 hits 的 `project`/`source_ref` 字段确认归属, 未按 project 过滤时可能混入他项目结果
- **dt sense "永不失败"**: 后端连不上 → `degraded` + stats 全 0 + 退出码 0。**全 0 统计 ≠ 项目没代码, 是降级信号**
- **失败三分**: 连接失败(degraded 列表) vs 超时(MCP 报错) vs 空结果(KG 确实无数据)。对 sense/search 设 10s 心理超时, 超过视为降级

## 3. 预算与失败停止

- 每任务 L0≤1 / L1≤2 / L2≤2 / 总计≤5 次, 注入 ≤3.5K tokens
- 开头注入总预算 ≤3.5K; 每轮注入压缩为 1-2 行状态摘要(~50-100 tok), 不裸查
- 连续 2 次 L1 无有效命中 → 转 L2 或磁盘
- 超时 → 视为降级不重试; degraded → 回退磁盘 + 恢复后 dt_learn 补记(不静默); 空结果 = KG 无此数据
- **KG 故障绝不阻塞任务主流程**

## 4. 记忆规则

- 用户说"记忆/记一下/记住" → **立即 `dt_memorize`**(命令不是建议)
- details 必须带**文件路径/位置标识**便于后续定位: `文件: /path/to/file.yml 端口9216`
- `project` 参数仅作溯源, 不参与检索过滤
- 检索记忆: `dt_search_kg(world="memory", ...)`

## 5. 缓存

- sense `(project, cwd)` TTL 120s; search `(project, query)` 60s; 写操作(dt_memorize/dt_build)后失效; 跨会话不缓存
