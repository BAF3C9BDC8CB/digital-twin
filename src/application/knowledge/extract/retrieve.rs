//! Retrieve 检索层 — GraphRAG 式混合检索（spec S5 / 主文档 §8）。
//!
//! 管线：召回(kg_nodes) → 图扩展(SAME_AS/RELATES/elementId) → 分桶截断
//! → rerank(sigmoid) → 融合排序。任一路故障降级，不整体失败（spec §3）。

use std::sync::Arc;
use std::sync::OnceLock;

use jieba_rs::Jieba;

use crate::application::context::graph_parse::parse_graph_rows;
use crate::application::context::search_mcp::SearchHit;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};
use crate::shared::collections::{DOC_CHUNKS, KG_NODES};
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

/// rerank 分数归一到 [0,1]（S5-D6 实测修正：xinference/SiliconFlow 的
/// `relevance_score` 已是 sigmoid 归一值——强相关对实测 0.9993、无关对 0.0——
/// 二次 sigmoid 会把分布压向 0.5、架空 0.6 权重。故只做防御性 clamp；
/// 若未来接入返回裸 logit 的 provider，需重新评估归一方式）。
pub fn clamp_unit(x: f32) -> f64 {
    x.clamp(0.0, 1.0) as f64
}

/// 虚词表(两字以上,最长优先匹配)——切分中文连续段时剔除。
/// ⚠️ 代码域内容词(注册/缓存/支付 等)绝不可入表,否则关键词通道会漏掉核心实体名。
const STOPWORDS_MULTI: &[&str] = &[
    "怎么样",
    "为什么",
    "是不是",
    "怎么",
    "什么",
    "如何",
    "怎样",
    "哪些",
    "哪个",
    "哪里",
    "是否",
    "可以",
    "需要",
    "应该",
    "可能",
    "一个",
    "一些",
    "这个",
    "那个",
    "这些",
    "那些",
    "进行",
    "通过",
    "关于",
    "对于",
    "基于",
    "为了",
    "之后",
    "之前",
    "时候",
    "还是",
    "或者",
    "以及",
    "没有",
    "不是",
    "我们",
    "你们",
    "他们",
    "它们",
    "使用",
    "用来",
    "直接",
    "大概",
    "问题",
    "方式",
    "方法",
    // 否定/程度修饰词——切掉后内容词(注册/连接/缓存)才能以整段高权重独立
    "无法",
    "不能",
    "不可",
    "未能",
    // 查询意图动词(高置信度:实体名极少以这些词开头,如"分析报告"主体是"报告";
    // 谨慎:配置/测试/设置/查询/发布 等同时是强内容词"配置中心/测试环境",不可入表)
    "查看",
    "对比",
    "组建",
    "分析",
    // 时态/连接/修饰词
    "曾经",
    "正在",
    "已经",
    "将要",
    "同时",
    "另外",
    "主要",
    "相关",
    "具体",
    "详细",
    // 口语/叙事词(常出现在长查询里,jieba 会切出,非检索内容)
    "同样",
    "现在",
    "继续",
    "后来",
    "一起",
    "一直",
    "记录",
    // 弱口语词(浏览器dock 场景实测:想要/只有/发现 等占名额挤掉内容词)
    "想要",
    "只有",
    "两个",
    "发现",
    "点击",
    "额外",
    "增加",
    "就是",
    "中会",
    "这是",
    "怎么回事",
];

/// 虚词表(单字)——切分中文连续段时剔除。
const STOPWORDS_SINGLE: &[char] = &[
    '的', '了', '着', '过', '和', '与', '及', '或', '在', '是', '对', '为', '从', '到', '中', '内',
    '里', '上', '下', '之', '以', '于', '而', '并', '且', '但', '也', '都', '很', '更', '最', '被',
    '把', '给', '让', '用', '按', '向', '往', '跟', '同', '将', '就', '才', '又', '再', '还', '只',
    '个', '这', '那', '些', '么', '吗', '呢', '吧', '啊', '等', '如', '若', '因', '由', '自', '各',
    '每', '其', '该', '本', '何', '谁', '我', '你', '他', '她', '它', '不',
];

/// 关键词候选(去重/排序前):text 已小写。
struct KwCandidate {
    text: String,
    weight: u32,
    pos: usize,
    len: usize,
}

/// ASCII 段产出候选:≥2 字保留,权重 4(与中文词同权,按位置排序)。
/// 曾用权重 5 导致英文词(mcp/ai/dock)霸榜挤掉中文核心词——实测
/// "谷歌浏览器dock图标"查询 kw=[mcp,ai,dock] 而非 [浏览器,谷歌]。
fn push_ascii(cands: &mut Vec<KwCandidate>, buf: &str, start: usize) {
    if buf.chars().count() >= 2 {
        cands.push(KwCandidate {
            text: buf.to_string(),
            weight: 4,
            pos: start,
            len: buf.chars().count(),
        });
    }
}

/// 全局 jieba 单例:词典内嵌于二进制(约 5MB),进程内惰性加载一次。
/// jieba-rs 0.10 的 cut(&self, ...) 线程安全,静态 OnceLock 即可并发使用。
static JIEBA: OnceLock<Jieba> = OnceLock::new();

fn jieba() -> &'static Jieba {
    JIEBA.get_or_init(Jieba::new)
}

/// 中文段交给 jieba 切词:词典级歧义消解("南京市长江大桥"→[南京市,长江大桥])
/// 优于 n-gram 启发式。过滤单字(含单字虚词)与多字虚词后,词按原文位置产出候选。
fn push_cjk_jieba(cands: &mut Vec<KwCandidate>, seg: &str, seg_start: usize) {
    for t in jieba().cut(seg, true) {
        let word = t.word;
        let wc = word.chars().count();
        if wc < 2 {
            continue; // 单字(含单字虚词)全部丢弃,单字 CONTAINS 噪声大
        }
        if STOPWORDS_MULTI.contains(&word) {
            continue; // 多字虚词(怎么/进行/无法/记录…)过滤
        }
        cands.push(KwCandidate {
            text: word.to_string(),
            weight: 4,
            pos: seg_start + t.start, // Token.start 为 char 偏移,与 seg_start 同基准
            len: wc,
        });
    }
}

/// 从查询提取关键词(三态扫描 + jieba 中文切词):
/// - ASCII 字母数字 → ASCII 段,小写化,≥2 字保留(权重5);
/// - 非 ASCII 字母(中文等)→ 中文段,jieba 切词后过滤虚词/单字(权重4);
/// - 其余字符(空白/ASCII 标点/全角标点 ，。！？等)一律视为分隔符,flush 两段。
/// 去重按 text(已小写)保留最高权重;排序 weight desc → pos asc → len desc;截断 max。
/// 供 3.3 关键词补召回使用。
pub(crate) fn keywords_of(query: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = query.chars().collect();
    let mut cands: Vec<KwCandidate> = Vec::new();
    let mut ascii_buf = String::new();
    let mut ascii_start = 0usize;
    let mut cjk_buf = String::new();
    let mut cjk_start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_alphanumeric() {
            // ASCII 段:从中文段切入时先 flush 中文
            if !cjk_buf.is_empty() {
                push_cjk_jieba(&mut cands, &cjk_buf, cjk_start);
                cjk_buf.clear();
            }
            if ascii_buf.is_empty() {
                ascii_start = i;
            }
            ascii_buf.push(ch.to_ascii_lowercase());
        } else if ch.is_alphabetic() && !ch.is_ascii() {
            // 中文段:从 ASCII 段切入时先 flush ASCII
            if !ascii_buf.is_empty() {
                push_ascii(&mut cands, &ascii_buf, ascii_start);
                ascii_buf.clear();
            }
            if cjk_buf.is_empty() {
                cjk_start = i;
            }
            cjk_buf.push(ch);
        } else {
            // 分隔符(含全角标点):flush 两段
            if !ascii_buf.is_empty() {
                push_ascii(&mut cands, &ascii_buf, ascii_start);
                ascii_buf.clear();
            }
            if !cjk_buf.is_empty() {
                push_cjk_jieba(&mut cands, &cjk_buf, cjk_start);
                cjk_buf.clear();
            }
        }
        i += 1;
    }
    if !ascii_buf.is_empty() {
        push_ascii(&mut cands, &ascii_buf, ascii_start);
    }
    if !cjk_buf.is_empty() {
        push_cjk_jieba(&mut cands, &cjk_buf, cjk_start);
    }
    // 去重:按 text 保留最高权重(同权重保留先出现者)
    let mut by_text: std::collections::HashMap<String, KwCandidate> =
        std::collections::HashMap::new();
    for c in cands {
        match by_text.get(&c.text) {
            Some(existing) if existing.weight >= c.weight => {}
            _ => {
                by_text.insert(c.text.clone(), c);
            }
        }
    }
    // 排序:权重降序 → 位置升序 → 长度降序
    let mut all: Vec<KwCandidate> = by_text.into_values().collect();
    all.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then(a.pos.cmp(&b.pos))
            .then(b.len.cmp(&a.len))
    });
    all.truncate(max);
    all.into_iter().map(|c| c.text).collect()
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
        Ok(hits
            .iter()
            .filter_map(|h| parse_seed(h, threshold))
            .collect())
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
    // Document 种子剔除（S5a 实测修正）：文档证据归 world=doc/doc_chunks 负责，
    // kg_nodes 中的 Document 点与同文档块语义重复，会在扁平分布下挤占 knowledge
    // 结果位（实测 top-15 中 13 条为 Document）。与 S5-D11 邻居白名单同一噪声逻辑。
    if labels.iter().any(|l| l == "Document") {
        return None;
    }
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
                head: e
                    .get("head")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                tail: e
                    .get("tail")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                rel_type: e
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
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
            name: nb
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
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
        // 无 elementId 的种子无法定位，静默跳过（payload 恒应携带；缺失记 debug，不刷屏）
        let located: Vec<&Seed> = seeds
            .iter()
            .filter(|s| {
                if s.element_id.is_none() {
                    tracing::debug!("种子 {} 缺少 elementId，跳过图扩展", s.business_id);
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
                let ca = a
                    .get("rel_confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5);
                let cb = b
                    .get("rel_confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5);
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
                let nb_bid =
                    crate::application::sync::kg_bridge::business_id_from_props(nb, nb_eid);
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
                    name: nb
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
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
                let rel_type = row
                    .get("rel_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
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
pub(crate) fn merge_candidates(seeds: &[Seed], nodes: Vec<ExpandedNode>) -> Vec<Candidate> {
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
    for n in nodes {
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
    let mut neighbors: Vec<Candidate> = candidates.iter().filter(|c| c.hop >= 1).cloned().collect();
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
// 编排管线（§3：召回 → 扩展 → 截断 → rerank → 融合 → 证据）
// ---------------------------------------------------------------------------

/// knowledge 世界检索请求。
pub struct RetrieveRequest<'a> {
    pub query: &'a str,
    pub project: Option<&'a str>,
    pub limit: usize,
    pub max_hops: u32,
    pub origin: Option<&'a str>,
}

/// 检索产出：命中 + 世界级降级标记（§5.7.4）。
pub struct RetrieveOutcome {
    pub hits: Vec<SearchHit>,
    pub degraded: Vec<String>,
}

/// 融合排序（§5.4）。降级模式 graph_boost 一阶衰减（种子 1.0、邻居统一 0.5）。
fn fuse(c: &Candidate, rerank_available: bool) -> ScoreBreakdown {
    let (wr, ws, wb) = if rerank_available {
        (0.6, 0.3, 0.1)
    } else {
        (0.0, 0.75, 0.25)
    };
    let boost = if rerank_available {
        c.graph_boost
    } else if c.hop == 0 {
        1.0
    } else {
        0.5
    };
    let rerank = c.rerank_score.unwrap_or(0.0);
    ScoreBreakdown {
        semantic: c.semantic,
        rerank,
        graph_boost: boost,
        final_score: wr * rerank + ws * c.semantic + wb * boost,
    }
}

impl Retriever {
    /// ③ 重排（§5.3）：name+summary 送 reranker，分数 clamp 到 [0,1]。
    /// 返回 false = 未配置或调用失败（调用方走 S5-D7 降级权重并打标）。
    #[allow(clippy::wrong_self_convention)]
    pub(crate) async fn apply_rerank(&self, query: &str, candidates: &mut [Candidate]) -> bool {
        let Some(ref rerank) = self.rerank else {
            return false;
        };
        if candidates.is_empty() {
            return true;
        }
        let docs: Vec<String> = candidates
            .iter()
            .map(|c| format!("{}。{}", c.name, c.summary))
            .collect();
        match rerank.rerank(query, &docs).await {
            Ok(scores) => {
                for (c, x) in candidates.iter_mut().zip(scores) {
                    c.rerank_score = Some(clamp_unit(x));
                }
                true
            }
            Err(e) => {
                tracing::warn!("rerank 失败，回退到降级权重：{e}");
                false
            }
        }
    }

    /// 3.3:关键词补召回——向量召回不足时,用 Memgraph 按 name/keywords
    /// CONTAINS 补种子(解决专有名词节点不进向量 TopK 的问题,如 Q8 Redis 缓存)。
    /// `project` 为 Some 时 Cypher 追加 `AND e.project = '{project}'` 过滤(转义单引号)。
    pub(crate) async fn keyword_recall(
        &self,
        query: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Vec<Seed> {
        let Some(ref graph) = self.graph else {
            return Vec::new();
        };
        // 关键词数量不设小上限(用户 2026-08-13:长句限制后意思可能全变,宁可多查)。
        // 20 为防极端的安全天花板:实测 100 字长句经 jieba 切词+虚词过滤后内容词 ≤15。
        let kws = keywords_of(query, 20);
        if kws.is_empty() {
            return Vec::new();
        }
        // 每 kw 固定 2 条配额:kw 数量放开后若按 limit 均分,20 个 kw 时每词只
        // 取 1 条,精确词(浏览器 exact)与宽泛词(环境 substr)待遇相同,精确词
        // 优势被稀释。固定 2 条让每个内容词都有种子,噪声由 match_kind 分级
        // (exact/prefix 强制保留,substr 正常竞争)与 rerank 收敛。
        let per_kw_cap = 2usize;
        let mut seeds: Vec<Seed> = Vec::new();
        for kw in &kws {
            // 注意:bolt 参数化的 toLower($kw) 在 Memgraph 上返回 0 行(客户端差异),
            // 这里用转义后的字符串字面量拼接,已验证字面量 CONTAINS 正常。
            let kw_lit = kw.replace('\'', "''");
            // 注意:遍历 List 必须用 any()——toString 对 List/Map 返回 null 会废掉
            // 整条 WHERE;any() 逐元素求值,非字符串元素仅该条不匹配,不致命。
            // OR 分支整体加括号,避免与 project 的 AND 结合(AND 优先级高于 OR)。
            let mut where_clause = format!(
                "(toLower(toString(coalesce(e.name, ''))) CONTAINS toLower('{kw_lit}') \
                 OR any(k IN coalesce(e.keywords, []) WHERE toLower(toString(k)) CONTAINS toLower('{kw_lit}')))"
            );
            if let Some(p) = project {
                let p_lit = p.replace('\'', "''");
                where_clause.push_str(&format!(" AND e.project = '{p_lit}'"));
            }
            let cypher = format!(
                r#"
MATCH (e:Entity)
WHERE {where_clause}
RETURN e.entity_id AS seed_id,
       e.name AS seed_name, toString(coalesce(e.summary, '')) AS seed_summary,
       coalesce(e.type, 'Entity') AS seed_type,
       elementId(e) AS seed_element_id,
       CASE WHEN toLower(toString(e.name)) = toLower('{kw_lit}') THEN 'exact'
            WHEN toLower(toString(e.name)) STARTS WITH toLower('{kw_lit}') THEN 'prefix'
            ELSE 'substr' END AS match_kind
ORDER BY CASE match_kind WHEN 'exact' THEN 0 WHEN 'prefix' THEN 1 ELSE 2 END
LIMIT 50
"#,
            );
            match graph
                .read_query(&cypher, std::collections::HashMap::new())
                .await
            {
                Ok(rows) => {
                    tracing::info!(
                        "keyword_recall: kw={} 查询返回 rows={:?}",
                        kw,
                        rows.as_array().map(|a| a.len())
                    );
                    if let Some(arr) = rows.as_array() {
                        // 每 kw 独立计数:严格配额,避免去重影响配额判断
                        let mut kw_count = 0usize;
                        for row in arr {
                            let seed_id = row.get("seed_id").and_then(|v| v.as_str()).unwrap_or("");
                            if seed_id.is_empty() {
                                continue;
                            }
                            if seeds.iter().any(|s| s.business_id == seed_id) {
                                continue;
                            }
                            // 命中质量分级:name 精确 0.95 / 前缀 0.90 / 其余子串 0.80。
                            // exact/prefix(≥0.90)进种子桶并被强制保留;substr(0.80)只
                            // 保证进候选,正常参与 rerank 竞争(见 search_knowledge)。
                            let semantic = match row.get("match_kind").and_then(|v| v.as_str()) {
                                Some("exact") => 0.95,
                                Some("prefix") => 0.90,
                                _ => 0.80,
                            };
                            seeds.push(Seed {
                                business_id: seed_id.to_string(),
                                element_id: row
                                    .get("seed_element_id")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                labels: vec![row
                                    .get("seed_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Entity")
                                    .to_string()],
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
                                semantic,
                            });
                            kw_count += 1;
                            // 本 kw 配额已满 → 继续下一个 kw(不 break 外层循环,
                            // 保证低权重但相关度高的 kw 也有机会进池)
                            if kw_count >= per_kw_cap {
                                break;
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("keyword_recall: kw={} 查询失败: {e}", kw),
            }
        }
        // 种子上限放宽到 2×limit:kw 数量放开后 truncate(limit) 会砍掉
        // 排序靠后的内容词种子,白费查询;2×limit 与向量种子池(3×limit)相比
        // 占比可控,噪声由后续 rerank 收敛。
        seeds.truncate(limit * 2);
        seeds
    }

    /// source_ref 回退：无边 Entity 候选查 MENTIONED_IN（§5.7.2；best-effort 静默失败）。
    async fn fill_source_refs(&self, candidates: &mut [Candidate]) {
        let Some(ref graph) = self.graph else {
            return;
        };
        let need: Vec<String> = candidates
            .iter()
            .filter(|c| c.source_ref.is_none() && c.business_id.starts_with("dt://entity/"))
            .map(|c| c.business_id.clone())
            .collect();
        if need.is_empty() {
            return;
        }
        let cypher = r#"
UNWIND $ids AS eid
MATCH (e:Entity {entity_id: eid})-[:MENTIONED_IN]->(d:Document)
RETURN eid AS eid, d.doc_id AS doc_id
ORDER BY eid, d.doc_id
"#;
        let mut params = std::collections::HashMap::new();
        params.insert("ids".to_string(), serde_json::json!(need));
        let Ok(raw) = graph.read_query(cypher, params).await else {
            return;
        };
        let mut first: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for row in parse_graph_rows(&raw) {
            let (Some(eid), Some(doc)) = (
                row.get("eid").and_then(|v| v.as_str()),
                row.get("doc_id").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            first
                .entry(eid.to_string())
                .or_insert_with(|| doc.to_string());
        }
        for c in candidates.iter_mut() {
            if c.source_ref.is_none() {
                c.source_ref = first.get(&c.business_id).cloned();
            }
        }
    }

    /// knowledge 世界混合检索全流程（§3）。
    pub async fn search_knowledge(
        &self,
        req: &RetrieveRequest<'_>,
    ) -> Result<RetrieveOutcome, DtError> {
        let mut degraded: Vec<String> = Vec::new();
        let limit = req.limit.max(1);

        // ① 召回（embed/vector 失败 → 空结果 + embed_unavailable）
        let mut seeds = match self
            .recall(req.query, req.project, req.origin, (limit * 3) as u64)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("knowledge 召回失败：{e}");
                return Ok(RetrieveOutcome {
                    hits: vec![],
                    degraded: vec!["embed_unavailable".into()],
                });
            }
        };

        // 3.3:关键词补召回(与向量召回取并集,无条件执行——向量可能返回足够数量但
        // 质量差,专有名词节点仍进不来;关键词通道保证 redis/nacos 类节点必进候选)
        let mut kw_ids: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        {
            let kw_seeds = self.keyword_recall(req.query, req.project, limit).await;
            let known: std::collections::HashSet<String> =
                seeds.iter().map(|s| s.business_id.clone()).collect();
            for s in kw_seeds {
                if !known.contains(&s.business_id) {
                    // 记录 kw 种子分级语义分,供强制保留判断(≥0.90=exact/prefix)
                    kw_ids.insert(s.business_id.clone(), s.semantic);
                    seeds.push(s);
                }
            }
        }

        // ② 图扩展（失败 → 仅向量召回 + graph_expansion_failed）
        let mut expansion = ExpansionResult::default();
        if self.graph.is_some() && !seeds.is_empty() {
            let (entity_seeds, biz_seeds): (Vec<Seed>, Vec<Seed>) = seeds
                .iter()
                .cloned()
                .partition(|s| s.labels.iter().any(|l| l == "Entity"));
            let mut failed = false;
            match self.expand_entity(&entity_seeds, req.max_hops).await {
                Ok(r) => {
                    expansion.nodes.extend(r.nodes);
                    expansion.edges.extend(r.edges);
                }
                Err(e) => {
                    tracing::warn!("实体扩展失败：{e}");
                    failed = true;
                }
            }
            match self.expand_business(&biz_seeds).await {
                Ok(r) => {
                    expansion.nodes.extend(r.nodes);
                    expansion.edges.extend(r.edges);
                }
                Err(e) => {
                    tracing::warn!("业务扩展失败：{e}");
                    failed = true;
                }
            }
            if failed {
                degraded.push("graph_expansion_failed".into());
            }
        }

        // ③ 合并 → 边挂接 → 分桶截断
        let mut candidates = merge_candidates(&seeds, expansion.nodes);
        attach_relations(&mut candidates, &expansion.edges);
        bucket_truncate(&mut candidates, rerank_top_n());

        // 3.3:关键词补召回种子强制保留——分桶截断可能把它们挤出。
        // 仅 exact/prefix 命中(semantic≥0.90)无条件保留;substr(0.80)命中
        // 正常参与 rerank 竞争,避免弱命中永久占位。
        if !kw_ids.is_empty() {
            let existing: std::collections::HashSet<String> =
                candidates.iter().map(|c| c.business_id.clone()).collect();
            for s in seeds.iter() {
                if let Some(sem) = kw_ids.get(&s.business_id) {
                    if *sem >= 0.90 && !existing.contains(&s.business_id) {
                        let extra = merge_candidates(std::slice::from_ref(s), vec![]);
                        candidates.extend(extra);
                    }
                }
            }
        }

        // ④ rerank（Task 9；桩恒 false → 降级）
        let reranked = self.apply_rerank(req.query, &mut candidates).await;
        if !reranked {
            degraded.push("rerank_unavailable".into());
        }

        // ⑤ source_ref 回退 + 融合 + 截断 limit，同分 hop 升序
        self.fill_source_refs(&mut candidates).await;
        let mut scored: Vec<(Candidate, ScoreBreakdown)> = candidates
            .into_iter()
            .map(|c| {
                let b = fuse(&c, reranked);
                (c, b)
            })
            .collect();
        scored.sort_by(|(ca, ba), (cb, bb)| {
            bb.final_score
                .partial_cmp(&ba.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(ca.hop.cmp(&cb.hop))
        });
        scored.truncate(limit);

        let hits = scored
            .into_iter()
            .map(|(c, b)| {
                // 文件类型：从 source_ref（doc_id）推断。
                let (file_type, file_type_label) =
                    crate::application::context::search_mcp::infer_file_type_pub(
                        c.source_ref.as_deref(),
                    );
                SearchHit {
                    id: c.business_id,
                    title: c.name,
                    snippet: c.summary.clone(),
                    content: None,
                    source_world: "knowledge".into(),
                    entity_type: c.entity_type,
                    file_type,
                    file_type_label,
                    score: b.final_score,
                    source_ref: c.source_ref,
                    metadata: None,
                    file_path: None,
                    project: None,
                    start_line: None,
                    end_line: None,
                    signature: None,
                    // 知识实体无"方法分析"——摘要即 summary；填入 llm_analysis
                    // 使 Config 等类型也能显示摘要（否则渲染层回退"暂无摘要"）。
                    llm_analysis: Some(c.summary.clone()),
                    calls: vec![],
                    element_id: c.element_id,
                    score_breakdown: Some(b),
                    hop: Some(c.hop),
                    via_same_as: if c.via_same_as { Some(true) } else { None },
                    relations: if c.relations.is_empty() {
                        None
                    } else {
                        Some(c.relations)
                    },
                    evidence: None,
                    rerank_degraded: if reranked { None } else { Some(true) },
                }
            })
            .collect();
        Ok(RetrieveOutcome { hits, degraded })
    }
}

// ---------------------------------------------------------------------------
// ⑤ 证据回填（§5.5.2；with_evidence，仅 knowledge 世界）
// ---------------------------------------------------------------------------

/// 把 doc_chunks 命中按 entity_ids 归属到 top-N 实体：每实体 ≤2 段、按分数降序。
/// 前置条件：entity_ids 数组匹配只在 QdrantRepo 原生 filter（R7）下成立——
/// 本函数只负责分组；filter 构造见 backfill_evidence，数组匹配语义由测试锁死。
fn group_evidence(
    results: &[serde_json::Value],
    ids: &[&str],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut per_entity: std::collections::HashMap<String, Vec<(f64, String)>> =
        std::collections::HashMap::new();
    for hit in results {
        let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let Some(payload) = hit.get("payload") else {
            continue;
        };
        let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let entity_ids: Vec<&str> = payload
            .get("entity_ids")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|e| e.as_str()).collect())
            .unwrap_or_default();
        for id in ids {
            if entity_ids.contains(id) {
                per_entity
                    .entry(id.to_string())
                    .or_default()
                    .push((score, text.to_string()));
            }
        }
    }
    per_entity
        .into_iter()
        .map(|(k, mut chunks)| {
            chunks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            chunks.truncate(2);
            (k, chunks.into_iter().map(|(_, t)| t).collect())
        })
        .collect()
}

impl Retriever {
    /// knowledge top-5 实体从 doc_chunks 回填证据段落（合并单查询；best-effort 静默失败）。
    pub async fn backfill_evidence(&self, query: &str, hits: &mut [SearchHit]) {
        let ids: Vec<&str> = hits.iter().take(5).map(|h| h.id.as_str()).collect();
        if ids.is_empty() {
            return;
        }
        let should: Vec<serde_json::Value> = ids
            .iter()
            .map(|id| serde_json::json!({"key": "entity_ids", "match": {"value": id}}))
            .collect();
        let filter = serde_json::json!({
            "must": [{"key": "source", "match": {"value": "doc"}}],
            "should": should,
        });
        let Ok(embeddings) = self.embed.embed_batch(&[query.to_string()]).await else {
            return;
        };
        let Some(qvec) = embeddings.into_iter().next() else {
            return;
        };
        let Ok(results) = self
            .vector
            .search_with_filter(DOC_CHUNKS, qvec, 20, filter)
            .await
        else {
            return;
        };
        let mut grouped = group_evidence(&results, &ids);
        for hit in hits.iter_mut().take(5) {
            if let Some(chunks) = grouped.remove(&hit.id) {
                hit.evidence = Some(chunks);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
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
            self.captured
                .lock()
                .unwrap()
                .push((query.to_string(), params));
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

    pub(crate) struct MockRerank {
        pub scores: Vec<f32>,
        pub fail: bool,
    }

    #[async_trait::async_trait]
    impl RerankService for MockRerank {
        async fn rerank(&self, _q: &str, docs: &[String]) -> Result<Vec<f32>, DtError> {
            if self.fail {
                return Err(DtError::Repository("rerank 服务不可用".into()));
            }
            Ok(self.scores.iter().take(docs.len()).cloned().collect())
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

    #[tokio::test]
    async fn recall_drops_document_labeled_seeds() {
        let mut doc_hit = seed_hit(0.95, "dt://doc/p/a.md", &["Document"]);
        doc_hit["payload"]["type"] = json!("document");
        let vector = MockVector {
            hits: vec![
                doc_hit,
                seed_hit(0.9, "dt://entity/p/Channel/ifcode", &["Entity"]),
            ],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let r = Retriever::new(None, Arc::new(vector), Arc::new(MockEmbed), None);
        let seeds = r.recall("q", None, None, 60).await.unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].business_id, "dt://entity/p/Channel/ifcode");
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
            entity_row(
                "C",
                serde_json::Value::Null,
                serde_json::Value::Null,
                json!([]),
            ),
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
        let candidates = merge_candidates(&seeds, expansion.nodes);
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
        let mut candidates = merge_candidates(&[mk_seed("A", 0.9), mk_seed("B", 0.8)], vec![]);
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
            let mut c = merge_candidates(&[mk_seed(&format!("S{i}"), *s)], vec![]);
            candidates.append(&mut c);
        }
        let mut nodes = vec![];
        for (i, pr) in [0.7, 0.6, 0.5].iter().enumerate() {
            let mut c = merge_candidates(
                &[],
                vec![ExpandedNode {
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

    #[test]
    fn fuse_degraded_uses_one_hop_boost() {
        // 降级：0.75·semantic + 0.25·boost(一阶)
        let c = merge_candidates(&[mk_seed("A", 0.8)], vec![mk_node("N", "A", 2, 0.9)]);
        let n = c.iter().find(|x| x.business_id == "N").unwrap();
        let b = fuse(n, false);
        // semantic = 0.8×0.25=0.2；boost 一阶 = 0.5（非 0.25）
        assert!((b.graph_boost - 0.5).abs() < 1e-9);
        assert!((b.final_score - (0.75 * 0.2 + 0.25 * 0.5)).abs() < 1e-9);
        assert_eq!(b.rerank, 0.0);
        let a = c.iter().find(|x| x.business_id == "A").unwrap();
        assert!((fuse(a, false).final_score - (0.75 * 0.8 + 0.25 * 1.0)).abs() < 1e-9);
    }

    #[tokio::test]
    async fn pipeline_degraded_full_path() {
        // 向量召回 3 种子：Entity A(0.9) + Entity C(0.7, 无边孤儿) + Knowledge K(0.8, 带 elementId)
        let vector = MockVector {
            hits: vec![
                seed_hit(0.9, "dt://entity/p/Channel/A", &["Entity"]),
                seed_hit(0.7, "dt://entity/p/Concept/C", &["Entity"]),
                {
                    let mut h = seed_hit(0.8, "k-1", &["Knowledge"]);
                    h["payload"]["elementId"] = json!("eid-k1");
                    h["payload"]["type"] = json!("Knowledge");
                    h
                },
            ],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![
                // ① Entity 扩展：A → B(1 跳, routes_to 0.9, doc d1)；C 无边
                json!([entity_row(
                    "dt://entity/p/Channel/A",
                    json!({"entity_id":"dt://entity/p/Service/B","name":"B","type":"Service","summary":"sb"}),
                    json!(1),
                    json!([{"type":"routes_to","confidence":0.9,"evidence":"e","doc_id":"d1",
                            "head":"dt://entity/p/Channel/A","tail":"dt://entity/p/Service/B"}]),
                )]),
                // ② 非 Entity 扩展：空
                json!([]),
                // ③ MENTIONED_IN 回退：仅 C 需要（A/B 已从边拿到 d1；K 非 dt://entity/ 前缀不回退）
                json!([{"eid": "dt://entity/p/Concept/C", "doc_id": "d-c"}]),
            ])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(Some(graph), Arc::new(vector), Arc::new(MockEmbed), None);
        let req = RetrieveRequest {
            query: "q",
            project: Some("p"),
            limit: 10,
            max_hops: 1,
            origin: None,
        };
        let out = r.search_knowledge(&req).await.unwrap();
        // rerank 桩 → 降级标记（条级 + 世界级）
        assert_eq!(out.degraded, vec!["rerank_unavailable"]);
        assert!(out.hits.iter().all(|h| h.rerank_degraded == Some(true)));
        assert!(out.hits.iter().all(|h| h.score_breakdown.is_some()));
        // B 经图扩展进入结果且 hop=1、source_ref 来自最高 confidence 边
        let b = out
            .hits
            .iter()
            .find(|h| h.id == "dt://entity/p/Service/B")
            .unwrap();
        assert_eq!(b.hop, Some(1));
        assert_eq!(b.source_ref.as_deref(), Some("d1"));
        assert_eq!(b.relations.as_ref().unwrap()[0].rel_type, "routes_to");
        // C 无边 → MENTIONED_IN 回退 source_ref
        let c = out
            .hits
            .iter()
            .find(|h| h.id == "dt://entity/p/Concept/C")
            .unwrap();
        assert_eq!(c.source_ref.as_deref(), Some("d-c"));
        assert!(c.relations.is_none());
        // K（非 Entity）不做 MENTIONED_IN 回退
        let k = out.hits.iter().find(|h| h.id == "k-1").unwrap();
        assert!(k.source_ref.is_none());
        // 排序：A(0.75×0.9+0.25×1.0=0.925) > K(0.85) > C(0.775) > B(0.4625)
        assert_eq!(out.hits[0].id, "dt://entity/p/Channel/A");
    }

    #[tokio::test]
    async fn pipeline_embed_failure_returns_empty_with_marker() {
        struct FailEmbed;
        #[async_trait::async_trait]
        impl EmbedService for FailEmbed {
            async fn embed_batch(&self, _t: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
                Err(DtError::Repository("embed 服务不可用".into()))
            }
            async fn health_check(&self) -> Result<HealthStatus, DtError> {
                Ok(HealthStatus::Healthy)
            }
        }
        let r = Retriever::new(None, Arc::new(empty_vector()), Arc::new(FailEmbed), None);
        let req = RetrieveRequest {
            query: "q",
            project: None,
            limit: 10,
            max_hops: 1,
            origin: None,
        };
        let out = r.search_knowledge(&req).await.unwrap();
        assert!(out.hits.is_empty());
        assert_eq!(out.degraded, vec!["embed_unavailable"]);
    }

    #[tokio::test]
    async fn pipeline_graph_failure_falls_back_to_vector_only() {
        struct FailGraph;
        #[async_trait::async_trait]
        impl GraphRepository for FailGraph {
            async fn read_query(
                &self,
                _q: &str,
                _p: HashMap<String, serde_json::Value>,
            ) -> Result<serde_json::Value, DtError> {
                Err(DtError::Repository("图服务不可用".into()))
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
        let vector = MockVector {
            hits: vec![seed_hit(0.9, "dt://entity/p/Channel/A", &["Entity"])],
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let r = Retriever::new(
            Some(Arc::new(FailGraph)),
            Arc::new(vector),
            Arc::new(MockEmbed),
            None,
        );
        let req = RetrieveRequest {
            query: "q",
            project: None,
            limit: 10,
            max_hops: 1,
            origin: None,
        };
        let out = r.search_knowledge(&req).await.unwrap();
        assert_eq!(out.hits.len(), 1); // 种子仍在
        assert!(out.degraded.contains(&"graph_expansion_failed".to_string()));
        assert!(out.degraded.contains(&"rerank_unavailable".to_string()));
    }

    #[test]
    fn rerank_score_clamped_to_unit_interval() {
        assert!((clamp_unit(0.9993) - 0.9993).abs() < 1e-4);
        assert_eq!(clamp_unit(0.0), 0.0);
        assert_eq!(clamp_unit(1.2), 1.0);
        assert_eq!(clamp_unit(-0.3), 0.0);
    }

    #[test]
    fn fuse_full_weights_with_rerank() {
        // 正常：0.6·rerank + 0.3·semantic + 0.1·boost
        let mut c = merge_candidates(&[mk_seed("A", 0.8)], vec![]);
        c[0].rerank_score = Some(0.5);
        let b = fuse(&c[0], true);
        assert!((b.final_score - (0.6 * 0.5 + 0.3 * 0.8 + 0.1 * 1.0)).abs() < 1e-9); // = 0.64
        assert!((b.rerank - 0.5).abs() < 1e-9);
        // 邻居：graph_boost 仍按 0.5^hop（非降级一阶）
        let mut c2 = merge_candidates(&[mk_seed("A", 0.8)], vec![mk_node("N", "A", 2, 0.9)]);
        let n = c2.iter_mut().find(|x| x.business_id == "N").unwrap();
        n.rerank_score = Some(1.0);
        let b2 = fuse(n, true);
        assert!((b2.graph_boost - 0.25).abs() < 1e-9);
    }

    #[tokio::test]
    async fn apply_rerank_writes_clamped_scores_and_fails_open() {
        // 正常路径：provider 已归一的分数原样保留；越界值被 clamp
        let mut c = merge_candidates(&[mk_seed("A", 0.9), mk_seed("B", 0.8)], vec![]);
        let r = Retriever::new(
            None,
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            Some(Arc::new(MockRerank {
                scores: vec![0.9993, 1.5],
                fail: false,
            })),
        );
        assert!(r.apply_rerank("q", &mut c).await);
        assert!(c.iter().all(|x| x.rerank_score.is_some()));
        // merge_candidates 出自 HashMap，候选顺序不定——断言分数集合而非逐位对应
        let mut got: Vec<f64> = c.iter().filter_map(|x| x.rerank_score).collect();
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(got.len(), 2);
        assert!((got[0] - 0.9993).abs() < 1e-4);
        assert_eq!(got[1], 1.0); // 1.5 → clamp 到 1.0
                                 // 失败路径：fails-open → false，候选无分数
        let mut c2 = merge_candidates(&[mk_seed("A", 0.9)], vec![]);
        let r2 = Retriever::new(
            None,
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            Some(Arc::new(MockRerank {
                scores: vec![],
                fail: true,
            })),
        );
        assert!(!r2.apply_rerank("q", &mut c2).await);
        assert!(c2[0].rerank_score.is_none());
        // 未配置：false
        let r3 = Retriever::new(None, Arc::new(empty_vector()), Arc::new(MockEmbed), None);
        assert!(!r3.apply_rerank("q", &mut c2).await);
    }

    #[test]
    fn group_evidence_caps_two_chunks_per_entity_prefers_high_score() {
        let chunk = |eids: &[&str], text: &str, score: f64| {
            json!({
                "score": score,
                "payload": {"text": text, "entity_ids": eids, "source": "doc",
                             "doc_id": "d", "block_index": 0}
            })
        };
        let results = vec![
            chunk(&["E1"], "低分证据", 0.5),
            chunk(&["E1", "E2"], "高分证据", 0.9),
            chunk(&["E1"], "中分证据", 0.7),
            chunk(&["E3"], "别人的证据", 0.99),
            {
                // nacos 形状点：无 text → 跳过
                let mut c = chunk(&["E1"], "", 0.99);
                c["payload"].as_object_mut().unwrap().remove("text");
                c["payload"].as_object_mut().unwrap().remove("entity_ids");
                c
            },
        ];
        let grouped = group_evidence(&results, &["E1", "E2"]);
        assert_eq!(grouped["E1"].len(), 2);
        assert_eq!(grouped["E1"][0], "高分证据"); // 按分数降序
        assert_eq!(grouped["E1"][1], "中分证据");
        assert_eq!(grouped["E2"].len(), 1);
        assert!(!grouped.contains_key("E3")); // 不在 top-N 请求列表
    }

    #[tokio::test]
    async fn backfill_evidence_builds_merged_should_filter() {
        let vector = Arc::new(empty_vector());
        let r = Retriever::new(None, vector.clone(), Arc::new(MockEmbed), None);
        let mut hits = vec![SearchHit {
            id: "E1".into(),
            title: "t".into(),
            snippet: "s".into(),
            content: None,
            source_world: "knowledge".into(),
            entity_type: "Channel".into(),
            file_type: None,
            file_type_label: None,
            score: 0.9,
            source_ref: None,
            metadata: None,
            file_path: None,
            project: None,
            start_line: None,
            end_line: None,
            signature: None,
            llm_analysis: None,
            calls: vec![],
            element_id: None,
            score_breakdown: None,
            hop: Some(0),
            via_same_as: None,
            relations: None,
            evidence: None,
            rerank_degraded: None,
        }];
        r.backfill_evidence("q", &mut hits).await;
        let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
        // must: source=doc；should: entity_ids 数组匹配（原生 filter 前置，§5.5）
        let must = filter["must"].as_array().unwrap();
        assert!(must
            .iter()
            .any(|c| c["key"] == "source" && c["match"]["value"] == "doc"));
        let should = filter["should"].as_array().unwrap();
        assert!(should
            .iter()
            .any(|c| c["key"] == "entity_ids" && c["match"]["value"] == "E1"));
    }

    // -----------------------------------------------------------------------
    // 改动 1:keywords_of 三态扫描 + 虚词切分 + n-gram 加权(矩阵测试)
    // -----------------------------------------------------------------------

    #[test]
    fn keywords_of_cjk_sentence_drops_stopwords() {
        // 中文整句:jieba 切 [怎么,使用,注册,功能,呢],虚词(怎么/使用/呢)剔除
        let kws = keywords_of("怎么使用注册功能呢", 8);
        assert_eq!(kws, vec!["注册", "功能"]);
        assert!(!kws.iter().any(|k| k == "怎么" || k == "使用" || k == "呢"));
    }

    #[test]
    fn keywords_of_mixed_cn_en_git_register() {
        // 中英混合:ASCII 段独立成词(git w5),中文段 jieba 切 [注册,逻辑](w4)
        assert_eq!(keywords_of("git 注册逻辑", 3), vec!["git", "注册", "逻辑"]);
    }

    #[test]
    fn keywords_of_redis_how_to_use() {
        // 粘连场景:redis 与 缓存 拆开;虚词 怎么/用 剔除
        assert_eq!(keywords_of("redis缓存怎么用", 3), vec!["redis", "缓存"]);
    }

    #[test]
    fn keywords_of_pure_english_split_by_space() {
        assert_eq!(
            keywords_of("Redis cache eviction policy", 8),
            vec!["redis", "cache", "eviction", "policy"]
        );
    }

    #[test]
    fn keywords_of_fullwidth_punctuation_is_delimiter() {
        // 全角标点(，)是分隔符而非词内容——修复前会拼成 "注册，逻辑" 一个 kw
        assert_eq!(keywords_of("注册，逻辑", 8), vec!["注册", "逻辑"]);
    }

    #[test]
    fn keywords_of_dedups_repeated_words() {
        assert_eq!(keywords_of("缓存 缓存", 8), vec!["缓存"]);
        assert_eq!(keywords_of("git git", 8), vec!["git"]);
    }

    #[test]
    fn keywords_of_empty_single_char_and_pure_punctuation() {
        assert!(keywords_of("", 8).is_empty());
        assert!(keywords_of("注", 8).is_empty());
        assert!(keywords_of("，，！？", 8).is_empty());
        assert!(keywords_of("a b", 8).is_empty()); // 单字 ASCII 丢弃
    }

    #[test]
    fn keywords_of_long_sentence_keeps_content_and_drops_stopwords() {
        let q = "同样还是无法注册，组建多人团队，配置了 redis 缓存和 mysql 数据库，查看之前的 git 记录对比";
        let kws = keywords_of(q, 30);
        assert!(kws.contains(&"注册".to_string()));
        assert!(kws.contains(&"git".to_string()));
        assert!(!kws.contains(&q.to_string())); // 整句绝不作为关键词
        assert!(kws.len() <= 30);
        // 虚词被剔除:同样/还是/了/和/之前的 都不在关键词里
        assert!(!kws.iter().any(|k| k == "同样" || k == "还是" || k == "了"));
    }

    #[test]
    fn keywords_of_max_zero_returns_empty() {
        assert!(keywords_of("注册逻辑", 0).is_empty());
        assert!(keywords_of("", 0).is_empty());
    }

    #[test]
    fn keywords_of_snake_case_env_var_splits() {
        // 下划线分隔的 ASCII 段各自成词,单字段丢弃
        assert_eq!(
            keywords_of("DT_KG_RERANK_TOP_N", 8),
            vec!["dt", "kg", "rerank", "top"]
        );
    }

    #[test]
    fn keywords_of_stopword_longest_match_first() {
        // 最长优先:怎么样/为什么 整体剔除,而非残留 怎/为 等单字
        assert!(keywords_of("怎么样", 8).is_empty());
        assert!(keywords_of("为什么", 8).is_empty());
        assert!(keywords_of("怎么使用", 8).is_empty());
    }

    #[test]
    fn keywords_of_mixed_ascii_cjk_keeps_chinese_content() {
        // 回归(2026-08-13 用户实测):长中英混合查询里 ASCII 词(mcp/ai/dock)
        // 曾以权重 5 霸榜,中文核心词(浏览器/谷歌)被挤出。权重统一后
        // 按位置排序,中文内容词必须进 kw。
        let q = "我想要的是只有两个浏览器，一个是我个人使用的谷歌浏览器，另外一个就是谷歌浏览器mcp环境，给AI使用的，我发现我点击dock中的谷歌浏览器，dock中会额外增加一个谷歌浏览器图标，这是怎么回事？";
        let kws = keywords_of(q, 5);
        assert!(
            kws.iter().any(|k| k == "浏览器" || k == "谷歌"),
            "中文核心词(浏览器/谷歌)必须进 kw,got={kws:?}"
        );
        // 虚词(想要/只有/发现/点击/增加/就是)全滤
        for w in ["想要", "只有", "发现", "点击", "增加", "就是"] {
            assert!(!kws.contains(&w.to_string()), "虚词 {w} 未滤,got={kws:?}");
        }
        // ASCII 词仍保留但不再霸榜
        assert!(kws.iter().any(|k| k == "mcp"));
    }

    // -----------------------------------------------------------------------
    // 改动 2-5:keyword_recall(project 过滤 / keywords OR 分支 / elementId /
    //           match_kind 分级 / LIMIT 50)+ search_knowledge 强制保留分级
    // -----------------------------------------------------------------------

    pub(crate) fn kw_row(
        seed_id: &str,
        name: &str,
        seed_type: &str,
        match_kind: &str,
    ) -> serde_json::Value {
        json!({
            "seed_id": seed_id,
            "seed_name": name,
            "seed_summary": format!("summary-{seed_id}"),
            "seed_type": seed_type,
            "seed_element_id": format!("eid-{seed_id}"),
            "match_kind": match_kind,
        })
    }

    #[tokio::test]
    async fn keyword_recall_builds_cypher_and_grades_match_kind() {
        // "git 注册逻辑" → kws = [git, 注册逻辑, 注册],每个 kw 一条查询
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![
                json!([kw_row("g1", "git", "Entity", "exact")]),
                json!([kw_row("c1", "注册逻辑梳理", "Knowledge", "prefix")]),
                json!([kw_row("c2", "注册流程文档", "Knowledge", "substr")]),
            ])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        let seeds = r
            .keyword_recall("git 注册逻辑", Some("offen-pay"), 10)
            .await;
        assert_eq!(seeds.len(), 3);
        let g1 = seeds.iter().find(|s| s.business_id == "g1").unwrap();
        assert!((g1.semantic - 0.95).abs() < 1e-9); // exact
        assert_eq!(g1.element_id.as_deref(), Some("eid-g1")); // elementId 解析(改动 3)
        let c1 = seeds.iter().find(|s| s.business_id == "c1").unwrap();
        assert!((c1.semantic - 0.90).abs() < 1e-9); // prefix
        let c2 = seeds.iter().find(|s| s.business_id == "c2").unwrap();
        assert!((c2.semantic - 0.80).abs() < 1e-9); // substr
                                                    // Cypher 断言:project 过滤 + keywords OR 分支 + elementId + match_kind 分级
        let (cypher, _params) = graph.captured.lock().unwrap()[0].clone();
        assert!(cypher.contains("AND e.project = 'offen-pay'"));
        assert!(cypher.contains(
            "any(k IN coalesce(e.keywords, []) WHERE toLower(toString(k)) CONTAINS toLower('git'))"
        ));
        assert!(cypher.contains("elementId(e) AS seed_element_id"));
        assert!(cypher.contains("THEN 'exact'"));
        assert!(cypher.contains("STARTS WITH toLower('git')"));
        assert!(cypher.contains(
            "ORDER BY CASE match_kind WHEN 'exact' THEN 0 WHEN 'prefix' THEN 1 ELSE 2 END"
        ));
        assert!(cypher.contains("LIMIT 50"));
        assert!(!cypher.contains("LIMIT 10")); // 改动 5:LIMIT 10 → 50
    }

    #[tokio::test]
    async fn keyword_recall_without_project_omits_project_clause() {
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![json!([kw_row(
                "c1",
                "缓存服务",
                "Knowledge",
                "substr"
            )])])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        let seeds = r.keyword_recall("缓存", None, 10).await;
        assert_eq!(seeds.len(), 1);
        assert!((seeds[0].semantic - 0.80).abs() < 1e-9);
        let (cypher, _) = graph.captured.lock().unwrap()[0].clone();
        assert!(!cypher.contains("e.project"));
    }

    #[tokio::test]
    async fn keyword_recall_escapes_single_quote_in_project() {
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
        let _ = r.keyword_recall("缓存", Some("off'en"), 10).await;
        let (cypher, _) = graph.captured.lock().unwrap()[0].clone();
        assert!(cypher.contains("e.project = 'off''en'"));
    }

    #[tokio::test]
    async fn keyword_recall_dedups_across_kws() {
        // 两个 kw 命中同一 seed_id → 去重;两个 kw 都发了查询
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![
                json!([kw_row("dup", "redis", "Entity", "exact")]),
                json!([kw_row("dup", "缓存", "Entity", "prefix")]),
            ])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        let seeds = r.keyword_recall("redis缓存", None, 10).await;
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].business_id, "dup");
        assert_eq!(graph.captured.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn keyword_recall_quota_distributes_across_kws() {
        // 回归(修复前缺陷):kw1 命中多时先到先得独占 limit,kw2 的查询
        // 永远不会发出("git 注册逻辑" 只出 git 不出注册)。
        // 新语义:limit=6, kws=3(git/注册逻辑/注册)→ per_kw_cap=2,
        // 每个 kw 各收 2 条,三次查询都发。
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![
                json!([
                    kw_row("g1", "git", "Entity", "exact"),
                    kw_row("g2", "git-commit", "Entity", "prefix"),
                    kw_row("g3", "git-branch", "Entity", "substr"),
                    kw_row("g4", "git-rebase", "Entity", "substr"),
                    kw_row("g5", "git-stash", "Entity", "substr"),
                ]),
                json!([
                    kw_row("r1", "注册逻辑", "Entity", "exact"),
                    kw_row("r2", "注册中心", "Entity", "prefix"),
                    kw_row("r3", "注册页面", "Entity", "substr"),
                ]),
                json!([
                    kw_row("r4", "注册表单", "Entity", "prefix"),
                    kw_row("r5", "注册入口", "Entity", "prefix"),
                ]),
            ])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        // "git 注册逻辑" → kws=["git","注册逻辑","注册"],limit=6 → per_kw_cap=2
        let seeds = r.keyword_recall("git 注册逻辑", None, 6).await;
        assert_eq!(seeds.len(), 6, "三个 kw 各收 2 条配额");
        assert_eq!(
            graph.captured.lock().unwrap().len(),
            3,
            "所有 kw 的查询都必须发出(修复前只发 1 次)"
        );
        assert!(
            seeds.iter().any(|s| s.business_id == "r1"),
            "kw2 种子(注册逻辑)必须进池"
        );
        assert!(
            seeds.iter().any(|s| s.business_id == "r4"),
            "kw3 种子(注册)必须进池"
        );
        assert!(
            seeds.iter().any(|s| s.business_id == "g1"),
            "kw1 种子(git)必须进池"
        );
        // exact 命中(0.95)优先于 substr(0.80)进池
        let g_sem = seeds
            .iter()
            .find(|s| s.business_id == "g1")
            .unwrap()
            .semantic;
        assert_eq!(g_sem, 0.95);
    }

    #[tokio::test]
    async fn keyword_recall_no_graph_and_empty_kws_short_circuit() {
        // graph=None → 直接返回空,不发查询
        let r = Retriever::new(None, Arc::new(empty_vector()), Arc::new(MockEmbed), None);
        assert!(r.keyword_recall("redis", None, 10).await.is_empty());
        // 无关键词(单字/纯标点)→ 不发查询
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::new()),
            captured: Mutex::new(vec![]),
        });
        let r2 = Retriever::new(
            Some(graph.clone()),
            Arc::new(empty_vector()),
            Arc::new(MockEmbed),
            None,
        );
        assert!(r2.keyword_recall("的", None, 10).await.is_empty());
        assert!(r2.keyword_recall("，，！", None, 10).await.is_empty());
        assert_eq!(graph.captured.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn search_knowledge_force_keeps_only_exact_prefix_kw_seeds() {
        // 构造 seed_cap 个高分向量种子,把 kw 种子挤出种子桶:
        // exact 命中(0.95)被分桶截断后仍被无条件保留;substr 命中(0.80)
        // 不再强制保留,正常参与 rerank 竞争。
        let top_n = rerank_top_n();
        let seed_cap = ((top_n as f64) * 0.6).ceil() as usize;
        let mut vector_hits: Vec<serde_json::Value> = Vec::new();
        for i in 0..seed_cap {
            let mut h = seed_hit(
                0.999 - i as f64 * 0.001,
                &format!("dt://entity/p/Service/V{i}"),
                &["Entity"],
            );
            h["payload"]["name"] = json!(format!("v{i}"));
            vector_hits.push(h);
        }
        let vector = MockVector {
            hits: vector_hits,
            captured_filter: Mutex::new(None),
            captured_limit: Mutex::new(None),
        };
        let graph = Arc::new(MockGraph {
            responses: Mutex::new(VecDeque::from(vec![
                // ① kw "redis":name 精确命中 → semantic 0.95
                json!([kw_row(
                    "dt://entity/p/Service/redis",
                    "redis",
                    "Entity",
                    "exact"
                )]),
                // ② kw "缓存":子串命中 → semantic 0.80
                json!([kw_row(
                    "dt://entity/p/Service/cache",
                    "缓存服务",
                    "Entity",
                    "substr"
                )]),
                // ③ Entity 图扩展:无邻居
                json!([]),
                // ④ MENTIONED_IN 回退:无来源
                json!([]),
            ])),
            captured: Mutex::new(vec![]),
        });
        let r = Retriever::new(Some(graph), Arc::new(vector), Arc::new(MockEmbed), None);
        let req = RetrieveRequest {
            query: "redis缓存怎么用",
            project: None,
            limit: seed_cap + 5,
            max_hops: 1,
            origin: None,
        };
        let out = r.search_knowledge(&req).await.unwrap();
        assert!(
            out.hits
                .iter()
                .any(|h| h.id == "dt://entity/p/Service/redis"),
            "exact 命中(0.95)应被无条件保留"
        );
        assert!(
            !out.hits
                .iter()
                .any(|h| h.id == "dt://entity/p/Service/cache"),
            "substr 命中(0.80)不应强制保留"
        );
        assert_eq!(out.hits.len(), seed_cap + 1);
    }
}
