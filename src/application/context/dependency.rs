//! **DependencyService** — call-chain analysis (4.6).
//!
//! Takes a target entity (service, file, config, or method) and explores
//! its upstream callers and downstream callees in the Reality World.
//! Produces a dependency graph with an impact analysis.
//!
//! # MCP tool: `dt_dependency`
//!
//! ```text
//! dt_dependency(target: str, direction?: "upstream"|"downstream"|"both")
//!   → DependencyGraph JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// Input for dependency analysis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DependencyRequest {
    /// Target entity (service name, file path, config key, method FQN).
    pub target: String,
    /// Direction: "upstream" (callers), "downstream" (callees), "both".
    pub direction: Option<String>,
    /// Maximum depth for traversal.
    pub max_depth: Option<u32>,
    /// Filter by project.
    pub project: Option<String>,
}

/// A section of the dependency graph (upstream or downstream).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DepSection {
    /// Entities in this section.
    pub entities: Vec<DepEntity>,
    /// Total number of entities (before any filter).
    pub count: usize,
}

/// A single entity in the dependency graph.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DepEntity {
    /// Entity name.
    pub name: String,
    /// Entity type (Service, Method, Class, Config, etc.).
    pub entity_type: String,
    /// Distance from the target (1 = direct, 2 = transitive, etc.).
    pub distance: u32,
    /// Source file path, if known.
    pub source_file: Option<String>,
    /// Additional notes.
    pub notes: Option<String>,
}

/// Impact analysis derived from the dependency graph.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImpactAnalysis {
    /// Affected services.
    pub services: Vec<String>,
    /// Affected configs/files.
    pub configs: Vec<String>,
    /// Upstream dependencies that would be affected.
    pub affected_upstream_count: usize,
    /// Downstream dependencies that could be impacted.
    pub affected_downstream_count: usize,
    /// Risk level: "low" | "medium" | "high".
    pub risk: String,
}

/// Output of the dependency query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DependencyGraph {
    /// The original target.
    pub target: String,
    /// Upstream (callers / dependents).
    pub upstream: DepSection,
    /// Downstream (callees / dependencies).
    pub downstream: DepSection,
    /// Impact analysis.
    pub impact_analysis: ImpactAnalysis,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// Analyses call-chain dependencies.
#[async_trait::async_trait]
pub trait DependencyTrait: Send + Sync {
    /// Analyse dependencies for a target entity.
    async fn analyse(&self, request: &DependencyRequest) -> Result<DependencyGraph, DtError>;
}

/// Canonical implementation of [`DependencyTrait`].
pub struct DependencyService {
    graph: Arc<dyn GraphRepository>,
}

impl DependencyService {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    /// Query upstream (callers / entities that depend on target).
    async fn query_upstream(
        &self,
        target: &str,
        max_depth: u32,
    ) -> Result<Vec<DepEntity>, DtError> {
        let depth = max_depth.min(5);
        let cypher = format!(
            r#"
            MATCH p = (caller)-[:CALLS|DEPENDS_ON|IMPORTS*1..{depth}]->(target)
            WHERE target.name CONTAINS $target OR target.source_file CONTAINS $target
            WITH caller, length(p) AS dist
            RETURN coalesce(caller.name, caller.method_name, caller.class_name, '') AS name,
                   labels(caller)[0] AS type,
                   coalesce(caller.source_file, '') AS source_file,
                   dist AS distance
            ORDER BY dist LIMIT 100
        "#
        );

        let mut params = std::collections::HashMap::new();
        params.insert(
            "target".to_string(),
            serde_json::Value::String(target.to_string()),
        );

        let result = self.graph.read_query(&cypher, params).await?;
        Self::parse_entity_rows(&result)
    }

    /// Query downstream (callees / entities that the target depends on).
    async fn query_downstream(
        &self,
        target: &str,
        max_depth: u32,
    ) -> Result<Vec<DepEntity>, DtError> {
        let depth = max_depth.min(5);
        let cypher = format!(
            r#"
            MATCH p = (target)-[:CALLS|DEPENDS_ON|IMPORTS*1..{depth}]->(callee)
            WHERE target.name CONTAINS $target OR target.source_file CONTAINS $target
            WITH callee, length(p) AS dist
            RETURN coalesce(callee.name, callee.method_name, callee.class_name, '') AS name,
                   labels(callee)[0] AS type,
                   coalesce(callee.source_file, '') AS source_file,
                   dist AS distance
            ORDER BY dist LIMIT 100
        "#
        );

        let mut params = std::collections::HashMap::new();
        params.insert(
            "target".to_string(),
            serde_json::Value::String(target.to_string()),
        );

        let result = self.graph.read_query(&cypher, params).await?;
        Self::parse_entity_rows(&result)
    }

    /// Parse graph query results into DepEntity list.
    /// Uses [`graph_parse::parse_graph_rows`] for unified Bolt + HTTP format support.
    fn parse_entity_rows(raw: &serde_json::Value) -> Result<Vec<DepEntity>, DtError> {
        let rows = super::graph_parse::parse_graph_rows(raw);
        let entities = rows
            .into_iter()
            .map(|row| DepEntity {
                name: row
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                entity_type: row
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string(),
                distance: row
                    .get("distance")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as u32,
                source_file: row
                    .get("source_file")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                notes: None,
            })
            .collect();
        Ok(entities)
    }

    /// Compute impact analysis from upstream + downstream lists.
    fn compute_impact(upstream: &[DepEntity], downstream: &[DepEntity]) -> ImpactAnalysis {
        let mut services = Vec::new();
        let mut configs = Vec::new();

        for e in upstream.iter().chain(downstream.iter()) {
            match e.entity_type.as_str() {
                "Service" | "MicroService" | "Module"
                    if !services.contains(&e.name) => {
                    services.push(e.name.clone());
                }
                "NacosConfig" | "Configuration" | "Config" | "YamlConfig"
                    if !configs.contains(&e.name) => {
                    configs.push(e.name.clone());
                }
                _ => {}
            }
        }

        let total_affected = upstream.len() + downstream.len();
        let risk = if total_affected > 10 {
            "high"
        } else if total_affected > 3 {
            "medium"
        } else {
            "low"
        };

        ImpactAnalysis {
            services,
            configs,
            affected_upstream_count: upstream.len(),
            affected_downstream_count: downstream.len(),
            risk: risk.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl DependencyTrait for DependencyService {
    async fn analyse(&self, request: &DependencyRequest) -> Result<DependencyGraph, DtError> {
        let direction = request.direction.as_deref().unwrap_or("both");
        let max_depth = request.max_depth.unwrap_or(2);

        let (upstream_entities, downstream_entities) = match direction {
            "upstream" => {
                let up = self.query_upstream(&request.target, max_depth).await?;
                (up, Vec::new())
            }
            "downstream" => {
                let down = self.query_downstream(&request.target, max_depth).await?;
                (Vec::new(), down)
            }
            _ => {
                let (up, down) = tokio::join!(
                    self.query_upstream(&request.target, max_depth),
                    self.query_downstream(&request.target, max_depth),
                );
                (up.unwrap_or_default(), down.unwrap_or_default())
            }
        };

        let impact = Self::compute_impact(&upstream_entities, &downstream_entities);

        Ok(DependencyGraph {
            target: request.target.clone(),
            upstream: DepSection {
                count: upstream_entities.len(),
                entities: upstream_entities,
            },
            downstream: DepSection {
                count: downstream_entities.len(),
                entities: downstream_entities,
            },
            impact_analysis: impact,
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
    fn dep_entity_construction() {
        let e = DepEntity {
            name: "PaymentService".into(),
            entity_type: "Service".into(),
            distance: 1,
            source_file: Some("src/payment.rs".into()),
            notes: None,
        };
        assert_eq!(e.name, "PaymentService");
        assert_eq!(e.distance, 1);
    }

    #[test]
    fn impact_analysis_computation() {
        let upstream = vec![
            DepEntity {
                name: "api-gateway".into(),
                entity_type: "Service".into(),
                distance: 1,
                source_file: None,
                notes: None,
            },
            DepEntity {
                name: "pay_config".into(),
                entity_type: "NacosConfig".into(),
                distance: 1,
                source_file: None,
                notes: None,
            },
        ];
        let downstream = vec![
            DepEntity {
                name: "payment-db".into(),
                entity_type: "Database".into(),
                distance: 1,
                source_file: None,
                notes: None,
            },
            DepEntity {
                name: "order-svc".into(),
                entity_type: "Service".into(),
                distance: 1,
                source_file: None,
                notes: None,
            },
        ];
        let impact = DependencyService::compute_impact(&upstream, &downstream);
        assert_eq!(impact.affected_upstream_count, 2);
        assert_eq!(impact.affected_downstream_count, 2);
        assert_eq!(impact.services.len(), 2);
        assert_eq!(impact.configs.len(), 1);
        assert_eq!(impact.risk, "medium"); // 4 total > 3
    }

    #[test]
    fn dependency_graph_serialization() {
        let graph = DependencyGraph {
            target: "pay-svc".into(),
            upstream: DepSection {
                entities: vec![],
                count: 0,
            },
            downstream: DepSection {
                entities: vec![DepEntity {
                    name: "payment-db".into(),
                    entity_type: "Database".into(),
                    distance: 1,
                    source_file: None,
                    notes: None,
                }],
                count: 1,
            },
            impact_analysis: ImpactAnalysis {
                services: vec![],
                configs: vec![],
                affected_upstream_count: 0,
                affected_downstream_count: 1,
                risk: "low".into(),
            },
        };
        let json = serde_json::to_string(&graph).unwrap();
        assert!(json.contains("pay-svc"));
        assert!(json.contains("payment-db"));
    }

    #[test]
    fn dependency_request_defaults() {
        let req = DependencyRequest {
            target: "payment-svc".into(),
            direction: None,
            max_depth: None,
            project: None,
        };
        assert_eq!(req.target, "payment-svc");
        assert_eq!(req.direction, None);
        assert_eq!(req.max_depth, None);
    }

    #[test]
    fn impact_high_risk() {
        let mut upstream = Vec::new();
        for i in 0..12 {
            upstream.push(DepEntity {
                name: format!("svc-{i}"),
                entity_type: "Service".into(),
                distance: 1,
                source_file: None,
                notes: None,
            });
        }
        let impact = DependencyService::compute_impact(&upstream, &[]);
        assert_eq!(impact.risk, "high");
    }
}
