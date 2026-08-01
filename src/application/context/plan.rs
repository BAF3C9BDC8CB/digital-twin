//! **PlanService** — Playbook matching & execution plan generation (4.3).
//!
//! Takes a task description + aggregated context, matches against known
//! Playbooks in the Knowledge World, and generates a step-by-step
//! execution plan with an impact estimate.
//!
//! # MCP tool: `dt_plan`
//!
//! ```text
//! dt_plan(task: str, context?: dict, domain?: str)
//!   → ExecutionPlan JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// Input for plan generation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanRequest {
    /// Task description to plan for.
    pub task: String,
    /// Domain hint (e.g. "支付", "auth").
    pub domain: Option<String>,
    /// Optional pre-built context from dt_context.
    pub context_json: Option<String>,
    /// Preferred plan style ("fast": minimal, "thorough": exhaustive).
    pub style: Option<String>,
}

/// A reference to a matched playbook.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlaybookRef {
    /// Playbook knowledge ID.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// How well this playbook matches (0..1).
    pub match_score: f64,
    /// Domain of the playbook.
    pub domain: String,
}

/// A single step in an execution plan.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    /// Sequence number (1-based).
    pub order: usize,
    /// Action title (e.g. "Review payment timeout config").
    pub action: String,
    /// Target file / entity / service.
    pub target: Option<String>,
    /// Estimated effort in minutes.
    pub estimated_minutes: Option<u32>,
    /// Prerequisites (step order numbers).
    pub requires: Vec<usize>,
    /// Notes or rationale.
    pub notes: Option<String>,
}

/// Estimated impact of the plan.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImpactEstimate {
    /// Affected services.
    pub services: Vec<String>,
    /// Affected config files.
    pub configs: Vec<String>,
    /// Risk level: "low" | "medium" | "high".
    pub risk: String,
    /// Estimated total effort in minutes.
    pub total_minutes: u32,
}

/// Output of the plan tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionPlan {
    /// The original task.
    pub task: String,
    /// Matched playbook, if any.
    pub matched_playbook: Option<PlaybookRef>,
    /// Ordered plan steps.
    pub plan: Vec<PlanStep>,
    /// Impact estimation.
    pub estimated_impact: ImpactEstimate,
    /// Whether this is a best-effort plan (no playbook match).
    pub is_generic: bool,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// Generates execution plans from tasks + playbooks.
#[async_trait::async_trait]
pub trait PlanServiceTrait: Send + Sync {
    /// Generate an execution plan for a task.
    async fn plan(&self, request: &PlanRequest) -> Result<ExecutionPlan, DtError>;
}

/// Canonical implementation of [`PlanServiceTrait`].
pub struct PlanService {
    graph: Arc<dyn GraphRepository>,
}

impl PlanService {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    /// Attempt to match the task against known Playbooks in the graph.
    async fn find_matching_playbook(
        &self,
        task: &str,
        _domain: Option<&str>,
    ) -> Result<Option<PlaybookRef>, DtError> {
        // Query the Knowledge World for playbooks relevant to the task
        let cypher = r#"
            MATCH (p:Playbook)
            WHERE p.name CONTAINS $fragment
               OR p.description CONTAINS $fragment
               OR p.domain CONTAINS $fragment
            RETURN elementId(p) AS id,
                   coalesce(p.name, p.title, '') AS title,
                   coalesce(p.domain, '') AS domain
            LIMIT 5
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "fragment".to_string(),
            serde_json::Value::String(task.to_string()),
        );

        let result = self.graph.read_query(cypher, params).await?;

        let rows = result
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|results| results.first())
            .and_then(|first| first.get("data"))
            .and_then(|data| data.as_array());

        let Some(rows) = rows else {
            return Ok(None);
        };

        for row_val in rows {
            let row = row_val.get("row").and_then(|r| r.as_array());
            if let Some(row) = row {
                let id = row
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let title = row
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let domain = row
                    .get(2)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok(Some(PlaybookRef {
                    id,
                    title,
                    match_score: 0.8,
                    domain,
                }));
            }
        }

        Ok(None)
    }

    /// Generate a generic fallback plan when no playbook matches.
    fn generate_generic_plan(task: &str) -> Vec<PlanStep> {
        vec![
            PlanStep {
                order: 1,
                action: format!("Analyze: {}", task),
                target: None,
                estimated_minutes: Some(10),
                requires: vec![],
                notes: Some("Understand the problem scope and gather context".into()),
            },
            PlanStep {
                order: 2,
                action: "Identify affected components".into(),
                target: None,
                estimated_minutes: Some(10),
                requires: vec![1],
                notes: Some("Use dt_context or dt_dependency to map the impact".into()),
            },
            PlanStep {
                order: 3,
                action: "Implement the fix".into(),
                target: None,
                estimated_minutes: Some(30),
                requires: vec![2],
                notes: Some("Make the necessary code/config changes".into()),
            },
            PlanStep {
                order: 4,
                action: "Verify with dt_verify".into(),
                target: None,
                estimated_minutes: Some(10),
                requires: vec![3],
                notes: Some("Run consistency checks across code, config, and DB".into()),
            },
            PlanStep {
                order: 5,
                action: "Record learnings with dt_learn".into(),
                target: None,
                estimated_minutes: Some(5),
                requires: vec![4],
                notes: Some("Capture patterns and pitfalls in the Knowledge World".into()),
            },
        ]
    }
}

#[async_trait::async_trait]
impl PlanServiceTrait for PlanService {
    async fn plan(&self, request: &PlanRequest) -> Result<ExecutionPlan, DtError> {
        let matched = self
            .find_matching_playbook(&request.task, request.domain.as_deref())
            .await?;

        let is_generic = matched.is_none();

        let (plan, impact) = if let Some(ref _pb) = matched {
            // Generate steps from the playbook (simplified for now)
            let steps = Self::generate_generic_plan(&request.task);
            let impact = ImpactEstimate {
                services: vec![],
                configs: vec![],
                risk: "medium".into(),
                total_minutes: steps.iter().filter_map(|s| s.estimated_minutes).sum(),
            };
            (steps, impact)
        } else {
            let steps = Self::generate_generic_plan(&request.task);
            let impact = ImpactEstimate {
                services: vec![],
                configs: vec![],
                risk: "unknown".into(),
                total_minutes: steps.iter().filter_map(|s| s.estimated_minutes).sum(),
            };
            (steps, impact)
        };

        Ok(ExecutionPlan {
            task: request.task.clone(),
            matched_playbook: matched,
            plan,
            estimated_impact: impact,
            is_generic,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_step_construction() {
        let step = PlanStep {
            order: 1,
            action: "Review config".into(),
            target: Some("payment.yml".into()),
            estimated_minutes: Some(10),
            requires: vec![],
            notes: Some("check timeouts".into()),
        };
        assert_eq!(step.order, 1);
        assert_eq!(step.action, "Review config");
        assert!(step.target.is_some());
    }

    #[test]
    fn impact_estimate_risk_levels() {
        let impact = ImpactEstimate {
            services: vec!["pay-svc".into()],
            configs: vec!["pay.yml".into()],
            risk: "high".into(),
            total_minutes: 45,
        };
        assert_eq!(impact.risk, "high");
        assert_eq!(impact.services.len(), 1);
        assert_eq!(impact.total_minutes, 45);
    }

    #[test]
    fn execution_plan_serialization() {
        let plan = ExecutionPlan {
            task: "fix timeout".into(),
            matched_playbook: None,
            plan: vec![],
            estimated_impact: ImpactEstimate {
                services: vec![],
                configs: vec![],
                risk: "low".into(),
                total_minutes: 0,
            },
            is_generic: true,
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("fix timeout"));
        assert!(json.contains("is_generic"));
    }

    #[test]
    fn generic_plan_has_steps() {
        let steps = PlanService::generate_generic_plan("fix bug");
        assert_eq!(steps.len(), 5);
        // Steps should be ordered
        for (i, step) in steps.iter().enumerate() {
            assert_eq!(step.order, i + 1);
        }
    }
}
