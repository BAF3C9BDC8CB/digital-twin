//! TestRunner — orchestrates the full build pipeline test flow.
//!
//! Phases:
//! 1. **build_test_data** — scan fixtures, parse code, write test- nodes, create
//!    vector embeddings
//! 2. **verify_test_data** — run Cypher queries & Qdrant checks
//! 3. **cleanup** — remove test- data (unless `--keep`)

use crate::application::pipeline::test::cleanup::cleanup_test_data;
use crate::application::pipeline::test::report::{CheckResult, TestReport};
use crate::domain::traits::{EmbedService, GraphRepository, VectorRepository};
use crate::infrastructure::parser::ParserRegistry;
use crate::shared::chunker::{chunk_by_type, ChunkConfig, DocType};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Directory containing test fixture files, relative to the project root.
const FIXTURES_DIR: &str = "test/fixtures";

/// Name used for all test- prefixed labels and collections.
const TEST_PROJECT: &str = "test-pipeline";

/// Wraps a label containing hyphens in backtick escapes for Cypher.
/// e.g. `test-Class` → `` `test-Class` ``
fn escape_label(label: &str) -> String {
    format!("`{}`", label)
}

/// Build a Cypher label like `` :`test-Class` `` from a simple name.
fn test_label(name: &str) -> String {
    escape_label(&format!("test-{}", name))
}

// ---------------------------------------------------------------------------
// TestRunner
// ---------------------------------------------------------------------------

/// Orchestrates the full test flow: build → verify → (clean unless keep).
pub struct TestRunner {
    /// Graph repository (Memgraph).
    graph: Arc<dyn GraphRepository>,
    /// Vector repository (Qdrant).
    vector: Arc<dyn VectorRepository>,
    /// Embedding service for generating vectors.
    embed: Arc<dyn EmbedService>,
    /// Project root path (where `test/fixtures/` lives).
    project_root: PathBuf,
    /// If `true`, test data is kept after verification.
    keep: bool,
}

impl TestRunner {
    /// Create a new test runner.
    ///
    /// # Arguments
    ///
    /// * `graph` — Memgraph repository
    /// * `vector` — Qdrant repository
    /// * `embed` — Embedding service
    /// * `project_root` — Project root (containing `test/fixtures/`)
    /// * `keep` — If `true`, do not clean up test data after verification
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        vector: Arc<dyn VectorRepository>,
        embed: Arc<dyn EmbedService>,
        project_root: PathBuf,
        keep: bool,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            project_root,
            keep,
        }
    }

    // ------------------------------------------------------------------
    // Main entry point
    // ------------------------------------------------------------------

    /// Run the full test flow: build → verify → (clean unless keep).
    ///
    /// Returns a [`TestReport`] with all check results.
    pub async fn run(&self) -> TestReport {
        let start = Instant::now();
        tracing::info!("TestRunner: starting pipeline test");

        let mut report = TestReport::new();

        // Phase 1: Build test data.
        tracing::info!("TestRunner: phase 1 — building test data");
        self.build_test_data(&mut report).await;

        // Phase 2: Verify test data.
        tracing::info!("TestRunner: phase 2 — verifying test data");
        self.verify_test_data(&mut report).await;

        // Phase 3: Cleanup unless --keep.
        if !self.keep {
            tracing::info!("TestRunner: phase 3 — cleaning up test data");
            self.cleanup(&mut report).await;
        } else {
            tracing::info!("TestRunner: --keep set, skipping cleanup");
            report.add(CheckResult::skipped(
                "Cleanup",
                "Pipeline",
                "--keep flag set, test data preserved",
            ));
        }

        let elapsed = start.elapsed();
        report.set_duration(elapsed.as_millis() as u64);

        tracing::info!(
            total = report.total,
            passed = report.passed,
            failed = report.failed,
            skipped = report.skipped,
            duration_ms = report.duration_ms,
            "TestRunner: test complete"
        );

        report
    }

    // ------------------------------------------------------------------
    // Phase 1: Build test data
    // ------------------------------------------------------------------

    /// Build test data by scanning fixtures, parsing code, and writing
    /// `test-` prefixed entities to Memgraph and Qdrant.
    async fn build_test_data(&self, report: &mut TestReport) {
        // -- Step 1: Code indexing from fixtures --
        if let Err(e) = self.build_code_data().await {
            tracing::warn!(error = %e, "Code indexing failed");
            report.add(CheckResult::failed(
                "Code indexing",
                "Pipeline",
                "code indexing to complete",
                &e,
            ));
        }

        // -- Step 2: Nacos config test data --
        if let Err(e) = self.build_nacos_data().await {
            tracing::warn!(error = %e, "Nacos data build failed");
            report.add(CheckResult::failed(
                "Nacos data build",
                "Pipeline",
                "nacos data to be written",
                &e,
            ));
        }

        // -- Step 3: K8s test data --
        if let Err(e) = self.build_k8s_data().await {
            tracing::warn!(error = %e, "K8s data build failed");
            report.add(CheckResult::failed(
                "K8s data build",
                "Pipeline",
                "k8s data to be written",
                &e,
            ));
        }

        // -- Step 4: Jenkins test data --
        if let Err(e) = self.build_jenkins_data().await {
            tracing::warn!(error = %e, "Jenkins data build failed");
            report.add(CheckResult::failed(
                "Jenkins data build",
                "Pipeline",
                "jenkins data to be written",
                &e,
            ));
        }

        // -- Step 5: Knowledge test data --
        if let Err(e) = self.build_knowledge_data().await {
            tracing::warn!(error = %e, "Knowledge data build failed");
            report.add(CheckResult::failed(
                "Knowledge data build",
                "Pipeline",
                "knowledge data to be written",
                &e,
            ));
        }

        // -- Step 6: Vector test data --
        if let Err(e) = self.build_vector_data().await {
            tracing::warn!(error = %e, "Vector data build failed");
            report.add(CheckResult::failed(
                "Vector data build",
                "Pipeline",
                "vector data to be written",
                &e,
            ));
        }
    }

    /// Index fixture files: parse Java/Python with tree-sitter, chunk
    /// Markdown/YAML, and write `test-` prefixed entities via Cypher.
    async fn build_code_data(&self) -> Result<(), String> {
        let fixtures_path = self.project_root.join(FIXTURES_DIR);
        if !fixtures_path.exists() {
            return Err(format!("fixtures directory not found: {}", fixtures_path.display()));
        }

        let parser_registry = ParserRegistry::new();
        let chunk_config = ChunkConfig::default();

        // Collect all fixture files recursively.
        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(&fixtures_path, &mut files, "").map_err(|e| e.to_string())?;

        for file_path in &files {
            let source = std::fs::read_to_string(file_path)
                .map_err(|e| format!("failed to read {}: {e}", file_path.display()))?;

            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

            match ext {
                "java" | "py" | "rs" | "go" | "ts" | "tsx" | "js" | "jsx" | "php" => {
                    // Parse with tree-sitter
                    let parse_result = parser_registry
                        .parse_file(&source, file_path, TEST_PROJECT)
                        .map_err(|e| format!("parse failed for {}: {e}", file_path.display()))?;

                    // Write classes
                    for class in &parse_result.classes {
                        let rel_path = file_path
                            .strip_prefix(&self.project_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                            .to_string();

                        let query = concat!(
                            "CREATE (n:`test-Class` {",
                            "  name: $name, ",
                            "  kind: $kind, ",
                            "  file_path: $file_path, ",
                            "  package: $package, ",
                            "  project: $project, ",
                            "  start_line: $start_line, ",
                            "  end_line: $end_line ",
                            "}) RETURN id(n)"
                        );
                        let mut params = HashMap::new();
                        params.insert("name".into(), serde_json::Value::String(class.name.clone()));
                        params.insert(
                            "kind".into(),
                            serde_json::Value::String(class.kind.as_str().to_string()),
                        );
                        params.insert(
                            "file_path".into(),
                            serde_json::Value::String(rel_path.clone()),
                        );
                        params.insert(
                            "package".into(),
                            serde_json::Value::String(class.package_or_module.clone()),
                        );
                        params.insert(
                            "project".into(),
                            serde_json::Value::String(TEST_PROJECT.to_string()),
                        );
                        params.insert(
                            "start_line".into(),
                            serde_json::Value::Number(serde_json::Number::from(class.start_line)),
                        );
                        params.insert(
                            "end_line".into(),
                            serde_json::Value::Number(serde_json::Number::from(class.end_line)),
                        );

                        self.graph.write_query(query, params).await.map_err(|e| {
                            format!("failed to write class {}: {e}", class.name)
                        })?;
                    }

                    // Write methods with BELONGS_TO relationship
                    for method in &parse_result.methods {
                        let rel_path = file_path
                            .strip_prefix(&self.project_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                            .to_string();

                        let query = concat!(
                            "CREATE (m:`test-Method` {",
                            "  name: $name, ",
                            "  signature: $signature, ",
                            "  params: $params, ",
                            "  return_type: $return_type, ",
                            "  class_name: $class_name, ",
                            "  file_path: $file_path, ",
                            "  language: $language, ",
                            "  project: $project, ",
                            "  start_line: $start_line, ",
                            "  end_line: $end_line ",
                            "}) WITH m ",
                            "MATCH (c:`test-Class` {name: $class_name, project: $project}) ",
                            "CREATE (m)-[:BELONGS_TO]->(c) "
                        );
                        let mut params = HashMap::new();
                        params.insert("name".into(), serde_json::Value::String(method.name.clone()));
                        params.insert(
                            "signature".into(),
                            serde_json::Value::String(method.signature.clone()),
                        );
                        params.insert(
                            "params".into(),
                            serde_json::Value::String(method.params.clone()),
                        );
                        params.insert(
                            "return_type".into(),
                            serde_json::Value::String(method.return_type.clone()),
                        );
                        params.insert(
                            "class_name".into(),
                            serde_json::Value::String(method.class_name.clone()),
                        );
                        params.insert(
                            "file_path".into(),
                            serde_json::Value::String(rel_path),
                        );
                        params.insert(
                            "language".into(),
                            serde_json::Value::String(method.language.clone()),
                        );
                        params.insert(
                            "project".into(),
                            serde_json::Value::String(TEST_PROJECT.to_string()),
                        );
                        params.insert(
                            "start_line".into(),
                            serde_json::Value::Number(serde_json::Number::from(method.start_line)),
                        );
                        params.insert(
                            "end_line".into(),
                            serde_json::Value::Number(serde_json::Number::from(method.end_line)),
                        );

                        self.graph.write_query(query, params).await.map_err(|e| {
                            format!("failed to write method {}: {e}", method.name)
                        })?;
                    }
                }
                "md" | "txt" => {
                    // Chunk document files and write as test-Document nodes
                    let doc_type = DocType::detect(&file_path.to_string_lossy(), &[]);
                    let doc_id = format!(
                        "dt://doc/{}/{}",
                        TEST_PROJECT,
                        file_path
                            .strip_prefix(&self.project_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                    );
                    let chunks = chunk_by_type(&source, &doc_id, doc_type, &chunk_config);

                    for chunk in &chunks {
                        let query = concat!(
                            "CREATE (n:`test-Document` {",
                            "  chunk_id: $chunk_id, ",
                            "  text: $text, ",
                            "  doc_id: $doc_id, ",
                            "  chunk_index: $chunk_index, ",
                            "  project: $project ",
                            "}) RETURN id(n)"
                        );
                        let mut params = HashMap::new();
                        params.insert(
                            "chunk_id".into(),
                            serde_json::Value::String(chunk.chunk_id.clone()),
                        );
                        params.insert(
                            "text".into(),
                            serde_json::Value::String(chunk.text.clone()),
                        );
                        params.insert(
                            "doc_id".into(),
                            serde_json::Value::String(doc_id.clone()),
                        );
                        params.insert(
                            "chunk_index".into(),
                            serde_json::Value::Number(serde_json::Number::from(
                                chunk.chunk_index as u64,
                            )),
                        );
                        params.insert(
                            "project".into(),
                            serde_json::Value::String(TEST_PROJECT.to_string()),
                        );

                        self.graph.write_query(query, params).await.map_err(|e| {
                            format!("failed to write document chunk {}: {e}", chunk.chunk_id)
                        })?;
                    }
                }
                "yaml" | "yml" => {
                    // Chunk YAML files
                    let doc_type = DocType::Yaml;
                    let doc_id = format!(
                        "dt://doc/{}/{}",
                        TEST_PROJECT,
                        file_path
                            .strip_prefix(&self.project_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                    );
                    let chunks = chunk_by_type(&source, &doc_id, doc_type, &chunk_config);

                    for chunk in &chunks {
                        let query = concat!(
                            "CREATE (n:`test-Document` {",
                            "  chunk_id: $chunk_id, ",
                            "  text: $text, ",
                            "  doc_id: $doc_id, ",
                            "  chunk_index: $chunk_index, ",
                            "  project: $project ",
                            "}) RETURN id(n)"
                        );
                        let mut params = HashMap::new();
                        params.insert(
                            "chunk_id".into(),
                            serde_json::Value::String(chunk.chunk_id.clone()),
                        );
                        params.insert(
                            "text".into(),
                            serde_json::Value::String(chunk.text.clone()),
                        );
                        params.insert(
                            "doc_id".into(),
                            serde_json::Value::String(doc_id.clone()),
                        );
                        params.insert(
                            "chunk_index".into(),
                            serde_json::Value::Number(serde_json::Number::from(
                                chunk.chunk_index as u64,
                            )),
                        );
                        params.insert(
                            "project".into(),
                            serde_json::Value::String(TEST_PROJECT.to_string()),
                        );

                        self.graph.write_query(query, params).await.map_err(|e| {
                            format!("failed to write yaml chunk {}: {e}", chunk.chunk_id)
                        })?;
                    }
                }
                _ => {
                    tracing::warn!(
                        path = %file_path.display(),
                        "Skipping fixture file with unsupported extension"
                    );
                }
            }
        }

        tracing::info!("Code indexing complete — wrote test- entities");
        Ok(())
    }

    /// Create Nacos test data nodes.
    async fn build_nacos_data(&self) -> Result<(), String> {
        let queries = [
            (
                "CREATE (n:`test-NacosConfig` {name: 'test-config', group: 'DEFAULT_GROUP', content: 'server.port=8080', project: $project})",
                "Nacos config",
            ),
            (
                "CREATE (n:`test-NacosService` {name: 'test-service', groupName: 'DEFAULT_GROUP', ip: '10.0.0.1', port: 8080, project: $project})",
                "Nacos service",
            ),
        ];

        for (query, label) in &queries {
            let mut params = HashMap::new();
            params.insert(
                "project".into(),
                serde_json::Value::String(TEST_PROJECT.to_string()),
            );
            self.graph
                .write_query(query, params)
                .await
                .map_err(|e| format!("failed to create {label}: {e}"))?;
        }

        tracing::info!("Nacos test data created");
        Ok(())
    }

    /// Create K8s test data nodes.
    async fn build_k8s_data(&self) -> Result<(), String> {
        let queries = [
            (
                "CREATE (n:`test-Pod` {name: 'nginx-7f8a', namespace: 'default', status: 'Running', nodeName: 'node-1', project: $project})",
                "K8s pod",
            ),
            (
                "CREATE (n:`test-Deployment` {name: 'nginx-deploy', namespace: 'default', replicas: 1, project: $project})",
                "K8s deployment",
            ),
        ];

        for (query, label) in &queries {
            let mut params = HashMap::new();
            params.insert(
                "project".into(),
                serde_json::Value::String(TEST_PROJECT.to_string()),
            );
            self.graph
                .write_query(query, params)
                .await
                .map_err(|e| format!("failed to create {label}: {e}"))?;
        }

        tracing::info!("K8s test data created");
        Ok(())
    }

    /// Create Jenkins test data nodes with relationship.
    async fn build_jenkins_data(&self) -> Result<(), String> {
        let mut params = HashMap::new();
        params.insert(
            "project".into(),
            serde_json::Value::String(TEST_PROJECT.to_string()),
        );

        // Create JenkinsJob node
        let job_query = concat!(
            "CREATE (j:`test-JenkinsJob` {",
            "  name: 'test-build', ",
            "  url: 'http://jenkins:8080/job/test-build', ",
            "  project: $project ",
            "}) RETURN id(j)"
        );
        self.graph
            .write_query(job_query, params.clone())
            .await
            .map_err(|e| format!("failed to create JenkinsJob: {e}"))?;

        // Create Build node with OF_JOB relationship
        let build_query = concat!(
            "MATCH (j:`test-JenkinsJob` {name: 'test-build', project: $project}) ",
            "CREATE (b:`test-Build` {",
            "  name: 'test-build#1', ",
            "  status: 'SUCCESS', ",
            "  number: 1, ",
            "  project: $project ",
            "}) ",
            "CREATE (b)-[:OF_JOB]->(j) RETURN id(b)"
        );
        self.graph
            .write_query(build_query, params)
            .await
            .map_err(|e| format!("failed to create Build: {e}"))?;

        tracing::info!("Jenkins test data created");
        Ok(())
    }

    /// Create Knowledge test data nodes.
    async fn build_knowledge_data(&self) -> Result<(), String> {
        let mut params = HashMap::new();
        params.insert(
            "project".into(),
            serde_json::Value::String(TEST_PROJECT.to_string()),
        );

        let query = concat!(
            "CREATE (k:`test-Knowledge` {",
            "  name: 'TestKnowledge', ",
            "  type: 'Decision', ",
            "  details: 'test decision', ",
            "  project: $project ",
            "}) RETURN id(k)"
        );
        self.graph
            .write_query(query, params)
            .await
            .map_err(|e| format!("failed to create Knowledge node: {e}"))?;

        tracing::info!("Knowledge test data created");
        Ok(())
    }

    /// Generate embeddings for test fixtures and write to Qdrant collection.
    async fn build_vector_data(&self) -> Result<(), String> {
        let collection = format!("{TEST_PROJECT}_semantic");
        let dim = 1024; // BGE-M3 default

        // Ensure the collection exists.
        self.vector
            .ensure_collection(&collection, dim)
            .await
            .map_err(|e| format!("failed to ensure collection: {e}"))?;

        // Generate embeddings for each fixture file.
        let fixtures_path = self.project_root.join(FIXTURES_DIR);
        let mut texts: Vec<(String, PathBuf)> = Vec::new();

        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(&fixtures_path, &mut files, "").map_err(|e| e.to_string())?;

        for file_path in &files {
            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            // Only embed code files and documents.
            if matches!(ext, "java" | "py" | "md" | "yaml" | "yml" | "txt") {
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        let truncated: String = content.chars().take(2000).collect();
                        texts.push((truncated, file_path.clone()));
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %file_path.display(),
                            error = %e,
                            "Failed to read fixture for embedding"
                        );
                    }
                }
            }
        }

        if texts.is_empty() {
            return Err("no fixture texts to embed".into());
        }

        let embed_texts: Vec<String> = texts.iter().map(|(t, _)| t.clone()).collect();
        let embeddings = self
            .embed
            .embed_batch(&embed_texts)
            .await
            .map_err(|e| format!("embedding failed: {e}"))?;

        let points: Vec<serde_json::Value> = embeddings
            .into_iter()
            .enumerate()
            .map(|(idx, vector)| {
                let (_, ref file_path) = texts[idx];
                serde_json::json!({
                    "id": idx as u64,
                    "vector": vector,
                    "payload": {
                        "file_path": file_path.to_string_lossy(),
                        "project": TEST_PROJECT,
                        "text": embed_texts[idx],
                    }
                })
            })
            .collect();

        if !points.is_empty() {
            self.vector
                .upsert(&collection, points)
                .await
                .map_err(|e| format!("vector upsert failed: {e}"))?;
        }

        tracing::info!(count = texts.len(), "Vector test data created");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Phase 2: Verify test data
    // ------------------------------------------------------------------

    /// Run Cypher and Qdrant checks to verify test data was stored correctly.
    async fn verify_test_data(&self, report: &mut TestReport) {
        // Graph checks
        self.verify_graph_check(
            report,
            "Classes exist",
            "MATCH (n:`test-Class`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "Methods exist",
            "MATCH (n:`test-Method`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "Methods BELONGS_TO Classes",
            "MATCH (m:`test-Method`)-[:BELONGS_TO]->(c:`test-Class`) RETURN count(*) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "NacosConfig exists",
            "MATCH (n:`test-NacosConfig`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "NacosService exists",
            "MATCH (n:`test-NacosService`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "Pod exists",
            "MATCH (n:`test-Pod`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "Pod has status",
            "MATCH (n:`test-Pod`) WHERE n.status IS NOT NULL RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "JenkinsJob exists",
            "MATCH (n:`test-JenkinsJob`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "JenkinsJob has URL",
            "MATCH (n:`test-JenkinsJob`) WHERE n.url IS NOT NULL RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "Build OF_JOB relationship",
            "MATCH ()-[r:OF_JOB]->(n:`test-JenkinsJob`) RETURN count(r) AS count",
            |v| v > 0,
        )
        .await;

        self.verify_graph_check(
            report,
            "Knowledge exists",
            "MATCH (n:`test-Knowledge`) RETURN count(n) AS count",
            |v| v > 0,
        )
        .await;

        // Qdrant checks
        self.verify_vector_collection(report).await;
        self.verify_vector_points(report).await;
    }

    /// Run a single Cypher verification check.
    async fn verify_graph_check<F>(
        &self,
        report: &mut TestReport,
        name: &str,
        query: &str,
        check_fn: F,
    ) where
        F: Fn(i64) -> bool,
    {
        let mut params = HashMap::new();
        params.insert(
            "project".into(),
            serde_json::Value::String(TEST_PROJECT.to_string()),
        );

        match self.graph.read_query(query, params).await {
            Ok(result) => {
                let count = result
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|row| row.get("count"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);

                if check_fn(count) {
                    report.add(CheckResult::passed(name, "Graph"));
                } else {
                    report.add(CheckResult::failed(
                        name,
                        "Graph",
                        format!("count > 0, got {count}"),
                        format!("count = {count}"),
                    ));
                }
            }
            Err(e) => {
                report.add(CheckResult::failed(
                    name,
                    "Graph",
                    "query to succeed",
                    &e.to_string(),
                ));
            }
        }
    }

    /// Verify that a `test-pipeline-*_semantic` collection exists in Qdrant.
    async fn verify_vector_collection(&self, report: &mut TestReport) {
        match self.vector.list_collections().await {
            Ok(collections) => {
                let has_test = collections
                    .iter()
                    .any(|name| name.starts_with("test-") && name.ends_with("_semantic"));

                if has_test {
                    report.add(CheckResult::passed(
                        "Qdrant test-*_semantic collection exists",
                        "Vector",
                    ));
                } else {
                    report.add(CheckResult::failed(
                        "Qdrant test-*_semantic collection exists",
                        "Vector",
                        format!("a test-pipeline-*_semantic collection to exist, found: {:?}", collections),
                        "no matching collection found",
                    ));
                }
            }
            Err(e) => {
                report.add(CheckResult::skipped(
                    "Qdrant test-*_semantic collection exists",
                    "Vector",
                    format!("Qdrant list_collections failed: {e}"),
                ));
            }
        }
    }

    /// Verify that the test Qdrant collection has vector points.
    async fn verify_vector_points(&self, report: &mut TestReport) {
        let collection = format!("{TEST_PROJECT}_semantic");

        match self.vector.collection_info(&collection).await {
            Ok(info) => {
                if info.points_count > 0 {
                    report.add(CheckResult::passed(
                        "Qdrant vector points > 0",
                        "Vector",
                    ));
                } else {
                    report.add(CheckResult::failed(
                        "Qdrant vector points > 0",
                        "Vector",
                        "points_count > 0",
                        &format!("points_count = {}", info.points_count),
                    ));
                }
            }
            Err(e) => {
                report.add(CheckResult::skipped(
                    "Qdrant vector points > 0",
                    "Vector",
                    format!("Qdrant collection_info failed: {e}"),
                ));
            }
        }
    }

    // ------------------------------------------------------------------
    // Phase 3: Cleanup
    // ------------------------------------------------------------------

    /// Remove all test- prefixed data unless `--keep` was set.
    async fn cleanup(&self, report: &mut TestReport) {
        match cleanup_test_data(&self.graph, &self.vector).await {
            Ok(deleted) => {
                report.add(CheckResult::passed(
                    "Cleanup",
                    "Pipeline",
                ));
                tracing::info!(deleted, "Test data cleanup complete");
            }
            Err(e) => {
                report.add(CheckResult::failed(
                    "Cleanup",
                    "Pipeline",
                    "cleanup to complete",
                    &e,
                ));
                tracing::warn!(error = %e, "Test data cleanup encountered errors");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: collect files recursively
// ---------------------------------------------------------------------------

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>, _prefix: &str) -> Result<(), std::io::Error> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_files(&path, files, "")?;
        } else {
            files.push(path);
        }
    }

    Ok(())
}
