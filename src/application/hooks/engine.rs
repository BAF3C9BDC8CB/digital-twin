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
    async fn execute_one(&self, cfg: &EventTypeConfig, ctx: &HookContext) -> WriteResult {
        let timer = Instant::now();

        // 1. 生成事件 ID（纯函数）
        let event_id = IdGenerator::generate(&cfg.id, ctx);

        // 2. 映射属性（纯函数）
        let props = PropertyMapper::map(&cfg.properties, ctx, &event_id);

        // 3. 写入节点
        if let Err(e) = self.node_writer
            .write(&cfg.label, &cfg.id_field, &event_id, &props)
            .await
        {
            return WriteResult::failed(&cfg.label, &event_id, e);
        }

        // 4. 创建关系
        if let Err(e) = self.rel_writer
            .write(&cfg.relationships, ctx, &event_id)
            .await
        {
            return WriteResult::failed(&cfg.label, &event_id, e);
        }

        // 5. 执行 side effects
        if !cfg.side_effects.is_empty() {
            let vars = self.build_side_effect_vars(ctx, &event_id);
            if let Err(e) = self.side_effector.run(&cfg.side_effects, &vars).await {
                return WriteResult::failed(&cfg.label, &event_id, e);
            }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockRepo(Arc<AtomicUsize>);

    #[async_trait]
    impl crate::domain::traits::GraphRepository for MockRepo {
        async fn read_query(
            &self,
            _q: &str,
            _p: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            Ok(Value::Null)
        }

        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            self.0.fetch_add(1, Ordering::SeqCst);
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
        let counter = Arc::new(AtomicUsize::new(0));
        let graph = Arc::new(MockRepo(counter.clone()));

        let engine = HookEngine::new(registry, graph);

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
        assert!(counter.load(Ordering::SeqCst) > 0);

        std::fs::remove_file(&path).ok();
    }
}
