use std::collections::HashMap;
use serde_json::json;

use crate::models::MethodBlock;
use crate::client::neo4j::MethodNode;
use crate::common::hash::method_id_to_u64;

impl From<&MethodBlock> for MethodNode {
    fn from(m: &MethodBlock) -> Self {
        MethodNode {
            method_id: m.method_id.clone(),
            project: m.project.clone(),
            file_path: m.file_path.clone(),
            language: m.language.clone(),
            package_or_module: m.package_or_module.clone(),
            class_name: m.class_name.clone(),
            name: m.name.clone(),
            signature: m.signature.clone(),
            params: m.params.join(", "),
            return_type: m.return_type.clone(),
            start_line: m.start_line as i64,
            end_line: m.end_line as i64,
            calls: m.calls.clone(),
        }
    }
}

pub fn build_payload(m: &MethodBlock) -> HashMap<String, serde_json::Value> {
    let mut p = HashMap::new();
    p.insert("method_id".into(), json!(m.method_id));
    p.insert("project".into(), json!(m.project));
    p.insert("file_path".into(), json!(m.file_path));
    p.insert("language".into(), json!(m.language));
    p.insert("package_or_module".into(), json!(m.package_or_module));
    p.insert("class_name".into(), json!(m.class_name));
    p.insert("name".into(), json!(m.name));
    p.insert("signature".into(), json!(m.signature));
    p.insert("params".into(), json!(m.params.join(", ")));
    p.insert("return_type".into(), json!(m.return_type));
    p.insert("start_line".into(), json!(m.start_line));
    p.insert("end_line".into(), json!(m.end_line));
    p.insert("comment".into(), json!(m.comment));
    p.insert("search_text".into(), json!(m.search_text));
    p.insert("source_code".into(), json!(m.source_code));
    p.insert("calls".into(), json!(m.calls));
    p
}

pub fn build_qdrant_point(m: &MethodBlock, vector: &[f32]) -> (serde_json::Value, Vec<f32>, HashMap<String, serde_json::Value>) {
    let point_id = json!(method_id_to_u64(&m.method_id));
    (point_id, vector.to_vec(), build_payload(m))
}

pub fn split_class_path(class_name: &str, file_path: &str) -> (String, String) {
    let pkg = std::path::Path::new(file_path).parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace('/', ".")
        .to_string();
    (pkg, class_name.to_string())
}
