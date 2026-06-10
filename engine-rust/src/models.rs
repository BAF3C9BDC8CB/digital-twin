use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodBlock {
    pub method_id: String, pub project: String, pub file_path: String,
    pub language: String, pub package_or_module: String, pub class_name: String,
    pub name: String, pub signature: String, pub params: Vec<String>,
    pub return_type: String, pub source_code: String, pub search_text: String,
    pub summary: String, pub start_line: usize, pub end_line: usize,
    pub comment: String, pub calls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassBlock {
    pub class_id: String, pub project: String, pub file_path: String,
    pub language: String, pub package_or_module: String, pub name: String,
    pub kind: EntityKind, pub summary: String, pub search_text: String,
    pub start_line: usize, pub end_line: usize,
    pub extends: Vec<String>, pub implements: Vec<String>,
    pub method_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityKind { Class, Interface, Enum, Struct, Trait }
