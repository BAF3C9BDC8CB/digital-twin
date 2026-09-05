# Hermes 文档 LLM 知识提取质量对比报告

生成时间: 2026-09-05
提取模型: deepseek-ai/DeepSeek-R1-0528-Qwen3-8B

覆盖文档: 39 篇（hermes-docs-zh 全部）

## 总量对比

| 维度 | HanLP | LLM (R1-8B) |
|------|-------|-------------|
| 实体总数 | 1097 | 936 |
| 关系总数 | 79 | 612 |
| 平均实体/篇 | 28.1 | 24.0 |
| 平均关系/篇 | 2.0 | 15.7 |

## 分文件对比

| 文档 | HanLP 实体 | HanLP 关系 | LLM 实体 | LLM 关系 |
|------|-----------|-----------|---------|---------|
| README.md | 7 | 0 | 10 | 6 |
| developer-guide/adding-tools.md | 11 | 0 | 14 | 9 |
| developer-guide/architecture.md | 15 | 0 | 28 | 14 |
| developer-guide/contributing.md | 20 | 4 | 15 | 8 |
| developer-guide/creating-skills.md | 16 | 2 | 21 | 10 |
| developer-guide/plugins.md | 47 | 5 | 57 | 40 |
| getting-started/installation.md | 28 | 6 | 15 | 8 |
| getting-started/learning-path.md | 13 | 1 | 10 | 9 |
| getting-started/nix-setup.md | 49 | 4 | 45 | 35 |
| getting-started/platform-support.md | 6 | 0 | 13 | 7 |
| getting-started/quickstart.md | 26 | 0 | 15 | 8 |
| getting-started/termux.md | 14 | 0 | 10 | 6 |
| getting-started/updating.md | 16 | 0 | 13 | 6 |
| guides/daily-briefing-bot.md | 17 | 1 | 10 | 8 |
| guides/python-library.md | 11 | 1 | 13 | 8 |
| guides/team-telegram-assistant.md | 28 | 1 | 15 | 8 |
| guides/tips.md | 15 | 0 | 14 | 9 |
| guides/use-voice-mode-with-hermes.md | 16 | 0 | 10 | 6 |
| user-guide/cli.md | 29 | 3 | 22 | 16 |
| user-guide/configuration.md | 124 | 10 | 97 | 66 |
| user-guide/features/batch-processing.md | 6 | 1 | 10 | 6 |
| user-guide/features/browser.md | 44 | 1 | 29 | 16 |
| user-guide/features/code-execution.md | 19 | 3 | 15 | 7 |
| user-guide/features/context-files.md | 12 | 2 | 13 | 7 |
| user-guide/features/cron.md | 62 | 3 | 30 | 22 |
| user-guide/features/delegation.md | 18 | 1 | 21 | 12 |
| user-guide/features/hooks.md | 55 | 2 | 55 | 37 |
| user-guide/features/mcp.md | 15 | 1 | 23 | 13 |
| user-guide/features/memory.md | 23 | 2 | 8 | 5 |
| user-guide/features/plugins.md | 22 | 0 | 26 | 15 |
| user-guide/features/provider-routing.md | 1 | 0 | 10 | 8 |
| user-guide/features/skills.md | 36 | 8 | 30 | 19 |
| user-guide/features/tools.md | 8 | 0 | 14 | 6 |
| user-guide/features/voice-mode.md | 39 | 3 | 23 | 13 |
| user-guide/messaging.md | 27 | 1 | 27 | 18 |
| user-guide/messaging/discord.md | 45 | 3 | 49 | 40 |
| user-guide/messaging/telegram.md | 61 | 6 | 45 | 36 |
| user-guide/security.md | 48 | 2 | 34 | 19 |
| user-guide/sessions.md | 48 | 2 | 27 | 26 |
---

## 类型质量验证（核心维度）

### HanLP 实体类型分布（39 篇累计 1097 个）

| 类型 | 数量 | 评价 |
|------|------|------|
| PERSON | 394 | ❌ 技术名词被误判为人名（Gateway/Cron/Telegram/SQLite 等） |
| PRODUCT | 171 | ⚠️ 泛产品类，非领域类型 |
| CARDINAL | 126 | ⚠️ 纯数字，无业务价值 |
| ORG | 119 | ⚠️ 泛机构类 |
| TIME / ORDINAL / DATE 等 | 287 | ⚠️ 非技术领域类型 |

**HanLP 有效领域实体占比极低**：PERSON 一项即占 36%，且大量为技术名词误判。

### LLM 实体类型分布（39 篇累计 936 个，全部合规）

| 类型 | 数量 |
|------|------|
| Concept | 230 |
| Tool | 194 |
| Service | 140 |
| File | 117 |
| Technology | 110 |
| Platform | 91 |
| Module | 54 |

**LLM 实体类型越界：0**（100% 落在 Service/Module/Tool/File/Technology/Concept/Platform 白名单内）

### 实体结构完整性（LLM）

- 缺 summary 的实体：0（936/936 全带描述）
- aliases 非数组：0
- 缺关键字段 / 非 dict 混入：0
- 抽样实体示例（user-guide/features/mcp.md）：
  ```json
  {"name": "MCP", "type": "Concept", "aliases": ["Model Context Protocol"], "summary": "模型上下文协议，用于标准化工具调用接口"}
  ```

### 关系对比

| 维度 | HanLP | LLM |
|------|-------|-----|
| 总数 | 79 | 612 |
| 类型 | 句法关系（conj/nn/dep/nummod 占 79%）| 业务关系（USES 269/CONTAINS 123/DEPENDS_ON 77/MANAGES 71）|
| 业务价值 | 几乎为零 | 100%（仅 4 条越界：MANAGED_BY×2/REQUIRES×1/CONFIGURES×1）|

---

## 结论

**LLM（DeepSeek-R1-0528-Qwen3-8B）在技术文档知识提取上全面优于 HanLP：**

1. **类型准确率**：HanLP 36% 实体被误判为 PERSON（技术名词当人名），LLM 936 实体类型 100% 合规
2. **关系质量**：HanLP 79 条全是句法关系（语法层），LLM 612 条全是业务关系（语义层），数量 7.7 倍
3. **描述完整性**：LLM 实体 100% 带 summary 描述（HanLP 无描述概念）
4. **覆盖**：39/39 篇全部成功，0 失败

**代价**：LLM 需调用远程 API（39 篇约 50 分钟、约 15 万 tokens），HanLP 本地 <90s 免费。**取舍：要质量用 LLM，要速度/成本用 HanLP。**
