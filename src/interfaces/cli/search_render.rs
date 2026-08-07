//! 检索结果渲染 — 人类格式（类型感知三行制）与 JSON（MCP 消费）。

use crate::application::context::search_mcp::{CrossWorldResult, SearchHit};

const MAX_CHARS: usize = 200;

fn truncate(s: &str) -> String {
    if s.chars().count() > MAX_CHARS {
        format!("{}…", s.chars().take(MAX_CHARS).collect::<String>())
    } else {
        s.to_string()
    }
}

fn render_hit(h: &SearchHit) -> String {
    let type_tag = match (&h.file_type_label, h.entity_type.as_str()) {
        (Some(ftl), et) if !et.is_empty() && et != "?" => format!("{ftl}/{et}"),
        (Some(ftl), _) => ftl.to_string(),
        (None, et) if !et.is_empty() => et.to_string(),
        _ => "?".to_string(),
    };
    let mut out = format!("[{:.4}] [{}] {}\n", h.score, type_tag, h.title);
    let (label, body) = match h.entity_type.as_str() {
        "Method" => (
            "分析",
            h.llm_analysis.clone().unwrap_or_else(|| h.snippet.clone()),
        ),
        "Doc" => ("原文", h.content.clone().unwrap_or_else(|| h.snippet.clone())),
        "Config" | "ConfigChunk" | "ConfigKey" => (
            "分析",
            h.llm_analysis.clone().unwrap_or_else(|| "暂无摘要".into()),
        ),
        _ => ("摘要", h.snippet.clone()),
    };
    if matches!(h.entity_type.as_str(), "Config" | "ConfigChunk" | "ConfigKey") {
        if let Some(content) = &h.content {
            out.push_str(&format!("  正文:\n{}\n", content.lines().map(|l| format!("    {}", l)).collect::<Vec<_>>().join("\n")));
        }
    }
    if !body.is_empty() {
        let one_line = body.lines().collect::<Vec<_>>().join("；");
        out.push_str(&format!("  {label}: {}\n", truncate(&one_line)));
    }
    let mut loc = String::new();
    if let Some(fp) = &h.file_path {
        loc = match (h.start_line, h.end_line) {
            (Some(s), Some(e)) => format!("位置: {fp}:L{s}-{e}"),
            _ => format!("位置: {fp}"),
        };
        if let Some(sig) = &h.signature {
            loc.push_str(&format!("  signature: {sig}"));
        }
    } else if let Some(sr) = &h.source_ref {
        loc = format!("来源: {sr}");
        if let Some(hop) = h.hop {
            loc.push_str(&format!("  [hop={hop}]"));
        }
    }
    if !loc.is_empty() {
        out.push_str(&format!("  {loc}\n"));
    }
    out
}

/// 人类格式：类型感知三行制（标题 / 分析·摘要·原文 / 位置·来源）+ 降级尾行。
pub fn render_human(result: &CrossWorldResult) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("  (无结果)\n");
    }
    for h in &result.hits {
        out.push_str(&render_hit(h));
    }
    if !result.degraded.is_empty() {
        out.push_str(&format!("  ⚠️ 降级: {}\n", result.degraded.join(", ")));
    }
    out
}

/// JSON：MCP/脚本消费（stdout 纯净约束由调用方保证——不打印任何其他行）。
pub fn render_json(result: &CrossWorldResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_hit() -> SearchHit {
        SearchHit {
            id: "1".into(),
            title: "createApp".into(),
            snippet: String::new(),
            content: None,
            source_world: "code".into(),
            entity_type: "Method".into(),
            file_type: None,
            file_type_label: None,
            score: 0.9412,
            source_ref: None,
            metadata: None,
            file_path: Some("test/project/app.js".into()),
            start_line: Some(32),
            end_line: Some(36),
            signature: Some("function createApp(port)".into()),
            calls: vec![],
            element_id: None,
            llm_analysis: Some("用途：创建服务器实例。\n逻辑：实例化服务器对象。".into()),
            score_breakdown: None,
            hop: None,
            via_same_as: None,
            relations: None,
            evidence: None,
            rerank_degraded: None,
        }
    }

    fn result_with(hits: Vec<SearchHit>, degraded: Vec<String>) -> CrossWorldResult {
        CrossWorldResult {
            query: "q".into(),
            world: "all".into(),
            total: hits.len(),
            hits,
            per_world_counts: std::collections::HashMap::new(),
            degraded,
        }
    }

    #[test]
    fn human_render_method_three_lines() {
        let out = render_human(&result_with(vec![base_hit()], vec![]));
        assert!(out.contains("[0.9412] [Method] createApp"));
        assert!(out.contains("分析: 用途：创建服务器实例。"));
        assert!(out.contains("位置: test/project/app.js:L32-36"));
        assert!(out.contains("signature: function createApp(port)"));
    }

    #[test]
    fn human_render_entity_shows_summary_source_and_hop() {
        let mut h = base_hit();
        h.entity_type = "Entity".into();
        h.source_world = "knowledge".into();
        h.title = "ifCode".into();
        h.snippet = "支付渠道编码，决定路由".into();
        h.llm_analysis = None;
        h.file_path = None;
        h.start_line = None;
        h.end_line = None;
        h.signature = None;
        h.source_ref = Some("dt://doc/支付架构决策.md".into());
        h.hop = Some(0);
        let out = render_human(&result_with(vec![h], vec![]));
        assert!(out.contains("摘要: 支付渠道编码，决定路由"));
        assert!(out.contains("来源: dt://doc/支付架构决策.md"));
        assert!(out.contains("[hop=0]"));
    }

    #[test]
    fn human_render_degraded_footer() {
        let out = render_human(&result_with(
            vec![base_hit()],
            vec!["rerank_unavailable".into()],
        ));
        assert!(out.contains("降级") && out.contains("rerank_unavailable"));
    }

    #[test]
    fn json_render_is_pure_parseable_json() {
        let out = render_json(&result_with(vec![base_hit()], vec![]));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["hits"][0]["llm_analysis"],
            "用途：创建服务器实例。\n逻辑：实例化服务器对象。"
        );
        assert_eq!(v["hits"][0]["start_line"], 32);
    }
}
