//! 测试验证——Digital Twin 流水线的独立验证函数。
//!
//! 该模块提供 [`verify_test_data`]，它：
//! 1. 加载 `test/expected.json`（基准数据）
//! 2. 查询 Memgraph 与 Qdrant 获取实际构建输出
//! 3. 对比实际与期望——每个方法、类、模块与 Qdrant 集合都会被检查。
//!    任何不匹配都会在 [`TestReport`] 中报告。

use crate::domain::traits::{GraphRepository, VectorRepository};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use super::report::{CheckResult, TestReport};

/// 所有测试数据使用的项目名（通过 `project` 属性隔离）。
const TEST_PROJECT: &str = "test-pipeline";

/// 基准答案文件路径（相对项目根目录）。
const EXPECTED_PATH: &str = "test/expected.json";

/// 运行完整的测试验证流水线：
/// 1. 加载 expected.json 基准数据
/// 2. 查询 Memgraph 获取实际构建实体
/// 3. 逐文件对比方法、类与模块
/// 4. 验证 Qdrant 集合
/// 5. 返回带详细结果的 [`TestReport`]
pub async fn verify_test_data(
    graph: Arc<dyn GraphRepository>,
    vector: Arc<dyn VectorRepository>,
) -> TestReport {
    let start = Instant::now();
    let mut report = TestReport::new();

    // ── 步骤 1：加载基准数据 ───────────────────────────────────────
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

    // ── 步骤 2：查询实际图数据 ─────────────────────────────────
    let mut params = HashMap::new();
    params.insert(
        "p".into(),
        serde_json::Value::String(TEST_PROJECT.to_string()),
    );

    // 2a. 查询所有方法
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

    // 2b. 查询所有类
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

    // 2c. 查询模块
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

    // 2d. 查询 CALLS 关系
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

    // ── 步骤 3：逐文件方法比较 ──────────────────────────────
    // 辅助：按后缀查找实际文件路径（图存储绝对路径，
    // expected.json 使用相对路径，如 "HelloService.java"）
    let find_actual_path = |expected_rel: &str,
                            actual_map: &HashMap<String, Vec<serde_json::Value>>|
     -> Option<String> {
        // 先尝试精确匹配
        if actual_map.contains_key(expected_rel) {
            return Some(expected_rel.to_string());
        }
        // 再尝试后缀匹配
        for key in actual_map.keys() {
            if key.ends_with(expected_rel) || key.ends_with(&format!("/{}", expected_rel)) {
                return Some(key.clone());
            }
        }
        None
    };

    // 收集所有期望文件路径并逐一检查
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

        // 找到与该期望路径匹配的实际文件路径
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

        // 将来自代码文件的全部实际文件路径登记为已检查
        //（expected.json 中带方法或类的文件）
        if let Some(ref actual_key) = actual_file_key {
            checked_files.insert(actual_key.clone());
        }

        // 语言检查
        if let Some(expected_lang) = file_info["language"].as_str() {
            if !expected_lang.is_empty() {
                let actual_langs: HashSet<&str> = actual_methods
                    .iter()
                    .filter_map(|m| m.get("language").and_then(|v| v.as_str()))
                    .collect();
                let expected_set: HashSet<&str> =
                    actual_langs.iter().map(|_| expected_lang).collect();
                // 检查至少一个方法具有正确的语言
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

        // 方法数量检查
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

        // 逐方法名称检查
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

        // 检查意外方法（在图中但不在期望中）
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

        // 类数量检查
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

        // 逐类名称检查
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

    // ── 步骤 4：检查图中但不在期望中的源代码文件 ──
    // 产生实体的所有代码文件都应列在 expected.json 中
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

    // ── 步骤 5：汇总级检查 ────────────────────────────────────
    // 方法总数
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

    // 类总数
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

    // 模块数量
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

    // 项目节点
    {
        let p_result = graph
            .read_query(
                "MATCH (p:Project {name: $p}) RETURN count(*) AS cnt",
                params.clone(),
            )
            .await;
        let has_project = match &p_result {
            Ok(result) => {
                result
                    .as_array()
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("cnt"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    > 0
            }
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

    // ── 步骤 6：Qdrant 检查 ───────────────────────────────────────────
    // Phase 5+6：方法现在位于全局 code_methods 集合中
    //（project 是 payload 标签，而非集合名的一部分）。
    let method_coll = crate::shared::collections::CODE_METHODS.to_string();

    match vector.list_collections().await {
        Ok(collections) => {
            // 方法集合
            let methods_exists = collections.iter().any(|c| c == &method_coll);
            let qdrant_methods_expected = summary["qdrant_methods_collection"]
                .as_bool()
                .unwrap_or(true);
            if methods_exists == qdrant_methods_expected {
                report.add(CheckResult::passed("Qdrant methods collection", "Vector"));
            } else {
                report.add(CheckResult::failed(
                    "Qdrant methods collection",
                    "Vector",
                    if qdrant_methods_expected {
                        "exists"
                    } else {
                        "absent"
                    },
                    if methods_exists { "exists" } else { "absent" },
                ));
            }

            if methods_exists {
                match vector.collection_info(&method_coll).await {
                    Ok(info) => {
                        let cnt = info.points_count;
                        // 若提供了显式的 qdrant_methods_vector_count 则使用之，
                        // 否则回退到 total_methods
                        let expected_qdrant = summary["qdrant_methods_vector_count"]
                            .as_i64()
                            .unwrap_or(expected_total_methods);
                        if cnt as i64 == expected_qdrant {
                            report
                                .add(CheckResult::passed("Qdrant method vectors count", "Vector"));
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

            // LLM 内容检查：搜索一个方法点以查找 llm_analysis 字段
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
                            let preview: String = llm_info.sample_text.chars().take(80).collect();
                            tracing::info!("LLM 内容样本: {}", preview);
                        }
                    } else {
                        report.add(CheckResult::passed(
                            "Methods collection has points (llm_analysis may be pending)",
                            "Vector",
                        ));
                        tracing::warn!(
                            "方法点中 llm_analysis 字段缺失或为空。Payload 键: {:?}",
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

    // ── 步骤 7：语言汇总检查 ─────────────────────────────────
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

    // ── 步骤 8：知识图谱验证（Extract + Consolidate） ─────
    // R11：LLM 提取是非确定性的，因此期望值为下界计数（>=）加上抽样存在 /
    // 字段形状检查——不做精确相等。旧的 NLP 关键词-实体检查已移除（§10.1）。
    verify_knowledge_graph(&graph, &vector, &expected, &params, &mut report).await;

    report.set_duration(start.elapsed().as_millis() as u64);
    tracing::info!(
        total = report.total,
        passed = report.passed,
        failed = report.failed,
        skipped = report.skipped,
        "验证完成"
    );
    report
}

/// 加载 expected.json 基准数据文件。
fn load_expected() -> Result<serde_json::Value, String> {
    let content = std::fs::read_to_string(EXPECTED_PATH)
        .map_err(|e| format!("cannot read {}: {}", EXPECTED_PATH, e))?;
    serde_json::from_str(&content).map_err(|e| format!("cannot parse {}: {}", EXPECTED_PATH, e))
}

/// 验证 Extract + Consolidate 链的知识图谱输出
///（Entity 节点、RELATES 边、MENTIONED_IN 溯源，以及双写的
/// kg_nodes / doc_chunks 向量 payload）。
///
/// R11：提取是非确定性的，因此 `expected.json` 中的每个计数期望都是
/// **下界**（`>=`），实体检查是**抽样存在**，我们只断言字段*形状*，
/// 从不做精确相等。
async fn verify_knowledge_graph(
    graph: &Arc<dyn GraphRepository>,
    vector: &Arc<dyn VectorRepository>,
    expected: &serde_json::Value,
    params: &HashMap<String, serde_json::Value>,
    report: &mut TestReport,
) {
    let summary = &expected["summary"];

    // 辅助：运行计数查询，返回 Option<i64>。查询出错时返回 None
    //（错误由调用方在此处报告一次）。
    let count = |query: &str,
                 graph: &Arc<dyn GraphRepository>,
                 params: &HashMap<String, serde_json::Value>| {
        let graph = graph.clone();
        let params = params.clone();
        let q = query.to_string();
        async move {
            match graph.read_query(&q, params).await {
                Ok(v) => v
                    .as_array()
                    .and_then(|rows| rows.first())
                    .and_then(|r| r.get("cnt"))
                    .and_then(|v| v.as_i64()),
                Err(_) => None,
            }
        }
    };

    // ── 图计数检查（下界） ────────────────────────────────
    let checks: [(&str, &str, &str); 3] = [
        (
            "min_entities",
            "Entity count",
            "MATCH (e:Entity {project: $p}) RETURN count(*) AS cnt",
        ),
        (
            "min_relates",
            "RELATES edge count",
            "MATCH (:Entity {project: $p})-[r:RELATES]->(:Entity {project: $p}) \
             RETURN count(r) AS cnt",
        ),
        (
            "min_mentioned_in",
            "MENTIONED_IN edge count",
            "MATCH (:Entity {project: $p})-[m:MENTIONED_IN]->(:Document {project: $p}) \
             RETURN count(m) AS cnt",
        ),
    ];

    let mut any_query_failed = false;
    for (key, label, cypher) in checks {
        let min = summary[key].as_i64().unwrap_or(0);
        if min == 0 {
            report.add(CheckResult::skipped(
                label,
                "KG",
                &format!("no `{key}` expectation in expected.json"),
            ));
            continue;
        }
        match count(cypher, graph, params).await {
            Some(actual) if actual >= min => {
                report.add(CheckResult::passed(
                    &format!("{label} >= {min} (actual {actual})"),
                    "KG",
                ));
            }
            Some(actual) => {
                report.add(CheckResult::failed(
                    label,
                    "KG",
                    &format!(">= {min}"),
                    &actual.to_string(),
                ));
            }
            None => {
                any_query_failed = true;
                report.add(CheckResult::failed(
                    label,
                    "KG",
                    "query succeeds",
                    "query error",
                ));
            }
        }
    }
    if any_query_failed {
        return; // 图不可达——跳过更深的形状检查
    }

    // ── 实体字段形状 ───────────────────────────────────────────────
    // 抽样一个 Entity 并确认它携带 §7.2 的图属性。
    match graph
        .read_query(
            "MATCH (e:Entity {project: $p}) \
             RETURN e.entity_id AS entity_id, e.name AS name, e.type AS type, \
                    e.summary AS summary, e.keywords AS keywords, e.aliases AS aliases \
             LIMIT 1",
            params.clone(),
        )
        .await
    {
        Ok(v) => {
            let sample = v.as_array().and_then(|rows| rows.first());
            match sample {
                Some(e) => {
                    let mut missing: Vec<&str> = Vec::new();
                    if e.get("entity_id").and_then(|x| x.as_str()).is_none() {
                        missing.push("entity_id");
                    }
                    if e.get("name").and_then(|x| x.as_str()).is_none() {
                        missing.push("name");
                    }
                    if e.get("type").and_then(|x| x.as_str()).is_none() {
                        missing.push("type");
                    }
                    if e.get("summary").and_then(|x| x.as_str()).is_none() {
                        missing.push("summary");
                    }
                    if e.get("keywords").and_then(|x| x.as_array()).is_none() {
                        missing.push("keywords");
                    }
                    if e.get("aliases").and_then(|x| x.as_array()).is_none() {
                        missing.push("aliases");
                    }
                    if missing.is_empty() {
                        report.add(CheckResult::passed(
                            "Entity carries entity_id/name/type/summary/keywords/aliases",
                            "KG",
                        ));
                    } else {
                        report.add(CheckResult::failed(
                            "Entity field shape",
                            "KG",
                            "all §7.2 properties present",
                            &format!("missing: {}", missing.join(", ")),
                        ));
                    }
                }
                None => report.add(CheckResult::failed(
                    "Entity field shape",
                    "KG",
                    "at least one Entity",
                    "no Entity nodes found",
                )),
            }
        }
        Err(e) => report.add(CheckResult::failed(
            "Entity field shape query",
            "KG",
            "query succeeds",
            &format!("{e}"),
        )),
    }

    // ── RELATES 边字段形状 ─────────────────────────────────────────
    match graph
        .read_query(
            "MATCH (:Entity {project: $p})-[r:RELATES]->(:Entity {project: $p}) \
             RETURN r.type AS type, r.doc_id AS doc_id, r.evidence AS evidence, \
                    r.confidence AS confidence LIMIT 1",
            params.clone(),
        )
        .await
    {
        Ok(v) => {
            let sample = v.as_array().and_then(|rows| rows.first());
            match sample {
                Some(r) => {
                    let mut missing: Vec<&str> = Vec::new();
                    if r.get("type").and_then(|x| x.as_str()).is_none() {
                        missing.push("type");
                    }
                    if r.get("doc_id").and_then(|x| x.as_str()).is_none() {
                        missing.push("doc_id");
                    }
                    // evidence/confidence 允许为空或默认，但键必须存在。
                    if r.get("evidence").is_none() {
                        missing.push("evidence");
                    }
                    if r.get("confidence").is_none() {
                        missing.push("confidence");
                    }
                    if missing.is_empty() {
                        report.add(CheckResult::passed(
                            "RELATES edge carries type/doc_id/evidence/confidence",
                            "KG",
                        ));
                    } else {
                        report.add(CheckResult::failed(
                            "RELATES edge field shape",
                            "KG",
                            "type/doc_id/evidence/confidence present",
                            &format!("missing: {}", missing.join(", ")),
                        ));
                    }
                }
                None => report.add(CheckResult::failed(
                    "RELATES edge field shape",
                    "KG",
                    "at least one RELATES edge",
                    "no RELATES edges found",
                )),
            }
        }
        Err(e) => report.add(CheckResult::failed(
            "RELATES field shape query",
            "KG",
            "query succeeds",
            &format!("{e}"),
        )),
    }

    // ── 抽样实体存在性 ──────────────────────────────────────────
    // sample_entities: [{name, type}, ...]——不区分大小写的规范名匹配，
    // 抽样检查，使非确定性不会导致运行失败。
    if let Some(samples) = summary["sample_entities"].as_array() {
        for sample in samples {
            let name = sample["name"].as_str().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let want_type = sample["type"].as_str().unwrap_or_default();
            let mut p = params.clone();
            p.insert("n".into(), serde_json::Value::String(name.to_lowercase()));
            let result = graph
                .read_query(
                    "MATCH (e:Entity {project: $p}) \
                     WHERE toLower(e.name) CONTAINS $n OR toLower(e.entity_id) CONTAINS $n \
                     RETURN e.type AS type LIMIT 5",
                    p,
                )
                .await;
            let check_name = format!("Sample entity '{}' present", name);
            match result {
                Ok(v) => {
                    let rows = v.as_array().cloned().unwrap_or_default();
                    if rows.is_empty() {
                        report.add(CheckResult::failed(
                            &check_name,
                            "KG",
                            "exists",
                            "not found",
                        ));
                    } else if want_type.is_empty() {
                        report.add(CheckResult::passed(&check_name, "KG"));
                    } else {
                        let type_ok = rows.iter().any(|r| {
                            r.get("type")
                                .and_then(|t| t.as_str())
                                .map(|t| t.eq_ignore_ascii_case(want_type))
                                .unwrap_or(false)
                        });
                        if type_ok {
                            report.add(CheckResult::passed(
                                &format!("{} (type={})", check_name, want_type),
                                "KG",
                            ));
                        } else {
                            let found: Vec<String> = rows
                                .iter()
                                .filter_map(|r| r.get("type").and_then(|t| t.as_str()))
                                .map(String::from)
                                .collect();
                            report.add(CheckResult::failed(
                                &format!("Sample entity '{}' type", name),
                                "KG",
                                want_type,
                                &found.join(", "),
                            ));
                        }
                    }
                }
                Err(e) => report.add(CheckResult::failed(
                    &check_name,
                    "KG",
                    "query succeeds",
                    &format!("{e}"),
                )),
            }
        }
    }

    // ── 向量 payload 检查 ────────────────────────────────────────────
    verify_vector_payloads(vector, summary, report).await;
}

/// 检查 `kg_nodes` 与 `doc_chunks` 中的单个点，并断言 payload
/// 携带 §7.2 / §7.3 规定的字段。
async fn verify_vector_payloads(
    vector: &Arc<dyn VectorRepository>,
    summary: &serde_json::Value,
    report: &mut TestReport,
) {
    // kg_nodes —— §7.2 字段（business_id / origin=extracted / summary / labels）。
    if summary["check_kg_nodes_payload"].as_bool().unwrap_or(true) {
        let kg = crate::shared::collections::KG_NODES;
        match point_payload(vector, kg).await {
            Some(payload) => {
                let mut missing: Vec<&str> = Vec::new();
                if payload
                    .get("business_id")
                    .and_then(|x| x.as_str())
                    .is_none()
                {
                    missing.push("business_id");
                }
                if payload.get("name").and_then(|x| x.as_str()).is_none() {
                    missing.push("name");
                }
                if payload.get("summary").and_then(|x| x.as_str()).is_none() {
                    missing.push("summary");
                }
                if payload.get("labels").and_then(|x| x.as_array()).is_none() {
                    missing.push("labels");
                }
                if payload.get("origin").and_then(|x| x.as_str()).is_none() {
                    missing.push("origin");
                }
                if missing.is_empty() {
                    report.add(CheckResult::passed(
                        "kg_nodes payload has business_id/name/summary/labels/origin",
                        "Vector",
                    ));
                } else {
                    report.add(CheckResult::failed(
                        "kg_nodes payload shape",
                        "Vector",
                        "§7.2 fields present",
                        &format!("missing: {}", missing.join(", ")),
                    ));
                }
            }
            None => report.add(CheckResult::skipped(
                "kg_nodes payload shape",
                "Vector",
                "collection absent or empty (no entities extracted)",
            )),
        }
    }

    // doc_chunks —— §7.3 字段（doc_id / block_index / entity_ids / text）。
    if summary["check_doc_chunks_payload"]
        .as_bool()
        .unwrap_or(true)
    {
        let dc = crate::shared::collections::DOC_CHUNKS;
        match point_payload(vector, dc).await {
            Some(payload) => {
                let mut missing: Vec<&str> = Vec::new();
                if payload.get("doc_id").and_then(|x| x.as_str()).is_none() {
                    missing.push("doc_id");
                }
                if payload.get("block_index").is_none() {
                    missing.push("block_index");
                }
                if payload.get("text").and_then(|x| x.as_str()).is_none() {
                    missing.push("text");
                }
                if missing.is_empty() {
                    report.add(CheckResult::passed(
                        "doc_chunks payload has doc_id/block_index/text",
                        "Vector",
                    ));
                } else {
                    report.add(CheckResult::failed(
                        "doc_chunks payload shape",
                        "Vector",
                        "§7.3 fields present",
                        &format!("missing: {}", missing.join(", ")),
                    ));
                }
            }
            None => report.add(CheckResult::skipped(
                "doc_chunks payload shape",
                "Vector",
                "collection absent or empty",
            )),
        }
    }
}

/// 从集合中获取一个任意点的 payload（虚拟向量搜索，limit 1），
/// 限定为本次测试构建写入的点（`project = test-pipeline`）。
/// 若集合缺失 / 为空 / 出错则返回 None。
///
/// project 过滤是必需的，因为 `kg_nodes` 是全局集合：
/// 它还保存着旧的 kg-sync 点，其 payload 形状（`elementId` /
/// `description` / `source`）早于 §7.2（`business_id` / `summary` /
/// `origin`）。不过滤的抽样会非确定性地选中这些点，
/// 即使 Consolidate 写入器是正确的，也会使形状断言失败。
async fn point_payload(
    vector: &Arc<dyn VectorRepository>,
    collection: &str,
) -> Option<serde_json::Value> {
    let dummy: Vec<f32> = vec![0.0; 1024];
    let filter = serde_json::json!({
        "must": [{"key": "project", "match": {"value": TEST_PROJECT}}]
    });
    let results = vector
        .search_with_filter(collection, dummy, 1, filter)
        .await
        .ok()?;
    let point = results.first()?;
    let payload = point
        .get("payload")
        .or(point.get("result"))
        .unwrap_or(point);
    Some(payload.clone())
}

/// 检查实体集合中 LLM 内容的结果。
struct LlmContentInfo {
    has_points: bool,
    has_content: bool,
    sample_text: String,
    payload_keys: Vec<String>,
}

/// 检查 Qdrant 实体集合中的一个实体点。
/// 报告点是否存在以及 `llm_analysis` 是否有内容。
async fn check_llm_content(vector: &Arc<dyn VectorRepository>, collection: &str) -> LlmContentInfo {
    let dummy_vec: Vec<f32> = vec![0.0; 1024];
    match vector.search(collection, dummy_vec, 1).await {
        Ok(results) => {
            if let Some(point) = results.first() {
                let payload = point
                    .get("payload")
                    .or(point.get("result"))
                    .unwrap_or(point);
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
            tracing::warn!("check_llm_content: 搜索错误: {e}");
            LlmContentInfo {
                has_points: false,
                has_content: false,
                sample_text: String::new(),
                payload_keys: vec![],
            }
        }
    }
}
