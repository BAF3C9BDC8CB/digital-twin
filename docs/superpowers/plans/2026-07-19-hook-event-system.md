# Hook 驱动的事件标签系统 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 7 个硬编码事件 handler 替换为统一的、配置驱动的 Hook 系统，新增事件类型只需改 YAML 配置。

**Architecture:** HookEngine（Template Method 编排器）组合 IdGenerator（纯函数 ID 生成）、PropertyMapper（纯函数属性映射）、NodeWriter（通用 MERGE 节点写入）、RelationshipWriter（通用关系创建）、SideEffectRunner（Cypher 模板逃生舱）五个组件，所有组件由 HookRegistry 从 `event-hooks.yaml` 加载的配置驱动。

**Tech Stack:** Rust, Neo4j (Bolt via GraphRepository trait), Serde, SHA256

---

## 全局约束

- 所有新组件放在 `src/application/hooks/` 目录下
- 每个文件 ≤ 100 行，单一职责
- 纯函数组件（IdGenerator, PropertyMapper）不依赖 Neo4j，用普通 `#[test]` 测试
- 集成组件（NodeWriter, RelationshipWriter, SideEffectRunner, HookEngine）用 mock GraphRepository 测试
- 不删除既有功能，只替换实现方式——迁移期间新旧系统可共存
- `event-hooks.yaml` 放在项目根目录 `config/` 下

## 文件结构

```
创建:
  src/application/hooks/mod.rs                  # 模块声明 + 重新导出
  src/application/hooks/types.rs                # 配置结构体 + HookContext
  src/application/hooks/id_generator.rs         # IdGenerator
  src/application/hooks/property_mapper.rs      # PropertyMapper
  src/application/hooks/registry.rs             # HookRegistry (YAML 加载)
  src/application/hooks/node_writer.rs          # NodeWriter
  src/application/hooks/relationship_writer.rs  # RelationshipWriter
  src/application/hooks/side_effect_runner.rs   # SideEffectRunner
  src/application/hooks/engine.rs               # HookEngine (编排器)
  config/event-hooks.yaml                       # 事件标签配置

修改:
  src/application/knowledge/memory/service.rs   # record_event 改为调用 HookEngine
  src/application/knowledge/memory/entities.rs  # EventType 枚举简化为字符串
  src/application/knowledge/memory/mod.rs       # 移除 dispatcher 注册
  src/interfaces/cli/event.rs                   # dt event 改为 --hook 参数
  src/application/build/watcher.rs              # 文件变更时 fire("code_modified")
  src/main.rs                                   # 注册 HookEngine 到 AppState

删除:
  src/application/knowledge/memory/dispatcher.rs
  src/application/knowledge/memory/handlers/    (整个目录)
```

---

### Task 1: Core 类型定义

**Files:**
- Create: `src/application/hooks/mod.rs`
- Create: `src/application/hooks/types.rs`

**Interfaces:**
- Produces: `EventTypeConfig`, `IdConfig`, `PropertyConfig`, `RelationshipConfig`, `HookContext`, `WriteResult`

- [ ] **Step 1: 创建模块目录和 mod.rs**

```bash
mkdir -p src/application/hooks
```

- [ ] **Step 2: 编写 types.rs**

```rust
// src/application/hooks/types.rs

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
```

- [ ] **Step 3: 编写 mod.rs**

```rust
// src/application/hooks/mod.rs

pub mod types;
pub mod id_generator;
pub mod property_mapper;
pub mod registry;
pub mod node_writer;
pub mod relationship_writer;
pub mod side_effect_runner;
pub mod engine;

pub use engine::HookEngine;
pub use registry::HookRegistry;
pub use types::{
    EventTypeConfig, HookConfig, HookContext, IdConfig, MatchConfig,
    PropertyConfig, RelationshipConfig, WriteResult,
};
```

- [ ] **Step 4: 验证编译**

```bash
cd /data/myProject/digital-twin-v2
cargo check 2>&1 | head -5
```
Expected: 编译成功（可能有未使用变量的 warning）

- [ ] **Step 5: Commit**

```bash
git add src/application/hooks/
git commit -m "feat: add hook system core types"
```

---

### Task 2: IdGenerator（纯函数）

**Files:**
- Create: `src/application/hooks/id_generator.rs`
- Test: 内联单元测试

**Interfaces:**
- Consumes: `IdConfig`, `HookContext`
- Produces: `IdGenerator::generate(&IdConfig, &HookContext) -> String`

- [ ] **Step 1: 编写实现**

```rust
// src/application/hooks/id_generator.rs

use sha2::{Digest, Sha256};
use super::types::{HookContext, IdConfig};

/// 确定性事件 ID 生成器
///
/// 格式: dt://event/{prefix}/{sha256_hex[:16]}
/// 相同输入 → 相同 ID → MERGE 天然去重
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

        assert_eq!(id1, id2, "same input must produce same ID");
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

        assert_ne!(id_a, id_b, "different entity_id must produce different ID");
    }

    #[test]
    fn generate_formats_correctly() {
        let cfg = IdConfig {
            prefix: "fix".into(),
            fields: vec!["entity_id".into()],
        };
        let ctx = make_ctx("BUG-123", "root cause: null pointer");

        let id = IdGenerator::generate(&cfg, &ctx);

        assert!(id.starts_with("dt://event/fix/"), "bad prefix: {id}");
        assert_eq!(id.len(), "dt://event/fix/".len() + 16, "bad hash length");
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
        // Missing field produces empty string → hash is deterministic
        let id2 = IdGenerator::generate(&cfg, &ctx);
        assert_eq!(id, id2);
    }
}
```

- [ ] **Step 2: 运行测试**

```bash
cd /data/myProject/digital-twin-v2
cargo test id_generator -- --nocapture
```
Expected: 4 tests PASS

- [ ] **Step 3: Commit**

```bash
git add src/application/hooks/id_generator.rs
git commit -m "feat: add IdGenerator for deterministic event IDs"
```

---

### Task 3: PropertyMapper（纯函数）

**Files:**
- Create: `src/application/hooks/property_mapper.rs`
- Test: 内联单元测试

**Interfaces:**
- Consumes: `&[PropertyConfig]`, `&HookContext`, `event_id: &str`
- Produces: `HashMap<String, serde_json::Value>`

- [ ] **Step 1: 编写实现**

```rust
// src/application/hooks/property_mapper.rs

use std::collections::HashMap;
use serde_json::Value;
use super::types::{HookContext, PropertyConfig};

/// 属性映射器：将配置 + context → 节点属性 HashMap
pub struct PropertyMapper;

impl PropertyMapper {
    /// 映射属性
    ///
    /// 支持的来源：
    ///   - "id"            → event_id
    ///   - "now"           → RFC 3339 当前时间
    ///   - "context.xxx"   → HookContext 中的字段
    ///
    /// 自动注入元属性：
    ///   - `_label`         = 当前标签名
    ///   - `_created_at`    = 当前时间
    ///   - `event_type`     = "hook_name"
    pub fn map(
        props: &[PropertyConfig],
        ctx: &HookContext,
        event_id: &str,
    ) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        let now = chrono::Utc::now().to_rfc3339();

        // 通用元属性
        map.insert("_label".into(), Value::String(ctx.hook_name.clone()));
        map.insert("_created_at".into(), Value::String(now.clone()));

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
    }
}
```

- [ ] **Step 2: 运行测试**

```bash
cd /data/myProject/digital-twin-v2
cargo test property_mapper -- --nocapture
```
Expected: 5 tests PASS

- [ ] **Step 3: Commit**

```bash
git add src/application/hooks/property_mapper.rs
git commit -m "feat: add PropertyMapper for config-driven property mapping"
```

---

### Task 4: NodeWriter（通用节点写入）

**Files:**
- Create: `src/application/hooks/node_writer.rs`
- Test: 用 mock GraphRepository 验证 Cypher 生成

**Interfaces:**
- Consumes: `Arc<dyn GraphRepository>`
- Produces: `NodeWriter::write(label, id_field, event_id, props) -> Result<()>`

- [ ] **Step 1: 编写实现**

```rust
// src/application/hooks/node_writer.rs

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
```

- [ ] **Step 2: 运行测试**

```bash
cd /data/myProject/digital-twin-v2
cargo test node_writer -- --nocapture
```
Expected: 3 tests PASS

- [ ] **Step 3: Commit**

```bash
git add src/application/hooks/node_writer.rs
git commit -m "feat: add NodeWriter for generic MERGE node writing"
```

---

### Task 5: RelationshipWriter（通用关系写入）

**Files:**
- Create: `src/application/hooks/relationship_writer.rs`
- Test: mock GraphRepository

**Interfaces:**
- Consumes: `Arc<dyn GraphRepository>`
- Produces: `RelationshipWriter::write(&[RelationshipConfig], &HookContext, event_id) -> Result<()>`

- [ ] **Step 1: 编写实现**

```rust
// src/application/hooks/relationship_writer.rs

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
```

- [ ] **Step 2: 运行测试**

```bash
cd /data/myProject/digital-twin-v2
cargo test relationship_writer -- --nocapture
```
Expected: 2 tests PASS

- [ ] **Step 3: Commit**

```bash
git add src/application/hooks/relationship_writer.rs
git commit -m "feat: add RelationshipWriter for generic relationship creation"
```

---

### Task 6: SideEffectRunner + HookRegistry（配置加载）

**Files:**
- Create: `src/application/hooks/side_effect_runner.rs`
- Create: `src/application/hooks/registry.rs`

**Interfaces:**
- Produces: `SideEffectRunner::run(&[String], &HashMap) -> Result<()>`
- Produces: `HookRegistry::from_file(path) -> Result<Self>`, `subscribers(hook_name) -> &[EventTypeConfig]`

- [ ] **Step 1: 编写 SideEffectRunner**

```rust
// src/application/hooks/side_effect_runner.rs

use std::collections::HashMap;
use std::sync::Arc;
use serde_json::Value;
use crate::domain::error::DtError;
use crate::domain::traits::GraphRepository;

/// 逃生舱：执行配置中的原始 Cypher 模板
///
/// 只有 Deployment 这类复杂场景才需要。
/// 模板中的 $xxx 变量已由调用方作为参数传入。
pub struct SideEffectRunner {
    graph: Arc<dyn GraphRepository>,
}

impl SideEffectRunner {
    pub fn new(graph: Arc<dyn GraphRepository>) -> Self {
        Self { graph }
    }

    pub async fn run(
        &self,
        effects: &[String],
        params: &HashMap<String, Value>,
    ) -> Result<(), DtError> {
        for cypher in effects {
            if cypher.trim().is_empty() {
                continue;
            }
            self.graph.write_query(cypher, params.clone()).await?;
        }
        Ok(())
    }
}
```

- [ ] **Step 2: 编写 HookRegistry**

```rust
// src/application/hooks/registry.rs

use std::collections::HashMap;
use std::path::Path;
use crate::application::hooks::types::{EventTypeConfig, HookConfig};

/// Hook 注册表：从 YAML 加载配置并提供订阅查询
pub struct HookRegistry {
    /// 索引: hook_name → [EventTypeConfig]
    subscribers: HashMap<String, Vec<EventTypeConfig>>,
    /// 全量配置（用于 reload）
    config: HookConfig,
}

impl HookRegistry {
    /// 从 YAML 文件加载配置
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

    /// 返回订阅了指定 hook 的所有事件类型配置
    pub fn subscribers(&self, hook_name: &str) -> &[EventTypeConfig] {
        self.subscribers
            .get(hook_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 返回所有 hook 名称列表
    pub fn hook_names(&self) -> impl Iterator<Item = &str> {
        self.subscribers.keys().map(|s| s.as_str())
    }

    /// 重新加载配置（热重载用）
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
```

- [ ] **Step 3: 运行测试**

```bash
cd /data/myProject/digital-twin-v2
cargo test registry -- --nocapture
cargo test side_effect_runner -- --nocapture
```
Expected: 2 tests PASS + 编译成功（SideEffectRunner 暂时无独立测试）

- [ ] **Step 4: Commit**

```bash
git add src/application/hooks/registry.rs src/application/hooks/side_effect_runner.rs
git commit -m "feat: add HookRegistry and SideEffectRunner"
```

---

### Task 7: HookEngine（编排器）

**Files:**
- Create: `src/application/hooks/engine.rs`
- Test: mock GraphRepository 端到端测试

**Interfaces:**
- Consumes: `HookRegistry`, `NodeWriter`, `RelationshipWriter`, `SideEffectRunner`
- Produces: `HookEngine::fire(hook_name, context) -> Result<Vec<WriteResult>>`

- [ ] **Step 1: 编写实现**

```rust
// src/application/hooks/engine.rs

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use serde_json::Value;
use crate::domain::error::DtError;

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
```

- [ ] **Step 2: 编写集成测试（同一文件 #[cfg(test)] 中）**

在 engine.rs 末尾添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::hooks::types::{EventTypeConfig, IdConfig, PropertyConfig};
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockRepo {
        write_count: Arc<AtomicUsize>,
    }

    impl MockRepo {
        fn new() -> Self {
            Self { write_count: Arc::new(AtomicUsize::new(0)) }
        }
    }

    #[async_trait]
    impl crate::domain::traits::GraphRepository for MockRepo {
        async fn read_query(
            &self, _query: &str, _params: HashMap<String, Value>,
        ) -> Result<Value, DtError> { Ok(Value::Null) }

        async fn write_query(
            &self, _query: &str, _params: HashMap<String, Value>,
        ) -> Result<Value, DtError> {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }

        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }

    fn make_registry() -> HookRegistry {
        let yaml = r#"
hooks:
  test_hook:
    description: ""

event_types:
  - label: TestEvent
    subscribe: test_hook
    id:
      prefix: test
      fields: [entity_id]
    id_field: test_id
    properties:
      - name: source
        from: context.source
    relationships:
      - type: BELONGS_TO
        target_label: Project
        source_field: test_id
        match:
          context_field: project
          target_field: name
"#;
        let config: crate::application::hooks::types::HookConfig =
            serde_yaml::from_str(yaml).expect("parse");
        let mut subscribers: HashMap<String, Vec<EventTypeConfig>> = HashMap::new();
        for et in &config.event_types {
            subscribers.entry(et.subscribe.clone()).or_default().push(et.clone());
        }
        HookRegistry::from_loaded(subscribers, config)
    }

    // Helper needed on HookRegistry
    impl HookRegistry {
        fn from_loaded(
            subscribers: HashMap<String, Vec<EventTypeConfig>>,
            config: crate::application::hooks::types::HookConfig,
        ) -> Self {
            // Need to add this constructor or use a different approach
            todo!()
        }
    }
}
```

Wait, that's getting complicated with the test needing a private constructor on HookRegistry. Let me simplify - I'll add a `from_loaded` method or just test through actual YAML loading.

Actually, for the integration test in engine.rs, I should test with a real registry loaded from a YAML string in a temp file. Let me simplify.

Actually, let me rethink this test. The engine test should verify the full flow. Let me just test with a temporary YAML file.

Let me rewrite the test section to be simpler.

Actually, looking at this more carefully, I realize the test is getting unwieldy. Let me just have a simple integration test:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::hooks::registry::HookRegistry;
    use crate::domain::types::HealthStatus;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::path::Path;

    struct MockRepo(Arc<AtomicUsize>);

    #[async_trait]
    impl crate::domain::traits::GraphRepository for MockRepo {
        async fn read_query(&self, _q: &str, _p: HashMap<String, Value>) -> Result<Value, DtError> {
            Ok(Value::Null)
        }
        async fn write_query(&self, _q: &str, _p: HashMap<String, Value>) -> Result<Value, DtError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }
        async fn health_check(&self) -> Result<HealthStatus, DtError> { Ok(HealthStatus::Healthy) }
    }

    #[tokio::test]
    async fn engine_fire_writes_subscribed_events() {
        // Create a temp YAML
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
```

- [ ] **Step 2: 运行测试**

```bash
cd /data/myProject/digital-twin-v2
cargo test engine -- --nocapture
```
Expected: 1 test PASS (integration: engine fires hook → writes node)

- [ ] **Step 3: Commit**

```bash
git add src/application/hooks/engine.rs
git commit -m "feat: add HookEngine orchestrator"
```

---

### Task 8: event-hooks.yaml 配置

**Files:**
- Create: `config/event-hooks.yaml`

- [ ] **Step 1: 创建完整配置**

根据 spec §4.1 的配置示例，创建 `config/event-hooks.yaml`，覆盖全部 8 个事件类型：

| event_type | subscribe | side_effects |
|-----------|-----------|-------------|
| Modification | code_modified | 无 |
| Deployment | jenkins_deploy_completed | MERGE JenkinsJob, JenkinsBuild, ServiceInstance + LATEST_DEPLOY, DEPLOYED_TO |
| ConfigChange | config_changed | 无 |
| BugFix | bug_fix_recorded | 无 |
| Decision | decision_made | 无 |
| Conversation | session_ended | 无 |
| PodEvent | pod_event_occurred | 无 |
| K8sSyncEvent | k8s_synced | 无 |

```yaml
hooks:
  code_modified:
    description: "代码文件被修改/创建/删除"
  jenkins_deploy_started:
    description: "Jenkins 构建开始"
  jenkins_deploy_completed:
    description: "Jenkins 构建完成"
  config_changed:
    description: "外部配置被修改"
  decision_made:
    description: "架构决策被记录"
  bug_fix_recorded:
    description: "Bug 修复被记录"
  session_ended:
    description: "AI 会话结束"
  pod_event_occurred:
    description: "K8s Pod 出现异常"
  k8s_synced:
    description: "K8s 资源同步完成"

event_types:
  - label: Modification
    subscribe: code_modified
    id: { prefix: mod, fields: [entity_id, details] }
    id_field: mod_id
    properties:
      - { name: file, from: context.file, required: true }
      - { name: entity_type, from: context.entity_type }
      - { name: entity_id, from: context.entity_id }
      - { name: change_type, from: context.change_type, default: "modify" }
      - { name: diff_summary, from: context.diff_summary }
      - { name: reason, from: context.reason }
    relationships:
      - { type: AFFECTS, target_label: Method, source_field: mod_id, match: { context_field: entity_id, target_field: method_id } }
      - { type: BELONGS_TO, target_label: Project, source_field: mod_id, match: { context_field: project, target_field: name } }

  - label: Deployment
    subscribe: jenkins_deploy_completed
    id: { prefix: deploy, fields: [entity_id, details] }
    id_field: deploy_id
    properties:
      - { name: job, from: context.job, required: true }
      - { name: env, from: context.env, required: true }
      - { name: branch, from: context.branch }
      - { name: version, from: context.version }
      - { name: status, from: context.status, default: "success" }
    side_effects:
      - "MERGE (job:JenkinsJob {name: $job}) ON CREATE SET job.job_id = $job_id, job.full_name = $job ON MATCH SET job.latest_deploy_env = $env, job.latest_deploy_version = $version, job.latest_deployed_at = $now"
      - "MERGE (build:JenkinsBuild {build_id: $build_id}) SET build.number = $build_number, build.deployed_env = $env, build.deployed_at = $now, build.result = $status"
      - "MERGE (si:ServiceInstance {instance_id: $instance_id}) SET si.service_name = $job, si.env = $env, si.updated_at = $now"
      - "OPTIONAL MATCH (job:JenkinsJob {name: $job})-[old:LATEST_DEPLOY]->() DELETE old WITH job, build, si MERGE (job)-[:LATEST_DEPLOY]->(si) MERGE (build)-[:DEPLOYED_TO {env: $env, version: $version, deployed_at: $now}]->(si)"

  - label: ConfigChange
    subscribe: config_changed
    id: { prefix: cfg, fields: [entity_id, details] }
    id_field: change_id
    properties:
      - { name: data_id, from: context.data_id }
      - { name: key, from: context.key }
      - { name: old_value, from: context.old_value }
      - { name: new_value, from: context.new_value }
    relationships:
      - { type: AFFECTS, target_label: NacosConfig, source_field: change_id, match: { context_field: entity_id, target_field: config_id } }

  - label: BugFix
    subscribe: bug_fix_recorded
    id: { prefix: fix, fields: [entity_id, details] }
    id_field: fix_id
    properties:
      - { name: issue, from: context.issue, required: true }
      - { name: root_cause, from: context.root_cause }
      - { name: solution, from: context.solution }
      - { name: files_changed, from: context.files_changed }
    relationships:
      - { type: FIXES, target_label: Method, source_field: fix_id, match: { context_field: entity_id, target_field: method_id } }

  - label: Decision
    subscribe: decision_made
    id: { prefix: decision, fields: [entity_id, details] }
    id_field: decision_id
    properties:
      - { name: title, from: context.title }
      - { name: context, from: context.context }
      - { name: choice, from: context.choice }
      - { name: rationale, from: context.rationale }
      - { name: alternatives, from: context.alternatives }
      - { name: consequences, from: context.consequences }
      - { name: confidence, from: context.confidence }
    relationships:
      - { type: BASED_ON, target_label: Knowledge, source_field: decision_id, match: { context_field: knowledge_id, target_field: knowledge_id } }

  - label: Conversation
    subscribe: session_ended
    id: { prefix: conv, fields: [entity_id, details] }
    id_field: conv_id
    properties:
      - { name: summary, from: context.summary }

  - label: PodEvent
    subscribe: pod_event_occurred
    id: { prefix: pod, fields: [entity_id, details] }
    id_field: event_id
    properties:
      - { name: pod_name, from: context.pod_name }
      - { name: namespace, from: context.namespace }
      - { name: reason, from: context.reason }
      - { name: message, from: context.message }
      - { name: restart_count, from: context.restart_count }
    relationships:
      - { type: AFFECTS, target_label: ServiceInstance, source_field: event_id, match: { context_field: entity_id, target_field: instance_id } }

  - label: K8sSyncEvent
    subscribe: k8s_synced
    id: { prefix: k8s, fields: [entity_id, details] }
    id_field: event_id
    properties:
      - { name: cluster, from: context.cluster }
      - { name: namespaces_count, from: context.namespaces_count }
      - { name: deployments_count, from: context.deployments_count }
      - { name: services_count, from: context.services_count }
      - { name: duration_ms, from: context.duration_ms }
```

- [ ] **Step 2: 验证 YAML 格式**

```bash
cd /data/myProject/digital-twin-v2
python3 -c "import yaml; yaml.safe_load(open('config/event-hooks.yaml')); print('OK')"
```

- [ ] **Step 3: Commit**

```bash
git add config/event-hooks.yaml
git commit -m "feat: add event-hooks.yaml with all 8 event type definitions"
```

---

### Task 9: 注册 HookEngine 到 AppState

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: 在 main.rs 中创建和注入 HookEngine**

找到 AppState（或类似的共享状态结构体），注入 `Arc<HookEngine>`：

```rust
// src/main.rs

mod application {
    pub mod hooks;
    // ... existing modules
}

use application::hooks::engine::HookEngine;
use application::hooks::registry::HookRegistry;

// 在构建 AppState 的地方：
let registry = Arc::new(
    HookRegistry::from_file("config/event-hooks.yaml")
        .expect("failed to load event-hooks.yaml")
);
let hook_engine = Arc::new(HookEngine::new(
    registry,
    graph_repo.clone(),
));

// 将 hook_engine 注入 AppState
let app_state = Arc::new(AppState {
    hook_engine,
    // ... existing fields
});
```

- [ ] **Step 2: 编译检查**

```bash
cd /data/myProject/digital-twin-v2
cargo check
```
Expected: 编译成功

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: register HookEngine in AppState"
```

---

### Task 10: 替换 BugFixHandler

**Files:**
- Modify: `src/application/knowledge/memory/service.rs`
- Delete: `src/application/knowledge/memory/handlers/bug_fix.rs`
- Test: 现有测试适配

- [ ] **Step 1: 修改 service.rs 中的 record_event**

将 BugFix 分支改为调用 `hook_engine.fire("bug_fix_recorded", context)`：

```rust
// 在 MemoryService 中新增 hook_engine 引用
pub struct DefaultMemoryService {
    graph: Arc<dyn GraphRepository>,
    dispatcher: EventDispatcher,
    hook_engine: Arc<HookEngine>,  // 新增
}

impl DefaultMemoryService {
    pub fn new(
        graph: Arc<dyn GraphRepository>,
        hook_engine: Arc<HookEngine>,  // 新增参数
    ) -> Self {
        Self {
            graph,
            dispatcher: build_default_dispatcher(),
            hook_engine,
        }
    }
}

// record_event 中：
for et in &config.event_types {
    if et.label == "BugFix" {  // 等全部迁移完再删这个分支
        self.hook_engine.fire("bug_fix_recorded", ctx).await;
        return Ok(());
    }
}
```

- [ ] **Step 2: 删除 bug_fix.rs**（代码不再使用）
- [ ] **Step 3: 编译通过后 commit**

```bash
git add src/application/knowledge/memory/service.rs
git rm src/application/knowledge/memory/handlers/bug_fix.rs
git commit -m "refactor: replace BugFixHandler with hook engine"
```

---

### Task 11: 替换 ConversationHandler

与 Task 10 相同模式：
- service.rs 中 `hook_engine.fire("session_ended", ...)`
- 删除 `handlers/conversation.rs`

- [ ] **Step 1: 修改 service.rs + 删除 conversation.rs**
- [ ] **Step 2: Commit**

---

### Task 12: 替换 ConfigChangeHandler

- service.rs 中 `hook_engine.fire("config_changed", ...)`
- 删除 `handlers/config_change.rs`

- [ ] **Step 1: 修改 + 删除**
- [ ] **Step 2: Commit**

---

### Task 13: 替换 DecisionHandler

- service.rs 中 `hook_engine.fire("decision_made", ...)`
- 删除 `handlers/decision.rs`

- [ ] **Step 1: 修改 + 删除**
- [ ] **Step 2: Commit**

---

### Task 14: 替换 ModificationHandler

- `watcher.rs` 中文件变更时调用 `hook_engine.fire("code_modified", context)`
- 删除 `handlers/modification.rs`

- [ ] **Step 1: 修改 watcher.rs**
- [ ] **Step 2: 删除 modification.rs**
- [ ] **Step 3: Commit**

---

### Task 15: 替换 DeploymentHandler（复杂 side effects）

- service.rs 中 `hook_engine.fire("jenkins_deploy_completed", context)`
- **关键**：确保 side_effects 中的 Cypher 与当前 DeploymentHandler 完全一致
- 删除 `handlers/deployment.rs`

- [ ] **Step 1: 对照现有 deployment.rs 中的 Cypher，验证 event-hooks.yaml 中 side_effects 一致**
- [ ] **Step 2: 切换调用**
- [ ] **Step 3: 删除 deployment.rs**
- [ ] **Step 4: Commit**

---

### Task 16: 清理 + dt event CLI 更新

**Files:**
- Modify: `src/interfaces/cli/event.rs`
- Delete: `src/application/knowledge/memory/dispatcher.rs`
- Modify: `src/application/knowledge/memory/entities.rs`（简化 EventType）
- Modify: `src/application/knowledge/memory/mod.rs`

- [ ] **Step 1: 更新 dt event CLI**

```rust
// src/interfaces/cli/event.rs
// 改为 --hook 参数代替 --type

pub async fn handle_event(
    hook_name: String,        // 原来叫 event_type
    context_json: String,     // 原来拆成 entity_id/entity_type/details
    // ...
) -> anyhow::Result<()> {
    let context: HookContext = serde_json::from_str(&context_json)?;
    hook_engine.fire(&hook_name, context).await?;
    Ok(())
}
```

- [ ] **Step 2: 删除 dispatcher.rs**
- [ ] **Step 3: 简化 entities.rs 中的 EventType 枚举**（如有引用则保留别名）

```rust
// EventType 可简化为：
pub type EventType = String;  // 或保留枚举作为向后兼容
```

- [ ] **Step 4: 删除 handlers/ 目录**

```bash
rm -rf src/application/knowledge/memory/handlers/
```

- [ ] **Step 5: Commit**

```bash
git add src/interfaces/cli/event.rs
git rm src/application/knowledge/memory/dispatcher.rs
git rm -r src/application/knowledge/memory/handlers/
git commit -m "refactor: cleanup old handlers, update dt event CLI to --hook"
```

---

### Task 17: 更新文档

**Files:**
- Modify: AGENTS.md TRIGGER-RULES 部分

- [ ] **Step 1: 更新 TRIGGER-RULES**

将所有 `dt event --type` 规则改为描述 hook 自动触发行为：

```markdown
## 事件触发机制（已自动化）

事件写入已由 Hook 系统自动处理，AI 不再需要手动调用 `dt event`：

| 操作 | 自动触发的 Hook | 写入标签 |
|------|----------------|---------|
| 代码修改 | `code_modified`（dt build 插件） | `:Modification` |
| Jenkins 部署 | `jenkins_deploy_completed`（jcli_build） | `:Deployment` + 更新 JenkinsJob/Build/ServiceInstance |
| Nacos 配置变更 | `config_changed` | `:ConfigChange` |
| 架构决策 | `decision_made`（dt memorize） | `:Decision` |
| Bug 修复 | `bug_fix_recorded` | `:BugFix` |
| 会话结束 | `session_ended` | `:Conversation` |
| K8s Pod 异常 | `pod_event_occurred` | `:PodEvent` |
| K8s 同步完成 | `k8s_synced` | `:K8sSyncEvent` |
```

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: update TRIGGER-RULES for hook-driven event system"
```
<｜end▁of▁thinking｜>

