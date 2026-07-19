use std::collections::HashMap;
use std::path::Path;
use crate::application::hooks::types::{EventTypeConfig, HookConfig};

pub struct HookRegistry {
    subscribers: HashMap<String, Vec<EventTypeConfig>>,
    config: HookConfig,
}

impl HookRegistry {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let config: HookConfig = serde_yaml::from_str(&content)?;

        let mut subscribers: HashMap<String, Vec<EventTypeConfig>> = HashMap::new();

        for et in &config.event_types {
            subscribers
                .entry(et.subscribe.clone())
                .or_default()
                .push(et.clone());
        }

        Ok(Self { subscribers, config })
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

        let config: HookConfig = serde_yaml::from_str(yaml).expect("parse yaml");
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

        let config: HookConfig = serde_yaml::from_str(yaml).expect("parse");
        let mut subscribers: HashMap<String, Vec<EventTypeConfig>> = HashMap::new();
        for et in &config.event_types {
            subscribers.entry(et.subscribe.clone()).or_default().push(et.clone());
        }

        assert_eq!(subscribers.get("code_modified").unwrap().len(), 1);
        assert_eq!(subscribers.get("deploy_done").unwrap().len(), 2);
    }
}
