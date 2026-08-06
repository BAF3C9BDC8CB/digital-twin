//! memory 世界 — 事件节点关键词检索（自 cli/build.rs:1509-1516 迁入，补 elementId 定位）。

use std::collections::HashMap;

use crate::application::context::search_mcp::{CrossWorldSearch, SearchHit};

impl CrossWorldSearch {
    pub(crate) async fn search_memory(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let Some(ref graph) = self.graph_ref() else {
            return Vec::new();
        };
        let cypher = format!(
            "MATCH (n) WHERE (n:Modification OR n:Deployment OR n:ConfigChange \
             OR n:BugFix OR n:Decision OR n:Conversation OR n:Session) \
             AND (n.details CONTAINS $q OR coalesce(n.summary, '') CONTAINS $q) \
             RETURN labels(n)[0] AS type, coalesce(n.name, n.entity_id, n.session_id, '') AS name, \
                    coalesce(n.details, n.summary, '') AS desc, elementId(n) AS eid \
             LIMIT {limit}"
        );
        let mut params = HashMap::new();
        params.insert("q".into(), serde_json::Value::String(query.to_string()));
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
                        source_world: "memory".into(),
                        entity_type: row
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        file_type: None,
                        file_type_label: None,
                        score: 0.0,
                        source_ref: None,
                        file_path: None,
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
        assert!(captured.contains("elementId(n) AS eid"));
    }
}
