//! [`DeploymentHandler`] — records deployment by linking JenkinsJob,
//! JenkinsBuild, and ServiceInstance in a three-node model.
//!
//! Parses [`MemoryEvent::details`] for:
//! - `job`            — Jenkins Job name
//! - `env`            — deployment environment ("test" | "prod")
//! - `build_number`   — Jenkins build number (required, used to find/build_id)
//! - `branch`         — git branch
//! - `version`        — artifact version
//! - `params`         — build parameters (JSON)
//! - `status`         — "success" | "failure"
//!
//! Produces Cypher:
//! ```cypher
//! MERGE (job:JenkinsJob {name: $job}) ...
//! MATCH (build:JenkinsBuild {build_id: $build_id}) ...
//! MERGE (si:ServiceInstance {instance_id: $instance_id}) ...
//! MERGE (job)-[:LATEST_DEPLOY]->(si)
//! MERGE (build)-[:DEPLOYED_TO {env, version, deployed_at}]->(si)
//! ```

use async_trait::async_trait;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

use crate::application::knowledge::memory::dispatcher::EventHandler;
use crate::application::knowledge::memory::entities::{EventType, MemoryEvent};
use crate::application::knowledge::memory::handlers::parse_key_values;

/// Handler for deployment events.
///
/// Produces Cypher:
/// ```cypher
/// MERGE (job:JenkinsJob {name: $job})
/// ON CREATE SET job.job_id = $job_id, ...
/// MERGE (build:JenkinsBuild {build_id: $build_id})
/// ON CREATE SET build.number = $build_number_int, ...
/// MERGE (si:ServiceInstance {instance_id: $instance_id})
/// WITH job, build, si
/// OPTIONAL MATCH (job)-[old:LATEST_DEPLOY]->() DELETE old
/// MERGE (job)-[:LATEST_DEPLOY]->(si)
/// MERGE (build)-[:DEPLOYED_TO {env, version, deployed_at}]->(si)
/// MATCH (s:Session {session_id: $session_id})
/// MERGE (s)-[:HAS_EVENT]->(build)
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
        let version = props
            .get("version")
            .cloned()
            .unwrap_or_default();
        let status = props
            .get("status")
            .cloned()
            .unwrap_or_else(|| "success".to_string());
        let build_number = props
            .get("build_number")
            .cloned()
            .unwrap_or_default();

        // Build an instance_id for the ServiceInstance target.
        // Format: dt://service/{service_name}/instance/{env}
        let instance_id = format!(
            "dt://service/{}/instance/{}",
            job, env
        );

        let job_id = format!("dt://jenkins/job/{}", job);

        let build_id = if build_number.is_empty() {
            format!("dt://jenkins/job/{}/build/unknown", job)
        } else {
            format!("dt://jenkins/job/{}/build/{}", job, build_number)
        };

        let cypher = r#"
            // 1. Ensure JenkinsJob exists
            MERGE (job:JenkinsJob {name: $job})
            ON CREATE SET
                job.job_id = $job_id,
                job.full_name = $job,
                job.latest_deploy_env = $env,
                job.latest_deploy_version = $version,
                job.latest_deployed_at = $now
            ON MATCH SET
                job.latest_deploy_env = $env,
                job.latest_deploy_version = $version,
                job.latest_deployed_at = $now

            // 2. Update JenkinsBuild with deployment info
            MERGE (build:JenkinsBuild {build_id: $build_id})
            ON CREATE SET
                build.number = $build_number_int,
                build.deployed_env = $env,
                build.deployed_at = $now,
                build.deployed_version = $version,
                build.result = $status,
                build.timestamp = $timestamp_raw
            ON MATCH SET
                build.deployed_env = $env,
                build.deployed_at = $now,
                build.deployed_version = $version

            // 3. Create ServiceInstance
            MERGE (si:ServiceInstance {instance_id: $instance_id})
            ON CREATE SET
                si.service_name = $job,
                si.env = $env,
                si.updated_at = $now
            ON MATCH SET
                si.service_name = $job,
                si.env = $env,
                si.updated_at = $now

            // 4. Link JenkinsJob -> ServiceInstance (latest deploy, replace old)
            WITH job, build, si
            OPTIONAL MATCH (job)-[old:LATEST_DEPLOY]->()
            DELETE old
            MERGE (job)-[:LATEST_DEPLOY]->(si)

            // 5. Link JenkinsBuild -> ServiceInstance
            MERGE (build)-[:DEPLOYED_TO {env: $env, version: $version, deployed_at: $now}]->(si)

            // 6. Link Session -> JenkinsBuild (timeline, replaces old Deployment link)
            WITH job, build, si
            MATCH (s:Session {session_id: $session_id})
            MERGE (s)-[:HAS_EVENT]->(build)
            "#.to_string();

        let mut params = std::collections::HashMap::new();
        params.insert("job".into(), serde_json::Value::String(job.clone()));
        params.insert("job_id".into(), serde_json::Value::String(job_id));
        params.insert("build_id".into(), serde_json::Value::String(build_id));
        params.insert("env".into(), serde_json::Value::String(env));
        params.insert("version".into(), serde_json::Value::String(version));
        params.insert("status".into(), serde_json::Value::String(status));
        params.insert("now".into(), serde_json::Value::String(event.timestamp.to_rfc3339()));
        params.insert("instance_id".into(), serde_json::Value::String(instance_id));

        // build_number_int: parse for Neo4j integer property (0 if missing/invalid)
        let build_number_int: i64 = build_number.parse().unwrap_or(0);
        params.insert("build_number_int".into(), serde_json::Value::Number(build_number_int.into()));

        // timestamp_raw: epoch millis for JenkinsBuild compatibility
        let timestamp_raw = event.timestamp.timestamp_millis();
        params.insert("timestamp_raw".into(), serde_json::Value::Number(timestamp_raw.into()));

        params.insert("session_id".into(), serde_json::Value::String(event.session_id.clone()));

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
            assert!(query.contains("MERGE (job:JenkinsJob"), "should merge JenkinsJob");
            assert!(query.contains("MERGE (build:JenkinsBuild"), "should merge JenkinsBuild");
            assert!(query.contains("MERGE (si:ServiceInstance"), "should merge ServiceInstance");
            assert!(query.contains("LATEST_DEPLOY"), "should create LATEST_DEPLOY");
            assert!(query.contains("DEPLOYED_TO"), "should create DEPLOYED_TO");
            assert!(query.contains("WITH job, build, si"), "should carry vars with WITH before MATCH Session");
            // Must NOT create Deployment nodes
            assert!(!query.contains("MERGE (e:Deployment"), "should NOT create Deployment node");
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
             build_number: 42; params: {\"k\":\"v\"}; status: success",
        );

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deployment_handler_defaults_without_build_number() {
        let handler = DeploymentHandler;
        let counter = Arc::new(AtomicUsize::new(0));
        let repo = CountingRepo {
            counter: counter.clone(),
        };

        let evt = make_event("job: test-job; env: prod");

        handler.handle(&evt, &repo).await.expect("handler should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
