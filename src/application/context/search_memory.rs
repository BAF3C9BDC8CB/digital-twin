//! memory 世界 — 事件节点关键词检索（自 cli/build.rs:1509-1516 迁入，补 elementId 定位）。

use std::collections::HashMap;

use crate::application::context::search_mcp::{CrossWorldSearch, SearchHit};

impl CrossWorldSearch {
    pub(crate) async fn search_memory(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Vec<SearchHit> {
        // ── 向量语义检索优先（记忆已向量化到 kg_nodes）──────────────────
        // 用户原则: 所有查询必须通过向量, 除非显式标注精准查询。
        // memory 世界向量通道: embed query → kg_nodes 集合 search_with_filter
        // (project/scope 过滤下沉 payload 级), 语义召回自然语言表述。
        // 关键词通道仅作向量不可用时的降级兜底。
        let vec_hits = self._search_memory_vector(query, limit, project).await;
        if !vec_hits.is_empty() {
            return vec_hits;
        }
        // 向量不可用或 0 命中 → 关键词降级（OR 语义, 按命中词数排序）
        self._search_memory_keyword(query, limit, project).await
    }

    /// 向量通道: kg_nodes 集合语义检索。
    async fn _search_memory_vector(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Vec<SearchHit> {
        let (Some(ref vector), Some(ref embed)) = (self.vector_ref(), self.embed_ref()) else {
            return Vec::new();
        };
        let Ok(embeddings) = embed.embed_batch(&[query.to_string()]).await else {
            return Vec::new();
        };
        let Some(query_vec) = embeddings.into_iter().next() else {
            return Vec::new();
        };
        // 记忆统一全局检索（不分项目/全局）——按语义内容召回,
        // 靠记忆内容中的文件路径/标识定位实际位置。
        // project 参数不再参与过滤（保留用于返回字段溯源）。
        let internal_limit = (limit as u64 * 2).max(10);
        let Ok(rows) = vector
            .search(
                crate::shared::collections::KG_NODES,
                query_vec,
                internal_limit,
            )
            .await
        else {
            return Vec::new();
        };
        rows.into_iter()
            .filter_map(|row| {
                let payload = row.get("payload").cloned().unwrap_or_default();
                let score = row
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if score < 0.3 {
                    return None; // 分数阈值, 低相关不召回
                }
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let desc = payload
                    .get("content")
                    .or_else(|| payload.get("summary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let proj = payload
                    .get("project")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let kid = payload
                    .get("knowledge_id")
                    .or_else(|| payload.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(SearchHit {
                    id: kid.clone(),
                    title: name,
                    snippet: desc,
                    content: None,
                    source_world: "memory".into(),
                    entity_type: "Knowledge".into(),
                    file_type: None,
                    file_type_label: None,
                    score,
                    source_ref: None,
                    metadata: None,
                    file_path: None,
                    project: if proj.is_empty() { None } else { Some(proj) },
                    start_line: None,
                    end_line: None,
                    signature: None,
                    calls: vec![],
                    element_id: if kid.is_empty() { None } else { Some(kid) },
                    llm_analysis: None,
                    score_breakdown: None,
                    hop: None,
                    via_same_as: None,
                    relations: None,
                    evidence: None,
                    rerank_degraded: None,
                })
            })
            .take(limit)
            .collect()
    }

    /// 关键词降级通道: 图数据库 OR 关键词检索（向量不可用时兜底）。
    async fn _search_memory_keyword(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Vec<SearchHit> {
        let Some(ref graph) = self.graph_ref() else {
            return Vec::new();
        };
        // 记忆统一全局检索（不分项目/全局）——project 不参与过滤,
        // 靠记忆内容中的文件路径/标识定位实际位置。
        let project_filter = String::new();
        // 关键词拆分：按空白/标点切分。
        // OR 语义：任意关键词命中即可召回（多词 AND 对自然语言查询过严，
        // 如"payment 数据库 连接"要求三词全中，但记忆里是"支付/库"导致 0 命中）。
        // 按命中词数降序排序，命中的词越多排名越前。
        let keywords: Vec<&str> = query
            .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
            .filter(|s| !s.is_empty() && s.chars().count() >= 2)
            .collect();
        if keywords.is_empty() {
            return Vec::new();
        }
        // 每个关键词的命中条件（name/details/summary/content 任一字段）
        let kw_conds: Vec<String> = keywords
            .iter()
            .enumerate()
            .map(|(i, _)| {
                format!(
                    "(n.name CONTAINS $kw{0} OR n.details CONTAINS $kw{0} OR \
                     coalesce(n.summary, '') CONTAINS $kw{0} OR \
                     coalesce(n.content, '') CONTAINS $kw{0})",
                    i
                )
            })
            .collect();
        // 命中词数（排序用）
        let score_expr = format!(
            "({})",
            keywords
                .iter()
                .enumerate()
                .map(|(i, _)| format!(
                    "CASE WHEN (n.name CONTAINS $kw{0} OR n.details CONTAINS $kw{0} OR \
                     coalesce(n.summary, '') CONTAINS $kw{0} OR \
                     coalesce(n.content, '') CONTAINS $kw{0}) THEN 1 ELSE 0 END",
                    i
                ))
                .collect::<Vec<_>>()
                .join(" + ")
        );
        let cypher = format!(
            "MATCH (n) WHERE (n:Modification OR n:Deployment OR n:ConfigChange OR \
             n:BugFix OR n:Decision OR n:Conversation OR n:Session OR n:Knowledge) \
             AND ({}){project_filter}\
             RETURN labels(n)[0] AS type, coalesce(n.name, n.entity_id, n.session_id, '') AS name, \
                    coalesce(n.details, n.summary, n.content, '') AS desc, elementId(n) AS eid, \
                    {score_expr} AS score \
             ORDER BY score DESC \
             LIMIT {limit}",
            kw_conds.join(" OR "),
        );
        let mut params = HashMap::new();
        for (i, kw) in keywords.iter().enumerate() {
            params.insert(
                format!("kw{}", i),
                serde_json::Value::String(kw.to_string()),
            );
        }
        let Ok(result) = graph.read_query(&cypher, params).await else {
            return Vec::new();
        };
        result
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| SearchHit {
                        id: row
                            .get("eid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        title: row
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        snippet: row
                            .get("desc")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        content: None,
                        source_world: "memory".into(),
                        entity_type: row
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        file_type: None,
                        file_type_label: None,
                        score: row
                            .get("score")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                        source_ref: None,
                        metadata: None,
                        file_path: None,
                        project: None,
                        start_line: None,
                        end_line: None,
                        signature: None,
                        calls: vec![],
                        element_id: row
                            .get("eid")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        llm_analysis: None,
                        score_breakdown: None,
                        hop: None,
                        via_same_as: None,
                        relations: None,
                        evidence: None,
                        rerank_degraded: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::context::search_mcp::{
        CrossWorldSearch, CrossWorldSearchTrait, SearchRequest,
    };
    use crate::domain::traits::GraphRepository;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MockGraph {
        captured_query: std::sync::Mutex<String>,
    }

    impl MockGraph {
        fn new() -> Self {
            Self {
                captured_query: std::sync::Mutex::new(String::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl GraphRepository for MockGraph {
        async fn read_query(
            &self,
            query: &str,
            _params: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            *self.captured_query.lock().unwrap() = query.to_string();
            Ok(serde_json::json!([{
                "type": "Decision", "name": "s5-d6-clamp",
                "desc": "rerank 分数 clamp 归一", "eid": "4:0:99"
            }]))
        }
        async fn write_query(
            &self,
            _q: &str,
            _p: HashMap<String, serde_json::Value>,
        ) -> Result<serde_json::Value, crate::domain::error::DtError> {
            Ok(serde_json::json!([]))
        }
        async fn health_check(
            &self,
        ) -> Result<crate::domain::types::HealthStatus, crate::domain::error::DtError> {
            Ok(crate::domain::types::HealthStatus::Healthy)
        }
    }


    #[tokio::test]
    async fn memory_world_project_filter_adds_condition() {
        // 记忆统一全局检索: 传了 project 也不应出现 project 过滤条件（不分项目/全局）
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "S5".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: Some("pay-center".into()),
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let _ = cws.search(&req).await.unwrap();
        let captured = graph.captured_query.lock().unwrap().clone();
        assert!(!captured.contains("n.project"));
        assert!(!captured.contains("scope"));
        // 不带 project（全局）时同样不出现 project 条件
        let graph2 = Arc::new(MockGraph::new());
        let cws2 = CrossWorldSearch::new(Some(graph2.clone()), None, None, None);
        let req2 = SearchRequest {
            query: "S5".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let _ = cws2.search(&req2).await.unwrap();
        let captured2 = graph2.captured_query.lock().unwrap().clone();
        assert!(!captured2.contains("n.project"));
    }

    #[tokio::test]
    async fn memory_world_queries_event_labels_and_maps_rows() {
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "S5".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let result = cws.search(&req).await.unwrap();
        assert_eq!(result.per_world_counts.get("memory"), Some(&1));
        let hit = &result.hits[0];
        assert_eq!(hit.entity_type, "Decision");
        assert_eq!(hit.title, "s5-d6-clamp");
        assert_eq!(hit.source_world, "memory");
        assert_eq!(hit.element_id.as_deref(), Some("4:0:99"));
        let captured = graph.captured_query.lock().unwrap().clone();
        assert!(captured.contains("n:Modification"));
        assert!(captured.contains("n:Decision"));
        assert!(captured.contains("n:Knowledge"));
        assert!(captured.contains("elementId(n) AS eid"));
    }

    #[tokio::test]
    async fn memory_world_multi_keyword_and_semantics() {
        // 多词查询 → 拆成多个 kw 参数，AND 连接，且不出现整串 $q
        let graph = Arc::new(MockGraph::new());
        let cws = CrossWorldSearch::new(Some(graph.clone()), None, None, None);
        let req = SearchRequest {
            query: "净盘 分账基数".into(),
            world: Some("memory".into()),
            limit: Some(5),
            project: None,
            max_hops: None,
            with_evidence: None,
            origin: None,
            doc_id: None,
            file_type: None,
            entity_type_filter: None,
        };
        let _ = cws.search(&req).await.unwrap();
        let captured = graph.captured_query.lock().unwrap().clone();
        // AND 语义：两个关键词条件都出现
        assert!(captured.contains("CONTAINS $kw0"));
        assert!(captured.contains("CONTAINS $kw1"));
        assert!(captured.contains("AND"));
        // 不应有整串 $q（旧实现）
        assert!(!captured.contains("CONTAINS $q"));
    }
}
