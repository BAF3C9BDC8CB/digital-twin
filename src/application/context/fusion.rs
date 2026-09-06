//! 跨世界融合 — RRF（自 application/search.rs 迁入）+ SearchHit 级融合。

use std::collections::HashMap;

use crate::application::context::search_mcp::SearchHit;

/// 用于多源融合的排序搜索结果项。
#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub content: Option<String>,
    pub source_ref: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub llm_analysis: Option<String>,
    pub source_world: String,
    pub entity_type: String,
    pub score: f64,
}

/// 倒数排名融合（RRF）— 将多个来源的排序列表合并为单个排序列表。
///
/// 每个列表中的每个条目获得 `1.0 / (k + rank)` 的分值，
/// 其中 `k` 是阻尼常数（默认 60）。分值在所有列表中累加，
/// 然后按分值降序排序条目。
pub fn reciprocal_rank_fusion(
    rank_lists: Vec<Vec<RankedItem>>,
    k: f64,
    limit: usize,
) -> Vec<RankedItem> {
    let mut score_map: HashMap<String, (f64, RankedItem)> = HashMap::new();

    for list in &rank_lists {
        for (rank, item) in list.iter().enumerate() {
            let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
            let key = format!("{}:{}", item.source_world, item.id);
            score_map
                .entry(key)
                .and_modify(|(score, _)| *score += rrf_score)
                .or_insert_with(|| (rrf_score, item.clone()));
        }
    }

    let mut fused: Vec<_> = score_map.into_values().collect();
    fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    fused
        .into_iter()
        .take(limit)
        .map(|(score, mut item)| {
            item.score = score;
            item
        })
        .collect()
}

/// SearchHit 级 RRF：以 `{source_world}:{id}` 为键跨列表去重累分。
///
/// 排序仍按 RRF 累分（跨世界名次融合），但**输出 score 保留该条命中的
/// 原始最大分**（各列表中的最高相似度），而非 RRF 分——RRF 分量级
/// 1/(k+rank)≈0.016 会误导下游：展示层以为相关度极低，LLM 过滤的
/// 0.6/0.9 阈值分桶全部失效导致误杀。原始分保持真实相关度语义。
pub fn rrf_hits(world_lists: Vec<Vec<SearchHit>>, k: f64, limit: usize) -> Vec<SearchHit> {
    let mut score_map: HashMap<String, (f64, f64, SearchHit)> = HashMap::new();
    for list in &world_lists {
        for (rank, item) in list.iter().enumerate() {
            let rrf_score = 1.0 / (k + (rank as f64 + 1.0));
            let key = format!("{}:{}", item.source_world, item.id);
            score_map
                .entry(key)
                .and_modify(|(rrf, orig_max, _)| {
                    *rrf += rrf_score;
                    if item.score > *orig_max {
                        *orig_max = item.score;
                    }
                })
                .or_insert_with(|| (rrf_score, item.score, item.clone()));
        }
    }
    let mut fused: Vec<_> = score_map.into_values().collect();
    fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    fused
        .into_iter()
        .take(limit)
        .map(|(_, orig_max, mut item)| {
            item.score = orig_max;
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::SearchHit;

    fn hit(world: &str, id: &str, score: f64) -> SearchHit {
        SearchHit {
            id: id.into(),
            title: id.into(),
            snippet: String::new(),
            content: None,
            source_world: world.into(),
            entity_type: "X".into(),
            file_type: None,
            file_type_label: None,
            score,
            source_ref: None,
            metadata: None,
            file_path: None,
            project: None,
            start_line: None,
            end_line: None,
            signature: None,
            calls: vec![],
            element_id: None,
            llm_analysis: None,
            score_breakdown: None,
            hop: None,
            via_same_as: None,
            relations: None,
            evidence: None,
            rerank_degraded: None,
        }
    }

    #[test]
    fn rrf_hits_keys_by_world_and_id_and_keeps_original_score() {
        let code = vec![hit("code", "a", 0.9), hit("code", "b", 0.8)];
        let kn = vec![hit("knowledge", "b", 0.7), hit("knowledge", "c", 0.6)];
        let fused = rrf_hits(vec![code, kn], 60.0, 10);
        // 键为 world:id —— code:b 与 knowledge:b 是不同条目，不去重
        assert_eq!(fused.len(), 4);
        // 排序按 RRF 分（rank1 条目居前），但 score 保留原始分而非 RRF 分
        let code_a = fused.iter().find(|h| h.id == "a" && h.source_world == "code").unwrap();
        let kn_c = fused.iter().find(|h| h.id == "c" && h.source_world == "knowledge").unwrap();
        assert!((code_a.score - 0.9).abs() < 1e-9);
        assert!((kn_c.score - 0.6).abs() < 1e-9);
        // rank1 条目(code:a / knowledge:b)必须在 rank2 条目(code:b / knowledge:c)之前
        let pos = |id: &str, w: &str| fused.iter().position(|h| h.id == id && h.source_world == w).unwrap();
        assert!(pos("a", "code") < pos("b", "code"));
        assert!(pos("b", "knowledge") < pos("c", "knowledge"));
        // 确保没有任何 score 是 RRF 量级（1/61≈0.016）
        assert!(fused.iter().all(|h| h.score > 0.5));
    }

    #[test]
    fn rrf_hits_respects_limit() {
        let l1 = (0..5)
            .map(|i| hit("code", &format!("x{i}"), 0.9))
            .collect::<Vec<_>>();
        assert_eq!(rrf_hits(vec![l1], 60.0, 3).len(), 3);
    }
}
