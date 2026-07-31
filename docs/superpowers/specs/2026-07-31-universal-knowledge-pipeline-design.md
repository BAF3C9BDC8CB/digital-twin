# 通用知识管线架构设计：抽取 → 整合 → 检索

- 日期：2026-07-31
- 状态：已评审（待实现）
- 范围：`dt build` 文档知识提取全链路重构 + 向量库设计 + 知识搜索重写（搜索实现延后）
- 关联文档：`docs/architecture-v2-six-worlds.md`、`docs/architecture-v2-data-pipeline.md`

---

## 1. 背景与目标

现有 `dt build` 的知识提取只能"吃"手工标注的结构化数据（`@knowledge` 注释），文档只做了
chunk + embed，没有实体/关系抽取；知识搜索走纯字符串 `CONTAINS` 匹配，向量能力完全没用
在知识世界上。

目标：**任意文档 → 无监督抽取实体与关系 → 图存储 + 全量向量化 → 向量召回 + 图扩展的混
合检索**。其中构建（抽取+整合）优先实现，检索层架构定稿但实现延后。

核心原则：

1. 知识唯一来源是 LLM 抽取，手工注解流程整段删除。
2. 向量库不是独立知识库，是知识图谱（KG）的**语义索引**；知识真相只在 Memgraph。
3. HanLP 只做 LLM 的候选锚点，不独立入库。
4. 现有机制凡是有更优做法的，直接改进，不受现状束缚（改进点见 §9）。

---

## 2. 现状诊断

读完 `build/pipeline.rs`、`pipeline/processors/*`、`knowledge/*`、`context/search_mcp.rs`
后，问题集中在三处：

### 2.1 知识来源被绑死在手工标注上

- `extract_knowledge_annotations()`（`build/pipeline.rs:611`、`infrastructure/parser`）
  靠正则扫代码注释里手写的 `@knowledge domain="..." concept="..."`，没标注就没知识。
- `process_documents()`（`build/pipeline.rs:1216`）对文档只做 chunk + embed，整块文本丢
  进向量库和一个 `Document` 节点，完全不抽取实体和关系。

### 2.2 LLM/HanLP 能力没用在知识提取上

- 项目已有通用管线雏形（`pipeline/engine.rs` + `processors/{hanlp_client, llm_client,
  store}`），但 `build` 没调用它，走 `PipelineTemplate` 里另一套硬编码流程。
- `store.rs::collect_entities` 把 LLM 响应当"一整块 analysis 文本"存
  （`store.rs:248-261`），不解析 JSON 里的 entities/relations，不建图关系。HanLP 的
  NER/关键词反而被当成独立实体写图（`store.rs:216-245`），噪声直接污染图。
- LLM 唯一实际生效的用途是 Phase 2 给代码方法生成"用途/逻辑"两行字。

### 2.3 知识搜索不走向量

- `search_mcp.rs` 的 `world=knowledge` 用 `MATCH (n) WHERE n.name CONTAINS $fragment ...`
  纯子串匹配。只有 `code` 和 `doc` 两个世界走 Qdrant。知识世界是"哑巴图"。

---

## 3. 决策记录（已锁定）

| # | 决策点 | 结论 |
|---|--------|------|
| D1 | `@knowledge` 手工注解流程 | **完全删除**，知识唯一来源 = LLM 抽取 |
| D2 | HanLP 定位 | **LLM 的候选锚点**，NER/关键词只进 prompt，不入库 |
| D3 | 实体消歧 | **两级**：规范名精确 MERGE + 向量近邻（cos > 0.92）合并 |
| D4 | 抽取实体向量存储 | **复用 `kg_nodes` collection**，与现有业务节点同库 |
| D5 | 文档块原文 | **双写**：块原文 → `doc_chunks`，实体语义 → `kg_nodes` |
| D6 | 落地顺序 | **构建优先**，检索层实现延后（架构本文档定稿） |
| D7 | 向量点主键 | **改从业务主键派生**（改进点 I1，见 §9），不再用 elementId |

---

## 4. 目标总体架构

```
┌────────────────────────────────────────────────────────────────────┐
│ Extract 抽取层    任意文件 → 结构化信号                              │
│                                                                    │
│   File ──chunk──> Block ──┬──> HanLP: NER候选 + 关键词 (仅锚点)     │
│                           └──> LLM:   实体(规范名/类型/摘要)         │
│                                       + 关系三元组(带证据/置信度)     │
└───────────────────────────────┬────────────────────────────────────┘
                                │  ExtractedGraph { entities, relations,
                                │                   block_summary, doc_id }
┌───────────────────────────────▼────────────────────────────────────┐
│ Consolidate 整合层    信号 → 去重/消歧 → 落库 → 双写向量             │
│                                                                    │
│   规范化 → 两级消歧 → MERGE Entity 节点 → MERGE RELATES 边           │
│                    → MENTIONED_IN 溯源                               │
│                    → 双写: entity→kg_nodes(边写边查)                 │
│                            chunk原文→doc_chunks(带 entity_ids)       │
└───────────────────────────────┬────────────────────────────────────┘
                                │
┌───────────────────────────────▼────────────────────────────────────┐
│ Retrieve 检索层（实现延后，架构定稿）                                │
│                                                                    │
│   query ─embed─> Qdrant kg_nodes 语义召回 top-3K entity_id          │
│                  ├─> Memgraph 图扩展（1~2 跳邻居/关系）              │
│                  └─> bge-reranker 重排 → 融合排序                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## 5. Extract 抽取层

### 5.1 组件分工

| 组件 | 职责 | 产出 | 理由 |
|------|------|------|------|
| Chunk | 按语义边界切块（`shared::chunker`，现有） | `Block` | LLM 有上下文窗口限制 |
| HanLP | 快速、零 token 成本的候选识别 | NER 实体候选、关键词 | 给 LLM 当锚点，提召回、省 token；**不入库** |
| LLM | 判断与结构化 | 规范化实体 + 关系三元组 + 块摘要 | 只有 LLM 能做类型判定、指代消解、关系抽取 |

协同关键：**不让 LLM 从零读全文抽实体**。把 HanLP 的 NER 候选 + 关键词塞进 prompt，LLM
做三件事：① 从候选确认/合并/补充实体，定 type 和一句话 summary；② 抽关系三元组并带原
文证据句；③ 输出块级 summary（后面向量化用）。

### 5.2 统一产出结构 `ExtractedGraph`

新增 `src/application/knowledge/extract/model.rs`：

```rust
pub struct ExtractedGraph {
    pub doc_id: String,
    pub block_index: u32,
    pub block_summary: String,
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

pub struct ExtractedEntity {
    pub mention: String,         // 原文提法
    pub canonical_name: String,  // 规范名（消歧主键的原料）
    pub entity_type: String,     // 自由类型，推荐词表见 prompt
    pub summary: String,         // 一句话语义摘要（向量化的核心文本）
    pub keywords: Vec<String>,
}

pub struct ExtractedRelation {
    pub head: String,      // 对应某实体的 canonical_name
    pub relation: String,  // 规范动词，如 routes_to / depends_on / configured_by
    pub tail: String,
    pub evidence: String,  // 原文证据句
    pub confidence: f32,   // 0~1
}
```

### 5.3 Prompt 重写：`config/prompts/document_with_nlp.yaml`

现有 prompt（35 行）问题：实体只有 `name/type/description`，关系限定
`depends|contains|relates` 三种，无证据、无置信度、无规范名。整体重写为：

```yaml
name: document_with_nlp
description: "通用文档知识抽取 — 实体(规范名/类型/摘要) + 关系三元组(带证据)"
system: |
  你是知识抽取助手。基于给定的 NLP 候选和文档内容，抽取结构化知识，仅输出 JSON。

  输出格式：
  {
    "block_summary": "本块内容概述（50字以内）",
    "entities": [
      {"mention": "原文提法", "canonical_name": "规范名",
       "type": "Service|Channel|Config|Table|Api|Concept|Person|Org|Product|Other",
       "summary": "一句话说明它是什么/做什么", "keywords": ["关键词"]}
    ],
    "relations": [
      {"head": "规范名A", "relation": "规范动词如 routes_to/depends_on/contains",
       "tail": "规范名B", "evidence": "原文证据句", "confidence": 0.0}
    ]
  }

  规则：
  - 仅输出 JSON，不要 markdown，不要额外说明
  - canonical_name 用于跨块指同一实体，同一实体必须使用同一个规范名
  - relation 的 head/tail 必须引用 entities 里的 canonical_name
  - NLP 候选仅供召回参考，你可确认、合并、补充或丢弃
  - confidence 反映证据充分程度；不确定的字段设 null
prompt: |
  文件：${file_path}

  NLP 实体候选：
  ${hanlp.entities}

  关键词：
  ${hanlp.keywords}

  文档内容：
  ${file_text}
```

`llm_client.rs::build_render_context` 已有把 HanLP 输出注入 prompt 的逻辑，继续沿用；
改动点是 LLM 响应不再当整块文本，而是**解析 JSON → `ExtractedGraph`**（解析失败降级：
整块文本只进 `doc_chunks`，不写图）。

---

## 6. Consolidate 整合层

LLM 逐块抽取会产出大量重复实体（同一"支付网关"出现在 10 个块），直接写图会炸。
`store.rs` 整体重写为该层，新增 `src/application/knowledge/extract/consolidate.rs`。

### 6.1 两级实体消歧

```rust
// 第一级（便宜）：规范名精确 MERGE
let canonical = normalize(&entity.canonical_name); // 小写、trim、全半角统一

// 第二级（准）：向量近邻消歧，复用 embed 服务
let hits = qdrant.search("kg_nodes", embed(canonical + summary),
                         k = 5, filter = project);
if hits.top.score > 0.92 && type 一致 {
    // MERGE 到已有 entity_id，合并 aliases / summary
} else {
    // 新建 entity_id = dt://entity/{project}/{type}/{canonical}
}
```

**顺序依赖**：近邻查询依赖 `kg_nodes` 已有向量，所以同一次 build 内必须**边写边查**，
逐实体 upsert 而不是最后批量 upsert（与现有 store 的批量逻辑不同，重写时留意）。

### 6.2 图落库 Cypher

```cypher
// 实体：以稳定业务键为主键
MERGE (e:Entity {entity_id: $entity_id})
  ON CREATE SET e.name = $name, e.type = $type, e.summary = $summary,
                e.keywords = $keywords, e.project = $project, e.aliases = [$mention]
  ON MATCH  SET e.summary = $summary,
                e.aliases = coalesce(e.aliases, []) + $new_aliases

// 关系：单一 RELATES 类型 + type 属性（Memgraph 不支持参数化边类型，务实取舍）
MATCH (h:Entity {entity_id: $head_id}), (t:Entity {entity_id: $tail_id})
MERGE (h)-[r:RELATES {type: $rel_type}]->(t)
  SET r.evidence = $evidence, r.confidence = $confidence

// 溯源：实体来自哪个文档
MATCH (e:Entity {entity_id: $id}), (d:Document {doc_id: $doc_id})
MERGE (e)-[:MENTIONED_IN]->(d)
```

配套一次性迁移：`CREATE INDEX ON :Entity(entity_id);`（回查和图扩展都按它过滤）。

### 6.3 双写向量（每实体/每块各一次）

```
Entity MERGE 成功
  → embed(text = canonical_name + summary + keywords.join(" "))
  → upsert kg_nodes（payload 见 §7.2）
  → 图节点标记 _kg_synced_at

Block 处理完成
  → embed(text = block_summary + 原文块)
  → upsert doc_chunks（payload 带 entity_ids，见 §7.3）
```

---

## 7. 向量库设计

### 7.1 Collection 分工（全部 dim=1024，BGE-M3）

定义于 `src/shared/collections.rs`，沿用现有三库，职责重新划清：

| Collection | 内容 | embedding 文本 | 写入时机 |
|------------|------|----------------|----------|
| `code_methods` | 代码方法（现状不动） | 方法签名 + LLM 用途 | `dt build` Phase 2 |
| `doc_chunks` | 文档块原文（双写①） | block_summary + 原文块 | Consolidate 层 |
| `kg_nodes` | KG 节点语义（双写②）：抽取 Entity + 现有业务节点 | canonical_name + summary + keywords | Consolidate 层，边写边 upsert |

粒度互补：实体命中后可回 `doc_chunks` 取证据段落；块检索也能兜底实体抽取的遗漏。

### 7.2 `kg_nodes` payload schema（扩展现有 `build_payload`）

```json
{
  "elementId": "4:91:12345",
  "business_id": "dt://entity/offen-pay/Channel/ifcode",
  "name": "ifCode",
  "type": "Channel",
  "summary": "渠道路由字段，决定支付请求路由到哪个平台",
  "keywords": ["路由", "支付平台", "渠道"],
  "project": "offen-pay",
  "labels": ["Entity"],
  "doc_id": "dt://doc/offen-pay/pay-design.md",
  "origin": "extracted",
  "source": "kg"
}
```

- `elementId`：Memgraph 内部 ID，供图扩展（`elementId(n) IN $ids`）使用。**全量重建后会
  变**，只做运行时扩展，不做跨重建关联。
- `business_id`：稳定业务主键（Entity 即 `entity_id`；旧业务节点用各自 `knowledge_id`
  等）。跨库关联、过滤、删除一律以它为准。
- `origin`：`extracted | learned | manual`，区分知识来源，检索时可过滤。
- `summary` 完整保留（不再截断 200 字，embedding 质量优先；展示截断是调用方的事）。

### 7.3 `doc_chunks` payload schema（新增 `entity_ids`）

```json
{
  "doc_id": "dt://doc/offen-pay/pay-design.md",
  "block_index": 3,
  "project": "offen-pay",
  "entity_ids": ["dt://entity/offen-pay/Channel/ifcode", "..."],
  "source": "doc"
}
```

`entity_ids` 把块和块内提到的实体关联起来：证据检索可 join，也支持"该块提到哪些实体"
的反向查询。

### 7.4 向量点主键（point_id）——改进点 I1

现状：`make_point_id(elementId)`（`kg_bridge.rs:1007`，SHA-256 派生确定性 UUID）。
elementId 全量重建后变化 → 旧向量点成孤儿，无法幂等覆盖，也无法按业务键删除。

**改为：`point_id = make_point_id(business_id)`**。函数本身不动，调用处改传业务主键：
- 重建幂等：同一实体反复 upsert 覆盖同一个点；
- 删除简单：按 business_id 直接算 point_id 删除，无需先查图拿 elementId；
- 一致性可校验：图里有的 business_id 与库里 point 集合可直接 diff。

### 7.5 与知识图谱的联系：强耦合、单向真相

1. **定位**：向量库是 KG 的语义索引，不存任何图里没有的知识。KG 是唯一真相源。
2. **双键回链**：`elementId`（图扩展）+ `business_id`（稳定关联）。
3. **一致性三道保险**：
   - **写穿**：Entity MERGE 成功后立即 embed + upsert，图节点标 `_kg_synced_at`；
   - **兜底**：`dt kg-sync` 扫无 `_kg_synced_at` 节点补偿（现有机制保留）；
   - **删除闭环**（现状缺失，新增）：图节点删除时按 point_id 删向量点；
     FullRebuild 时先 `delete_by_filter(project=...)` 清项目向量（`VectorRepository`
     已有 `delete_by_filter`，`domain/traits.rs:58`）。
4. **消歧依赖写穿**：向量近邻消歧要求单实体 upsert 后立即可查，禁止攒批。

---

## 8. Retrieve 检索层（架构定稿，实现延后）

`search_mcp.rs` 的 `search_knowledge` CONTAINS 查询（约 264-327 行）整体替换为
GraphRAG 式混合检索：

```
fn search_knowledge(query, project, limit):
  1. 召回:  q_vec = embed(query)
            hits = qdrant.search("kg_nodes", q_vec, k = limit*3, filter = project)
            → top-3K 个 business_id + 语义分
  2. 图扩展: MATCH (e:Entity)-[r:RELATES]-(nb) WHERE e.entity_id IN $seed_ids
            → 1~2 跳邻居和关系边纳入候选（捞回向量漏掉但结构相关的）
  3. 重排:  bge-reranker-v2-m3 对 (query, 候选 name+summary) 打分
            （rerank_provider 配置已存在）
  4. 融合:  语义分 + 图距离衰减 + rerank 分 → 排序截断 limit
```

同一逻辑适用于 `world=code` / `world=doc` 的后续增强；`doc_chunks` 支撑"给我证据段落"
类查询。该层实现为独立后续任务，不影响构建链落地。

---

## 9. 对现有机制的改进点（超出简单复用）

| # | 位置 | 现状 | 改进 |
|---|------|------|------|
| I1 | `kg_bridge.rs:868,1007` | point_id 从 elementId 派生，重建后孤儿化 | 改从 `business_id` 派生（§7.4） |
| I2 | `kg_bridge.rs:983 build_payload` | 硬编码 `service_type/environment`，无实体字段 | 统一核心 schema（§7.2），按 label 放扩展字段 |
| I3 | `kg_bridge.rs:842 concat_props` | 跳过数组，`keywords/aliases` 拼不进 embedding 文本 | 支持字符串数组拼接 |
| I4 | `kg_bridge.rs:996` | description 截断 200 字进 payload | `summary` 完整保留（§7.2） |
| I5 | 删除路径 | 只有写穿+补偿，图删向量留 | 按 point_id/business_id 删除 + FullRebuild 清项目（§7.5） |
| I6 | `document_with_nlp.yaml` | 弱 schema，3 种关系，无证据 | 整体重写（§5.3） |
| I7 | Memgraph | `Entity.entity_id` 无索引 | `CREATE INDEX ON :Entity(entity_id)` |

---

## 10. 现有代码映射：删 / 改 / 增

### 10.1 删除

| 位置 | 内容 |
|------|------|
| `build/pipeline.rs:230-249` | Step 6b 整段 |
| `build/pipeline.rs:611` | `extract_knowledge_annotations` 调用 |
| `build/pipeline.rs:847-1133`（约） | `write_knowledge_annotations()` 整个函数 |
| `build/pipeline.rs:1333, 1366` | `process_documents` 里的注解提取与写入 |
| `build/pipeline.rs:63`、`build/service.rs:126` | `ExtractionResult.knowledge_annotations` 字段及线程收集逻辑 |
| `infrastructure/parser` | `extract_knowledge_annotations` |
| `knowledge/knowledge/annotation.rs` | `KnowledgeAnnotation` 提取部分 |
| `store.rs:216-245` | HanLP 实体/关键词写图分支 |
| `store.rs:248-261` | LLM 响应当整块存的分支 |
| `knowledge/learn.rs` | `LearnService` 不进 build 主流程（保留代码，停用接入） |

### 10.2 改造

| 位置 | 改什么 |
|------|--------|
| `pipeline/processors/llm_client.rs` | 响应解析为 `ExtractedGraph`（JSON 解析失败降级为只进 doc_chunks） |
| `config/prompts/document_with_nlp.yaml` | 按 §5.3 重写 |
| `pipeline/processors/store.rs` | 重写为 Consolidate 层：解析 → 消歧 → 写图 → 双写向量（边写边查） |
| `build/pipeline.rs::process_documents`（1216 起） | 文档块喂给 pipeline engine，不再只 chunk+embed |
| `sync/kg_bridge.rs` | `build_payload`/`build_search_text`/`build_qdrant_point` 按 §7.2/§7.4/I2-I4 扩展；新增按 business_id 删除 |
| `shared/vectorizer.rs` | `doc_chunks` payload 增加 `entity_ids`（§7.3） |

### 10.3 新增

| 位置 | 内容 |
|------|------|
| `application/knowledge/extract/model.rs` | `ExtractedGraph` 等结构（§5.2） |
| `application/knowledge/extract/consolidate.rs` | 消歧 + 落库 + 双写编排（§6） |
| `application/knowledge/extract/retrieve.rs` | 混合检索（延后，§8） |
| Memgraph 迁移 | `CREATE INDEX ON :Entity(entity_id)` |

统一入口：build 的文档处理真正走 `pipeline::engine`（tree_sitter → chunk → hanlp →
llm → store），代码文件继续走现有 AST 抽取，文档文件走通用抽取链。

---

## 11. 落地顺序（构建优先，每步独立可验证）

| 步骤 | 内容 | 验证方式 |
|------|------|----------|
| **S1** | 定义 `ExtractedGraph` + 重写 `document_with_nlp.yaml`；llm_client 解析 JSON | 拿 3~5 个真实文档跑通"文本→实体+关系 JSON"，人工核对 JSON 质量 |
| **S2** | 重写 `store.rs` 为 Consolidate 层：两级消歧 + 写 Entity/RELATES/MENTIONED_IN + 双写向量（含 I1-I5 改进）；建 `entity_id` 索引 | `dt build --test` 对 `test/expected.json` 验证；Cypher 抽查 Entity/RELATES；Qdrant 抽查 payload |
| **S3** | `process_documents` 接入 pipeline engine | `dt build --test` 全量；对比同一文档重复 build 的实体去重效果 |
| **S4** | 删除 `@knowledge` 全链路 + store 老分支 + learn 停用 | `cargo build && cargo test && cargo clippy --all-targets` 全绿 |
| **S5**（延后） | 检索层：`search_knowledge` 重写为向量召回 + 图扩展 + rerank | 语义查询命中非字面匹配实体（如"渠道怎么路由"命中 ifCode） |

---

## 12. 风险与注意点

1. **边写边查的性能**：逐实体 upsert + 近邻查询比批量慢。可接受（文档量级小）；若成瓶
   颈，按文档维度做"块内批量、文档间写穿"的折中，消歧阈值内结果不变。
2. **LLM JSON 稳定性**：必须有解析失败降级（只进 doc_chunks）+ 重试一次（带"仅输出
   JSON"修正提示）。降级块记入日志便于后续补抽。
3. **消歧误合并**：cos>0.92 + type 一致双条件，宁可多建节点不可错并；`SAME_AS` 边保留
   人工纠正入口。
4. **关系类型发散**：自由文本 relation 会发散，靠 prompt 推荐词表收敛；后续可跑一次
   关系聚类治理，不在本期范围。
5. **point_id 切换的迁移**：I1 切换派生键后，`kg_nodes` 存量点需随下一次 build/kg-sync
   自然重建；切换前可对 `kg_nodes` 做一次 `delete_collection` 清库（数据皆可从图重建，
   无损失）。
