//! Qdrant collection name conventions.
//!
//! Phase 5+6: Global collections with project as payload tag.
//! Legacy `{project}_xxx` collections are kept for backward compatibility
//! during migration — search queries both and fuses via RRF.

/// Global collection for code method vectors.
pub const CODE_METHODS: &str = "code_methods";

/// Global collection for document chunk vectors.
pub const DOC_CHUNKS: &str = "doc_chunks";

/// Global collection for KG business node vectors.
pub const KG_NODES: &str = "kg_nodes";

/// Global collection for config chunk vectors (dt sync --config-chunks 写入).
pub const CONFIG_CHUNKS: &str = "config_chunks";

/// Vector dimension (BGE-M3 = 1024).
pub const VECTOR_DIM: u32 = 1024;

/// Resolve a collection name for the given source type.
///
/// Phase 5+6: returns global collection names (project is a payload tag, not
/// part of the collection name). Legacy `{project}_xxx` names are detected
/// by `is_legacy_collection`.
pub fn collection_name(source: &str, _project: &str) -> &'static str {
    match source {
        "methods" | "code" => CODE_METHODS,
        "semantic" | "doc" => DOC_CHUNKS,
        "knowledge" | "kg" => KG_NODES,
        _ => CODE_METHODS, // fallback
    }
}

/// Check if a collection name is a legacy `{project}_xxx` format.
pub fn is_legacy_collection(name: &str) -> bool {
    name.ends_with("_methods") && name != CODE_METHODS
        || name.ends_with("_semantic") && name != DOC_CHUNKS
        || name.ends_with("_knowledge") && name != KG_NODES
        || name.ends_with("_entities")
}

/// Check if a collection name is a new global collection.
pub fn is_global_collection(name: &str) -> bool {
    name == CODE_METHODS || name == DOC_CHUNKS || name == KG_NODES
}

/// Get the entity type from a collection name (for search result display).
pub fn entity_type_from_collection(col: &str) -> &'static str {
    if col == CODE_METHODS || col.ends_with("_methods") {
        "Method"
    } else if col == DOC_CHUNKS || col.ends_with("_semantic") {
        "Doc"
    } else if col == KG_NODES || col.ends_with("_knowledge") {
        "Knowledge"
    } else if col.ends_with("_entities") {
        "Entity"
    } else {
        "?"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_name_returns_global() {
        assert_eq!(collection_name("methods", "offen-pay"), CODE_METHODS);
        assert_eq!(collection_name("semantic", "offen-pay"), DOC_CHUNKS);
        assert_eq!(collection_name("knowledge", "offen-pay"), KG_NODES);
    }

    #[test]
    fn is_legacy_detection() {
        assert!(is_legacy_collection("offen-pay_methods"));
        assert!(is_legacy_collection("test-pipeline_semantic"));
        assert!(!is_legacy_collection(CODE_METHODS));
        assert!(!is_legacy_collection(DOC_CHUNKS));
        assert!(!is_legacy_collection(KG_NODES));
    }

    #[test]
    fn entity_type_from_various_collections() {
        assert_eq!(entity_type_from_collection(CODE_METHODS), "Method");
        assert_eq!(entity_type_from_collection("offen-pay_methods"), "Method");
        assert_eq!(entity_type_from_collection(DOC_CHUNKS), "Doc");
        assert_eq!(entity_type_from_collection(KG_NODES), "Knowledge");
    }
}
