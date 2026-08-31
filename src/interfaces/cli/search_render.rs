//! 检索结果渲染 — 人类格式（类型感知三行制）与 JSON（MCP 消费）。

use crate::application::context::search_mcp::{CrossWorldResult, SearchHit};
use std::collections::HashMap;

const MAX_CHARS: usize = 200;

/// 项目名 → 磁盘绝对根路径 的解析表。
///
/// 由 `config.yaml` 的 `projects` 段构造（base + 别名目录），用于把
/// `dt://doc/{project}/{rel}` 形式的来源解析为磁盘全路径。仅影响
/// CLI 人类可读渲染；不改动 `SearchHit.source_ref` 数据值、JSON 与 MCP。
#[derive(Debug, Default, Clone)]
pub struct ProjectPathResolver {
    /// 项目别名 → 绝对根路径。
    roots: HashMap<String, String>,
}

impl ProjectPathResolver {
    /// 从项目名→绝对根路径对构造。重复名后者覆盖前者。
    pub fn new(roots: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
        }
    }

    /// 取项目名对应的磁盘根路径（不存在返回 None）。
    pub fn root_for(&self, project: &str) -> Option<&str> {
        self.roots.get(project).map(|s| s.as_str())
    }

    /// 将 `dt://doc/{project}/{rel_path}` 解析为磁盘绝对路径。
    ///
    /// 仅解析 `dt://doc/` 前缀且项目名在表中、且相对路径不逃逸根目录
    /// 的情况；其余（`dt://nacos/`、`dt://entity/`、未知项目、
    /// 含 `..` 的路径）返回 `None`，调用方保留原值。
    pub fn resolve_doc_source(&self, source_ref: &str) -> Option<String> {
        let rest = source_ref.strip_prefix("dt://doc/")?;
        let (project, rel) = rest.split_once('/')?;
        if project.is_empty() || rel.is_empty() || rel.contains("..") {
            return None;
        }
        let root = self.roots.get(project)?;
        // 相对路径以 / 开头时去掉,避免拼出双斜杠
        let rel_trim = rel.trim_start_matches('/');
        Some(format!("{}/{}", root.trim_end_matches('/'), rel_trim))
    }
}

fn truncate(s: &str) -> String {
    if s.chars().count() > MAX_CHARS {
        format!("{}…", s.chars().take(MAX_CHARS).collect::<String>())
    } else {
        s.to_string()
    }
}

fn render_hit(h: &SearchHit, show_content: bool, resolver: &ProjectPathResolver) -> String {
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
            match &h.llm_analysis {
                Some(a) if !a.trim().is_empty() => a.clone(),
                // 无 LLM 分析（未处理/failed）：明确标注，不再伪装成位置串。
                // 位置信息由下方"位置:"行单独展示。
                _ => "暂无 LLM 分析".into(),
            },
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
        // code 世界：优先拼磁盘全路径（project + resolver），否则显示相对路径。
        let shown = h
            .project
            .as_ref()
            .and_then(|p| resolver.root_for(p))
            .map(|root| {
                format!(
                    "{}/{}",
                    root.trim_end_matches('/'),
                    fp.trim_start_matches('/')
                )
            })
            .unwrap_or_else(|| fp.clone());
        loc = match (h.start_line, h.end_line) {
            (Some(s), Some(e)) => format!("位置: {shown}:L{s}-{e}"),
            _ => format!("位置: {shown}"),
        };
        if let Some(sig) = &h.signature {
            loc.push_str(&format!("  signature: {sig}"));
        }
    } else if let Some(sr) = &h.source_ref {
        // 人类可读渲染：dt://doc/{项目}/{相对路径} → 磁盘全路径（查得到映射时）；
        // 其余（nacos/entity 虚拟来源、未知项目）保留原始 URI。
        let shown = resolver
            .resolve_doc_source(sr)
            .unwrap_or_else(|| sr.clone());
        loc = format!("来源: {shown}");
        if let Some(hop) = h.hop {
            loc.push_str(&format!("  [hop={hop}]"));
        }
    }
    if !loc.is_empty() {
        out.push_str(&format!("  {loc}\n"));
    }
    // 图增强关系(code 世界 CALLS/CONTAINS, knowledge RELATES):显示调用/被调/所属。
    if let Some(rels) = &h.relations {
        let mut parts: Vec<String> = Vec::new();
        for r in rels {
            let name = r.other_end_name.trim();
            if name.is_empty() {
                continue;
            }
            let label = match (r.rel_type.as_str(), r.direction.as_str()) {
                ("belongs_to", _) => "属于",
                ("calls", _) => "调用",
                ("called_by", _) => "被调",
                (t, _) => t,
            };
            parts.push(format!("{label} {name}"));
        }
        if !parts.is_empty() {
            out.push_str(&format!("  图: {}\n", parts.join("; ")));
        }
    }
    out
}

/// 低分阈值：rerank 分数低于此值的命中通常与查询无关（实测
/// 跨项目污染结果普遍 <0.5，有效命中 >0.66）。低于阈值时提示降级。
const LOW_SCORE_THRESHOLD: f64 = 0.5;

/// 人类格式：类型感知三行制（标题 / 分析·摘要·原文 / 位置·来源）+ 降级尾行。
/// `show_content` 开启时展开正文原文块（`--show-content`）。
/// `resolver` 用于把 `dt://doc/{项目}/...` 来源解析为磁盘全路径（仅展示层）。
///
/// 跨项目结果按 project 分组展示（团队 B 建议 #3），让调用方一眼看出
/// 命中来源分布，避免跨项目噪音淹没目标项目结果。
pub fn render_human(
    result: &CrossWorldResult,
    show_content: bool,
    resolver: &ProjectPathResolver,
) -> String {
    let mut out = String::new();
    if result.hits.is_empty() {
        out.push_str("  (无结果)\n");
    }
    // 按 project 分组（None 归入 "（无项目）"），保持原有相对顺序。
    let mut order: Vec<String> = Vec::new();
    let mut buckets: HashMap<String, Vec<&SearchHit>> = HashMap::new();
    for h in &result.hits {
        let key = h.project.clone().unwrap_or_else(|| "（无项目）".to_string());
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(h);
    }
    if order.len() > 1 {
        out.push_str(&format!(
            "  📦 命中项目分布: {}（共 {} 条）\n",
            order.join(" / "),
            result.hits.len()
        ));
    }
    for key in &order {
        if order.len() > 1 {
            out.push_str(&format!("  ── {} ──\n", key));
        }
        for h in &buckets[key] {
            out.push_str(&render_hit(h, show_content, resolver));
        }
    }
    // 低分降级提示：全部命中低于阈值时，结果大概率与查询无关
    // （跨项目噪音/世界错配），给出显式警示而非静默返回。
    // 注意：world=all 走跨世界 RRF 融合（fusion.rs，分值 = 1/(60+rank)，
    // top1 恒 ≈0.0164，远低于 0.5 阈值），阈值只对单世界（code/knowledge/doc
    // 等的语义分）有效——world=all 时跳过，避免恒误报。
    if !result.hits.is_empty() && result.world != "all" {
        let max_score = result
            .hits
            .iter()
            .map(|h| h.score)
            .fold(f64::NEG_INFINITY, f64::max);
        if max_score < LOW_SCORE_THRESHOLD {
            out.push_str(&format!(
                "  ⚠️ 结果可能不相关：最高分 {max_score:.4} 低于阈值 {LOW_SCORE_THRESHOLD}（常见原因：world 选错或跨项目噪音；可尝试 --world code + --project <项目名>）\n"
            ));
        }
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
            project: Some("copartner-h5".into()),
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

    fn result_with_world(
        hits: Vec<SearchHit>,
        degraded: Vec<String>,
        world: &str,
    ) -> CrossWorldResult {
        CrossWorldResult {
            query: "q".into(),
            world: world.into(),
            total: hits.len(),
            hits,
            per_world_counts: std::collections::HashMap::new(),
            degraded,
        }
    }

    fn result_with(hits: Vec<SearchHit>, degraded: Vec<String>) -> CrossWorldResult {
        result_with_world(hits, degraded, "all")
    }

    fn resolver() -> ProjectPathResolver {
        ProjectPathResolver::new([(
            "pay-center".to_string(),
            "/data/aflmProjects/unimportant/uvp-pay-center".to_string(),
        )])
    }

    #[test]
    fn human_render_method_three_lines() {
        let out = render_human(&result_with(vec![base_hit()], vec![]), false, &resolver());
        assert!(out.contains("[0.9412] [Method] createApp"));
        assert!(out.contains("分析: 用途：创建服务器实例。"));
        assert!(out.contains("位置: test/project/app.js:L32-36"));
        assert!(out.contains("signature: function createApp(port)"));
    }

    #[test]
    fn human_render_method_without_llm_shows_placeholder() {
        // Method 命中但 llm_analysis 缺失（未处理/failed）→ 显示"暂无 LLM 分析"，
        // 不再伪装成 snippet 位置串；位置信息由"位置:"行单独展示。
        let mut h = base_hit();
        h.llm_analysis = None;
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(out.contains("分析: 暂无 LLM 分析"));
        assert!(out.contains("位置: test/project/app.js:L32-36"));
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
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(out.contains("摘要: 支付渠道编码，决定路由"));
        assert!(out.contains("来源: dt://doc/支付架构决策.md"));
        assert!(out.contains("[hop=0]"));
    }

    #[test]
    fn human_render_resolves_doc_source_to_disk_path() {
        // dt://doc/{项目}/{相对路径} → 磁盘全路径（仅展示层,source_ref 数据不变）
        let mut h = base_hit();
        h.entity_type = "Entity".into();
        h.source_world = "knowledge".into();
        h.title = "ifCode".into();
        h.snippet = "s".into();
        h.file_path = None;
        h.source_ref = Some("dt://doc/pay-center/src/main/resources/bootstrap.yml".into());
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(out.contains(
            "来源: /data/aflmProjects/unimportant/uvp-pay-center/src/main/resources/bootstrap.yml"
        ));
        // 未映射项目/虚拟来源保留原 URI
        let mut h2 = base_hit();
        h2.entity_type = "Entity".into();
        h2.source_world = "knowledge".into();
        h2.file_path = None;
        h2.source_ref = Some("dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud".into());
        let out2 = render_human(&result_with(vec![h2], vec![]), false, &resolver());
        assert!(out2.contains("来源: dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud"));
    }

    #[test]
    fn human_render_degraded_footer() {
        let out = render_human(
            &result_with(vec![base_hit()], vec!["rerank_unavailable".into()]),
            false,
            &resolver(),
        );
        assert!(out.contains("降级") && out.contains("rerank_unavailable"));
    }

    #[test]
    fn human_render_all_world_skips_low_score_warning() {
        // world=all 走跨世界 RRF 融合（fusion.rs，分值 = 1/(60+rank)，
        // top1 恒 ≈0.0164，远低于 0.5 阈值）——不得出现低分误报
        //（修复前 RRF 分恒触发"结果可能不相关"告警）。
        let mut h = base_hit();
        h.score = 1.0 / 61.0; // RRF top1 量级
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(!out.contains("结果可能不相关"));
        // 常规输出不受影响
        assert!(out.contains("[0.0164] [Method] createApp"));
    }

    #[test]
    fn human_render_single_world_low_score_still_warns() {
        // 单世界（code/knowledge/doc 等）为语义分，低于阈值仍应告警，
        // 提示文案与格式保持不变。
        let mut h = base_hit();
        h.score = 0.3;
        let out = render_human(
            &result_with_world(vec![h], vec![], "code"),
            false,
            &resolver(),
        );
        assert!(out.contains("⚠️ 结果可能不相关"));
        assert!(out.contains("最高分 0.3000 低于阈值 0.5"));
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
        let out = render_human(&result_with(vec![c], vec![]), false, &resolver());
        assert!(out.contains("[nacos配置/ConfigKey]"));
        assert!(out.contains("分析: 用途：配置 Nacos 服务发现。"));
        assert!(out.contains("来源: dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud"));
        assert!(!out.contains("正文:"));

        // Method：默认不显示正文
        let mut m = base_hit();
        m.content = Some("function createApp(port) {\n  return port;\n}".into());
        let out_m = render_human(&result_with(vec![m], vec![]), false, &resolver());
        assert!(!out_m.contains("正文:"));

        // Doc：默认不显示正文
        let mut d = base_hit();
        d.entity_type = "Doc".into();
        d.content = Some("第一行\n第二行".into());
        d.file_path = None;
        d.start_line = None;
        d.end_line = None;
        d.signature = None;
        let out_d = render_human(&result_with(vec![d], vec![]), false, &resolver());
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
        let out = render_human(&result_with(vec![c], vec![]), true, &resolver());
        assert!(out.contains("正文:"));
        assert!(out.contains("    spring:"));
        assert!(out.contains("      server-addr: nacos:8848  #注释"));

        // Method：展开代码片段原文
        let mut m = base_hit();
        m.content = Some("function createApp(port) {\n  return port;\n}".into());
        let out_m = render_human(&result_with(vec![m], vec![]), true, &resolver());
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
        let out_d = render_human(&result_with(vec![d], vec![]), true, &resolver());
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
        assert_eq!(v["hits"][0]["project"], "copartner-h5");
    }

    #[test]
    fn human_render_method_resolves_disk_path_when_project_known() {
        // project 在 resolver 中 → 位置显示磁盘全路径
        let mut h = base_hit();
        h.project = Some("pay-center".into());
        h.file_path = Some("src/main/java/X.java".into());
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(out.contains(
            "位置: /data/aflmProjects/unimportant/uvp-pay-center/src/main/java/X.java:L32-36"
        ));
    }

    #[test]
    fn human_render_method_falls_back_to_relative_when_project_unknown() {
        // project 不在 resolver 中 → 回退相对路径
        let mut h = base_hit();
        h.project = Some("unknown-proj".into());
        h.file_path = Some("src/main/java/X.java".into());
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(out.contains("位置: src/main/java/X.java:L32-36"));
    }

    #[test]
    fn human_render_method_without_project_falls_back_to_relative() {
        // project 为 None（旧数据/其他世界）→ 回退相对路径
        let mut h = base_hit();
        h.project = None;
        h.file_path = Some("src/main/java/X.java".into());
        let out = render_human(&result_with(vec![h], vec![]), false, &resolver());
        assert!(out.contains("位置: src/main/java/X.java:L32-36"));
    }
}
