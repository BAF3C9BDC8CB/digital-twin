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
| D8 | 实体类型 | **固定枚举**（`EntityType`，§5.3），词表外归 `Other`；非自由文本 |
| D9 | 实体生命周期 | **边随文档走、实体按引用存活、孤儿周期清理**（§6.5），非只增不删 |
| D10 | 并发漏合并 | **默认接受 + `SAME_AS` 事后治理**（§6.1），不默认加锁 |

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

### 5.2 块级数据流（引擎如何走到块）

pipeline engine **以文件为单位执行**（`engine.rs:196-237`，多文件并行；单文件内各
processor 按优先级顺序各执行一次）。chunk processor 一次产出**全部块**的 JSON 数组
（`chunk.rs:90-111`，含 `chunk_index`）和 `doc_id`（`chunk.rs:83-87`，
`dt://doc/{project}/{path}`）。现状 `llm_client` 不消费 chunks、直接用 `ctx.file_text`
全文本——这正是要改的点。

新数据流（文件内块级循环，归属 llm/extract 处理器内部）：

```
engine: 一次 execute/文件
  chunk 处理器 → outputs["chunk"]: { doc_id, chunks[{chunk_index, text, ...}] }
  hanlp 处理器 → 逐块跑，输出与 chunks 按 block_index 对齐：
                 outputs["hanlp"]: hanlp_blocks[{block_index, entities, keywords}]
  llm 处理器   → 遍历 chunks[]，每块一次 LLM 调用
                 → 渲染第 i 块 prompt 时注入 hanlp_blocks[i] 的候选
                  （不是全文候选——块级对齐是本数据流的硬约束）
                 → 每块产出一个 ExtractedGraph { doc_id, block_index = chunk_index, ... }
  store/consolidate → 消费 Vec<ExtractedGraph>，逐块落库
```

即：**块不独立走管线，而是在 llm 处理器内部循环**；`block_index` 直接取
`chunk.chunk_index`，Consolidate 据此关联 `doc_chunks` payload 的 `block_index`。

**块级调用的并发策略（默认）**：块级循环**串行**。理由：engine 在 GPU 阶段已按文件
粒度做 semaphore 并发（`engine.rs:307-315`），多文件的大文档天然互相填满 GPU 容量；
块内串行实现最简单且块间无数据依赖问题（每块独立抽取）。若日后 profiling 表明"单
大文档阻塞管线"成为瓶颈，可选升级为**块级有界并发**（如 3）：直接复用
`infer_client` 内按 in-flight HTTP 请求限流的共享 `Semaphore`
（`infer_client.rs:83-102`），无需新建并发设施。

### 5.3 统一产出结构 `ExtractedGraph`

新增 `src/application/knowledge/extract/model.rs`：

```rust
pub struct ExtractedGraph {
    pub doc_id: String,       // 来自 chunk 处理器输出
    pub block_index: u32,     // = chunk.chunk_index
    pub block_summary: String,
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
    pub degraded: bool,       // JSON 解析失败降级标记（§5.5）
}

pub struct ExtractedEntity {
    pub mention: String,         // 原文提法
    pub canonical_name: String,  // 规范名（消歧主键的原料）
    pub entity_type: EntityType, // 固定枚举（见下），未知 → Other
    pub summary: String,         // 一句话语义摘要（向量化的核心文本）
    pub keywords: Vec<String>,
}

/// 固定类型词表——不是自由文本。消歧的"type 一致"强约束依赖它是封闭集合。
pub enum EntityType {
    Service, Channel, Config, Table, Api,
    Concept, Person, Org, Product, Other,
}

pub struct ExtractedRelation {
    pub head: String,      // 必须等于某实体的 canonical_name
    pub relation: String,  // 规范动词，如 routes_to / depends_on / configured_by
    pub tail: String,
    // Option 是必要的：prompt 规则允许"不确定设 null"（§5.4），
    // serde 把显式 null 反序列化到 String/f32 会直接失败、误触发 §5.5 降级。
    // Option 字段自动同时容忍"字段缺失"和"显式 null"。
    pub evidence: Option<String>,
    pub confidence: Option<f32>,
}
```

LLM 返回词表外的 type 时归一为 `Other`（记录原值到 `aliases`），保证 §6.1 的
"type 一致"是强约束而不是宽松匹配。

字段为空的消费规则（Consolidate 层归一化时执行）：`confidence.unwrap_or(0.5)`、
`evidence.unwrap_or_default()`；`canonical_name`/`summary` 为 null 的实体属于无效
产出，**整条丢弃并记日志**（不误判为降级块）。

### 5.4 Prompt 重写：`config/prompts/document_with_nlp.yaml`

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
  - type 必须从给定词表中选择，词表外的归入 Other
  - canonical_name 用于跨块指同一实体，同一实体必须使用同一个规范名
  - relation 的 head/tail 必须引用 entities 里的 canonical_name
  - NLP 候选仅供召回参考，你可确认、合并、补充或丢弃
  - confidence 反映证据充分程度；不确定的字段设 null
prompt: |
  文件：${file_path}

  NLP 实体候选：
  ${entities}

  关键词：
  ${keywords}

  文档内容：
  ${file_text}
```

**模板变量必须是扁平的 `${entities}` / `${keywords}`，不是 `${hanlp.entities}`。**
渲染器 `render_template`（`pipeline/prompt.rs:144-174`）支持 `${a.b}` 点路径，但
`build_render_context`（`llm_client.rs:152-166`）注入的是**扁平键**
（`entities/keywords/summary/file_text`）；解析不到的路径会**原样留在渲染结果里**
（`prompt.rs:143`），不会报错。现有 yaml 写的 `${hanlp.entities}` 今天就是坏的——
HanLP 候选从未真正进入 prompt。重写时一并修正，实现者不要再踩。

**`build_render_context` 同步改为按块渲染**：现在它把整个 hanlp 输出整体注入；新数
据流下每次渲染第 i 块，上下文为
`{ file_path, file_text: chunks[i].text, entities: hanlp_blocks[i].entities,
keywords: hanlp_blocks[i].keywords }`（§5.2 块级对齐）。`file_text` 也从全文改为块
文本——这同时把单次 LLM 调用的 token 消耗降到块级。

### 5.5 LLM 响应解析与降级

LLM 响应不再当整块文本，而是**解析 JSON → `ExtractedGraph`**。解析失败时：

1. 重试一次（附加"仅输出 JSON"修正提示）；
2. 仍失败则降级：`degraded = true`，该块**只进 `doc_chunks` 不写图**，
   embedding 文本 = **原始块文本**（没有 block_summary 可用），payload 标记
   `"degraded": true` 便于后续补抽；
3. 降级块计入日志与 build 报告。

---

## 6. Consolidate 整合层

LLM 逐块抽取会产出大量重复实体（同一"支付网关"出现在 10 个块），直接写图会炸。
`store.rs` 整体重写为该层，新增 `src/application/knowledge/extract/consolidate.rs`。

### 6.1 两级实体消歧

```rust
// 规范化：小写、trim、全半角统一 + URI 保留字符百分号编码（先 % → %25，
// 再 / 空格 # ? 等）。编码是硬要求：canonical 由 LLM 从中文文档自由生成，
// 可能含 "/api/pay/route"、"读/写分离" 这类字符，不编码会注入额外 URI 段、
// 破坏 entity_id 层级、让下游按段解析错位。选百分号编码而非字符替换——
// 替换会让 "读/写分离" 与 "读_写分离" 碰撞成同一 ID。
// （make_method_id/make_class_id 不转义是安全的：代码标识符受语言语法约束
//   不可能含 /；LLM 产物没有这个约束，新链路的新风险不能照搬旧惯例。）
let canonical = normalize(&entity.canonical_name);
let entity_id = format!("dt://entity/{project}/{type}/{canonical}");

// 第一级（便宜）：精确命中直接短路——不 embed 查询向量、不做近邻搜索。
// （存储向量仍随 §6.3 块批量 embed，用于同步 keywords/summary 的演化。）
if graph.entity_exists(&entity_id) {  // 可按块批量 UNWIND 一次查完
    // → 直接 MERGE（ON MATCH 更新 summary/aliases/keywords）
} else {
    // 第二级（准）：向量近邻消歧，复用 embed 服务
    let hits = qdrant.search("kg_nodes", embed(&entity_embed_text(&entity)),
                             k = 5, filter = project);
    if hits.top.score > 0.92 && type 一致 {
        // → MERGE 到已有 entity_id，合并 aliases/summary/keywords
    } else {
        // → 新建
    }
}
```

**消歧查询与入库存储必须使用同一个文本构造函数**（硬约束）：

```rust
/// 消歧查询（§6.1）和实体入库（§6.3）共用，禁止两处各写各的拼接。
/// 构造方式不同的向量在同一空间算余弦会有系统性偏差，0.92 阈值失真。
fn entity_embed_text(e: &ExtractedEntity) -> String {
    format!("{}。{}。关键词: {}", e.canonical_name, e.summary, e.keywords.join(" "))
}
```

**顺序依赖**：近邻查询依赖 `kg_nodes` 已有向量，所以同一次 build 内必须**边写边查**，
逐实体 upsert 而不是最后批量 upsert（与现有 store 的批量逻辑不同，重写时留意）。

**并发安全**（engine 多文件并行，`engine.rs:196-237`）：两个 worker 同时处理含同一实
体的不同文档时——

- **同名实体（canonical 相同）：安全**。`entity_id` 是从 canonical 确定性派生的
  （`dt://entity/{project}/{type}/{canonical}`），两 worker 算出同一个 ID；
  Memgraph `MERGE` 原子，叠加 `entity_id` 唯一约束（§6.2 迁移），只会产生一个节点；
  向量 upsert 用确定性 point_id（I1），重复写幂等。
- **近重复实体（canonical 不同、cos>0.92）：存在漏合并窗口**。worker B 的近邻查询可
  能先于 worker A 的 upsert 落库，导致本该合并的两个实体各自新建节点。后果是有界的
  （少量近重复节点，不产生重复向量点），处理策略：
  1. **默认接受**，靠 §6.4 的 `SAME_AS` 边事后治理——下次增量 build 处理其中任一实
     体时，近邻查询会命中另一个，补写 `SAME_AS`；
  2. 可选强化：Consolidate 层对"消歧查询 + 写图 + upsert"临界区加**项目级互斥锁**
     （per-project `tokio::sync::Mutex`），彻底消除窗口，代价是文档间消歧串行化。
     默认不启用，观测到近重复率不可接受时再开。

### 6.2 图落库 Cypher

**Document 节点归属 Consolidate 层**：现有 `MERGE (d:Document ...)` 只在旧路径
（`build/pipeline.rs:1419`），新链路必须自己保证 Document 存在，否则 `MENTIONED_IN`
会因节点不存在而静默失败（`MATCH` 不命中即整条不执行）。每个块处理前先 MERGE：

```cypher
// 0. 文档节点（每块幂等 MERGE，先于一切溯源写入）
MERGE (d:Document {doc_id: $doc_id})
  ON CREATE SET d.project = $project, d.file_path = $file_path,
                d.doc_type = $doc_type

// 1. 实体：以稳定业务键为主键
//    aliases 必须去重合并（REDUCE 实现）——无条件 append 会让同一 mention
//    在每次增量 build 重复入列，aliases 随构建次数线性膨胀
MERGE (e:Entity {entity_id: $entity_id})
  ON CREATE SET e.name = $name, e.type = $type, e.summary = $summary,
                e.keywords = $keywords, e.project = $project, e.aliases = [$mention]
  ON MATCH  SET e.summary = $summary,
                e.aliases = REDUCE(acc = coalesce(e.aliases, []), x IN $new_aliases |
                              CASE WHEN x IN acc THEN acc ELSE acc + x END),
                e.keywords = REDUCE(kacc = coalesce(e.keywords, []), x IN $keywords |
                              CASE WHEN x IN kacc THEN kacc ELSE kacc + x END)

// 2. 关系：单一 RELATES 类型 + type 属性（Memgraph 不支持参数化边类型，务实取舍）
//    $head_id/$tail_id 必须来自本块 canonical→entity_id 映射表（下方硬约束），
//    禁止从 canonical 重新派生——否则第二级消歧合并的实体会静默丢边
//    r.doc_id 是边级溯源：增量重建时按它精确清除该文档产生的旧关系（§6.5）
MATCH (h:Entity {entity_id: $head_id}), (t:Entity {entity_id: $tail_id})
MERGE (h)-[r:RELATES {type: $rel_type, doc_id: $doc_id}]->(t)
  SET r.evidence = $evidence, r.confidence = $confidence

// 3. 溯源：实体来自哪个文档
MATCH (e:Entity {entity_id: $id}), (d:Document {doc_id: $doc_id})
MERGE (e)-[:MENTIONED_IN]->(d)
```

**关系端点解析（硬约束，违反即静默丢边）**：`$head_id`/`$tail_id` **禁止**从
`head`/`tail` 的 canonical 重新派生。`ExtractedRelation.head/tail` 存的是
canonical_name（§5.3），而第二级向量消歧会把实体合并到**另一个主实体的
entity_id**——被合并的实体根本没有按自己 canonical 派生的节点。此时用 head 派生
ID 去 MATCH 会落空，`MERGE` 整条不执行，关系边**静默丢失**（例："支付网关"被合并
进"支付服务网关"后，`支付网关 -routes_to-> 银联渠道` 按前者派生 ID 必然 MATCH 不
中）。正确做法：

```
Consolidate 处理每个块时维护本块映射表：
    canonical_name → 消歧后实际落库的 entity_id
    （每个实体在 §6.1 消歧出结果时即登记，无论短路/合并/新建）
关系落库时 head_id = map[head], tail_id = map[tail]
映射表未命中 → 回退按规范名精确派生（端点可能是历史 build 建的老节点）
仍不命中    → 记日志 + 丢弃该关系（计入 build 报告的孤儿关系数），
              不补建占位实体
```

**事务边界（有意选择最终一致）**：§6.2 的 0/1/2/3 是四条独立 `write_query` 调用
（现有 `GraphRepository::write_query` 一次一条，`store.rs:323`），不包多语句事务。
中途失败会留下部分写入——接受，靠既有补偿机制收敛：`_kg_synced_at` 只在某实体的
全部步骤（实体+关系+溯源+向量 upsert）成功后才标记，`dt kg-sync` 会兜底重放未完成
节点（§7.5）。文档级清除（§6.5）在下一轮 build 入口也会抹平残留。

配套一次性迁移：

```cypher
CREATE INDEX ON :Entity(entity_id);
CREATE CONSTRAINT ON (e:Entity) ASSERT e.entity_id IS UNIQUE;  // 并发安全依赖它
```

**图属性与向量的有意近似**：`ON MATCH` 后图的 `keywords/aliases` 是累积并集，而
§6.3 的存储向量始终用**最新一次抽取**的 keywords/summary 构造。两者不完全一致是
有意的：向量保检索时效（反映最新语义），图保完整历史；完全同步需要写后读回合并
集再 embed，一次额外往返换边际收益，不做。

### 6.3 双写向量（每实体/每块各一次）

```
Entity MERGE 成功
  → embed(text = entity_embed_text(entity))   // 与 §6.1 消歧查询同一构造函数，硬约束
  → upsert kg_nodes（payload 见 §7.2）
  → 图节点标记 _kg_synced_at

Block 处理完成
  → embed(text = block_summary + 原文块)   // 降级块：只用原文块（§5.5）
  → upsert doc_chunks（payload 带 entity_ids，见 §7.3；降级块带 "degraded": true）
```

**embed 与 upsert 解耦批量化**：消歧的"边写边查"约束的是 **upsert 落库顺序**，不是
embed 顺序。因此 embed 可按块批量——`embed_batch(块内全部实体的 entity_embed_text)`
一次 API 往返拿回全部向量，随后逐实体执行"近邻查询 → 写图 → upsert"。10 块×5 实体
的文档，实体 embed 从 50 次串行往返压到 10 次批量调用，消歧正确性不受影响。

### 6.4 `SAME_AS` 边（消歧安全阀，最小定义）

用途：① 向量近邻消歧判定"应合并但保留双节点"时挂边；② 并发漏合并（§6.1）的事后治
理；③ 人工纠正入口。

```cypher
// 单向一条即可，查询时按无向对待：MATCH (a)-[:SAME_AS]-(b)
MATCH (a:Entity {entity_id: $from_id}), (b:Entity {entity_id: $to_id})
MERGE (a)-[r:SAME_AS]->(b)
  SET r.score = $score,           // 触发时的余弦相似度，人工纠正置 1.0
      r.created_by = $created_by, // "auto" | "manual"
      r.reason = $reason,
      r.created_at = datetime()
```

- `created_by = "auto"`：Consolidate 消歧或后续 build 补挂；
- `created_by = "manual"`：人工纠正。本期不提供专门 dt 命令，直接用 Cypher（上面的语
  句即入口）；检索层必须把 `SAME_AS` 邻居视为同一实体返回。反向纠正（拆散错误合并）
  同样用 Cypher `MATCH ()-[r:SAME_AS]->() WHERE ... DELETE r`。

### 6.5 实体生命周期（增量构建下的更新/删除）

有意采取**"边随文档走、实体按引用存活"**的策略，而不是无脑只增不删。

**触发入口（两个，按事件类型分开）**：

- **文档被修改/新增** → 增量策略的 SHA1 diff 会把它放进 `changed_paths`
  （`strategy/incremental.rs:74-84`）→ 文档正常进入管线 → **Consolidate 层入口自
  治**：任何文档开始抽取写入前，先执行本条第 1 点的清除 Cypher，再写入新产物。
  清除是幂等的（文档首次构建时无旧产物，清除为无操作），因此不需要 strategy 层
  传任何标记——"进管线即先清后写"。
- **文档被删除** → 增量策略产出 `deleted_paths`（同处），这些文档**不进管线**，
  Consolidate 没机会自治 → **由 build 编排层消费 `deleted_paths`**，对其中每个
  `doc_id` 执行本条第 2 点的删除清理。
- **FullRebuild**：整库清空前无需逐文档清理（`full_rebuild.rs` 的 wipe 已覆盖）。

1. **文档被修改/重建**：先按溯源精确清除该文档的旧产物，再走正常抽取写入——
   ```cypher
   MATCH ()-[r:RELATES {doc_id: $doc_id}]->() DELETE r;
   MATCH ()-[m:MENTIONED_IN]->(:Document {doc_id: $doc_id}) DELETE m;
   ```
   同时**按 `doc_id` 删除该文档全部旧 `doc_chunks` 向量点**
   （`delete_by_filter(doc_id=...)`）再写新块——否则块数变少时（10 块→8 块），
   旧 `block_index` 的点会残留成孤儿，新构建覆盖不到它们。
   （这正是 §6.2 给 `RELATES` 加 `doc_id` 属性的原因。）
2. **文档被删除**：同上清除边 + 删 `Document` 节点 + 删 `doc_chunks` 向量点。
3. **实体节点**：只要还存在任何 `MENTIONED_IN` 或被其他实体的 `RELATES` 引用就保留
   （它是跨文档共享知识，一篇文档消失不杀死它）。
4. **孤儿实体**（零 `MENTIONED_IN`）：不实时清理，由周期性任务/FullRebuild 统一处理，
   并同步按 point_id 删 `kg_nodes` 向量点（§7.5 删除闭环）：
   ```cypher
   OPTIONAL MATCH (e:Entity)-[m:MENTIONED_IN]->()
   WITH e, count(m) AS c WHERE c = 0
   DETACH DELETE e
   ```
5. 关系边的粒度说明：`RELATES` 的 MERGE key 含 `doc_id`（§6.2），所以**不同文档对同
   一对实体的证据以多条边共存**（各自溯源，检索时可聚合）；同一文档重建时先删后写
   （本条第 1 点），不会产生陈旧边。

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
  "degraded": false,
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

`doc_chunks` 没有 business_id，其 point_id 同样明确为确定性派生：

```
point_id = make_point_id("{doc_id}:{block_index}")
```

首次构建/FullRebuild/同 build 重跑均幂等覆盖；文档重建时的孤儿清理仍走
`delete_by_filter(doc_id=...)`（§6.5），两者互补。

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
            → top-3K 个 {business_id, labels} + 语义分
  2. 图扩展: 种子按键类型分流（硬约束）：kg_nodes 是异构库（§7.2），
            business_id 对抽取 Entity 是 entity_id，对 learned/manual
            业务节点是 knowledge_id 等——一律按 entity_id 过滤会让后者
            全部掉队。按 payload.labels 分组后分别扩展：
              Entity      → MATCH (e:Entity)-[r:RELATES]-(nb)
                            WHERE e.entity_id IN $entity_seeds
              其他业务节点 → 按其各自 id 字段（knowledge_id 等）定位后
                            取 1 跳邻居
            → 1~2 跳邻居和关系边纳入候选（捞回向量漏掉但结构相关的）
            → SAME_AS 邻居视为同一实体（§6.4）
            → 边去重：RELATES 的 MERGE key 含 doc_id（§6.2），同一
              (head, rel_type, tail) 可能有多条边（证据来自不同文档）。
              候选集按 (head, rel_type, tail) 去重，保留 confidence 最高
              的一条，其余边的 evidence 聚合为该条的补充证据——避免同一
              关系重复计数、挤占 limit 名额
  3. 重排:  bge-reranker-v2-m3 对 (query, 候选 name+summary) 打分
            （rerank_provider 配置已存在；注意现状：rerank 链路已铺好
              但业务零调用，S5 是首个调用点。reranker 只在检索层使用，
              构建期 S1-S4 不涉及。本地 xinference 的 rerank 模型需与
              此对齐为 bge-reranker-v2-m3，不要用 bge-reranker-base）
  4. 融合:  语义分 + 图距离衰减 + rerank 分 → 排序截断 limit
```

融合排序的初始权重（方向性建议，实现时可调，收敛后写回本节）：

```
final = 0.6 × rerank分            // 主排序信号：reranker 最懂 query 相关性
      + 0.3 × 语义分              // 向量召回分，打底
      + 0.1 × graph_boost         // 图证据加成：直接命中=1.0，1跳=0.5，2跳=0.25（0.5^hop 指数衰减）
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
| I6 | `document_with_nlp.yaml` | 弱 schema，3 种关系，无证据；`${hanlp.*}` 变量名与渲染上下文不匹配（静默失效） | 整体重写 + 扁平变量名（§5.4） |
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
| `pipeline/processors/llm_client.rs` | 消费 chunk 输出、**逐块循环**调 LLM（§5.2）；响应解析为 `ExtractedGraph`，失败按 §5.5 降级 |
| `config/prompts/document_with_nlp.yaml` | 按 §5.4 重写（含变量名修正） |
| `pipeline/processors/store.rs` | 重写为 Consolidate 层：解析 → 消歧 → 写图 → 双写向量（边写边查） |
| `build/pipeline.rs::process_documents`（1216 起） | 文档块喂给 pipeline engine，不再只 chunk+embed |
| `sync/kg_bridge.rs` | `build_payload`/`build_search_text`/`build_qdrant_point` 按 §7.2/§7.4/I2-I4 扩展；新增按 business_id 删除 |
| `shared/vectorizer.rs` | `doc_chunks` payload 增加 `entity_ids`（§7.3） |

### 10.3 新增

| 位置 | 内容 |
|------|------|
| `application/knowledge/extract/model.rs` | `ExtractedGraph` 等结构（§5.3） |
| `application/knowledge/extract/consolidate.rs` | 消歧 + 落库 + 双写编排（§6） |
| `application/knowledge/extract/retrieve.rs` | 混合检索（延后，§8） |
| Memgraph 迁移 | `CREATE INDEX ON :Entity(entity_id)` + `entity_id` 唯一约束（§6.2） |

统一入口：build 的文档处理真正走 `pipeline::engine`（tree_sitter → chunk → hanlp →
llm → store），代码文件继续走现有 AST 抽取，文档文件走通用抽取链。

---

## 11. 落地顺序（构建优先，每步独立可验证）

| 步骤 | 内容 | 验证方式 |
|------|------|----------|
| **S1** | 定义 `ExtractedGraph` + 重写 `document_with_nlp.yaml`；llm_client 解析 JSON | 固定 ≥5 个真实文档的测试集，可量化门槛：① JSON 解析成功率 ≥90%（含一次重试）；② relation 的 head/tail 在 entities 中的覆盖率 ≥95%；③ 抽 20 个实体人工核对，准确率 ≥80% |
| **S2** | 重写 `store.rs` 为 Consolidate 层：两级消歧 + 写 Entity/RELATES/MENTIONED_IN + 双写向量（含 I1-I5 改进）；建 `entity_id` 索引 + 唯一约束 | **同步更新 `test/expected.json`**：加入 Entity 节点数、RELATES 边数、MENTIONED_IN 边数的预期值和关键字段抽样断言——不更新的话 `dt build --test` 只能回归旧字段，验证不到新功能。然后 `dt build --test` 全绿；Cypher 抽查 Entity/RELATES；Qdrant 抽查 payload |
| **S3** | `process_documents` 接入 pipeline engine | `dt build --test` 全量；对比同一文档重复 build 的实体去重效果 |
| **S4** | 删除 `@knowledge` 全链路 + store 老分支 + learn 停用 | `cargo build && cargo test && cargo clippy --all-targets` 全绿 |
| **S5**（延后） | 检索层：`search_knowledge` 重写为向量召回 + 图扩展 + rerank | 语义查询命中非字面匹配实体（如"渠道怎么路由"命中 ifCode） |

---

## 12. 风险与注意点

1. **边写边查的性能**：embed 已按块批量（§6.3，与 upsert 解耦），网络往返大头已消
   除；剩余成本是逐实体的"近邻查询 + 写图 + upsert"，可接受（文档量级小）。注意
   **upsert 不能批量化**——消歧依赖单实体落库后立即可查，批量攒写会破坏正确性，
   这不是性能上可做的折中。
2. **并发漏合并**（不是重复节点）：同名实体靠确定性 `entity_id` + 唯一约束 + `MERGE`
   原子性保证安全；真实风险是近重复实体的漏合并窗口，分析与对策见 §6.1。
3. **LLM JSON 稳定性**：解析失败 → 重试一次 → 降级（`degraded=true`，只进 doc_chunks，
   embedding 用原文块），定义见 §5.5。
4. **消歧误合并**：cos>0.92 + type 一致（固定枚举，§5.3）双条件，宁可多建节点不可错
   并；`SAME_AS` 边为人工纠正入口，schema 见 §6.4。
5. **关系类型发散**：自由文本 relation 会发散，靠 prompt 推荐词表收敛；后续可跑一次
   关系聚类治理，不在本期范围。
6. **point_id 切换的迁移**：I1 切换派生键后，`kg_nodes` 存量点需随下一次 build/kg-sync
   自然重建；切换前可对 `kg_nodes` 做一次 `delete_collection` 清库（数据皆可从图重建，
   无损失）。
7. **实体只增不删的长期健康**：已定义生命周期策略（§6.5）：边随文档走、实体按引用存
   活、孤儿周期清理，非无脑累积。
8. **`Other` 类型的同名误并（有界低频风险）**：`entity_id` 命名空间含 type，但 `Other`
   无区分度——两个语义不同、规范名恰好相同的实体若都被归为 `Other`，会在**第一级精
   确 MERGE** 被错误合并。对策（明确不采用某些方案，理由如下）：
   - **不采用**"提高 Other 的 cos 阈值"：碰撞发生在第一级（名字精确匹配），与第二级
     向量阈值无关，药不对症。
   - **不采用** `entity_id` 追加 `hash(summary)`：同一实体两次提及的 summary 措辞略不
     同就会产生不同 ID，合法去重被整体破坏，代价大于收益。
   - **采用**的缓解：① prompt 引导尽量归入具体词表类型、把 `Other` 当最后手段（压低
     `Other` 占比即压低碰撞面）；② 出错后走 §6.4 人工 Cypher 纠正（拆点、重建边）。
     该风险频率低、后果可逆，接受。

---

## 13. 实施进度（执行跟踪，2026-08-01 更新）

> 执行方式：superpowers subagent-driven-development；账本与简报/报告/评审包在
> `.superpowers/sdd/2026-07-31-universal-knowledge-pipeline-design/`。
> 基线：分支 `feat/v2-architecture`，工作树含用户 v2 重构未提交基线（实现叠加其上，
> 各任务只 `git add` 自己的文件）。测试基线：701+2 预存失败（`ts_java::parses_hello_service`、
> `backup_sqlite::copy_database_writes_file`，与本方案无关）。

### 13.1 任务总表

| 任务 | 内容 | 状态 | 提交 | 评审 |
|------|------|------|------|------|
| **S1** | Extract 抽取层：`ExtractedGraph` 模型 + `document_with_nlp.yaml` 重写 + llm/hanlp 处理器块级化 + R4 build.rs SF URL 修正 | ✅ 完成 | `0ebc13d` | Spec ✅ / 质量 Approved（无 Critical/Important，4 Minor 延后） |
| **S2a** | Consolidate 核心：consolidate.rs（两级消歧/写图/双写/purge/SAME_AS/I7 迁移）+ store.rs 薄壳重写（R8）+ R7 `search_with_filter` | ✅ 完成 | `98044df` + 修复轮 `6936cc0` | 修复轮 1 后 ALL ADDRESSED（1C+2I+5M 全部修复） |
| **S2b** | kg_bridge I1-I5（point_id 改 business_id 派生、payload 统一 schema、concat_props 数组、summary 不截断、删除接口）+ full_rebuild 清项目向量接线 | ✅ 完成（自审，待确认） | `ccb53e6` | 自审通过（无 subagent 评审，见 §13.5） |
| **S2c** | runner.rs 断言更新（R11：删 hanlp keyword 断言，Entity/RELATES/MENTIONED_IN 下界+抽样断言）+ expected.json + `dt build --test` 集成验证 | ✅ 完成 | `72798e3` | 控制者自审（同 S2b 模式） |
| **S3** | `process_documents` 接入 pipeline engine（含 deleted_paths 接 purge_document、旧 doc_chunks 写入摘除） | ✅ 完成（自审，用户指示无 subagent） | `647bddd` | 控制者自审（同 S2b 模式，见 §13.7） |
| **S4** | 删除 `@knowledge` 全链路 + store 老分支残留 + learn 停用 | ⬜ 待做 | — | — |
| 终审 | 全分支 code review（最 capable 模型）+ finishing-a-development-branch | ⬜ 待做 | — | — |
| **S5** | 检索层混合检索（向量召回+图扩展+rerank） | ⏸️ 方案本身延后，不在本轮 | — | — |

### 13.2 S1 验收数字（独立复验通过）

- ① JSON 解析成功率 **100%**（7 文档 68 块 0 降级，门槛 ≥90%）
- ② relation head/tail 覆盖率 **100%**（76/76，门槛 ≥95%）
- ③ 20 实体抽样人工核对 **≥80%** 达标
- 复现：`cargo test --test extract_real_docs -- --ignored`（本地 xinference qwen3.5）

### 13.3 执行期决策（用户批准 + 控制者裁决，后续任务必须遵守）

- **环境**：LLM=本地 xinference `qwen3.5`（已运行）；embed=SiliconFlow（key 取 `config/config.yaml.bak`，跑测试前 `export SILICONFLOW_API_KEY=...`，**不得写入被提交文件**）；Memgraph `bolt://localhost:7688`、Qdrant `:6334`；HanLP DOWN（链路须优雅降级）；bge-reranker-v2-m3 待 S5 再启动。
- **R1**：llm 文档路径输出同时含 `graphs` + `response`（旧 store 兼容），S2 后旧消费已移除。
- **R2**：hanlp 逐块对齐（block_index=chunk_index），matches 扩展 `yaml|yml|properties`，无 chunk 回退全文单块。
- **R4**：build.rs siliconflow 分支 base_url 改读 `providers.siliconflow.url`（修现存 bug，用户批准）。**旁注**：`build.rs:2147` handle_build 的 SF deps client 有同类 bug 未修，终审时上报用户。
- **R7**：`VectorRepository::search_with_filter` 默认后过滤 + QdrantRepo 原生覆写（向后兼容）。
- **R8**：store.rs 薄壳化，旧 tree_sitter/hanlp/llm-analysis 分支全移除（`{project}_entities` legacy 无消费方）。
- **R9**：store 输出计数 entities_merged/created、relations_written/orphaned、degraded_blocks/blocks_processed/empty_blocks。
- **R10**：S2 期间旧 `process_documents` 仍写 doc_chunks（chunk_id 为 point id）与新 Consolidate 并存——已知临时状态，S3 摘除。
- **R11**：expected.json 断言风格=下界（>=）+ 关键实体抽样存在性（LLM 非确定性，不做精确相等）。
- **doc_chunks payload 增补 `text` 字段**（§7.1 证据段落检索需要，方案 §7.3 未列，控制者裁决补充）。
- **关系端点历史回退**：`STARTS WITH dt://entity/{project}/` + `ENDS WITH /{normalized}`（评审后收窄；端点 type 未知无法精确派生）。
- **auto SAME_AS 当前不可达**（§6.1 二级合并不产生双节点），仅实现 manual 入口 Cypher，代码注释已声明。

### 13.4 已锁定的实现要点（S2a 落地版，与 §6 的偏差记录）

- `ensure_schema`（I7 迁移 + KG_NODES/DOC_CHUNKS ensure_collection）per-process OnceLock，**仅成功时闩锁**（瞬时故障下轮重试）。
- `SnapshotRepository` step 进度三方法（mark_step_done/is_step_done/clear_step_progress）在 traits.rs 带**默认实现**（no-op/false 安全回退）——基线工作树 SqliteRepo 有真实现。
- `block_map` 双键登记（原始+规范化 canonical），MENTIONED_IN/doc_chunks 收集处 HashSet 去重。
- confidence 存储前 f64 域内 6 位舍入（f32→f64 精度修正）。
- 降级/空摘要块 embed 文本=纯原文块。
- 测试：S1 后 727 → S2a 后 765 → S2b 后 **772 passed / 2 failed（预存，未扩大）**；clippy 0 error。

### 13.5 S2b 落地要点与偏差记录（2026-08-01，会话内直接实现）

- **I1**：`business_id(node)` 派生序 = 21 个显式 id 属性（entity_id/knowledge_id/concept_id/…）
  → `name@namespace|db`（K8sDeployment/Table/ConfigKey 复合键）→ element_id 兜底。
  图重建后同业务节点回写同一向量点，幂等。
- **I2**：payload 统一核心 schema 与 §7.2 一致；两处有意偏差：① `description` 保留为
  `summary` 的完整文本**别名**（retriever.rs/search_mcp.rs 现读它，兼容期双写，S5 后可摘）；
  ② `origin` 缺省 `"learned"`（业务节点主入口为 learn/memorize 流）。
- **I5**：`delete_kg_vector(vector, business_id)` 经 `delete_by_filter(business_id payload)`
  实现——trait 本无 `delete_points(ids)` 方法，避免新增 trait 方法引发 8 处 mock 连锁改动；
  I2 前的 legacy 点无 `business_id` key 删不掉，按 §12.6 一次性清 `kg_nodes` 处理。
- **full_rebuild**：`prepare()` 先 `delete_by_filter(project=...)` 清 `KG_NODES`+`DOC_CHUNKS`
  （失败 warn 不致命）；pipeline.rs 调用点从 `None` 接通 `vector.as_deref()`。
  `code_methods` 全局集合**不在**清理范围（基线行为：确定性 id 覆写）。
- **自审说明**：应用户要求本任务未派 subagent 评审；控制者自审覆盖：filter 形状一致性、
  BUSINESS_LABELS 过滤前提、双写兼容、删除幂等。遗留噪音：提交含基线未提交内容 + 全量
  fmt 重排（同 2a deferred minor，因改动依赖同文件基线内容无法拆分）。

### 13.6 S2c 完成记录（2026-08-01，R11 断言落地）

- **断言重写**：runner.rs Step 8 由 HanLP keyword 精确相等断言（§10.1 已摘 HanLP 链路）
  整体替换为 `verify_knowledge_graph`：① Entity/RELATES/MENTIONED_IN 下界计数（`>=`）；
  ② Entity（§7.2：entity_id/name/type/summary/keywords/aliases）与 RELATES 边
  （type/doc_id/evidence/confidence）字段形状抽样；③ `sample_entities` 抽样存在性
  （name/entity_id 大小写不敏感 CONTAINS，presence-only 不断言 type，容忍 LLM 类型漂移）；
  ④ `kg_nodes`（§7.2）/ `doc_chunks`（§7.3）向量 payload 字段检查。
- **关键缺陷修复（断言采样偏差）**：`point_payload` 原实现从零向量全集合采样 `kg_nodes`，
  会非确定性命中旧 kg-sync 点（`elementId`/`description`/`source` 格式，3945 点中仅 281
  点属 test-pipeline 新格式）→ §7.2 字段断言假失败（首次运行 failed=1 即此因）。
  修复：采样加 `project=test-pipeline` 过滤（走 R7 `search_with_filter` Qdrant 原生过滤）。
  Consolidate 写入侧本身正确，新格式点字段齐全（business_id/origin=extracted/summary/labels）。
- **expected.json 校准**：首轮实测 Entity 265 / RELATES 74 / MENTIONED_IN 274，下界取
  ~55-60%（150/40/150）——回归检测导向（抽取链路整体崩坏 vs 正常工作），容忍 LLM 抽取
  数量漂移；抽样实体 3 个：aria2c / channelextra / paychannelservice.createpay。
- **集成验证（全量干净重建）**：`dt clean --test && dt build --test`（xinference qwen3.5，
  HanLP DOWN 优雅降级，28 文件全成功）→ **118 total / 117 passed / 0 failed / 1 skipped**
  （skip=llm_analysis 内容检查，expected 标记 `has_llm_analysis_on_methods=false`，有意）。
  独立二轮复跑实测 263/58/270——RELATES 漂移最大（74→58，-22%），下界仍成立，
  抽样实体全部复现，校准余量设计有效。

### 13.7 S3 完成记录（2026-08-01，会话内直接实现——用户指示无 subagent）

- **旧路径摘除（R10 收尾）**：`process_documents`/`write_document_to_graph`/
  `write_chunk_to_graph`/`DocumentItem` 整体删除（§10.2 标"改造"，实际无残留职责——
  engine 已覆盖文档抽取链）：chunk+embed doc_chunks 双写、Document/DocumentChunk 图
  节点（全库无消费方）、文档 @knowledge 提取一并移除；`PipelineTemplate` 只保留文档
  **生命周期**职责。
- **deleted_paths → purge_document（§6.5.2 接线）**：build/pipeline.rs 新增 Step 3b——
  strategy 对 doc_files 再选一次（deleted 含 code/doc 混存快照表污染，按
  `document_extensions` 过滤）→ `purge_document`（提为 consolidate.rs 自由函数，
  `&dyn` 注入、无需 embed；Consolidator 方法委托）→ **成功 purge 者**才
  `delete_file_progress` 清快照+步骤进度（失败保留下轮重报，自审修复）。Step 9b
  保存变更文档快照基线（置于 Step 9 后——FullRebuildStrategy.update_snapshots 先
  delete_project 全清，顺序错误会丢基线）。
- **doc_id 归一 rel 形态**：engine 旧产物为绝对路径（`dt://doc/{proj}//data/...`，
  实测同文档 rel+abs 双 Document 节点）；build.rs engine 输入/skip 键/成功匹配三处
  改项目相对路径，chunk.rs 与 purge 统一 `domain::id::make_document_id`。
  遗留影响：其他生产项目的 abs 形态垃圾经 `dt build --full` 自愈（prepare 清项目
  向量 + §6.5.1 入口清边），abs Document 节点可一次性 Cypher 清除
  （`... doc_id STARTS WITH 'dt://doc/{project}//' DETACH DELETE d`）。
- **新增 trait 方法**：`SnapshotRepository::delete_file_progress`（默认 no-op 防 mock
  连锁——同 §13.4 step 进度先例；SqliteRepo/MemorySnapshotRepo 真实现，
  file_snapshots+pipeline_progress 两表）——修复"删除后同内容重建被陈旧步骤进度
  跳过"；PDF 文档不再有任何处理（engine skip_ext 含 pdf），删除 purge 仍生效。
- **验收数字**：`cargo test` 775 passed / 2 failed（预存，未扩大，新增 3 项单测）；
  clippy 0 error；`dt clean --test && dt build --test` → **117/0 failed/1 skipped**；
  重复 build 实体**零漂移**（271/62/280/12 完全一致）；删除
  `redis-cache-pitfall.md` → Document/MENTIONED_IN(7)/RELATES(4)/doc_chunks 点/
  快照/步骤进度全部清零 → 恢复文件 → 1 文件重抽（不被陈旧进度跳过）→ 计数复原。
- **旁注（out-of-scope，终审上报）**：①代码文件删除有同类陈旧进度问题（恢复同内容
  文件 → Method 节点丢失直至内容变更，可用同一 `delete_file_progress` 修）；②gRPC
  BuildServiceImpl 路径无 engine，文档仅生命周期无抽取（既有状态）。
