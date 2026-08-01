//! **Resolver Stage** — detects conflicts and inconsistencies across worlds.
//!
//! Cross-references information from different worlds (e.g., Reality vs Knowledge,
//! Reality vs Memory) to flag potential conflicts as alerts.

use crate::domain::error::DtError;
use async_trait::async_trait;

use super::ContextStage;
use crate::application::context::models::{Alert, AlertSeverity, ContextState, WorldItem};

/// Detects conflicts and flags alerts.
pub struct ResolverStage;

#[async_trait]
impl ContextStage for ResolverStage {
    fn name(&self) -> &str {
        "resolver"
    }

    async fn process(&self, mut state: ContextState) -> Result<ContextState, DtError> {
        let mut alerts: Vec<Alert> = Vec::new();

        // 1.  Cross-reference Reality ↔ Memory (experiences)
        detect_reality_memory_conflicts(&state.reality_deduped, &state.memory_deduped, &mut alerts);

        // 2.  Cross-reference Reality ↔ Knowledge (concept/playbook mismatch)
        detect_reality_knowledge_conflicts(
            &state.reality_deduped,
            &state.knowledge_deduped,
            &mut alerts,
        );

        // 3.  Detect suspiciously low relevance (all items below threshold)
        detect_low_coverage(
            &state.reality_deduped,
            &state.knowledge_deduped,
            &state.memory_deduped,
            &state.semantic_deduped,
            &state.reasoning_deduped,
            &mut alerts,
        );

        let alert_count = alerts.len();
        state.alerts.append(&mut alerts);

        if alert_count > 0 {
            tracing::info!("[resolver] raised {alert_count} alerts");
        }

        Ok(state)
    }
}

// ---------------------------------------------------------------------------
// Conflict detection rules
// ---------------------------------------------------------------------------

/// Detect when a Memory-World experience warns about a Reality-World entity.
///
/// Example: an Experience node saying "payment-svc has a memory leak"
/// and a Reality node for "payment-svc" → flag a warning.
fn detect_reality_memory_conflicts(
    reality: &[WorldItem],
    memory: &[WorldItem],
    alerts: &mut Vec<Alert>,
) {
    // For now, use a simple name-matching strategy.
    for exp in memory {
        let exp_name = exp.label.to_lowercase();
        let exp_content = exp.content.to_lowercase();

        // Look for warning keywords in experience content
        let has_warning = exp_content.contains("bug")
            || exp_content.contains("leak")
            || exp_content.contains("crash")
            || exp_content.contains("fail")
            || exp_content.contains("error")
            || exp_content.contains("issue")
            || exp_content.contains("warning");

        if !has_warning {
            continue;
        }

        // Cross-reference with reality items
        for real in reality {
            let real_name = real.label.to_lowercase();

            if exp_name.contains(&real_name) || real_name.contains(&exp_name) {
                alerts.push(Alert {
                    severity: AlertSeverity::Warning,
                    source: "resolver".into(),
                    message: format!(
                        "Memory world contains a warning for '{}': {}",
                        real.label, exp.content
                    ),
                    related_id: Some(real.id.clone()),
                });
                break;
            }
        }
    }
}

/// Detect when Reality content contradicts Knowledge best-practices.
///
/// Example: Reality shows a service using HTTP, but Knowledge has a playbook
/// that says "all services must use HTTPS".
fn detect_reality_knowledge_conflicts(
    _reality: &[WorldItem],
    _knowledge: &[WorldItem],
    _alerts: &mut Vec<Alert>,
) {
    // Placeholder: real conflict detection will use Cypher queries to
    // cross-reference config values against knowledge-base constraints.
}

/// Detect when all worlds return very few or no results.
///
/// Indicates that the system has no relevant knowledge for this task,
/// which is worth surfacing as an informational alert.
fn detect_low_coverage(
    reality: &[WorldItem],
    knowledge: &[WorldItem],
    memory: &[WorldItem],
    semantic: &[WorldItem],
    reasoning: &[WorldItem],
    alerts: &mut Vec<Alert>,
) {
    let total = reality.len() + knowledge.len() + memory.len() + semantic.len() + reasoning.len();

    if total == 0 {
        alerts.push(Alert {
            severity: AlertSeverity::Info,
            source: "resolver".into(),
            message: "No relevant information found in any knowledge world for this task. "
                .to_string(),
            related_id: None,
        });
    } else if total < 3 {
        alerts.push(Alert {
            severity: AlertSeverity::Info,
            source: "resolver".into(),
            message: format!(
                "Limited context available: only {total} items found across all worlds."
            ),
            related_id: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::models::{AlertSeverity, ContextOptions, WorldItem};

    fn make_state() -> ContextState {
        ContextState::new("test task", &ContextOptions::default())
    }

    #[tokio::test]
    async fn resolver_name() {
        assert_eq!(ResolverStage.name(), "resolver");
    }

    #[tokio::test]
    async fn resolver_detects_memory_warning() {
        let mut state = make_state();
        state.reality_deduped = vec![
            WorldItem::new("r1", "payment-svc", "Payment processing service").with_type("Service"),
            WorldItem::new("r2", "user-svc", "User service").with_type("Service"),
            WorldItem::new("r3", "order-svc", "Order service").with_type("Service"),
        ];
        state.memory_deduped = vec![WorldItem::new(
            "m1",
            "payment-svc memory leak",
            "The payment-svc has a known memory leak",
        )
        .with_type("Experience")];

        let stage = ResolverStage;
        let result = stage.process(state).await.unwrap();

        assert_eq!(result.alerts.len(), 1);
        assert_eq!(result.alerts[0].severity, AlertSeverity::Warning);
        assert!(result.alerts[0].message.contains("payment-svc"));
        assert!(result.alerts[0].message.contains("memory leak"));
    }

    #[tokio::test]
    async fn resolver_no_conflicts_when_clean() {
        let mut state = make_state();
        state.reality_deduped = vec![
            WorldItem::new("r1", "user-svc", "User service").with_type("Service"),
            WorldItem::new("r2", "payment-svc", "Payment service").with_type("Service"),
            WorldItem::new("r3", "order-svc", "Order service").with_type("Service"),
        ];
        state.memory_deduped =
            vec![
                WorldItem::new("m1", "Deploy success", "Deployment went smoothly")
                    .with_type("Experience"),
            ];

        let stage = ResolverStage;
        let result = stage.process(state).await.unwrap();
        // No warning keywords in the experience and no low coverage
        assert!(result.alerts.is_empty());
    }

    #[tokio::test]
    async fn resolver_low_coverage_alert() {
        let state = make_state();

        let stage = ResolverStage;
        let result = stage.process(state).await.unwrap();

        assert_eq!(result.alerts.len(), 1);
        assert_eq!(result.alerts[0].severity, AlertSeverity::Info);
        assert!(result.alerts[0].message.contains("No relevant information"));
    }

    #[tokio::test]
    async fn resolver_limited_coverage_alert() {
        let mut state = make_state();
        state.reality_deduped = vec![WorldItem::new("r1", "test", "test").with_type("Service")];

        let stage = ResolverStage;
        let result = stage.process(state).await.unwrap();

        assert_eq!(result.alerts.len(), 1);
        assert!(result.alerts[0].message.contains("Limited context"));
    }

    #[test]
    fn detect_reality_memory_conflicts_no_warning_keywords() {
        let reality = vec![WorldItem::new("r1", "svc", "content").with_type("S")];
        let memory = vec![WorldItem::new("m1", "svc", "everything is fine").with_type("E")];
        let mut alerts = Vec::new();
        detect_reality_memory_conflicts(&reality, &memory, &mut alerts);
        assert!(alerts.is_empty());
    }

    #[test]
    fn detect_low_coverage_zero_items() {
        let mut alerts = Vec::new();
        detect_low_coverage(&[], &[], &[], &[], &[], &mut alerts);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, AlertSeverity::Info);
    }
}
