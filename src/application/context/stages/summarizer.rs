//! **Summarizer Stage** — token budget compression and alert injection.
//!
//! When the total estimated tokens exceed the configured budget, this stage
//! compresses lower-priority items by truncating their content.  It also
//! injects high-severity experience alerts into the alert list.

use async_trait::async_trait;
use crate::domain::error::DtError;

use crate::application::context::models::{Alert, AlertSeverity, ContextState, WorldItem};
use super::ContextStage;

/// Priority order for trimming: lower-priority worlds get trimmed first.
#[allow(dead_code)]
const WORLD_PRIORITY: [&str; 6] = [
    "semantic",  // Trim first — broad, less precise
    "knowledge",
    "reasoning",
    "memory",
    "runtime",
    "reality",   // Trim last — most directly actionable
];

/// Compresses content by token budget and injects experience alerts.
pub struct SummarizerStage;

#[async_trait]
impl ContextStage for SummarizerStage {
    fn name(&self) -> &str {
        "summarizer"
    }

    async fn process(&self, mut state: ContextState) -> Result<ContextState, DtError> {
        // 1.  Inject high-severity experience items as alerts
        inject_experience_alerts(&state.memory_deduped, &mut state.alerts);

        // 2.  Compute current token estimate
        let max_tokens = state.options.max_tokens.unwrap_or(4096);

        // Build a temporary AggregatedContext to estimate tokens
        let estimated = estimate_current_tokens(&state);

        if estimated > max_tokens {
            let excess = estimated.saturating_sub(max_tokens);
            tracing::info!(
                "[summarizer] token budget exceeded: {estimated} > {max_tokens} (excess: {excess})"
            );
            compress_by_priority(&mut state, max_tokens);
        } else {
            tracing::info!(
                "[summarizer] token budget ok: {estimated}/{max_tokens}"
            );
        }

        Ok(state)
    }
}

// ---------------------------------------------------------------------------
// Experience → Alert injection
// ---------------------------------------------------------------------------

/// Check memory-world items for high-severity experiences and inject them
/// as alerts for the user.
fn inject_experience_alerts(memory: &[WorldItem], alerts: &mut Vec<Alert>) {
    for item in memory {
        // Only process items with Experience or BugFix types
        if item.entity_type != "Experience" && item.entity_type != "BugFix" {
            continue;
        }

        let content_lower = item.content.to_lowercase();

        let severity = if content_lower.contains("critical")
            || content_lower.contains("p0")
            || content_lower.contains("outage")
            || content_lower.contains("data loss")
            || content_lower.contains("security")
        {
            AlertSeverity::Critical
        } else if content_lower.contains("bug")
            || content_lower.contains("leak")
            || content_lower.contains("crash")
            || content_lower.contains("warning")
            || content_lower.contains("deprecated")
        {
            AlertSeverity::Warning
        } else {
            continue; // Not a notable experience
        };

        alerts.push(Alert {
            severity,
            source: "summarizer".into(),
            message: format!(
                "[{}] {}: {}",
                item.entity_type, item.label, item.content
            ),
            related_id: Some(item.id.clone()),
        });
    }
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Estimate the token count from the current deduped state.
fn estimate_current_tokens(state: &ContextState) -> usize {
    let worlds: [&[WorldItem]; 6] = [
        &state.reality_deduped,
        &state.knowledge_deduped,
        &state.memory_deduped,
        &state.semantic_deduped,
        &state.runtime_deduped,
        &state.reasoning_deduped,
    ];

    let mut chars = 0usize;
    for items in &worlds {
        for item in *items {
            chars += item.char_count();
        }
    }
    for alert in &state.alerts {
        chars += alert.message.len() + alert.source.len() + 32;
    }
    chars / 4
}

// ---------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------

type WorldAccessor = fn(&mut ContextState) -> &mut Vec<WorldItem>;

/// Compress items across worlds by truncating content to fit within a token
/// budget.  Lower-priority worlds are trimmed first.
fn compress_by_priority(state: &mut ContextState, max_tokens: usize) {
    let world_map: [(&str, WorldAccessor); 6] = [
        ("semantic", |s: &mut ContextState| &mut s.semantic_deduped),
        ("knowledge", |s: &mut ContextState| &mut s.knowledge_deduped),
        ("reasoning", |s: &mut ContextState| &mut s.reasoning_deduped),
        ("memory", |s: &mut ContextState| &mut s.memory_deduped),
        ("runtime", |s: &mut ContextState| &mut s.runtime_deduped),
        ("reality", |s: &mut ContextState| &mut s.reality_deduped),
    ];

    let mut current = estimate_current_tokens(state);

    // Phase 1: trim low-priority worlds first
    for (world_name, get_world) in &world_map {
        if current <= max_tokens {
            break;
        }
        let items = get_world(state);
        let before = items.len();
        trim_world_items(items, 200); // Truncate content to 200 chars
        current = estimate_current_tokens(state);
        tracing::info!(
            "[summarizer] trimmed {world_name}: {before} items, now {current}/{max_tokens} tokens",
        );
    }

    // Phase 2: if still over budget, drop lowest-score items from low-priority worlds
    if current > max_tokens {
        for (_world_name, get_world) in &world_map {
            if current <= max_tokens {
                break;
            }
            let items = get_world(state);
            // Sort by score ascending and remove lowest until budget fits
            items.sort_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            while current > max_tokens && !items.is_empty() {
                let removed = items.remove(0);
                current = current.saturating_sub(removed.char_count() / 4);
            }
        }
    }
}

/// Truncate the content of each item in a world to at most `max_chars`.
fn trim_world_items(items: &mut [WorldItem], max_chars: usize) {
    for item in items.iter_mut() {
        if item.content.len() > max_chars {
            item.content.truncate(max_chars);
            item.content.push_str("...");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::models::{AlertSeverity, ContextOptions, WorldItem};

    #[test]
    fn trim_world_items_truncates() {
        let mut items = vec![
            WorldItem::new("a", "A", &"x".repeat(500)),
            WorldItem::new("b", "B", "short"),
        ];
        trim_world_items(&mut items, 200);
        assert!(items[0].content.len() <= 203); // 200 + "..."
        assert!(items[0].content.ends_with("..."));
        assert_eq!(items[1].content, "short"); // unchanged
    }

    #[test]
    fn inject_experience_alerts_critical() {
        let memory = vec![
            WorldItem::new("m1", "Critical bug", "Security vulnerability causing data loss")
                .with_type("Experience"),
        ];
        let mut alerts = Vec::new();
        inject_experience_alerts(&memory, &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Critical);
    }

    #[test]
    fn inject_experience_alerts_warning() {
        let memory = vec![
            WorldItem::new("m2", "Memory leak", "Payment service has a memory leak")
                .with_type("BugFix"),
        ];
        let mut alerts = Vec::new();
        inject_experience_alerts(&memory, &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Warning);
    }

    #[test]
    fn inject_experience_alerts_skips_non_experience() {
        let memory = vec![
            WorldItem::new("m3", "Deploy", "Deployment completed").with_type("Deployment"),
            WorldItem::new("m4", "Nice day", "Everything is fine").with_type("Experience"),
        ];
        let mut alerts = Vec::new();
        inject_experience_alerts(&memory, &mut alerts);
        assert!(alerts.is_empty());
    }

    #[test]
    fn estimate_current_tokens_empty() {
        let state = ContextState::new("test", &ContextOptions::default());
        assert_eq!(estimate_current_tokens(&state), 0);
    }

    #[test]
    fn estimate_current_tokens_with_items() {
        let mut state = ContextState::new("test", &ContextOptions::default());
        state.reality_deduped = vec![
            WorldItem::new("r1", "Payment Service", "Handles payment processing").with_type("S"),
        ];
        let tokens = estimate_current_tokens(&state);
        // "Payment Service"(16) + "Handles payment processing"(26) + "S"(1) = 43 / 4 = 10
        assert_eq!(tokens, 10);
    }

    #[test]
    fn compress_by_priority_trims_low_priority() {
        let mut state = ContextState::new("test", &ContextOptions::default());
        state.semantic_deduped = vec![
            WorldItem::new("s1", "Doc", &"x".repeat(1000)).with_type("D"),
        ];
        let before = estimate_current_tokens(&state);
        compress_by_priority(&mut state, 50);
        let after = estimate_current_tokens(&state);
        assert!(after < before || after <= 50);
        // After trimming, the item should be truncated (ends with ...)
        if !state.semantic_deduped.is_empty() {
            assert!(state.semantic_deduped[0].content.ends_with("..."));
        }
    }

    #[tokio::test]
    async fn summarizer_stage_name() {
        assert_eq!(SummarizerStage.name(), "summarizer");
    }

    #[tokio::test]
    async fn summarizer_within_budget() {
        let mut state = ContextState::new("test", &ContextOptions::default());
        state.reality_deduped = vec![
            WorldItem::new("r1", "Sv", "OK").with_type("S"),
        ];
        // Copy before processing
        let expected_items = state.reality_deduped.clone();

        let stage = SummarizerStage;
        let result = stage.process(state).await.unwrap();

        // Should be unchanged within budget
        assert_eq!(result.reality_deduped.len(), expected_items.len());
    }

    #[tokio::test]
    async fn summarizer_injects_experience_alerts() {
        let mut state = ContextState::new("test", &ContextOptions::default());
        state.memory_deduped = vec![
            WorldItem::new("m1", "Memory Leak", "Critical memory leak causing outage")
                .with_type("Experience"),
        ];

        let stage = SummarizerStage;
        let result = stage.process(state).await.unwrap();

        let experience_alerts: Vec<&Alert> = result
            .alerts
            .iter()
            .filter(|a| a.source == "summarizer")
            .collect();
        assert!(!experience_alerts.is_empty());
        assert!(experience_alerts.iter().any(|a| a.severity == AlertSeverity::Critical));
    }
}
