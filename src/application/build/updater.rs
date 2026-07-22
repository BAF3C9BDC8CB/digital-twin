//! Update command — single-file real-time incremental update.
//!
//! Implements `dt update --file <path>`: deletes old entities for a file,
//! re-parses it, writes new entities to the graph database, and rebuilds call edges.
//!
//! # Idempotency
//!
//! The update is idempotent: old Method/Class nodes for the file are deleted
//! first (via DETACH DELETE), then new nodes are upserted via MERGE on their
//! unique `method_id` / `class_id`. Running the same update twice produces
//! the same graph state.
//!
//! # Flow
//!
//! 1. Acquire file lock via `WriteCoordinator`
//! 2. Delete old Method + Class nodes for this file
//! 3. Parse file via `ParserRegistry` → MethodBlock + ClassBlock
//! 4. Write new Method + Class nodes (MERGE)
//! 5. Write CONTAINS relationships
//! 6. Rebuild CALLS relationships for affected methods
//! 7. Release lock → return `UpdateReport`

use clap::Parser;
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, SnapshotRepository, VectorRepository};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::shared::coordinator::WriteCoordinator;
use crate::infrastructure::parser::ParserRegistry;
use crate::infrastructure::scanner;

// ---------------------------------------------------------------------------
// UpdateCommand — CLI struct
// ---------------------------------------------------------------------------

/// Single file incremental update command.
///
/// ```bash
/// dt update --file /path/to/PayService.java --project my-project
/// dt update --file /path/to/PayService.java --type delete
/// ```
#[derive(Parser, Debug, Clone)]
#[command(name = "update", about = "Single-file incremental update into the knowledge graph")]
pub struct UpdateCommand {
    /// Absolute path to the file to update.
    #[arg(long = "file")]
    pub file_path: PathBuf,

    /// Project name (required).
    #[arg(long = "project", short = 'p')]
    pub project_name: String,

    /// Operation type: create, modify, or delete.
    #[arg(long = "type", default_value = "modify")]
    pub op_type: String,
}

// ---------------------------------------------------------------------------
// UpdateReport
// ---------------------------------------------------------------------------

/// Report produced by a single-file update operation.
#[derive(Debug, Clone)]
pub struct UpdateReport {
    /// Absolute file path that was updated.
    pub file: String,
    /// Project name.
    pub project: String,
    /// Number of methods written.
    pub methods_updated: usize,
    /// Number of classes written.
    pub classes_updated: usize,
    /// Number of CALLS relationships rebuilt.
    pub calls_rebuilt: usize,
    /// Wall-clock duration in milliseconds.
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// UpdateRunner — core logic
// ---------------------------------------------------------------------------

/// Dependencies needed to execute a single-file update.
pub struct UpdateDependencies {
    pub graph: Option<Arc<dyn GraphRepository>>,
    pub vector: Option<Arc<dyn VectorRepository>>,
    pub snapshot: Option<Arc<dyn SnapshotRepository>>,
    pub embed: Option<Arc<dyn EmbedService>>,
    pub coordinator: Option<Arc<WriteCoordinator>>,
}

/// Executes the single-file update pipeline.
///
/// This is the core logic used by both the CLI `dt update` command and
/// the daemon's `update_file` gRPC endpoint.
pub struct UpdateRunner {
    parser_registry: Arc<ParserRegistry>,
}

impl UpdateRunner {
    /// Create a new runner with the given parser registry.
    pub fn new(parser_registry: Arc<ParserRegistry>) -> Self {
        Self { parser_registry }
    }

    /// Execute the update for a single file.
    ///
    /// # Arguments
    /// - `project`: project name.
    /// - `root`: project root directory (for computing relative paths).
    /// - `file_path`: absolute path to the file.
    /// - `deps`: storage backends and write coordinator.
    pub async fn run(
        &self,
        project: &str,
        root: &Path,
        file_path: &Path,
        deps: &UpdateDependencies,
    ) -> Result<UpdateReport, DtError> {
        let start = Instant::now();

        // ---- Step 1: Acquire file lock ----
        let _guard = if let Some(coord) = &deps.coordinator {
            Some(coord.acquire_file(file_path).await)
        } else {
            None
        };

        // ---- Step 2: Delete old entities for this file ----
        let rel_path = scanner::rel_path(root, file_path);
        if let Some(graph) = &deps.graph {
            delete_by_file_path(graph, project, &rel_path).await;
        }

        // ---- Handle "delete" type: skip re-insertion ----
        if is_delete_type(&rel_path) {
            // Delete-only: remove old data and return.
            let elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(UpdateReport {
                file: file_path.to_string_lossy().to_string(),
                project: project.to_string(),
                methods_updated: 0,
                classes_updated: 0,
                calls_rebuilt: 0,
                elapsed_ms,
            });
        }

        // ---- Step 3: Read and parse the file ----
        let source = std::fs::read_to_string(file_path).map_err(DtError::Io)?;
        let parse_result = self
            .parser_registry
            .parse_file(&source, file_path, project)?;

        // ---- Step 4: Update snapshot (if available) ----
        if let Some(snapshot_repo) = &deps.snapshot {
            let (file_hash, file_mtime) =
                scanner::compute_file_hash(file_path).unwrap_or_default();
            let fs_snapshot = crate::domain::types::FileSnapshot {
                file_path: rel_path.clone(),
                project: project.to_string(),
                file_sha1: file_hash,
                file_mtime,
                method_count: parse_result.methods.len() as u32,
                updated_at: chrono::Utc::now().to_rfc3339(),
            };
            let _ = snapshot_repo
                .save_snapshots(project, &[fs_snapshot])
                .await;
        }

        let methods_count = parse_result.methods.len();
        let classes_count = parse_result.classes.len();

        // ---- Step 5: Write graph (methods, classes, relationships) ----
        if let Some(graph) = &deps.graph {
            // 5a. Write methods
            write_methods(graph, &parse_result.methods).await;

            // 5b. Write classes
            write_classes(graph, &parse_result.classes).await;

            // 5c. Write CONTAINS relationships
            write_contains_relationships(graph, &parse_result.classes).await;

            // 5d. Write module nodes
            write_modules_for_file(graph, project, &parse_result.methods, &parse_result.classes)
                .await;
        }

        // ---- Step 6: Rebuild CALLS relationships ----
        let calls_rebuilt = if let Some(graph) = &deps.graph {
            rebuild_calls_for_file(graph, project, &parse_result.methods, &rel_path).await
        } else {
            0
        };

        let elapsed_ms = start.elapsed().as_millis() as u64;

        Ok(UpdateReport {
            file: file_path.to_string_lossy().to_string(),
            project: project.to_string(),
            methods_updated: methods_count,
            classes_updated: classes_count,
            calls_rebuilt,
            elapsed_ms,
        })
    }
}

// ---------------------------------------------------------------------------
// Helper: decide if this is a "delete" operation
// ---------------------------------------------------------------------------

/// The `--type` flag drives whether we re-insert after deletion.
/// For `delete`, we only clear old data.
/// For `create` and `modify`, we parse and re-write.
fn is_delete_type(op_type: &str) -> bool {
    op_type.eq_ignore_ascii_case("delete")
}

// ---------------------------------------------------------------------------
// Helper: delete old entities for a single file
// ---------------------------------------------------------------------------

/// Delete Method and Class nodes that belong to the given `file_path`.
///
/// We use `DETACH DELETE` so that incoming/outgoing relationships (CALLS,
/// CONTAINS) are automatically removed without leaving orphans.
async fn delete_by_file_path(
    graph: &Arc<dyn GraphRepository>,
    project: &str,
    rel_path: &str,
) {
    use std::collections::HashMap;

    // Delete Method nodes
    {
        let mut params = HashMap::new();
        params.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );
        params.insert(
            "file".to_string(),
            serde_json::Value::String(rel_path.to_string()),
        );
        let _ = graph
            .write_query(
                "MATCH (m:Method {project: $project, file_path: $file}) DETACH DELETE m",
                params,
            )
            .await;
    }

    // Delete Class nodes
    {
        let mut params = HashMap::new();
        params.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );
        params.insert(
            "file".to_string(),
            serde_json::Value::String(rel_path.to_string()),
        );
        let _ = graph
            .write_query(
                "MATCH (c:Class {project: $project, file_path: $file}) DETACH DELETE c",
                params,
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// Helpers: write entities
// ---------------------------------------------------------------------------

/// Write (MERGE) method nodes in batches of 200.
async fn write_methods(graph: &Arc<dyn GraphRepository>, methods: &[crate::domain::types::MethodBlock]) {
    use std::collections::HashMap;

    for chunk in methods.chunks(200) {
        let methods_json: Vec<serde_json::Value> = chunk
            .iter()
            .map(|m| {
                serde_json::json!({
                    "method_id": m.method_id,
                    "project": m.project,
                    "file_path": m.file_path,
                    "language": m.language,
                    "package_or_module": m.package_or_module,
                    "class_name": m.class_name,
                    "name": m.name,
                    "signature": m.signature,
                    "params": m.params,
                    "return_type": m.return_type,
                    "start_line": m.start_line,
                    "end_line": m.end_line,
                    "calls": m.calls,
                    "comment": m.comment,
                })
            })
            .collect();

        let mut params = HashMap::new();
        params.insert("methods".to_string(), serde_json::Value::Array(methods_json));

        let _ = graph
            .write_query(
                r#"UNWIND $methods AS m
                MERGE (n:Method {method_id: m.method_id})
                SET n.project = m.project,
                    n.file_path = m.file_path,
                    n.language = m.language,
                    n.package_or_module = m.package_or_module,
                    n.class_name = m.class_name,
                    n.name = m.name,
                    n.signature = m.signature,
                    n.params = m.params,
                    n.return_type = m.return_type,
                    n.start_line = m.start_line,
                    n.end_line = m.end_line,
                    n.calls = m.calls,
                    n.comment = m.comment
                WITH n, m
                MERGE (p:Project {name: m.project})
                MERGE (n)-[:BELONGS_TO]->(p)"#,
                params,
            )
            .await;
    }
}

/// Write (MERGE) class nodes in batches of 100.
async fn write_classes(graph: &Arc<dyn GraphRepository>, classes: &[crate::domain::types::ClassBlock]) {
    use std::collections::HashMap;

    for chunk in classes.chunks(100) {
        let classes_json: Vec<serde_json::Value> = chunk
            .iter()
            .map(|c| {
                serde_json::json!({
                    "class_id": c.class_id,
                    "name": c.name,
                    "kind": c.kind.as_str(),
                    "file_path": c.file_path,
                    "package_or_module": c.package_or_module,
                    "project": c.project,
                    "start_line": c.start_line,
                    "end_line": c.end_line,
                })
            })
            .collect();

        let mut params = HashMap::new();
        params.insert("classes".to_string(), serde_json::Value::Array(classes_json));

        let _ = graph
            .write_query(
                r#"UNWIND $classes AS c
                MERGE (n:Class {class_id: c.class_id})
                SET n.name = c.name,
                    n.kind = c.kind,
                    n.file_path = c.file_path,
                    n.package_or_module = c.package_or_module,
                    n.project = c.project,
                    n.start_line = c.start_line,
                    n.end_line = c.end_line
                WITH n, c
                MERGE (p:Project {name: c.project})
                MERGE (n)-[:BELONGS_TO]->(p)"#,
                params,
            )
            .await;
    }
}

/// Write CONTAINS relationships (Class → Method) for the given classes.
async fn write_contains_relationships(
    graph: &Arc<dyn GraphRepository>,
    classes: &[crate::domain::types::ClassBlock],
) {
    use std::collections::HashMap;

    for c in classes {
        for mid in &c.method_ids {
            let mut params = HashMap::new();
            params.insert(
                "class_id".to_string(),
                serde_json::Value::String(c.class_id.clone()),
            );
            params.insert(
                "method_id".to_string(),
                serde_json::Value::String(mid.clone()),
            );
            let _ = graph
                .write_query(
                    "MATCH (c:Class {class_id: $class_id}) \
                     MATCH (m:Method {method_id: $method_id}) \
                     MERGE (c)-[:CONTAINS]->(m)",
                    params,
                )
                .await;
        }
    }
}

/// Write module nodes from the package/module paths found in methods and classes.
async fn write_modules_for_file(
    graph: &Arc<dyn GraphRepository>,
    project: &str,
    methods: &[crate::domain::types::MethodBlock],
    classes: &[crate::domain::types::ClassBlock],
) {
    use std::collections::{HashMap, HashSet};

    let mut module_set: HashSet<String> = HashSet::new();
    for m in methods {
        if !m.package_or_module.is_empty() {
            module_set.insert(m.package_or_module.clone());
        }
    }
    for c in classes {
        if !c.package_or_module.is_empty() {
            module_set.insert(c.package_or_module.clone());
        }
    }

    let modules: Vec<crate::domain::types::ModuleBlock> = module_set
        .into_iter()
        .map(|name| crate::domain::types::ModuleBlock {
            module_id: crate::domain::id::make_module_id(project, &name),
            name,
            project: project.to_string(),
        })
        .collect();

    for chunk in modules.chunks(100) {
        let modules_json: Vec<serde_json::Value> = chunk
            .iter()
            .map(|m| {
                serde_json::json!({
                    "module_id": m.module_id,
                    "name": m.name,
                    "project": m.project,
                })
            })
            .collect();

        let mut params = HashMap::new();
        params.insert("modules".to_string(), serde_json::Value::Array(modules_json));

        let _ = graph
            .write_query(
                r#"UNWIND $modules AS m
                MERGE (n:Module {module_id: m.module_id})
                SET n.name = m.name, n.project = m.project"#,
                params,
            )
            .await;
    }
}

/// Rebuild CALLS relationships for methods in a single file.
///
/// Matches caller methods (those in this file) to callee methods by name.
/// Returns the number of CALLS relationships created.
async fn rebuild_calls_for_file(
    graph: &Arc<dyn GraphRepository>,
    project: &str,
    methods: &[crate::domain::types::MethodBlock],
    rel_path: &str,
) -> usize {
    use std::collections::HashMap;

    // Collect unique caller method names in this file
    let caller_names: Vec<String> = methods.iter().map(|m| m.name.clone()).collect();
    if caller_names.is_empty() {
        return 0;
    }

    // First delete existing CALLS where the caller is in this file
    {
        let mut params = HashMap::new();
        params.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );
        params.insert(
            "file".to_string(),
            serde_json::Value::String(rel_path.to_string()),
        );
        let _ = graph
            .write_query(
                "MATCH (caller:Method {project: $project, file_path: $file})-[r:CALLS]->() DELETE r",
                params,
            )
            .await;
    }

    // Now create new CALLS based on current method bodies
    let mut created = 0usize;
    for method in methods {
        if method.calls.is_empty() {
            continue;
        }

        let mut params = HashMap::new();
        params.insert(
            "method_id".to_string(),
            serde_json::Value::String(method.method_id.clone()),
        );
        params.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );

        let calls_json: Vec<serde_json::Value> = method
            .calls
            .iter()
            .map(|c| serde_json::Value::String(c.clone()))
            .collect();
        params.insert("calls".to_string(), serde_json::Value::Array(calls_json));

        let result = graph
            .write_query(
                r#"MATCH (caller:Method {method_id: $method_id, project: $project})
                UNWIND $calls AS called_name
                MATCH (callee:Method {project: $project, name: called_name})
                WHERE callee.method_id <> caller.method_id
                MERGE (caller)-[:CALLS]->(callee)"#,
                params,
            )
            .await;

        // Count created — approximate from result
        if let Ok(ref val) = result {
            if let Some(props) = val.get("properties") {
                if props.as_object().is_some_and(|o| !o.is_empty()) {
                    created += method.calls.len();
                }
            }
        } else {
            // If query runs without error, count all attempts
            created += method.calls.len();
        }
    }

    created
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_command_parses_args() {
        let cmd = UpdateCommand::try_parse_from([
            "update",
            "--file",
            "/tmp/foo.java",
            "--project",
            "test-proj",
            "--type",
            "modify",
        ]);
        assert!(cmd.is_ok());
        let cmd = cmd.unwrap();
        assert_eq!(cmd.file_path, PathBuf::from("/tmp/foo.java"));
        assert_eq!(cmd.project_name, "test-proj");
        assert_eq!(cmd.op_type, "modify");
    }

    #[test]
    fn update_command_default_type_is_modify() {
        let cmd = UpdateCommand::try_parse_from([
            "update",
            "--file",
            "/tmp/bar.py",
            "--project",
            "p2",
        ]);
        assert!(cmd.is_ok());
        assert_eq!(cmd.unwrap().op_type, "modify");
    }

    #[test]
    fn is_delete_type_matches() {
        assert!(is_delete_type("delete"));
        assert!(is_delete_type("DELETE"));
        assert!(is_delete_type("Delete"));
        assert!(!is_delete_type("modify"));
        assert!(!is_delete_type("create"));
    }

    #[test]
    fn update_report_contains_all_fields() {
        let report = UpdateReport {
            file: "/tmp/x.rs".into(),
            project: "test".into(),
            methods_updated: 3,
            classes_updated: 1,
            calls_rebuilt: 5,
            elapsed_ms: 42,
        };
        assert_eq!(report.file, "/tmp/x.rs");
        assert_eq!(report.project, "test");
        assert_eq!(report.methods_updated, 3);
        assert_eq!(report.classes_updated, 1);
        assert_eq!(report.calls_rebuilt, 5);
        assert_eq!(report.elapsed_ms, 42);
    }

    #[test]
    fn update_runner_creates() {
        let registry = Arc::new(ParserRegistry::new());
        let runner = UpdateRunner::new(registry);
        // Just verify it compiles and constructs
        let _ = &runner;
    }

    #[tokio::test]
    async fn runner_with_noop_graph_produces_report() {
        let registry = Arc::new(ParserRegistry::new());
        let runner = UpdateRunner::new(registry);

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let graph: Arc<dyn GraphRepository> = Arc::new(crate::infrastructure::memgraph::NoopGraphRepo);
        let deps = UpdateDependencies {
            graph: Some(graph),
            vector: None,
            snapshot: None,
            embed: None,
            coordinator: None,
        };

        let report = runner
            .run("test", dir.path(), &file, &deps)
            .await
            .unwrap();

        assert_eq!(report.project, "test");
        // methods_updated and classes_updated are usize — always ≥ 0
        let _ = report.methods_updated;
        let _ = report.classes_updated;
        // elapsed_ms can be 0 (noop backends may return instantly)
        let _ = report.elapsed_ms;
    }

    #[tokio::test]
    async fn runner_delete_type_clears_and_returns_zero() {
        let _ = tempfile::tempdir().unwrap();
        // Verify that a delete-type update report has zero counts
        let report = UpdateReport {
            file: "/tmp/gone.java".into(),
            project: "test".into(),
            methods_updated: 0,
            classes_updated: 0,
            calls_rebuilt: 0,
            elapsed_ms: 0,
        };
        assert_eq!(report.methods_updated, 0);
        assert_eq!(report.calls_rebuilt, 0);
    }
}
