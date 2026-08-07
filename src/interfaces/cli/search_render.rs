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

fn render_hit(h: &SearchHit, show_content: bool) -> String {
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
        "Doc" => (
            "原文",
            h.content.clone().unwrap_or_else(|| h.snippet.clone()),
        ),
        "Config" | "ConfigChunk" | "ConfigKey" => (
            "分析",
            h.llm_analysis.clone().unwrap_or_else(|| "暂无摘要".into()),
        ),
        _ => ("摘要", h.snippet.clone()),
    };
    // 正文展开：仅 --show-content 开启且 content 存在时输出原文块（Config/Method/Doc 通用）。
    if show_content {
        if let Some(content) = &h.content {
            if !content.is_empty() {
                out.push_str(&format!(
                    "  正文:\n{}\n",
                    content
                        .lines()
                        .map(|l| format!("    {}", l))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
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
/// `show_content` 开启时展开正文原文块（`--show-content`）。
pub fn render_human(result: &CrossWorldResult, show_content: bool) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("  (无结果)\n");
    }
    for h in &result.hits {
        out.push_str(&render_hit(h, show_content));
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
        let out = render_human(&result_with(vec![base_hit()], vec![]), false);
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
        let out = render_human(&result_with(vec![h], vec![]), false);
        assert!(out.contains("摘要: 支付渠道编码，决定路由"));
        assert!(out.contains("来源: dt://doc/支付架构决策.md"));
        assert!(out.contains("[hop=0]"));
    }

    #[test]
    fn human_render_degraded_footer() {
        let out = render_human(
            &result_with(vec![base_hit()], vec!["rerank_unavailable".into()]),
            false,
        );
        assert!(out.contains("降级") && out.contains("rerank_unavailable"));
    }

    #[test]
    fn human_render_default_hides_content_for_all_types() {
        // ConfigKey：默认不显示正文
        let mut c = base_hit();
        c.entity_type = "ConfigKey".into();
        c.file_type = Some("nacos_config".into());
        c.file_type_label = Some("nacos配置".into());
        c.content = Some("spring:\n  cloud:\n    nacos:\n      server-addr: x".into());
        c.file_path = None;
        c.start_line = None;
        c.end_line = None;
        c.signature = None;
        c.source_ref = Some("dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud".into());
        c.llm_analysis = Some("用途：配置 Nacos 服务发现。".into());
        let out = render_human(&result_with(vec![c], vec![]), false);
        assert!(out.contains("[nacos配置/ConfigKey]"));
        assert!(out.contains("分析: 用途：配置 Nacos 服务发现。"));
        assert!(out.contains("来源: dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud"));
        assert!(!out.contains("正文:"));

        // Method：默认不显示正文
        let mut m = base_hit();
        m.content = Some("function createApp(port) {\n  return port;\n}".into());
        let out_m = render_human(&result_with(vec![m], vec![]), false);
        assert!(!out_m.contains("正文:"));

        // Doc：默认不显示正文
        let mut d = base_hit();
        d.entity_type = "Doc".into();
        d.content = Some("第一行\n第二行".into());
        d.file_path = None;
        d.start_line = None;
        d.end_line = None;
        d.signature = None;
        let out_d = render_human(&result_with(vec![d], vec![]), false);
        assert!(!out_d.contains("正文:"));
    }

    #[test]
    fn human_render_show_content_expands_all_three_types() {
        // ConfigKey：正文展开，保留原始缩进
        let mut c = base_hit();
        c.entity_type = "ConfigKey".into();
        c.file_type = Some("nacos_config".into());
        c.file_type_label = Some("nacos配置".into());
        c.content =
            Some("spring:\n  cloud:\n    nacos:\n      server-addr: nacos:8848  #注释".into());
        c.file_path = None;
        c.start_line = None;
        c.end_line = None;
        c.signature = None;
        c.source_ref = Some("dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud".into());
        let out = render_human(&result_with(vec![c], vec![]), true);
        assert!(out.contains("正文:"));
        assert!(out.contains("    spring:"));
        assert!(out.contains("      server-addr: nacos:8848  #注释"));

        // Method：展开代码片段原文
        let mut m = base_hit();
        m.content = Some("function createApp(port) {\n  return port;\n}".into());
        let out_m = render_human(&result_with(vec![m], vec![]), true);
        assert!(out_m.contains("正文:"));
        assert!(out_m.contains("    function createApp(port) {"));
        assert!(out_m.contains("      return port;"));

        // Doc：展开原文
        let mut d = base_hit();
        d.entity_type = "Doc".into();
        d.content = Some("第一行\n第二行".into());
        d.file_path = None;
        d.start_line = None;
        d.end_line = None;
        d.signature = None;
        let out_d = render_human(&result_with(vec![d], vec![]), true);
        assert!(out_d.contains("正文:"));
        assert!(out_d.contains("    第一行"));
        assert!(out_d.contains("    第二行"));
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
