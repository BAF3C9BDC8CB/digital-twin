# KG-Router：知识图谱统一路由与智能过滤

## 一、背景与目标

### 现状
- **现有 LLM 接入**：项目已通过 `EmbedProviderRouter` 统一路由 LLM 请求（siliconflow/xinference），配置在 `config/pipeline.yaml`
- **搜索结果噪音**：向量检索可能返回语义相似但实际不相关的结果，需要二次过滤
- **缺乏上下文路由**：所有查询使用相同模型，未根据任务类型优化选择

### 目标
1. **复用现有 LLM 基础设施**：基于 `LlmService` trait 扩展，不重复实现 provider 接入
2. **智能结果过滤**：LLM 二次分析搜索结果相关性，移除噪音
3. **任务感知路由**：根据任务类型（代码抽取/文档总结/知识查询）选择最优模型
4. **可配置开关**：pipeline.yaml 中控制功能启用

---

## 二、架构设计

### 2.1 核心组件

```
┌─────────────────────────────────────────────┐
│         业务层（Search/Memorize/Learn）       │
└──────────────────┬──────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────┐
│          KG Router（知识图谱内实现）          │
├─────────────────────────────────────────────┤
│  1. 路由决策层                               │
│     - 从 Memgraph 查询任务类型 → 模型映射    │
│     - 降级策略（主模型失败切换备选）          │
│                                             │
│  2. 结果过滤器（可选，开关控制）              │
│     - LLM 分析每条搜索结果相关性             │
│     - 过滤 score < threshold 的结果          │
│                                             │
│  3. 可观测性                                 │
│     - 调用日志 (:LlmCall 节点)               │
│     - Token 消耗统计                         │
└──────────────────┬──────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────┐
│   EmbedProviderRouter (现有 LlmService)      │
│   - siliconflow / xinference / openai_compat│
└─────────────────────────────────────────────┘
```

### 2.2 与现有系统集成

**复用点**：
- `src/infrastructure/provider_router.rs::EmbedProviderRouter` 已实现 `LlmService`
- `config/pipeline.yaml::providers` 已配置多提供方（siliconflow/xinference/openai_compatible）
- 直接调用 `llm_service.chat(system_prompt, user_prompt, temperature, max_tokens)`

**新增点**：
- 知识图谱中存储路由规则（`:RouteRule` 节点）
- 结果过滤逻辑（`KgRouter::filter_results()`）
- 配置项 `pipeline.yaml::kg_router` 控制开关

---

## 三、配置管理

### 3.1 Pipeline 配置扩展

在 `config/pipeline.yaml` 新增 `kg_router` 段：

```yaml
kg_router:
  enabled: true  # 启用任务感知路由
  
  result_filter:
    enabled: true  # 启用搜索结果智能过滤
    threshold: 0.6  # 相关性阈值（0-1），低于此值的结果被移除
    max_tokens: 200  # 过滤判断时 LLM 最大 token 数
    temperature: 0.0  # 过滤判断使用确定性推理
  
  observability:
    log_calls: true  # 记录所有 LLM 调用到 Memgraph
    log_filter_results: true  # 记录过滤详情
```

### 3.2 Rust 配置结构

在 `src/application/pipeline/config.rs` 新增：

```rust
/// KG Router 配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KgRouterConfig {
    /// 启用任务感知路由。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 搜索结果智能过滤配置。
    #[serde(default)]
    pub result_filter: ResultFilterConfig,

    /// 可观测性配置。
    #[serde(default)]
    pub observability: ObservabilityConfig,
}

impl Default for KgRouterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            result_filter: ResultFilterConfig::default(),
            observability: ObservabilityConfig::default(),
        }
    }
}

/// 搜索结果过滤配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultFilterConfig {
    /// 启用过滤功能。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 相关性阈值（0.0-1.0），低于此值的结果被移除。
    #[serde(default = "default_filter_threshold")]
    pub threshold: f32,

    /// 过滤判断时 LLM 最大 token 数。
    #[serde(default = "default_filter_max_tokens")]
    pub max_tokens: u32,

    /// 过滤判断时的温度参数（建议 0.0 确定性推理）。
    #[serde(default)]
    pub temperature: f32,
}

const fn default_filter_threshold() -> f32 {
    0.6
}

const fn default_filter_max_tokens() -> u32 {
    200
}

impl Default for ResultFilterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: default_filter_threshold(),
            max_tokens: default_filter_max_tokens(),
            temperature: 0.0,
        }
    }
}

/// 可观测性配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// 记录所有 LLM 调用到 Memgraph。
    #[serde(default = "default_true")]
    pub log_calls: bool,

    /// 记录过滤详情。
    #[serde(default = "default_true")]
    pub log_filter_results: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_calls: true,
            log_filter_results: true,
        }
    }
}
```

在 `PipelineConfig` 中添加字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    // ... 现有字段 ...
    
    /// KG Router 路由与过滤配置。
    #[serde(default)]
    pub kg_router: Option<KgRouterConfig>,
}
```

---

## 四、知识图谱数据模型

### 4.1 节点设计

```cypher
// 任务类型定义
(:TaskType {
  name: "CodeExtraction",  // 代码抽取
  description: "从源码中提取函数签名、类定义等结构化信息",
  requires_quality: true,  // 是否需要高质量模型
  requires_speed: false
})

// 路由规则
(:RouteRule {
  task_type: "CodeExtraction",
  primary_provider: "siliconflow",  // 对应 pipeline.yaml 中的 provider 名
  primary_model: "tencent/Hunyuan-MT-7B",
  fallback_provider: "xinference",
  fallback_model: "qwen3.5",
  temperature: 0.1,
  max_tokens: 2000
})

// LLM 调用记录（可观测性）
(:LlmCall {
  id: "call_20260903_080000_abc123",
  timestamp: datetime("2026-09-03T08:00:00Z"),
  task_type: "CodeExtraction",
  provider_used: "siliconflow",
  model_used: "tencent/Hunyuan-MT-7B",
  tokens_used: 1523,
  latency_ms: 2341,
  status: "success",  // success / error / filtered
  error_message: null
})

// 过滤记录（搜索结果过滤详情）
(:FilterLog {
  id: "filter_20260903_080001_def456",
  timestamp: datetime("2026-09-03T08:00:01Z"),
  query: "如何实现 LLM 路由",
  original_count: 10,
  filtered_count: 3,
  removed_items: ["item_id_1", "item_id_5", "item_id_7"]
})
```

### 4.2 关系设计

```cypher
(:RouteRule)-[:USES_PRIMARY]->(:TaskType)
(:RouteRule)-[:FALLBACK_TO]->(:TaskType)
(:LlmCall)-[:TRIGGERED_BY]->(:RouteRule)
(:FilterLog)-[:RELATED_TO]->(:LlmCall)
```

---

## 五、实现方案

### 5.1 核心服务接口

```rust
// src/services/kg_router.rs

use crate::domain::traits::LlmService;
use crate::infrastructure::memgraph::MemgraphClient;
use crate::application::pipeline::config::{KgRouterConfig, PipelineConfig};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// KG Router 服务 —— 任务感知 LLM 调用与结果过滤。
pub struct KgRouter {
    kg_client: Arc<MemgraphClient>,
    llm_service: Arc<dyn LlmService>,
    config: KgRouterConfig,
}

/// 任务类型枚举。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TaskType {
    CodeExtraction,      // 代码抽取
    DocSummarization,    // 文档总结
    KnowledgeQuery,      // 知识查询
    ResultFiltering,     // 结果过滤（内部使用）
}

impl TaskType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::CodeExtraction => "CodeExtraction",
            Self::DocSummarization => "DocSummarization",
            Self::KnowledgeQuery => "KnowledgeQuery",
            Self::ResultFiltering => "ResultFiltering",
        }
    }
}

/// 路由规则（从知识图谱查询得到）。
#[derive(Debug, Clone)]
pub struct RouteRule {
    pub task_type: String,
    pub primary_provider: String,
    pub primary_model: String,
    pub fallback_provider: Option<String>,
    pub fallback_model: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
}

/// 搜索结果项（待过滤）。
#[derive(Debug, Clone, Serialize)]
pub struct SearchResultItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
}

/// 过滤后的结果。
#[derive(Debug)]
pub struct FilteredResults {
    pub items: Vec<SearchResultItem>,
    pub removed_count: usize,
    pub filter_time_ms: u64,
}

impl KgRouter {
    pub fn new(
        kg_client: Arc<MemgraphClient>,
        llm_service: Arc<dyn LlmService>,
        config: KgRouterConfig,
    ) -> Self {
        Self {
            kg_client,
            llm_service,
            config,
        }
    }

    /// 根据任务类型调用 LLM（任务感知路由）。
    pub async fn call(
        &self,
        task_type: TaskType,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, DtError> {
        if !self.config.enabled {
            // 未启用路由，直接使用默认参数调用
            return self
                .llm_service
                .chat(system_prompt, user_prompt, 0.1, 4096)
                .await;
        }

        let start = std::time::Instant::now();

        // 1. 从知识图谱查询路由规则
        let rule = self.fetch_route_rule(task_type).await?;

        // 2. 调用主模型（注：当前 EmbedProviderRouter 不支持动态切换 provider，
        //    此处仅演示逻辑，实际需扩展 LlmService trait 或使用配置预设）
        let response = self
            .llm_service
            .chat(
                system_prompt,
                user_prompt,
                rule.temperature,
                rule.max_tokens,
            )
            .await;

        let latency_ms = start.elapsed().as_millis() as u64;

        // 3. 记录调用日志
        if self.config.observability.log_calls {
            self.log_call(task_type, &rule, latency_ms, &response)
                .await?;
        }

        response
    }

    /// 过滤搜索结果（LLM 判断相关性）。
    pub async fn filter_results(
        &self,
        query: &str,
        results: Vec<SearchResultItem>,
    ) -> Result<FilteredResults, DtError> {
        if !self.config.result_filter.enabled || results.is_empty() {
            return Ok(FilteredResults {
                items: results,
                removed_count: 0,
                filter_time_ms: 0,
            });
        }

        let start = std::time::Instant::now();
        let original_count = results.len();
        let mut filtered_items = Vec::new();
        let mut removed_ids = Vec::new();

        for item in results {
            let relevance_score = self.judge_relevance(query, &item).await?;

            if relevance_score >= self.config.result_filter.threshold {
                filtered_items.push(item);
            } else {
                removed_ids.push(item.id.clone());
            }
        }

        let filter_time_ms = start.elapsed().as_millis() as u64;
        let removed_count = removed_ids.len();

        // 记录过滤日志
        if self.config.observability.log_filter_results {
            self.log_filter(query, original_count, removed_count, &removed_ids)
                .await?;
        }

        Ok(FilteredResults {
            items: filtered_items,
            removed_count,
            filter_time_ms,
        })
    }

    /// LLM 判断单条结果与查询的相关性（返回 0.0-1.0 分数）。
    async fn judge_relevance(
        &self,
        query: &str,
        item: &SearchResultItem,
    ) -> Result<f32, DtError> {
        let system_prompt = r#"你是一个搜索结果相关性评估专家。
请根据用户查询和搜索结果内容，判断该结果是否真正相关。
直接返回一个 0.0-1.0 之间的数字（无需解释）：
- 1.0：完全相关，直接回答查询
- 0.5-0.9：部分相关，可能有帮助
- 0.0-0.4：不相关或仅语义相似"#;

        let user_prompt = format!(
            "查询：{}\n\n结果标题：{}\n结果摘要：{}\n\n相关性评分（0.0-1.0）：",
            query, item.title, item.snippet
        );

        let response = self
            .llm_service
            .chat(
                system_prompt,
                &user_prompt,
                self.config.result_filter.temperature,
                self.config.result_filter.max_tokens,
            )
            .await?;

        // 解析 LLM 返回的分数
        let score = response
            .trim()
            .parse::<f32>()
            .unwrap_or(0.5); // 解析失败时给中等分数

        Ok(score.clamp(0.0, 1.0))
    }

    /// 从知识图谱查询路由规则。
    async fn fetch_route_rule(&self, task_type: TaskType) -> Result<RouteRule, DtError> {
        let query = format!(
            r#"MATCH (r:RouteRule {{task_type: '{}'}}) 
               RETURN r.primary_provider AS primary_provider,
                      r.primary_model AS primary_model,
                      r.fallback_provider AS fallback_provider,
                      r.fallback_model AS fallback_model,
                      r.temperature AS temperature,
                      r.max_tokens AS max_tokens
               LIMIT 1"#,
            task_type.as_str()
        );

        let result = self.kg_client.query(&query).await?;

        if result.is_empty() {
            // 未找到规则，使用默认配置
            return Ok(RouteRule {
                task_type: task_type.as_str().to_string(),
                primary_provider: "siliconflow".to_string(),
                primary_model: "tencent/Hunyuan-MT-7B".to_string(),
                fallback_provider: None,
                fallback_model: None,
                temperature: 0.1,
                max_tokens: 4096,
            });
        }

        // 解析查询结果（此处简化，实际需根据 Memgraph 返回格式解析）
        // ...
        todo!("解析 Memgraph 查询结果为 RouteRule")
    }

    /// 记录 LLM 调用日志到知识图谱。
    async fn log_call(
        &self,
        task_type: TaskType,
        rule: &RouteRule,
        latency_ms: u64,
        response: &Result<String, DtError>,
    ) -> Result<(), DtError> {
        let (status, error_msg, tokens_used) = match response {
            Ok(text) => ("success", None, self.estimate_tokens(text)),
            Err(e) => ("error", Some(e.to_string()), 0),
        };

        let query = format!(
            r#"CREATE (c:LlmCall {{
                id: '{}',
                timestamp: datetime(),
                task_type: '{}',
                provider_used: '{}',
                model_used: '{}',
                tokens_used: {},
                latency_ms: {},
                status: '{}',
                error_message: {}
            }})"#,
            self.generate_call_id(),
            task_type.as_str(),
            rule.primary_provider,
            rule.primary_model,
            tokens_used,
            latency_ms,
            status,
            error_msg
                .as_ref()
                .map(|e| format!("'{}'", e))
                .unwrap_or_else(|| "null".to_string())
        );

        self.kg_client.execute(&query).await?;
        Ok(())
    }

    /// 记录过滤日志到知识图谱。
    async fn log_filter(
        &self,
        query: &str,
        original_count: usize,
        removed_count: usize,
        removed_ids: &[String],
    ) -> Result<(), DtError> {
        let removed_ids_str = removed_ids.join(",");
        let cypher = format!(
            r#"CREATE (f:FilterLog {{
                id: '{}',
                timestamp: datetime(),
                query: '{}',
                original_count: {},
                filtered_count: {},
                removed_items: '{}'
            }})"#,
            self.generate_call_id(),
            query.replace('\'', "\\'"),
            original_count,
            original_count - removed_count,
            removed_ids_str
        );

        self.kg_client.execute(&cypher).await?;
        Ok(())
    }

    fn generate_call_id(&self) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        format!("call_{}", now)
    }

    fn estimate_tokens(&self, text: &str) -> u32 {
        // 粗略估算：中文 1 字符 ≈ 1.5 token，英文 1 单词 ≈ 1.3 token
        (text.len() as f32 * 0.4) as u32
    }
}
```

### 5.2 集成到搜索服务

在 `src/application/knowledge/search/service.rs` 中使用 `KgRouter`：

```rust
pub struct SearchService {
    // ... 现有字段 ...
    kg_router: Option<Arc<KgRouter>>,
}

impl SearchService {
    pub async fn search(
        &self,
        query: &str,
        world: World,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, DtError> {
        // 1. 执行向量检索
        let raw_results = self.vector_search(query, world, limit * 2).await?; // 多取一些待过滤

        // 2. 如果启用了过滤，使用 KgRouter 过滤结果
        let filtered = if let Some(router) = &self.kg_router {
            let filtered = router.filter_results(query, raw_results).await?;
            
            if filtered.removed_count > 0 {
                tracing::info!(
                    "搜索过滤移除 {} 条不相关结果，耗时 {}ms",
                    filtered.removed_count,
                    filtered.filter_time_ms
                );
            }
            
            filtered.items
        } else {
            raw_results
        };

        // 3. 返回前 limit 条
        Ok(filtered.into_iter().take(limit).collect())
    }
}
```

---

## 六、初始化路由规则

运行脚本将路由规则写入 Memgraph：

```rust
// scripts/init_kg_router_rules.rs

use digital_twin_v2::infrastructure::memgraph::MemgraphClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = MemgraphClient::new("bolt://localhost:7687", "memgraph", "")?;

    // 代码抽取：高质量需求
    client.execute(r#"
        MERGE (t:TaskType {name: 'CodeExtraction'})
        SET t.description = '从源码中提取函数签名、类定义等结构化信息',
            t.requires_quality = true
    "#).await?;

    client.execute(r#"
        CREATE (r:RouteRule {
            task_type: 'CodeExtraction',
            primary_provider: 'siliconflow',
            primary_model: 'tencent/Hunyuan-MT-7B',
            fallback_provider: 'xinference',
            fallback_model: 'qwen3.5',
            temperature: 0.1,
            max_tokens: 2000
        })
    "#).await?;

    // 文档总结：平衡成本与质量
    client.execute(r#"
        CREATE (r:RouteRule {
            task_type: 'DocSummarization',
            primary_provider: 'xinference',
            primary_model: 'qwen3.5',
            fallback_provider: 'siliconflow',
            fallback_model: 'tencent/Hunyuan-MT-7B',
            temperature: 0.3,
            max_tokens: 1000
        })
    "#).await?;

    // 知识查询：低延迟需求
    client.execute(r#"
        CREATE (r:RouteRule {
            task_type: 'KnowledgeQuery',
            primary_provider: 'xinference',
            primary_model: 'qwen3.5',
            fallback_provider: null,
            fallback_model: null,
            temperature: 0.2,
            max_tokens: 512
        })
    "#).await?;

    println!("✓ 路由规则初始化完成");
    Ok(())
}
```

---

## 七、可观测性查询

### 7.1 调用统计

```cypher
// 按任务类型统计 token 消耗
MATCH (c:LlmCall)
WHERE c.timestamp > datetime() - duration({days: 7})
RETURN c.task_type, 
       sum(c.tokens_used) AS total_tokens, 
       count(*) AS call_count,
       avg(c.latency_ms) AS avg_latency_ms
ORDER BY total_tokens DESC;

// 错误率分析
MATCH (c:LlmCall)
WHERE c.timestamp > datetime() - duration({hours: 24})
RETURN c.provider_used,
       count(CASE WHEN c.status = 'error' THEN 1 END) AS error_count,
       count(*) AS total_count,
       100.0 * count(CASE WHEN c.status = 'error' THEN 1 END) / count(*) AS error_rate
ORDER BY error_rate DESC;
```

### 7.2 过滤效果分析

```cypher
// 过滤效率统计
MATCH (f:FilterLog)
WHERE f.timestamp > datetime() - duration({days: 1})
RETURN avg(f.original_count) AS avg_original,
       avg(f.filtered_count) AS avg_filtered,
       avg(f.original_count - f.filtered_count) AS avg_removed,
       100.0 * avg(f.original_count - f.filtered_count) / avg(f.original_count) AS removal_rate;

// 高过滤率查询（可能表明检索质量问题）
MATCH (f:FilterLog)
WHERE f.timestamp > datetime() - duration({hours: 6})
  AND (f.original_count - f.filtered_count) * 1.0 / f.original_count > 0.5
RETURN f.query, 
       f.original_count, 
       f.filtered_count,
       (f.original_count - f.filtered_count) * 100.0 / f.original_count AS removal_rate
ORDER BY removal_rate DESC
LIMIT 10;
```

---

## 八、迁移计划

### 8.1 配置文件更新

在 `config/pipeline.yaml` 添加：

```yaml
kg_router:
  enabled: true
  result_filter:
    enabled: true
    threshold: 0.6
    max_tokens: 200
    temperature: 0.0
  observability:
    log_calls: true
    log_filter_results: true
```

### 8.2 代码改造

1. **第一阶段**：实现 `KgRouter` 核心逻辑（无业务集成）
   - 路由规则查询
   - 结果过滤功能
   - 日志记录

2. **第二阶段**：集成到搜索服务
   - `SearchService` 注入 `KgRouter`
   - 搜索结果调用 `filter_results()`

3. **第三阶段**：扩展到其他场景
   - `dt_memorize` 使用任务感知路由
   - `dt_learn` 使用专用模型

### 8.3 灰度发布

- **灰度开关**：`kg_router.enabled = false` 保持现有行为
- **过滤开关**：`result_filter.enabled = false` 跳过过滤（仅路由）
- **阈值调整**：生产环境初期设置 `threshold = 0.4`（宽松），观察一周后调整

---

## 九、扩展方向

### 9.1 动态路由优化

```cypher
// 根据历史调用成功率动态调整主备模型
MATCH (c:LlmCall {task_type: 'CodeExtraction'})
WHERE c.timestamp > datetime() - duration({days: 7})
WITH c.provider_used AS provider,
     count(CASE WHEN c.status = 'success' THEN 1 END) AS success_count,
     count(*) AS total_count
RETURN provider,
       100.0 * success_count / total_count AS success_rate
ORDER BY success_rate DESC;
```

### 9.2 成本优化

- 按月统计 token 消耗，自动切换到低成本模型
- 高频查询缓存 LLM 响应（`:LlmCache` 节点）

### 9.3 多轮对话支持

- 扩展 `KgRouter::call()` 支持 `messages: Vec<Message>`
- 记录会话上下文到 Memgraph

---

## 十、注意事项

### 10.1 限制

- **Provider 切换**：当前 `EmbedProviderRouter` 在运行时不支持动态切换 provider，路由规则中的 `primary_provider` 仅作记录，实际仍使用 `pipeline.yaml` 配置的默认 provider
- **Token 估算**：`estimate_tokens()` 为粗略估算，精确统计需调用 tokenizer

### 10.2 性能影响

- **过滤延迟**：每条结果需 1 次 LLM 调用，10 条结果约增加 2-5 秒
- **并发优化**：可批量调用（future::join_all）降低总延迟
- **缓存策略**：相同查询 24 小时内复用过滤结果

### 10.3 成本控制

- 过滤使用低 token 配额（max_tokens=200）
- 生产环境建议仅对用户直接查询开启过滤，内部 API 调用跳过
