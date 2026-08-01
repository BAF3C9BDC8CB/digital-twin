//! Knowledge (Memorize) gRPC handler for the DtCore service.
//!
//! Delegates to [`DefaultKnowledgeService`] to persist Knowledge, Experience,
//! Concept, Domain, and Playbook nodes into the knowledge graph.

use crate::application::knowledge::knowledge::service::{
    DefaultKnowledgeService, KnowledgeService,
};
use crate::domain::traits::GraphRepository;
use crate::proto::dt::common;
use crate::proto::dt::core::*;
use std::sync::Arc;
use tonic::Status;

/// Handler for `Memorize` RPC — write knowledge into the graph.
pub async fn handle_memorize(
    req: MemorizeRequest,
    graph: Option<Arc<dyn GraphRepository>>,
) -> Result<common::Empty, Status> {
    let graph = graph.ok_or_else(|| Status::unavailable("Graph backend not available"))?;

    let svc = DefaultKnowledgeService::new(graph);
    let project = if req.project.is_empty() {
        "unknown"
    } else {
        &req.project
    };
    let etype = if req.entity_type.is_empty() {
        &req.r#type
    } else {
        &req.entity_type
    };

    match req.r#type.to_lowercase().as_str() {
        "decision" | "knowledgeadded" | "environment" | "dependencies" => {
            let knowledge = crate::application::knowledge::knowledge::knowledge_from_details(
                &req.entity_id,
                etype,
                project,
                &req.details,
            );
            svc.write_knowledge(&knowledge)
                .await
                .map_err(|e| Status::internal(format!("write_knowledge failed: {e}")))?;
        }
        "experience" => {
            let experience = crate::application::knowledge::knowledge::experience_from_details(
                &req.entity_id,
                project,
                &req.details,
            );
            svc.write_experience(&experience)
                .await
                .map_err(|e| Status::internal(format!("write_experience failed: {e}")))?;
        }
        "concept" => {
            let concept = crate::application::knowledge::knowledge::concept_from_details(
                &req.entity_id,
                &req.details,
            );
            svc.write_concept(&concept)
                .await
                .map_err(|e| Status::internal(format!("write_concept failed: {e}")))?;
        }
        "domain" => {
            let domain = crate::application::knowledge::knowledge::domain_from_details(
                &req.entity_id,
                &req.details,
            );
            svc.write_domain(&domain)
                .await
                .map_err(|e| Status::internal(format!("write_domain failed: {e}")))?;
        }
        "playbook" => {
            let playbook = crate::application::knowledge::knowledge::playbook_from_details(
                &req.entity_id,
                project,
                &req.details,
            );
            svc.write_playbook(&playbook)
                .await
                .map_err(|e| Status::internal(format!("write_playbook failed: {e}")))?;
        }
        other => {
            return Err(Status::invalid_argument(format!(
                "unknown knowledge type: {other}. \
                 Supported: decision, knowledgeadded, environment, dependencies, \
                 experience, concept, domain, playbook"
            )));
        }
    }

    Ok(common::Empty {})
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingRepo {
        write_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl GraphRepository for CountingRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            self.read_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn write_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, crate::domain::error::DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn memorize_requires_graph() {
        let req = MemorizeRequest {
            r#type: "decision".into(),
            entity_id: "test-1".into(),
            entity_type: "Decision".into(),
            project: "test".into(),
            details: "title: Test; domain: test".into(),
        };
        let result = handle_memorize(req, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn memorize_unknown_type_errors() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let graph: Arc<dyn GraphRepository> = Arc::new(CountingRepo {
            write_count: write.clone(),
            read_count: read.clone(),
        });

        let req = MemorizeRequest {
            r#type: "bogus".into(),
            entity_id: "test-1".into(),
            entity_type: "Bogus".into(),
            project: "test".into(),
            details: "something".into(),
        };
        let result = handle_memorize(req, Some(graph)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn memorize_decision_succeeds() {
        let write = Arc::new(AtomicUsize::new(0));
        let read = Arc::new(AtomicUsize::new(0));
        let graph: Arc<dyn GraphRepository> = Arc::new(CountingRepo {
            write_count: write.clone(),
            read_count: read.clone(),
        });

        let req = MemorizeRequest {
            r#type: "decision".into(),
            entity_id: "test-decision".into(),
            entity_type: "Decision".into(),
            project: "test".into(),
            details: "title: Test Decision; domain: test".into(),
        };
        let result = handle_memorize(req, Some(graph)).await;
        assert!(result.is_ok());
        assert!(write.load(Ordering::SeqCst) >= 1);
    }
}
