//! 跨世界融合 — RRF（自 application/search.rs 迁入）+ SearchHit 级融合。

use std::collections::HashMap;

use crate::application::context::search_mcp::SearchHit;

/// 用于多源融合的排序搜索结果项。
#[derive(Debug, Clone)]
pub struct RankedItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
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

/// SearchHit 级 RRF：以 `{source_world}:{id}` 为键跨列表去重累分，
/// 最终 score 为 RRF 分（量级 1/(k+rank)，仅用于排序与展示）。
pub fn rrf_hits(world_lists: Vec<Vec<SearchHit>>, k: f64, limit: usize) -> Vec<SearchHit> {
    let mut score_map: HashMap<String, (f64, SearchHit)> = HashMap::new();
    for list in &world_lists {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::SearchHit;

    fn hit(world: &str, id: &str, score: f64) -> SearchHit {
        SearchHit {
            id: id.into(),
            title: id.into(),
            snippet: String::new(),
            source_world: world.into(),
            entity_type: "X".into(),
            file_type: None,
            file_type_label: None,
            score,
            source_ref: None,
            file_path: None,
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
    fn rrf_hits_keys_by_world_and_id_and_sets_rrf_score() {
        let code = vec![hit("code", "a", 0.9), hit("code", "b", 0.8)];
        let kn = vec![hit("knowledge", "b", 0.7), hit("knowledge", "c", 0.6)];
        let fused = rrf_hits(vec![code, kn], 60.0, 10);
        // 键为 world:id —— code:b 与 knowledge:b 是不同条目，不去重
        assert_eq!(fused.len(), 4);
        // rank1 的 RRF 分 = 1/(60+1)
        assert!((fused[0].score - 1.0 / 61.0).abs() < 1e-9);
    }

    #[test]
    fn rrf_hits_respects_limit() {
        let l1 = (0..5)
            .map(|i| hit("code", &format!("x{i}"), 0.9))
            .collect::<Vec<_>>();
        assert_eq!(rrf_hits(vec![l1], 60.0, 3).len(), 3);
    }
}
