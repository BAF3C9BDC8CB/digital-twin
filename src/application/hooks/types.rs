use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单一事件标签的完整配置（从 YAML 反序列化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTypeConfig {
    /// Neo4j 标签名，如 "Modification"
    pub label: String,
    /// 订阅的 hook 名，如 "code_modified"
    pub subscribe: String,
    /// ID 生成配置
    pub id: IdConfig,
    /// 节点的唯一标识属性名，如 "mod_id"
    pub id_field: String,
    /// 属性映射规则列表
    #[serde(default)]
    pub properties: Vec<PropertyConfig>,
    /// 关系配置列表
    #[serde(default)]
    pub relationships: Vec<RelationshipConfig>,
    /// 额外的 Cypher 模板（side effects）
    #[serde(default)]
    pub side_effects: Vec<String>,
    /// 配置段 SHA256 哈希（运行时计算，YAML 中不存）
    /// 用于懒迁移：节点上的 _schema_hash ≠ 当前配置的 hash → 触发迁移
    #[serde(skip)]
    pub schema_hash: String,
    /// 当前配置的属性名列表（副本，避免每次重新收集）
    #[serde(skip)]
    pub property_names: Vec<String>,
    /// 当前配置的关系类型列表（副本，避免每次重新收集）
    #[serde(skip)]
    pub relationship_types: Vec<String>,
}

/// ID 生成策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdConfig {
    /// ID 前缀，如 "mod", "deploy"
    pub prefix: String,
    /// 用于生成 ID 的 context 字段名列表
    pub fields: Vec<String>,
}

/// 属性映射规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyConfig {
    /// 节点属性名
    pub name: String,
    /// 来源："context.xxx" | "id" | "now"
    pub from: String,
    /// 是否必填
    #[serde(default)]
    pub required: bool,
    /// 默认值
    pub default: Option<String>,
}

/// 关系配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipConfig {
    /// 关系类型，如 "AFFECTS", "FIXES", "BELONGS_TO"
    #[serde(rename = "type")]
    pub rel_type: String,
    /// 目标节点标签，如 "Method", "Project"
    pub target_label: String,
    /// 匹配规则
    pub r#match: MatchConfig,
    /// 事件节点的关联属性名（默认用 id_field）
    #[serde(default = "default_source_field")]
    pub source_field: String,
}

fn default_source_field() -> String {
    "event_id".to_string()
}

/// 关系匹配规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchConfig {
    /// context 中的字段名
    pub context_field: String,
    /// 目标节点的属性名
    pub target_field: String,
}

/// Hook 触发时携带的上下文数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    /// 触发此 hook 的名称
    pub hook_name: String,
    /// 所属项目
    pub project: String,
    /// 当前会话 ID
    pub session_id: String,
    /// 实体 ID（如 Jenkins 任务名、Nacos dataId）
    pub entity_id: String,
    /// 实体类型（如 "JenkinsJob"）
    pub entity_type: String,
    /// 任意键值对（hook 点自由填充）
    #[serde(flatten)]
    pub fields: HashMap<String, String>,
}

impl HookContext {
    /// 从 context 取值：优先 fields，回退到命名属性
    pub fn get(&self, key: &str) -> Option<&str> {
        if let Some(v) = self.fields.get(key) {
            return Some(v.as_str());
        }
        match key {
            "project" => Some(&self.project),
            "session_id" => Some(&self.session_id),
            "entity_id" => Some(&self.entity_id),
            "entity_type" => Some(&self.entity_type),
            "hook_name" => Some(&self.hook_name),
            _ => None,
        }
    }
}

/// 单个标签写入结果
#[derive(Debug, Clone)]
pub struct WriteResult {
    pub label: String,
    pub event_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

impl WriteResult {
    pub fn success(label: &str, event_id: &str, elapsed: std::time::Duration) -> Self {
        Self {
            label: label.to_string(),
            event_id: event_id.to_string(),
            success: true,
            error: None,
            elapsed_ms: elapsed.as_millis() as u64,
        }
    }

    pub fn failed(label: &str, event_id: &str, err: impl std::fmt::Display) -> Self {
        Self {
            label: label.to_string(),
            event_id: event_id.to_string(),
            success: false,
            error: Some(err.to_string()),
            elapsed_ms: 0,
        }
    }
}

/// YAML 根配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    pub hooks: HashMap<String, HookDef>,
    pub event_types: Vec<EventTypeConfig>,
}

/// Hook 点定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDef {
    pub description: String,
}
