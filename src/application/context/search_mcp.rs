//! **CrossWorldSearch** — 跨世界语义搜索（4.8）。
//!
//! 在单次查询中跨所有可用的知识世界（code、knowledge graph、
//! documents、vector store）检索，返回混合结果。
//!
//! # MCP 工具：`dt_search`
//!
//! ```text
//! dt_search(query: str, world?: str, limit?: int)
//!   → CrossWorldResult JSON
//! ```

use std::sync::Arc;

use crate::application::knowledge::extract::retrieve::{
    RelationSnippet, RetrieveRequest, Retriever, ScoreBreakdown,
};
use crate::domain::error::DtError;
use crate::domain::traits::{EmbedService, GraphRepository, RerankService, VectorRepository};

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// 跨世界搜索的输入。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchRequest {
    /// 自然语言搜索查询。
    pub query: String,
    /// 目标世界："code"、"knowledge"、"doc"、"all"。默认："all"。
    pub world: Option<String>,
    /// 每个世界的最大结果数。
    pub limit: Option<usize>,
    /// 按项目过滤。
    pub project: Option<String>,
    /// 图扩展跳数（knowledge 世界），白名单 {1,2}，默认 1。
    pub max_hops: Option<u32>,
    /// knowledge top-5 实体从 doc_chunks 回填证据段落（仅 knowledge 世界生效）。
    pub with_evidence: Option<bool>,
    /// 按 kg_nodes payload origin 过滤召回种子（extracted/learned/manual）。
    pub origin: Option<String>,
    /// 仅 world=doc：限定单文档内检索证据块。
    pub doc_id: Option<String>,
}

/// 来自任意世界的单个搜索命中。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    /// 唯一标识符。
    pub id: String,
    /// 短标签/标题。
    pub title: String,
    /// 内容或摘要片段。
    pub snippet: String,
    /// 来源世界（"code"、"knowledge"、"doc"、"vector"）。
    pub source_world: String,
    /// 实体类型（Method、Class、Knowledge、Document 等）。
    pub entity_type: String,
    /// 相关度分数 [0.0, 1.0]。
    pub score: f64,
    /// 源文件或引用 URL。
    pub source_ref: Option<String>,
    /// 源文件路径（code 世界）。
    pub file_path: Option<String>,
    /// 起始行号（code 世界）。
    pub start_line: Option<u32>,
    /// 结束行号（code 世界）。
    pub end_line: Option<u32>,
    /// 函数/方法签名（code 世界）。
    pub signature: Option<String>,
    /// 方法级 LLM 分析（code world，payload 直取；"用途：…\n逻辑：…"）。
    #[serde(default)]
    pub llm_analysis: Option<String>,
    /// 被调用的函数名（code 世界）。
    pub calls: Vec<String>,
    /// 来自知识图谱的元素 ID。
    pub element_id: Option<String>,
    /// 排序分解（knowledge 世界新链路填充）。
    #[serde(default)]
    pub score_breakdown: Option<ScoreBreakdown>,
    /// 图距离：0=直接命中（含 SAME_AS 别名），1/2=扩展邻居。
    #[serde(default)]
    pub hop: Option<u32>,
    /// 是否经 SAME_AS 别名归并命中。
    #[serde(default)]
    pub via_same_as: Option<bool>,
    /// 命中实体的关系摘要（去重聚合后，上限 5 条）。
    #[serde(default)]
    pub relations: Option<Vec<RelationSnippet>>,
    /// 证据段落（world=doc 或 with_evidence 回填）。
    #[serde(default)]
    pub evidence: Option<Vec<String>>,
    /// rerank 降级标记。
    #[serde(default)]
    pub rerank_degraded: Option<bool>,
}

/// 跨世界搜索的输出。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrossWorldResult {
    /// 原始查询。
    pub query: String,
    /// 被搜索的世界。
    pub world: String,
    /// 聚合结果。
    pub hits: Vec<SearchHit>,
    /// 命中总数。
    pub total: usize,
    /// 各世界的命中数，便于透明化。
    pub per_world_counts: std::collections::HashMap<String, usize>,
    /// 降级标记（"rerank_unavailable" / "graph_expansion_failed" / "embed_unavailable"）。
    #[serde(default)]
    pub degraded: Vec<String>,
}

// ---------------------------------------------------------------------------
// Service trait + impl
// ---------------------------------------------------------------------------

/// 执行跨世界语义搜索。
#[async_trait::async_trait]
pub trait CrossWorldSearchTrait: Send + Sync {
    /// 跨世界搜索。
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError>;
}

/// [`CrossWorldSearchTrait`] 的标准实现。
pub struct CrossWorldSearch {
    graph: Option<Arc<dyn GraphRepository>>,
    vector: Option<Arc<dyn VectorRepository>>,
    embed: Option<Arc<dyn EmbedService>>,
    rerank: Option<Arc<dyn RerankService>>,
}

impl CrossWorldSearch {
    /// 使用后端创建。
    pub fn new(
        graph: Option<Arc<dyn GraphRepository>>,
        vector: Option<Arc<dyn VectorRepository>>,
        embed: Option<Arc<dyn EmbedService>>,
        rerank: Option<Arc<dyn RerankService>>,
    ) -> Self {
        Self {
            graph,
            vector,
            embed,
            rerank,
        }
    }

    /// 创建无后端实例（用于测试）。
    pub fn empty() -> Self {
        Self {
            graph: None,
            vector: None,
            embed: None,
            rerank: None,
        }
    }

    /// 供同 crate 扩展方法（search_config/search_memory）访问图后端。
    pub(crate) fn graph_ref(&self) -> &Option<Arc<dyn GraphRepository>> {
        &self.graph
    }

    /// 供同 crate 扩展方法（search_config）访问向量后端。
    pub(crate) fn vector_ref(&self) -> &Option<Arc<dyn VectorRepository>> {
        &self.vector
    }

    /// 供同 crate 扩展方法（search_config）访问 embed 后端。
    pub(crate) fn embed_ref(&self) -> &Option<Arc<dyn EmbedService>> {
        &self.embed
    }

    /// 通过 Qdrant 向量搜索检索 Reality World（代码实体）。
    ///
    /// 查询 `{project}_methods` 集合（未指定项目时查询所有 `*_methods`），
    /// 并提取完整的 payload 字段，包括 start_line、end_line、
    /// calls 与 method_id。分数阈值从 `DT_SEARCH_MIN_SCORE` 读取
    /// （默认 0.3）。
    async fn search_code(
        &self,
        query: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };

        let embeddings = embed.embed_batch(&[query.to_string()]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        // U-D6：只查全局 code_methods；project 过滤下沉 payload 级
        let method_cols = vec![crate::shared::collections::CODE_METHODS.to_string()];

        if method_cols.is_empty() {
            return Ok(Vec::new());
        }

        // 环境变量中的分数阈值（默认 0.3）
        let min_score = std::env::var("DT_SEARCH_MIN_SCORE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.3);

        let mut all_hits = Vec::new();
        for col in &method_cols {
            match vector.search(col, query_vec.clone(), limit as u64).await {
                Ok(results) => {
                    for hit in results {
                        let score = hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        if score < min_score {
                            continue;
                        }
                        let payload = hit.get("payload").or(hit.get("result")).unwrap_or(&hit);
                        let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        if name.is_empty() || name == "?" {
                            continue;
                        }
                        if let Some(p) = project {
                            let pp = payload.get("project").and_then(|v| v.as_str()).unwrap_or("");
                            if pp != p {
                                continue;
                            }
                        }

                        let calls: Vec<String> = payload
                            .get("calls")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| c.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        all_hits.push(SearchHit {
                            // Qdrant point id 可能是数值或 UUID 字符串，均须保留；
                            // 缺失时回退 payload 身份，保证 RRF 键唯一（防 "code:?" 坍缩）。
                            id: match hit.get("id") {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(serde_json::Value::Number(n)) => n.to_string(),
                                _ => payload
                                    .get("entity_id")
                                    .or(payload.get("method_id"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| {
                                        format!(
                                            "{}:{}",
                                            payload
                                                .get("file_path")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("?"),
                                            name
                                        )
                                    }),
                            },
                            title: name.to_string(),
                            snippet: {
                                let fp = payload
                                    .get("file_path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let sl = payload
                                    .get("start_line")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let el = payload
                                    .get("end_line")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                if fp.is_empty() {
                                    String::new()
                                } else {
                                    format!("{}: L{}-{}", fp, sl, el)
                                }
                            },
                            source_world: "code".into(),
                            entity_type: "Method".into(),
                            score,
                            source_ref: None,
                            file_path: payload
                                .get("file_path")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            start_line: payload
                                .get("start_line")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32),
                            end_line: payload
                                .get("end_line")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32),
                            signature: payload
                                .get("signature")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            llm_analysis: payload
                                .get("llm_analysis")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            calls,
                            element_id: payload
                                .get("method_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            score_breakdown: None,
                            hop: None,
                            via_same_as: None,
                            relations: None,
                            evidence: None,
                            rerank_degraded: None,
                        });
                    }
                }
                Err(e) => tracing::warn!("对 {col} 的 Qdrant 搜索失败: {e}"),
            }
        }

        // 按分数降序排序，截断到 limit
        all_hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_hits.truncate(limit);
        Ok(all_hits)
    }

    /// 通过 GraphRAG 混合检索（S5）搜索 Knowledge World。
    ///
    /// 委托 retrieve.rs；返回 (hits, degraded)。vector/embed 缺失时无可召回手段，返回空。
    async fn search_knowledge(&self, request: &SearchRequest) -> (Vec<SearchHit>, Vec<String>) {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return (Vec::new(), Vec::new());
        };
        let retriever = Retriever::new(
            self.graph.clone(),
            vector.clone(),
            embed.clone(),
            self.rerank.clone(),
        );
        let req = RetrieveRequest {
            query: &request.query,
            project: request.project.as_deref(),
            limit: request.limit.unwrap_or(20),
            max_hops: request.max_hops.unwrap_or(1),
            origin: request.origin.as_deref(),
        };
        match retriever.search_knowledge(&req).await {
            Ok(mut outcome) => {
                if request.with_evidence == Some(true) {
                    retriever
                        .backfill_evidence(&request.query, &mut outcome.hits)
                        .await;
                }
                (outcome.hits, outcome.degraded)
            }
            Err(e) => {
                tracing::warn!("知识检索失败: {e}");
                (Vec::new(), Vec::new())
            }
        }
    }

    /// 通过 `doc_chunks` 搜索 Doc World（S5 §5.5）。
    ///
    /// filter 强制 `source="doc"`——排除 config_sync 写入的 nacos 配置点
    /// （payload 无 text/doc_id/block_index）。分数过 DT_SEARCH_MIN_SCORE。
    async fn search_doc(
        &self,
        query: &str,
        project: Option<&str>,
        doc_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>, DtError> {
        let (Some(ref vector), Some(ref embed)) = (&self.vector, &self.embed) else {
            return Ok(Vec::new());
        };
        let embeddings = embed.embed_batch(&[query.to_string()]).await?;
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Ok(Vec::new());
        };

        let mut must = vec![serde_json::json!({"key": "source", "match": {"value": "doc"}})];
        if let Some(p) = project {
            must.push(serde_json::json!({"key": "project", "match": {"value": p}}));
        }
        if let Some(d) = doc_id {
            must.push(serde_json::json!({"key": "doc_id", "match": {"value": d}}));
        }
        let filter = serde_json::json!({ "must": must });

        let results = vector
            .search_with_filter(crate::shared::collections::DOC_CHUNKS, query_vec, limit as u64, filter)
            .await?;
        let threshold = crate::application::knowledge::extract::retrieve::min_score();
        let hits = results
            .into_iter()
            .filter_map(|hit| {
                let score = hit.get("score")?.as_f64()?;
                if score < threshold {
                    return None;
                }
                let payload = hit.get("payload")?;
                let doc = payload.get("doc_id")?.as_str()?;
                let block = payload.get("block_index")?.as_u64()? as u32;
                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                Some(SearchHit {
                    id: format!("{doc}:{block}"),
                    title: doc.rsplit('/').next().unwrap_or(doc).to_string(),
                    snippet: text.chars().take(200).collect(),
                    source_world: "doc".into(),
                    entity_type: "Doc".into(),
                    score,
                    source_ref: Some(doc.to_string()),
                    file_path: None,
                    start_line: None,
                    end_line: None,
                    signature: None,
                    llm_analysis: None,
                    calls: vec![],
                    element_id: None,
                    score_breakdown: None,
                    hop: None,
                    via_same_as: None,
                    relations: None,
                    evidence: None,
                    rerank_degraded: None,
                })
            })
            .collect();
        Ok(hits)
    }
}

#[async_trait::async_trait]
impl CrossWorldSearchTrait for CrossWorldSearch {
    async fn search(&self, request: &SearchRequest) -> Result<CrossWorldResult, DtError> {
        let world = request.world.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(20);
        let project = request.project.as_deref();

        let mut per_world = std::collections::HashMap::new();
        let mut degraded: Vec<String> = Vec::new();

        let all_hits = if world == "all" {
            // U-D2/U-D3：all = code+knowledge+doc，跨世界 RRF
            let mut lists: Vec<Vec<SearchHit>> = Vec::new();

            let code_hits = self
                .search_code(&request.query, project, limit)
                .await
                .unwrap_or_default();
            per_world.insert("code".to_string(), code_hits.len());
            lists.push(code_hits);

            let (kn_hits, dgr) = self.search_knowledge(request).await;
            degraded.extend(dgr);
            per_world.insert("knowledge".to_string(), kn_hits.len());
            lists.push(kn_hits);

            let doc_hits = self
                .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                .await
                .unwrap_or_default();
            per_world.insert("doc".to_string(), doc_hits.len());
            lists.push(doc_hits);

            crate::application::context::fusion::rrf_hits(lists, 60.0, limit)
        } else {
            let mut hits = match world {
                "code" => self
                    .search_code(&request.query, project, limit)
                    .await
                    .unwrap_or_default(),
                "knowledge" => {
                    let (h, dgr) = self.search_knowledge(request).await;
                    degraded.extend(dgr);
                    h
                }
                // "vector" 保留为 "doc" 别名（S5 §8.7）
                "doc" | "vector" => self
                    .search_doc(&request.query, project, request.doc_id.as_deref(), limit)
                    .await
                    .unwrap_or_default(),
                "config" => {
                    let (h, dgr) = self.search_config(&request.query, project, limit).await;
                    degraded.extend(dgr);
                    h
                }
                "memory" => self.search_memory(&request.query, limit).await,
                _ => Vec::new(),
            };
            per_world.insert(world.to_string(), hits.len());
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit);
            hits
        };

        let total = all_hits.len();
        Ok(CrossWorldResult {
            query: request.query.clone(),
            world: world.to_string(),
            hits: all_hits,
            total,
            per_world_counts: per_world,
            degraded,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_hit_construction() {
        let hit = SearchHit {
            id: "4:abc".into(),
            title: "PaymentService".into(),
            snippet: "Handles payment processing".into(),
            source_world: "code".into(),
            entity_type: "Service".into(),
            score: 0.95,
            source_ref: Some("src/payment.rs".into()),
            file_path: Some("src/payment.rs".into()),
            start_line: Some(10),
            end_line: Some(45),
            signature: Some("pub fn process()".into()),
            llm_analysis: None,
            calls: vec!["validate".into(), "save".into()],
            element_id: Some("4:xyz".into()),
            score_breakdown: Some(ScoreBreakdown {
                semantic: 0.71,
                rerank: 0.92,
                graph_boost: 1.0,
                final_score: 0.83,
            }),
            hop: Some(0),
            via_same_as: None,
            relations: Some(vec![RelationSnippet {
                rel_type: "routes_to".into(),
                other_end_id: "dt://entity/p/Service/s".into(),
                other_end_name: "PayChannelService".into(),
                direction: "out".into(),
                confidence: 0.9,
                evidence: Some("ifCode 决定路由".into()),
                supplementary_count: 2,
            }]),
            evidence: None,
            rerank_degraded: None,
        };
        assert_eq!(hit.source_world, "code");
        assert!(hit.score > 0.9);
        assert_eq!(hit.start_line, Some(10));
        assert_eq!(hit.calls.len(), 2);
        assert_eq!(hit.relations.as_ref().unwrap()[0].supplementary_count, 2);
    }

    #[test]
    fn search_hit_deserializes_legacy_json_without_new_fields() {
        let legacy = r#"{
            "id":"1","title":"t","snippet":"s","source_world":"knowledge",
            "entity_type":"Knowledge","score":0.5,"source_ref":null,
            "file_path":null,"start_line":null,"end_line":null,
            "signature":null,"calls":[],"element_id":null
        }"#;
        let hit: SearchHit = serde_json::from_str(legacy).unwrap();
        assert!(hit.score_breakdown.is_none());
        assert!(hit.hop.is_none());
        assert!(hit.relations.is_none());
        assert!(hit.rerank_degraded.is_none());
    }

    #[test]
    fn cross_world_result_empty() {
        let result = CrossWorldResult {
            query: "test".into(),
            world: "all".into(),
            hits: vec![],
            total: 0,
            per_world_counts: std::collections::HashMap::new(),
            degraded: vec![],
        };
        assert_eq!(result.total, 0);
    }

    #[test]
    fn cross_world_result_serialization() {
        let mut counts = std::collections::HashMap::new();
        counts.insert("code".into(), 5);
        counts.insert("knowledge".into(), 3);

        let result = CrossWorldResult {
            query: "auth".into(),
            world: "all".into(),
            hits: vec![SearchHit {
                id: "1".into(),
                title: "AuthService".into(),
                snippet: "manages auth".into(),
                source_world: "code".into(),
                entity_type: "Service".into(),
                score: 0.98,
                source_ref: None,
                file_path: None,
                start_line: None,
                end_line: None,
                signature: None,
                llm_analysis: None,
                calls: vec![],
                element_id: None,
                score_breakdown: None,
                hop: None,
                via_same_as: None,
                relations: None,
                evidence: None,
                rerank_degraded: None,
            }],
            total: 8,
            per_world_counts: counts,
            degraded: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("AuthService"));
        assert!(json.contains("\"total\":8"));
    }

    #[test]
    fn search_request_defaults() {
        let req = SearchRequest {
            query: "payment".into(),
            world: None,
            limit: None,
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
        };
        assert_eq!(req.query, "payment");
        assert_eq!(req.world, None);
    }

    #[tokio::test]
    async fn knowledge_world_with_empty_backends_returns_empty_and_no_panic() {
        let cws = CrossWorldSearch::empty();
        let req = SearchRequest {
            query: "q".into(),
            world: Some("knowledge".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 0);
        assert_eq!(result.per_world_counts.get("knowledge"), Some(&0));
        assert!(result.degraded.is_empty());
    }

    #[test]
    fn constructor_accepts_rerank_as_fourth_param() {
        fn _accept(_: Option<std::sync::Arc<dyn crate::domain::traits::RerankService>>) {}
        let cws = CrossWorldSearch::new(None, None, None, None);
        drop(cws);
    }

    struct StubVector {
        hits: Vec<serde_json::Value>,
        captured_filter: std::sync::Mutex<Option<serde_json::Value>>,
    }

    #[async_trait::async_trait]
    impl VectorRepository for StubVector {
        async fn ensure_collection(&self, _c: &str, _d: u32) -> Result<(), DtError> {
            Ok(())
        }
        async fn search(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            Ok(self.hits.clone())
        }
        async fn search_with_filter(
            &self,
            _c: &str,
            _v: Vec<f32>,
            _l: u64,
            filter: serde_json::Value,
        ) -> Result<Vec<serde_json::Value>, DtError> {
            *self.captured_filter.lock().unwrap() = Some(filter);
            Ok(self.hits.clone())
        }
        async fn upsert(&self, _c: &str, _p: Vec<serde_json::Value>) -> Result<(), DtError> {
            Ok(())
        }
        async fn delete_by_filter(&self, _c: &str, _f: serde_json::Value) -> Result<(), DtError> {
            Ok(())
        }
        async fn list_collections(&self) -> Result<Vec<String>, DtError> {
            Ok(vec![])
        }
        async fn collection_info(
            &self,
            name: &str,
        ) -> Result<crate::domain::types::CollectionInfo, DtError> {
            Ok(crate::domain::types::CollectionInfo {
                name: name.into(),
                points_count: 0,
                vector_dim: 0,
                model_version: String::new(),
            })
        }
        async fn delete_collection(&self, _n: &str) -> Result<(), DtError> {
            Ok(())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    struct StubEmbed;

    #[async_trait::async_trait]
    impl EmbedService for StubEmbed {
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, DtError> {
            Ok(texts.iter().map(|_| vec![0.1_f32; 4]).collect())
        }
        async fn health_check(&self) -> Result<crate::domain::types::HealthStatus, DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }

    #[tokio::test]
    async fn code_world_extracts_llm_analysis_and_uses_single_collection() {
        let method = serde_json::json!({
            "id": "pt-m1", "score": 0.9,
            "payload": {
                "name": "createApp", "file_path": "test/project/app.js",
                "start_line": 32, "end_line": 36,
                "signature": "function createApp(port)",
                "llm_analysis": "用途：创建服务器实例。\n逻辑：实例化服务器对象。",
                "project": "test-pipeline", "calls": []
            }
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![method],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(None, Some(vector), Some(std::sync::Arc::new(StubEmbed)), None);
        let req = SearchRequest {
            query: "createApp".into(), world: Some("code".into()), limit: Some(5),
            project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(
            hit.llm_analysis.as_deref(),
            Some("用途：创建服务器实例。\n逻辑：实例化服务器对象。")
        );
        assert_eq!(hit.file_path.as_deref(), Some("test/project/app.js"));
        assert_eq!(hit.start_line, Some(32));
    }

    #[tokio::test]
    async fn code_world_handles_numeric_point_id_and_payload_fallback() {
        // Qdrant 数值型 point id：as_str() 会失败——必须按 number 提取
        let m1 = serde_json::json!({
            "id": 123, "score": 0.9,
            "payload": { "name": "m1", "file_path": "a.js",
                         "start_line": 1, "end_line": 2, "project": "p" }
        });
        // id 缺失：回退 payload entity_id
        let m2 = serde_json::json!({
            "score": 0.8,
            "payload": { "name": "m2", "file_path": "b.js", "entity_id": "dt://method/p/m2",
                         "start_line": 1, "end_line": 2, "project": "p" }
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![m1, m2],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(None, Some(vector), Some(std::sync::Arc::new(StubEmbed)), None);
        let req = SearchRequest {
            query: "q".into(), world: Some("code".into()), limit: Some(5),
            project: None, max_hops: None, with_evidence: None, origin: None, doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].id, "123");
        assert_eq!(result.hits[1].id, "dt://method/p/m2");
        // RRF 键唯一性：两条 hit 的 id 不得相同（防 "code:?" 坍缩回归）
        assert_ne!(result.hits[0].id, result.hits[1].id);
    }

    #[tokio::test]
    async fn doc_world_filters_source_and_maps_chunk_payload() {
        let chunk = serde_json::json!({
            "id": "pt-1", "score": 0.9,
            "payload": {
                "doc_id": "dt://doc/offen-pay/pay-design.md",
                "block_index": 3,
                "project": "offen-pay",
                "text": "路由规则：根据 ifCode 匹配渠道表",
                "entity_ids": ["dt://entity/offen-pay/Channel/ifcode"],
                "degraded": false,
                "source": "doc"
            }
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![chunk],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(vector.clone()),
            Some(std::sync::Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "ifCode 证据".into(),
            world: Some("doc".into()),
            limit: Some(5),
            project: Some("offen-pay".into()),
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: Some("dt://doc/offen-pay/pay-design.md".into()),
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.per_world_counts.get("doc"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.id, "dt://doc/offen-pay/pay-design.md:3");
        assert_eq!(hit.source_world, "doc");
        assert_eq!(hit.entity_type, "Doc");
        assert_eq!(hit.title, "pay-design.md");
        assert!(hit.snippet.contains("ifCode"));
        assert_eq!(
            hit.source_ref.as_deref(),
            Some("dt://doc/offen-pay/pay-design.md")
        );
        // filter 硬条件：source="doc" + project + doc_id
        let filter = vector.captured_filter.lock().unwrap().clone().unwrap();
        let must = filter["must"].as_array().unwrap();
        assert!(must
            .iter()
            .any(|c| c["key"] == "source" && c["match"]["value"] == "doc"));
        assert!(must
            .iter()
            .any(|c| c["key"] == "project" && c["match"]["value"] == "offen-pay"));
        assert!(must.iter().any(|c| c["key"] == "doc_id"));
    }

    #[tokio::test]
    async fn doc_world_skips_nacos_shaped_points() {
        // nacos 配置点（config_sync 写入端）：无 text/doc_id/block_index → 解析丢弃
        let nacos = serde_json::json!({
            "id": "pt-9", "score": 0.9,
            "payload": {"entity_id": "x", "key": "k", "value": "v", "namespace": "public",
                         "source_type": "nacos_config", "project": "p"}
        });
        let vector = std::sync::Arc::new(StubVector {
            hits: vec![nacos],
            captured_filter: std::sync::Mutex::new(None),
        });
        let cws = CrossWorldSearch::new(
            None,
            Some(vector),
            Some(std::sync::Arc::new(StubEmbed)),
            None,
        );
        let req = SearchRequest {
            query: "q".into(),
            world: Some("doc".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.hits.len(), 0);
    }
}
