# 统一检索设计（Unified Search）

**日期**：2026-08-03
**状态**：已定稿（设计评审通过）
**前置**：S5 知识混合检索（`2026-08-01-knowledge-search-design.md` §10.6 后续任务卡）、S5 实施记录
**触发**：`dt search-kg "createApp"` 搜不到已索引的 JS 函数（code_methods 被排除在检索范围外）；搜索结果缺少 LLM 分析/文件路径/行号，AI 与人工无法以最小上下文定位内容

---

## 1. 问题陈述

当前检索面存在三套互不共用的栈：

| 入口 | 栈 | 残缺 |
|------|-----|------|
| CLI `dt search` / `dt search-kg` | legacy `application/search.rs`（纯向量+RRF+CONTAINS） | search-kg 不查 code_methods；输出缺 llm_analysis/行号 |
| gRPC `Search` RPC | S5 新栈 `search_mcp.rs` | world 硬编码 `code`，knowledge/doc 不可达；proto 无 world 字段 |
| MCP `dt_search` 等 | subprocess 调 CLI | `dt_search`/`dt_search_expand` 传 `--query` flag 与 CLI 位置参数不兼容，**当前实际报错不可用**；返回纯文本非结构化 |

后果：S5 GraphRAG 管线在产物代码中不可达；代码符号对部分入口不可见；AI 消费者拿到的是需要二次解析的纯文本。

## 2. 目标与非目标

**目标**：
- 一个检索服务（`CrossWorldSearch`）、一个契约（`SearchHit`/`CrossWorldResult`），所有入口委托
- 任何 hit 至少带出「分析或原文」+「定位」之一（**最小上下文可读**的契约底线）
- `dt search` 单命令覆盖全部世界；MCP 返回结构化 JSON；gRPC proto 透传 world

**非目标**：
- 不改动索引/写入侧（code_methods、kg_nodes、doc_chunks、config_chunks 的生产管线不变）
- 不为 code 世界引入图扩展（保持纯向量；Method 图关系扩展另行评估）
- 不删除 legacy Qdrant 集合数据本身（仅删除对 `{project}_methods` 的扫描逻辑）

## 3. 目标架构

```
┌─ CLI: dt search [--world W] [--json]   （人类格式 / JSON）
├─ MCP: dt_search / dt_search_kg         （subprocess 调 CLI --json，透传 JSON）
└─ gRPC: Search RPC                      （proto 透传 world + 全量字段）
        │  全部委托
        ▼
┌──────────────────────────────────────────────┐
│ application/context/search_mcp.rs             │
│ CrossWorldSearch —— 唯一检索服务              │
│   code      → search_code    （向量 code_methods）      │
│   knowledge → retrieve.rs    （S5 GraphRAG 全管线）     │
│   doc       → search_doc     （doc_chunks + source=doc）│
│   config    → search_config  （config_chunks + 关键词，迁入）│
│   memory    → search_memory  （事件标签 Cypher，迁入）   │
│   all       → code+knowledge+doc 并行 + RRF            │
└──────────────────────────────────────────────┘
```

## 4. 决策记录

| # | 决策 | 结论 | 理由 |
|---|------|------|------|
| U-D1 | location 字段 | **渲染层组装**，契约保持原子字段（file_path/start_line/doc_id/element_id） | CLI 与 MCP 格式需求不同 |
| U-D2 | `all` 世界构成 | **code + knowledge + doc** | config/memory 噪声大，显式指定才查 |
| U-D3 | `dt search` 默认 world | **`all`** | "搜一次找到任何东西"的心智；RRF 处理跨世界分数不可比 |
| U-D4 | MCP 输出 | **JSON**（CLI 新增 `--json` flag，MCP 透传） | AI 最小上下文消费；dt_search 当前本就报错，无兼容负担 |
| U-D5 | `dt search-kg` | **完全移除**（无 stub、无引导、不考虑旧版兼容）：main.rs 删除 SearchKg 变体与 dispatch 分支，clap 原生报 `unrecognized subcommand` | 用户明确选择（二次确认：不要兼容层）；MCP `dt_search_kg` 改调 `dt search --world knowledge --json` |
| U-D6 | legacy `{project}_methods` 扫描 | **移除**（search_code 只查 `code_methods`） | 多年无写入方；现存 32 点全在 code_methods |
| U-D7 | `dt_search_expand` MCP 工具 | **删除**，能力并入 `dt_search --world all` | 与 dt_search 重复 |

## 5. 统一契约

`SearchHit` 现有字段不动（id/title/snippet/source_world/entity_type/score/source_ref/file_path/start_line/end_line/signature/calls/element_id/score_breakdown/hop/via_same_as/relations/evidence/rerank_degraded），**新增 1 个**：

| 字段 | 类型 | 来源 | 说明 |
|------|------|------|------|
| `llm_analysis` | `Option<String>` | code_methods payload 直取 | 方法级 LLM 分析（"用途：…\n逻辑：…"） |

各类型 hit 的「分析/定位」保障矩阵：

| entity_type | 分析字段 | 定位字段 |
|---|---|---|
| Method | `llm_analysis`（索引期生成；缺失时回退 `signature`） | `file_path` + `start_line`-`end_line` |
| Entity/Concept/Config… | `summary`（kg_nodes，S5 抽取期生成） | `source_ref`/`doc_id`（来源文档） |
| Doc chunk | `snippet` = chunk 原文 | `doc_id` → 路径 + 块序号 |
| Memory 事件 | `snippet` = 事件摘要 | 时间戳 + 关联实体 id |

## 6. 各世界管线改动

### 6.1 code（search_code）
- 增取 payload `llm_analysis` 填入契约
- 移除 `list_collections()` + `_methods` 后缀扫描，直接查 `CODE_METHODS` 常量（U-D6）
- 保留：`DT_SEARCH_MIN_SCORE`（默认 0.3）阈值、project 过滤

### 6.2 knowledge（retrieve.rs）
- 无改动。**首次在产物代码中可达**（CLI/MCP/gRPC 全入口）

### 6.3 doc（search_doc）
- 无改动（source="doc" 硬过滤 + doc_id 参数已就绪）

### 6.4 config（search_config，新函数）
从 `cli/build.rs:819-1100` 原样迁入：
- 多查询变体 embed，使用 QueryRewriter 做中英桥接——`rewrite` 模块从 search.rs **迁入 search_mcp.rs 作为私有内联模块**，仅 search_config 使用（knowledge 世界走 retrieve.rs，不依赖 rewrite）
- 向量查 `config_chunks` + `doc_chunks`（**注意**：doc_chunks 中 nacos 点 source≠doc，config 世界不按 source 过滤）
- ASCII 关键词过滤 + RRF 融合
- Cypher 回退（ConfigKey/Server/Database/NacosConfig/NacosService CONTAINS）
- `config_chunks` 硬编码字符串提升为 `shared/collections.rs` 常量 `CONFIG_CHUNKS`

### 6.5 memory（search_memory，新函数）
从 `cli/build.rs:1509-1515` 迁入：
- Cypher CONTAINS 查 `Modification/Deployment/ConfigChange/BugFix/Decision/Conversation/Session`
- title=事件摘要，snippet=详情，定位=时间戳+关联实体

### 6.6 all（跨世界融合）
- **顺序**调 code+knowledge+doc 三世界（先顺序保证简单可测，世界失败单列 degraded 不熔断；并行优化列入后续项）
- RRF 融合（k=60，逻辑自 search.rs `reciprocal_rank_fusion` 迁入）
- `per_world_counts` 已有字段填充

## 7. 输出格式

### 7.1 CLI 人类格式（类型感知三行制）

```
[0.9412] [Method] createApp
  分析: 创建服务器实例并注册健康检查接口；实例化服务器对象并绑定路由回调
  位置: test/project/app.js:32-36  signature: function createApp(port)

[0.9108] [Entity] ifCode
  摘要: 支付渠道编码，决定路由到哪个支付平台
  来源: dt://doc/支付架构决策.md  [hop=0]

[0.8803] [Doc] 支付架构决策.md#chunk-3
  原文: [决策] 采用 ifCode + wayCode 两级拆分：ifCode 决定路由…
  位置: docs/decisions/支付架构决策.md
```

- 分析/摘要/原文截 200 字符（现行 100 太短，llm_analysis 两段式约 60-120 字符可全显）
- degraded 非空时末尾打一行降级标记

### 7.2 JSON（`--json`，MCP 消费）

直接序列化 `CrossWorldResult`（serde 派生已具备或补 derive）：

```json
{"query":"createApp","world":"all","total":1,"degraded":[],
 "per_world_counts":{"code":1},
 "hits":[{"entity_type":"Method","title":"createApp","score":0.9412,
   "llm_analysis":"用途：创建服务器实例…\n逻辑：实例化服务器对象…",
   "file_path":"test/project/app.js","start_line":32,"end_line":36,
   "signature":"function createApp(port)"}]}
```

### 7.3 gRPC proto（dt_core.proto）

`SearchRequest` 增：`world, max_hops, with_evidence, origin, doc_id`
`SearchResult` 增：`entity_type, snippet, llm_analysis, end_line, score_breakdown(消息), hop, relations(消息列表), evidence, rerank_degraded`
——即 S5-D10 任务卡全量字段。

## 8. 入口改造

| 入口 | 改动 |
|------|------|
| `dt search` | 默认 world `code`→`all`；删除全部内联检索逻辑（build.rs:767-1582），变纯渲染壳（人类格式/JSON 二选一）；**`--json` 时 stdout 仅含 JSON**（"Search: query=..."等 header 行一律抑制，日志走 stderr 不受影响） |
| `dt search-kg` | **完全移除子命令**（U-D5）：Commands 变体与 dispatch 分支整体删除，clap 原生报 `unrecognized subcommand` |
| MCP `dt_search` | subprocess 改 `[DT_BIN, "search", query, "--world", w, "--json"]`（位置参数，修 `--query` bug），stdout 即 JSON 透传 |
| MCP `dt_search_kg` | 改调 `dt search --world knowledge --json` |
| MCP `dt_search_expand` | 删除（U-D7） |
| gRPC `handle_search` | 透传 proto 新字段到 SearchRequest；world 不再硬编码 |

## 9. 退役清单

- `src/application/search.rs` 整文件（fusion→all 世界、expansion→删除、rewrite→search_config 私有模块）
- `cli/build.rs` 中 handle_search/handle_search_kg 的 ~900 行内联检索逻辑
- gRPC `search_via_vector`/`search_via_graph`（已 deprecated，随 proto 变更一并删）
- `infrastructure/qdrant/collection.rs` 的 `CollectionKind`/`collection_name` 命名体系（vestigial，无写入方无检索方）
- legacy `{project}_methods` 集合扫描（U-D6）

## 10. 测试策略

- **单测**：search_config/search_memory 迁移逻辑、RRF 融合、CLI 渲染（类型感知三行制）、JSON 序列化、proto 映射
- **live 测试**（新文件 `tests/unified_search.rs`，与 s5_knowledge_search.rs 并列）：
  - `dt search --world all "createApp"` → Method 命中且含 llm_analysis + 路径行号
  - `dt search --world knowledge "新增渠道的唯一代码标识"` → ifCode 语义命中（S5 链路产物可达验证）
  - `dt search --world config` / `--world memory` 基本可用
  - MCP JSON 输出 schema 校验
- **基线**：773 passed / 2 预存失败（ts_java、backup_sqlite）不扩大；`cargo clippy --all-targets` 0 error

## 11. 验收标准

1. `dt search "createApp"`（默认 all）→ Method 命中，显示分析+`app.js:32-36`
2. `dt search "云仓 支付" --world knowledge` → Entity 命中，显示摘要+来源
3. `dt search-kg` → clap 报 `unrecognized subcommand`（命令已不存在）
4. MCP `dt_search` 返回合法 JSON，含 llm_analysis/file_path/start_line/end_line
5. gRPC Search RPC 传 `world="knowledge"` 返回 ifCode 语义命中（S5 链路端到端）
6. 全量 cargo test 与 clippy 达标（§10 基线）
