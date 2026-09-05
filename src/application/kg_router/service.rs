//! KG Router 核心服务实现。

use crate::application::pipeline::config::KgRouterConfig;
use crate::domain::error::DtError;
use crate::domain::traits::{GraphRepository, LlmService};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// KG Router 服务 —— 任务感知 LLM 调用与结果过滤。
pub struct KgRouter {
    kg_client: Arc<dyn GraphRepository>,
    llm_service: Arc<dyn LlmService>,
    config: KgRouterConfig,
}

/// 任务类型枚举。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TaskType {
    CodeExtraction,   // 代码抽取
    DocSummarization, // 文档总结
    KnowledgeQuery,   // 知识查询
    ResultFiltering,  // 结果过滤（内部使用）
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

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "CodeExtraction" => Some(Self::CodeExtraction),
            "DocSummarization" => Some(Self::DocSummarization),
            "KnowledgeQuery" => Some(Self::KnowledgeQuery),
            "ResultFiltering" => Some(Self::ResultFiltering),
            _ => None,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
}

/// 过滤后的结果。
#[derive(Debug, Clone, Serialize)]
pub struct FilteredResults {
    pub items: Vec<SearchResultItem>,
    pub removed_count: usize,
    pub filter_time_ms: u64,
}

impl KgRouter {
    pub fn new(
        kg_client: Arc<dyn GraphRepository>,
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
        let start = Instant::now();

        // 1. 从知识图谱查询路由规则
        let rule = self.fetch_route_rule(task_type).await?;

        // 2. 调用 LLM（注：当前 EmbedProviderRouter 使用配置中的 provider，
        //    路由规则中的 provider 仅作记录）
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
            let _ = self.log_call(task_type, &rule, latency_ms, &response).await;
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

        let start = Instant::now();
        let original_count = results.len();
        let mut filtered_items = Vec::new();
        let mut removed_ids = Vec::new();

        for item in results {
            match self.judge_relevance(query, &item).await {
                Ok(relevance_score) => {
                    if relevance_score >= self.config.result_filter.threshold {
                        filtered_items.push(item);
                    } else {
                        removed_ids.push(item.id.clone());
                    }
                }
                Err(e) => {
                    // 判断失败时保留该结果（避免误删）
                    tracing::warn!("结果过滤 LLM 调用失败: {}, 保留结果 {}", e, item.id);
                    filtered_items.push(item);
                }
            }
        }

        let filter_time_ms = start.elapsed().as_millis() as u64;
        let removed_count = removed_ids.len();

        // 记录过滤日志
        if self.config.observability.log_filter_results && removed_count > 0 {
            let _ = self
                .log_filter(query, original_count, removed_count, &removed_ids)
                .await;
        }

        Ok(FilteredResults {
            items: filtered_items,
            removed_count,
            filter_time_ms,
        })
    }

    /// LLM 判断单条结果与查询的相关性（返回 0.0-1.0 分数）。
    async fn judge_relevance(&self, query: &str, item: &SearchResultItem) -> Result<f32, DtError> {
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
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<f32>().ok())
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

        let result = self.kg_client.read_query(&query, HashMap::new()).await?;

        // 解析 JSON 结果
        let rows = result
            .as_array()
            .ok_or_else(|| DtError::Repository("查询结果不是数组".into()))?;

        if rows.is_empty() {
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

        let row = &rows[0];
        let primary_provider = row
            .get("primary_provider")
            .and_then(|v| v.as_str())
            .unwrap_or("siliconflow")
            .to_string();
        let primary_model = row
            .get("primary_model")
            .and_then(|v| v.as_str())
            .unwrap_or("tencent/Hunyuan-MT-7B")
            .to_string();
        let fallback_provider = row
            .get("fallback_provider")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let fallback_model = row
            .get("fallback_model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let temperature = row
            .get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.1) as f32;
        let max_tokens = row
            .get("max_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(4096) as u32;

        Ok(RouteRule {
            task_type: task_type.as_str().to_string(),
            primary_provider,
            primary_model,
            fallback_provider,
            fallback_model,
            temperature,
            max_tokens,
        })
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
            Ok(text) => ("success", "null".to_string(), self.estimate_tokens(text)),
            Err(e) => (
                "error",
                format!("'{}'", e.to_string().replace('\'', "\\'")),
                0,
            ),
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
        );

        self.kg_client.write_query(&query, HashMap::new()).await?;
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

        self.kg_client.write_query(&cypher, HashMap::new()).await?;
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
        // 简化为：总字符数 * 0.4
        (text.len() as f32 * 0.4) as u32
    }

    /// 初始化路由规则到知识图谱。
    pub async fn init_rules(&self) -> Result<(), DtError> {
        // 代码抽取：高质量需求
        self.kg_client
            .write_query(
                r#"
            MERGE (t:TaskType {name: 'CodeExtraction'})
            SET t.description = '从源码中提取函数签名、类定义等结构化信息',
                t.requires_quality = true
        "#,
                HashMap::new(),
            )
            .await?;

        self.kg_client
            .write_query(
                r#"
            MERGE (r:RouteRule {task_type: 'CodeExtraction'})
            SET r.primary_provider = 'siliconflow',
                r.primary_model = 'tencent/Hunyuan-MT-7B',
                r.temperature = 0.1,
                r.max_tokens = 2000
        "#,
                HashMap::new(),
            )
            .await?;

        // 文档总结：平衡成本与质量
        self.kg_client
            .write_query(
                r#"
            MERGE (r:RouteRule {task_type: 'DocSummarization'})
            SET r.primary_provider = 'siliconflow',
                r.primary_model = 'tencent/Hunyuan-MT-7B',
                r.temperature = 0.3,
                r.max_tokens = 1000
        "#,
                HashMap::new(),
            )
            .await?;

        // 知识查询：低延迟需求
        self.kg_client
            .write_query(
                r#"
            MERGE (r:RouteRule {task_type: 'KnowledgeQuery'})
            SET r.primary_provider = 'siliconflow',
                r.primary_model = 'tencent/Hunyuan-MT-7B',
                r.temperature = 0.2,
                r.max_tokens = 512
        "#,
                HashMap::new(),
            )
            .await?;

        // 结果过滤：确定性推理
        self.kg_client
            .write_query(
                r#"
            MERGE (r:RouteRule {task_type: 'ResultFiltering'})
            SET r.primary_provider = 'siliconflow',
                r.primary_model = 'tencent/Hunyuan-MT-7B',
                r.temperature = 0.0,
                r.max_tokens = 200
        "#,
                HashMap::new(),
            )
            .await?;

        Ok(())
    }

    /// 查询调用统计（按任务类型）。
    pub async fn query_call_stats(&self, days: u32) -> Result<Vec<CallStats>, DtError> {
        let query = format!(
            r#"MATCH (c:LlmCall)
               WHERE c.timestamp > datetime() - duration({{days: {}}})
               RETURN c.task_type AS task_type,
                      sum(c.tokens_used) AS total_tokens,
                      count(*) AS call_count,
                      avg(c.latency_ms) AS avg_latency_ms,
                      count(CASE WHEN c.status = 'error' THEN 1 END) AS error_count
               ORDER BY total_tokens DESC"#,
            days
        );

        let result = self.kg_client.read_query(&query, HashMap::new()).await?;
        let rows = result
            .as_array()
            .ok_or_else(|| DtError::Repository("查询结果不是数组".into()))?;
        let mut stats = Vec::new();

        for row in rows {
            let task_type = row
                .get("task_type")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let total_tokens = row
                .get("total_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as u64;
            let call_count = row.get("call_count").and_then(|v| v.as_i64()).unwrap_or(0) as u64;
            let avg_latency_ms = row
                .get("avg_latency_ms")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as u64;
            let error_count = row.get("error_count").and_then(|v| v.as_i64()).unwrap_or(0) as u64;

            stats.push(CallStats {
                task_type,
                total_tokens,
                call_count,
                avg_latency_ms,
                error_count,
            });
        }

        Ok(stats)
    }

    /// 查询过滤统计。
    pub async fn query_filter_stats(&self, hours: u32) -> Result<FilterStats, DtError> {
        let query = format!(
            r#"MATCH (f:FilterLog)
               WHERE f.timestamp > datetime() - duration({{hours: {}}})
               RETURN avg(f.original_count) AS avg_original,
                      avg(f.filtered_count) AS avg_filtered,
                      count(*) AS total_filters"#,
            hours
        );

        let result = self.kg_client.read_query(&query, HashMap::new()).await?;
        let rows = result
            .as_array()
            .ok_or_else(|| DtError::Repository("查询结果不是数组".into()))?;

        if rows.is_empty() {
            return Ok(FilterStats {
                avg_original: 0.0,
                avg_filtered: 0.0,
                avg_removed: 0.0,
                removal_rate: 0.0,
                total_filters: 0,
            });
        }

        let row = &rows[0];
        let avg_original = row
            .get("avg_original")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let avg_filtered = row
            .get("avg_filtered")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let total_filters = row
            .get("total_filters")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u64;

        let avg_removed = avg_original - avg_filtered;
        let removal_rate = if avg_original > 0.0 {
            (avg_removed / avg_original) * 100.0
        } else {
            0.0
        };

        Ok(FilterStats {
            avg_original,
            avg_filtered,
            avg_removed,
            removal_rate,
            total_filters,
        })
    }
}

/// 调用统计。
#[derive(Debug, Clone, Serialize)]
pub struct CallStats {
    pub task_type: String,
    pub total_tokens: u64,
    pub call_count: u64,
    pub avg_latency_ms: u64,
    pub error_count: u64,
}

/// 过滤统计。
#[derive(Debug, Clone, Serialize)]
pub struct FilterStats {
    pub avg_original: f64,
    pub avg_filtered: f64,
    pub avg_removed: f64,
    pub removal_rate: f64,
    pub total_filters: u64,
}
