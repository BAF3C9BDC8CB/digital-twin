use sha2::{Digest, Sha256};

use super::types::{HookContext, IdConfig};

pub struct IdGenerator;

impl IdGenerator {
    pub fn generate(config: &IdConfig, ctx: &HookContext) -> String {
        let mut hasher = Sha256::new();
        hasher.update(config.prefix.as_bytes());

        for field in &config.fields {
            hasher.update(b":");
            let value = ctx.get(field).unwrap_or("");
            hasher.update(value.as_bytes());
        }

        let hash = hex::encode(hasher.finalize());
        format!("dt://event/{}/{}", config.prefix, &hash[..16])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_ctx(entity_id: &str, details: &str) -> HookContext {
        let mut fields = HashMap::new();
        fields.insert("details".to_string(), details.to_string());
        HookContext {
            hook_name: "test".into(),
            project: "test".into(),
            session_id: "2026-07-19-001".into(),
            entity_id: entity_id.into(),
            entity_type: "Test".into(),
            fields,
        }
    }

    #[test]
    fn generate_returns_deterministic_id() {
        let cfg = IdConfig {
            prefix: "deploy".into(),
            fields: vec!["entity_id".into(), "details".into()],
        };
        let ctx = make_ctx("my-job", "branch: main; env: prod");

        let id1 = IdGenerator::generate(&cfg, &ctx);
        let id2 = IdGenerator::generate(&cfg, &ctx);

        assert_eq!(id1, id2, "相同输入必须产生相同 ID");
    }

    #[test]
    fn generate_returns_unique_id_for_different_input() {
        let cfg = IdConfig {
            prefix: "deploy".into(),
            fields: vec!["entity_id".into()],
        };
        let ctx_a = make_ctx("job-a", "details");
        let ctx_b = make_ctx("job-b", "details");

        let id_a = IdGenerator::generate(&cfg, &ctx_a);
        let id_b = IdGenerator::generate(&cfg, &ctx_b);

        assert_ne!(id_a, id_b, "不同的 entity_id 必须产生不同的 ID");
    }

    #[test]
    fn generate_formats_correctly() {
        let cfg = IdConfig {
            prefix: "fix".into(),
            fields: vec!["entity_id".into()],
        };
        let ctx = make_ctx("BUG-123", "root cause: null pointer");

        let id = IdGenerator::generate(&cfg, &ctx);

        assert!(id.starts_with("dt://event/fix/"), "错误的前缀: {id}");
        assert_eq!(id.len(), "dt://event/fix/".len() + 16, "错误的哈希长度");
    }

    #[test]
    fn generate_handles_missing_field_gracefully() {
        let cfg = IdConfig {
            prefix: "test".into(),
            fields: vec!["nonexistent".into()],
        };
        let ctx = make_ctx("x", "y");

        let id = IdGenerator::generate(&cfg, &ctx);

        assert!(id.starts_with("dt://event/test/"));
        let id2 = IdGenerator::generate(&cfg, &ctx);
        assert_eq!(id, id2);
    }
}
