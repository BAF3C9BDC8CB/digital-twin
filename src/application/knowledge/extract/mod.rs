//! Extract 抽取层——通用知识管线的第一阶段（抽取 → 整合 → 检索）。
//!
//! 将逐块的 LLM 响应转换为结构化的 [`ExtractedGraph`] 值。
//! 本层从不写入图数据库、从不做向量化、从不接触向量存储——由
//! 任务 2 的 Consolidate 层消费产出的 `Vec<ExtractedGraph>`。

pub mod consolidate;
pub mod model;
pub mod retrieve;

pub use consolidate::{
    entity_embed_text, entity_id_for, link_same_as, normalize, purge_document, ConsolidateStats,
    Consolidator,
};
pub use model::{EntityType, ExtractedEntity, ExtractedGraph, ExtractedRelation};

/// 将一条 LLM 块响应解析为 [`ExtractedGraph`]（§5.5）。
///
/// 容忍 markdown 围栏或前后赘述：先尝试整体解析，
/// 失败则回退到从第一个 `{` 到最后一个 `}` 的子串。
///
/// `canonical_name` 或 `summary` 为空的实体是无效输出——
/// 在此丢弃（并记 warn 日志），且**不**将块标记为降级。
///
/// `doc_id` 与 `block_index` 会盖印到结果上；成功时 `degraded` 恒为 `false`。
pub fn parse_block_response(
    response: &str,
    doc_id: &str,
    block_index: u32,
) -> Result<ExtractedGraph, serde_json::Error> {
    let mut graph = match serde_json::from_str::<ExtractedGraph>(response) {
        Ok(g) => g,
        Err(first_err) => {
            // 容忍 markdown 围栏 / 前后赘述：用第一个 '{' 到最后一个 '}' 的子串重试。
            let start = response.find('{');
            let end = response.rfind('}');
            match (start, end) {
                (Some(s), Some(e)) if s < e => serde_json::from_str(&response[s..=e])?,
                _ => return Err(first_err),
            }
        }
    };

    graph.doc_id = doc_id.to_string();
    graph.block_index = block_index;
    graph.degraded = false;

    // 丢弃无效实体（canonical_name/summary 缺失或为空）——
    // 这不属于块的降级。
    graph.entities.retain(|e| {
        let valid = !e.canonical_name.trim().is_empty() && !e.summary.trim().is_empty();
        if !valid {
            tracing::warn!(
                "丢弃无效实体（canonical_name/summary 为空）: mention='{}'",
                e.mention
            );
        }
        valid
    });

    Ok(graph)
}

/// 构建块的降级占位图（§5.5）：空的 summary/entities/relations 且 `degraded = true`。
pub fn degraded_graph(doc_id: &str, block_index: u32) -> ExtractedGraph {
    ExtractedGraph {
        doc_id: doc_id.to_string(),
        block_index,
        block_summary: String::new(),
        entities: Vec::new(),
        relations: Vec::new(),
        degraded: true,
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "dt://doc/proj/readme.md";

    #[test]
    fn parses_plain_json() {
        let raw = r#"{"block_summary":"概述","entities":[{"mention":"m","canonical_name":"支付网关","type":"Service","summary":"路由支付"}],"relations":[{"head":"支付网关","relation":"routes_to","tail":"订单服务","evidence":null,"confidence":0.9}]}"#;
        let g = parse_block_response(raw, DOC, 3).unwrap();
        assert_eq!(g.doc_id, DOC);
        assert_eq!(g.block_index, 3);
        assert_eq!(g.block_summary, "概述");
        assert_eq!(g.entities.len(), 1);
        assert_eq!(g.entities[0].canonical_name, "支付网关");
        assert_eq!(g.relations.len(), 1);
        assert_eq!(g.relations[0].confidence, Some(0.9));
        assert!(!g.degraded);
    }

    #[test]
    fn parses_markdown_fenced_json_with_prose() {
        let raw = "当然，以下是抽取结果：\n```json\n{\"block_summary\":\"s\",\"entities\":[],\"relations\":[]}\n```\n希望对你有帮助";
        let g = parse_block_response(raw, DOC, 0).unwrap();
        assert_eq!(g.block_summary, "s");
        assert!(!g.degraded);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_block_response("完全不是 JSON", DOC, 0).is_err());
        assert!(parse_block_response("", DOC, 0).is_err());
    }

    #[test]
    fn drops_entities_without_canonical_name_or_summary() {
        let raw = r#"{"block_summary":"s","entities":[
            {"mention":"a","canonical_name":"","type":"Service","summary":"有摘要"},
            {"mention":"b","canonical_name":"有名字","type":"Api","summary":""},
            {"mention":"c","canonical_name":"   ","type":"Api","summary":"空白名字"},
            {"mention":"d","canonical_name":"好实体","type":"Api","summary":"好摘要"}
        ],"relations":[]}"#;
        let g = parse_block_response(raw, DOC, 0).unwrap();
        assert_eq!(g.entities.len(), 1);
        assert_eq!(g.entities[0].canonical_name, "好实体");
        // 丢弃无效实体不算降级。
        assert!(!g.degraded);
    }

    #[test]
    fn degraded_graph_has_empty_payload() {
        let g = degraded_graph(DOC, 7);
        assert_eq!(g.doc_id, DOC);
        assert_eq!(g.block_index, 7);
        assert!(g.block_summary.is_empty());
        assert!(g.entities.is_empty());
        assert!(g.relations.is_empty());
        assert!(g.degraded);
    }
}
