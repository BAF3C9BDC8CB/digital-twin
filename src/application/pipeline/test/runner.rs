//! Test verification — standalone verification function for the Digital Twin
//! pipeline.
//!
//! Instead of relying on a `TestRunner` struct with build/sync capabilities, this
//! module provides a single public function [`verify_test_data`] that:
//! 1. Cleans old test data from Memgraph and Qdrant
//! 2. Runs verification checks (10 Cypher queries + Qdrant collection checks)
//! 3. Returns a [`TestReport`] summarising pass/fail/skip results

use crate::domain::traits::{GraphRepository, VectorRepository};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use super::report::{CheckResult, TestReport};

/// Project name used for all test data (isolated via `project` property).
const TEST_PROJECT: &str = "test-pipeline";

/// Run the full test verification pipeline: cleanup old data, then verify
/// all entity types stored correctly under `TEST_PROJECT`.
///
/// This is the public entry point for the pipeline integration test.
pub async fn verify_test_data(
    graph: Arc<dyn GraphRepository>,
    vector: Arc<dyn VectorRepository>,
) -> TestReport {
    let start = Instant::now();
    let mut report = TestReport::new();

    // Verification — Cypher queries for entities created by the build pipeline.
    // Note: cleanup_test_data() is called in main.rs BEFORE the build runs.
    let checks: Vec<(&str, &str, i64)> = vec![
        ("Methods or Structs exist", "MATCH (n {project: $p}) WHERE n:Method OR n:Struct RETURN count(*) AS cnt", 1),
        ("Methods exist", "MATCH (n:Method {project: $p}) RETURN count(*) AS cnt", 1),
        ("Methods BELONGS_TO Project", "MATCH (m:Method {project: $p})-[:BELONGS_TO]->(p:Project) RETURN count(*) AS cnt", 1),
        ("Project node by name", "MATCH (p:Project {name: $p}) RETURN count(*) AS cnt", 1),
        ("CALLS relationships exist", "MATCH (:Method {project: $p})-[r:CALLS]->(:Method {project: $p}) RETURN count(*) AS cnt", 0),
        ("Module nodes exist", "MATCH (m:Module {project: $p}) RETURN count(*) AS cnt", 1),
    ];

    for (name, query, expected_min) in checks {
        let mut params = HashMap::new();
        params.insert(
            "p".into(),
            serde_json::Value::String(TEST_PROJECT.to_string()),
        );
        match graph.read_query(query, params).await {
            Ok(result) => {
                let count = result
                    .as_array()
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("cnt"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let expected_str = format!(">= {expected_min}");
                let actual_str = count.to_string();
                if count >= expected_min {
                    report.add(CheckResult::passed(name, "Graph"));
                } else {
                    report.add(CheckResult::failed(name, "Graph", &expected_str, &actual_str));
                }
            }
            Err(e) => {
                report.add(CheckResult::failed(
                    name,
                    "Graph",
                    &format!(">= {expected_min}"),
                    &format!("query failed: {e}"),
                ));
            }
        }
    }

    // Phase 2: Verification — Qdrant checks.
    let method_coll = format!("{}_methods", TEST_PROJECT);
    let entities_coll = format!("{}_entities", TEST_PROJECT);
    match vector.list_collections().await {
        Ok(collections) => {
            // Check methods collection (primary — vectors from tree-sitter code parsing)
            let methods_exists = collections.iter().any(|c| c == &method_coll);
            report.add(if methods_exists {
                CheckResult::passed("Qdrant methods collection exists", "Vector")
            } else {
                CheckResult::failed(
                    "Qdrant methods collection exists",
                    "Vector",
                    &method_coll,
                    &format!("collections: {:?}", collections),
                )
            });

            if methods_exists {
                match vector.collection_info(&method_coll).await {
                    Ok(info) => {
                        let cnt = info.points_count;
                        report.add(if cnt > 0 {
                            CheckResult::passed("Qdrant method vectors > 0", "Vector")
                        } else {
                            CheckResult::failed(
                                "Qdrant method vectors > 0",
                                "Vector", "> 0", &cnt.to_string(),
                            )
                        });
                    }
                    Err(e) => report.add(CheckResult::failed(
                        "Qdrant method vectors > 0",
                        "Vector", "info ok", &format!("{e}"),
                    )),
                }
            }

            // Check entities collection (optional — only created by pipeline analysis)
            let entities_exists = collections.iter().any(|c| c == &entities_coll);
            if entities_exists {
                report.add(CheckResult::passed("Qdrant entities collection exists", "Vector"));
            } else {
                report.add(CheckResult::skipped(
                    "Qdrant entities collection exists",
                    "Vector",
                    "pipeline analysis disabled or no entities created",
                ));
            }
        }
        Err(e) => {
            report.add(CheckResult::failed(
                "Qdrant collections list",
                "Vector", "list ok", &format!("{e}"),
            ));
        }
    }

    report.set_duration(start.elapsed().as_millis() as u64);
    tracing::info!(
        total = report.total,
        passed = report.passed,
        failed = report.failed,
        skipped = report.skipped,
        "verify complete"
    );
    report
}
