//! Qdrant collection 命名约定。
//!
//! 阶段 5+6：全局 collection，项目作为 payload 标签。
//! 迁移期间保留旧的 `{project}_xxx` collection 以兼容旧数据——
//! 搜索时同时查询两者并通过 RRF 融合。

/// 代码方法向量的全局 collection。
pub const CODE_METHODS: &str = "code_methods";

/// 代码类（Class）描述向量的全局 collection。
/// Phase 2.6 类描述补偿成功后写入（description 文本向量），
/// 使 `dt search --world code` 能检索到类实体（此前类只有 Memgraph 节点）。
pub const CODE_CLASSES: &str = "code_classes";

/// 文档分块向量的全局 collection。
pub const DOC_CHUNKS: &str = "doc_chunks";

/// KG 业务节点向量的全局 collection。
pub const KG_NODES: &str = "kg_nodes";

/// 配置分块向量的全局 collection（dt sync --config-chunks 写入）。
pub const CONFIG_CHUNKS: &str = "config_chunks";

/// 向量维度（BGE-M3 = 1024）。
pub const VECTOR_DIM: u32 = 1024;

/// named vectors 中 base 向量的名称（确定性召回向量：embed(signature+comment)）。
pub const VECTOR_NAME_BASE: &str = "base";

/// named vectors 中 llm 向量的名称（LLM 分析文本的 rerank 向量）。
/// 仅当 LLM 分析成功（llm_status=success）后才写入，缺失表示无 llm 向量。
pub const VECTOR_NAME_LLM: &str = "llm";

/// 为给定的源类型解析 collection 名称。
///
/// 阶段 5+6：返回全局 collection 名称（项目是 payload 标签，
/// 不属于 collection 名称的一部分）。旧的 `{project}_xxx` 名称由
/// `is_legacy_collection` 检测。
pub fn collection_name(source: &str, _project: &str) -> &'static str {
    match source {
        "methods" | "code" => CODE_METHODS,
        "semantic" | "doc" => DOC_CHUNKS,
        "knowledge" | "kg" => KG_NODES,
        _ => CODE_METHODS, // 兜底
    }
}

/// 检查 collection 名称是否为旧的 `{project}_xxx` 格式。
pub fn is_legacy_collection(name: &str) -> bool {
    name.ends_with("_methods") && name != CODE_METHODS
        || name.ends_with("_semantic") && name != DOC_CHUNKS
        || name.ends_with("_knowledge") && name != KG_NODES
        || name.ends_with("_entities")
}

/// 检查 collection 名称是否为新的全局 collection。
pub fn is_global_collection(name: &str) -> bool {
    name == CODE_METHODS || name == DOC_CHUNKS || name == KG_NODES
}

/// 从 collection 名称获取实体类型（用于搜索结果展示）。
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
