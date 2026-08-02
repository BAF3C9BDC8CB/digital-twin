//! Retrieve 检索层 — GraphRAG 式混合检索（spec S5 / 主文档 §8）。
//!
//! 管线：召回(kg_nodes) → 图扩展(SAME_AS/RELATES/elementId) → 分桶截断
//! → rerank(sigmoid) → 融合排序。任一路故障降级，不整体失败（spec §3）。

use std::sync::Arc;

use crate::application::context::graph_parse::parse_graph_rows;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};
use crate::shared::collections::KG_NODES;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// 输出契约子结构（spec §5.7.3）
// ---------------------------------------------------------------------------

/// 排序分解——权重调参（主文档 §8 收敛回写）的观测数据源。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreBreakdown {
    /// 向量召回分；邻居 = 种子分 × 0.5^hop（§5.4 近似）。
    pub semantic: f64,
    /// sigmoid 归一后；rerank_degraded 时为 0.0。
    pub rerank: f64,
    /// 正常 1.0/0.5/0.25（0.5^hop）；降级模式一阶衰减 1.0/0.5（§5.4）。
    pub graph_boost: f64,
    /// = SearchHit.score，冗余字段便于消费方免对齐。
    pub final_score: f64,
}

/// 关系摘要——命中实体对外的 top-5 关系（按 confidence 降序）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelationSnippet {
    /// RELATES.type，如 routes_to / depends_on。
    pub rel_type: String,
    /// 对端 business_id。
    pub other_end_id: String,
    /// 对端 name（展示用；不在候选集中时为空串）。
    pub other_end_name: String,
    /// "out" | "in"（相对命中实体）。
    pub direction: String,
    pub confidence: f64,
    /// 最高 confidence 边的证据句。
    pub evidence: Option<String>,
    /// 其余文档来源的补充证据数（边聚合产物；>0 表示多文档佐证）。
    pub supplementary_count: u32,
}

// ---------------------------------------------------------------------------
// 配置（env；spec §8 配置项汇总表）
// ---------------------------------------------------------------------------

/// 语义分阈值（复用 code 世界同款 env，S5 起 knowledge/doc 世界生效）。
pub fn min_score() -> f64 {
    std::env::var("DT_SEARCH_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.3)
}

/// rerank 候选总上限（分桶规则见 §5.2.3）。
pub fn rerank_top_n() -> usize {
    std::env::var("DT_KG_RERANK_TOP_N")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
}

/// 图扩展跳数白名单 {1,2}，其他值钳到 2（S5-D3；严禁直接拼请求整数进 Cypher）。
pub fn clamp_max_hops(h: u32) -> u32 {
    h.clamp(1, 2)
}

/// bge-reranker-v2-m3 返回 logit，sigmoid 归一到 [0,1]（S5-D6）。
pub fn sigmoid(x: f32) -> f64 {
    1.0 / (1.0 + (-(x as f64)).exp())
}

// ---------------------------------------------------------------------------
// Retriever — 混合检索执行器（管线在后续任务逐层填充）
// ---------------------------------------------------------------------------

/// GraphRAG 混合检索执行器。`graph`/`rerank` 可选（缺失即走对应降级路径）。
pub struct Retriever {
    pub(crate) graph: Option<Arc<dyn GraphRepository>>,
    pub(crate) vector: Arc<dyn VectorRepository>,
    pub(crate) embed: Arc<dyn EmbedService>,
    pub(crate) rerank: Option<Arc<dyn RerankService>>,
}

impl Retriever {
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            rerank,
        }
    }

    /// embed(query) → kg_nodes.search_with_filter(project[+origin], k) → 种子（§5.1）。
    pub(crate) async fn recall(
        &self,
        query: &str,
        project: Option<&str>,
        origin: Option<&str>,
        k: u64,
    ) -> Result<Vec<Seed>, DtError> {
        let embeddings = self.embed.embed_batch(&[query.to_string()]).await?;
        let Some(qvec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };
        let hits = self
            .vector
            .search_with_filter(KG_NODES, qvec, k, recall_filter(project, origin))
            .await?;
        let threshold = min_score();
        Ok(hits.iter().filter_map(|h| parse_seed(h, threshold)).collect())
    }
}

// ---------------------------------------------------------------------------
// ① 召回（§5.1）
// ---------------------------------------------------------------------------

/// 向量召回的种子节点。`business_id` 为稳定业务主键（旧格式点缺失即丢弃）。
#[derive(Debug, Clone)]
pub(crate) struct Seed {
    pub business_id: String,
    pub element_id: Option<String>,
    pub labels: Vec<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    pub semantic: f64,
}

/// Qdrant 原生 filter：project/origin 可选 must 条件（R7；§5.1/§5.7.1）。
fn recall_filter(project: Option<&str>, origin: Option<&str>) -> serde_json::Value {
    let mut must = Vec::new();
    if let Some(p) = project {
        must.push(serde_json::json!({"key": "project", "match": {"value": p}}));
    }
    if let Some(o) = origin {
        must.push(serde_json::json!({"key": "origin", "match": {"value": o}}));
    }
    serde_json::json!({ "must": must })
}

/// 从 Qdrant hit 解析种子；语义分 < min_score 或无 business_id 丢弃（§5.1）。
fn parse_seed(hit: &serde_json::Value, min_score: f64) -> Option<Seed> {
    let score = hit.get("score")?.as_f64()?;
    if score < min_score {
        return None;
    }
    let p = hit.get("payload")?;
    let business_id = p.get("business_id")?.as_str()?;
    if business_id.is_empty() {
        return None;
    }
    let labels: Vec<String> = p
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let name = p
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let summary = p
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entity_type = p
        .get("type")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| labels.first().cloned())
        .unwrap_or_else(|| "?".to_string());
    let element_id = p
        .get("elementId")
        .and_then(|v| v.as_str())
        .map(String::from);
    Some(Seed {
        business_id: business_id.to_string(),
        element_id,
        labels,
        name,
        summary,
        entity_type,
        semantic: score,
    })
}

// ---------------------------------------------------------------------------
// ② 图扩展（§5.2）
// ---------------------------------------------------------------------------

/// 归一化后的关系边（head/tail 为 business_id）。
#[derive(Debug, Clone)]
pub(crate) struct RawEdge {
    pub head: String,
    pub tail: String,
    pub rel_type: String,
    pub confidence: f64,
    pub evidence: Option<String>,
    pub doc_id: Option<String>,
}

/// 图扩展产物节点（SAME_AS 别名或 RELATES/任意关系邻居；不含原始种子）。
#[derive(Debug, Clone)]
pub(crate) struct ExpandedNode {
    pub business_id: String,
    pub element_id: Option<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    /// 扩展出该节点的种子 business_id（邻居语义分衰减用）。
    pub from_seed: String,
    pub hop: u32,
    pub via_same_as: bool,
    /// 路径各边 confidence 最小值（缺失按 0.5）；分桶预排分用（§5.2.3）。
    pub path_min_confidence: f64,
}

#[derive(Debug, Default)]
pub(crate) struct ExpansionResult {
    pub nodes: Vec<ExpandedNode>,
    pub edges: Vec<RawEdge>,
}

fn edge_confidence(e: &serde_json::Value) -> f64 {
    e.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5)
}

/// §5.2.1：Entity 种子 — SAME_AS 无向归并 + RELATES*1..max_hops（白名单插值）。
fn entity_expansion_cypher(max_hops: u32) -> String {
    let hops = clamp_max_hops(max_hops);
    format!(
        r#"
UNWIND $seeds AS seed
MATCH (e:Entity {{entity_id: seed}})
OPTIONAL MATCH (e)-[:SAME_AS]-(alias:Entity)
WITH collect(DISTINCT e) + collect(DISTINCT alias) AS seed_nodes
UNWIND seed_nodes AS s
OPTIONAL MATCH path = (s)-[:RELATES*1..{hops}]-(nb:Entity)
WITH s, nb, relationships(path) AS rels, length(path) AS hop
RETURN s.entity_id AS seed_id,
       elementId(s) AS seed_element_id,
       s.name AS seed_name,
       s.summary AS seed_summary,
       s.type AS seed_type,
       nb {{ .entity_id, .name, .type, .summary, .keywords }} AS neighbor,
       elementId(nb) AS neighbor_element_id,
       hop,
       [r IN rels | {{type: r.type, confidence: r.confidence,
                     evidence: r.evidence, doc_id: r.doc_id,
                     head: startNode(r).entity_id, tail: endNode(r).entity_id}}] AS edges
LIMIT 500
"#
    )
}

/// 解析 Entity 扩展行。`original` = 原始种子的 entity_id 集合（判定 via_same_as）。
fn parse_entity_rows(rows: Vec<serde_json::Value>, original: &HashSet<&str>) -> ExpansionResult {
    let mut result = ExpansionResult::default();
    let mut seen_alias: HashSet<String> = HashSet::new();
    for row in rows {
        let seed_id = row.get("seed_id").and_then(|v| v.as_str()).unwrap_or("");
        if seed_id.is_empty() {
            continue;
        }
        // SAME_AS 别名（命中节点不在原始种子集）→ hop=0 候选；原始种子自身跳过
        if !original.contains(seed_id) && seen_alias.insert(seed_id.to_string()) {
            result.nodes.push(ExpandedNode {
                business_id: seed_id.to_string(),
                element_id: row
                    .get("seed_element_id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                name: row
                    .get("seed_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                summary: row
                    .get("seed_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                entity_type: row
                    .get("seed_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Entity")
                    .to_string(),
                from_seed: seed_id.to_string(),
                hop: 0,
                via_same_as: true,
                path_min_confidence: 1.0,
            });
        }
        // 边（无论邻居是否存在都收集——hop 为 null 时 edges 为空数组）
        let edges = row
            .get("edges")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let path_min = edges.iter().map(edge_confidence).fold(1.0_f64, f64::min);
        for e in &edges {
            result.edges.push(RawEdge {
                head: e.get("head").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                tail: e.get("tail").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                rel_type: e.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                confidence: edge_confidence(e),
                evidence: e.get("evidence").and_then(|v| v.as_str()).map(String::from),
                doc_id: e.get("doc_id").and_then(|v| v.as_str()).map(String::from),
            });
        }
        // 邻居节点
        let Some(nb) = row.get("neighbor").filter(|n| !n.is_null()) else {
            continue;
        };
        let Some(nb_id) = nb.get("entity_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let hop = row.get("hop").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        result.nodes.push(ExpandedNode {
            business_id: nb_id.to_string(),
            element_id: row
                .get("neighbor_element_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            name: nb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            summary: nb
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            entity_type: nb
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("Entity")
                .to_string(),
            from_seed: seed_id.to_string(),
            hop,
            via_same_as: false,
            path_min_confidence: if edges.is_empty() { 0.5 } else { path_min },
        });
    }
    result
}

impl Retriever {
    /// Entity 种子图扩展（SAME_AS 归并 + RELATES 邻居，§5.2.1）。
    pub(crate) async fn expand_entity(
        &self,
        seeds: &[Seed],
        max_hops: u32,
    ) -> Result<ExpansionResult, DtError> {
        let Some(ref graph) = self.graph else {
            return Ok(ExpansionResult::default());
        };
        if seeds.is_empty() {
            return Ok(ExpansionResult::default());
        }
        let original: HashSet<&str> = seeds.iter().map(|s| s.business_id.as_str()).collect();
        let mut params = std::collections::HashMap::new();
        params.insert(
            "seeds".to_string(),
            serde_json::json!(seeds.iter().map(|s| &s.business_id).collect::<Vec<_>>()),
        );
        let raw = graph
            .read_query(&entity_expansion_cypher(max_hops), params)
            .await?;
        Ok(parse_entity_rows(parse_graph_rows(&raw), &original))
    }

    /// 非 Entity 种子图扩展（elementId 定位，§5.2.2）。
    pub(crate) async fn expand_business(&self, seeds: &[Seed]) -> Result<ExpansionResult, DtError> {
        let Some(ref graph) = self.graph else {
            return Ok(ExpansionResult::default());
        };
        // 无 elementId 的种子无法定位，静默跳过（payload 恒应携带；缺失记 warn）
        let located: Vec<&Seed> = seeds
            .iter()
            .filter(|s| {
                if s.element_id.is_none() {
                    tracing::warn!(
                        "seed {} has no elementId, skip graph expansion",
                        s.business_id
                    );
                }
                s.element_id.is_some()
            })
            .collect();
        if located.is_empty() {
            return Ok(ExpansionResult::default());
        }
        let mut params = std::collections::HashMap::new();
        params.insert(
            "seed_eids".to_string(),
            serde_json::json!(located
                .iter()
                .map(|s| s.element_id.clone().unwrap())
                .collect::<Vec<_>>()),
        );
        let raw = graph.read_query(BIZ_EXPANSION_CYPHER, params).await?;
        let rows = parse_graph_rows(&raw);

        // seed_eid → seed（from_seed / 边端点 business_id 映射用）
        let by_eid: std::collections::HashMap<&str, &Seed> = located
            .iter()
            .filter_map(|s| s.element_id.as_deref().map(|e| (e, *s)))
            .collect();

        // 按种子分组 → 白名单过滤 → confidence 降序 → ≤PER_SEED_NEIGHBOR_CAP
        let mut per_seed: std::collections::HashMap<String, Vec<&serde_json::Value>> =
            std::collections::HashMap::new();
        for row in &rows {
            let Some(nb) = row.get("neighbor").filter(|n| !n.is_null()) else {
                continue;
            };
            let labels: Vec<String> = nb
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !neighbor_allowed(&labels) {
                continue;
            }
            let seed_eid = row
                .get("seed_eid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            per_seed.entry(seed_eid).or_default().push(row);
        }

        let mut result = ExpansionResult::default();
        for (seed_eid, mut group) in per_seed {
            group.sort_by(|a, b| {
                let ca = a.get("rel_confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
                let cb = b.get("rel_confidence").and_then(|v| v.as_f64()).unwrap_or(0.5);
                cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
            });
            group.truncate(PER_SEED_NEIGHBOR_CAP);
            let from_seed = by_eid
                .get(seed_eid.as_str())
                .map(|s| s.business_id.clone())
                .unwrap_or_default();
            for row in group {
                let nb = &row["neighbor"];
                let nb_eid = row
                    .get("neighbor_element_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let nb_bid = crate::application::sync::kg_bridge::business_id_from_props(nb, nb_eid);
                let labels: Vec<String> = nb
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|l| l.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let entity_type = nb
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| labels.first().cloned())
                    .unwrap_or_else(|| "?".to_string());
                let conf = row
                    .get("rel_confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5);
                result.nodes.push(ExpandedNode {
                    business_id: nb_bid.clone(),
                    element_id: row
                        .get("neighbor_element_id")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    name: nb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    summary: nb
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    entity_type,
                    from_seed: from_seed.clone(),
                    hop: 1,
                    via_same_as: false,
                    path_min_confidence: conf,
                });
                // 边端点 elementId → business_id（端点非种子即邻居）
                let rel_type = row.get("rel_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if !rel_type.is_empty() {
                    let head_eid = row.get("rel_head").and_then(|v| v.as_str()).unwrap_or("");
                    let tail_eid = row.get("rel_tail").and_then(|v| v.as_str()).unwrap_or("");
                    let resolve = |eid: &str| -> String {
                        if eid == seed_eid {
                            from_seed.clone()
                        } else if eid == nb_eid {
                            nb_bid.clone()
                        } else {
                            eid.to_string()
                        }
                    };
                    result.edges.push(RawEdge {
                        head: resolve(head_eid),
                        tail: resolve(tail_eid),
                        rel_type,
                        confidence: conf,
                        evidence: None,
                        doc_id: None,
                    });
                }
            }
        }
        Ok(result)
    }
}

/// S5-D11 邻居白名单：Entity 恒允许；BUSINESS_LABELS 内且非 Events 组/Document 允许。
/// 与 `kg_bridge::BUSINESS_LABELS` 同源引用，禁止复制 label 列表（Global Constraints）。
pub(crate) fn neighbor_allowed(labels: &[String]) -> bool {
    const DENY: &[&str] = &["ConfigChange", "BugFix", "Decision", "PodEvent", "Document"];
    labels.iter().any(|l| l == "Entity")
        || (labels
            .iter()
            .any(|l| crate::application::sync::kg_bridge::BUSINESS_LABELS.contains(&l.as_str()))
            && !labels.iter().any(|l| DENY.contains(&l.as_str())))
}

/// §5.2.2：非 Entity 种子 — payload elementId 定位（无映射表），1 跳任意关系邻居。
const BIZ_EXPANSION_CYPHER: &str = r#"
UNWIND $seed_eids AS eid
MATCH (n) WHERE elementId(n) = eid
OPTIONAL MATCH (n)-[r]-(nb)
RETURN eid AS seed_eid,
       nb { .* , labels: labels(nb) } AS neighbor,
       elementId(nb) AS neighbor_element_id,
       type(r) AS rel_type,
       r.confidence AS rel_confidence,
       elementId(startNode(r)) AS rel_head,
       elementId(endNode(r)) AS rel_tail
"#;

/// 每种子邻居上限（§5.2.2；配合 §5.2.3 邻居桶承担扇出防护）。
const PER_SEED_NEIGHBOR_CAP: usize = 10;

// ---------------------------------------------------------------------------
// ③ 候选合并与分桶截断（§5.2.3）
// ---------------------------------------------------------------------------

/// rerank 候选。`pre_rank` 仅用于候选选拔（分桶排序键），不参与最终融合。
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub business_id: String,
    pub element_id: Option<String>,
    pub name: String,
    pub summary: String,
    pub entity_type: String,
    /// 种子=自身召回分；邻居=种子分 × 0.5^hop（§5.4 近似）。
    pub semantic: f64,
    pub hop: u32,
    pub via_same_as: bool,
    /// 1.0 / 0.5 / 0.25（0.5^hop）。
    pub graph_boost: f64,
    /// 桶内预排分：种子=semantic；邻居=种子 semantic × path_min_confidence。
    pub pre_rank: f64,
    pub relations: Vec<RelationSnippet>,
    pub source_ref: Option<String>,
    pub rerank_score: Option<f64>,
}

fn boost_of(hop: u32) -> f64 {
    0.5_f64.powi(hop as i32)
}

/// 合并种子与扩展产物：按 business_id 去重，同节点取最小 hop（最大 boost）。
pub(crate) fn merge_candidates(seeds: &[Seed], expansion: ExpansionResult) -> Vec<Candidate> {
    let seed_score = |bid: &str| {
        seeds
            .iter()
            .find(|s| s.business_id == bid)
            .map(|s| s.semantic)
            .unwrap_or(0.0)
    };
    let mut map: std::collections::HashMap<String, Candidate> = std::collections::HashMap::new();
    for s in seeds {
        map.insert(
            s.business_id.clone(),
            Candidate {
                business_id: s.business_id.clone(),
                element_id: s.element_id.clone(),
                name: s.name.clone(),
                summary: s.summary.clone(),
                entity_type: s.entity_type.clone(),
                semantic: s.semantic,
                hop: 0,
                via_same_as: false,
                graph_boost: 1.0,
                pre_rank: s.semantic,
                relations: vec![],
                source_ref: None,
                rerank_score: None,
            },
        );
    }
    for n in expansion.nodes {
        let base = seed_score(&n.from_seed);
        let (hop, boost, semantic, pre_rank) = if n.via_same_as {
            (0, 1.0, base, base) // 别名视为同一实体
        } else {
            (
                n.hop,
                boost_of(n.hop),
                base * boost_of(n.hop),
                base * n.path_min_confidence,
            )
        };
        map.entry(n.business_id.clone())
            .and_modify(|c| {
                if hop < c.hop {
                    c.hop = hop;
                    c.graph_boost = boost;
                    c.semantic = semantic;
                    c.pre_rank = pre_rank;
                    c.via_same_as = n.via_same_as;
                }
            })
            .or_insert(Candidate {
                business_id: n.business_id.clone(),
                element_id: n.element_id.clone(),
                name: n.name.clone(),
                summary: n.summary.clone(),
                entity_type: n.entity_type.clone(),
                semantic,
                hop,
                via_same_as: n.via_same_as,
                graph_boost: boost,
                pre_rank,
                relations: vec![],
                source_ref: None,
                rerank_score: None,
            });
    }
    // 无 summary 候选：name 兜底进 rerank 文本；仍为空则丢弃（§5.2.3）
    map.into_values()
        .filter(|c| !c.summary.is_empty() || !c.name.is_empty())
        .collect()
}

/// 边去重聚合 + 挂接到候选：同一 (head, rel_type, tail) 保最高 confidence，
/// 其余计 supplementary_count；source_ref = 最高 confidence 边 doc_id（§5.2.1/§5.7.2）。
pub(crate) fn attach_relations(candidates: &mut [Candidate], edges: &[RawEdge]) {
    use std::collections::HashMap;
    // 三元组去重（保最高 confidence，计补充证据数）
    let mut dedup: HashMap<(&str, &str, &str), (&RawEdge, u32)> = HashMap::new();
    for e in edges {
        dedup
            .entry((e.head.as_str(), e.rel_type.as_str(), e.tail.as_str()))
            .and_modify(|(best, cnt)| {
                if e.confidence > best.confidence {
                    *best = e;
                }
                *cnt += 1;
            })
            .or_insert((e, 0));
    }
    // 先建 business_id → name 快照（避免 iter_mut 与不可变借用冲突）
    let names: HashMap<String, String> = candidates
        .iter()
        .map(|c| (c.business_id.clone(), c.name.clone()))
        .collect();
    let mut best_doc: HashMap<String, (f64, Option<String>)> = HashMap::new();
    for c in candidates.iter_mut() {
        let mut rels: Vec<RelationSnippet> = dedup
            .values()
            .filter(|(e, _)| e.head == c.business_id || e.tail == c.business_id)
            .map(|(e, cnt)| {
                let outgoing = e.head == c.business_id;
                let other = if outgoing { &e.tail } else { &e.head };
                RelationSnippet {
                    rel_type: e.rel_type.clone(),
                    other_end_id: other.clone(),
                    other_end_name: names.get(other).cloned().unwrap_or_default(),
                    direction: if outgoing { "out".into() } else { "in".into() },
                    confidence: e.confidence,
                    evidence: e.evidence.clone(),
                    supplementary_count: *cnt,
                }
            })
            .collect();
        rels.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rels.truncate(5);
        c.relations = rels;
    }
    // source_ref：候选为端点的边中 confidence 最高者的 doc_id
    for e in edges {
        for bid in [&e.head, &e.tail] {
            let entry = best_doc.entry(bid.clone()).or_insert((f64::MIN, None));
            if e.confidence > entry.0 && e.doc_id.is_some() {
                *entry = (e.confidence, e.doc_id.clone());
            }
        }
    }
    for c in candidates.iter_mut() {
        if let Some((_, doc)) = best_doc.get(&c.business_id) {
            c.source_ref = doc.clone();
        }
    }
}

/// 分桶截断（§5.2.3）：种子桶 ⌈top_n×0.6⌉，邻居桶 top_n−种子实际数（保底 40% 可上浮）。
pub(crate) fn bucket_truncate(candidates: &mut Vec<Candidate>, top_n: usize) {
    let seed_cap = ((top_n as f64) * 0.6).ceil() as usize;
    let mut seeds: Vec<Candidate> = candidates.iter().filter(|c| c.hop == 0).cloned().collect();
    seeds.sort_by(|a, b| {
        b.semantic
            .partial_cmp(&a.semantic)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    seeds.truncate(seed_cap);
    let neighbor_cap = top_n.saturating_sub(seeds.len());
    let mut neighbors: Vec<Candidate> =
        candidates.iter().filter(|c| c.hop >= 1).cloned().collect();
    neighbors.sort_by(|a, b| {
        b.pre_rank
            .partial_cmp(&a.pre_rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    neighbors.truncate(neighbor_cap);
    seeds.extend(neighbors);
    *candidates = seeds;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use serde_json::json;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    pub(crate) struct MockGraph {
        pub responses: Mutex<VecDeque<serde_json::Value>>,
        pub captured: Mutex<Vec<(String, HashMap<String, serde_json::Value>)>>,
    }

    #[async_trait::async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.captured.lock().unwrap().push((query.to_string(), params));
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json!([])))
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(json!([]))
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) struct MockVector {
        pub hits: Vec<serde_json::Value>,
        pub captured_filter: Mutex<Option<serde_json::Value>>,
        pub captured_limit: Mutex<Option<u64>>,
    }

    #[async_trait::async_trait]
    impl VectorRepository for MockVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.hits.clone())
        }
        async fn search_with_filter(
            &self,
            _c: &str,
            _v: Vec<f32>,
            limit: u64,
            filter: serde_json::Value,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            *self.captured_filter.lock().unwrap() = Some(filter);
            *self.captured_limit.lock().unwrap() = Some(limit);
            Ok(self.hits.clone())
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.to_string(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) struct MockEmbed;

    #[async_trait::async_trait]
    impl EmbedService for MockEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    pub(crate) fn empty_vector() -> MockVector {
        MockVector {
            hits: vec![],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        }
    }

    pub(crate) fn seed_hit(score: f64, business_id: &str, labels: &[&str]) -> serde_json::Value {
        json!({
            "id": "pt-1",
            "score": score,
            "payload": {
                "business_id": business_id,
                "elementId": "4:91:1",
                "labels": labels,
                "name": "ifCode",
                "summary": "渠道路由字段",
                "type": "Channel",
                "project": "offen-pay",
                "origin": "extracted"
            }
        })
    }

    #[tokio::test]
    async fn recall_filters_by_score_and_business_id() {
        let vector = MockVector {
            hits: vec![
                seed_hit(0.9, "dt://entity/p/Channel/ifcode", &["Entity"]),
                seed_hit(0.1, "dt://entity/p/Channel/low", &["Entity"]), // 低于阈值
                {
                    // 旧格式点：无 business_id → 丢弃
                    let mut h = seed_hit(0.95, "x", &["Knowledge"]);
                    h["payload"].as_object_mut().unwrap().remove("business_id");
                    h
                },
            ],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let r = Retriever::new(None, Arc::new(vector), Arc::new(MockEmbed), None);
        let seeds = r
            .recall("渠道怎么路由", Some("offen-pay"), Some("extracted"), 60)
            .await
            .unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].business_id, "dt://entity/p/Channel/ifcode");
        assert_eq!(seeds[0].semantic, 0.9);
        assert!(seeds[0].labels.iter().any(|l| l == "Entity"));
    }

    #[tokio::test]
    async fn recall_builds_native_filter_with_project_and_origin() {
        let vector = Arc::new(empty_vector());
        let r = Retriever::new(None, vector.clone(), Arc::new(MockEmbed), None);
        let _ = r.recall("q", Some("p1"), Some("manual"), 60).await.unwrap();
        let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
        let must = filter["must"].as_array().unwrap();
        assert!(must
            .iter()
            .any(|c| c["key"] == "project" && c["match"]["value"] == "p1"));
        assert!(must
            .iter()
            .any(|c| c["key"] == "origin" && c["match"]["value"] == "manual"));
        assert_eq!(*vector.captured_limit.lock().unwrap(), Some(60));
    }

    pub(crate) fn entity_row(
        seed_id: &str,
        neighbor: serde_json::Value,
        hop: serde_json::Value,
        edges: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "seed_id": seed_id,
            "seed_element_id": format!("eid-{seed_id}"),
            "seed_name": format!("name-{seed_id}"),
            "seed_summary": format!("summary-{seed_id}"),
            "seed_type": "Channel",
            "neighbor": neighbor,
            "neighbor_element_id": neighbor.get("entity_id").map(|_| "eid-nb"),
            "hop": hop,
            "edges": edges,
        })
    }

    #[tokio::test]
    async fn expand_entity_builds_whitelisted_cypher() {
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![json!([])])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        let seeds = vec![Seed {
            business_id: "A".into(),
            element_id: Some("eid-A".into()),
            labels: vec!["Entity".into()],
            name: "a".into(),
            summary: "s".into(),
            entity_type: "Channel".into(),
            semantic: 0.9,
        }];
        let _ = r.expand_entity(&seeds, 2).await.unwrap();
        let (cypher, params) = graph.captured.lock().unwrap()[0].clone();
        assert!(cypher.contains("SAME_AS"));
        assert!(cypher.contains("RELATES*1..2"));
        assert!(cypher.contains("LIMIT 500"));
        assert_eq!(params["seeds"], json!(["A"]));
        // max_hops=1 时拼 *1..1（白名单插值）
        let graph2 = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![json!([])])),
            captured: Mutex::new(vec![]),
        });
        let r2 = Retriever::new(
            Some(graph2.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        let _ = r2.expand_entity(&seeds, 1).await.unwrap();
        assert!(graph2.captured.lock().unwrap()[0]
            .0
            .contains("RELATES*1..1"));
    }

    #[test]
    fn parse_entity_rows_alias_and_neighbor_and_edges() {
        let original: HashSet<&str> = ["A"].into_iter().collect();
        let rows = vec![
            // 原始种子 A → 1 跳邻居 B，两条同三元组边（不同 doc_id，待 Task 6 聚合）
            entity_row(
                "A",
                json!({"entity_id":"B","name":"nb","type":"Service","summary":"sb"}),
                json!(1),
                json!([{"type":"routes_to","confidence":0.9,"evidence":"e1","doc_id":"d1","head":"A","tail":"B"},
                       {"type":"routes_to","confidence":0.7,"evidence":"e2","doc_id":"d2","head":"A","tail":"B"}]),
            ),
            // SAME_AS 别名 C（不在原始种子集）→ 无邻居
            entity_row("C", serde_json::Value::Null, serde_json::Value::Null, json!([])),
        ];
        let result = parse_entity_rows(rows, &original);
        // 别名节点：hop=0、via_same_as=true
        let alias = result.nodes.iter().find(|n| n.business_id == "C").unwrap();
        assert!(alias.via_same_as);
        assert_eq!(alias.hop, 0);
        // 邻居节点：hop=1、via_same_as=false、path_min_confidence 取路径最小值
        let nb = result.nodes.iter().find(|n| n.business_id == "B").unwrap();
        assert_eq!(nb.hop, 1);
        assert!(!nb.via_same_as);
        assert!((nb.path_min_confidence - 0.7).abs() < 1e-9);
        // 原始种子不产生 ExpandedNode（自身在召回侧已是候选）
        assert!(result.nodes.iter().all(|n| n.business_id != "A"));
        // 两条原始边都保留（聚合在 Task 6 attach_relations）
        assert_eq!(result.edges.len(), 2);
    }

    #[test]
    fn neighbor_whitelist() {
        assert!(neighbor_allowed(&["Entity".into()]));
        assert!(neighbor_allowed(&["Knowledge".into()]));
        assert!(neighbor_allowed(&["Table".into()]));
        assert!(!neighbor_allowed(&["Document".into()]));
        assert!(!neighbor_allowed(&["ConfigChange".into()]));
        assert!(!neighbor_allowed(&["PodEvent".into()]));
        assert!(!neighbor_allowed(&["Method".into()])); // 代码节点不在 BUSINESS_LABELS
    }

    #[tokio::test]
    async fn expand_business_filters_whitelist_and_caps_per_seed() {
        // 构造 12 个邻居：11 个 Knowledge（confidence 0.05*i）+ 1 个 Document（应被白名单滤掉）
        let mut rows = vec![];
        for i in 0..11 {
            rows.push(json!({
                "seed_eid": "eid-seed",
                "neighbor": {"knowledge_id": format!("k-{i}"), "name": format!("n{i}"),
                              "summary": "s", "labels": ["Knowledge"]},
                "neighbor_element_id": format!("eid-n{i}"),
                "rel_type": "RELATES_TO",
                "rel_confidence": 0.05 * i as f64,
            }));
        }
        rows.push(json!({
            "seed_eid": "eid-seed",
            "neighbor": {"doc_id": "d-1", "name": "doc", "labels": ["Document"]},
            "neighbor_element_id": "eid-doc",
            "rel_type": "MENTIONED_IN",
            "rel_confidence": 0.99,
        }));
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![json!(rows)])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        let seeds = vec![Seed {
            business_id: "k-seed".into(),
            element_id: Some("eid-seed".into()),
            labels: vec!["Knowledge".into()],
            name: "s".into(),
            summary: "s".into(),
            entity_type: "Knowledge".into(),
            semantic: 0.8,
        }];
        let result = r.expand_business(&seeds).await.unwrap();
        // 白名单过滤 Document；逐种子截断 ≤10（11 个 Knowledge 取 confidence 前 10）
        assert_eq!(result.nodes.len(), 10);
        assert!(result.nodes.iter().all(|n| n.entity_type != "Document"));
        assert!(result.nodes.iter().any(|n| n.business_id == "k-10")); // 最高分保留
        assert!(!result.nodes.iter().any(|n| n.business_id == "k-0")); // 最低分被截
        // Cypher 用 elementId 定位，参数为 payload elementId 列表
        let (cypher, params) = graph.captured.lock().unwrap()[0].clone();
        assert!(cypher.contains("elementId(n) = eid"));
        assert_eq!(params["seed_eids"], json!(["eid-seed"]));
    }

    pub(crate) fn mk_seed(bid: &str, score: f64) -> Seed {
        Seed {
            business_id: bid.into(),
            element_id: Some(format!("eid-{bid}")),
            labels: vec!["Entity".into()],
            name: format!("n{bid}"),
            summary: "s".into(),
            entity_type: "Channel".into(),
            semantic: score,
        }
    }

    pub(crate) fn mk_node(bid: &str, from: &str, hop: u32, conf: f64) -> ExpandedNode {
        ExpandedNode {
            business_id: bid.into(),
            element_id: Some(format!("eid-{bid}")),
            name: format!("n{bid}"),
            summary: "s".into(),
            entity_type: "Service".into(),
            from_seed: from.into(),
            hop,
            via_same_as: false,
            path_min_confidence: conf,
        }
    }

    #[test]
    fn merge_dedups_by_min_hop_and_decays_neighbor_semantic() {
        let seeds = vec![mk_seed("A", 0.9), mk_seed("B", 0.8)];
        let expansion = ExpansionResult {
            nodes: vec![
                mk_node("B", "A", 1, 0.9),
                mk_node("C", "A", 1, 0.9),
                mk_node("D", "A", 2, 0.6),
            ],
            edges: vec![],
        };
        let candidates = merge_candidates(&seeds, expansion);
        // B 既是种子又是邻居 → 保留 hop=0（种子形态，semantic=0.8 自身分）
        let b = candidates.iter().find(|c| c.business_id == "B").unwrap();
        assert_eq!(b.hop, 0);
        assert!((b.semantic - 0.8).abs() < 1e-9);
        // C：1 跳邻居，semantic = 0.9 × 0.5 = 0.45；pre_rank = 0.9 × 0.9
        let c = candidates.iter().find(|c| c.business_id == "C").unwrap();
        assert!((c.semantic - 0.45).abs() < 1e-9);
        assert!((c.pre_rank - 0.81).abs() < 1e-9);
        assert!((c.graph_boost - 0.5).abs() < 1e-9);
        // D：2 跳，boost 0.25
        let d = candidates.iter().find(|c| c.business_id == "D").unwrap();
        assert!((d.graph_boost - 0.25).abs() < 1e-9);
    }

    #[test]
    fn attach_relations_dedups_triple_and_fills_source_ref() {
        let mut candidates =
            merge_candidates(&[mk_seed("A", 0.9), mk_seed("B", 0.8)], ExpansionResult::default());
        let edges = vec![
            RawEdge {
                head: "A".into(),
                tail: "B".into(),
                rel_type: "routes_to".into(),
                confidence: 0.9,
                evidence: Some("e1".into()),
                doc_id: Some("d1".into()),
            },
            RawEdge {
                head: "A".into(),
                tail: "B".into(),
                rel_type: "routes_to".into(),
                confidence: 0.7,
                evidence: Some("e2".into()),
                doc_id: Some("d2".into()),
            },
        ];
        attach_relations(&mut candidates, &edges);
        let a = candidates.iter().find(|c| c.business_id == "A").unwrap();
        let rel = &a.relations[0];
        assert_eq!(rel.rel_type, "routes_to");
        assert_eq!(rel.direction, "out");
        assert_eq!(rel.other_end_id, "B");
        assert_eq!(rel.other_end_name, "nB");
        assert!((rel.confidence - 0.9).abs() < 1e-9);
        assert_eq!(rel.supplementary_count, 1); // 第二文档证据聚合
        assert_eq!(a.source_ref.as_deref(), Some("d1")); // 最高 confidence 边的 doc_id
        let b = candidates.iter().find(|c| c.business_id == "B").unwrap();
        assert_eq!(b.relations[0].direction, "in");
    }

    #[test]
    fn bucket_truncate_reserves_neighbor_quota() {
        // 60/40 分桶：top_n=4 → 种子桶 3，邻居桶 1
        let mut candidates = vec![];
        for (i, s) in [0.95, 0.9, 0.85, 0.8].iter().enumerate() {
            let mut c = merge_candidates(&[mk_seed(&format!("S{i}"), *s)], ExpansionResult::default());
            candidates.append(&mut c);
        }
        let mut nodes = vec![];
        for (i, pr) in [0.7, 0.6, 0.5].iter().enumerate() {
            let mut c = merge_candidates(
                &[],
                ExpansionResult {
                    nodes: vec![ExpandedNode {
                        business_id: format!("N{i}"),
                        element_id: None,
                        name: "n".into(),
                        summary: "s".into(),
                        entity_type: "Service".into(),
                        from_seed: "S0".into(),
                        hop: 1,
                        via_same_as: false,
                        path_min_confidence: *pr,
                    }],
                    edges: vec![],
                },
            );
            // 手动设 pre_rank 便于断言（merge 会算成 seed.semantic×conf，此处无种子 → 用构造值）
            c[0].pre_rank = *pr;
            nodes.push(c.remove(0));
        }
        candidates.extend(nodes);
        bucket_truncate(&mut candidates, 4);
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates.iter().filter(|c| c.hop == 0).count(), 3); // 种子桶 ⌈4×0.6⌉=3
        assert_eq!(candidates.iter().filter(|c| c.hop >= 1).count(), 1); // 邻居桶 4-3=1
        assert!(candidates.iter().any(|c| c.business_id == "N0")); // 邻居取 pre_rank 最高
        assert!(!candidates.iter().any(|c| c.business_id == "S3")); // 种子按语义分截断
    }
}
