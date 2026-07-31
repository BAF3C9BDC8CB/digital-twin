//! Unified output model of the Extract layer (方案 §5.3).
//!
//! The LLM extraction processor produces one [`ExtractedGraph`] per document
//! block. Task 2's Consolidate layer consumes `Vec<ExtractedGraph>` — the
//! Extract layer itself never writes the graph database, never embeds, and
//! never touches the vector store.
//!
//! This module is pure serde data structures — no infrastructure types.

use serde::{Deserialize, Deserializer, Serialize};

/// Structured knowledge extracted from a single document block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedGraph {
    /// Document ID from the chunk processor output (`dt://doc/{project}/{path}`).
    #[serde(default)]
    pub doc_id: String,
    /// Block index, equal to `chunk.chunk_index`.
    #[serde(default)]
    pub block_index: u32,
    /// One-line summary of the block content.
    #[serde(default)]
    pub block_summary: String,
    /// Entities extracted from the block.
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    /// Relations extracted from the block.
    #[serde(default)]
    pub relations: Vec<ExtractedRelation>,
    /// Degradation marker — set when JSON parsing failed even after one retry (§5.5).
    #[serde(default)]
    pub degraded: bool,
}

/// A single extracted entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedEntity {
    /// Mention as it appears in the source text.
    #[serde(default)]
    pub mention: String,
    /// Canonical name — the raw material of the disambiguation primary key.
    #[serde(default)]
    pub canonical_name: String,
    /// Fixed-vocabulary entity type. Out-of-vocabulary values degrade to
    /// [`EntityType::Other`] (see its custom `Deserialize`).
    #[serde(rename = "type", default)]
    pub entity_type: EntityType,
    /// One-sentence semantic summary — the core text for embedding.
    #[serde(default)]
    pub summary: String,
    /// Keywords; tolerated missing (defaults to empty).
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Fixed entity type vocabulary — a closed set, not free text.
///
/// The Consolidate layer's "type consistency" hard constraint relies on this
/// being a closed set. Out-of-vocabulary values produced by the LLM are
/// normalised to [`EntityType::Other`] during deserialization (with a warn
/// log recording the original value).
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

impl<'de> Deserialize<'de> for EntityType {
    /// Closed-vocabulary deserialization. Out-of-vocabulary values are
    /// normalised to [`EntityType::Other`] with a warn log recording the
    /// original value; explicit `null` also maps to `Other`.
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

/// A single extracted relation triple.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedRelation {
    /// Must equal some entity's `canonical_name`.
    #[serde(default)]
    pub head: String,
    /// Normalised verb, e.g. `routes_to` / `depends_on` / `configured_by`.
    #[serde(default)]
    pub relation: String,
    /// Must equal some entity's `canonical_name`.
    #[serde(default)]
    pub tail: String,
    /// `Option` is required: the prompt rules allow "set null when unsure"
    /// (§5.4). Deserialising an explicit null into `String`/`f32` would fail
    /// and wrongly trigger the §5.5 degradation path. `Option` tolerates both
    /// a missing field and an explicit null.
    #[serde(default)]
    pub evidence: Option<String>,
    /// See [`ExtractedRelation::evidence`] for why this is an `Option`.
    #[serde(default)]
    pub confidence: Option<f32>,
}

// ---------------------------------------------------------------------------
// Tests
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
            assert_eq!(t, expected, "vocabulary value {s}");
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
