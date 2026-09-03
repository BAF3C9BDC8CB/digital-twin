//! KG Router — 知识图谱统一 LLM 路由与智能过滤服务。
//!
//! # 功能
//!
//! 1. **任务感知路由**：根据任务类型从知识图谱查询路由规则，调用最优模型
//! 2. **智能结果过滤**：LLM 二次判断搜索结果相关性，移除噪音
//! 3. **可观测性**：记录所有 LLM 调用到知识图谱供分析
//!
//! # 架构
//!
//! - 复用现有 `LlmService` trait（不重复实现 provider 接入）
//! - 路由规则存储在 Memgraph（`:RouteRule` 节点）
//! - 调用日志存储在 Memgraph（`:LlmCall`、`:FilterLog` 节点）

pub mod service;

pub use service::{FilteredResults, KgRouter, RouteRule, SearchResultItem, TaskType};
