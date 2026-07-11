//! **Ranker Stage** — semantic relevance ranking with score filtering.
//!
//! Sorts retrieved items by relevance score (descending), filters out items
//! below the configured minimum score threshold, and caps the number of items
//! per world if a max limit is configured.

use async_trait::async_trait;
use crate::domain::error::DtError;

use crate::application::context::models::{ContextState, WorldItem};
use super::ContextStage;

/// Ranks items per world and filters low-relevance results.
pub struct RankerStage;

#[async_trait]
impl ContextStage for RankerStage {
    fn name(&self) -> &str {
        "ranker"
    }

    async fn process(&self, mut state: ContextState) -> Result<ContextState, DtError> {
        let min_score = state.options.min_score.unwrap_or(0.3);
        let max_items = state.options.max_items_per_world.unwrap_or(20);

        rank_world(
            "reality",
            &state.reality_raw,
            &mut state.reality_ranked,
            min_score,
            max_items,
        );
        rank_world(
            "knowledge",
            &state.knowledge_raw,
            &mut state.knowledge_ranked,
            min_score,
            max_items,
        );
        rank_world(
            "memory",
            &state.memory_raw,
            &mut state.memory_ranked,
            min_score,
            max_items,
        );
        rank_world(
            "semantic",
            &state.semantic_raw,
            &mut state.semantic_ranked,
            min_score,
            max_items,
        );
        rank_world(
            "runtime",
            &state.runtime_raw,
            &mut state.runtime_ranked,
            min_score,
            max_items,
        );
        rank_world(
            "reasoning",
            &state.reasoning_raw,
            &mut state.reasoning_ranked,
            min_score,
            max_items,
        );

        Ok(state)
    }
}

// ---------------------------------------------------------------------------
// Ranking logic
// ---------------------------------------------------------------------------

/// Rank items for a single world: sort, filter, cap.
fn rank_world(
    world: &str,
    raw: &[WorldItem],
    ranked: &mut Vec<WorldItem>,
    min_score: f64,
    max_items: usize,
) {
    let original = raw.len();

    let mut sorted = raw.to_vec();
    sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let filtered: Vec<WorldItem> = sorted
        .into_iter()
        .filter(|item| item.score >= min_score)
        .take(max_items)
        .collect();

    let dropped = original - filtered.len();
    if dropped > 0 {
        tracing::info!(
            "[ranker] {world}: kept {}/{} items (min_score={}, max_items={})",
            filtered.len(),
            original,
            min_score,
            max_items,
        );
    }

    *ranked = filtered;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::models::{ContextOptions, WorldItem};

    fn make_items(scores: &[f64]) -> Vec<WorldItem> {
        scores
            .iter()
            .enumerate()
            .map(|(i, &score)| {
                WorldItem::new(format!("id{i}"), format!("Item {i}"), format!("content {i}"))
                    .with_score(score)
                    .with_type("Test")
            })
            .collect()
    }

    fn make_state(raw_items: Vec<WorldItem>) -> ContextState {
        let mut state = ContextState::new("test task", &ContextOptions::default());
        state.reality_raw = raw_items;
        state
    }

    #[tokio::test]
    async fn ranker_sorts_by_score_descending() {
        let items = make_items(&[0.3, 0.9, 0.5, 0.7]);
        let state = make_state(items);

        let stage = RankerStage;
        let result = stage.process(state).await.unwrap();

        assert_eq!(result.reality_ranked.len(), 3); // 0.3 filtered out
        assert!((result.reality_ranked[0].score - 0.9).abs() < 0.001);
        assert!((result.reality_ranked[1].score - 0.7).abs() < 0.001);
        assert!((result.reality_ranked[2].score - 0.5).abs() < 0.001);
    }

    #[tokio::test]
    async fn ranker_filters_below_min_score() {
        let items = make_items(&[0.2, 0.4, 0.6, 0.8]);
        let mut state = make_state(items);
        state.options.min_score = Some(0.5);

        let stage = RankerStage;
        let result = stage.process(state).await.unwrap();

        assert_eq!(result.reality_ranked.len(), 2);
        for item in &result.reality_ranked {
            assert!(item.score >= 0.5);
        }
    }

    #[tokio::test]
    async fn ranker_respects_max_items() {
        let items: Vec<WorldItem> = (0..30)
            .map(|i| {
                WorldItem::new(format!("id{i}"), format!("Item {i}"), format!("content {i}"))
                    .with_score(0.9)
                    .with_type("Test")
            })
            .collect();

        let mut state = make_state(items);
        state.options.max_items_per_world = Some(5);

        let stage = RankerStage;
        let result = stage.process(state).await.unwrap();

        assert_eq!(result.reality_ranked.len(), 5);
    }

    #[tokio::test]
    async fn ranker_empty_input() {
        let state = ContextState::new("test", &ContextOptions::default());
        let stage = RankerStage;
        let result = stage.process(state).await.unwrap();
        assert!(result.reality_ranked.is_empty());
    }

    #[tokio::test]
    async fn ranker_name() {
        assert_eq!(RankerStage.name(), "ranker");
    }

    #[test]
    fn rank_world_filters_and_sorts() {
        let raw = make_items(&[0.2, 0.9, 0.5, 0.3]);
        let mut ranked = Vec::new();
        rank_world("test", &raw, &mut ranked, 0.5, 10);
        assert_eq!(ranked.len(), 2);
        assert!((ranked[0].score - 0.9).abs() < 0.001);
    }
}
