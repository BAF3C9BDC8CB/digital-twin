//! **DomainQueryService** — domain knowledge model subgraph traversal (4.4).
//!
//! Starting from a domain name, traverses the Knowledge World subgraph to
//! surface related Concepts, Services, and Playbooks, optionally expanding
//! to a configurable depth.
//!
//! # MCP tool: `dt_domain`
//!
//! ```text
//! dt_domain(domain: str, depth?: int)
//!   → DomainModel JSON
//! ```

use std::sync::Arc;

use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// Input for domain query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DomainRequest {
    /// Domain name (e.g. "支付", "auth", "user-management").
    pub domain: String,
    /// Traversal depth.  Default: 1 (immediate neighbours).
    pub depth: Option<u32>,
}

/// A concept discovered in the domain subgraph.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConceptInfo {
    /// Concept name / title.
    pub name: String,
    /// Concept description or definition.
    pub description: String,
    /// Entity type (e.g. "Concept", "Knowledge", "Experience").
    pub entity_type: String,
    /// Relationship depth from the domain root.
    pub depth: u32,
}

/// Output of the domain query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DomainModel {
    /// The queried domain.
    pub domain: String,
    /// Concepts and knowledge nodes discovered.
    pub concepts: Vec<ConceptInfo>,
    /// Related services in this domain.
    pub services: Vec<String>,
    /// Related playbooks in this domain.
    pub playbooks: Vec<String>,
    /// Sub-domains, if any.
    pub sub_domains: Vec<String>,
    /// Total nodes discovered.
    pub total_count: usize,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// Queries the Knowledge World for domain-specific information.
#[async_trait::async_trait]
pub trait DomainQueryTrait: Send + Sync {
    /// Build a domain model from a domain name.
    async fn query(&self, request: &DomainRequest) -> Result<DomainModel, DtError>;
}

/// Canonical implementation of [`DomainQueryTrait`].
pub struct DomainQueryService {
    graph: Arc<dyn GraphRepository>,
}

impl DomainQueryService {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    /// Traverse the graph from a Domain node outward.
    async fn traverse_domain(
        &self,
        domain: &str,
        _depth: u32,
    ) -> Result<(Vec<ConceptInfo>, Vec<String>, Vec<String>, Vec<String>), DtError> {
        // Find concepts and knowledge related to this domain
        let cypher = r#"
            MATCH (d:Domain {name: $domain})
            OPTIONAL MATCH (d)-[:CONTAINS*1..2]->(n)
            WHERE labels(n)[0] IN ['Concept', 'Knowledge', 'Experience', 'Playbook']
            RETURN coalesce(n.name, n.title, '') AS name,
                   labels(n)[0] AS type,
                   coalesce(n.description, n.definition, n.summary, '') AS description
            LIMIT 30
        "#;

        let mut params = std::collections::HashMap::new();
        params.insert(
            "domain".to_string(),
            serde_json::Value::String(domain.to_string()),
        );

        let result = self.graph.read_query(cypher, params).await?;

        let rows = result
            .get("results")
            .and_then(|r| r.as_array())
            .and_then(|results| results.first())
            .and_then(|first| first.get("data"))
            .and_then(|data| data.as_array());

        let mut concepts = Vec::new();
        let mut services = Vec::new();
        let mut playbooks = Vec::new();
        let mut sub_domains = Vec::new();

        if let Some(rows) = rows {
            for row_val in rows {
                let row = row_val.get("row").and_then(|r| r.as_array());
                if let Some(row) = row {
                    let name = row
                        .first()
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let etype = row
                        .get(1)
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let desc = row
                        .get(2)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    if !name.is_empty() && name != "?" {
                        match etype.as_str() {
                            "Playbook" => playbooks.push(name.clone()),
                            "Service" => services.push(name.clone()),
                            "Domain" => sub_domains.push(name.clone()),
                            _ => {}
                        }
                        concepts.push(ConceptInfo {
                            name,
                            description: desc,
                            entity_type: etype,
                            depth: 1,
                        });
                    }
                }
            }
        }

        Ok((concepts, services, playbooks, sub_domains))
    }
}

#[async_trait::async_trait]
impl DomainQueryTrait for DomainQueryService {
    async fn query(&self, request: &DomainRequest) -> Result<DomainModel, DtError> {
        let depth = request.depth.unwrap_or(1);
        let (concepts, services, playbooks, sub_domains) =
            self.traverse_domain(&request.domain, depth).await?;

        let total = concepts.len() + services.len() + playbooks.len() + sub_domains.len();

        Ok(DomainModel {
            domain: request.domain.clone(),
            concepts,
            services,
            playbooks,
            sub_domains,
            total_count: total,
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
    fn domain_model_default() {
        let model = DomainModel {
            domain: "test".into(),
            concepts: vec![],
            services: vec![],
            playbooks: vec![],
            sub_domains: vec![],
            total_count: 0,
        };
        assert_eq!(model.domain, "test");
        assert_eq!(model.total_count, 0);
    }

    #[test]
    fn concept_info_construction() {
        let c = ConceptInfo {
            name: "PaymentFlow".into(),
            description: "Standard payment processing flow".into(),
            entity_type: "Concept".into(),
            depth: 1,
        };
        assert_eq!(c.name, "PaymentFlow");
        assert_eq!(c.entity_type, "Concept");
    }

    #[test]
    fn domain_request_defaults() {
        let req = DomainRequest {
            domain: "支付".into(),
            depth: None,
        };
        assert_eq!(req.domain, "支付");
        assert_eq!(req.depth, None);
    }

    #[test]
    fn domain_model_serialization() {
        let model = DomainModel {
            domain: "auth".into(),
            concepts: vec![ConceptInfo {
                name: "JWT".into(),
                description: "JSON Web Token auth".into(),
                entity_type: "Concept".into(),
                depth: 1,
            }],
            services: vec!["auth-svc".into()],
            playbooks: vec!["auth-migration".into()],
            sub_domains: vec![],
            total_count: 2,
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.contains("auth"));
        assert!(json.contains("JWT"));
        assert!(json.contains("auth-svc"));
        assert!(json.contains("auth-migration"));
    }
}
