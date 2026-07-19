use std::collections::HashMap;
use std::sync::Arc;
use serde_json::Value;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use super::types::{HookContext, RelationshipConfig};

/// 通用关系写入器
///
/// 生成的 Cypher 模式：
/// ```cypher
/// MATCH (e {{source_field}: $event_id})
/// MATCH (t:{target_label} {{target_field}: $target_id})
/// MERGE (e)-[:{rel_type}]->(t)
/// ```
pub struct RelationshipWriter {
    graph: Arc<dyn GraphRepository>,
}

impl RelationshipWriter {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    pub async fn write(
        &self,
        rels: &[RelationshipConfig],
        ctx: &HookContext,
        event_id: &str,
    ) -> Result<(), DtError> {
        for rel in rels {
            let target_id = match ctx.get(&rel.r#match.context_field) {
                Some(v) if !v.is_empty() => v.to_string(),
                _ => continue,
            };

            let cypher = format!(
                "MATCH (e {{ {}: $event_id }})
                 MATCH (t:{} {{ {}: $target_id }})
                 MERGE (e)-[:{}]->(t)",
                rel.source_field,
                rel.target_label,
                rel.r#match.target_field,
                rel.rel_type,
            );

            let mut params: HashMap<String, Value> = HashMap::new();
            params.insert("event_id".into(), Value::String(event_id.into()));
            params.insert("target_id".into(), Value::String(target_id));

            self.graph.write_query(&cypher, params).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::hooks::types::MatchConfig;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockRepo {
        queries: Arc<std::sync::Mutex<Vec<String>>>,
        write_count: Arc<AtomicUsize>,
    }

    impl MockRepo {
        fn new() -> Self {
            Self {
                queries: Arc::new(std::sync::Mutex::new(Vec::new())),
                write_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl GraphRepository for MockRepo {
        async fn read_query(
            &self, _query: &str, _params: HashMap<String, Value>,
        ) -> Result<Value, DtError> { Ok(Value::Null) }

        async fn write_query(
            &self, query: &str, _params: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            self.queries.lock().unwrap().push(query.to_string());
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }

    #[tokio::test]
    async fn write_creates_relationship() {
        let repo = Arc::new(MockRepo::new());
        let writer = RelationshipWriter::new(repo.clone());

        let rels = vec![RelationshipConfig {
            rel_type: "AFFECTS".into(),
            target_label: "Method".into(),
            source_field: "mod_id".into(),
            r#match: MatchConfig {
                context_field: "entity_id".into(),
                target_field: "method_id".into(),
            },
        }];

        let mut fields = HashMap::new();
        fields.insert("entity_id".into(), "method-123".into());
        let ctx = HookContext {
            hook_name: "code_modified".into(),
            project: "test".into(),
            session_id: "s1".into(),
            entity_id: "method-123".into(),
            entity_type: "Method".into(),
            fields,
        };

        writer.write(&rels, &ctx, "evt-1").await.unwrap();

        let queries = repo.queries.lock().unwrap();
        let q = &queries[0];
        assert!(q.contains("MERGE (e)-[:AFFECTS]->(t)"));
        assert!(q.contains("MATCH (t:Method { method_id: $target_id })"));
    }

    #[tokio::test]
    async fn write_skips_when_target_missing() {
        let repo = Arc::new(MockRepo::new());
        let writer = RelationshipWriter::new(repo.clone());

        let rels = vec![RelationshipConfig {
            rel_type: "AFFECTS".into(),
            target_label: "Method".into(),
            source_field: "mod_id".into(),
            r#match: MatchConfig {
                context_field: "nonexistent".into(),
                target_field: "method_id".into(),
            },
        }];

        let ctx = HookContext {
            hook_name: "test".into(),
            project: "test".into(),
            session_id: "s1".into(),
            entity_id: "".into(),
            entity_type: "".into(),
            fields: HashMap::new(),
        };

        writer.write(&rels, &ctx, "evt-1").await.unwrap();

        let queries = repo.queries.lock().unwrap();
        assert!(queries.is_empty(), "should not write when target is missing");
    }
}
