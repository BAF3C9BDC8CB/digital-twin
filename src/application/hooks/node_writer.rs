use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// 通用节点写入器
///
/// 所有标签共用同一个方法，通过配置决定写什么。
/// 生成的 Cypher 模式：
/// ```cypher
/// MERGE (e:{label} {{id_field}: $event_id})
/// SET e.{prop1} = $p_{prop1}, e.{prop2} = $p_{prop2}
/// SET e._schema_hash = $schema_hash
/// ```
pub struct NodeWriter {
    graph: Arc<dyn GraphRepository>,
}

/// 从节点读取到的 schema 状态
pub struct SchemaState {
    /// 节点上存的 _schema_hash，None = 新节点或旧节点无此属性
    pub hash: Option<String>,
    /// 节点上存的 _schema_props（JSON 数组），空列表 = 旧节点
    pub props: Vec<String>,
}

impl NodeWriter {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    /// 基本写入：MERGE + SET（不处理迁移）
    pub async fn write(
        &self,
        label: &str,
        id_field: &str,
        event_id: &str,
        props: &HashMap<String, Value>,
    ) -> Result<(), DtError> {
        let mut cypher = format!("MERGE (e:{} {{ {}: $event_id }})\n", label, id_field);

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

    /// 读取节点现有的 schema 状态（_schema_hash + _schema_props）
    ///
    /// 返回 `None` 表示节点不存在（新节点，无需迁移）。
    /// 返回 `Some(state)` 对比 cfg.schema_hash 判断是否需要迁移。
    pub async fn read_schema_state(
        &self,
        label: &str,
        id_field: &str,
        event_id: &str,
    ) -> Result<Option<SchemaState>, DtError> {
        let cypher = format!(
            "MATCH (e:{} {{ {}: $event_id }})
             RETURN e._schema_hash AS hash, e._schema_props AS props",
            label, id_field,
        );

        let mut params: HashMap<String, Value> = HashMap::new();
        params.insert("event_id".into(), Value::String(event_id.into()));

        let result = self.graph.read_query(&cypher, params).await?;

        // 解析返回结果
        if let Some(row) = result.as_array().and_then(|arr| arr.first()) {
            let hash = row
                .get("hash")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let props: Vec<String> = row
                .get("props")
                .and_then(|v| v.as_str())
                .map(|s| serde_json::from_str(s).unwrap_or_default())
                .unwrap_or_default();
            Ok(Some(SchemaState { hash, props }))
        } else {
            Ok(None) // 节点不存在
        }
    }

    /// 写入节点 + 迁移废弃属性
    ///
    /// 流程：
    /// 1. MERGE 节点
    /// 2. SET 当前配置的所有属性
    /// 3. SET _schema_hash, _schema_props
    /// 4. 如有废弃属性，REMOVE 它们
    ///
    /// 返回 `true` 表示发生了迁移（有属性被 REMOVE）
    pub async fn write_with_migration(
        &self,
        label: &str,
        id_field: &str,
        event_id: &str,
        props: &HashMap<String, Value>,
        old_state: &Option<SchemaState>,
        current_prop_names: &[String],
        schema_hash: &str,
    ) -> Result<bool, DtError> {
        // 计算需要 REMOVE 的废弃属性
        let deprecated: Vec<String> = match old_state {
            Some(state) if state.hash.as_deref() != Some(schema_hash) => {
                // 只在 hash 不一致时才做属性清理
                state
                    .props
                    .iter()
                    .filter(|n| !n.starts_with('_')) // 保留系统属性
                    .filter(|n| !current_prop_names.contains(n))
                    .cloned()
                    .collect()
            }
            _ => vec![],
        };

        let has_migration = !deprecated.is_empty();

        // 构建 Cypher
        let mut cypher = format!("MERGE (e:{} {{ {}: $event_id }})\n", label, id_field,);

        let mut params: HashMap<String, Value> = HashMap::new();
        params.insert("event_id".into(), Value::String(event_id.into()));

        // SET 当前属性
        for (key, value) in props {
            if value.is_null() {
                continue;
            }
            let pn = format!("p_{}", key.replace('.', "_"));
            cypher.push_str(&format!("SET e.{} = ${}\n", key, pn));
            params.insert(pn, value.clone());
        }

        // SET schema 元属性
        cypher.push_str("SET e._schema_hash = $schema_hash\n");
        cypher.push_str("SET e._schema_props = $schema_props\n");
        params.insert("schema_hash".into(), Value::String(schema_hash.into()));
        let props_json = serde_json::to_string(current_prop_names).unwrap_or_default();
        params.insert("schema_props".into(), Value::String(props_json));

        // SET _kg_synced_at = NULL — marks node for re-sync to Qdrant
        // on the next incremental kg-sync (which is triggered immediately
        // after the hook fires).
        cypher.push_str("SET e._kg_synced_at = NULL\n");

        // REMOVE 废弃属性
        if has_migration {
            let removes: Vec<String> = deprecated.iter().map(|p| format!("e.{}", p)).collect();
            cypher.push_str(&format!("REMOVE {}\n", removes.join(", ")));
        }

        self.graph.write_query(&cypher, params).await?;
        Ok(has_migration)
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

        writer
            .write("Deployment", "deploy_id", "evt-1", &props)
            .await
            .unwrap();

        let queries = repo.queries.lock().unwrap();
        let q = &queries[0];
        assert!(
            q.contains("MERGE (e:Deployment { deploy_id: $event_id })")
                || q.contains("MERGE (e:Deployment {deploy_id: $event_id})"),
            "bad cypher: {q}"
        );
        assert!(q.contains("SET e.job = $p_job"));
    }

    #[tokio::test]
    async fn write_sets_meta_properties() {
        let repo = Arc::new(MockRepo::new());
        let writer = NodeWriter::new(repo.clone());

        let mut props = HashMap::new();
        props.insert("_label".into(), Value::String("test-hook".into()));
        props.insert(
            "_created_at".into(),
            Value::String("2026-01-01T00:00:00Z".into()),
        );

        writer
            .write("BugFix", "fix_id", "evt-2", &props)
            .await
            .unwrap();

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
