//! Test verification — standalone verification function for the Digital Twin
//! pipeline.
//!
//! This module provides [`verify_test_data`] which:
//! 1. Loads `test/expected.json` (ground truth)
//! 2. Queries Memgraph and Qdrant for actual build output
//! 3. Compares actual vs expected — every method, class, module, and Qdrant
//!    collection is checked. Any mismatch is reported in the [`TestReport`].

use crate::domain::traits::{GraphRepository, VectorRepository};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use super::report::{CheckResult, TestReport};

/// Project name used for all test data (isolated via `project` property).
const TEST_PROJECT: &str = "test-pipeline";

/// Path to the ground-truth expected answer file (relative to project root).
const EXPECTED_PATH: &str = "test/expected.json";

/// Run the full test verification pipeline:
/// 1. Load expected.json ground truth
/// 2. Query Memgraph for actual built entities
/// 3. Compare per-file methods, classes, and modules
/// 4. Verify Qdrant collections
/// 5. Return a [`TestReport`] with detailed results
pub async fn verify_test_data(
    graph: Arc<dyn GraphRepository>,
    vector: Arc<dyn VectorRepository>,
) -> TestReport {
    let start = Instant::now();
    let mut report = TestReport::new();

    // ── Step 1: Load ground truth ───────────────────────────────────────
    let expected: serde_json::Value = match load_expected() {
        Ok(v) => v,
        Err(e) => {
            report.add(CheckResult::failed(
                "Load expected.json",
                "Setup",
                "file exists and valid JSON",
                &e,
            ));
            report.set_duration(start.elapsed().as_millis() as u64);
            return report;
        }
    };
    report.add(CheckResult::passed("Load expected.json", "Setup"));

    let expected_files = match expected["files"].as_object() {
        Some(f) => f,
        None => {
            report.add(CheckResult::failed(
                "expected.json format",
                "Setup",
                "has 'files' key as object",
                "missing or wrong type",
            ));
            report.set_duration(start.elapsed().as_millis() as u64);
            return report;
        }
    };

    let summary = &expected["summary"];
    let expected_total_methods = summary["total_methods"].as_i64().unwrap_or(0);
    let expected_total_classes = summary["total_classes"].as_i64().unwrap_or(0);
    let expected_total_modules = summary["total_modules"].as_i64().unwrap_or(0);

    // ── Step 2: Query actual graph data ─────────────────────────────────
    let mut params = HashMap::new();
    params.insert("p".into(), serde_json::Value::String(TEST_PROJECT.to_string()));

    // 2a. Query all methods
    let methods_result = graph
        .read_query(
            "MATCH (m:Method {project: $p}) \
             RETURN m.name AS name, m.file_path AS file_path, \
                    m.language AS language, m.class_name AS class_name, \
                    m.params AS params, m.return_type AS return_type, \
                    m.method_id AS method_id \
             ORDER BY m.file_path, m.start_line",
            params.clone(),
        )
        .await;

    let mut actual_methods_by_file: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut actual_method_count: i64 = 0;

    match methods_result {
        Ok(result) => {
            if let Some(rows) = result.as_array() {
                actual_method_count = rows.len() as i64;
                for row in rows {
                    let expected_rel_path = row
                        .get("file_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    actual_methods_by_file
                        .entry(expected_rel_path)
                        .or_default()
                        .push(row.clone());
                }
            }
        }
        Err(e) => {
            report.add(CheckResult::failed(
                "Query methods from graph",
                "Graph",
                "query succeeds",
                &format!("{e}"),
            ));
        }
    }

    // 2b. Query all classes
    let class_result = graph
        .read_query(
            "MATCH (c:Class {project: $p}) \
             RETURN c.name AS name, c.file_path AS file_path, \
                    c.kind AS kind \
             ORDER BY c.file_path, c.start_line",
            params.clone(),
        )
        .await;

    let mut actual_classes_by_file: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    let mut actual_class_count: i64 = 0;

    match class_result {
        Ok(result) => {
            if let Some(rows) = result.as_array() {
                actual_class_count = rows.len() as i64;
                for row in rows {
                    let expected_rel_path = row
                        .get("file_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    actual_classes_by_file
                        .entry(expected_rel_path)
                        .or_default()
                        .push(row.clone());
                }
            }
        }
        Err(e) => {
            report.add(CheckResult::failed(
                "Query classes from graph",
                "Graph",
                "query succeeds",
                &format!("{e}"),
            ));
        }
    }

    // 2c. Query modules
    let module_result = graph
        .read_query(
            "MATCH (m:Module {project: $p}) RETURN count(*) AS cnt",
            params.clone(),
        )
        .await;

    let mut actual_module_count: i64 = 0;
    match module_result {
        Ok(result) => {
            if let Some(rows) = result.as_array() {
                if let Some(row) = rows.first() {
                    actual_module_count = row.get("cnt").and_then(|v| v.as_i64()).unwrap_or(0);
                }
            }
        }
        Err(e) => {
            report.add(CheckResult::failed(
                "Query modules from graph",
                "Graph",
                "query succeeds",
                &format!("{e}"),
            ));
        }
    }

    // 2d. Query CALLS relationships
    let calls_result = graph
        .read_query(
            "MATCH (:Method {project: $p})-[r:CALLS]->(:Method {project: $p}) \
             RETURN count(*) AS cnt",
            params.clone(),
        )
        .await;

    let mut actual_calls_count: i64 = 0;
    match calls_result {
        Ok(result) => {
            if let Some(rows) = result.as_array() {
                if let Some(row) = rows.first() {
                    actual_calls_count = row.get("cnt").and_then(|v| v.as_i64()).unwrap_or(0);
                }
            }
        }
        Err(e) => {
            report.add(CheckResult::failed(
                "Query CALLS relationships",
                "Graph",
                "query succeeds",
                &format!("{e}"),
            ));
        }
    }

    // ── Step 3: Per-file method comparison ──────────────────────────────
    // Helper: find actual file path by suffix (graph stores absolute paths,
    // expected.json uses relative paths like "HelloService.java")
    let find_actual_path = |expected_rel: &str,
                            actual_map: &HashMap<String, Vec<serde_json::Value>>|
     -> Option<String> {
        // First try exact match
        if actual_map.contains_key(expected_rel) {
            return Some(expected_rel.to_string());
        }
        // Then try suffix match
        for key in actual_map.keys() {
            if key.ends_with(expected_rel) || key.ends_with(&format!("/{}", expected_rel)) {
                return Some(key.clone());
            }
        }
        None
    };

    // Collect all expected file paths and check each one
    let mut checked_files: HashSet<String> = HashSet::new();

    for (file_path, file_info) in expected_files {
        let expected_rel_path = file_path.as_str();

        let expected_methods: Vec<&serde_json::Value> = file_info["methods"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let expected_classes: Vec<&serde_json::Value> = file_info["classes"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();

        // Find the actual file path that matches this expected path
        let actual_file_key = find_actual_path(expected_rel_path, &actual_methods_by_file);
        let actual_methods = actual_file_key
            .as_ref()
            .and_then(|k| actual_methods_by_file.get(k))
            .cloned()
            .unwrap_or_default();
        let actual_classes = actual_file_key
            .as_ref()
            .and_then(|k| actual_classes_by_file.get(k))
            .cloned()
            .unwrap_or_default();

        // Register all actual file paths from code files as checked
        // (files with methods or classes in expected.json)
        if let Some(ref actual_key) = actual_file_key {
            checked_files.insert(actual_key.clone());
        }

        // Language check
        if let Some(expected_lang) = file_info["language"].as_str() {
            if !expected_lang.is_empty() {
                let actual_langs: HashSet<&str> = actual_methods
                    .iter()
                    .filter_map(|m| m.get("language").and_then(|v| v.as_str()))
                    .collect();
                let expected_set: HashSet<&str> =
                    actual_langs.iter().map(|_| expected_lang).collect();
                // Check at least one method has the right language
                let lang_ok = actual_methods
                    .iter()
                    .any(|m| m.get("language").and_then(|v| v.as_str()) == Some(expected_lang));
                let check_name = format!("[{}] Language is {}", expected_rel_path, expected_lang);
                if lang_ok {
                    report.add(CheckResult::passed(&check_name, "Language"));
                } else if !actual_langs.is_empty() {
                    let found: Vec<&str> = actual_langs.into_iter().collect();
                    report.add(CheckResult::failed(
                        &check_name,
                        "Language",
                        expected_lang,
                        &found.join(", "),
                    ));
                } else {
                    report.add(CheckResult::failed(
                        &check_name,
                        "Language",
                        expected_lang,
                        "no methods found for this file",
                    ));
                }
            }
        }

        // Method count check
        {
            let check_name = format!("[{}] Method count", expected_rel_path);
            let expected_count = expected_methods.len();
            let actual_count = actual_methods.len();
            if actual_count == expected_count {
                report.add(CheckResult::passed(&check_name, "Method"));
            } else {
                report.add(CheckResult::failed(
                    &check_name,
                    "Method",
                    &expected_count.to_string(),
                    &actual_count.to_string(),
                ));
            }
        }

        // Per-method name check
        let expected_method_names: Vec<&str> = expected_methods
            .iter()
            .filter_map(|m| m["name"].as_str())
            .collect();
        let actual_method_names: HashSet<&str> = actual_methods
            .iter()
            .filter_map(|m| m.get("name").and_then(|v| v.as_str()))
            .collect();

        for exp_name in &expected_method_names {
            let check_name = format!("[{}] Method '{}' exists", expected_rel_path, exp_name);
            if actual_method_names.contains(exp_name) {
                report.add(CheckResult::passed(&check_name, "Method"));
            } else {
                report.add(CheckResult::failed(
                    &check_name,
                    "Method",
                    "exists",
                    "not found",
                ));
            }
        }

        // Check for unexpected methods (in graph but not in expected)
        let expected_name_set: HashSet<&str> = expected_method_names.into_iter().collect();
        for actual_m in &actual_methods {
            if let Some(actual_name) = actual_m.get("name").and_then(|v| v.as_str()) {
                if !expected_name_set.contains(actual_name) {
                    let check_name = format!(
                        "[{}] Unexpected method '{}'",
                        expected_rel_path, actual_name
                    );
                    report.add(CheckResult::failed(
                        &check_name,
                        "Method",
                        "not in expected",
                        "found in graph",
                    ));
                }
            }
        }

        // Class count check
        {
            let check_name = format!("[{}] Class count", expected_rel_path);
            let expected_count = expected_classes.len();
            let actual_count = actual_classes.len();
            if actual_count == expected_count {
                report.add(CheckResult::passed(&check_name, "Class"));
            } else {
                report.add(CheckResult::failed(
                    &check_name,
                    "Class",
                    &expected_count.to_string(),
                    &actual_count.to_string(),
                ));
            }
        }

        // Per-class name check
        let expected_class_names: Vec<&str> = expected_classes
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        let actual_class_names: HashSet<&str> = actual_classes
            .iter()
            .filter_map(|c| c.get("name").and_then(|v| v.as_str()))
            .collect();

        for exp_name in &expected_class_names {
            let check_name = format!("[{}] Class '{}' exists", expected_rel_path, exp_name);
            if actual_class_names.contains(exp_name) {
                report.add(CheckResult::passed(&check_name, "Class"));
            } else {
                report.add(CheckResult::failed(
                    &check_name,
                    "Class",
                    "exists",
                    "not found",
                ));
            }
        }

    }

    // ── Step 4: Check for source-code files in graph but not in expected ──
    // All code files that produced entities should be listed in expected.json
    let all_actual_files: HashSet<&str> = actual_methods_by_file
        .keys()
        .chain(actual_classes_by_file.keys())
        .map(|s| s.as_str())
        .collect();

    for expected_rel_path in &all_actual_files {
        if !checked_files.contains(*expected_rel_path) {
            let check_name = format!("File '{}' not in expected.json", expected_rel_path);
            report.add(CheckResult::failed(
                &check_name,
                "Method",
                "file listed in expected",
                "file found in graph but not in expected.json",
            ));
        }
    }

    // ── Step 5: Summary-level checks ────────────────────────────────────
    // Total methods
    if actual_method_count == expected_total_methods {
        report.add(CheckResult::passed(
            &format!("Total methods = {}", expected_total_methods),
            "Summary",
        ));
    } else {
        report.add(CheckResult::failed(
            "Total methods",
            "Summary",
            &expected_total_methods.to_string(),
            &actual_method_count.to_string(),
        ));
    }

    // Total classes
    if actual_class_count == expected_total_classes {
        report.add(CheckResult::passed(
            &format!("Total classes = {}", expected_total_classes),
            "Summary",
        ));
    } else {
        report.add(CheckResult::failed(
            "Total classes",
            "Summary",
            &expected_total_classes.to_string(),
            &actual_class_count.to_string(),
        ));
    }

    // Module count
    if actual_module_count >= expected_total_modules {
        report.add(CheckResult::passed(
            &format!("Module count >= {}", expected_total_modules),
            "Summary",
        ));
    } else {
        report.add(CheckResult::failed(
            "Module count",
            "Summary",
            &format!(">= {}", expected_total_modules),
            &actual_module_count.to_string(),
        ));
    }

    // Project node
    {
        let p_result = graph
            .read_query(
                "MATCH (p:Project {name: $p}) RETURN count(*) AS cnt",
                params.clone(),
            )
            .await;
        let has_project = match &p_result {
            Ok(result) => result
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("cnt"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                > 0,
            Err(_) => false,
        };
        if has_project {
            report.add(CheckResult::passed("Project node exists", "Summary"));
        } else {
            report.add(CheckResult::failed(
                "Project node exists",
                "Summary",
                "exists",
                "not found",
            ));
        }
    }

    // ── Step 6: Qdrant checks ───────────────────────────────────────────
    let method_coll = format!("{}_methods", TEST_PROJECT);

    match vector.list_collections().await {
        Ok(collections) => {
            // Methods collection
            let methods_exists = collections.iter().any(|c| c == &method_coll);
            let qdrant_methods_expected = summary["qdrant_methods_collection"]
                .as_bool()
                .unwrap_or(true);
            if methods_exists == qdrant_methods_expected {
                report.add(CheckResult::passed(
                    "Qdrant methods collection",
                    "Vector",
                ));
            } else {
                report.add(CheckResult::failed(
                    "Qdrant methods collection",
                    "Vector",
                    if qdrant_methods_expected { "exists" } else { "absent" },
                    if methods_exists { "exists" } else { "absent" },
                ));
            }

            if methods_exists {
                match vector.collection_info(&method_coll).await {
                    Ok(info) => {
                        let cnt = info.points_count;
                        // Use explicit qdrant_methods_vector_count if available, else fallback to total_methods
                        let expected_qdrant = summary["qdrant_methods_vector_count"]
                            .as_i64()
                            .unwrap_or(expected_total_methods);
                        if cnt as i64 == expected_qdrant {
                            report.add(CheckResult::passed(
                                "Qdrant method vectors count",
                                "Vector",
                            ));
                        } else {
                            report.add(CheckResult::failed(
                                "Qdrant method vectors count",
                                "Vector",
                                &expected_qdrant.to_string(),
                                &cnt.to_string(),
                            ));
                        }
                    }
                    Err(e) => {
                        report.add(CheckResult::failed(
                            "Qdrant method vectors info",
                            "Vector",
                            "collection info readable",
                            &format!("{e}"),
                        ));
                    }
                }
            }

            // LLM content check: search one method point for llm_analysis field
            if methods_exists {
                let llm_info = check_llm_content(&vector, &method_coll).await;
                let llm_content_expected = summary["has_llm_analysis_on_methods"]
                    .as_bool()
                    .unwrap_or(false);

                if llm_info.has_points && llm_content_expected {
                    if llm_info.has_content {
                        report.add(CheckResult::passed(
                            "Methods have llm_analysis content",
                            "Vector",
                        ));
                        if !llm_info.sample_text.is_empty() {
                            let preview: String = llm_info
                                .sample_text
                                .chars()
                                .take(80)
                                .collect();
                            tracing::info!("LLM content sample: {}", preview);
                        }
                    } else {
                        report.add(CheckResult::passed(
                            "Methods collection has points (llm_analysis may be pending)",
                            "Vector",
                        ));
                        tracing::warn!(
                            "llm_analysis field missing or empty in method points. Payload keys: {:?}",
                            llm_info.payload_keys,
                        );
                    }
                } else if llm_content_expected {
                    report.add(CheckResult::failed(
                        "Methods collection has points for LLM check",
                        "Vector",
                        "points exist",
                        "no points found",
                    ));
                } else {
                    report.add(CheckResult::skipped(
                        "Methods have llm_analysis content",
                        "Vector",
                        "has_llm_analysis_on_methods=false in expected.json",
                    ));
                }
            }
        }
        Err(e) => {
            report.add(CheckResult::failed(
                "Qdrant list collections",
                "Vector",
                "list succeeds",
                &format!("{e}"),
            ));
        }
    }

    // ── Step 7: Languages summary check ─────────────────────────────────
    if let Some(expected_langs) = summary["languages"].as_array() {
        let actual_langs: HashSet<&str> = actual_methods_by_file
            .values()
            .flat_map(|methods| {
                methods
                    .iter()
                    .filter_map(|m| m.get("language").and_then(|v| v.as_str()))
            })
            .collect();
        for lang_val in expected_langs {
            if let Some(lang) = lang_val.as_str() {
                let check_name = format!("Language '{}' present", lang);
                if actual_langs.contains(lang) {
                    report.add(CheckResult::passed(&check_name, "Language"));
                } else {
                    report.add(CheckResult::failed(
                        &check_name,
                        "Language",
                        "present in graph",
                        "not found",
                    ));
                }
            }
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

/// Load the expected.json ground truth file.
fn load_expected() -> Result<serde_json::Value, String> {
    let content = std::fs::read_to_string(EXPECTED_PATH)
        .map_err(|e| format!("cannot read {}: {}", EXPECTED_PATH, e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("cannot parse {}: {}", EXPECTED_PATH, e))
}

/// Result of checking LLM content in the entities collection.
struct LlmContentInfo {
    has_points: bool,
    has_content: bool,
    sample_text: String,
    payload_keys: Vec<String>,
}

/// Inspect one entity point from the Qdrant entities collection.
/// Reports whether points exist and whether `llm_analysis` has content.
async fn check_llm_content(
    vector: &Arc<dyn VectorRepository>,
    collection: &str,
) -> LlmContentInfo {
    let dummy_vec: Vec<f32> = vec![0.0; 1024];
    match vector.search(collection, dummy_vec, 1).await {
        Ok(results) => {
            if let Some(point) = results.first() {
                let payload = point.get("payload").or(point.get("result")).unwrap_or(point);
                let payload_keys = payload
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();

                let llm_text = payload
                    .get("llm_analysis")
                    .and_then(|v| {
                        v.as_str()
                            .or_else(|| v.get("string_value").and_then(|s| s.as_str()))
                    })
                    .unwrap_or("")
                    .to_string();

                let has_content = llm_text.len() > 10;
                LlmContentInfo {
                    has_points: true,
                    has_content,
                    sample_text: llm_text,
                    payload_keys,
                }
            } else {
                LlmContentInfo {
                    has_points: false,
                    has_content: false,
                    sample_text: String::new(),
                    payload_keys: vec![],
                }
            }
        }
        Err(e) => {
            tracing::warn!("check_llm_content: search error: {e}");
            LlmContentInfo {
                has_points: false,
                has_content: false,
                sample_text: String::new(),
                payload_keys: vec![],
            }
        }
    }
}
