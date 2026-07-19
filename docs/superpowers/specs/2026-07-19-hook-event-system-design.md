# Hook 驱动的事件标签系统设计

> 日期: 2026-07-19
> 状态: 设计稿
> 替代: `src/application/knowledge/memory/handlers/` 下 7 个 handler + `dispatcher.rs`

---

## 一、问题分析

### 现状痛点

当前 Memory World 的事件系统有 7 个 handler，每个独立处理一种事件类型：

| Handler | 代码量 | 写入标签 | 节点数 | 重复度 |
|---------|--------|---------|-------|--------|
| `ModificationHandler` | ~70 行 | `:Modification` | 52 | 高 |
| `DeploymentHandler` | ~170 行 | `:JenkinsJob`, `:JenkinsBuild`, `:ServiceInstance`（但不写 `:Deployment` 节点）| 各 208+/1k+/1 | 中（含复杂 side effects）|
| `ConfigChangeHandler` | ~50 行 | `:ConfigChange` | 4 | 高 |
| `BugFixHandler` | ~40 行 | `:BugFix` | 0 | 高 |
| `DecisionHandler` | ~70 行 | `:Decision` | 0 | 高 |
| `ConversationHandler` | ~50 行 | `:Conversation` | 0 | 高 |
| `PodEvent` | 无 handler | `:PodEvent` | 0 | 枚举中有定义，从未实现 |

**所有 handler 的核心流程完全一致：**
1. 解析 `details` 中的 key=value 对
2. 生成事件 ID（SHA256）
3. 拼 Cypher MERGE 语句
4. 设置节点属性
5. 创建关系
6. 关联到 Session

6 个 handler 的重复意味着：
- 新增事件类型 = 写 Rust 代码 + 注册 handler
- 修改属性映射 = 改 Rust 代码 + 重新编译
- AI 必须手动执行 `dt event --type` 遵循 TRIGGER-RULES
- 每个 handler 的测试覆盖不完全一致

---

## 二、设计目标

1. **配置驱动**：事件标签的定义、属性映射、关系创建全部由 YAML 配置控制，不写 Rust 代码
2. **自动触发**：hook 点内嵌在工具/MCP 实现中，AI 不需要手动触发事件
3. **一对多**：一个 hook 点可触发多个标签写入（例如部署完成时同时写入 `:Deployment` + 更新 `:JenkinsJob`）
4. **可扩展**：新增事件类型只需改 YAML，零代码变更
5. **可测试**：核心组件为纯函数，无需 Neo4j 即可测试

---

## 三、架构总览

```
┌─────────────────────────────────────────────────────────┐
│                     HookEngine                           │
│                   (Template Method)                      │
│                                                          │
│  fire(hook_name, context)                                │
│    ├─ registry.subscribers(hook_name) → Vec<Config>      │
│    └─ for each config:                                   │
│         execute_one(config, context)                     │
│           ├─ IdGenerator.generate()         纯函数       │
│           ├─ PropertyMapper.map()           纯函数       │
│           ├─ NodeWriter.write()             → Neo4j     │
│           ├─ RelationshipWriter.write()     → Neo4j     │
│           └─ SideEffectRunner.run()         → Neo4j     │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    GraphRepository (trait)                │
│                     (Neo4j Bolt)                         │
└─────────────────────────────────────────────────────────┘
```

### 组件职责矩阵

| 组件 | 模式 | 是否有状态 | 依赖 Neo4j | 可独立测试 |
|------|------|-----------|-----------|-----------|
| `HookEngine` | Template Method | 有（组合子组件） | 间接 | ✅（mock repo） |
| `IdGenerator` | Strategy | 无 | 否 | ✅ |
| `PropertyMapper` | Strategy | 无 | 否 | ✅ |
| `NodeWriter` | — | 有（GraphRepository） | 是 | ✅（mock repo） |
| `RelationshipWriter` | — | 有（GraphRepository） | 是 | ✅（mock repo） |
| `SideEffectRunner` | — | 有（GraphRepository） | 是 | ✅（mock repo） |
| `HookRegistry` | — | 有（内存配置） | 否 | ✅ |

---

## 四、配置格式

文件：`event-hooks.yaml`，放置在项目根目录或 `config/` 下。

### 4.1 完整示例

```yaml
# event-hooks.yaml

hooks:
  code_modified:
    description: "代码文件被修改/创建/删除"

  jenkins_deploy_started:
    description: "Jenkins 构建开始"
  jenkins_deploy_completed:
    description: "Jenkins 构建完成"

  config_changed:
    description: "外部配置（Nacos/Apollo）被修改"

  decision_made:
    description: "架构决策被记录"

  session_ended:
    description: "AI 会话结束"

  bug_fix_recorded:
    description: "Bug 修复被记录"

  pod_event_occurred:
    description: "K8s Pod 出现异常"

  k8s_synced:
    description: "K8s 资源同步完成"


# 每一条 event_type 定义一个要写入的标签
# 通过 subscribe 字段绑定到一个 hook
event_types:

  # ── 代码修改 ──────────────────────────────────
  - label: Modification
    subscribe: code_modified
    id:
      prefix: mod
      fields: [entity_id, details]       # SHA256(prefix + ":" + val1 + ":" + val2)
    id_field: mod_id                     # 节点的唯一标识字段名
    properties:
      - name: file
        from: context.file
        required: true
      - name: entity_type
        from: context.entity_type
      - name: entity_id
        from: context.entity_id
      - name: change_type
        from: context.change_type
        default: "modify"
      - name: diff_summary
        from: context.diff_summary
      - name: reason
        from: context.reason
    relationships:
      - type: AFFECTS
        target_label: Method
        match:
          context_field: entity_id
          target_field: method_id
      - type: BELONGS_TO
        target_label: Project
        match:
          context_field: project
          target_field: name

  # ── Jenkins 部署 ──────────────────────────────
  - label: Deployment
    subscribe: jenkins_deploy_completed
    id:
      prefix: deploy
      fields: [entity_id, details]
    id_field: deploy_id
    properties:
      - name: job
        from: context.job
        required: true
      - name: env
        from: context.env
        required: true
      - name: branch
        from: context.branch
      - name: version
        from: context.version
      - name: status
        from: context.status
        default: "success"
    side_effects:
      - |
        MERGE (job:JenkinsJob {name: $job})
        ON CREATE SET job.job_id = $job_id, job.full_name = $job
        ON MATCH SET job.latest_deploy_env = $env,
                     job.latest_deploy_version = $version,
                     job.latest_deployed_at = $now
      - |
        MERGE (build:JenkinsBuild {build_id: $build_id})
        SET build.number = $build_number, build.deployed_env = $env,
            build.deployed_at = $now, build.result = $status
      - |
        MERGE (si:ServiceInstance {instance_id: $instance_id})
        SET si.service_name = $job, si.env = $env, si.updated_at = $now
      - |
        WITH job, build, si
        OPTIONAL MATCH (job)-[old:LATEST_DEPLOY]->()
        DELETE old
        MERGE (job)-[:LATEST_DEPLOY]->(si)
        MERGE (build)-[:DEPLOYED_TO {env: $env, version: $version, deployed_at: $now}]->(si)

  # ── 配置变更 ──────────────────────────────────
  - label: ConfigChange
    subscribe: config_changed
    id:
      prefix: cfg
      fields: [entity_id, details]
    id_field: change_id
    properties:
      - name: data_id
        from: context.data_id
      - name: key
        from: context.key
      - name: old_value
        from: context.old_value
      - name: new_value
        from: context.new_value
    relationships:
      - type: AFFECTS
        target_label: NacosConfig
        match:
          context_field: entity_id
          target_field: config_id

  # ── 架构决策 ──────────────────────────────────
  - label: Decision
    subscribe: decision_made
    id:
      prefix: decision
      fields: [entity_id, details]
    id_field: decision_id
    properties:
      - name: title
        from: context.title
      - name: context
        from: context.context
      - name: choice
        from: context.choice
      - name: rationale
        from: context.rationale
      - name: alternatives
        from: context.alternatives
      - name: consequences
        from: context.consequences
      - name: confidence
        from: context.confidence
    relationships:
      - type: BASED_ON
        target_label: Knowledge
        match:
          context_field: knowledge_id
          target_field: knowledge_id

  # ── 会话结束 ──────────────────────────────────
  - label: Conversation
    subscribe: session_ended
    id:
      prefix: conv
      fields: [entity_id, details]
    id_field: conv_id
    properties:
      - name: summary
        from: context.summary

  # ── Bug 修复 ──────────────────────────────────
  - label: BugFix
    subscribe: bug_fix_recorded
    id:
      prefix: fix
      fields: [entity_id, details]
    id_field: fix_id
    properties:
      - name: issue
        from: context.issue
        required: true
      - name: root_cause
        from: context.root_cause
      - name: solution
        from: context.solution
      - name: files_changed
        from: context.files_changed
    relationships:
      - type: FIXES
        target_label: Method
        match:
          context_field: entity_id
          target_field: method_id

  # ── Pod 异常事件（预留） ───────────────────────
  - label: PodEvent
    subscribe: pod_event_occurred
    id:
      prefix: pod
      fields: [entity_id, details]
    id_field: event_id
    properties:
      - name: pod_name
        from: context.pod_name
      - name: namespace
        from: context.namespace
      - name: reason
        from: context.reason
      - name: message
        from: context.message
      - name: restart_count
        from: context.restart_count
    relationships:
      - type: AFFECTS
        target_label: ServiceInstance
        match:
          context_field: entity_id
          target_field: instance_id

  # ── K8s 同步 ──────────────────────────────────
  - label: K8sSyncEvent
    subscribe: k8s_synced
    id:
      prefix: k8s
      fields: [entity_id, details]
    id_field: event_id
    properties:
      - name: cluster
        from: context.cluster
      - name: namespaces_count
        from: context.namespaces_count
      - name: deployments_count
        from: context.deployments_count
      - name: services_count
        from: context.services_count
      - name: duration_ms
        from: context.duration_ms
```

### 4.3 HookContext 结构

Hook 触发时携带的上下文数据，是所有组件之间传递数据的标准格式：

```rust
// 定义在 hooks/types.rs

/// Hook 触发时携带的上下文数据
///
/// 每个 hook 点自己决定往 context 里放什么字段，
/// 配置中的 properties[].from 和 relationships[].match 从此处取值。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    /// 触发此 hook 的 hook 名称
    pub hook_name: String,
    /// 所属项目名（从环境或调用方传入）
    pub project: String,
    /// 当前会话 ID
    pub session_id: String,
    /// 实体 ID（如 Jenkins 任务名、Nacos dataId）
    pub entity_id: String,
    /// 实体类型（如 "JenkinsJob", "NacosConfig"）
    pub entity_type: String,
    /// 任意键值对，hook 点自由填充
    ///
    /// 配置中的 context.xxx 从此处取值。
    /// 例如 config_changed hook 可以放入 { data_id, key, old_value, new_value }
    pub fields: HashMap<String, String>,
}

impl HookContext {
    /// 从 context 取值，优先从 fields 查找，回退到命名属性
    pub fn get(&self, key: &str) -> Option<&str> {
        // 先查 fields
        if let Some(v) = self.fields.get(key) {
            return Some(v.as_str());
        }
        // 回退到结构体字段
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
```

### 4.4 SideEffect 模板变量注入

`side_effects` 中的 Cypher 模板使用 `$xxx` 语法引用变量。
HookEngine 执行时，将所有 `HookContext` 字段 + 生成的 `event_id` 注入为 Cypher 参数：

| 模板变量 | 来源 | 示例值 |
|---------|------|-------|
| `$event_id` | IdGenerator 生成 | `dt://event/deploy/a1b2c3d4e5f6g7h8` |
| `$now` | 当前 UTC 时间 | `2026-07-19T00:00:00Z` |
| `$job` | context.fields["job"] | `my-app-stable` |
| `$env` | context.fields["env"] | `prod` |
| `$session_id` | context.session_id | `2026-07-19-001` |
| `$project` | context.project | `digital-twin` |

### 4.5 配置项说明

| 路径 | 类型 | 说明 |
|------|------|------|
| `hooks.<name>.description` | string | Hook 点的语义描述 |
| `event_types[].label` | string | Neo4j 标签名，如 `Deployment` |
| `event_types[].subscribe` | string | 订阅的 hook 名称 |
| `event_types[].id.prefix` | string | 事件 ID 前缀，如 `deploy` |
| `event_types[].id.fields` | string[] | 用于生成 ID 的 context 字段列表 |
| `event_types[].id_field` | string | 节点属性中用作唯一标识的字段名 |
| `event_types[].properties[]` | object[] | 属性映射规则 |
| `event_types[].properties[].name` | string | 节点属性名 |
| `event_types[].properties[].from` | string | 来源：`context.xxx` 或 `id` 或 `now` |
| `event_types[].properties[].required` | bool | 是否必须 |
| `event_types[].properties[].default` | string | 缺省默认值 |
| `event_types[].side_effects` | string[] | 额外的 Cypher 模板（可选） |
| `event_types[].relationships[]` | object[] | 要创建的关系 |
| `event_types[].relationships[].type` | string | 关系类型，如 `AFFECTS` |
| `event_types[].relationships[].target_label` | string | 目标节点标签 |
| `event_types[].relationships[].match` | object | 匹配规则 |

---

## 五、核心组件设计

### 5.1 HookEngine — 编排器

```rust
// src/application/hooks/engine.rs

pub struct HookEngine {
    registry: Arc<HookRegistry>,
    node_writer: NodeWriter,
    rel_writer: RelationshipWriter,
    side_effector: SideEffectRunner,
}

impl HookEngine {
    /// 触发一个 hook，执行所有订阅的标签写入
    pub async fn fire(&self, hook_name: &str, context: HookContext) -> Result<Vec<WriteResult>> {
        let subscribers = self.registry.subscribers(hook_name);
        let mut results = Vec::with_capacity(subscribers.len());

        for cfg in subscribers {
            let result = self.execute_one(cfg, &context).await;
            results.push(result);
        }

        Ok(results)
    }

    /// 执行单个标签配置的写入
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

        // 5. 执行 side effects（如果有）
        if !cfg.side_effects.is_empty() {
            let ctx_vars = self.build_template_vars(ctx, &event_id);
            if let Err(e) = self.side_effector
                .run(&cfg.side_effects, &ctx_vars)
                .await
            {
                return WriteResult::failed(&cfg.label, &event_id, e);
            }
        }

        WriteResult::success(&cfg.label, &event_id, timer.elapsed())
    }
}
```

### 5.2 IdGenerator — 标识生成

```rust
// src/application/hooks/id_generator.rs

pub struct IdGenerator;

impl IdGenerator {
    /// 生成确定的 SHA256 事件 ID
    ///
    /// 格式: dt://event/{prefix}/{sha256[:16]}
    /// 相同输入 → 相同 ID → MERGE 天然去重
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
```

### 5.3 PropertyMapper — 属性映射

```rust
// src/application/hooks/property_mapper.rs

pub struct PropertyMapper;

impl PropertyMapper {
    /// 将配置 + context → 节点属性 HashMap
    ///
    /// 支持的来源:
    ///   - "id"             → 生成的 event_id
    ///   - "context.xxx"    → HookContext 中的字段
    ///   - "now"            → 当前 UTC 时间戳 (RFC 3339)
    pub fn map(
        props: &[PropertyConfig],
        ctx: &HookContext,
        event_id: &str,
    ) -> HashMap<String, serde_json::Value> {
        let mut map = HashMap::new();
        let now = chrono::Utc::now().to_rfc3339();

        // 通用元属性：所有节点都带上标签名和时间戳
        map.insert("_label".into(), /* ... */);
        map.insert("_created_at".into(), Value::String(now));

        for prop in props {
            let value = match prop.from.as_str() {
                "id" => Value::String(event_id.to_string()),
                "now" => Value::String(now.clone()),
                s if s.starts_with("context.") => {
                    let key = s.trim_start_matches("context.");
                    ctx.get(key)
                        .map(|v| Value::String(v.to_string()))
                        .unwrap_or_else(|| {
                            prop.default
                                .as_ref()
                                .map(|d| Value::String(d.clone()))
                                .unwrap_or(Value::Null)
                        })
                }
                _ => Value::Null,
            };

            if value.is_null() && prop.required {
                // 缺失必填字段 → 记录告警但不中断
                tracing::warn!("missing required property: {}", prop.name);
            }

            map.insert(prop.name.clone(), value);
        }

        map
    }
}
```

### 5.4 NodeWriter — 通用节点写入

```rust
// src/application/hooks/node_writer.rs

pub struct NodeWriter {
    graph: Arc<dyn GraphRepository>,
}

impl NodeWriter {
    /// 通用 MERGE 节点写入
    ///
    /// 生成的 Cypher（以 Deployment 为例）:
    /// ```cypher
    /// MERGE (e:Deployment {deploy_id: $event_id})
    /// SET e._label = "Deployment",
    ///     e._created_at = "2026-07-19T00:00:00Z",
    ///     e.job = $job,
    ///     e.env = $env,
    ///     e.branch = $branch
    /// ```
    ///
    /// 所有标签使用同一套 Cypher 生成逻辑，
    /// 没有任何标签特定的代码分支。
    pub async fn write(
        &self,
        label: &str,
        id_field: &str,
        event_id: &str,
        props: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DtError> {
        let mut cypher = String::from("MERGE (e:");
        cypher.push_str(label);
        cypher.push_str(" {");
        cypher.push_str(id_field);
        cypher.push_str(": $event_id})\n");

        for (key, value) in props {
            if value.is_null() { continue; }
            let param_name = format!("p_{}", key.replace('.', "_"));
            cypher.push_str(&format!("SET e.{} = ${}\n", key, param_name));
        }

        let mut params = std::collections::HashMap::new();
        params.insert("event_id".into(), serde_json::Value::String(event_id.into()));
        for (key, value) in props {
            if value.is_null() { continue; }
            let param_name = format!("p_{}", key.replace('.', "_"));
            params.insert(param_name, value.clone());
        }

        self.graph.write_query(&cypher, params).await?;
        Ok(())
    }
}
```

### 5.5 RelationshipWriter — 通用关系写入

```rust
// src/application/hooks/relationship_writer.rs

pub struct RelationshipWriter {
    graph: Arc<dyn GraphRepository>,
}

impl RelationshipWriter {
    /// 通用关系创建
    ///
    /// 生成的 Cypher:
    /// ```cypher
    /// MATCH (e:Deployment {deploy_id: $event_id})
    /// MATCH (t:ServiceInstance {instance_id: $target_id})
    /// MERGE (e)-[:DEPLOYS_TO]->(t)
    /// ```
    pub async fn write(
        &self,
        rels: &[RelationshipConfig],
        ctx: &HookContext,
        event_id: &str,
    ) -> Result<(), DtError> {
        for rel in rels {
            let target_id = ctx.get(&rel.match.context_field).unwrap_or_default();
            if target_id.is_empty() { continue; }

            let cypher = format!(
                "MATCH (e {{{}: $event_id}})
                 MATCH (t:{} {{ {}: $target_id }})
                 MERGE (e)-[:{}]->(t)",
                rel.source_field,  // e.g., deploy_id
                rel.target_label,  // e.g., ServiceInstance
                rel.match.target_field,  // e.g., instance_id
                rel.rel_type,      // e.g., DEPLOYS_TO
            );

            let mut params = std::collections::HashMap::new();
            params.insert("event_id".into(), Value::String(event_id.into()));
            params.insert("target_id".into(), Value::String(target_id));

            self.graph.write_query(&cypher, params).await?;
        }
        Ok(())
    }
}
```

### 5.6 SideEffectRunner — 逃生舱

```rust
// src/application/hooks/side_effect_runner.rs

pub struct SideEffectRunner {
    graph: Arc<dyn GraphRepository>,
}

impl SideEffectRunner {
    /// 执行原始 Cypher 模板。
    ///
    /// 模板中的 $xxx 变量由调用方预先替换为参数值。
    /// 这是处理复杂操作的逃生舱（如 Deployment 的 JenkinsJob/JenkinsBuild 更新）。
    pub async fn run(
        &self,
        effects: &[String],
        vars: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DtError> {
        for cypher in effects {
            self.graph.write_query(cypher, vars.clone()).await?;
        }
        Ok(())
    }
}
```

---

## 六、Hook 点位置

| Hook 名 | 内嵌位置 | 触发时机 | 代码文件 |
|---------|---------|---------|---------|
| `code_modified` | `dt build` 插件回调 | 文件修改/创建时 | `src/application/build/watcher.rs` |
| `jenkins_deploy_started` | `jcli_build` MCP | Jenkins 构建开始时 | 外部 MCP / `dt trigger-hook` |
| `jenkins_deploy_completed` | `jcli_build` MCP / 构建状态轮询 | Jenkins 构建完成时 | 外部 MCP / `dt trigger-hook` |
| `config_changed` | `dt event --hook` | AI 修改外部配置后 | `src/interfaces/cli/event.rs` |
| `decision_made` | `dt memorize` | 记录架构决策时 | `src/interfaces/cli/event.rs` |
| `bug_fix_recorded` | `dt event --hook` | AI 记录 Bug 修复时 | `src/interfaces/cli/event.rs` |
| `session_ended` | session-end protocol | AI 会话结束时 | AGENTS.md 协议 |
| `pod_event_occurred` | K8s 监控/事件监听 | K8s Pod 异常时 | 预留 |
| `k8s_synced` | `dt k8s-sync` | K8s 同步完成后 | `src/application/sync/k8s/` |

### 触发方式

对于内部 Rust 代码：
```rust
// 直接调用
hooks.fire("code_modified", context).await?;
```

对于外部 MCP 工具（如 `jcli_build`）：
```bash
# 通过 CLI 触发
dt trigger-hook jenkins_deploy_completed \
  --context '{"job":"my-app","env":"prod","version":"v1.2.3"}'
```

---

## 七、`dt event` 命令演变

当前：
```bash
dt event --type Deployment --entity-id job_xxx \
  --entity-type JenkinsJob \
  --details "branch: main; env: prod; status: success" \
  --project my-app
```

改为：
```bash
# 退化为通用的 hook 手动触发器
dt event --hook jenkins_deploy_completed \
  --context '{"job":"my-app","env":"prod","branch":"main","status":"success"}'
```

`dt event` 不再知道任何标签名，只负责将 context 传递给 hook 系统。

---

## 八、测试策略

### 纯函数组件（无需 Neo4j）

| 组件 | 测试内容 | 用例数 |
|------|---------|-------|
| `IdGenerator` | 确定性、唯一性、格式 | ~5 |
| `PropertyMapper` | 字段映射、默认值、必填检查 | ~8 |
| `HookRegistry` | YAML 加载、订阅查询、热重载 | ~6 |

### 集成组件（mock GraphRepository）

| 组件 | 测试内容 |
|------|---------|
| `NodeWriter` | Cypher 生成、参数绑定、错误处理 |
| `RelationshipWriter` | 关系创建、缺失目标处理 |
| `SideEffectRunner` | 模板执行、多语句 |
| `HookEngine` | 全链路：fire → 多个 subscriber → 写入结果 |

---

## 九、迁移策略

### 第一步：新建基础（不破坏现有系统）

1. 创建 `src/application/hooks/` 目录和上述 7 个组件
2. 创建 `event-hooks.yaml`（包含所有现存事件类型的配置）
3. 实现 `HookRegistry` 从 YAML 加载
4. 编写单元测试覆盖纯函数

### 第二步：替换简单 handler

逐个替换，每个 handler 替换后可独立验证：

| 步骤 | Handler | 写入标签 | 改为 | 验证方式 |
|------|---------|---------|------|---------|
| 2a | `ConversationHandler` | `:Conversation` | `hooks.fire("session_ended", ...)` | 会话结束写入正常 |
| 2b | `BugFixHandler` | `:BugFix` | `hooks.fire("bug_fix_recorded", ...)` | BugFix 节点写入正常 |
| 2c | `ConfigChangeHandler` | `:ConfigChange` | `hooks.fire("config_changed", ...)` | ConfigChange 节点写入正常 |
| 2d | `DecisionHandler` | `:Decision` | `hooks.fire("decision_made", ...)` | Decision 节点写入正常 |
| 2e | `ModificationHandler` | `:Modification` | `hooks.fire("code_modified", ...)` | Modification 节点写入正常 |

### 第三步：替换 DeploymentHandler（最复杂）

是唯一需要 `side_effects` 的事件类型。先确保 side_effects Cypher 模板与新配置一致，再切换。

### 第四步：清理

1. 删除 `handlers/` 目录（7 个文件）
2. 删除 `dispatcher.rs` 和 `EventDispatcher` 结构体
3. 简化 `entities.rs`（`EventType` 枚举可降级为配置中的字符串）
4. 简化 `service.rs`（`record_event` 逻辑改为调用 `HookEngine`）
5. 更新 AGENTS.md 中的 TRIGGER-RULES

---

## 十、设计模式总结

| 模式 | 应用位置 | 说明 |
|------|---------|------|
| **Template Method** | `HookEngine::execute_one()` | 定义事件写入的固定步骤骨架，子步骤由子组件实现 |
| **Strategy** | `IdGenerator`, `PropertyMapper` | ID 生成策略和属性映射策略由配置控制 |
| **Observer** | Hook → EventType | Hook 是 Subject，EventType 配置是 Observer |
| **Composite** | `HookEngine` | 组合多个 writer/runner 完成完整写入 |
| **Bridge** | `NodeWriter`/`RelationshipWriter` → `GraphRepository` | 通过 trait 桥接，不依赖具体 Neo4j 实现 |
| **Factory** | `HookRegistry::from_file()` | 从 YAML 创建配置对象 |

---

## 十一、未完成事项

- [ ] `dt trigger-hook` CLI 子命令的具体定义
- [ ] 热重载（`SIGHUP` 重新加载 `event-hooks.yaml`）
  - 注：侧效模板变量注入已在 §4.4 中定义
- [ ] 配置校验（启动时检查 YAML 合法性）
- [ ] 性能指标：hook 触发耗时、每个 subscriber 耗时
