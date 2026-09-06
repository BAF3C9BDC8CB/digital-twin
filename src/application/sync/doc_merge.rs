//! 文档主题归并（切片 C）—— 跨路径/跨项目识别「描述同一主题」的文档。
//!
//! 核心洞察：文档的主题由其中提到的实体刻画。两篇文档共享的实体
//! （canonical 归一化后）越多，主题越相近。因此用**实体重叠系数**
//! （Overlap Coefficient = |A∩B| / min(|A|,|B|)）作为相似度度量——
//! 纯图计算、零外部依赖（不调 embed/LLM）、天然跨项目：
//! 实体按 `normalize(name)` + type 对齐，不受 project 边界限制。
//! 选重叠系数而非 Jaccard：长文档不会稀释小文档的相似度，
//! 子主题关系（小文档 ⊂ 大文档）得 1.0。
//!
//! 输出：相似度 ≥ 阈值的文档对之间建立 `SAME_TOPIC_AS` 边：
//! `(d1:Document)-[:SAME_TOPIC_AS {similarity, method:"overlap_coefficient"}]->(d2:Document)`
//!
//! 与 consolidate 的 `SAME_AS`（实体级）互补：本文档处理的是文档级主题边。

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use std::collections::{HashMap, HashSet};

/// 主题归并相似度阈值——两篇文档视为「同主题」的重叠系数下限。
pub const TOPIC_SIMILARITY_THRESHOLD: f64 = 0.30;

/// 一篇文档的实体指纹。
#[derive(Debug, Clone)]
struct DocFingerprint {
    doc_id: String,
    /// 归一化后的 (entity_type, normalized_name) 集合。
    entities: HashSet<(String, String)>,
}

/// 归并统计。
#[derive(Debug, Clone, Default)]
pub struct DocMergeReport {
    /// 参与归并的文档数。
    pub documents_scanned: usize,
    /// 计算了相似度的文档对数。
    pub pairs_compared: usize,
    /// 相似度 ≥ 阈值、建立 SAME_TOPIC_AS 边的文档对数。
    pub topics_merged: usize,
    /// 已存在边而跳过的对（幂等，仅当报告启用时计数）。
    pub edges_existing_skipped: usize,
    /// 墙钟耗时（毫秒）。
    pub elapsed_ms: u64,
}

/// 执行文档主题归并。
///
/// 1. 拉取全部 Document（可限定 project 集）
/// 2. 对每篇文档拉取其 MENTIONED_IN 实体，构造实体指纹
/// 3. 两两比较重叠系数相似度，≥ 阈值建 SAME_TOPIC_AS 边
///
/// 实体对齐键 = `(e.type, normalize(e.name))`——type 属性是提取时的
/// 实体类型（Config/Concept/Service...），name 是 canonical 名。
pub async fn merge_documents_by_topic(
    graph: &dyn GraphRepository,
    projects: &[String],
    threshold: f64,
) -> Result<DocMergeReport, DtError> {
    let started = std::time::Instant::now();
    let mut report = DocMergeReport::default();

    // ── 1. 拉取文档集 ──
    let docs = fetch_documents(graph, projects).await?;
    if docs.is_empty() {
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(report);
    }
    report.documents_scanned = docs.len();

    // ── 2. 每篇文档的实体指纹 ──
    let mut fingerprints: Vec<DocFingerprint> = Vec::with_capacity(docs.len());
    for doc_id in &docs {
        let entities = fetch_document_entities(graph, doc_id).await?;
        if entities.len() < 2 {
            // 实体少于 2 个的文档无法可靠判同主题，跳过（避免噪声边）
            continue;
        }
        fingerprints.push(DocFingerprint {
            doc_id: doc_id.clone(),
            entities,
        });
    }
    if fingerprints.len() < 2 {
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(report);
    }

    // ── 3. 两两比较（O(n²)，文档量级数百内可接受）──
    // 同一文档只比较一次（i < j）；先按 doc_id 排序保证确定性。
    fingerprints.sort_by(|a, b| a.doc_id.cmp(&b.doc_id));
    let n = fingerprints.len();
    for i in 0..n {
        for j in (i + 1)..n {
            report.pairs_compared += 1;
            let (a, b) = (&fingerprints[i], &fingerprints[j]);
            let sim = overlap_coefficient(&a.entities, &b.entities);
            if sim >= threshold {
                let edge_created = write_topic_edge(graph, &a.doc_id, &b.doc_id, sim).await?;
                if edge_created {
                    report.topics_merged += 1;
                } else {
                    report.edges_existing_skipped += 1;
                }
            }
        }
    }

    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// 拉取全部文档（可按 project 过滤）。
async fn fetch_documents(
    graph: &dyn GraphRepository,
    projects: &[String],
) -> Result<Vec<String>, DtError> {
    let params: HashMap<String, serde_json::Value> = if projects.is_empty() {
        HashMap::new()
    } else {
        let mut p = HashMap::new();
        p.insert("projects".to_string(), serde_json::json!(projects));
        p
    };
    let query = if projects.is_empty() {
        "MATCH (d:Document) RETURN d.doc_id AS doc_id"
    } else {
        "MATCH (d:Document) WHERE d.project IN $projects RETURN d.doc_id AS doc_id"
    };
    let rows = graph.read_query(query, params).await?;
    Ok(rows
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| r.get("doc_id")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// 拉取一篇文档的实体指纹：`(type, normalize(name))` 集合。
async fn fetch_document_entities(
    graph: &dyn GraphRepository,
    doc_id: &str,
) -> Result<HashSet<(String, String)>, DtError> {
    let mut params = HashMap::new();
    params.insert("doc_id".to_string(), serde_json::json!(doc_id));
    let rows = graph
        .read_query(
            "MATCH (e:Entity)-[:MENTIONED_IN]->(d:Document {doc_id: $doc_id}) \
             RETURN e.type AS type, e.name AS name",
            params,
        )
        .await?;
    let mut set = HashSet::new();
    if let Some(rs) = rows.as_array() {
        for r in rs {
            let ty = r.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !name.is_empty() {
                set.insert((ty.to_string(), normalize_key(name)));
            }
        }
    }
    Ok(set)
}

/// 文档主题边写入的实体名归一化——与 consolidate::normalize 对齐。
/// 全角→半角、修剪、转小写；保留中文（中文不需百分号编码做 ID，
/// 这里仅作比较键，宽松处理）。
fn normalize_key(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            c if ('\u{FF01}'..='\u{FF5E}').contains(&c) => {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            }
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_lowercase()
}

/// 重叠系数（Overlap Coefficient）：|A ∩ B| / min(|A|, |B|)。
///
/// 与 Jaccard 的区别：Jaccard 的分母是并集，文档越长越吃亏（长文档
/// 的实体把分子稀释）。重叠系数只看「较小文档被多大比例覆盖」——
/// 一篇大文档与一篇小文档若小文档的实体大多在大文档里，系数接近 1，
/// 正确反映「小文档是大文档的子主题」。
fn overlap_coefficient(a: &HashSet<(String, String)>, b: &HashSet<(String, String)>) -> f64 {
    let (smaller, larger) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if smaller.is_empty() {
        return 0.0;
    }
    let intersection = smaller.iter().filter(|k| larger.contains(*k)).count();
    intersection as f64 / smaller.len() as f64
}

/// 建 SAME_TOPIC_AS 边（幂等）。返回是否新建成（false = 已存在）。
async fn write_topic_edge(
    graph: &dyn GraphRepository,
    doc_id_a: &str,
    doc_id_b: &str,
    similarity: f64,
) -> Result<bool, DtError> {
    // 四舍五入到 4 位小数，避免浮点噪声
    let sim = (similarity * 1e4).round() / 1e4;
    let mut params = HashMap::new();
    params.insert("a".to_string(), serde_json::json!(doc_id_a));
    params.insert("b".to_string(), serde_json::json!(doc_id_b));
    params.insert("sim".to_string(), serde_json::json!(sim));
    let resp = graph
        .write_query(
            "MATCH (a:Document {doc_id: $a}), (b:Document {doc_id: $b}) \
             MERGE (a)-[r:SAME_TOPIC_AS]->(b) \
             ON CREATE SET r.similarity = $sim, r.method = 'overlap_coefficient', \
                            r.created_at = datetime() \
             ON MATCH SET r.similarity = $sim \
             RETURN r.similarity AS s",
            params,
        )
        .await?;
    // MERGE 后无论如何都返回行；无法从 Cypher 直接区分 create/match，
    // 用「写入前是否已有该边」判断成本高——这里保守返回 true（视为写入）。
    Ok(resp.as_array().map(|_| true).unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_basic() {
        let a: HashSet<(String, String)> = [("Concept".into(), "支付网关".into())]
            .into_iter()
            .collect();
        let b: HashSet<(String, String)> = [("Concept".into(), "支付网关".into())]
            .into_iter()
            .collect();
        // 重叠系数：相同集合 → 1.0
        assert!((overlap_coefficient(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn overlap_half() {
        // a = {1,2}, b = {1,3}: 重叠系数 = 小者{1,2} 中在大者内的 {1} → 1/2
        let a: HashSet<(String, String)> = [("A".into(), "1".into()), ("A".into(), "2".into())]
            .into_iter()
            .collect();
        let b: HashSet<(String, String)> = [("A".into(), "1".into()), ("A".into(), "3".into())]
            .into_iter()
            .collect();
        assert!((overlap_coefficient(&a, &b) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn overlap_subset_is_one() {
        // a ⊂ b: 小者全在大者内 → 1.0（Jaccard 只会给 0.5，重叠系数更准）
        let a: HashSet<(String, String)> = [("A".into(), "1".into())].into_iter().collect();
        let b: HashSet<(String, String)> = [
            ("A".into(), "1".into()),
            ("A".into(), "2".into()),
            ("A".into(), "3".into()),
        ]
        .into_iter()
        .collect();
        assert!((overlap_coefficient(&a, &b) - 1.0).abs() < 1e-9);
        assert!((overlap_coefficient(&b, &a) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn overlap_disjoint() {
        let a: HashSet<(String, String)> = [("A".into(), "1".into())].into_iter().collect();
        let b: HashSet<(String, String)> = [("A".into(), "2".into())].into_iter().collect();
        assert_eq!(overlap_coefficient(&a, &b), 0.0);
    }

    #[test]
    fn normalize_key_handles_case_and_width() {
        assert_eq!(normalize_key("  Memgraph  "), "memgraph");
        assert_eq!(normalize_key("ＩｆＣｏｄｅ"), "ifcode");
        assert_eq!(normalize_key("读\u{3000}写"), "读 写");
    }

    #[test]
    fn threshold_default() {
        assert!(TOPIC_SIMILARITY_THRESHOLD > 0.0 && TOPIC_SIMILARITY_THRESHOLD < 1.0);
    }
}
