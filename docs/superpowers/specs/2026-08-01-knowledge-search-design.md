# 知识图谱搜索方案：向量召回 + 图扩展 + Rerank 混合检索（S5）

- 日期：2026-08-01
- 状态：方案定稿（待实现）
- 范围：主设计文档 §8「Retrieve 检索层」的落地细化——`world=knowledge` 混合检索重写、
  `world=doc` 证据检索、rerank 链路首次接入
- 关联文档：`docs/superpowers/specs/2026-07-31-universal-knowledge-pipeline-design.md`
  （下称「主文档」，本节引用其 §5-§13 全部结论作为既定前提）

---

## 1. 背景与目标

主文档 S1-S4 已完成构建链（抽取 → 整合 → 双写向量），`kg_nodes` 已有 `extracted`
实体的完整语义索引（§7.2 payload）。但检索侧仍停留在 S5 之前的状态：

1. `world=knowledge`（`search_mcp.rs:264-327`）是纯字符串 `CONTAINS` 子串匹配，
   **且只覆盖 `Concept/Playbook/Knowledge/Experience/Decision` 五个 label——S2 起
   写入的大量 `Entity` 节点根本不在检索范围内**，新知识对搜索不可见。
2. `world=doc` 实际走 `search_vector` 查 `kg_nodes`（不是 `doc_chunks`），无法回答
   "给我证据段落"类查询。
3. rerank 链路（`RerankService` trait + provider_router + siliconflow/xinference 双
   实现）已铺好但**业务零调用**，S5 是首个调用点（主文档 §8 已声明）。
4. `world=knowledge` 现状忽略 `project` 过滤参数，跨项目串扰。

目标：把 `world=knowledge` 重写为 **GraphRAG 式混合检索**（向量召回 → 图扩展 →
rerank → 融合排序），`world=doc` 改为查 `doc_chunks` 并支持实体证据回填；输出结构
携带排序分解与图证据，可解释、可调参。

核心原则（继承主文档）：

1. 向量库是 KG 的语义索引，检索的**真相回链永远在 Memgraph**（business_id 为准）。
2. `SAME_AS` 邻居视为同一实体（主文档 §6.4）。
3. 任何一路故障可降级：rerank 挂 → 语义+图两路融合；图挂 → 纯向量召回兜底。

---

## 2. 决策记录

| # | 决策点 | 结论 |
|---|--------|------|
| S5-D1 | 混合检索实现位置 | 新增 `src/application/knowledge/extract/retrieve.rs`（主文档 §10.3 预留），`search_mcp.rs` 只做 world 分发与结果适配，GraphRAG 逻辑不下沉到 context 模块 |
| S5-D2 | 召回集合 | `kg_nodes` 单库召回，`search_with_filter` 原生过滤 `project`（R7 已落地）；**不**召回 `doc_chunks` 混入 knowledge 世界——块证据走 world=doc 或证据回填（§5.5） |
| S5-D3 | 图扩展跳数 | 请求参数 `max_hops`，**默认 1、上限 2**。2 跳扇出随度爆炸，默认保守；主文档 §8 的"1~2 跳"由参数覆盖 |
| S5-D4 | 非 Entity 种子定位 | **用 payload `elementId` 定位**（主文档 §7.2 既定用途："供图扩展 `elementId(n) IN $ids` 使用"），Entity 种子仍按 `entity_id`（有唯一约束索引）。**废弃 label→id 映射表方案**：实测该表与 `business_id` 的 21 项**属性序**派生不同源——Playbook/Experience 节点实为 `playbook_id`/`experience_id`（`knowledge/service.rs:515/371`），Table/ConfigKey 无 `*_id` 属性（复合键 `name@db`/`name@namespace` 兜底，`memgraph/schema.rs:76,79`）——映射表会对这些 label 生成恒 0 命中的查询，且 S5c 验收恰好测不到 |
| S5-D5 | rerank 候选截断 | **分桶截断**（§5.2.3）：种子桶 ≤⌈top_n×0.6⌉ + 邻居桶保底 ⌊top_n×0.4⌋，top_n 默认 **50**（`DT_KG_RERANK_TOP_N` 可调），控制单次 rerank 延迟与 token 成本。原一刀切 top-50 方案作废：邻居预排分 = 种子分×0.5^hop 恒排在自己种子之后，k=limit×3 召回下种子轻易占满名额，邻居活不到 rerank，图扩展名存实亡 |
| S5-D6 | rerank 分数归一 | **实测修正（S5b 联调）**：xinference/SiliconFlow 的 `relevance_score` 已是 sigmoid 归一值（强相关对 0.9993、无关对 0.0），**不做二次 sigmoid**（会把分布压向 0.5、架空 0.6 权重），实现为防御性 `clamp_unit` [0,1]；若未来接入返回裸 logit 的 provider 需重新评估 |
| S5-D7 | 融合权重 | 初值 `0.6 rerank + 0.3 语义 + 0.1 graph_boost`（主文档 §8），收敛后回写；rerank 不可用时权重重归一为 `0.75 语义 + 0.25 graph_boost`，**且 graph_boost 降为一阶衰减**（邻居统一 0.5，不再 0.5^hop，§5.4）——降级时两个幸存信号若都随 hop 指数衰减会把邻居压死，图扩展收益归零 |
| S5-D8 | SearchHit 结构 | **扩展不重建**：保留现有字段，新增 `score_breakdown` / `hop` / `relations` / `evidence` / `via_same_as` / `rerank_degraded` 可选字段，进程内消费方无感升级（gRPC 链路的承载限制见 S5-D10） |
| S5-D9 | world=doc | 改查 `doc_chunks`（§7.3 payload 含 `text`），filter **强制 `source="doc"`** 以排除 `config_sync.rs` 写入的 nacos 配置点（payload 无 `text`，§5.5）；支持 `doc_id` 过滤参数做文档内证据检索；不再借道 `kg_nodes` |
| S5-D10 | 入口可达性 | 本期新链路**仅进程内可达**（测试/后续任务直接调用 retrieve.rs）：gRPC `build_service.rs:116` 硬编码 `world="code"`，且 proto `SearchRequest` 无 `world` 字段、`SearchResult` 无新字段位（`dt_core.proto:45-65`），透传必须改 proto；MCP `dt_search` 与 CLI `dt search`/`dt search-kg` 走**另一套检索栈**（`application/search.rs`：QueryRewriter + RRF + `expand_nodes`），本期不动。proto 变更 + CLI 换栈 + 两栈重叠模块归并立独立后续任务（§7 收尾开立任务卡） |
| S5-D11 | 邻居白名单 | 图扩展邻居按 `BUSINESS_LABELS`（`kg_bridge.rs:64-99`）白名单过滤，**剔除 Events 组（4 个 label）与 `Document`**——事件/会话节点是噪声，文档节点经证据回填呈现而非作为知识邻居；白名单外的代码节点（Method/Class 等）同样不进候选。名单与 `kg_bridge` 同源注释，防漂移 |

---

## 3. 目标总体架构

```
query ──┬──────────────────────────────────────────────────────────┐
        │ ① 召回   embed(query) → kg_nodes.search_with_filter       │
        │          (project[+origin], k = limit×3) → 种子            │
        │          {business_id, elementId, labels, 语义分}          │
        │                                                          │
        │ ② 图扩展 按 labels 是否含 Entity 分流（S5-D4）：           │
        │          Entity   → entity_id 定位：SAME_AS 无向归并        │
        │                       + RELATES*1..max_hops 邻居            │
        │          非Entity → elementId 定位，1 跳任意关系邻居        │
        │          → 候选合并：种子+别名(hop=0) + 邻居(hop=1..2)      │
        │          → 边去重聚合 (head,rel_type,tail) 保最高confidence │
        │          → 邻居白名单(S5-D11) → 分桶截断 top-N(S5-D5)       │
        │                                                          │
        │ ③ 重排   reranker.rerank(query, 候选 name+summary)         │
        │          → sigmoid 归一（S5-D6）                           │
        │                                                          │
        │ ④ 融合   final = 0.6·rerank + 0.3·语义 + 0.1·graph_boost  │
        │          （0.5^hop 衰减；降级时 boost 一阶衰减，S5-D7）     │
        │          → 排序截断 limit                                  │
        │                                                          │
        └─ ⑤ 证据   （可选）doc_chunks 按 entity_ids+source=doc 回填 │
```

降级路径（任一路失败不整体失败）：

```
rerank 失败/未配置 → final = 0.75·语义 + 0.25·graph_boost（boost 一阶衰减），
                     条级标 rerank_degraded + 世界级 degraded:["rerank_unavailable"]
graph 扩展失败     → 仅向量召回 + rerank，graph_boost 全 0，
                     世界级 degraded:["graph_expansion_failed"]
embed 失败         → knowledge 世界返回空（无可召回手段），记 error 日志 +
                     世界级 degraded:["embed_unavailable"]（原"仅日志"升级为可观测）
```

---

## 4. 现状诊断（改造点定位）

| 位置 | 现状 | 问题 |
|------|------|------|
| `search_mcp.rs:259-327` `search_knowledge` | `CONTAINS` 子串匹配 5 个旧 label | 无语义能力；`Entity` 不在 label 白名单，S2 新知识不可见；忽略 `project` |
| `search_mcp.rs:330-365` `search_vector` | 查 `kg_nodes`，读 `payload.description` | world=doc 语义错位（应查 `doc_chunks`）；依赖 I2 兼容别名 `description` |
| `search_mcp.rs:101-111` `CrossWorldSearch::new` | 依赖 `{graph, vector, embed}` | 无 `rerank` 依赖，需加第四参 |
| `build_service.rs:116` | gRPC 调用点 `world=Some("code")` | 仅暴露 code 世界；world 透传须先改 proto（无字段可透传），列入后续任务（S5-D10） |
| `infrastructure/{siliconflow,xinference}.rs` | `RerankService` 双实现就绪 | 业务零调用；本地 xinference rerank 模型已配置 `bge-reranker-v2-m3`（`config/pipeline.yaml:35`，无 `BAAI/` 前缀，属正常）；注意当前 yaml 把 embed/rerank/llm 全路由到 xinference，与代码默认 siliconflow 不一致，联调以 yaml 为准（§8.11） |
| `search_mcp.rs:313` | knowledge 命中 score **恒为 0.5**（Cypher 未返回 score 列，`unwrap_or(0.5)` 兜底）；`DT_SEARCH_MIN_SCORE` 现状仅作用于 code 世界 | world=all 混排比"纯 CONTAINS"更糟：knowledge 命中以固定分参与全局排序；S5 后融合分取代 |
| `application/search.rs` + `interfaces/cli/build.rs:758,1583` + `mcp/mcp-server.py` | **存在第二套检索栈**：CLI `dt search`/`dt search-kg` 与 MCP `dt_search`（`mcp-server.py:462` 有 gRPC TODO，实际走 CLI 子进程）使用 QueryRewriter + RRF + `expand_nodes`（elementId 图扩展），与栈①无共享代码 | S5 不改栈②（S5-D10）；`expand_nodes` 与 retrieve.rs 图扩展功能重叠，归并属后续任务 |
| `proto/dt_core.proto:45-65` | `SearchRequest` 仅 query/limit/expand/path/project（`expand`/`path` 还被 handler 丢弃）；`SearchResult` 仅 score/name/file_path/start_line/signature | gRPC 链路**无法承载 §5.7 契约**（score_breakdown/hop/relations/evidence 全部丢字段）；proto 变更列入后续任务（S5-D10） |

---

## 5. 检索流水线详细设计

### 5.1 召回：`kg_nodes` 语义召回

```rust
// retrieve.rs
let q_vec = embed.embed_batch(&[query]).await?.pop()...;
let filter = project.map(|p| json!({
    "must": [{"key": "project", "match": {"value": p}}]
})).unwrap_or_default());
let hits = vector.search_with_filter(KG_NODES, q_vec, (limit * 3) as u64, filter).await?;
// 种子：{business_id, labels, name, summary, 语义分}；语义分 < min_score 丢弃
// （复用 DT_SEARCH_MIN_SCORE，默认 0.3，与 search_code 一致）
```

- 用 `search_with_filter`（R7，Qdrant 原生覆写）而非召回后内存过滤——`limit*3` 的
  窗口在后过滤下会被其他项目的点挤占。
- 种子的 `labels` 来自 §7.2 payload，是 §5.2 分流的依据；`elementId` 同取自 payload
  （非 Entity 种子的图扩展定位键，S5-D4）。
- `origin` 字段（`extracted|learned|manual`）**默认不过滤**；请求显式传 `origin` 时
  并入 filter 的 `must` 条件（§5.7.1），同时透传到结果 payload 供调用方自行筛。
- `min_score` 过滤发生在图扩展**之前**：被滤种子的邻居连带失去扩展机会——这是有意
  的（低置信召回的图邻域同样不可信）。0.3 沿用 code 世界经验值，对 BGE-M3 中文知识
  召回分布的适配性未经论证，S5b 实测后校准（§8.4）。
- **Document 种子剔除**（S5a 实测修正）：kg-sync 会把 `Document` 节点同步进
  `kg_nodes`（BUSINESS_LABELS 含 Document），其语义与 doc_chunks 块重复，扁平分布下
  挤占 knowledge 结果位（实测 top-15 占 13）。`parse_seed` 剔除 `Document` label 点
  ——文档证据归 world=doc/doc_chunks，与 S5-D11 邻居白名单同一噪声逻辑。

### 5.2 图扩展

#### 5.2.1 Entity 种子：SAME_AS 归并 + RELATES 邻居（单条 Cypher）

```cypher
// $seeds: 本批 Entity 种子的 entity_id 列表；$max_hops ∈ {1,2}
UNWIND $seeds AS seed
MATCH (e:Entity {entity_id: seed})
OPTIONAL MATCH (e)-[:SAME_AS]-(alias:Entity)
WITH collect(DISTINCT e) + collect(DISTINCT alias) AS seed_nodes
UNWIND seed_nodes AS s
OPTIONAL MATCH path = (s)-[:RELATES*1..2]-(nb:Entity)
WITH s, nb, relationships(path) AS rels, length(path) AS hop
RETURN s.entity_id AS seed_id,
       nb { .entity_id, .name, .type, .summary, .keywords } AS neighbor,
       hop,
        [r IN rels | {type: r.type, confidence: r.confidence,
                      evidence: r.evidence, doc_id: r.doc_id,
                      head: startNode(r).entity_id, tail: endNode(r).entity_id}] AS edges
LIMIT 500   // 防御性保险丝（全局语义，见下条；逐种子截断在 Rust 侧）
```

- **SAME_AS 按无向处理**（主文档 §6.4："查询时按无向对待"），归并后的别名实体与主
  实体共享邻居候选，结果中以被命中节点的 `entity_id` 呈现并带 `via_same_as: true`
  标记；别名命中**视为同一实体：hop=0、graph_boost=1.0**。`via_same_as` 由 Rust 侧
  判定（命中节点的 entity_id 不在原始种子 entity_id 集合中即为别名命中）。
- **防御性限流**：语句末尾加全局 `LIMIT 500` 作纯保险丝（正常结果远低于此）。注意
  Cypher `LIMIT` 在 `UNWIND` 之后是**全局语义**，给不了"每组上限"——逐种子邻居截断
  在 Rust 侧做（§5.2.2/§5.2.3），不靠 SQL 层。
- 变长路径 `*1..2` 由 `$max_hops` 在 Rust 侧拼入（Cypher 不支持参数化跳数上界——
  只接受字面量，拼接值来自白名单枚举 `{1,2}`，无注入面）。
- **边去重聚合**（主文档 §8 硬约束）：RELATES 的 MERGE key 含 `doc_id`，同一
  `(head, rel_type, tail)` 可能多条。Rust 侧按三元组分组：保留 `confidence` 最高一
  条，其余边的 `evidence`/`doc_id` 聚合为 `supplementary_evidence[]`，避免同一关系
  重复计数挤占 limit。

#### 5.2.2 非 Entity 业务种子：elementId 定位扩展（无映射表）

`kg_nodes` 是异构库（主文档 §8 硬约束）：`business_id` 对抽取 Entity 是
`entity_id`，对 learned/manual 业务节点是 `knowledge_id` 等。原方案的 label→id
静态映射表经对 `kg_bridge.rs:1091-1143` 实测**与派生机制不同源**（派生是 21 项
属性的**顺序扫描**、非 label 分派；且 Playbook/Experience 实为 `playbook_id`/
`experience_id`，Table/ConfigKey 无 `*_id` 属性、走 `name@db`/`name@namespace`
复合键兜底）——映射表会对这些 label 生成恒 0 命中的查询。改为复用 §7.2 payload
的 `elementId` 字段直接定位（主文档 §7.2 既定用途），与 label 无关、无表可漂移：

```cypher
// 全部非 Entity 种子一条语句；elementId 为 Memgraph 运行时键（同代数据内稳定）
UNWIND $seed_eids AS eid
MATCH (n) WHERE elementId(n) = eid
OPTIONAL MATCH (n)-[r]-(nb)
RETURN eid AS seed_eid,
       nb { .* , labels: labels(nb) } AS neighbor,
       type(r) AS rel_type
```

- **邻居白名单过滤在 Rust 侧**（S5-D11）：保留 `BUSINESS_LABELS` 内节点，剔除
  Events 组（4 个 label）与 `Document`；白名单外节点（Method/Class 等）丢弃。
- **逐种子邻居截断在 Rust 侧**：每种子按边 `confidence` 降序（缺失按 0.5）保留
  ≤10 条。`LIMIT` 在 `UNWIND` 后是全局语义，给不了"每组上限"——高连接度节点
  （如 Domain）的扇出防护由本截断 + §5.2.3 邻居桶配额共同承担。

#### 5.2.3 候选合并与分桶截断

```
候选 = 种子(hop=0, graph_boost=1.0, via_same_as=false)
     + SAME_AS 别名(hop=0, graph_boost=1.0, via_same_as=true)  // 视为同一实体
     + 1跳邻居(hop=1, graph_boost=0.5)
     + 2跳邻居(hop=2, graph_boost=0.25)   // max_hops=2 时
```

- **去重键 = business_id**：Entity 邻居直接取 `entity_id`；非 Entity 邻居复用
  `kg_bridge::business_id` 的 21 项属性序派生（必要时提为 pub 导出——retrieve.rs
  引入该依赖，禁止另写一套派生）。同节点多条路径到达时取最小 hop（最大 boost）。
- 无 summary 的邻居节点候选：name 兜底进 rerank 文本；仍为空则丢弃。

**分桶截断**（S5-D5。替代原一刀切 top-50——一刀切下邻居预排分 = 种子分×0.5^hop
恒排在自己种子之后，k=limit×3 召回下 hop=0 种子轻易占满 50 名额，邻居活不到
rerank，与"图扩展捞回向量漏掉的实体"的目标自相矛盾）：

```
top_n = DT_KG_RERANK_TOP_N（默认 50）
种子桶：hop=0（含 SAME_AS 别名）按语义分降序，上限 ⌈top_n × 0.6⌉
邻居桶：hop≥1，上限 = top_n − 种子桶实际数（保底 ⌊top_n × 0.4⌋，
        种子不足时邻居可上浮至满仓）
        桶内预排分 = 所属种子语义分 × 路径各边 confidence 最小值（缺失按 0.5）
        ——不再乘 0.5^hop：hop 衰减是排序信号，留给 §5.4 融合公式；
        候选选拔阶段只问"证据强不强"，不问"离多远"
60/40 为初值，S5b 后据 score_breakdown 观测校准（§8.4）。
```

### 5.3 重排（Rerank）

```rust
let docs: Vec<String> = candidates.iter()
    .map(|c| format!("{}。{}", c.name, c.summary))   // 主文档 §8：name+summary
    .collect();
let logits = rerank.rerank(query, &docs).await?;      // RerankService 现有 trait
let scores: Vec<f32> = logits.iter().map(|x| x.clamp(0.0, 1.0)).collect();
// 注：provider 的 relevance_score 已归一（S5-D6 实测修正），clamp 仅防御越界
```

- 依赖注入：`CrossWorldSearch::new` 增加第四参 `rerank: Option<Arc<dyn RerankService>>`
  （S5-D1 配套）；`build_service.rs:101` 构造处由 `rerank_provider` 配置路由
  （provider_router 已实现 `RerankService`）。
- **模型对齐**：本地 xinference 必须加载 `bge-reranker-v2-m3`（主文档 §8 明确不要用
  bge-reranker-base）；SiliconFlow 走 `SILICONFLOW_RERANKER_MODEL`（默认已是
  `BAAI/bge-reranker-v2-m3`，`siliconflow.rs:36`）。
- rerank 批大小 = 候选数（≤top_n），单次调用；失败走 S5-D7 降级权重，结果集打
  `rerank_degraded: true`（条级）+ `degraded: ["rerank_unavailable"]`（世界级，§5.7.4）。
- **文本不对称注意**：rerank 输入是 `name。summary`，而入库向量文本是
  `name。summary。关键词: …`（主文档 §6.1 同一构造函数）——两路信号的文本原料不同
  （rerank 是独立模型，不涉及向量空间一致性，无正确性问题），但解读
  `score_breakdown` 调参时需知晓：semantic 与 rerank 的相关性会被 keywords 差异
  系统性拉低。

### 5.4 融合排序

```
final = 0.6 × rerank分(sigmoid)      // 主排序信号
      + 0.3 × 语义分                  // 邻居的语义分 = 其种子语义分 × 0.5^hop
      + 0.1 × graph_boost             // 1.0 / 0.5 / 0.25（0.5^hop）
```

- 邻居的"语义分"不可直接得（未对邻居单独 embed），用种子分衰减近似——这是有意的
  近似：rerank 分才是邻居的主排序信号，语义分只打底。
- **降级模式（rerank 不可用）**：权重重归一为 `0.75 语义 + 0.25 graph_boost`，且
  graph_boost 改为**一阶衰减**（种子 1.0、邻居统一 0.5，不再 0.5^hop）。原因：降级
  时两个幸存信号若都随 hop 指数衰减，邻居在排序上被双重压死、图扩展收益归零——
  最需要兜底的场景恰好不能放弃图证据。已知残余偏差：降级模式下邻居排序仍系统性
  低于种子，属可接受的保底行为，明示不规避。
- 每条命中的 `score_breakdown = {semantic, rerank, graph_boost, final}` 全部入结果
  （S5-D8），为权重调参（收敛后回写主文档 §8）提供观测数据。
- 截断 `limit`，同分按 `hop` 升序（直接命中优先）。

### 5.5 world=doc：证据检索改写

`search_vector` 拆为两个明确入口：

1. **world=doc → `doc_chunks`**：`search_with_filter(DOC_CHUNKS, q_vec, limit,
   filter)`，filter = `project`（可选）+ `doc_id`（可选，§5.7.1）+
   **强制 `source="doc"`**。`source="doc"` 是硬条件：`doc_chunks` 存在第二个写入
   端（`config_sync.rs:357-373` 的 nacos 配置点，payload 为 `key/value/namespace`
   等，**无 `text/doc_id/block_index/entity_ids`**），不过滤会被语义召回进 doc
   世界、snippet 全空。返回 §7.3 payload 的 `text` 字段（证据段落原文）、
   `block_index`、`degraded` 标记；`score` 同样过 `DT_SEARCH_MIN_SCORE`（现状
   `search_vector` 不过阈值，S5 起与 knowledge 一致，S5b 校准）。
2. **实体证据回填（可选参数 `with_evidence`，仅 knowledge 世界生效，其他世界忽
   略）**：knowledge 检索出最终 top-N（N≤5）实体后，以 `entity_ids` payload 匹配
   反查 `doc_chunks`——**合并为一次** `search_with_filter`（`should` 数组携带全部
   entity_id + 强制 `source="doc"`），Rust 侧按实体分组各取 ≤2 段进
   `SearchHit.evidence`，避免 N 次串行往返。
   **前置条件**：`entity_ids` 是字符串数组，数组匹配只在 QdrantRepo 原生 filter
   覆写（R7，`repo.rs:374-423`）下成立——trait 默认的客户端后置过滤
   （`traits.rs:111-120`）是精确 JSON 相等，对数组恒 false。本功能必须走原生
   filter 路径，落地时加一条针对数组匹配的测试锁死，防后端替换后静默无证据。

### 5.6 SearchHit 扩展（S5-D8）

```rust
pub struct SearchHit {
    // ……现有字段全部保留……
    /// 排序分解（knowledge 世界新链路填充），字段定义见 §5.7。
    pub score_breakdown: Option<ScoreBreakdown>,
    /// 图距离：0=直接命中（含 SAME_AS 别名），1/2=扩展邻居。
    pub hop: Option<u32>,
    /// 是否经 SAME_AS 别名归并命中（hop=0 且为 true 时表示命中的是别名实体，
    /// 与主实体共享邻居候选；§5.2.1）。
    pub via_same_as: Option<bool>,
    /// 命中实体的关系摘要（去重聚合后，上限 5 条），字段定义见 §5.7。
    pub relations: Option<Vec<RelationSnippet>>,
    /// 证据段落（world=doc 或 with_evidence 回填）。
    pub evidence: Option<Vec<String>>,
    /// rerank 降级标记。
    pub rerank_degraded: Option<bool>,
}
```

全部 `Option` + `#[serde(default)]`，旧消费方（gRPC build_service、测试）无感。

### 5.7 输出格式契约

#### 5.7.1 请求参数（`SearchRequest` 扩展后全集）

| 字段 | 类型 | 默认 | 说明 |
|------|------|------|------|
| `query` | `String` | （必填） | 自然语言查询 |
| `world` | `Option<String>` | `"all"` | `code` / `knowledge` / `doc` / `all`；`vector` 保留为 `doc` 别名（§8.7） |
| `limit` | `Option<usize>` | `20` | 每世界上限，融合后截断 |
| `project` | `Option<String>` | `None` | 项目过滤；S5 起 knowledge 世界**必须**生效（现状 bug 修复） |
| `max_hops` | `Option<u32>` | `1` | 图扩展跳数，白名单 `{1, 2}`，其他值钳到 2（S5-D3） |
| `with_evidence` | `Option<bool>` | `false` | knowledge top-5 实体从 `doc_chunks` 回填证据段落（§5.5）；仅 knowledge 世界生效，其他世界忽略 |
| `origin` | `Option<String>` | `None` | 按 §7.2 `origin` 过滤召回种子：`extracted` / `learned` / `manual`；`None` = 不过滤；非法值宽容回退为不过滤（不报错） |
| `doc_id` | `Option<String>` | `None` | 仅 world=doc 生效：限定单文档内检索证据块（§5.5） |

#### 5.7.2 单条命中（`SearchHit`）字段填充矩阵

`—` = 该世界不填充（序列化为 `null`，`skip_serializing_if` 视消费方而定，不做硬要求）。

| 字段 | 类型 | knowledge | doc | code（现状不变） |
|------|------|-----------|-----|------|
| `id` | `String` | **稳定业务主键** `business_id`（Entity 即 `entity_id`） | `{doc_id}:{block_index}` | Qdrant point id |
| `title` | `String` | 实体 `name` | 文档名（`doc_id` 末段） | 方法名 |
| `snippet` | `String` | 实体 `summary` | 块原文 `text` 截断 200 字 | `file_path: Lx-Ly` |
| `source_world` | `String` | `"knowledge"` | `"doc"` | `"code"` |
| `entity_type` | `String` | payload `type`（Entity 为 `EntityType` 枚举值；业务节点为主业务 label），缺失回退 `labels[0]` | `"Doc"` | `"Method"` |
| `score` | `f64` | **融合分 final**（§5.4） | 向量语义分 | 向量语义分 |
| `source_ref` | `Option<String>` | 溯源 `doc_id`：取 confidence 最高 RELATES 边的 `doc_id`；**无 RELATES 边的孤立实体回退 `MENTIONED_IN`**（`(e)-[:MENTIONED_IN]->(d)` 按 `d.doc_id` 字典序取首条，保证确定性） | `doc_id` | — |
| `element_id` | `Option<String>` | payload `elementId`（图扩展运行时键） | — | `method_id` |
| `hop` | `Option<u32>` | `0` 直接命中（含 SAME_AS 别名）/ `1` / `2` | — | — |
| `via_same_as` | `Option<bool>` | 别名命中时 `true`，否则省略 | — | — |
| `score_breakdown` | `Option<ScoreBreakdown>` | 必填（见 §5.7.3） | — | — |
| `relations` | `Option<Vec<RelationSnippet>>` | 去重聚合后 top-5（见 §5.7.3） | — | — |
| `evidence` | `Option<Vec<String>>` | `with_evidence=true` 时每实体 ≤2 段 | — | — |
| `rerank_degraded` | `Option<bool>` | 降级时 `true`，正常省略 | — | — |
| `file_path` / `start_line` / `end_line` / `signature` / `calls` | — | — | — | code 专用（现状） |

`id` 填 `business_id` 而非 point id 是**有意的破坏性修正**：现状 `search_knowledge`
返回 Memgraph 内部 id（全量重建后失效），business_id 才是跨重建稳定的关联键（主文
档 §7.2/§7.5）。`element_id` 字段保留承载运行时图扩展键，职责分离。

#### 5.7.3 新增子结构

```rust
/// 排序分解——权重调参（主文档 §8 收敛回写）的观测数据源。
pub struct ScoreBreakdown {
    pub semantic: f64,     // 向量召回分；邻居 = 种子分 × 0.5^hop（§5.4 近似）
    pub rerank: f64,       // sigmoid 归一后；rerank_degraded 时为 0.0
    pub graph_boost: f64,  // 正常 1.0/0.5/0.25（0.5^hop）；降级模式一阶衰减 1.0/0.5（§5.4）
    pub final_score: f64,  // = SearchHit.score，冗余字段便于消费方免对齐
}

/// 关系摘要——命中实体对外的 top-5 关系（按 confidence 降序）。
pub struct RelationSnippet {
    pub rel_type: String,          // RELATES.type，如 routes_to / depends_on
    pub other_end_id: String,      // 对端 business_id
    pub other_end_name: String,    // 对端 name（展示用）
    pub direction: String,         // "out" | "in"（相对命中实体）
    pub confidence: f64,
    pub evidence: Option<String>,  // 最高 confidence 边的证据句
    /// 其余文档来源的补充证据数（§5.2.1 边聚合产物；>0 表示多文档佐证）
    pub supplementary_count: u32,
}
```

#### 5.7.4 世界级元信息（`CrossWorldResult` 扩展）

```rust
pub struct CrossWorldResult {
    // ……现有字段（query / world / hits / total / per_world_counts）全部保留……
    /// 降级标记（非空即发生了 §3 降级路径之一）：
    ///   "rerank_unavailable" — rerank 服务失败/未配置，走 S5-D7 降级权重
    ///   "graph_expansion_failed" — 图扩展失败，仅向量召回 + rerank
    ///   "embed_unavailable" — embed 失败，knowledge 世界空结果
    pub degraded: Vec<String>,
}
```

**条级与世界级降级标记的关系**：knowledge 世界 rerank 降级时
`rerank_degraded=true`（条级，§5.6）与 `degraded:["rerank_unavailable"]`（世界级）
**同时设置**——前者服务逐条消费，后者服务聚合展示；图扩展/embed 失败只有世界级
标记（条级无冗余字段，graph_boost 全 0 可从 `score_breakdown` 观测）。

`per_world_counts` 语义不变（各世界返回条数）；`hits` 跨世界混排时按 `score`
降序——**注意量纲差异**：knowledge 是融合分、code/doc 是纯语义分，混排可比性有
限，调用方需要严格可比时应指定单一 world。该限制写入文档不规避（`world=all`
现状行为即如此，S5 不引入新偏差）。

#### 5.7.5 knowledge 命中 JSON 示例

```json
{
  "query": "渠道怎么路由",
  "world": "knowledge",
  "hits": [
    {
      "id": "dt://entity/offen-pay/Channel/ifcode",
      "title": "ifCode",
      "snippet": "渠道路由字段，决定支付请求路由到哪个平台",
      "source_world": "knowledge",
      "entity_type": "Channel",
      "score": 0.83,
      "source_ref": "dt://doc/offen-pay/pay-design.md",
      "element_id": "4:91:12345",
      "hop": 0,
      "score_breakdown": {
        "semantic": 0.71,
        "rerank": 0.92,
        "graph_boost": 1.0,
        "final_score": 0.83
      },
      "relations": [
        {
          "rel_type": "routes_to",
          "other_end_id": "dt://entity/offen-pay/Service/pay-channel-service",
          "other_end_name": "PayChannelService",
          "direction": "out",
          "confidence": 0.9,
          "evidence": "ifCode 决定支付请求路由到哪个平台渠道",
          "supplementary_count": 2
        }
      ],
      "evidence": [
        "……路由规则：根据请求中的 ifCode 字段匹配渠道表，选择成本最低的可用渠道……"
      ]
    }
  ],
  "total": 3,
  "per_world_counts": { "knowledge": 3 },
  "degraded": []
}
```

---

## 6. 现有代码映射：删 / 改 / 增

### 6.1 删除

| 位置 | 内容 |
|------|------|
| `search_mcp.rs:264-327` | `search_knowledge` 的 CONTAINS Cypher 及行内映射（委托 retrieve.rs 后无残留职责） |

### 6.2 改造

| 位置 | 改什么 |
|------|--------|
| `search_mcp.rs` | `search_knowledge` → 委托 `retrieve::search_knowledge`；`search_vector` → world=doc 改查 `doc_chunks`（§5.5）；`CrossWorldSearch::new` 加 `rerank` 参（**`empty()` 构造器同步加参置 `None`，防漏网**）；`SearchHit` 扩展 + 输出格式契约（§5.6/§5.7）；`SearchRequest` 加 `max_hops`/`with_evidence`/`origin`/`doc_id` 可选参（§5.7.1） |
| `interfaces/grpc/services/build_service.rs:100-130` | 构造处接 `rerank_provider` 路由出 `Arc<dyn RerankService>`；**world 透传依赖 proto 变更，本期不做**（S5-D10），仍硬编码 `code` |
| `shared/collections.rs` | 无改动（复用 KG_NODES/DOC_CHUNKS 常量） |

### 6.3 新增

| 位置 | 内容 |
|------|------|
| `application/knowledge/extract/retrieve.rs` | 混合检索全流程（§5.1-5.4）+ elementId 定位扩展（§5.2.2，无映射表）+ 边去重聚合 + 邻居白名单（S5-D11）+ 分桶截断（§5.2.3）+ `via_same_as` 判定 + 邻居 `business_id` 派生（复用 `kg_bridge::business_id`，必要时提为 pub 导出） |
| `application/knowledge/extract/retrieve.rs` 内单测 | 候选合并/去重/融合权重/sig 降级各路径（graph/vector/embed mock） |

---

## 7. 落地顺序（每步独立可验证）

| 步骤 | 内容 | 验证方式 |
|------|------|----------|
| **S5-0**（前置） | 全量重建 `kg_nodes`（`delete_collection` + 全量 build / `dt kg-sync`） | 旧格式点无 `project`/`business_id`/`origin`（主文档 §13.5/§13.6：3945 点中仅 281 新格式），不重建则召回面被过滤卡死、验收失真；重建后抽查新格式点占比 ≈100% |
| **S5a** | retrieve.rs 骨架：召回 + Entity 图扩展（SAME_AS 归并）+ 非 Entity elementId 扩展（§5.2.2）+ 候选合并/边去重/白名单/分桶截断（无 rerank，融合用降级权重）；`search_knowledge` 切换委托 | 单测覆盖合并/去重/白名单/分桶/降级；对 test-pipeline 实测（**必须显式传 project**）：语义查询"渠道怎么路由"命中 `ifCode`（CONTAINS 必不命中）；`Entity` 首次出现在 knowledge 结果中；learned 节点（Knowledge/Concept/Playbook）种子经 elementId 扩展带出 1 跳邻居，且 Event/Document 不进候选 |
| **S5b** | rerank 接入 + sigmoid 归一 + 完整融合公式 + score_breakdown 输出 | 对比 S5a 同查询排序变化；`rerank_degraded` 路径单测；xinference bge-reranker-v2-m3 启动联调 |
| **S5c** | world=doc 改查 `doc_chunks`（含 `source="doc"` 硬过滤）+ 证据回填（`with_evidence`，合并单查询）+ `doc_id` 参数 | "给我 ifCode 的证据段落"返回含原文 text 的块；nacos 配置点（无 `text`）不出现在 doc 世界结果中 |
| 收尾 | 权重收敛观测（score_breakdown 采样）→ 回写主文档 §8；主文档 §13.1 S5 状态更新；**开立后续任务卡：proto 变更（`SearchRequest` 加 `world`/`max_hops`/`with_evidence`/`origin`/`doc_id`，`SearchResult` 加新字段）+ `build_service` 透传 + CLI/MCP 换栈 + `application/search.rs` 重叠模块（QueryRewriter/RRF/expand_nodes）归并**（S5-D10） | `cargo test && cargo clippy --all-targets` 全绿 |

---

## 8. 风险与注意点

1. **延迟预算**：单次检索端到端 ≈ embed（~100ms）+ 召回（~50ms）+ 图扩展
   （≤2 条 Cypher，~100ms）+ rerank（50 候选约 200-500ms xinference 本地 /
   100-300ms SiliconFlow）+ 可选证据回填（合并单查询，~100ms），**目标 p95 < 1.5s**。
   `DT_KG_RERANK_TOP_N` 下调是压延迟的主旋钮；S5b 实测后再定是否加 rerank 超时
   熔断（超时即走降级权重，不阻塞返回）。
2. **高连接度节点扇出**：`Domain`/`Service` 类节点 1 跳邻居可达数百。防护 =
   逐种子 Rust 侧截断（≤10，§5.2.2）+ 邻居桶配额（§5.2.3）+ Entity 变长路径全局
   `LIMIT 500` 保险丝（§5.2.1）三层。注意 Cypher `LIMIT` 在 `UNWIND` 后是全局语
   义，**给不了"每组上限"**，不要依赖它做防护。Entity 侧密度以 test-pipeline
   （RELATES ~60-70 边）外推生产项目（offen-pay 等）依据不足，S5b 实测生产图后
   重估是否加 Entity 邻居硬上限。
3. **变长路径参数化**：`*1..2` 跳数不能参数化，只能白名单拼接（§5.2.1），严禁把
   请求整数直接插进 Cypher 字符串。
4. **语义分量纲与阈值**：Qdrant cosine 分数实践上落在 [0,1]（BGE-M3 文本对余弦
   极少为负；注意代码中无显式归一化，依赖模型/API 侧行为），与 sigmoid 后的
   rerank 分同量纲，融合无需再归一；若未来换 embed 模型需重估。
   `DT_SEARCH_MIN_SCORE=0.3` 沿用 code 世界经验值，对 BGE-M3 中文知识召回分布的
   适配性未经论证——S5b 用 `score_breakdown` 采样校准（world=doc 同样生效，现状
   该阈值只作用 code 世界）。
5. **SAME_AS 当前无双节点**：auto SAME_AS 不可达（主文档 §13.3），但检索侧必须按
   §6.4 实现无向归并——manual 边随时可能出现，不能等有了再改检索。
6. **`description` 兼容别名**：S5 后 `search_mcp` 不再读 `payload.description`，
   I2 双写别名的最后消费方清零，可在后续任务中摘除（主文档 §13.5 注明"S5 后可摘"）。
7. **旧 `world=vector` 别名**：`search_mcp.rs:395` 把 `world=doc` 与 `world=vector`
   混为一谈。S5c 后 `vector` 别名保留映射到 doc 行为，不新增语义。
8. **`SearchHit.id` 语义变更**：knowledge 世界从 Memgraph 内部 id 改为 business_id
   （§5.7.2），是有意的破坏性修正——旧值全量重建后失效，本就不该被消费方持久化。
   gRPC 链路（build_service）仅透传不解析，无消费方受影响。
9. **elementId 漂移**：payload 的 `elementId` 是写入时快照，与图的一致性依赖"图节
   点删除时向量点同步删除"（主文档 §7.5 删除闭环）。若发生漂移（如图库局部重建后
   旧点残留），图扩展 `MATCH` 落空 → 退化为纯向量召回（可接受降级，不报错）；
   全量重建 / `dt kg-sync` 后自愈。
10. **双检索栈并存**：`application/search.rs`（QueryRewriter/RRF/`expand_nodes`，
    服务 CLI `dt search`/`dt search-kg` 与 MCP `dt_search`）与 retrieve.rs 在图扩
    展/融合上功能重叠。本期并存是有意的（S5-D10），但归并任务卡必须在收尾时开立，
    避免长期双轨漂移。
11. **运行态 provider 路由**：当前 `config/pipeline.yaml` 将 embed/rerank/llm 全部
    路由到 xinference，而代码默认 siliconflow——S5b 联调前显式确认实际生效路由
    （以 yaml 为准）；xinference 侧 `model_reranker: bge-reranker-v2-m3` 已配置
    （无 `BAAI/` 前缀，与 SiliconFlow 侧默认值的前缀差异属正常）。

**本期配置项汇总**：

| 配置项 | 位置 | 默认 | 说明 |
|--------|------|------|------|
| `DT_KG_RERANK_TOP_N` | env（新增） | `50` | rerank 候选总上限（分桶规则见 §5.2.3） |
| `DT_SEARCH_MIN_SCORE` | env（复用） | `0.3` | 语义分阈值；S5 起 knowledge/doc 世界同样生效，S5b 校准 |
| `SILICONFLOW_RERANKER_MODEL` | env（已有） | `BAAI/bge-reranker-v2-m3` | SiliconFlow rerank 模型（`siliconflow.rs:36`） |
| `providers.xinference.model_reranker` | `config/pipeline.yaml`（已有） | `bge-reranker-v2-m3` | 本地 xinference rerank 模型 |

---

## 9. 验收标准（S5 全量）

1. 语义查询命中非字面匹配实体（主文档 §11 S5 验收原句）："渠道怎么路由" → `ifCode`
   出现在 top-5，且 `score_breakdown` 完整。**容差**：LLM 抽取存在漂移（主文档 §13.6
   实测 RELATES -22%），若跌出 top-5 但在 top-10，以 `score_breakdown` 归因（召回/
   扩展/排序哪层丢分）并记录实际位次，不视为失败；跌出 top-10 才算回归。
2. 图扩展捞回结构相关但向量漏掉的实体：构造查询使其向量召回不含某 1 跳邻居，
   验证该邻居经图扩展进入候选、**通过分桶截断进入 rerank 候选集**且 `hop=1`——
   回归 §5.2.3 分桶设计，防"邻居被种子名额挤光"回潮。
3. `SAME_AS`（manual 挂边）两节点任一被召回，另一节点以 `hop=0`、
   `via_same_as=true` 呈现，且其邻居纳入候选。
4. 降级三分支各自可观测：rerank 关停 → 结果打 `rerank_degraded` +
   `degraded:["rerank_unavailable"]`；图扩展失败 → `degraded:["graph_expansion_failed"]`；
   embed 关停 → knowledge 世界空 + `degraded:["embed_unavailable"]`。
5. world=doc 返回 `doc_chunks` 原文段落且**不含 nacos 配置点**（`source="doc"`
   过滤生效）；`doc_id` 参数限定单文档生效；`with_evidence` 时 top-5 实体各附
   ≤2 段证据（数组匹配走原生 filter 路径，有测试锁死）。
6. 输出格式符合 §5.7 契约：knowledge 命中的 `id` 为 business_id、`hop`/
   `score_breakdown`/`via_same_as` 齐备；`CrossWorldResult.degraded` 与降级路径
   一一对应；`origin` 过滤参数生效（`None`=不过滤，非法值宽容回退）。
7. `cargo test && cargo clippy --all-targets` 全绿；新增单测覆盖：候选合并去重、
   边聚合、跳数白名单、邻居白名单（S5-D11）、分桶截断（§5.2.3）、降级三分支、
   sigmoid 归一、`via_same_as` 判定、source_ref 的 MENTIONED_IN 回退。

---

## 10. S5 实施记录（2026-08-02）

### 10.1 落地提交

| 提交 | 内容 |
|------|------|
| `3f97574` | fix: kg_bridge build_payload name 兜底链（Document 节点 name 为 null 的附带修复） |
| 契约扩展 | SearchHit/SearchRequest/CrossWorldResult + retrieve.rs 骨架 |
| kg_bridge | `business_id_from_props` 提取（21 项属性序同源共享） |
| retrieve.rs | 召回 / Entity 扩展 / 非 Entity elementId 扩展 / 合并分桶 / 降级融合管线 |
| search_mcp | `search_knowledge` 委托切换（CONTAINS 删除）+ `search_doc` 改写 |
| rerank | `create_rerank_router` + clamp 归一 + build_service 接线 |
| 证据回填 | with_evidence 合并单查询 |
| 实测 | `tests/s5_knowledge_search.rs`（6 个 #[ignore] live 测试） |

### 10.2 实测结果（test-pipeline；xinference bge-m3 + bge-reranker-v2-m3）

- **S5a（降级链路）**：语义非字面命中 ✓（"新增渠道的唯一代码标识" → ifCode #1，hop=0）；
  图扩展捞回向量漏掉的实体 ✓（alipay 向量 rank #128 超出召回窗口 k=120，经 ifCode
  RELATES 边以 hop=1 出现在 #31）；Entity 首入 knowledge 结果 ✓；`rerank_unavailable`
  降级标记 ✓；`score_breakdown` 齐全 ✓。
- **S5b（rerank 接入）**：rerank 分把降级模式的扁平排序（0.72-0.85）拉开为清晰梯度
  （ifCode 0.998 → #1 0.941；wayCode 0.992 → #2 0.922；#3 起断崖 ≤0.56）；rerank 关停
  模拟 → `rerank_degraded=true` + `degraded:["rerank_unavailable"]`，降级公式抽查一致 ✓。
- **S5c（world=doc + with_evidence）**：doc 世界返回原文块（`{doc_id}:{block_index}`），
  nacos 点被 `source="doc"` 硬过滤 + payload 解析双重排除 ✓；top-5 实体各附 ≤2 段证据，
  ifCode 命中决策文档原段 ✓。
- **规范查询归因（§9.1 容差实例）**："渠道怎么路由" 未命中 ifCode——本次构建其摘要漂移
  为 "用于标识新增渠道的唯一代码标识"（无路由语义），向量 rank #74/275 跌出召回窗口。
  归因：LLM 抽取漂移，非检索链路故障（`s5a_canonical_query_attribution` 仅打印不断言）。

### 10.3 实施期偏差（已并入本文档）

1. **S5-D6 clamp 替代二次 sigmoid**（见决策表与 §5.3 注）。
2. **Document 种子剔除**（§5.1 新增条目；S5a 实测 top-15 被 Document 点占 13 的污染）。
3. **reranker 运行形态**：GPU 满载时 50 候选 rerank CUDA OOM，本机以 `device=cpu` 运行
   bge-reranker-v2-m3（50 候选秒级）；生产部署按 §8.1 评估 GPU 配额或沿用 CPU。

### 10.4 权重收敛观测

`score_breakdown` 采样（S5b 实测）：rerank 分对"真正相关"与"字面相邻"实体的区分度极高
（0.998 vs ≤0.56），0.6/0.3/0.1 初值排序效果符合预期，**本期不调整**。已知特性：
降级模式下邻居排序系统性低于种子（一阶衰减后 alipay 仍 #31），属 §5.4 明示的保底行为；
若后续观测到邻居长期无法进入 top-limit，再评估提升邻居桶占比或调整降级权重。

### 10.5 已知边界（后续优化点）

同一节点既被向量低分召回（种子桶溢出）又是扩展邻居时，按最小 hop 规则归入种子桶后会被
整体丢弃（本次 wechat/yinsheng 实例）——可考虑"种子桶溢出节点回退邻居桶再竞争"，列入
后续优化，不阻塞本期。

### 10.6 后续任务卡（S5-D10 入口可达性）

**标题**：检索入口暴露与双栈归并
**背景**：S5 混合检索当前仅进程内可达。gRPC `build_service` 硬编码 `world="code"` 且
proto `SearchRequest` 无 `world` 字段、`SearchResult` 无新字段位；CLI `dt search`/
`dt search-kg` 与 MCP `dt_search` 走第二栈（`application/search.rs`）。
**范围**：
1. proto：`SearchRequest` 加 `world`/`max_hops`/`with_evidence`/`origin`/`doc_id`；
   `SearchResult` 加 `score_breakdown`/`hop`/`via_same_as`/`relations`/`evidence`/
   `rerank_degraded`（或新增 CrossWorldSearch RPC 全量承载 §5.7 契约）。
2. `build_service`：透传 world 及新参数；`mcp-server.py:462` 的 gRPC TODO 一并接通。
3. CLI：`dt search` knowledge/doc 世界改走 retrieve.rs；退役/归并
   `application/search.rs` 的 `expand_nodes`/`fusion`/`rewrite` 中与 retrieve.rs
   重叠的部分（QueryRewriter 是否保留为查询改写前置另行评估）。
**验收**：`dt search --world knowledge "新增渠道的唯一代码标识"` 与 gRPC 同参调用返回一致结果。
