//! [`DeploymentHandler`] — creates `(:Deployment)` nodes in Neo4j.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `job`     — Jenkins Job name
//! - `env`     — deployment environment ("test" | "prod")
//! - `branch`  — git branch
//! - `version` — artifact version
//! - `params`  — build parameters (JSON)
//! - `status`  — "success" | "failure"

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::{make_event_id, parse_key_values};

/// Handler for deployment events.
///
/// Produces Cypher:
/// ```cypher
/// MERGE (e:Deployment {deploy_id: $deploy_id})
/// SET e.job = $job, e.env = $env, ...
/// MERGE (si:ServiceInstance {instance_id: $instance_id})
/// MERGE (e)-[:DEPLOYS]->(si)
/// ```
pub struct DeploymentHandler;

#[async_trait]
impl EventHandler for DeploymentHandler {
    fn event_type(&self) -> EventType {
        EventType::Deployment
    }

    async fn handle(
        &self,
        event: &MemoryEvent,
        graph: &dyn GraphRepository,
    ) -> Result<(), DtError> {
        let props = parse_key_values(&event.details);

        let job = props
            .get("job")
            .cloned()
            .unwrap_or_else(|| event.entity_id.clone());
        let env = props
            .get("env")
            .cloned()
            .unwrap_or_else(|| "test".to_string());
        let branch = props
            .get("branch")
            .cloned()
            .unwrap_or_default();
        let version = props
            .get("version")
            .cloned()
            .unwrap_or_default();
        let params_json = props
            .get("params")
            .cloned()
            .unwrap_or_default();
        let status = props
            .get("status")
            .cloned()
            .unwrap_or_else(|| "success".to_string());
        let deploy_id =
            make_event_id("deploy", &job, &event.details);

        // Build an instance_id for the ServiceInstance target.
        // Format: dt://service/{service_name}/instance/{env}
        let service_name = props
            .get("service")
            .cloned()
            .unwrap_or_else(|| job.clone());
        let instance_id = format!(
            "dt://service/{}/instance/{}",
            service_name, env
        );

        let cypher = r#"
            MERGE (e:Deployment {deploy_id: $deploy_id})
            SET e.job = $job,
                e.env = $env,
                e.branch = $branch,
                e.version = $version,
                e.params = $params,
                e.status = $status,
                e.session_id = $session_id,
                e.timestamp = $timestamp,
                e.entity_id = $entity_id,
                e.event_type = $event_type,
                e.details = $details
            WITH e
            MERGE (si:ServiceInstance {instance_id: $instance_id})
            MERGE (e)-[:DEPLOYS]->(si)
            WITH e
            MERGE (prj:Project {name: $project})
            MERGE (e)-[:BELONGS_TO]->(prj)
            "#.to_string();

        let mut params = std::collections::HashMap::new();
        params.insert(
            "deploy_id".into(),
            serde_json::Value::String(deploy_id),
        );
        params.insert("job".into(), serde_json::Value::String(job));
        params.insert("env".into(), serde_json::Value::String(env));
        params.insert(
            "branch".into(),
            serde_json::Value::String(branch),
        );
        params.insert(
            "version".into(),
            serde_json::Value::String(version),
        );
        params.insert(
            "params".into(),
            serde_json::Value::String(params_json),
        );
        params.insert(
            "status".into(),
            serde_json::Value::String(status),
        );
        params.insert(
            "session_id".into(),
            serde_json::Value::String(event.session_id.clone()),
        );
        params.insert(
            "timestamp".into(),
            serde_json::Value::String(event.timestamp.to_rfc3339()),
        );
        params.insert(
            "instance_id".into(),
            serde_json::Value::String(instance_id),
        );
        params.insert(
            "entity_id".into(),
            serde_json::Value::String(event.entity_id.clone()),
        );
        params.insert(
            "event_type".into(),
            serde_json::Value::String(event.event_type.as_str().into()),
        );
        params.insert(
            "details".into(),
            serde_json::Value::String(event.details.clone()),
        );
        params.insert(
            "project".into(),
            serde_json::Value::String(event.project.clone()),
        );

        graph.write_query(&cypher, params).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingRepo {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GraphRepository for CountingRepo {
        async fn read_query(
            &self,
            _query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            Ok(serde_json::Value::Null)
        }

        async fn write_query(
            &self,
            query: &str,
            _params: std::collections::HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, DtError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            assert!(query.contains("MERGE (e:Deployment"));
            assert!(query.contains("e)-[:DEPLOYS]->(si)"));
            Ok(serde_json::Value::Null)
        }

        async fn health_check(
            &self,
        ) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    fn make_event(details: &str) -> MemoryEvent {
        MemoryEvent {
            event_type: EventType::Deployment,
            entity_id: "my-job".into(),
            entity_type: "JenkinsJob".into(),
            project: "test".into(),
            details: details.into(),
            session_id: "2026-07-09-001".into(),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn deployment_handler_writes_correct_query() {
        let handler = DeploymentHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "job: my-job; env: prod; branch: main; version: v2.0; \
             params: {\"k\":\"v\"}; status: success",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deployment_handler_defaults() {
        let handler = DeploymentHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("job: test-job");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deployment_handler_service_from_details() {
        let handler = DeploymentHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event(
            "job: build-service; env: staging; service: my-svc",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
