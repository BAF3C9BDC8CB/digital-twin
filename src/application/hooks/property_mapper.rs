use std::collections::HashMap;
use serde_json::Value;
use super::types::{HookContext, PropertyConfig};

pub struct PropertyMapper;

impl PropertyMapper {
    pub fn map(
        props: &[PropertyConfig],
        ctx: &HookContext,
        event_id: &str,
    ) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        let now = chrono::Utc::now().to_rfc3339();

        map.insert("_label".into(), Value::String(ctx.hook_name.clone()));
        map.insert("_created_at".into(), Value::String(now.clone()));
        map.insert("event_type".into(), Value::String(ctx.hook_name.clone()));

        for prop in props {
            let value = Self::resolve_value(prop, ctx, event_id, &now);

            if value.is_null() && prop.required {
                tracing::warn!(
                    "missing required property '{}' for hook '{}'",
                    prop.name, ctx.hook_name
                );
            }

            if !value.is_null() {
                map.insert(prop.name.clone(), value);
            }
        }

        map
    }

    fn resolve_value(
        prop: &PropertyConfig,
        ctx: &HookContext,
        event_id: &str,
        now: &str,
    ) -> Value {
        match prop.from.as_str() {
            "id" => Value::String(event_id.to_string()),
            "now" => Value::String(now.to_string()),
            s if s.starts_with("context.") => {
                let key = s.trim_start_matches("context.");
                ctx.get(key)
                    .map(|v| Value::String(v.to_string()))
                    .or_else(|| {
                        prop.default
                            .as_ref()
                            .map(|d| Value::String(d.clone()))
                    })
                    .unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::hooks::types::PropertyConfig;
    use std::collections::HashMap;

    fn make_ctx(entity_id: &str) -> HookContext {
        let mut fields = HashMap::new();
        fields.insert("job".to_string(), "my-app".to_string());
        fields.insert("env".to_string(), "prod".to_string());
        HookContext {
            hook_name: "jenkins_deploy_completed".into(),
            project: "digital-twin".into(),
            session_id: "2026-07-19-001".into(),
            entity_id: entity_id.into(),
            entity_type: "JenkinsJob".into(),
            fields,
        }
    }

    #[test]
    fn map_id_property() {
        let cfg = vec![PropertyConfig {
            name: "deploy_id".into(),
            from: "id".into(),
            required: true,
            default: None,
        }];
        let ctx = make_ctx("my-app");
        let props = PropertyMapper::map(&cfg, &ctx, "my-event-id");

        assert_eq!(props.get("deploy_id").unwrap(), &Value::String("my-event-id".into()));
    }

    #[test]
    fn map_context_property() {
        let cfg = vec![PropertyConfig {
            name: "job".into(),
            from: "context.job".into(),
            required: true,
            default: None,
        }];
        let ctx = make_ctx("my-app");
        let props = PropertyMapper::map(&cfg, &ctx, "id");

        assert_eq!(props.get("job").unwrap(), &Value::String("my-app".into()));
    }

    #[test]
    fn map_context_property_with_default() {
        let cfg = vec![PropertyConfig {
            name: "branch".into(),
            from: "context.branch".into(),
            required: false,
            default: Some("main".into()),
        }];
        let ctx = make_ctx("my-app");
        let props = PropertyMapper::map(&cfg, &ctx, "id");

        assert_eq!(props.get("branch").unwrap(), &Value::String("main".into()));
    }

    #[test]
    fn map_missing_required_logs_warning() {
        let cfg = vec![PropertyConfig {
            name: "required_field".into(),
            from: "context.nonexistent".into(),
            required: true,
            default: None,
        }];
        let ctx = make_ctx("my-app");
        let props = PropertyMapper::map(&cfg, &ctx, "id");

        assert!(!props.contains_key("required_field"));
    }

    #[test]
    fn map_includes_meta_properties() {
        let ctx = make_ctx("my-app");
        let props = PropertyMapper::map(&[], &ctx, "id");

        assert_eq!(props.get("_label").unwrap(), &Value::String("jenkins_deploy_completed".into()));
        assert!(props.contains_key("_created_at"));
        assert_eq!(props.get("event_type").unwrap(), &Value::String("jenkins_deploy_completed".into()));
    }
}
