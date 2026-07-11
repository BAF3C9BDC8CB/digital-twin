//! **Dedup Stage** — merges duplicate / near-duplicate information across items.
//!
//! Identifies items that describe the same underlying entity and merges them,
//! preserving all distinct source references.  Two items are considered
//! duplicates when their content similarity exceeds a threshold.

use std::collections::HashSet;
use async_trait::async_trait;
use crate::domain::error::DtError;

use crate::application::context::models::{ContextState, WorldItem};
use super::ContextStage;

/// Deduplication threshold (Jaccard similarity on word sets).
/// Items above this threshold in the same world are merged.
const SIMILARITY_THRESHOLD: f64 = 0.75;

/// Merges duplicate items within each world.
pub struct DedupStage;

#[async_trait]
impl ContextStage for DedupStage {
    fn name(&self) -> &str {
        "dedup"
    }

    async fn process(&self, mut state: ContextState) -> Result<ContextState, DtError> {
        let worlds: [(&str, fn(&ContextState) -> &[WorldItem], fn(&mut ContextState) -> &mut Vec<WorldItem>); 6] = [
            ("reality", |s: &ContextState| s.reality_ranked.as_slice(), |s: &mut ContextState| &mut s.reality_deduped),
            ("knowledge", |s: &ContextState| s.knowledge_ranked.as_slice(), |s: &mut ContextState| &mut s.knowledge_deduped),
            ("memory", |s: &ContextState| s.memory_ranked.as_slice(), |s: &mut ContextState| &mut s.memory_deduped),
            ("semantic", |s: &ContextState| s.semantic_ranked.as_slice(), |s: &mut ContextState| &mut s.semantic_deduped),
            ("runtime", |s: &ContextState| s.runtime_ranked.as_slice(), |s: &mut ContextState| &mut s.runtime_deduped),
            ("reasoning", |s: &ContextState| s.reasoning_ranked.as_slice(), |s: &mut ContextState| &mut s.reasoning_deduped),
        ];

        for (world, get_ranked, get_deduped) in &worlds {
            let ranked = get_ranked(&state);
            let original_count = ranked.len();
            let deduped = deduplicate(ranked);
            let merged = original_count - deduped.len();

            if merged > 0 {
                tracing::info!(
                    "[dedup] {world}: {original_count} → {} (merged {merged} duplicates)",
                    deduped.len(),
                );
            }

            *get_deduped(&mut state) = deduped;
        }

        Ok(state)
    }
}

// ---------------------------------------------------------------------------
// Deduplication logic
// ---------------------------------------------------------------------------

/// Deduplicate a list of items, merging near-duplicates.
///
/// Strategy: iterate over items sorted by score descending.  For each item,
/// check if it's similar to any already-kept item.  If so, merge the source
/// references; otherwise keep it as a new unique item.
fn deduplicate(items: &[WorldItem]) -> Vec<WorldItem> {
    let mut kept: Vec<WorldItem> = Vec::with_capacity(items.len());

    for item in items {
        let mut merged = false;

        for existing in kept.iter_mut() {
            if is_similar(item, existing) {
                // Merge sources from the duplicate into the existing item
                for src in &item.sources {
                    if !existing.sources.contains(src) {
                        existing.sources.push(src.clone());
                    }
                }
                // Take the higher score
                if item.score > existing.score {
                    existing.score = item.score;
                }
                merged = true;
                break;
            }
        }

        if !merged {
            kept.push(item.clone());
        }
    }

    kept
}

/// Check if two items are semantically similar based on word overlap.
///
/// Uses Jaccard similarity on the word sets of their content fields.
/// Returns `true` when similarity exceeds `SIMILARITY_THRESHOLD`.
fn is_similar(a: &WorldItem, b: &WorldItem) -> bool {
    // Same entity type and ID with high similarity → definitely duplicate
    if a.id == b.id && a.entity_type == b.entity_type {
        return true;
    }

    // Different worlds → never deduplicate across worlds
    // (world is embedded in the id prefix, e.g., "reality::0")

    let words_a = word_set(&a.content);
    let words_b = word_set(&b.content);

    if words_a.is_empty() || words_b.is_empty() {
        return false;
    }

    let union = words_a.union(&words_b).count();
    let intersection = words_a.intersection(&words_b).count();

    if union == 0 {
        return false;
    }

    let jaccard = intersection as f64 / union as f64;
    jaccard >= SIMILARITY_THRESHOLD
}

/// Extract a set of lowercase words from content text.
fn word_set(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| w.len() > 1)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::models::WorldItem;

    #[test]
    fn deduplicate_no_duplicates() {
        let items = vec![
            WorldItem::new("a", "A", "unique content alpha"),
            WorldItem::new("b", "B", "different content beta"),
            WorldItem::new("c", "C", "completely different text"),
        ];
        let result = deduplicate(&items);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn deduplicate_merges_duplicates() {
        let items = vec![
            WorldItem::new("a", "A", "payment service handles transactions")
                .with_score(0.9)
                .with_source("src1")
                .with_type("Service"),
            WorldItem::new("b", "B", "payment service handles transactions")
                .with_score(0.7)
                .with_source("src2")
                .with_type("Service"),
        ];
        let result = deduplicate(&items);
        assert_eq!(result.len(), 1);
        // Higher score should be kept
        assert!((result[0].score - 0.9).abs() < 0.001);
        // Both sources should be merged
        assert!(result[0].sources.contains(&"src1".to_string()));
        assert!(result[0].sources.contains(&"src2".to_string()));
    }

    #[test]
    fn deduplicate_preserves_order_by_score() {
        let items = vec![
            WorldItem::new("a", "A", "top content").with_score(0.95).with_type("T"),
            WorldItem::new("b", "B", "mid content").with_score(0.8).with_type("T"),
            WorldItem::new("c", "C", "low content").with_score(0.6).with_type("T"),
        ];
        let result = deduplicate(&items);
        assert_eq!(result.len(), 3);
        assert!((result[0].score - 0.95).abs() < 0.001);
        assert!((result[1].score - 0.8).abs() < 0.001);
        assert!((result[2].score - 0.6).abs() < 0.001);
    }

    #[test]
    fn is_similar_identical() {
        let a = WorldItem::new("x", "X", "the quick brown fox jumps over the lazy dog")
            .with_type("T");
        let b = WorldItem::new("y", "Y", "the quick brown fox jumps over the lazy dog")
            .with_type("T");
        assert!(is_similar(&a, &b));
    }

    #[test]
    fn is_similar_different() {
        let a = WorldItem::new("x", "X", "deployment configuration for payment service").with_type("T");
        let b = WorldItem::new("y", "Y", "database schema for user table").with_type("T");
        assert!(!is_similar(&a, &b));
    }

    #[test]
    fn is_similar_same_id_and_type() {
        let a = WorldItem::new("id-123", "A", "content a").with_type("Service");
        let b = WorldItem::new("id-123", "B", "content b").with_type("Service");
        assert!(is_similar(&a, &b));
    }

    #[test]
    fn word_set_extracts_words() {
        let words = word_set("Payment Service handles transactions");
        assert!(words.contains("payment"));
        assert!(words.contains("service"));
        assert!(words.contains("handles"));
        assert!(words.contains("transactions"));
        assert_eq!(words.len(), 4);
    }

    #[test]
    fn word_set_filters_short_and_punctuation() {
        let words = word_set("a i o e u z, x! y? w;");
        assert!(words.is_empty());
    }

    #[tokio::test]
    async fn dedup_stage_process() {
        let mut state = ContextState::new("test", &crate::application::context::models::ContextOptions::default());
        state.reality_ranked = vec![
            WorldItem::new("a", "A", "payment service").with_score(0.9).with_type("S"),
            WorldItem::new("b", "B", "payment service").with_score(0.8).with_type("S"),
        ];

        let stage = DedupStage;
        let result = stage.process(state).await.unwrap();
        assert_eq!(result.reality_deduped.len(), 1);
    }

    #[tokio::test]
    async fn dedup_stage_name() {
        assert_eq!(DedupStage.name(), "dedup");
    }
}
