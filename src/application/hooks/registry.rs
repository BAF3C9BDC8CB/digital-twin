use crate::application::hooks::types::{EventTypeConfig, HookConfig};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

pub struct HookRegistry {
    subscribers: HashMap<String, Vec<EventTypeConfig>>,
    config: HookConfig,
}

/// 计算 event_type 配置段的确定性 SHA256 哈希。
///
/// 参与哈希计算的内容（这些变化会触发懒迁移）：
///   - label, id_field（节点结构）
///   - properties[].name, properties[].required, properties[].default（属性结构）
///   - relationships[].rel_type, .target_label, .match（关系结构）
///
/// 不参与哈希的内容（这些变化不影响已有节点）：
///   - subscribe（hook 绑定变化不改变数据形态）
///   - side_effects（业务逻辑变化，不改变节点属性/关系结构）
///   - id 配置（ID 变化只影响新节点，旧节点不受影响）
///   - properties[].from（属性来源变化不影响已存属性名）
fn compute_schema_hash(cfg: &EventTypeConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cfg.label.as_bytes());
    hasher.update(b"\x00");
    hasher.update(cfg.id_field.as_bytes());
    hasher.update(b"\x00");

    // 按 name 排序确保确定性
    let mut prop_names: Vec<&str> = cfg.properties.iter().map(|p| p.name.as_str()).collect();
    prop_names.sort_unstable();
    for name in &prop_names {
        hasher.update(name.as_bytes());
        hasher.update(b"\x00");
    }

    // 属性配置也参与哈希（required/default 变化 → 约束变化 → 需要迁移）
    for p in &cfg.properties {
        hasher.update(p.required.to_string().as_bytes());
        hasher.update(b"\x00");
        if let Some(d) = &p.default {
            hasher.update(d.as_bytes());
            hasher.update(b"\x00");
        }
    }

    // 关系配置：按 (type, label) 排序确保确定性
    let mut rel_pairs: Vec<(&str, &str)> = cfg
        .relationships
        .iter()
        .map(|r| (r.rel_type.as_str(), r.target_label.as_str()))
        .collect();
    rel_pairs.sort_unstable();
    for (rt, tl) in &rel_pairs {
        hasher.update(rt.as_bytes());
        hasher.update(b"\x00");
        hasher.update(tl.as_bytes());
        hasher.update(b"\x00");
    }

    format!("sch_{}", hex::encode(&hasher.finalize()[..8]))
}

impl HookRegistry {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let mut config: HookConfig = serde_yaml::from_str(&content)?;

        let mut subscribers: HashMap<String, Vec<EventTypeConfig>> = HashMap::new();

        // 取得 event_types 的所有权，逐个修改后再重建 config
        let event_types = std::mem::take(&mut config.event_types);
        let mut processed: Vec<EventTypeConfig> = Vec::with_capacity(event_types.len());

        for mut et in event_types {
            et.schema_hash = compute_schema_hash(&et);
            et.property_names = et.properties.iter().map(|p| p.name.clone()).collect();
            et.relationship_types = et
                .relationships
                .iter()
                .map(|r| r.rel_type.clone())
                .collect();

            subscribers
                .entry(et.subscribe.clone())
                .or_default()
                .push(et.clone());

            processed.push(et);
        }

        config.event_types = processed;

        Ok(Self {
            subscribers,
            config,
        })
    }

    pub fn subscribers(&self, hook_name: &str) -> &[EventTypeConfig] {
        self.subscribers
            .get(hook_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn hook_names(&self) -> impl Iterator<Item = &str> {
        self.subscribers.keys().map(|s| s.as_str())
    }

    pub fn reload(&mut self, path: impl AsRef<Path>) -> Result<(), Box<dyn std::error::Error>> {
        let new = Self::from_file(path)?;
        self.subscribers = new.subscribers;
        self.config = new.config;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_from_yaml_parses_correctly() {
        let yaml = r#"
hooks:
  code_modified:
    description: "Code changed"

event_types:
  - label: Modification
    subscribe: code_modified
    id:
      prefix: mod
      fields: [entity_id, details]
    id_field: mod_id
    properties:
      - name: file
        from: context.file
        required: true
"#;

        let config: HookConfig = serde_yaml::from_str(yaml).expect("解析 yaml");
        assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.event_types.len(), 1);
        assert_eq!(config.event_types[0].label, "Modification");
    }

    #[test]
    fn registry_subscribers_returns_correct_entries() {
        let yaml = r#"
hooks:
  code_modified:
    description: ""
  deploy_done:
    description: ""

event_types:
  - label: Modification
    subscribe: code_modified
    id: { prefix: mod, fields: [entity_id] }
    id_field: mod_id

  - label: Deployment
    subscribe: deploy_done
    id: { prefix: deploy, fields: [entity_id] }
    id_field: deploy_id

  - label: AuditLog
    subscribe: deploy_done
    id: { prefix: audit, fields: [entity_id] }
    id_field: log_id
"#;

        let config: HookConfig = serde_yaml::from_str(yaml).expect("解析");
        let mut subscribers: HashMap<String, Vec<EventTypeConfig>> = HashMap::new();
        for et in &config.event_types {
            subscribers
                .entry(et.subscribe.clone())
                .or_default()
                .push(et.clone());
        }

        assert_eq!(subscribers.get("code_modified").unwrap().len(), 1);
        assert_eq!(subscribers.get("deploy_done").unwrap().len(), 2);
    }
}
