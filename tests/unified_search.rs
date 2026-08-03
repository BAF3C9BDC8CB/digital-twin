//! 统一检索 live 验收（需 Memgraph+Qdrant+xinference 在线）：
//! cargo test --test unified_search -- --ignored --nocapture

use std::process::Command;

fn dt(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_dt"))
        .args(args)
        .output()
        .expect("dt binary");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn search_json(args: &[&str]) -> serde_json::Value {
    let (stdout, stderr, code) = dt(args);
    assert_eq!(code, 0, "dt search failed: {stderr}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not pure JSON: {e}\n--- stdout ---\n{stdout}"))
}

#[test]
#[ignore]
fn u_all_world_finds_createapp_with_analysis_and_location() {
    let v = search_json(&["search", "createApp", "--json"]);
    let hits = v["hits"].as_array().unwrap();
    // RRF 融合后第一 Method 未必是 createApp（code 世界 rank1 是 createOrder）——
    // 按 title 精确定位目标命中
    let m = hits
        .iter()
        .find(|h| h["entity_type"] == "Method" && h["title"] == "createApp")
        .unwrap_or_else(|| panic!("no createApp Method hit in all-world results: {hits:?}"));
    assert!(m["file_path"].as_str().unwrap().contains("app.js"));
    assert_eq!(m["start_line"], 32);
    assert!(!m["llm_analysis"].as_str().unwrap_or("").is_empty());
}

#[test]
#[ignore]
fn u_knowledge_ifcode_semantic_hit_via_cli() {
    let v = search_json(&["search", "新增渠道的唯一代码标识", "--world", "knowledge", "--json"]);
    let hits = v["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "knowledge world empty");
    let top3: Vec<String> = hits
        .iter()
        .take(3)
        .map(|h| h["title"].as_str().unwrap_or("").to_lowercase())
        .collect();
    assert!(top3.iter().any(|t| t.contains("ifcode")), "ifCode not in top3: {top3:?}");
}

/// memory 世界：当前 Memgraph 无预置事件节点（S5 Task 0 清库重建后事件为空），
/// 故自种一粒探针事件再检索——种子→断言→清理，自包含。
#[tokio::test]
#[ignore]
async fn u_memory_world_finds_seeded_event() {
    use dt_daemon::domain::traits::GraphRepository;
    use std::collections::HashMap;
    use std::sync::Arc;

    let graph = dt_daemon::infrastructure::memgraph::MemgraphClient::connect(
        "bolt://localhost:7688",
        "memgraph",
        "",
    )
    .await
    .expect("memgraph connect");
    let repo: Arc<dyn GraphRepository> = Arc::new(graph);

    let mut params = HashMap::new();
    params.insert("name".into(), serde_json::Value::String("unified-search-live-probe".into()));
    params.insert(
        "details".into(),
        serde_json::Value::String("UNIFIED_PROBE_7X9 事件检索探针".into()),
    );
    repo.write_query(
        "CREATE (n:Decision {name: $name, entity_id: $name, details: $details})",
        params.clone(),
    )
    .await
    .expect("seed probe event");

    let v = search_json(&["search", "UNIFIED_PROBE_7X9", "--world", "memory", "--json"]);
    let hits = v["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| h["entity_type"] == "Decision"
            && h["title"] == "unified-search-live-probe"),
        "seeded event not found: {hits:?}"
    );

    repo.write_query(
        "MATCH (n:Decision {name: $name}) DELETE n",
        params,
    )
    .await
    .expect("cleanup probe event");
}

#[test]
#[ignore]
fn u_config_world_returns_valid_json() {
    // 本地无 config_chunks 数据——只验证管线可用与 JSON 结构（不断言具体结果）
    let v = search_json(&["search", "nacos", "--world", "config", "--json"]);
    assert!(v["hits"].is_array());
    assert!(v["total"].is_u64());
}

#[test]
#[ignore]
fn u_search_kg_removed_clap_error() {
    let (_stdout, stderr, code) = dt(&["search-kg", "foo"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
}

#[test]
#[ignore]
fn u_human_format_three_lines_for_method() {
    let (stdout, _stderr, code) = dt(&["search", "createApp", "--world", "code"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("[Method] createApp"), "{stdout}");
    assert!(stdout.contains("分析:"), "{stdout}");
    assert!(stdout.contains("app.js:L32-36"), "{stdout}");
}
