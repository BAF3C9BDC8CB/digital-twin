//! Extract 抽取层——通用知识管线的第一阶段（抽取 → 整合 → 检索）。
//!
//! 将逐块的 LLM 响应转换为结构化的 [`ExtractedGraph`] 值。
//! 本层从不写入图数据库、从不做向量化、从不接触向量存储——由
//! 任务 2 的 Consolidate 层消费产出的 `Vec<ExtractedGraph>`。

pub mod consolidate;
pub mod model;
pub mod retrieve;

use serde::Deserialize;

pub use consolidate::{
    entity_embed_text, entity_id_for, link_same_as, normalize, purge_document, ConsolidateStats,
    Consolidator,
};
pub use model::{EntityType, ExtractedEntity, ExtractedGraph, ExtractedRelation};

/// 统一检索契约使用的明确空摘要。
pub const NO_LLM_ANALYSIS: &str = "暂无摘要";

fn non_empty_analysis(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        NO_LLM_ANALYSIS.to_string()
    } else {
        value.to_string()
    }
}
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
// F4: Nacos 配置专用解析（nacos_config prompt 输出 schema）
// ---------------------------------------------------------------------------

/// `nacos_config` prompt 的输出形状（F4 词表）。
///
/// 与通用 `ExtractedEntity` 的字段名不同：`name` → canonical_name/mention、
/// `purpose` → summary、`properties` 保留为原始 JSON。关系为
/// `from/to/type/evidence` 而非 `head/relation/tail/evidence`。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NacosLlmOutput {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub entities: Vec<NacosLlmEntity>,
    #[serde(default)]
    pub relations: Vec<NacosLlmRelation>,
}

/// Nacos 配置实体（F4 词表：NacosConfig/ConfigKey/ConfigSection/Database/Server）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NacosLlmEntity {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub entity_type: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub properties: serde_json::Value,
}

/// Nacos 配置关系（BELONGS_TO/CONTAINS/IN_NAMESPACE/HAS_SECTION/DETECTED_IN）。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NacosLlmRelation {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default, rename = "type")]
    pub rel_type: String,
    #[serde(default)]
    pub evidence: String,
}

/// 将一条 `nacos_config` 的 LLM 块响应解析为 [`ExtractedGraph`]。
///
/// 与 [`parse_block_response`] 同策略容忍 markdown 围栏/前后赘述；
/// 将 Nacos 输出 schema 映射为统一的 [`ExtractedEntity`]：
/// `name` → `canonical_name` + `mention`，`purpose` → `summary`。
/// `entity_type` 经封闭 [`EntityType`] 词表校验（词表外归一为 `Other` 并 WARN）。
///
/// `doc_id` 与 `block_index` 会盖印到结果上；成功时 `degraded` 恒为 `false`。
pub fn parse_nacos_block_response(
    response: &str,
    doc_id: &str,
    block_index: u32,
) -> Result<ExtractedGraph, serde_json::Error> {
    let mut raw = match serde_json::from_str::<NacosLlmOutput>(response) {
        Ok(g) => g,
        Err(first_err) => {
            let start = response.find('{');
            let end = response.rfind('}');
            match (start, end) {
                (Some(s), Some(e)) if s < e => serde_json::from_str(&response[s..=e])?,
                _ => return Err(first_err),
            }
        }
    };

    // 实体：丢弃 name 为空的无效输出；purpose 为空统一为明确占位。
    raw.entities.retain(|e| {
        let valid = !e.name.trim().is_empty();
        if !valid {
            tracing::warn!(
                "丢弃无效 Nacos 实体（name/purpose 为空）: name='{}'",
                e.name
            );
        }
        valid
    });

    let graph = ExtractedGraph {
        doc_id: doc_id.to_string(),
        block_index,
        block_summary: non_empty_analysis(&raw.summary),
        entities: raw
            .entities
            .into_iter()
            .map(|e| ExtractedEntity {
                mention: e.name.clone(),
                canonical_name: e.name,
                entity_type: serde_json::from_value(serde_json::json!(e.entity_type))
                    .unwrap_or(EntityType::Other),
                summary: non_empty_analysis(&e.purpose),
                keywords: Vec::new(),
            })
            .collect(),
        relations: raw
            .relations
            .into_iter()
            .map(|r| ExtractedRelation {
                head: r.from,
                relation: r.rel_type,
                tail: r.to,
                evidence: if r.evidence.is_empty() {
                    None
                } else {
                    Some(r.evidence)
                },
                confidence: None,
            })
            .collect(),
        degraded: false,
    };

    Ok(graph)
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

    // ── F4: parse_nacos_block_response ───────────────────────────

    /// nacos_config prompt 输出（F4 词表）应解析为 ExtractedGraph，
    /// 类型保持 NacosConfig/ConfigKey/ConfigSection/Database/Server，不归一。
    #[test]
    fn parses_nacos_config_output_with_f4_vocabulary() {
        let raw = r#"{
            "summary": "数据库连接配置",
            "entities": [
                {"name": "application.yaml", "type": "NacosConfig", "purpose": "完整配置文件", "properties": {"namespace": "prod"}},
                {"name": "server.port", "type": "ConfigKey", "purpose": "服务端口", "properties": {"value": "8080"}},
                {"name": "spring.datasource", "type": "ConfigSection", "purpose": "数据源区块", "properties": {}},
                {"name": "order_db", "type": "Database", "purpose": "订单库连接", "properties": {"url": "jdbc:mysql://..."}},
                {"name": "gateway", "type": "Server", "purpose": "网关服务", "properties": {"port": 8080}}
            ],
            "relations": [
                {"from": "application.yaml", "to": "order_db", "type": "CONTAINS", "evidence": "spring.datasource.url"},
                {"from": "gateway", "to": "application.yaml", "type": "DETECTED_IN", "evidence": "服务发现"}
            ]
        }"#;
        let g = parse_nacos_block_response(raw, "dt://nacos/prod/app.yaml", 2).unwrap();
        assert_eq!(g.doc_id, "dt://nacos/prod/app.yaml");
        assert_eq!(g.block_index, 2);
        assert!(!g.degraded);
        assert_eq!(g.block_summary, "数据库连接配置");
        assert_eq!(g.entities.len(), 5);
        assert_eq!(g.entities[0].canonical_name, "application.yaml");
        assert_eq!(g.entities[0].entity_type, EntityType::NacosConfig);
        assert_eq!(g.entities[0].summary, "完整配置文件");
        assert_eq!(g.entities[1].entity_type, EntityType::ConfigKey);
        assert_eq!(g.entities[2].entity_type, EntityType::ConfigSection);
        assert_eq!(g.entities[3].entity_type, EntityType::Database);
        assert_eq!(g.entities[4].entity_type, EntityType::Server);
        // mention 取 name（Nacos schema 无独立 mention）
        assert_eq!(g.entities[0].mention, "application.yaml");
        // 关系映射 from/to/type → head/relation/tail
        assert_eq!(g.relations.len(), 2);
        assert_eq!(g.relations[0].head, "application.yaml");
        assert_eq!(g.relations[0].relation, "CONTAINS");
        assert_eq!(g.relations[0].tail, "order_db");
        assert_eq!(g.relations[1].relation, "DETECTED_IN");
    }

    #[test]
    fn nacos_empty_analysis_uses_unified_placeholder() {
        let raw = r#"{"summary":"  ","entities":[
            {"name":"server.port","type":"ConfigKey","purpose":"  ","properties":{}}],"relations":[]}"#;
        let g = parse_nacos_block_response(raw, DOC, 0).unwrap();
        assert_eq!(g.block_summary, NO_LLM_ANALYSIS);
        assert_eq!(g.entities[0].summary, NO_LLM_ANALYSIS);
    }

    /// nacos_config 输出中的小写/别名类型同样被词表接受。
    #[test]
    fn parses_nacos_lowercase_types() {
        let raw = r#"{
            "summary": "s",
            "entities": [
                {"name": "a", "type": "database", "purpose": "连接"},
                {"name": "b", "type": "server", "purpose": "端口"}
            ],
            "relations": []
        }"#;
        let g = parse_nacos_block_response(raw, DOC, 0).unwrap();
        assert_eq!(g.entities[0].entity_type, EntityType::Database);
        assert_eq!(g.entities[1].entity_type, EntityType::Server);
    }

    /// 词表外类型仍归一为 Other（与 parse_block_response 一致）。
    #[test]
    fn parses_nacos_out_of_vocabulary_falls_back_to_other() {
        let raw = r#"{
            "summary": "s",
            "entities": [{"name": "x", "type": "Widget", "purpose": "未知"}],
            "relations": []
        }"#;
        let g = parse_nacos_block_response(raw, DOC, 0).unwrap();
        assert_eq!(g.entities[0].entity_type, EntityType::Other);
    }

    /// 容忍 markdown 围栏与前后赘述。
    #[test]
    fn parses_nacos_markdown_fenced_json() {
        let raw = "好的：\n```json\n{\"summary\":\"s\",\"entities\":[{\"name\":\"n\",\"type\":\"ConfigKey\",\"purpose\":\"p\"}],\"relations\":[]}\n```\n完毕";
        let g = parse_nacos_block_response(raw, DOC, 1).unwrap();
        assert_eq!(g.entities.len(), 1);
        assert_eq!(g.entities[0].entity_type, EntityType::ConfigKey);
    }
}
