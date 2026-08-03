//! Extract 层的统一输出模型（方案 §5.3）。
//!
//! LLM 抽取处理器为每个文档块产出一个 [`ExtractedGraph`]。
//! 任务 2 的 Consolidate 层消费 `Vec<ExtractedGraph>`——Extract 层本身
//! 从不写图数据库、从不做向量化、从不接触向量存储。
//!
//! 本模块是纯 serde 数据结构——不含任何基础设施类型。

use serde::{Deserialize, Deserializer, Serialize};

/// 从单个文档块抽取出的结构化知识。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedGraph {
    /// 来自分块处理器输出的文档 ID（`dt://doc/{project}/{path}`）。
    #[serde(default)]
    pub doc_id: String,
    /// 块索引，等于 `chunk.chunk_index`。
    #[serde(default)]
    pub block_index: u32,
    /// 块内容的一行摘要。
    #[serde(default)]
    pub block_summary: String,
    /// 从块中抽取的实体。
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    /// 从块中抽取的关系。
    #[serde(default)]
    pub relations: Vec<ExtractedRelation>,
    /// 降级标记——重试一次后 JSON 解析仍失败时置位（§5.5）。
    #[serde(default)]
    pub degraded: bool,
}

/// 单个抽取出的实体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedEntity {
    /// 源文本中出现的原样提及。
    #[serde(default)]
    pub mention: String,
    /// 规范名——消歧主键的原材料。
    #[serde(default)]
    pub canonical_name: String,
    /// 固定词表的实体类型。词表外的值降级为
    /// [`EntityType::Other`]（见其自定义 `Deserialize`）。
    #[serde(rename = "type", default)]
    pub entity_type: EntityType,
    /// 一句话语义摘要——向量化的核心文本。
    #[serde(default)]
    pub summary: String,
    /// 关键词；允许缺失（默认为空）。
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// 固定实体类型词表——封闭集合，非自由文本。
///
/// Consolidate 层的“类型一致性”硬约束依赖此封闭集合。
/// LLM 产生的词表外值在反序列化时归一为 [`EntityType::Other`]
/// （warn 日志记录原始值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum EntityType {
    Service,
    Channel,
    Config,
    Table,
    Api,
    Concept,
    Person,
    Org,
    Product,
    #[default]
    Other,
}

impl EntityType {
    /// 枚举变体名，用于 `dt://entity/{project}/{type}/{canonical}`
    /// 实体 ID 及图 `Entity` 节点的 `type` 属性（§6.1/§7.2）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Service => "Service",
            Self::Channel => "Channel",
            Self::Config => "Config",
            Self::Table => "Table",
            Self::Api => "Api",
            Self::Concept => "Concept",
            Self::Person => "Person",
            Self::Org => "Org",
            Self::Product => "Product",
            Self::Other => "Other",
        }
    }
}

impl<'de> Deserialize<'de> for EntityType {
    /// 封闭词表反序列化。词表外值归一为 [`EntityType::Other`]
    /// 并用 warn 日志记录原始值；显式 `null` 同样映射为 `Other`。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(match value.as_deref() {
            Some("Service") => Self::Service,
            Some("Channel") => Self::Channel,
            Some("Config") => Self::Config,
            Some("Table") => Self::Table,
            Some("Api") => Self::Api,
            Some("Concept") => Self::Concept,
            Some("Person") => Self::Person,
            Some("Org") => Self::Org,
            Some("Product") => Self::Product,
            Some("Other") => Self::Other,
            Some(other) => {
                tracing::warn!("LLM 返回词表外实体类型 '{other}'，归一为 Other");
                Self::Other
            }
            None => Self::Other,
        })
    }
}

/// 单个抽取出的关系三元组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedRelation {
    /// 必须等于某个实体的 `canonical_name`。
    #[serde(default)]
    pub head: String,
    /// 归一化后的动词，如 `routes_to` / `depends_on` / `configured_by`。
    #[serde(default)]
    pub relation: String,
    /// 必须等于某个实体的 `canonical_name`。
    #[serde(default)]
    pub tail: String,
    /// `Option` 是必须的：提示词规则允许“不确定就置 null”（§5.4）。
    /// 若把显式 null 反序列化进 `String`/`f32` 会失败，从而错误触发
    /// §5.5 降级路径。`Option` 同时容忍字段缺失与显式 null。
    #[serde(default)]
    pub evidence: Option<String>,
    /// 为何是 `Option` 的原因见 [`ExtractedRelation::evidence`]。
    #[serde(default)]
    pub confidence: Option<f32>,
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn entity_type_deserializes_vocabulary() {
        let cases = [
            ("Service", EntityType::Service),
            ("Channel", EntityType::Channel),
            ("Config", EntityType::Config),
            ("Table", EntityType::Table),
            ("Api", EntityType::Api),
            ("Concept", EntityType::Concept),
            ("Person", EntityType::Person),
            ("Org", EntityType::Org),
            ("Product", EntityType::Product),
            ("Other", EntityType::Other),
        ];
        for (s, expected) in cases {
            let t: EntityType = serde_json::from_value(json!(s)).unwrap();
            assert_eq!(t, expected, "词表值 {s}");
        }
    }

    #[test]
    fn entity_type_out_of_vocabulary_falls_back_to_other() {
        let t: EntityType = serde_json::from_value(json!("Widget")).unwrap();
        assert_eq!(t, EntityType::Other);
    }

    #[test]
    fn entity_type_null_falls_back_to_other() {
        let t: EntityType = serde_json::from_value(json!(null)).unwrap();
        assert_eq!(t, EntityType::Other);
    }

    #[test]
    fn entity_deserializes_prompt_shape() {
        let raw = json!({
            "mention": "支付网关服务",
            "canonical_name": "支付网关",
            "type": "Service",
            "summary": "处理支付路由",
            "keywords": ["支付", "路由"]
        });
        let e: ExtractedEntity = serde_json::from_value(raw).unwrap();
        assert_eq!(e.mention, "支付网关服务");
        assert_eq!(e.canonical_name, "支付网关");
        assert_eq!(e.entity_type, EntityType::Service);
        assert_eq!(e.summary, "处理支付路由");
        assert_eq!(e.keywords, vec!["支付".to_string(), "路由".to_string()]);
    }

    #[test]
    fn entity_keywords_default_to_empty() {
        let raw = json!({
            "mention": "m",
            "canonical_name": "c",
            "type": "Other",
            "summary": "s"
        });
        let e: ExtractedEntity = serde_json::from_value(raw).unwrap();
        assert!(e.keywords.is_empty());
    }

    #[test]
    fn entity_type_missing_defaults_to_other() {
        let raw = json!({"mention": "m", "canonical_name": "c", "summary": "s"});
        let e: ExtractedEntity = serde_json::from_value(raw).unwrap();
        assert_eq!(e.entity_type, EntityType::Other);
    }

    #[test]
    fn relation_tolerates_explicit_null_optionals() {
        let raw = json!({
            "head": "A",
            "relation": "depends_on",
            "tail": "B",
            "evidence": null,
            "confidence": null
        });
        let r: ExtractedRelation = serde_json::from_value(raw).unwrap();
        assert_eq!(r.head, "A");
        assert_eq!(r.relation, "depends_on");
        assert_eq!(r.tail, "B");
        assert_eq!(r.evidence, None);
        assert_eq!(r.confidence, None);
    }

    #[test]
    fn relation_tolerates_missing_optionals() {
        let raw = json!({"head": "A", "relation": "depends_on", "tail": "B"});
        let r: ExtractedRelation = serde_json::from_value(raw).unwrap();
        assert_eq!(r.evidence, None);
        assert_eq!(r.confidence, None);
    }

    #[test]
    fn entity_serializes_type_key() {
        let e = ExtractedEntity {
            mention: "m".into(),
            canonical_name: "c".into(),
            entity_type: EntityType::Service,
            summary: "s".into(),
            keywords: vec![],
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], json!("Service"));
        assert!(v.get("entity_type").is_none());
    }

    #[test]
    fn graph_serializes_full_shape() {
        let g = ExtractedGraph {
            doc_id: "dt://doc/p/f.md".into(),
            block_index: 2,
            block_summary: "s".into(),
            entities: vec![],
            relations: vec![],
            degraded: true,
        };
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!(v["doc_id"], json!("dt://doc/p/f.md"));
        assert_eq!(v["block_index"], json!(2));
        assert_eq!(v["degraded"], json!(true));
    }
}
