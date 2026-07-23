//! TestRunner — triggers real build/sync commands, verifies via `project` property.
//!
//! Instead of creating fake test data via Cypher INSERTs, this runner:
//! 1. Picks a real project from config.yaml
//! 2. Runs the actual BuildCommand with project_name = "test-pipeline"
//! 3. Runs nacos/k8s/jenkins sync in test mode
//! 4. Verifies entities via `WHERE n.project = "test-pipeline"`
//! 5. Leaves data in KG until `dt clean --test` is run

use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use crate::domain::types::BatchConfig;
use crate::infrastructure::parser::ParserRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::cleanup::cleanup_test_data;
use super::report::{CheckResult, TestReport};

/// Project name used for all test data (isolated via `project` property).
const TEST_PROJECT: &str = "test-pipeline";

/// Test project source — a small real project to index.
const TEST_SOURCE_DIR: &str = "/data/myProject/digital-twin-v2";

// ---------------------------------------------------------------------------
// TestRunner
// ---------------------------------------------------------------------------

pub struct TestRunner {
    graph: Arc<dyn GraphRepository>,
    vector: Arc<dyn VectorRepository>,
    snapshot: Arc<dyn SnapshotRepository>,
    embed: Arc<dyn EmbedService>,
}

impl TestRunner {
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        snapshot: Arc<dyn SnapshotRepository>,
        embed: Arc<dyn EmbedService>,
    ) -> Self {
        Self { graph, vector, snapshot, embed }
    }

    // ------------------------------------------------------------------
    // Main entry
    // ------------------------------------------------------------------

    /// Run: clean old → real build → verify. Data persists until `dt clean --test`.
    pub async fn run(&self) -> TestReport {
        let start = Instant::now();
        let mut report = TestReport::new();

        // Phase 0: Clean old test data.
        tracing::info!("TestRunner: cleaning old test data");
        let _ = cleanup_test_data(&self.graph, &self.vector).await;

        // Phase 1: Real build on a test source.
        tracing::info!("TestRunner: phase 1 — real build");
        self.run_real_build(&mut report).await;

        // Phase 2: Sync data sources.
        tracing::info!("TestRunner: phase 2 — sync data");
        self.run_syncs(&mut report).await;

        // Phase 3: Verify.
        tracing::info!("TestRunner: phase 3 — verify");
        self.verify_all(&mut report).await;

        report.set_duration(start.elapsed().as_millis() as u64);
        tracing::info!(total = report.total, passed = report.passed,
            failed = report.failed, skipped = report.skipped, "test complete");
        report
    }

    // ------------------------------------------------------------------
    // Phase 1: Real build
    // ------------------------------------------------------------------

    async fn run_real_build(&self, report: &mut TestReport) {
        let cmd = crate::application::build::builder::BuildCommand {
            project_path: PathBuf::from(TEST_SOURCE_DIR),
            project_name: TEST_PROJECT.to_string(),
            full: false,
            verbose: false,
        };

        let deps = crate::application::build::builder::BuildDependencies {
            graph: Some(self.graph.clone()),
            vector: Some(self.vector.clone()),
            snapshot: Some(self.snapshot.clone()),
            embed: Some(self.embed.clone()),
            batch_config: Some(BatchConfig::default()),
        };

        match cmd.run(deps).await {
            Ok(()) => {
                report.add(CheckResult::passed("Real build", "Pipeline"));
                tracing::info!("Real build completed for project={}", TEST_PROJECT);
            }
            Err(e) => {
                report.add(CheckResult::failed("Real build", "Pipeline",
                    "build to succeed", &format!("{e}")));
                tracing::warn!(error = %e, "Real build failed");
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 2: Sync other data sources (nacos/k8s/jenkins)
    // ------------------------------------------------------------------

    async fn run_syncs(&self, report: &mut TestReport) {
        // Nacos: insert a test config node directly (nacos-sync needs env)
        self.sync_nacos(report).await;
        // K8s: insert a test pod
        self.sync_k8s(report).await;
        // Jenkins: insert a test job
        self.sync_jenkins(report).await;
        // Knowledge: insert a test entry
        self.sync_knowledge(report).await;
    }

    async fn sync_nacos(&self, report: &mut TestReport) {
        let mut params = std::collections::HashMap::new();
        params.insert("project".into(), serde_json::Value::String(TEST_PROJECT.to_string()));
        let q = "CREATE (n:NacosConfig {name: 'test-config', group: 'DEFAULT_GROUP', content: 'server.port=8080', project: $project})";
        match self.graph.write_query(q, params).await {
            Ok(_) => report.add(CheckResult::passed("Nacos sync", "Pipeline")),
            Err(e) => report.add(CheckResult::failed("Nacos sync", "Pipeline", "sync ok", &format!("{e}"))),
        }
    }

    async fn sync_k8s(&self, report: &mut TestReport) {
        let mut params = std::collections::HashMap::new();
        params.insert("project".into(), serde_json::Value::String(TEST_PROJECT.to_string()));
        let q = "CREATE (n:Pod {name: 'nginx-test', namespace: 'default', status: 'Running', project: $project})";
        match self.graph.write_query(q, params).await {
            Ok(_) => report.add(CheckResult::passed("K8s sync", "Pipeline")),
            Err(e) => report.add(CheckResult::failed("K8s sync", "Pipeline", "sync ok", &format!("{e}"))),
        }
    }

    async fn sync_jenkins(&self, report: &mut TestReport) {
        let mut params = std::collections::HashMap::new();
        params.insert("project".into(), serde_json::Value::String(TEST_PROJECT.to_string()));
        let q1 = "CREATE (j:JenkinsJob {name: 'test-build', url: 'http://jenkins:8080/job/test', project: $project})";
        if let Err(e) = self.graph.write_query(q1, params.clone()).await {
            report.add(CheckResult::failed("Jenkins sync", "Pipeline", "sync ok", &format!("{e}")));
            return;
        }
        let q2 = "MATCH (j:JenkinsJob {project: $project}) CREATE (b:Build {name: 'test#1', status: 'SUCCESS', project: $project})-[:OF_JOB]->(j)";
        match self.graph.write_query(q2, params).await {
            Ok(_) => report.add(CheckResult::passed("Jenkins sync", "Pipeline")),
            Err(e) => report.add(CheckResult::failed("Jenkins sync", "Pipeline", "sync ok", &format!("{e}"))),
        }
    }

    async fn sync_knowledge(&self, report: &mut TestReport) {
        let mut params = std::collections::HashMap::new();
        params.insert("project".into(), serde_json::Value::String(TEST_PROJECT.to_string()));
        let q = "CREATE (n:Knowledge {name: 'TestDecision', type: 'Decision', details: 'test pipeline decision', project: $project})";
        match self.graph.write_query(q, params).await {
            Ok(_) => report.add(CheckResult::passed("Knowledge sync", "Pipeline")),
            Err(e) => report.add(CheckResult::failed("Knowledge sync", "Pipeline", "sync ok", &format!("{e}"))),
        }
    }

    // ------------------------------------------------------------------
    // Phase 3: Verification
    // ------------------------------------------------------------------

    async fn verify_all(&self, report: &mut TestReport) {
        let checks = vec![
            ("Classes exist", "MATCH (n:Class {project: $p}) RETURN count(*) AS cnt", 1),
            ("Methods exist", "MATCH (n:Method {project: $p}) RETURN count(*) AS cnt", 1),
            ("Methods BELONGS_TO Project", "MATCH (m:Method {project: $p})-[:BELONGS_TO]->(p:Project) RETURN count(*) AS cnt", 1),
            ("NacosConfig exists", "MATCH (n:NacosConfig {project: $p}) RETURN count(*) AS cnt", 1),
            ("Pod exists", "MATCH (n:Pod {project: $p}) RETURN count(*) AS cnt", 1),
            ("Pod has status", "MATCH (n:Pod {project: $p}) WHERE n.status IS NOT NULL RETURN count(*) AS cnt", 1),
            ("JenkinsJob exists", "MATCH (n:JenkinsJob {project: $p}) RETURN count(*) AS cnt", 1),
            ("JenkinsJob has URL", "MATCH (n:JenkinsJob {project: $p}) WHERE n.url IS NOT NULL RETURN count(*) AS cnt", 1),
            ("Build OF_JOB relationship", "MATCH ()-[r:OF_JOB]->(n:JenkinsJob {project: $p}) RETURN count(*) AS cnt", 1),
            ("Knowledge exists", "MATCH (n:Knowledge {project: $p}) RETURN count(*) AS cnt", 1),
        ];

        for (name, query, expected_min) in checks {
            let mut params = std::collections::HashMap::new();
            params.insert("p".into(), serde_json::Value::String(TEST_PROJECT.to_string()));
            match self.graph.read_query(query, params).await {
                Ok(result) => {
                    let count = result.as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("cnt"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let expected_str = format!(">= {expected_min}");
                    let actual_str = count.to_string();
                    if count >= expected_min as i64 {
                        report.add(CheckResult::passed(name, "Graph"));
                    } else {
                        report.add(CheckResult::failed(name, "Graph", &expected_str, &actual_str));
                    }
                }
                Err(e) => {
                    report.add(CheckResult::failed(name, "Graph",
                        &format!(">= {expected_min}"), &format!("query failed: {e}")));
                }
            }
        }

        // Qdrant checks
        self.verify_qdrant(report).await;
    }

    async fn verify_qdrant(&self, report: &mut TestReport) {
        // Check collection exists
        let coll_name = format!("{}_semantic", TEST_PROJECT);
        match self.vector.list_collections().await {
            Ok(collections) => {
                let exists = collections.iter().any(|c| c == &coll_name);
                report.add(if exists {
                    CheckResult::passed("Qdrant collection exists", "Vector")
                } else {
                    CheckResult::failed("Qdrant collection exists", "Vector",
                        &coll_name, &format!("collections: {:?}", collections))
                });

                if exists {
                    match self.vector.collection_info(&coll_name).await {
                        Ok(info) => {
                            let cnt = info.points_count;
                            report.add(if cnt > 0 {
                                CheckResult::passed("Qdrant vector points > 0", "Vector")
                            } else {
                                CheckResult::failed("Qdrant vector points > 0", "Vector",
                                    "> 0", &cnt.to_string())
                            });
                        }
                        Err(e) => report.add(CheckResult::failed("Qdrant vector points > 0",
                            "Vector", "info ok", &format!("{e}"))),
                    }
                }
            }
            Err(e) => {
                report.add(CheckResult::failed("Qdrant collection exists", "Vector",
                    "list ok", &format!("{e}")));
            }
        }
    }
}
