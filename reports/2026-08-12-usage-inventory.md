# Skill / MCP / 插件 真实使用情况整理（2026-08-12）

数据源：state.db messages.tool_name（实际调用）、skills/.usage.json（skill 使用）、config.yaml（配置）

## 一、代码提交（已完成）

- 7 个 commit 已推送 origin/feat/v2-architecture（33a9a92 最新：导出分析报告 + 记忆来源检查 + dt 使用观测脚本）
- ⚠️ config/pipeline.yaml 工作区有 api_key 变更（sk-NUb→sk-H1N）**未提交**（敏感信息，保留本地生效）
- ⚠️ 历史 commit 中 pipeline.yaml 已含明文 api_key（sk-iey...）——已存在的风险，建议后续轮换 key 或清理历史

## 二、MCP servers（9 个配置，实际调用统计）

| Server | 状态 | 调用次数 | 用途 |
|--------|------|---------|------|
| chrome-devtools | ✅ 重度使用 | 2189 | 浏览器自动化（evaluate_script 1539、snapshot 150、click 128）|
| memgraph | ✅ 使用 | 331 | Cypher 查询（run_cypher_query）|
| digital-twin | ✅ 使用 | 215 | KG 检索/构建（dt_search 122、dt_search_kg 44、dt_sense 16）|
| ocr-mcp-server | ✅ 使用 | ~130 | CommandCode 注册/订阅/发货流程 |
| httptoolkit | ⚠️ 0 调用 | 0 | HTTP 抓包（配置了但未用）|
| idalib-mcp | ⚠️ 0 调用 | 0 | IDA 逆向（未用）|
| js-reverse | ⚠️ 0 调用 | 0 | JS 逆向（未用）|
| stitch | ⚠️ 0 调用 | 0 | Google 设计系统（未用）|
| qdrant | 🔴 已禁用 | - | enabled=False |

**建议**：httptoolkit/idalib/js-reverse/stitch 可先禁用（enabled: false），保留配置不删除（逆向工具按需启用）。

## 三、插件（2 个，都真实使用）

| 插件 | 状态 | 证据 |
|------|------|------|
| digital-twin（memory provider）| ✅ 使用中 | agent.log registered/activated；测试会话 api_content 含 [KG 记忆] 注入（13:00/14:40 两铁证）|
| dt-sense | ✅ 使用中 | dt_sense 16 次调用 + [DT-SENSE] 注入每会话 |

## 四、Skills（65 个，51 使用 / 14 从未使用）

### 高频使用（近期活跃）
digital-twin-ops（今天）、spec-implementation-audit、hermes-agent、business-logic-codebase-analysis、repository-architecture-analysis、uvp-offen-pay、vue3-mobile-responsive-ui、dogfood、opencode-register-backend/frontend/account-ops、commandcode-ops、jeepay-rust-rewrite、team-orchestrator、kanban-* 系列、goofish-store-ops、xinference-ops、feishu-messaging、opencode-go-provider、clash-verge-proxy-ops、any-auto-register-ops、openvpn3-ops

### 从未使用（14 个，建议归档/删除）
| Skill | 冗余关系 |
|-------|---------|
| apifox-doc-extraction / apifox-docs-extraction / apifox-docs-scraping | 3 个 apifox 系列互相冗余，全未用 |
| browser-selector-workflow-review | 未用 |
| commandcode-account-ops | 未用（commandcode-ops 在用）|
| feishu-bot-development / feishu-bot-events | 未用（feishu-messaging 在用）|
| hermes-mcp-configuration / hermes-mcp-server-setup / mcp-server-migration | 3 个 MCP 配置系列互相冗余，全未用（hermes-mcp-config 在用）|
| linux-system-crash-diagnostics | 未用 |
| local-software-inventory | 未用（system-software-inventory 在用）|
| sillytavern-ops | 未用（sillytavern 在用）|
| static-functional-testing | 未用 |

**建议**：未使用的 14 个中——apifox 三兄弟留 1 删 2、MCP 三兄弟留 1 删 2、其余可直接删除或归档；或用 `hermes curator` 清理。

## 五、核心使用链路（当前真实运转的）

```
每轮:  [DT-SENSE] 项目感知（dt-sense 插件）
      + builtin 记忆（MEMORY.md 8 条）
      + [KG 记忆] prefetch（digital-twin provider → dt search --world knowledge）
任务中: dt_search_kg / dt_search（KG 检索）
      + memgraph run_cypher_query（精确查询）
      + chrome-devtools（前端调试）
      + ocr-mcp-server（CommandCode 运营）
      + kanban（团队协作）
```
