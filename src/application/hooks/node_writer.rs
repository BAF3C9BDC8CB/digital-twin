use std::collections::HashMap;
use std::sync::Arc;
use serde_json::Value;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

/// 通用节点写入器
///
/// 所有标签共用同一个方法，通过配置决定写什么。
/// 生成的 Cypher 模式：
/// ```cypher
/// MERGE (e:{label} {{id_field}: $event_id})
/// SET e._label = $p__label, e._created_at = $p__created_at
/// SET e.{prop1} = $p_{prop1}, e.{prop2} = $p_{prop2}
/// ```
pub struct NodeWriter {
    graph: Arc<dyn GraphRepository>,
}

impl NodeWriter {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    pub async fn write(
        &self,
        label: &str,
        id_field: &str,
        event_id: &str,
        props: &HashMap<String, Value>,
    ) -> Result<(), DtError> {
        let mut cypher = format!(
            "MERGE (e:{} {{ {}: $event_id }})\n",
            label, id_field
        );

        let mut params: HashMap<String, Value> = HashMap::new();
        params.insert("event_id".into(), Value::String(event_id.into()));

        for (key, value) in props {
            if value.is_null() {
                continue;
            }
            let param_name = format!("p_{}", key.replace('.', "_"));
            cypher.push_str(&format!("SET e.{} = ${}\n", key, param_name));
            params.insert(param_name, value.clone());
        }

        self.graph.write_query(&cypher, params).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            &self,
            _query: &str,
            _params: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            Ok(Value::Null)
        }

        async fn write_query(
            &self,
            query: &str,
            _params: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            self.queries.lock().unwrap().push(query.to_string());
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn write_merges_with_correct_label() {
        let repo = Arc::new(MockRepo::new());
        let writer = NodeWriter::new(repo.clone());

        let mut props = HashMap::new();
        props.insert("job".into(), Value::String("my-app".into()));

        writer.write("Deployment", "deploy_id", "evt-1", &props).await.unwrap();

        let queries = repo.queries.lock().unwrap();
        let q = &queries[0];
        assert!(q.contains("MERGE (e:Deployment { deploy_id: $event_id })")
            || q.contains("MERGE (e:Deployment {deploy_id: $event_id})"),
            "bad cypher: {q}");
        assert!(q.contains("SET e.job = $p_job"));
    }

    #[tokio::test]
    async fn write_sets_meta_properties() {
        let repo = Arc::new(MockRepo::new());
        let writer = NodeWriter::new(repo.clone());

        let mut props = HashMap::new();
        props.insert("_label".into(), Value::String("test-hook".into()));
        props.insert("_created_at".into(), Value::String("2026-01-01T00:00:00Z".into()));

        writer.write("BugFix", "fix_id", "evt-2", &props).await.unwrap();

        let queries = repo.queries.lock().unwrap();
        let q = &queries[0];
        assert!(q.contains("SET e._label = $p__label"));
        assert!(q.contains("SET e._created_at = $p__created_at"));
    }

    #[tokio::test]
    async fn write_skips_null_props() {
        let repo = Arc::new(MockRepo::new());
        let writer = NodeWriter::new(repo.clone());

        let mut props = HashMap::new();
        props.insert("job".into(), Value::String("my-app".into()));
        props.insert("branch".into(), Value::Null);

        writer.write("Test", "id", "evt-3", &props).await.unwrap();

        let queries = repo.queries.lock().unwrap();
        let q = &queries[0];
        assert!(q.contains("$p_job"), "should include 'job'");
        assert!(!q.contains("$p_branch"), "should skip null 'branch'");
    }
}
