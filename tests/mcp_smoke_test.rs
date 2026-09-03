//! dt-mcp 冒烟测试: 管道发送 initialize + tools/list + dt_health 调用

use std::io::Write;
use std::process::{Command, Stdio};

fn dt_mcp_bin() -> String {
    std::env::var("CARGO_BIN_EXE_dt_mcp").unwrap_or_else(|_| "target/debug/dt-mcp".to_string())
}

fn run_mcp(input: &str) -> String {
    let mut child = Command::new(dt_mcp_bin())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn dt-mcp");

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait dt-mcp");
    assert!(
        output.status.success(),
        "dt-mcp should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn test_mcp_initialize_and_list_tools() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"smoke\",\"version\":\"0.0.1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
    );

    let out = run_mcp(input);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() >= 2, "expected >=2 response lines, got: {out}");

    let init: serde_json::Value =
        serde_json::from_str(lines[0]).expect("init response should be JSON");
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "digital-twin");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let tools: serde_json::Value =
        serde_json::from_str(lines[1]).expect("tools/list response should be JSON");
    assert_eq!(tools["id"], 2);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name should be string"))
        .collect();
    assert_eq!(
        names,
        vec![
            "dt_search_kg",
            "dt_search",
            "dt_sense",
            "dt_memorize",
            "dt_event",
            "dt_learn",
            "dt_build",
            "dt_kg_sync",
            "dt_health",
            "dt_backup",
        ],
        "tool list must match required 10 dt_* tools"
    );

    // 抽查 schema: dt_search_kg 要求 query 必填
    let tools_arr = tools["result"]["tools"].as_array().unwrap();
    let search_tool = tools_arr
        .iter()
        .find(|t| t["name"] == "dt_search_kg")
        .unwrap();
    assert_eq!(search_tool["inputSchema"]["required"][0], "query");
}

#[test]
fn test_mcp_health_call() {
    // 调用 dt_health: 有后端输出健康报告, 无后端也应正常返回(不崩溃)
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"smoke\",\"version\":\"0.0.1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"dt_health\",\"arguments\":{}}}\n",
    );

    let out = run_mcp(input);
    let lines: Vec<&str> = out.lines().collect();
    let call: serde_json::Value =
        serde_json::from_str(lines[1]).expect("call response should be JSON");
    assert_eq!(call["id"], 2);
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("should have text");
    assert!(
        text.contains("正在检查后端健康状态"),
        "health output: {text}"
    );
}
