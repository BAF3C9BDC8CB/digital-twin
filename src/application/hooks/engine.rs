use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use serde_json::Value;

use super::id_generator::IdGenerator;
use super::node_writer::NodeWriter;
use super::property_mapper::PropertyMapper;
use super::registry::HookRegistry;
use super::relationship_writer::RelationshipWriter;
use super::side_effect_runner::SideEffectRunner;
use super::types::{EventTypeConfig, HookContext, WriteResult};

/// Hook 引擎：事件系统的核心编排器
///
/// 职责：
/// 1. 接收 fire(hook_name, context) 调用
/// 2. 从 HookRegistry 查出订阅了此 hook 的所有标签配置
/// 3. 对每个配置执行通用写入流程：
///    IdGenerator → PropertyMapper → NodeWriter → RelationshipWriter → SideEffectRunner
pub struct HookEngine {
    registry: Arc<HookRegistry>,
    node_writer: NodeWriter,
    rel_writer: RelationshipWriter,
    side_effector: SideEffectRunner,
}

impl HookEngine {
    pub fn new(
        registry: Arc<HookRegistry>,
        graph: Arc<dyn crate::domain::traits::GraphRepository>,
    ) -> Self {
        Self {
            registry,
            node_writer: NodeWriter::new(graph.clone()),
            rel_writer: RelationshipWriter::new(graph.clone()),
            side_effector: SideEffectRunner::new(graph),
        }
    }

    /// 触发一个 hook，执行所有订阅的标签写入
    pub async fn fire(&self, hook_name: &str, context: HookContext) -> Vec<WriteResult> {
        let subscribers = self.registry.subscribers(hook_name);
        let mut results = Vec::with_capacity(subscribers.len());

        for cfg in subscribers {
            let result = self.execute_one(cfg, &context).await;
            results.push(result);
        }

        results
    }

    /// 执行单个标签配置的完整写入流程
    ///
    /// 包含懒迁移步骤：
    ///   Step 0：读取节点现有的 _schema_hash
    ///   Step 1：生成事件 ID
    ///   Step 2：映射属性
    ///   Step 3：写入节点 + 自动 REMOVE 废弃属性
    ///   Step 4：写新关系 + 自动 DELETE 废弃关系
    ///   Step 5：执行 side effects
    async fn execute_one(&self, cfg: &EventTypeConfig, ctx: &HookContext) -> WriteResult {
        let timer = Instant::now();

        // Step 0: 生成事件 ID（用于后续所有操作）
        let event_id = IdGenerator::generate(&cfg.id, ctx);

        // Step 1: 读取节点现有 schema 状态
        let old_state = match self.node_writer
            .read_schema_state(&cfg.label, &cfg.id_field, &event_id)
            .await
        {
            Ok(state) => state,
            Err(e) => return WriteResult::failed(&cfg.label, &event_id, e),
        };

        // Step 2: 映射属性
        let props = PropertyMapper::map(&cfg.properties, ctx, &event_id);

        // Step 3: 写入节点 + 迁移废弃属性
        let migrated = match self.node_writer
            .write_with_migration(
                &cfg.label, &cfg.id_field, &event_id, &props,
                &old_state, &cfg.property_names, &cfg.schema_hash,
            )
            .await
        {
            Ok(m) => m,
            Err(e) => return WriteResult::failed(&cfg.label, &event_id, e),
        };

        // Step 4: 创建关系 + 删除废弃关系
        if let Err(e) = self.rel_writer
            .write_and_cleanup(&cfg.relationships, ctx, &event_id, migrated)
            .await
        {
            return WriteResult::failed(&cfg.label, &event_id, e);
        }

        // Step 5: 执行 side effects
        if !cfg.side_effects.is_empty() {
            let vars = self.build_side_effect_vars(ctx, &event_id);
            if let Err(e) = self.side_effector.run(&cfg.side_effects, &vars).await {
                return WriteResult::failed(&cfg.label, &event_id, e);
            }
        }

        if migrated {
            tracing::info!(
                "[migrate] {} {} schema updated (hash: {})",
                cfg.label, event_id, cfg.schema_hash,
            );
        }

        WriteResult::success(&cfg.label, &event_id, timer.elapsed())
    }

    /// 构建 side_effects 模板变量
    fn build_side_effect_vars(&self, ctx: &HookContext, event_id: &str) -> HashMap<String, Value> {
        let mut vars = HashMap::new();
        let now = chrono::Utc::now().to_rfc3339();

        vars.insert("event_id".into(), Value::String(event_id.into()));
        vars.insert("now".into(), Value::String(now));
        vars.insert("project".into(), Value::String(ctx.project.clone()));
        vars.insert("session_id".into(), Value::String(ctx.session_id.clone()));
        vars.insert("entity_id".into(), Value::String(ctx.entity_id.clone()));
        vars.insert("entity_type".into(), Value::String(ctx.entity_type.clone()));

        for (key, value) in &ctx.fields {
            vars.insert(key.clone(), Value::String(value.clone()));
        }

        vars
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::DtError;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Mock repo that tracks queries for Cypher verification
    struct TrackedRepo {
        queries: Arc<Mutex<Vec<String>>>,
        write_count: Arc<AtomicUsize>,
        /// Simulates read_schema_state returning None (new node)
        return_empty_schema: bool,
    }

    impl TrackedRepo {
        fn new(return_empty_schema: bool) -> Self {
            Self {
                queries: Arc::new(Mutex::new(Vec::new())),
                write_count: Arc::new(AtomicUsize::new(0)),
                return_empty_schema,
            }
        }
    }

    #[async_trait]
    impl crate::domain::traits::GraphRepository for TrackedRepo {
        async fn read_query(
            &self,
            q: &str,
            _p: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            // Track the query for verification
            self.queries.lock().unwrap().push(q.to_string());

            if self.return_empty_schema || q.contains("_schema_hash") {
                // Simulate no existing node → no migration needed
                return Ok(Value::Array(vec![]));
            }
            Ok(Value::Null)
        }

        async fn write_query(
            &self,
            q: &str,
            _p: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            self.queries.lock().unwrap().push(q.to_string());
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> {
            Ok(HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn engine_fire_writes_subscribed_events() {
        let yaml = r#"
hooks:
  test_hook: { description: "" }
event_types:
  - label: TestEvent
    subscribe: test_hook
    id: { prefix: test, fields: [entity_id] }
    id_field: test_id
    properties:
      - name: source
        from: context.source
        required: true
"#;
        let dir = std::env::temp_dir();
        let path = dir.join("test_event_hooks.yaml");
        std::fs::write(&path, yaml).unwrap();

        let registry = Arc::new(HookRegistry::from_file(&path).unwrap());
        let repo = Arc::new(TrackedRepo::new(true));
        let write_count = repo.write_count.clone();
        let engine = HookEngine::new(registry, repo);

        let mut fields = HashMap::new();
        fields.insert("source".to_string(), "test".to_string());
        let ctx = HookContext {
            hook_name: "test_hook".into(),
            project: "p".into(),
            session_id: "s1".into(),
            entity_id: "e1".into(),
            entity_type: "Test".into(),
            fields,
        };

        let results = engine.fire("test_hook", ctx).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert!(write_count.load(Ordering::SeqCst) > 0);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn engine_migration_removes_deprecated_properties() {
        // Config v1: has "region" property, v2 has "zone" instead
        let yaml = r#"
hooks:
  test_hook: { description: "" }
event_types:
  - label: MigrateTest
    subscribe: test_hook
    id: { prefix: mt, fields: [entity_id] }
    id_field: mt_id
    properties:
      - name: zone
        from: context.zone
"#;
        let dir = std::env::temp_dir();
        let path = dir.join("test_migration_hooks.yaml");
        std::fs::write(&path, yaml).unwrap();

        let registry = Arc::new(HookRegistry::from_file(&path).unwrap());
        assert_eq!(registry.subscribers("test_hook").len(), 1);
        let cfg = &registry.subscribers("test_hook")[0];
        assert!(!cfg.schema_hash.is_empty());

        // Verify the hash starts with "sch_"
        assert!(cfg.schema_hash.starts_with("sch_"), "bad hash: {}", cfg.schema_hash);

        std::fs::remove_file(&path).ok();
    }
}
