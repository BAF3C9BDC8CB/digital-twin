# Hermes 接入知识图谱 — 完整方案（汇总）

> 本文件整合三方评审 + 用户反馈修正，是最终实施方案的唯一来源。
> 2026-08-11

## 0. 问题根因

用户要求 Hermes agent 每个任务开始前先查询知识图谱（KG），禁止第一时间用 terminal 命令查信息。此前靠 home 级 AGENTS.md 软约束，失败原因（源码核实）：

1. **AGENTS.md 只在 cwd 生效**：home 级文件不跨项目注入（Hermes 官方文档明确），agent 在 /data/aflmProjects 等目录下工作时规则根本没加载。
2. **软约束无强制力**：即使加载，模型仍可能跳过 KG 直接 grep/find/curl。
3. **无预算/节流/降级语义**：即使查了 KG，也没有注入预算、缓存、失败降级的设计。

## 1. 目标

在**不改 Hermes 源码、不改 dt 源码**的前提下：
- 每个任务开始前，KG 简报自动进入模型上下文（硬强制）
- LLM 按需调用 dt_sense/dt_search_kg（执行层）
- 搜索时机有明确决策规则（什么时候必须搜/可以不搜/禁止搜）
- KG 故障时 fail-open，绝不阻塞主流程

## 2. 架构（三层）

```
用户消息
  ↓
[1] pre_llm_call hook（Python plugin，每会话一次）
  ├── 读 payload: user_message + cwd + session_id + is_first_turn
  ├── dt sense --json 感知当前 cwd 项目（17-36ms, ≤1.6KB）
  ├── 压缩为简报 + KG 健康状态 + 搜索触发规则
  └── 注入 context 到用户消息（≤1.5KB）
  ↓
[2] LLM 语义判断（目标定位）
  ├── 理解用户意图 → 决定目标项目/目录
  ├── 需要时: dt_sense(path=<目录>) MCP 工具显式传参
  ├── 需要时: dt_search_kg(query, limit=5) 语义检索
  └── 不确定时: 询问用户（禁止 find/grep 搜目录）
  ↓
[3] MCP 执行层（24 个 digital-twin 工具）
  ├── dt_search_kg / dt_search / dt_sense / run_cypher_query
  └── 查询策略按 AGENTS.md 场景 A/B/C
```

## 3. 目标目录定位（用户反馈修正）

**谁决定目标目录**：LLM 语义判断，不是 hook、不是 find。

| 层 | 职责 | 机制 |
|---|---|---|
| hook 预注入 | 不猜目录 | 只注入当前 cwd 简报 + KG 状态 + 注册表提示 |
| LLM 判断 | 理解意图定目标 | 用户说"看 warehouse" → 目标是 warehouse-center |
| MCP 执行 | 显式传参 | dt_sense(path=...) / dt_search_kg(query) |

**禁止**：用 find/grep/curl 搜目录（违背用户原则、不可靠、65 项目全盘搜太慢）。

## 4. 性能实测（本机）

| 操作 | 耗时 | 体积 | token 估算 |
|---|---|---|---|
| dt sense --json（未索引） | 17-36ms | 350B | ~100-150 |
| dt sense --json（已索引 63 方法） | ~30ms | 1.65KB | ~500-700 |
| dt search_kg limit=5 | 1.4s | 7.2KB | ~2.2-2.5K |
| dt search limit=10 | — | ~14KB | ~4-5K（太重） |

**结论**：sense 极轻量，search_kg 按需用，search limit=10 禁止用于注入。

## 5. 搜索时机决策（成本/收益模型，完整版见 `2026-08-11-kg-search-cost-benefit-model.md`）

**决策偏置：T1-T4 宁可假阳性（搜了无用 ≈5K effective），不可假阴性（漏搜出错 ≈20K+ 且违背原则）；T5-T7 反之。** 假阴性/假阳性代价不对称 2-5×（纯 token），计入用户介入与决策污染后 10-20×。

| 任务类型 | 默认动作 | 降级条件（命中率/新鲜度） |
|---|---|---|
| T1 事实查询（服务/配置/凭据/路径） | 搜（limit=5, hop0=事实） | H3→浅搜+磁盘验证；F3→结果标 ⚠stale 须源验证 |
| T2 变更执行（改配置/部署/重启） | 搜+（关键属性 L2 白名单确认） | 关键值一律源验证，禁盲改 |
| T3 历史决策回顾 | 搜 | H3→明示"KG 无决策记录" |
| T4 排障诊断 | 搜 | degraded→先 dt_health |
| T5 纯代码开发 | 浅搜（简报已覆盖） | 出现明确服务/配置引用→升级搜 |
| T6 项目探索/新任务 | 浅搜（简报即交付物） | — |
| T7 机械/文书/闲聊 | 不搜 | 明确引用→浅搜 |

预算：单任务 raw ≤3K（L0≤800 / L1≤2K 裁剪 top-3 / L2≤700 白名单）；注入永远是裁剪摘要，禁止原始输出与 limit=10；超限阶梯：裁剪→降 L2→第 3 次禁自动改磁盘直查。缓存：sense TTL 300s / search 300s，写操作硬失效优先。

## 6. 缓存/节流

| 项 | 设计 |
|---|---|
| 缓存键 | sense: (project, cwd)；search: (project, query) |
| TTL | sense 120s；search 60s |
| 节流 | 同 session 同键 1 次/分钟；失败冷却 30s |
| 失效 | dt_memorize/dt_build/jcli_build 后失效相关键 |
| 每会话 | 新会话开头必须 fresh sense（不跨会话缓存） |

## 7. 降级

- KG 宕机 → dt sense 返回 degraded 列表 + exit 0，注入 `⚠ KG degraded: [...]`
- hook 失败 → 仅 warning 不 crash agent（fail-open）
- 查询失败 → 降级阶梯：Cypher → CLI 直连 → 读磁盘 → 跳过
- 防雪崩：失败冷却 30s 不重试；每任务最多重试 1 次

## 8. 待实施清单

- [ ] 创建 `~/.hermes/plugins/dt-sense/`（plugin.yaml + __init__.py）
- [ ] 更新 `~/.hermes/SOUL.md`（静态行为准则 + 搜索时机清单）
- [ ] `hermes config set hooks_auto_accept true`（gateway 需要）
- [ ] 重启 gateway（systemctl --user restart hermes-gateway）
- [ ] 验证：hermes hooks list / 新会话复述简报 / 日志触发次数
- [ ] 回退：rm -rf ~/.hermes/plugins/dt-sense/

## 9. 参考

- 查询策略设计文档: `docs/superpowers/specs/2026-08-11-kg-auto-query-strategy.md`
- 评审结论: deleg_170f42fa（3 角色：架构/查询策略/运维风险）
- Hermes hooks 文档: ~/.hermes/hermes-agent/website/docs/user-guide/features/hooks.md
